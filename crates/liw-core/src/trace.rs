//! Takılma teşhisi: kare zamanlaması + Android günlüğü + host kaynakları.
//!
//! # Neden ayrı bir modül
//!
//! `bench` "ne kadar kötü" sorusunu ölçüyor, `perf` "hangi kaldıraçlar
//! açık" diye bakıyor. İkisi de **neden** sorusuna cevap vermiyor.
//! Kullanıcının gerçek soruları şunlar: "bu FPS düşüşü fare sisteminden
//! mi geliyor, oyunun kendisinden mi?" ve "ESC menüsü neden bir dakika
//! yükleniyor?"
//!
//! Cevap ancak KORELASYONLA verilebilir: takılmanın olduğu anda Android
//! ne yapıyordu. Bu modül o eşleştirmeyi yapar.
//!
//! # Saf tutuluyor
//!
//! Burada I/O yok; ham metin girer, bulgu çıkar. Böylece gerçek bir
//! takılma beklemeden, kaydedilmiş günlüklerle test edilebiliyor —
//! aksi halde teşhis mantığını doğrulamanın yolu yok.

use serde::{Deserialize, Serialize};

/// Bir günlük satırının ne anlattığı.
///
/// Sıralama ÖNEMLİ değil ama ayrım önemli: "GC oldu" ile "ağ zaman
/// aşımına uğradı" tamamen farklı sorunlar ve farklı çözümleri var.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Kind {
    /// Uygulama ana iş parçacığı çok iş yapmış (Choreographer/HWUI).
    MainThread,
    /// Çöp toplama duraklaması.
    Gc,
    /// Kilit çekişmesi (monitor contention).
    Lock,
    /// Uygulama yanıt vermiyor.
    Anr,
    /// Binder çağrısı yavaş/başarısız.
    Binder,
    /// Ağ: DNS çözülemedi, bağlantı zaman aşımı.
    Network,
    /// ARM köprüsü (libhoudini) veya derleme/doğrulama.
    ArmBridge,
    /// Ekran birleştirici kare kaçırdı.
    Composer,
    /// Genel "yavaş işlem" uyarısı.
    Slow,
    /// Bir süreç çöktü / yığın dökümü alındı.
    Crash,
    /// Reklam aracılık yığını çalışıyor (IronSource/Unity/Pangle/AdMob).
    AdStack,
    /// Oyun DURAKLATILDI: başka bir etkinlik öne geçti.
    ///
    /// Reklam, sistem diyaloğu, izin isteği... Oyun duraklatılınca kare
    /// üretmeyi bırakır. Araç bunu "donma" sanıyordu ve üstüne "sebep
    /// konteynerin dışında" diye YANLIŞ hüküm veriyordu.
    ///
    /// Gerçekte yaşandı: menüye girince 8 saniyelik "donma" göründü;
    /// sebebi oyunun kendi reklam etkinliğiydi (`gms.ads.AdActivity`).
    /// Sistemde hiçbir sorun yoktu.
    Paused,
    /// GİRDİ YOLU: Android girdi cihazını kaybetti/yeniden buldu.
    ///
    /// Kullanıcının doğrudan sorduğu şey: "bu takılma fare sisteminden mi
    /// geliyor?" Bunu ancak girdi katmanının kendi günlüğü söyleyebilir.
    Input,
}

impl Kind {
    /// Kullanıcıya gösterilecek ad.
    pub fn label(self) -> &'static str {
        match self {
            Kind::MainThread => "uygulama ana iş parçacığı",
            Kind::Gc => "çöp toplama",
            Kind::Lock => "kilit çekişmesi",
            Kind::Anr => "ANR (yanıt yok)",
            Kind::Binder => "binder",
            Kind::Network => "ağ",
            Kind::ArmBridge => "ARM köprüsü / derleme",
            Kind::Composer => "ekran birleştirici",
            Kind::Slow => "yavaş işlem",
            Kind::Crash => "çökme / yığın dökümü",
            Kind::Paused => "oyun duraklatıldı (başka etkinlik öne geçti)",
            Kind::AdStack => "reklam aracılık yığını",
            Kind::Input => "GİRDİ YOLU",
        }
    }
    /// Bir takılmayı açıklama gücü. Yüksek olan hükümde öne çıkar.
    ///
    /// Ağ ve ANR en yüksek: saniyeler süren donmaları TEK BAŞINA
    /// açıklayabilirler. GC en düşük: sürekli olur ve çoğu zaman zararsız.
    pub fn weight(self) -> u32 {
        match self {
            Kind::Anr => 100,
            // Girdi yolu kopması bizim katmanımız: en yüksek öncelikte
            // görünmeli, çünkü tek düzeltebileceğimiz şey o.
            Kind::Input => 95,
            Kind::Crash => 92,
            Kind::Network => 90,
            // Duraklamanın hemen üstünde: duraklamanın NEDENİNİ söyler.
            Kind::AdStack => 89,
            // Gerçek arızaların ALTINDA.
            //
            // Duraklama bir kare boşluğunu AÇIKLAR ama kendisi arıza
            // değildir; üstelik gerçek bir arızayla BİRLİKTE olabilir.
            // Ölçüldü: menüde açılan reklam etkinliği hem duraklama hem
            // ANR üretti. Duraklamayı öne koymak ANR'yi gizlerdi —
            // kullanıcı "sorun yok" duyup gerçek hatayı kaçırırdı.
            Kind::Paused => 88,
            Kind::ArmBridge => 70,
            Kind::Lock => 60,
            Kind::Binder => 50,
            Kind::MainThread => 40,
            Kind::Slow => 30,
            Kind::Composer => 20,
            Kind::Gc => 10,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogEvent {
    /// Monotonik zaman (ms). Kare zaman damgalarıyla aynı eksende.
    pub t_ms: f64,
    pub pid: u32,
    pub tag: String,
    pub kind: Kind,
    /// Kısaltılmış mesaj.
    pub msg: String,
}

/// Bir günlük satırını sınıflandırır.
///
/// Desenler gerçek Android çıktısından alındı. Eşleşmeyen satır `None`
/// döner ve TAMAMEN yok sayılır: teşhis aracının gürültüyü kanıt gibi
/// göstermesi, hiç kanıt göstermemesinden kötüdür.
pub fn classify(tag: &str, msg: &str) -> Option<Kind> {
    let m = msg;
    // Sıra önemli: bir satır birden çok desene uyabilir, en açıklayıcı
    // olan kazanmalı.
    if tag == "ActivityManager" && m.contains("ANR in") { return Some(Kind::Anr); }
    // Girdi yolu: dokunuş borumuz kaybolduysa enjeksiyon SESSİZCE ölür.
    // Ölçülen: ekran hotplug'ında hwcomposer FIFO'yu silip yeniden
    // yaratıyor ve elimizdeki tanıtıcı sahipsiz kalıyor.
    if tag == "EventHub" && (m.contains("wl_touch_events")
        || m.contains("wl_pointer_events") || m.contains("wayland_touch"))
    { return Some(Kind::Input); }
    // Oyun duraklatıldı: kare gelmemesi ARIZA DEĞİL.
    if tag == "InputDispatcher" && m.contains("because it is paused") {
        return Some(Kind::Paused);
    }
    if tag == "ActivityTaskManager" && m.contains("START u")
        && (m.contains("AdActivity") || m.contains("ads."))
    {
        return Some(Kind::Paused);
    }
    // Reklam aracılık yığını: menüde takılmanın en sık sebebi.
    //
    // Ölçüldü: IronSource + UnityAds + Pangle + Google Ads sırayla
    // deneniyor, video reklam YAZILIMDA çözülüyor (OMX.google.vp9 /
    // SoftAAC2 — Waydroid'de donanım video çözücü yok) ve oyunun ana iş
    // parçacığı bloklanıp ANR üretiyor.
    if matches!(tag, "UnityAds" | "Ads" | "ironSourceSDK")
        || tag.ends_with("MediationAdapter")
    {
        return Some(Kind::AdStack);
    }
    if tag == "tombstoned" && m.contains("crash request") { return Some(Kind::Crash); }
    if m.contains("Fatal signal") || m.contains("Collecting stacks for native pid") {
        return Some(Kind::Crash);
    }
    if m.contains("Unable to resolve host")
        || m.contains("UnknownHostException")
        || m.contains("SocketTimeoutException")
        || m.contains("ConnectException")
        || m.contains("failed to connect to")
        || m.contains("Connection timed out")
    { return Some(Kind::Network); }
    if m.contains("Long monitor contention") { return Some(Kind::Lock); }
    if tag.starts_with("Choreographer") && m.contains("Skipped") {
        return Some(Kind::MainThread);
    }
    if m.starts_with("Davey!") { return Some(Kind::MainThread); }
    if tag == "houdini" || m.contains("houdini")
        || tag == "dex2oat" || m.contains("dex2oat")
        || m.contains("Verification of") || m.contains("Compilation of")
    { return Some(Kind::ArmBridge); }
    if m.contains("Slow Binder") || m.contains("binder thread pool")
        || m.contains("Binder transaction failed")
    { return Some(Kind::Binder); }
    if m.contains("Missed HW composer frame")
        || m.contains("GraphicBufferAllocator") && m.contains("failed")
    { return Some(Kind::Composer); }
    if m.contains("Slow operation") || m.contains("Slow Looper")
        || m.contains("Slow dispatch")
    { return Some(Kind::Slow); }
    // GC en sona: "GC freed" satırları çok sık ve çoğu zararsız.
    if tag == "art" && m.contains("GC freed") { return Some(Kind::Gc); }
    None
}

/// Bir logcat satırını ayrıştırır.
///
/// İKİ biçim destekleniyor:
///
/// * `-v monotonic -v threadtime` → `   1234.567  1234  1256 I Tag: msg`
///   Tercih edilen: doğrudan `CLOCK_MONOTONIC`, kare zaman damgalarıyla
///   aynı eksen, saat dilimi ve yıl sorunu yok.
/// * varsayılan `threadtime` → `08-28 00:48:47.391  88  88 I Tag: msg`
///   Geri düşüş: yalnızca günün saati var, `now_ms` ile hizalanıyor.
///
/// İkisini de desteklemek şart çünkü helper'ın eski sürümü yalnızca
/// ikincisini veriyor ve teşhis aracının onunla da çalışması gerekiyor.
pub fn parse_line(line: &str, now_ms: f64, now_secs_of_day: f64) -> Option<LogEvent> {
    let l = line.trim_start();
    if l.is_empty() || l.starts_with("---") { return None; }
    let mut it = l.split_whitespace();
    let first = it.next()?;

    let (t_ms, mut it) = if first.contains('-') {
        // Duvar saati biçimi: tarih, sonra saat.
        let clock = it.next()?;
        let sod = secs_of_day(clock)?;
        // Gün sınırını sar: günlük satırı bizden "sonra" görünüyorsa dün.
        let mut age = now_secs_of_day - sod;
        if age < -1.0 { age += 86400.0; }
        (now_ms - age * 1000.0, it)
    } else {
        // Monotonik biçim: doğrudan saniye.
        (first.parse::<f64>().ok()? * 1000.0, it)
    };

    let pid: u32 = it.next()?.parse().ok()?;
    let _tid = it.next()?;
    let _level = it.next()?;
    // Kalanı "Tag: mesaj".
    let rest = it.collect::<Vec<_>>().join(" ");
    let (tag, msg) = rest.split_once(':')?;
    let (tag, msg) = (tag.trim(), msg.trim());
    let kind = classify(tag, msg)?;
    Some(LogEvent {
        t_ms, pid, tag: tag.to_string(), kind,
        msg: truncate(msg, 160),
    })
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n { return s.to_string(); }
    s.chars().take(n).collect::<String>() + "…"
}

/// `HH:MM:SS.mmm` → gün içi saniye.
fn secs_of_day(clock: &str) -> Option<f64> {
    let mut p = clock.split(':');
    let h: f64 = p.next()?.parse().ok()?;
    let m: f64 = p.next()?.parse().ok()?;
    let s: f64 = p.next()?.parse().ok()?;
    Some(h * 3600.0 + m * 60.0 + s)
}

pub fn parse_log(raw: &str, now_ms: f64, now_secs_of_day: f64) -> Vec<LogEvent> {
    raw.lines()
        .filter_map(|l| parse_line(l, now_ms, now_secs_of_day))
        .collect()
}

/// Günlük satırlarını etikete göre sayar (gürültü ölçümü).
///
/// Neden gerekli: Waydroid'in hwcomposer'ı kare başına iki satır yazıyor.
/// 180 Hz'de saniyede ~360 satır eder. Bunun iki sonucu var ve ikisi de
/// kullanıcıyı ilgilendirir:
///
/// 1. Teşhis penceresi çöker — 400 satırlık kuyruk bir saniyeyi kapsar
///    ve aradığımız olaylar görülmeden halkadan düşer.
/// 2. Her satır `logd`'ye kopyalanıyor; bedava değil.
pub fn tag_rates(raw: &str, span_s: f64) -> Vec<(String, f64)> {
    let mut n: std::collections::HashMap<String, usize> = Default::default();
    for l in raw.lines() {
        let Some(tag) = tag_of(l) else { continue };
        *n.entry(tag).or_default() += 1;
    }
    let span = span_s.max(0.001);
    let mut v: Vec<(String, f64)> = n.into_iter()
        .map(|(t, c)| (t, c as f64 / span)).collect();
    v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    v
}

/// Satırdan etiketi çıkarır (biçimden bağımsız).
fn tag_of(line: &str) -> Option<String> {
    let l = line.trim_start();
    if l.is_empty() || l.starts_with("---") { return None; }
    let mut it = l.split_whitespace();
    let first = it.next()?;
    if first.contains('-') { it.next()?; }
    it.next()?; it.next()?; it.next()?;         // pid tid level
    let rest = it.collect::<Vec<_>>().join(" ");
    let (tag, _) = rest.split_once(':')?;
    Some(tag.trim().to_string())
}

/// Uzun bir kare aralığı ve onu açıklayan kanıtlar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hitch {
    /// Takılmanın BAŞLADIĞI an (ms, monotonik).
    pub t_ms: f64,
    /// Süresi (ms).
    pub len_ms: f64,
    pub evidence: Vec<LogEvent>,
}

/// Eşiği aşan kare aralıklarını takılma sayar.
///
/// Eşik dışarıdan veriliyor çünkü doğru değer yenileme hızına bağlı:
/// 60 Hz'de 33 ms takılma sayılmaz, 180 Hz'de sayılır.
pub fn hitches(intervals: &[(f64, f64)], threshold_ms: f64) -> Vec<Hitch> {
    intervals.iter()
        .filter(|(_, d)| *d >= threshold_ms)
        .map(|(t, d)| Hitch { t_ms: *t, len_ms: *d, evidence: Vec::new() })
        .collect()
}

/// Her takılmaya, zaman penceresine düşen günlük olaylarını iliştirir.
///
/// Pencere takılmadan ÖNCE de bakıyor: sebep genelde takılmadan hemen
/// önce loglanır (GC başlangıcı, ağ isteği), sonuç ise sonra.
pub fn correlate(hs: &mut [Hitch], events: &[LogEvent], before_ms: f64, after_ms: f64) {
    for h in hs.iter_mut() {
        let lo = h.t_ms - before_ms;
        let hi = h.t_ms + h.len_ms + after_ms;
        h.evidence = events.iter()
            .filter(|e| e.t_ms >= lo && e.t_ms <= hi)
            .cloned()
            .collect();
        // En açıklayıcı kanıt başa.
        h.evidence.sort_by(|a, b| b.kind.weight().cmp(&a.kind.weight()));
        h.evidence.truncate(4);
    }
}

/// Bir teşhis sonucu.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    pub kind: Option<Kind>,
    pub headline: String,
    pub detail: String,
}

/// Takılmalardan ve olaylardan hüküm çıkarır.
///
/// Kural: KANIT YOKSA SUÇLAMA YOK. "Muhtemelen GPU" demek, ölçmeden
/// tahmin etmektir ve kullanıcıyı yanlış yere bakmaya iter. Kanıt
/// bulunmadığında bunu açıkça söylüyoruz.
pub fn verdicts(hs: &[Hitch], total_frames: usize, jank_pct: f64) -> Vec<Verdict> {
    let mut out = Vec::new();
    if hs.is_empty() {
        out.push(Verdict {
            kind: None,
            headline: "Takılma yakalanmadı".into(),
            detail: format!("{total_frames} kare izlendi, eşiği aşan aralık yok. \
                             Sorun yaşadığın anı yakalamak için o sırada \
                             çalıştır."),
        });
        return out;
    }

    // Kanıt türlerini takılma SAYISINA göre topla (satır sayısına göre
    // değil): tek bir takılmada 50 GC satırı olması GC'yi baş suçlu
    // yapmaz, 50 ayrı takılmanın her birinde GC olması yapar.
    let mut tally: std::collections::HashMap<Kind, usize> = Default::default();
    for h in hs {
        let mut seen: std::collections::HashSet<Kind> = Default::default();
        for e in &h.evidence { seen.insert(e.kind); }
        for k in seen { *tally.entry(k).or_default() += 1; }
    }

    let unexplained = hs.iter().filter(|h| h.evidence.is_empty()).count();
    let mut ranked: Vec<(Kind, usize)> = tally.into_iter().collect();
    ranked.sort_by(|a, b| (b.1 * b.0.weight() as usize)
        .cmp(&(a.1 * a.0.weight() as usize)));

    for (k, n) in ranked.iter().take(3) {
        let pct = 100.0 * *n as f64 / hs.len() as f64;
        out.push(Verdict {
            kind: Some(*k),
            headline: format!("{} — {n}/{} takılmada ({pct:.0}%)",
                              k.label(), hs.len()),
            detail: explain(*k).into(),
        });
    }
    if unexplained > 0 {
        out.push(Verdict {
            kind: None,
            headline: format!("{unexplained}/{} takılma AÇIKLANAMADI", hs.len()),
            detail: "Bu takılmaların yanında Android tarafında eşzamanlı bir \
                     olay bulunamadı. Aşağıdaki host bulgularına bak."
                .into(),
        });
    }
    if jank_pct > 5.0 {
        out.push(Verdict {
            kind: None,
            headline: format!("Jank oranı yüksek: %{jank_pct:.1}"),
            detail: "Tek tük takılma değil, sürekli bir sorun var. \
                     Önce çözünürlük/yenileme hızını düşürüp tekrar ölç: \
                     düzeliyorsa GPU sınırındasın.".into(),
        });
    }
    out
}

/// Açıklanamayan takılmaların host tarafındaki karşılığı.
#[derive(Debug, Clone, Copy, Default)]
pub struct HostFacts {
    /// Kaç açıklanamayan takılmada GPU doymuştu.
    pub gpu_saturated: usize,
    /// Kaç açıklanamayan takılmada VRAM doluya yakındı.
    pub vram_saturated: usize,
    /// Kaç açıklanamayan takılmada bellek baskısı yüksekti.
    pub mem_pressed: usize,
    pub unexplained: usize,
}

/// Android tarafında kanıt yokken host tarafına bakar.
///
/// "Açıklanamadı" demekle yetinmek kullanıcıyı boşlukta bırakıyor;
/// ölçtüğümüz host verisi çoğu zaman cevabı zaten içeriyor. Yine de
/// KANIT YOKSA SUÇLAMA YOK kuralı geçerli: hiçbir host göstergesi
/// doymamışsa `None` döner.
pub fn host_verdict(f: &HostFacts) -> Option<Verdict> {
    if f.unexplained == 0 { return None; }
    let half = f.unexplained.div_ceil(2);
    if f.gpu_saturated >= half {
        return Some(Verdict {
            kind: None,
            headline: format!(
                "Açıklanamayan takılmaların {}/{}'inde host GPU doymuştu",
                f.gpu_saturated, f.unexplained),
            detail: "Android tarafında olay yok ama host GPU'su o anlarda                      tavandaydı. Çözünürlüğü ya da yenileme hızını düşürüp                      tekrar ölç; takılmalar azalıyorsa GPU sınırındasın."
                .into(),
        });
    }
    if f.vram_saturated >= half {
        return Some(Verdict {
            kind: None,
            headline: format!(
                "Açıklanamayan takılmaların {}/{}'inde VRAM doluya yakındı",
                f.vram_saturated, f.unexplained),
            detail: "VRAM dolduğunda sürücü doku takası yapar ve bu tek tük                      uzun kare üretir. Başka GPU tüketen uygulamaları kapat."
                .into(),
        });
    }
    if f.mem_pressed >= half {
        return Some(Verdict {
            kind: None,
            headline: format!(
                "Açıklanamayan takılmaların {}/{}'inde bellek baskısı yüksekti",
                f.mem_pressed, f.unexplained),
            detail: "Host belleği sıkışınca sayfa geri alımı bekleme üretir.                      `/proc/pressure/memory` ölçümü bunu gösteriyor."
                .into(),
        });
    }
    None
}

fn explain(k: Kind) -> &'static str {
    match k {
        Kind::AdStack =>
            "Reklam aracılık yığını çalışıyor: SDK sırayla birden çok \
             reklam ağını deniyor ve video reklamı YAZILIMDA çözüyor — \
             Waydroid'de donanım video çözücü yok. Bu, oyunun ana iş \
             parçacığını saniyelerce bloklayıp ANR'ye kadar gidebiliyor. \
             Uygulamanın kendi davranışı; bizim katmanımızdan bağımsız.",
        Kind::Paused =>
            "Oyun DURAKLATILDI: başka bir etkinlik öne geçti (reklam, \
             sistem diyaloğu, izin isteği). Duraklamış uygulama kare \
             üretmez, yani kare boşluğunun açıklaması budur. \
             DİKKAT: bu tek başına 'sorun yok' demek değildir — \
             yukarıda ANR ya da çökme de listelendiyse asıl mesele \
             odur, duraklama yalnızca onun görünen yüzüdür.",
        Kind::Network =>
            "Oyun ağ isteği yapıp zaman aşımına uğruyor. Menü açılışında \
             dakikalarca beklemenin en sık nedeni budur: reklam/analitik \
             SDK'sı DNS çözemeyince soket zaman aşımını (genelde 30 sn, \
             iki deneme) bekliyor. `liw net doctor` ile DNS'i doğrula.",
        Kind::Anr =>
            "Uygulama yanıt vermedi. Günlükteki ANR bloğu hangi iş \
             parçacığının nerede takıldığını yazar.",
        Kind::ArmBridge =>
            "ARM kodu x86'ya çevriliyor ya da sınıflar doğrulanıyor. İlk \
             açılışta ve yeni bir ekrana ilk girişte olur; aynı ekran \
             ikinci kez açıldığında sürmüyorsa normaldir.",
        Kind::Lock =>
            "İki iş parçacığı aynı kilidi bekliyor. Uygulamanın kendi \
             sorunu; bizim katmanımızdan bağımsız.",
        Kind::Binder =>
            "Sistem servisi çağrısı yavaş. Genelde system_server yük \
             altında demektir.",
        Kind::MainThread =>
            "Oyun ana iş parçacığında kare süresini aşan iş yapmış. \
             Bizim enjeksiyon katmanımız bunu üretmez — dokunuş olayları \
             ayrı bir yoldan gelir.",
        Kind::Slow =>
            "Sistem kendi işlemini yavaş buldu. Tek başına nedene işaret \
             etmez, diğer kanıtlarla birlikte okunmalı.",
        Kind::Composer =>
            "Ekran birleştirici kare kaçırdı. Host tarafında GPU veya \
             compositor sıkışması olabilir.",
        Kind::Input =>
            "Android girdi cihazımızı kaybetti ya da yeniden kurdu. \
             Dokunuş borusu silinip yaratıldığında (ekran hotplug'ı) \
             elimizdeki tanıtıcı ölür ve enjeksiyon sessizce durur. \
             Keymapper'ı yeniden başlatmak düzeltir.",
        Kind::Crash =>
            "Bir süreç çöktü ya da yığın dökümü alındı. Döküm almak \
             saniyeler sürebilir ve o sırada sistem geneli takılır.",
        Kind::Gc =>
            "Çöp toplama duraklaması. Sürekli olur ve genelde zararsızdır; \
             yalnızca her takılmada görünüyorsa şüphelen.",
    }
}

#[cfg(test)]
mod tests {
    /// GERÇEK GÜNLÜK: menüdeki takılmanın kaynağı reklam aracılık yığını.
    #[test]
    fn ad_mediation_tags_are_recognised() {
        for (tag, msg) in [
            ("UnityAds", "Unity Ads was not able to get current network type"),
            ("IronSourceMediationAdapter", "Loading IronSource interstitial ad"),
            ("UnityMediationAdapter", "Unity Ads is initialized for game ID"),
            ("PangleMediationAdapter", "{"),
            ("ironSourceSDK", "API: f a - Interstitial The requested instance does not exist"),
            ("Ads", "canOpenAppGmsgHandler disabled."),
        ] {
            assert_eq!(super::classify(tag, msg), Some(super::Kind::AdStack),
                "{tag} tanınmalı");
        }
    }

    /// Reklam yığını duraklamayı AÇIKLAR, ama ANR'yi gizlememeli.
    #[test]
    fn ad_stack_ranks_between_pause_and_real_faults() {
        assert!(super::Kind::AdStack.weight() > super::Kind::Paused.weight());
        assert!(super::Kind::Anr.weight() > super::Kind::AdStack.weight());
    }

    /// Duraklama gerçek arızaları GİZLEMEMELİ.
    ///
    /// Ölçüldü: menüdeki reklam etkinliği hem duraklama hem ANR üretti.
    /// Duraklamayı öne koymak kullanıcıya "sorun yok" dedirtirdi.
    #[test]
    fn real_faults_outrank_a_pause() {
        for worse in [super::Kind::Anr, super::Kind::Crash,
                      super::Kind::Input, super::Kind::Network] {
            assert!(worse.weight() > super::Kind::Paused.weight(),
                "{worse:?} duraklamadan önce gelmeli");
        }
    }

    /// Ama duraklama gürültünün üstünde kalmalı: tek kanıt oysa söylensin.
    #[test]
    fn pause_still_outranks_noise() {
        for lesser in [super::Kind::Composer, super::Kind::Gc,
                       super::Kind::Slow, super::Kind::MainThread] {
            assert!(super::Kind::Paused.weight() > lesser.weight());
        }
    }

    /// GERÇEK GÜNLÜK: menüye girince 8 saniyelik "donma" görünüyordu.
    /// Sebep oyunun kendi reklamıydı; sistemde hiçbir sorun yoktu.
    #[test]
    fn ad_activity_is_recognised_as_pause_not_fault() {
        assert_eq!(super::classify("ActivityTaskManager",
            "START u0 {cmp=com.ForgeGames.SpecialForcesGroup2/\
             com.google.android.gms.ads.AdActivity (has extras)} from uid 10148"),
            Some(super::Kind::Paused));
        assert_eq!(super::classify("InputDispatcher",
            "Not sending touch event to 1328819 com.ForgeGames.SpecialForcesGroup2/\
             com.epicgames.ue4.GameActivity because it is paused"),
            Some(super::Kind::Paused));
    }

    /// Sıradan etkinlik başlatma duraklama SAYILMAMALI.
    #[test]
    fn ordinary_activity_start_is_not_a_pause() {
        assert_ne!(super::classify("ActivityTaskManager",
            "START u0 {cmp=com.kiloo.subwaysurf/.MainActivity} from uid 10148"),
            Some(super::Kind::Paused));
    }

    use super::*;

    #[test]
    fn monotonic_line_parses() {
        let e = parse_line(
            "  1234.567890  4321  4350 I Choreographer: Skipped 42 frames!  \
             The application may be doing too much work on its main thread.",
            0.0, 0.0).unwrap();
        assert!((e.t_ms - 1_234_567.89).abs() < 0.1, "{}", e.t_ms);
        assert_eq!(e.pid, 4321);
        assert_eq!(e.kind, Kind::MainThread);
    }

    /// Duvar saati biçimi de anlaşılmalı: helper'ın eski sürümü yalnızca
    /// onu veriyor ve araç onunla da çalışmalı.
    #[test]
    fn wallclock_line_is_aligned_to_now() {
        // Şu an gün içi 100.0 sn, monotonik 500_000 ms.
        // Satır 00:01:30 = 90 sn, yani 10 sn önce.
        let e = parse_line(
            "08-28 00:01:30.000    88    88 W art: Long monitor contention with owner x",
            500_000.0, 100.0).unwrap();
        assert!((e.t_ms - 490_000.0).abs() < 1.0, "{}", e.t_ms);
        assert_eq!(e.kind, Kind::Lock);
    }

    /// Gece yarısını geçen günlük satırı GELECEKTEN gelmiş sayılmamalı.
    #[test]
    fn wallclock_wraps_over_midnight() {
        // Şu an 00:00:05 (5 sn), satır 23:59:55 (86395 sn) = 10 sn önce.
        let e = parse_line(
            "08-28 23:59:55.000    88    88 W art: Long monitor contention with owner x",
            500_000.0, 5.0).unwrap();
        assert!((e.t_ms - 490_000.0).abs() < 1.0, "{}", e.t_ms);
    }

    /// İlgisiz satırlar TAMAMEN elenmeli. Gürültüyü kanıt diye göstermek
    /// hiç kanıt göstermemekten kötü.
    #[test]
    fn noise_is_dropped() {
        for l in [
            "--------- beginning of main",
            "08-28 00:48:47.391    88    88 I hwcomposer: attach dmabuf: 2560x1440",
            "  10.5  1  1 I ActivityManager: Start proc 123",
            "",
        ] {
            assert!(parse_line(l, 0.0, 0.0).is_none(), "elenmeliydi: {l}");
        }
    }

    #[test]
    fn network_signatures_are_recognised() {
        for m in ["java.net.SocketTimeoutException: failed to connect",
                  "Unable to resolve host \"ads.example.com\"",
                  "java.net.UnknownHostException: x",
                  "Connection timed out"] {
            assert_eq!(classify("System.err", m), Some(Kind::Network), "{m}");
        }
    }

    /// Girdi yolu olayları TANINMALI: kullanıcının "fare sisteminden mi"
    /// sorusunun tek doğrudan kanıtı bu.
    #[test]
    fn input_path_loss_is_recognised() {
        assert_eq!(classify("EventHub",
            "Removing device '/dev/input/wl_touch_events' due to inotify event"),
            Some(Kind::Input));
        assert_eq!(classify("EventHub",
            "Removed device: path=/dev/input/wl_touch_events name=wayland_touch"),
            Some(Kind::Input));
        // Alakasız EventHub gürültüsü sayılmamalı.
        assert_eq!(classify("EventHub", "usingClockIoctl=false"), None);
    }

    #[test]
    fn crashes_are_recognised() {
        assert_eq!(classify("tombstoned", "received crash request for pid 138"),
                   Some(Kind::Crash));
        assert_eq!(classify("libc", "Fatal signal 11 (SIGSEGV)"), Some(Kind::Crash));
    }

    #[test]
    fn anr_outranks_everything() {
        assert!(Kind::Anr.weight() > Kind::Network.weight());
        assert!(Kind::Network.weight() > Kind::Gc.weight());
    }

    #[test]
    fn hitches_use_the_threshold() {
        let iv = [(0.0, 5.0), (100.0, 40.0), (200.0, 6.0), (300.0, 120.0)];
        let h = hitches(&iv, 30.0);
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].len_ms, 40.0);
        assert_eq!(h[1].t_ms, 300.0);
    }

    /// Kanıt penceresi takılmadan ÖNCESİNİ de kapsamalı: sebep genelde
    /// takılmadan hemen önce loglanır.
    #[test]
    fn correlation_looks_before_and_after() {
        let mut h = vec![Hitch { t_ms: 1000.0, len_ms: 100.0, evidence: vec![] }];
        let ev = vec![
            LogEvent { t_ms: 940.0, pid: 1, tag: "art".into(), kind: Kind::Gc,
                       msg: "GC freed".into() },
            LogEvent { t_ms: 1150.0, pid: 1, tag: "x".into(), kind: Kind::Network,
                       msg: "timeout".into() },
            LogEvent { t_ms: 5000.0, pid: 1, tag: "y".into(), kind: Kind::Lock,
                       msg: "uzak".into() },
        ];
        correlate(&mut h, &ev, 100.0, 100.0);
        assert_eq!(h[0].evidence.len(), 2, "pencere dışı olay girmemeli");
        // Ağırlığı yüksek olan başta.
        assert_eq!(h[0].evidence[0].kind, Kind::Network);
    }

    /// Tek takılmadaki 50 GC satırı GC'yi baş suçlu yapmamalı.
    #[test]
    fn tally_counts_hitches_not_lines() {
        let gc = |t: f64| LogEvent { t_ms: t, pid: 1, tag: "art".into(),
            kind: Kind::Gc, msg: "GC freed".into() };
        let net = |t: f64| LogEvent { t_ms: t, pid: 1, tag: "x".into(),
            kind: Kind::Network, msg: "timeout".into() };
        let hs = vec![
            Hitch { t_ms: 0.0, len_ms: 50.0,
                    evidence: (0..4).map(|i| gc(i as f64)).collect() },
            Hitch { t_ms: 100.0, len_ms: 50.0, evidence: vec![net(100.0)] },
            Hitch { t_ms: 200.0, len_ms: 50.0, evidence: vec![net(200.0)] },
        ];
        let v = verdicts(&hs, 1000, 1.0);
        assert_eq!(v[0].kind, Some(Kind::Network),
                   "ağ 2 takılmada, GC 1 takılmada — ağ önde olmalı: {v:?}");
    }

    /// Kanıt yoksa bunu SÖYLEMELİ; uydurma suçlama yapmamalı.
    #[test]
    fn unexplained_hitches_are_reported_as_such() {
        let hs = vec![Hitch { t_ms: 0.0, len_ms: 300.0, evidence: vec![] }];
        let v = verdicts(&hs, 100, 1.0);
        assert!(v.iter().any(|x| x.headline.contains("AÇIKLANAMADI")), "{v:?}");
        assert!(v.iter().all(|x| !x.headline.contains("çöp toplama")));
    }

    /// Etiket hızları ölçülmeli: teşhis penceresini boğan gürültüyü
    /// bulmanın tek yolu bu.
    #[test]
    fn tag_rates_find_the_flooder() {
        let mut raw = String::new();
        for i in 0..300 {
            raw.push_str(&format!(
                "08-28 00:56:33.{i:03}    88    88 I hwcomposer: attach dmabuf\n"));
        }
        raw.push_str("08-28 00:56:34.000  100  100 I art: GC freed 1\n");
        let r = tag_rates(&raw, 2.0);
        assert_eq!(r[0].0, "hwcomposer");
        assert!((r[0].1 - 150.0).abs() < 0.1, "{r:?}");
        assert_eq!(r[1].0, "art");
    }

    #[test]
    fn tag_of_handles_both_formats() {
        assert_eq!(tag_of("08-28 00:56:33.146 88 88 I hwcomposer: x").as_deref(),
                   Some("hwcomposer"));
        assert_eq!(tag_of("  123.456 88 88 I Choreographer: x").as_deref(),
                   Some("Choreographer"));
        assert_eq!(tag_of("--------- beginning of main"), None);
    }

    /// Host doymamışsa SUÇLAMA YAPMAMALI.
    #[test]
    fn host_verdict_stays_silent_without_evidence() {
        assert!(host_verdict(&HostFacts { unexplained: 4, ..Default::default() })
                .is_none());
        assert!(host_verdict(&HostFacts::default()).is_none());
    }

    /// Çoğunlukta doyma varsa söylemeli — "açıklanamadı" deyip bırakmak
    /// kullanıcıyı boşlukta bırakıyor.
    #[test]
    fn host_verdict_names_the_saturated_resource() {
        let v = host_verdict(&HostFacts {
            unexplained: 4, gpu_saturated: 3, ..Default::default() }).unwrap();
        assert!(v.headline.contains("GPU"), "{v:?}");
        let v = host_verdict(&HostFacts {
            unexplained: 4, vram_saturated: 4, ..Default::default() }).unwrap();
        assert!(v.headline.contains("VRAM"), "{v:?}");
    }

    #[test]
    fn no_hitches_says_so_plainly() {
        let v = verdicts(&[], 5000, 0.1);
        assert_eq!(v.len(), 1);
        assert!(v[0].headline.contains("yakalanmadı"));
    }
}
