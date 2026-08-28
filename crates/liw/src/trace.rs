//! `liw trace` — takılmanın NEDENİNİ söyleyen teşhis.
//!
//! `liw bench` "ne kadar kötü" der, `liw perf` "hangi kaldıraçlar açık"
//! der. İkisi de "bu düşüşü ne yaptı" sorusuna cevap vermez.
//!
//! Buradaki fikir tek: her şeyi AYNI SAATE koy. Kare sunum zamanları
//! `CLOCK_MONOTONIC`, `logcat -v monotonic` de öyle, host örneklerine de
//! aynı saati yazıyoruz. Ortak eksen olunca "takılma anında ne oluyordu"
//! sorusu ölçülebilir hale geliyor.
//!
//! # Donma yakalama
//!
//! FPS düşüşü ile donma farklı şeyler ve farklı yöntem ister. 60 saniye
//! kare gelmiyorsa ortada "uzun aralık" yoktur — hiç aralık yoktur.
//! Bu yüzden döngü "en son ne zaman yeni kare gördüm" diye ayrıca bakar
//! ve donma SÜRERKEN günlüğü yakalar. Sonradan bakmak çoğu zaman geç
//! kalıyor: logcat halkası dolup kanıtı düşürüyor.

use anyhow::{Context, Result};
use liw_core::bench::{parse_latency, sample_interval_ms, FrameData};
use liw_core::hostsample::{self, CpuMeter, HostSample};
use liw_core::trace::{self, Kind, LogEvent};
use liw_core::HelperClient;
use std::collections::HashSet;

/// Donma sayılmadan önce kaç ms kare gelmemeli.
const STALL_MS: f64 = 900.0;

/// Günlük kuyruğunun kaç satırı çekilsin.
///
/// Çok küçük olursa iki çekim arasında olaylar kaçar; çok büyük olursa
/// D-Bus mesajı şişer ve her çekim yavaşlar.
const LOG_LINES: u32 = 400;

struct Stall {
    start_ms: f64,
    end_ms: Option<f64>,
    log: Vec<LogEvent>,
}

/// Günlük olaylarını tekrarsız biriktirir.
///
/// `logcat -t N` her çekimde son N satırı verir, yani ardışık çekimler
/// büyük ölçüde ÜST ÜSTE biner. Tekilleştirmeden biriktirmek aynı olayı
/// onlarca kez sayar ve hüküm tamamen bozulur.
#[derive(Default)]
struct LogSink {
    seen: HashSet<(u64, u32, String)>,
    events: Vec<LogEvent>,
}

impl LogSink {
    fn add(&mut self, evs: Vec<LogEvent>) -> Vec<LogEvent> {
        let mut fresh = Vec::new();
        for e in evs {
            let key = ((e.t_ms * 1000.0) as u64, e.pid, e.msg.clone());
            if self.seen.insert(key) {
                fresh.push(e.clone());
                self.events.push(e);
            }
        }
        fresh
    }
}

/// Günlüğü çeker ve ayrıştırır.
///
/// Önce monotonik biçimi dener; helper eski sürümdeyse duvar saatli
/// `Logcat`e düşer. Sessizce boş dönmek "hiç olay yok" gibi görünüp
/// teşhisi yanlış yönlendirirdi, o yüzden hangi yolun kullanıldığı
/// çağırana bildiriliyor.
async fn fetch_log(h: &HelperClient, monotonic: bool) -> (Vec<LogEvent>, bool) {
    let now = hostsample::monotonic_ms();
    let sod = hostsample::local_secs_of_day();
    if monotonic {
        if let Ok(raw) = h.log_trace("main", LOG_LINES).await {
            return (trace::parse_log(&raw, now, sod), true);
        }
    }
    match h.logcat("main", LOG_LINES).await {
        Ok(raw) => (trace::parse_log(&raw, now, sod), false),
        Err(_) => (Vec::new(), false),
    }
}

pub async fn run(pkg: String, duration_s: u64, jank_ms: Option<f64>) -> Result<()> {
    let h = HelperClient::connect().await
        .context("liwd-helper'a bağlanılamadı — systemctl status liwd-helper")?;

    println!("Katman aranıyor...");
    let layer = crate::bench::pick_layer(&h, &pkg).await?;
    let first = parse_latency(&h.surface_latency(&layer).await?)
        .context("ilk anlık görüntü ayrıştırılamadı")?;
    let interval = sample_interval_ms(first.refresh_ns);
    let refresh_ms = first.refresh_ns as f64 / 1e6;
    // Eşik yenilemeden türetiliyor: 60 Hz'de 33 ms takılma değil,
    // 180 Hz'de kesinlikle takılma.
    let jank_ms = jank_ms.unwrap_or((refresh_ms * 2.0).max(20.0));

    // Monotonik günlük var mı? Bir kez dene ve sonucu SÖYLE.
    let mono = h.log_trace("main", 1).await.is_ok();

    println!("Katman : {layer}");
    println!("Refresh: {refresh_ms:.2} ms ({:.1} Hz)  ->  örnekleme {interval} ms",
        1000.0 / refresh_ms.max(0.001));
    println!("Eşik   : takılma >{jank_ms:.1} ms   donma >{:.0} ms", STALL_MS);
    println!("Günlük : {}", if mono { "monotonik (tam hizalı)" }
                            else { "duvar saati (helper eski — hizalama yaklaşık)" });
    println!("Süre   : {duration_s}s — SORUNU YAŞADIĞIN ŞEYİ YAP");

    // Günlük gürültüsünü ölç: teşhis penceresinin ne kadarını kaplıyor?
    // Ölçüm FİLTRESİZ yapılmalı, yoksa susturduğumuz şeyi göremeyiz.
    let noise = measure_log_noise(&h).await;
    if let Some((tag, rate)) = noise.first() {
        if *rate > 50.0 {
            println!("Gürültü: '{tag}' saniyede {rate:.0} satır yazıyor                       — teşhis penceresini daraltıyor");
        }
    }
    println!();

    let mut fd = FrameData::new();
    let mut host: Vec<(f64, HostSample)> = Vec::new();
    let mut cpu = CpuMeter::default();
    let mut sink = LogSink::default();
    let mut stalls: Vec<Stall> = Vec::new();

    let mut frame_tick = tokio::time::interval(
        std::time::Duration::from_millis(interval));
    frame_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut host_tick = tokio::time::interval(std::time::Duration::from_secs(1));
    let mut log_tick = tokio::time::interval(std::time::Duration::from_secs(2));
    let mut ui_tick = tokio::time::interval(std::time::Duration::from_secs(2));

    let deadline = tokio::time::Instant::now()
        + std::time::Duration::from_secs(duration_s);
    // İzlemenin BAŞLANGICI: bundan önceki günlük olayları rapora
    // girmemeli. `logcat -t N` halkanın son N satırını verir ve sistem
    // sessizken bu DAKİKALARI kapsar; onları saymak "20 saniyede 60 olay
    // oldu" yalanını söyletiyordu.
    let t_start_ms = hostsample::monotonic_ms();
    let mut last_frame_ms = hostsample::monotonic_ms();
    let mut last_seen: Option<f64> = None;
    let mut in_stall = false;

    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            _ = tokio::signal::ctrl_c() => { println!(); break; }

            _ = frame_tick.tick() => {
                let Ok(raw) = h.surface_latency(&layer).await else { continue };
                let Some(s) = parse_latency(&raw) else { continue };
                fd.add(&s);
                let now = hostsample::monotonic_ms();
                // "Yeni kare geldi mi": tamponun EN SON karesi ilerlediyse.
                if fd.last_frame_ms() != last_seen {
                    last_seen = fd.last_frame_ms();
                    last_frame_ms = now;
                    if in_stall {
                        in_stall = false;
                        if let Some(st) = stalls.last_mut() { st.end_ms = Some(now); }
                        let d = (now - stalls.last().map(|s| s.start_ms)
                                 .unwrap_or(now)) / 1000.0;
                        println!("  \x1b[32m✓ donma bitti\x1b[0m ({d:.1} sn sürdü)");
                    }
                } else if !in_stall && now - last_frame_ms > STALL_MS {
                    in_stall = true;
                    stalls.push(Stall { start_ms: last_frame_ms,
                                        end_ms: None, log: Vec::new() });
                    println!("  \x1b[33m⏸ DONMA başladı\x1b[0m — günlük yakalanıyor…");
                }
            }

            _ = host_tick.tick() => {
                host.push((hostsample::monotonic_ms(),
                           hostsample::sample(&mut cpu).await));
            }

            _ = log_tick.tick() => {
                let (evs, _) = fetch_log(&h, mono).await;
                let fresh = sink.add(evs);
                // Donma SÜRERKEN kanıtı hemen göster ve sakla: logcat
                // halkası dolarsa sonradan bakmak geç kalır.
                if in_stall {
                    if let Some(st) = stalls.last_mut() {
                        for e in &fresh {
                            println!("      \x1b[2m{}\x1b[0m {}: {}",
                                     e.kind.label(), e.tag, e.msg);
                        }
                        st.log.extend(fresh);
                    }
                }
            }

            _ = ui_tick.tick() => {
                if in_stall { continue; }
                let n = fd.interval_count();
                if n < 5 { continue; }
                let gpu = host.last().map(|(_, s)| s.gpu_pct).unwrap_or(0.0);
                let cpup = host.last().map(|(_, s)| s.cpu_pct).unwrap_or(0.0);
                print!("\r  {:5.1} FPS   p99 {:6.2} ms   jank {:3}   \
                        GPU %{gpu:.0}  CPU %{cpup:.0}   ({n} aralık)      ",
                    1000.0 / fd.percentile(50.0).max(0.001),
                    fd.percentile(99.0), fd.jank_count(1.5));
                use std::io::Write;
                let _ = std::io::stdout().flush();
            }
        }
    }
    println!();
    sink.events.retain(|e| e.t_ms >= t_start_ms - 250.0);
    report(&fd, &host, &sink, &stalls, jank_ms, mono, &noise);
    Ok(())
}

/// Günlük yazma hızını etiket başına ölçer.
///
/// İki filtresiz çekim arasındaki FARK sayılıyor; tek çekim yalnızca
/// halkada ne olduğunu söyler, hızını söylemez.
async fn measure_log_noise(h: &HelperClient) -> Vec<(String, f64)> {
    let Ok(a) = h.logcat("main", 2000).await else { return Vec::new() };
    let t0 = hostsample::monotonic_ms();
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    let Ok(b) = h.logcat("main", 2000).await else { return Vec::new() };
    let span = (hostsample::monotonic_ms() - t0) / 1000.0;
    // İkinci çekimde YENİ olan satırlar.
    let old: std::collections::HashSet<&str> = a.lines().collect();
    let fresh: String = b.lines().filter(|l| !old.contains(l))
        .collect::<Vec<_>>().join("\n");
    trace::tag_rates(&fresh, span)
}

fn report(fd: &FrameData, host: &[(f64, HostSample)], sink: &LogSink,
          stalls: &[Stall], jank_ms: f64, mono: bool, noise: &[(String, f64)]) {
    let line = "=".repeat(64);
    println!("{line}");

    if fd.interval_count() < 30 {
        println!("Yeterli kare verisi yok ({} aralık).", fd.interval_count());
        println!("Oyun ön planda ve HAREKETLİ miydi? Durgun ekran kare üretmez.");
        println!("{line}");
        return;
    }

    println!("KARE   {} aralık, {} tekil kare, kapsam %{:.0}",
        fd.interval_count(), fd.frame_count(), fd.coverage_pct());
    if fd.is_below_refresh() {
        println!("       oyun {:.0} FPS'e kilitli (ekran {:.0} Hz) — takılma ölçütü \
                  oyunun periyodu",
            fd.target_fps(), 1000.0 / fd.refresh_ms().max(0.001));
    }
    println!("  p50 {:.2} ms ({:.0} FPS)   p99 {:.2} ms   en kötü {:.2} ms   \
              jank>1.5x %{:.2}",
        fd.percentile(50.0), 1000.0 / fd.percentile(50.0).max(0.001),
        fd.percentile(99.0), fd.percentile(100.0), fd.jank_pct(1.5));

    // --- donmalar ---
    let fd_stalls = fd.stalls_ms();
    if !stalls.is_empty() || !fd_stalls.is_empty() {
        println!();
        println!("DONMALAR");
        for s in stalls {
            let d = s.end_ms.map(|e| (e - s.start_ms) / 1000.0);
            match d {
                Some(d) => println!("  {d:.1} sn"),
                None => println!("  (ölçüm bitene kadar sürüyordu)"),
            }
            if s.log.is_empty() {
                println!("      Android tarafında hiçbir olay yok — sebep \
                          büyük ihtimalle konteynerin DIŞINDA.");
            }
            for e in s.log.iter().take(6) {
                println!("      {:<24} {}: {}", e.kind.label(), e.tag, e.msg);
            }
        }
        for (t, d) in &fd_stalls {
            if stalls.iter().any(|s| (s.start_ms - t).abs() < 2000.0) { continue; }
            println!("  {:.1} sn (kare verisinden)", d / 1000.0);
            let ev: Vec<&LogEvent> = sink.events.iter()
                .filter(|e| e.t_ms >= t - 500.0 && e.t_ms <= t + d + 500.0)
                .collect();
            for e in ev.iter().take(4) {
                println!("      {:<24} {}: {}", e.kind.label(), e.tag, e.msg);
            }
        }
    }

    // --- takılmalar ve korelasyon ---
    let iv = fd.intervals_ms();
    let mut hs = trace::hitches(&iv, jank_ms);
    // Pencere, zaman hizalamasının DOĞRULUĞUNA bağlı olmalı.
    //
    // Monotonik günlükte hizalama tam; dar pencere doğru eşleştirme
    // yapar. Duvar saatinde hata ±1 sn'ye kadar çıkıyor ve dar pencere
    // hiçbir şey eşleştiremiyor — araç da "açıklanamadı" diyerek sebebi
    // Android'in dışında sanmaya itiyordu. Gerçekte yaşandı.
    let (before, after) = if mono { (150.0, 80.0) } else { (1500.0, 1500.0) };
    trace::correlate(&mut hs, &sink.events, before, after);
    hs.sort_by(|a, b| b.len_ms.partial_cmp(&a.len_ms).unwrap_or(std::cmp::Ordering::Equal));

    if !hs.is_empty() {
        println!();
        println!("EN UZUN TAKILMALAR");
        for hh in hs.iter().take(6) {
            println!("  {:.1} ms", hh.len_ms);
            if hh.evidence.is_empty() {
                println!("      (Android tarafında eşzamanlı olay yok)");
            }
            for e in &hh.evidence {
                println!("      {:<24} {}: {}", e.kind.label(), e.tag, e.msg);
            }
            // Aynı ana denk gelen host örneği.
            if let Some((_, s)) = host.iter()
                .min_by(|a, b| (a.0 - hh.t_ms).abs()
                    .partial_cmp(&(b.0 - hh.t_ms).abs()).unwrap())
            {
                println!("      host: GPU %{:.0}  CPU %{:.0}  mem.baskı {:.2}",
                         s.gpu_pct, s.cpu_pct, s.mem_pressure);
            }
        }
    }

    // --- günlük imzaları ---
    if !sink.events.is_empty() {
        let mut tally: std::collections::HashMap<Kind, usize> = Default::default();
        for e in &sink.events { *tally.entry(e.kind).or_default() += 1; }
        let mut v: Vec<_> = tally.into_iter().collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        println!();
        println!("GÜNLÜK İMZALARI ({} olay, izleme penceresinde)",
                 sink.events.len());
        for (k, n) in &v { println!("  {:<26} {n}", k.label()); }
        // Girdi yolu kaybı tek başına bir uyarıyı hak ediyor: enjeksiyon
        // sessizce ölür ve kullanıcı bunu "fare çalışmıyor" diye yaşar.
        if v.iter().any(|(k, _)| *k == Kind::Input) {
            println!();
            for l in wrap("UYARI: Android dokunuş cihazımızı kaybetti/yeniden                 kurdu. Bu olduğunda elimizdeki boru tanıtıcısı ölür ve                 enjeksiyon durur. `liw keymap stop && liw keymap start --grab`                 ile geri gelir.", 62) { println!("  {l}"); }
        }
    } else if !mono {
        println!();
        println!("Günlükten hiç olay çıkmadı. Helper eski sürüm olduğu için \
                  zaman hizalaması yaklaşık;");
        println!("`sudo bash dist/install-helper.sh` sonrası teşhis belirgin \
                  şekilde keskinleşir.");
    }

    // --- host ---
    if !host.is_empty() {
        println!();
        println!("HOST ({} örnek)", host.len());
        for (label, vals, unit) in [
            ("GPU", host.iter().map(|(_, h)| h.gpu_pct).collect::<Vec<_>>(), "%"),
            ("CPU", host.iter().map(|(_, h)| h.cpu_pct).collect(), "%"),
            ("mem.baskı", host.iter().map(|(_, h)| h.mem_pressure).collect(), ""),
        ] {
            let (mean, peak) = hostsample::summarise(&vals);
            println!("  {label:<10} ort {mean:7.1}{unit}   tepe {peak:7.1}{unit}");
        }
        // VRAM'i TOPLAMLA birlikte göster. Ham "4094 MB" doluymuş gibi
        // okunuyor; 12288'in üçte biri olduğu ancak oranla anlaşılıyor.
        let total = host.iter().map(|(_, h)| h.vram_total_mb).fold(0.0, f64::max);
        let (vmean, vpeak) = hostsample::summarise(
            &host.iter().map(|(_, h)| h.vram_mb).collect::<Vec<_>>());
        if total > 0.0 {
            println!("  {:<10} ort {vmean:7.0}MB   tepe {vpeak:7.0}MB  / {total:.0}MB                       (tepe %{:.0})", "VRAM", 100.0 * vpeak / total);
        } else {
            println!("  {:<10} ort {vmean:7.0}MB   tepe {vpeak:7.0}MB                        (toplam okunamadı)", "VRAM");
        }
    }

    // --- günlük gürültüsü ---
    if let Some((tag, rate)) = noise.first() {
        if *rate > 50.0 {
            println!();
            println!("GÜNLÜK GÜRÜLTÜSÜ");
            println!("  '{tag}' saniyede {rate:.0} satır yazıyor.");
            for l in wrap(&format!(
                "Bu iki şeye mal oluyor: teşhis kuyruğu saniyeler yerine                  milisaniyeleri kapsıyor (olaylar görülmeden düşüyor), ve                  her satır logd'ye kopyalanıyor. Susturmak için:                  waydroid prop set log.tag.{tag} S"), 62)
            { println!("  {l}"); }
        }
    }

    // --- hüküm ---
    println!();
    println!("HÜKÜM");
    for v in trace::verdicts(&hs, fd.frame_count(), fd.jank_pct(1.5)) {
        println!("  ▸ {}", v.headline);
        for l in wrap(&v.detail, 62) { println!("    {l}"); }
    }

    // Android'de kanıt bulunamayan takılmaların host tarafındaki karşılığı.
    let mut facts = trace::HostFacts::default();
    let vram_total = host.iter().map(|(_, h)| h.vram_total_mb).fold(0.0, f64::max);
    for hh in hs.iter().filter(|h| h.evidence.is_empty()) {
        facts.unexplained += 1;
        let Some((_, s)) = host.iter().min_by(|a, b| (a.0 - hh.t_ms).abs()
            .partial_cmp(&(b.0 - hh.t_ms).abs()).unwrap()) else { continue };
        if s.gpu_pct >= 95.0 { facts.gpu_saturated += 1; }
        if vram_total > 0.0 && s.vram_mb / vram_total >= 0.92 {
            facts.vram_saturated += 1;
        }
        if s.mem_pressure >= 10.0 { facts.mem_pressed += 1; }
    }
    if let Some(v) = trace::host_verdict(&facts) {
        println!("  ▸ {}", v.headline);
        for l in wrap(&v.detail, 62) { println!("    {l}"); }
    } else if facts.unexplained > 0 {
        println!("  ▸ Host kaynakları da doymamış");
        for l in wrap("Ne Android tarafında olay var ne de host'ta doyma.             Geriye compositor/sunum yolu ve güç yönetimi kalıyor:             `liw perf status` çıktısına bak. Günlük hizalaması yaklaşıksa             (helper eski) kanıt kaçmış da olabilir.", 62)
        { println!("    {l}"); }
    }
    if !mono {
        println!();
        println!("  NOT: helper eski sürüm — günlük duvar saatiyle geliyor ve");
        println!("  eşleştirme ±1 sn belirsiz. `sudo bash dist/install-helper.sh`");
        println!("  sonrası korelasyon kare hassasiyetine iner.");
    }
    println!("{line}");
}

/// Basit satır sarma; hüküm metinleri uzun ve terminalde okunmalı.
fn wrap(s: &str, w: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for word in s.split_whitespace() {
        if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > w {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() { cur.push(' '); }
        cur.push_str(word);
    }
    if !cur.is_empty() { out.push(cur); }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use liw_core::trace::Kind;

    /// Üst üste binen logcat çekimleri aynı olayı BİR kez saymalı.
    ///
    /// `logcat -t N` her çağrıda son N satırı verir; tekilleştirmeden
    /// biriktirmek aynı olayı onlarca kez sayar ve hükmü tamamen bozar.
    #[test]
    fn overlapping_log_fetches_are_deduplicated() {
        let mut s = LogSink::default();
        let e = |t: f64| LogEvent { t_ms: t, pid: 7, tag: "art".into(),
            kind: Kind::Gc, msg: "GC freed".into() };
        assert_eq!(s.add(vec![e(1.0), e(2.0)]).len(), 2);
        // İkinci çekim: biri eski, biri yeni.
        assert_eq!(s.add(vec![e(2.0), e(3.0)]).len(), 1);
        assert_eq!(s.events.len(), 3);
    }

    #[test]
    fn wrap_keeps_every_word_within_width() {
        let t = "kısa kelimeler ile uzun bir metin sarılmalı ve hiçbir \
                 kelime kaybolmamalı";
        let ls = wrap(t, 20);
        assert!(ls.iter().all(|l| l.chars().count() <= 20), "{ls:?}");
        assert_eq!(ls.join(" ").split_whitespace().count(),
                   t.split_whitespace().count());
    }
}
