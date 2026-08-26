//! `liw keymap` — girdi motorunu gerçek klavyeyle dene.

use anyhow::{Context, Result};
use liw_core::input::{
    backend::TouchBackend,
    capture::{self, translate, GrabbedDevice},
    Engine, InputEvent, Profile, ScreenMap, TouchAction, TriggerKind, UinputBackend,
};
use std::path::PathBuf;

pub fn list_devices() -> Result<()> {
    let devs = capture::discover();
    if devs.is_empty() {
        println!("Girdi cihazı bulunamadı.");
        println!("Kullanıcı 'input' grubunda mı?  ->  groups | grep input");
        return Ok(());
    }
    let best = capture::best_keyboard(&devs).map(|d| d.path.clone());
    println!("{:<20} {:<9} {:<5} {:<7} {}", "YOL", "TÜR", "PUAN", "SANAL", "AD");
    for d in &devs {
        let mark = if Some(&d.path) == best.as_ref() { " <- varsayılan" } else { "" };
        println!("{:<20} {:<9} {:<5} {:<7} {}{}",
            d.path.display(), format!("{:?}", d.kind), d.typing_score,
            if d.virtual_device { "evet" } else { "hayır" }, d.name, mark);
    }
    println!();
    println!("PUAN = gerçek yazma klavyesi olma olasılığı (22 üzerinden).");
    println!("Doğru cihazdan emin değilsen:  liw keymap watch");
    Ok(())
}

/// Motoru gerçek girdiyle çalıştırır ve üretilen dokunuşları yazdırır.
///
/// Android'e hiçbir şey enjekte edilmez — bu, eşlemenin doğruluğunu
/// konteynere hiç dokunmadan doğrulamak içindir.
pub async fn test_profile(
    profile_path: PathBuf,
    device: Option<PathBuf>,
    grab: bool,
    screen: (u32, u32),
    inject: bool,
) -> Result<()> {
    let text = std::fs::read_to_string(&profile_path)
        .with_context(|| format!("profil okunamadı: {}", profile_path.display()))?;
    let profile = Profile::from_toml(&text).context("profil geçersiz")?;
    println!("Profil : {} ({})", profile.name, profile.package);
    println!("Bağlantı sayısı: {}", profile.bindings.len());

    let devs = capture::discover();
    // Sıra: açık -d > kaydedilmiş kalibrasyon > kaba tahmin.
    // Tahmin son çare ve güvenilmez olduğu için kullanıcı uyarılır.
    let cfg = liw_core::Config::load();
    let target = match device.or(cfg.keyboard) {
        Some(p) => p,
        None => {
            eprintln!("uyarı: kalibre edilmiş klavye yok, tahmin ediliyor.");
            eprintln!("       doğrusu için:  liw keymap detect --save");
            capture::best_keyboard(&devs)
                .map(|d| d.path.clone())
                .context("klavye bulunamadı — 'liw keymap devices' ile bak")?
        }
    };
    let name = devs.iter().find(|d| d.path == target)
        .map(|d| d.name.clone()).unwrap_or_default();
    println!("Cihaz  : {} ({})", target.display(), name);

    if grab {
        println!();
        println!("!! KİLİT AÇIK: tuşlar masaüstüne GİTMEYECEK.");
        println!("!! Bu yüzden Ctrl+C TERMİNALE ULAŞMAZ.");
        println!("!! ÇIKIŞ TUŞU: ESC  (üç kez arka arkaya)");
        println!("!! Program ölürse çekirdek kilidi zaten bırakır.");
    } else {
        println!("Kilit  : kapalı (tuşlar masaüstüne de gidiyor; --grab ile kilitle)");
    }
    println!("Ekran  : {}x{}", screen.0, screen.1);
    println!("---- tuşlara bas (Ctrl+C ile çık) ----");

    // Enjeksiyon arka ucu. Sanal cihaz OLUŞTURULDUKTAN sonra keşif yapılırsa
    // kendi cihazımızı yakalama riski doğar; bu yüzden hedef cihaz yukarıda
    // zaten seçildi.
    let mut backend: Option<UinputBackend> = if inject {
        let mut b = UinputBackend::new(ScreenMap::default())
            .context("sanal dokunmatik ekran oluşturulamadı")?;
        // Cihazın libinput/KWin tarafından tanınması birkaç yüz ms sürebilir.
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        println!("Enjeksiyon: uinput  ->  {}", b.dev_nodes().join(", "));
        println!("  UYARI: dokunuşlar EKRAN uzayında. Waydroid tam ekran değilse");
        println!("         yanlış yere gider (ScreenMap ile telafi edilecek).");
        Some(b)
    } else {
        println!("Enjeksiyon: KAPALI (sadece yazdırılıyor; --inject ile aç)");
        None
    };

    let dev = GrabbedDevice::open(&target, grab)?;
    let mut stream = dev.into_stream()?;
    let mut engine = Engine::new(profile);

    // Jest saati. Kaydırmalar ara adımlarla ilerler; bu tick olmadan
    // jest başlar ama asla tamamlanmaz. 4ms ~ 250Hz: 144Hz ekranda bile
    // her kareye en az bir adım düşer.
    let t0 = std::time::Instant::now();
    let mut lat = liw_core::input::LatencyStats::new();
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(4));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Kaçış kapısı. Kilitliyken Ctrl+C terminale ulaşmadığı için ESC'yi
    // KENDİ döngümüzde yakalamak zorundayız; yoksa kullanıcının klavyesi
    // programda takılı kalır. Üç kez şartı, oyunda ESC'ye basmakla
    // kazara çıkmayı önler.
    const ESC: u16 = 1;
    let mut esc_streak = 0u8;

    loop {
        tokio::select! {
            ev = stream.next_event() => {
                let ev = ev.context("olay akışı koptu")?;
                let ev_time = ev.timestamp();
                let Some(input) = translate(&ev) else { continue };

                if grab {
                    match input {
                        InputEvent::Press(TriggerKind::Key(ESC)) => {
                            esc_streak += 1;
                            if esc_streak >= 3 {
                                println!();
                                println!("ESC x3 — çıkılıyor, kilit bırakılıyor");
                                let acts = engine.set_enabled(false);
                                for act in &acts { print!("[çıkış] "); print_action(act, screen); }
                                if let Some(b) = backend.as_mut() { let _ = b.dispatch(&acts); }
                                break;
                            }
                            println!("  (ESC {esc_streak}/3 — çıkmak için tekrar bas)");
                        }
                        InputEvent::Press(_) => esc_streak = 0,
                        _ => {}
                    }
                }

                // tick'in ürettiği eylemler ATILMAMALI: önceki jestin son
                // MOVE'u ve UP'ı orada olabilir. Atılırsa parmak ekranda
                // asılı kalır ve sonraki jestler bozulur.
                let mut acts = engine.tick(t0.elapsed().as_millis() as u64);
                acts.extend(engine.handle(input));
                for act in &acts { print_action(act, screen); }
                if let Some(b) = backend.as_mut() {
                    if let Err(e) = b.dispatch(&acts) {
                        eprintln!("  !! enjeksiyon hatası: {e}");
                    } else if !acts.is_empty() {
                        lat.record(ev_time);
                    }
                }
            }
            _ = ticker.tick(), if engine.has_pending() => {
                let acts = engine.tick(t0.elapsed().as_millis() as u64);
                for act in &acts { print_action(act, screen); }
                if let Some(b) = backend.as_mut() {
                    if let Err(e) = b.dispatch(&acts) {
                        eprintln!("  !! enjeksiyon hatası: {e}");
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                println!();
                let acts = engine.set_enabled(false);
                for act in &acts { print!("[çıkış] "); print_action(act, screen); }
                if let Some(b) = backend.as_mut() { let _ = b.dispatch(&acts); }
                println!();
                println!("{}", lat.report("tuş → dokunuş yazımı (yalnızca liwinux katmanı)"));
                println!("bitti — kilit bırakıldı");
                break;
            }
        }
    }
    Ok(())
}

fn print_action(act: &TouchAction, (w, h): (u32, u32)) {
    match act {
        TouchAction::Down { id, at } => {
            let (x, y) = at.to_px(w, h);
            println!("  DOWN  p{id}  ({x:>5},{y:>5})   norm({:.3},{:.3})", at.x, at.y);
        }
        TouchAction::Move { id, at } => {
            let (x, y) = at.to_px(w, h);
            println!("  MOVE  p{id}  ({x:>5},{y:>5})   norm({:.3},{:.3})", at.x, at.y);
        }
        TouchAction::Up { id } => println!("  UP    p{id}"),
    }
}

/// Tek bir dokunuş gönderir — eşlemeden bağımsız enjeksiyon testi.
///
/// Bu ayrımın nedeni teşhis: oyun tepki vermezse sorunun eşlemede mi
/// yoksa enjeksiyon yolunda mı olduğunu bilmek gerekir.
pub async fn poke(
    x: f32, y: f32, hold_ms: u64, drag_to: Option<(f32, f32)>,
    map: ScreenMap,
) -> Result<()> {
    use liw_core::input::Norm;

    let mut b = UinputBackend::new(map)
        .context("sanal dokunmatik ekran oluşturulamadı")?;
    println!("Sanal cihaz: {}", b.dev_nodes().join(", "));
    println!("Harita: origin({:.3},{:.3}) scale({:.3},{:.3}) invert({},{})",
        map.origin_x, map.origin_y, map.scale_x, map.scale_y, map.invert_x, map.invert_y);
    println!("KWin/libinput'un cihazı tanıması bekleniyor...");
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;

    let from = Norm::new(x, y);
    println!("DOWN  ({x:.3}, {y:.3})");
    b.dispatch(&[TouchAction::Down { id: 0, at: from }])
        .context("DOWN gönderilemedi")?;

    if let Some((tx, ty)) = drag_to {
        // Sürüklemeyi adımlara böl: tek sıçrama jest olarak tanınmaz,
        // oyunlar ara hareketleri bekler.
        const STEPS: u32 = 12;
        let step_ms = (hold_ms / STEPS as u64).max(4);
        for i in 1..=STEPS {
            let t = i as f32 / STEPS as f32;
            let at = Norm::new(x + (tx - x) * t, y + (ty - y) * t);
            b.dispatch(&[TouchAction::Move { id: 0, at }])
                .context("MOVE gönderilemedi")?;
            tokio::time::sleep(std::time::Duration::from_millis(step_ms)).await;
        }
        println!("MOVE  ({tx:.3}, {ty:.3})  [{STEPS} adımda]");
    } else {
        tokio::time::sleep(std::time::Duration::from_millis(hold_ms)).await;
    }

    b.dispatch(&[TouchAction::Up { id: 0 }]).context("UP gönderilemedi")?;
    println!("UP");
    // Cihaz hemen yok olursa son olaylar işlenmeden kaybolabilir.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    println!("bitti");
    Ok(())
}

/// Tüm klavyeleri aynı anda dinler ve hangi cihazın hangi kodu ürettiğini yazar.
///
/// Teşhis için: "tuşa bastım ama bir şey olmadı" durumunda ilk sorulacak şey
/// doğru cihazı dinleyip dinlemediğimizdir. Çoklu arayüzlü klavyelerde
/// (Razer, Logitech) harf tuşları beklenmedik bir event düğümünde olabilir.
pub async fn watch(device: Option<PathBuf>) -> Result<()> {
    let devs = capture::discover();
    let targets: Vec<_> = match device {
        Some(p) => vec![p],
        None => devs.iter()
            .filter(|d| !matches!(d.kind, capture::DeviceKind::Pointer))
            .filter(|d| !d.virtual_device)
            .map(|d| d.path.clone())
            .collect(),
    };
    if targets.is_empty() {
        println!("Dinlenecek cihaz yok.");
        return Ok(());
    }

    println!("Dinlenen cihazlar:");
    for t in &targets {
        let n = devs.iter().find(|d| &d.path == t)
            .map(|d| d.name.as_str()).unwrap_or("?");
        println!("  {}  {}", t.display(), n);
    }
    println!();
    println!("Tuşlara bas — hangi cihazın hangi kodu ürettiğini göreceksin.");
    println!("Profilde kullanılacak değer 'kod' sütunudur. Ctrl+C ile çık.");
    println!();
    println!("{:<20} {:<8} {:<10} {}", "CİHAZ", "KOD", "DURUM", "AD");

    let mut set = tokio::task::JoinSet::new();
    for path in targets {
        let name = devs.iter().find(|d| d.path == path)
            .map(|d| d.name.clone()).unwrap_or_default();
        // Kilit YOK: teşhis sırasında kullanıcının klavyesini almak kabul edilemez.
        let dev = match GrabbedDevice::open(&path, false) {
            Ok(d) => d,
            Err(e) => { eprintln!("  atlandı {}: {e}", path.display()); continue; }
        };
        let mut stream = match dev.into_stream() {
            Ok(s) => s,
            Err(e) => { eprintln!("  akış kurulamadı {}: {e}", path.display()); continue; }
        };
        let short = path.file_name().map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        set.spawn(async move {
            while let Ok(ev) = stream.next_event().await {
                if let Some(input) = translate(&ev) {
                    let (kod, durum) = match input {
                        InputEvent::Press(TriggerKind::Key(k)) => (k.to_string(), "BASILDI"),
                        InputEvent::Release(TriggerKind::Key(k)) => (k.to_string(), "bırakıldı"),
                        InputEvent::Press(t) => (format!("{t:?}"), "BASILDI"),
                        InputEvent::Release(t) => (format!("{t:?}"), "bırakıldı"),
                        InputEvent::MouseMove { .. } => continue,
                    };
                    println!("{:<20} {:<8} {:<10} {}", short, kod, durum, name);
                }
            }
        });
    }

    tokio::signal::ctrl_c().await.ok();
    set.abort_all();
    println!();
    println!("bitti");
    Ok(())
}

/// Klavyeyi kalibrasyonla belirler: bir tuşa bas, hangi cihaz ürettiyse o.
///
/// Otomatik tespit yerine ölçüm. Çoklu arayüzlü klavyelerde yetenek listesi
/// ayırt edici DEĞİL — bu makinede aynı klavyenin iki arayüzü de tam tuş
/// aralığını bildiriyor ama olayları yalnızca biri üretiyor.
pub async fn detect(save: bool) -> Result<()> {
    let devs = capture::discover();
    let cands: Vec<_> = devs.iter()
        .filter(|d| !d.virtual_device)
        .filter(|d| !matches!(d.kind, capture::DeviceKind::Pointer))
        .collect();
    if cands.is_empty() {
        println!("Aday klavye yok. 'input' grubunda mısın?  ->  groups");
        return Ok(());
    }
    println!("{} aday dinleniyor. ŞİMDİ BİR TUŞA BAS...", cands.len());
    println!();

    let (tx, mut rx) = tokio::sync::mpsc::channel::<(PathBuf, String, u16)>(8);
    let mut set = tokio::task::JoinSet::new();
    for d in &cands {
        let (path, name, tx) = (d.path.clone(), d.name.clone(), tx.clone());
        let Ok(dev) = GrabbedDevice::open(&path, false) else { continue };
        let Ok(mut stream) = dev.into_stream() else { continue };
        set.spawn(async move {
            while let Ok(ev) = stream.next_event().await {
                if let Some(InputEvent::Press(TriggerKind::Key(k))) = translate(&ev) {
                    let _ = tx.send((path.clone(), name.clone(), k)).await;
                    return;
                }
            }
        });
    }
    drop(tx);

    let found = tokio::select! {
        v = rx.recv() => v,
        _ = tokio::time::sleep(std::time::Duration::from_secs(20)) => {
            println!("Zaman aşımı — tuş algılanmadı."); None
        }
        _ = tokio::signal::ctrl_c() => { println!("iptal edildi"); None }
    };
    set.abort_all();

    let Some((path, name, code)) = found else { return Ok(()) };
    println!("Klavye : {}  ({})", path.display(), name);
    println!("İlk tuş kodu: {code}");

    if save {
        let mut cfg = liw_core::Config::load();
        cfg.keyboard = Some(path);
        let p = cfg.save().context("yapılandırma kaydedilemedi")?;
        println!("Kaydedildi: {}", p.display());
        println!("Artık 'liw keymap test' bu cihazı varsayılan kullanacak.");
    } else {
        println!("(kaydetmek için: liw keymap detect --save)");
    }
    Ok(())
}

/// Android dokunuş göstergesini açar/kapatır (kalibrasyon için).
pub async fn overlay(on: bool) -> Result<()> {
    let h = liw_core::HelperClient::connect().await
        .context("liwd-helper'a bağlanılamadı — çalışıyor mu? \
                  (sudo systemctl status liwd-helper)")?;
    h.set_pointer_location(on).await.context("gösterge ayarlanamadı")?;
    println!("dokunuş göstergesi: {}", if on { "AÇIK" } else { "kapalı" });
    if on {
        println!("Artık her dokunuş ekranda işaretlenecek — nereye düştüğünü göreceksin.");
    }
    Ok(())
}

/// X ekseni boyunca tarama yapar: hangi koordinatların pencereye ulaştığını bulur.
///
/// Koordinat eşlemesini tahminle ayarlamak yerine ölçmek için. Kullanıcı
/// hangi noktaların ekranda göründüğünü ve NEREDE göründüğünü bildirir.
pub async fn sweep(axis: char, count: u32, gap_ms: u64) -> Result<()> {
    use liw_core::input::Norm;

    let mut b = UinputBackend::new(ScreenMap::default())
        .context("sanal dokunmatik ekran oluşturulamadı")?;
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    println!("Tarama: {} ekseni, {count} nokta, {gap_ms}ms aralık", axis);
    println!("Android ekranını izle — hangi numaraların göründüğünü not al.");
    println!();

    for i in 0..count {
        let t = i as f32 / (count - 1).max(1) as f32;
        let at = if axis == 'y' { Norm::new(0.5, t) } else { Norm::new(t, 0.5) };
        println!("  #{:<2}  {} = {:.2}   ({:.3}, {:.3})", i, axis, t, at.x, at.y);
        b.dispatch(&[TouchAction::Down { id: 0, at }])?;
        tokio::time::sleep(std::time::Duration::from_millis(160)).await;
        b.dispatch(&[TouchAction::Up { id: 0 }])?;
        tokio::time::sleep(std::time::Duration::from_millis(gap_ms)).await;
    }
    println!();
    println!("Görünen ilk ve son numarayı söyle — eşlemeyi ondan hesaplayacağım.");
    Ok(())
}

/// Ön plandaki oyunu izler, profilini otomatik yükler ve tuşları eşler.
///
/// Motor `liw_core::input::Runner`'da; `liwd` de aynısını kullanır. Buradaki
/// tek fark ön planı kimin sağladığı ve çıktının nereye yazıldığı.
pub async fn run(grab: bool, poll_ms: u64) -> Result<()> {
    use liw_core::input::{Runner, RunnerConfig, RunnerEvent, ScreenMap, Store};

    let helper = liw_core::HelperClient::connect().await
        .context("liwd-helper'a bağlanılamadı — çalışıyor mu? \
                  (systemctl status liwd-helper)")?;

    let devs = capture::discover();
    let cfg = liw_core::Config::load();
    let device = cfg.keyboard.clone()
        .or_else(|| capture::best_keyboard(&devs).map(|d| d.path.clone()))
        .context("klavye yok — 'liw keymap detect --save' ile kalibre et")?;
    let dev_name = devs.iter().find(|d| d.path == device)
        .map(|d| d.name.clone()).unwrap_or_default();

    let store = Store::discover();
    println!("Klavye  : {} ({})", device.display(), dev_name);
    println!("Profil  : {} adet yüklü", store.len());
    for p in &store.problems {
        eprintln!("  uyarı: {} — {}", p.path.display(), p.error);
    }
    println!("Kilit   : {}", if grab { "profil etkinken açılacak" } else { "kapalı" });
    println!("Yoklama : {poll_ms}ms");
    println!("Ctrl+C ile çık. Kilitliyken çıkış: ESC ×3");
    println!();

    let mut runner = Runner::new(
        RunnerConfig { device, grab, screen_map: ScreenMap::default() }, store);

    let (fg_tx, fg_rx) = tokio::sync::mpsc::channel::<String>(4);
    let (ev_tx, mut ev_rx) = tokio::sync::mpsc::channel::<RunnerEvent>(16);
    let (sd_tx, sd_rx) = tokio::sync::watch::channel(false);

    // Ön plan yoklaması ayrı görevde: girdi yolu bloke edilmemeli.
    let poll = tokio::spawn(async move {
        let mut t = tokio::time::interval(std::time::Duration::from_millis(poll_ms));
        loop {
            t.tick().await;
            match helper.foreground_package().await {
                Ok(p) if !p.is_empty() => { let _ = fg_tx.try_send(p); }
                Ok(_) => {}
                Err(e) => eprintln!("ön plan sorgulanamadı: {e}"),
            }
        }
    });

    let printer = tokio::spawn(async move {
        while let Some(e) = ev_rx.recv().await {
            match e {
                RunnerEvent::ProfileActivated { package, profile } =>
                    println!("[{package}] profil ETKİN: {profile}"),
                RunnerEvent::ProfileCleared { package } =>
                    println!("[{package}] profil yok — eşleme kapalı"),
                RunnerEvent::Grabbed => println!("  kilit alındı"),
                RunnerEvent::Ungrabbed => println!("  kilit bırakıldı"),
                RunnerEvent::EscapeRequested => println!("ESC ×3 — çıkılıyor"),
            }
        }
    });

    let sd = sd_tx.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        println!();
        println!("çıkılıyor");
        let _ = sd.send(true);
    });

    let lat = runner.run(fg_rx, sd_rx, Some(ev_tx)).await
        .context("keymapper hatayla durdu")?;
    poll.abort();
    printer.abort();

    println!();
    println!("=== GECİKME (yalnızca liwinux katmanı) ===");
    println!("{}", lat.report("tuş → dokunuş yazımı"));
    println!();
    println!("KAPSAM: çekirdek evdev zaman damgasından uinput yazımına kadar.");
    println!("DAHİL DEĞİL: libinput, KWin, wl_touch, Waydroid, Android girdi");
    println!("hattı ve oyunun kendi tepki süresi.");
    println!("bitti");
    Ok(())
}

// --- liwd üzerinden keymapper kontrolü ---

const BUS: &str = "id.liwinux.Manager1";
const OBJ: &str = "/id/liwinux/Manager1";

async fn daemon() -> Result<zbus::Proxy<'static>> {
    let conn = zbus::Connection::session().await
        .context("oturum veri yoluna bağlanılamadı")?;
    let p = zbus::Proxy::new(&conn, BUS, OBJ, BUS).await
        .context("liwd proxy'si kurulamadı")?;
    p.introspect().await
        .context("liwd çalışmıyor — systemctl --user status liwd")?;
    Ok(p)
}

/// Keymapper'ı liwd içinde başlatır: terminal kapansa da çalışmaya devam eder.
pub async fn daemon_start(grab: bool) -> Result<()> {
    let p = daemon().await?;
    p.call::<_, _, ()>("StartKeymapper", &(grab,)).await
        .context("StartKeymapper başarısız")?;
    println!("keymapper başlatıldı (liwd içinde){}",
        if grab { ", profil etkinken kilitlenecek" } else { "" });
    println!("Durum için: liw keymap status");
    Ok(())
}

pub async fn daemon_stop() -> Result<()> {
    let p = daemon().await?;
    p.call::<_, _, ()>("StopKeymapper", &()).await
        .context("StopKeymapper başarısız")?;
    println!("keymapper durduruldu");
    Ok(())
}

pub async fn daemon_status() -> Result<()> {
    let p = daemon().await?;
    let json: String = p.call("KeymapperStatus", &()).await
        .context("KeymapperStatus başarısız")?;
    let st: liw_core::input::RunnerState = serde_json::from_str(&json)
        .context("durum çözümlenemedi")?;

    println!("Çalışıyor    : {}", if st.running { "evet" } else { "hayır" });
    println!("Ön plan      : {}", st.foreground.as_deref().unwrap_or("-"));
    println!("Etkin profil : {}", st.active_profile.as_deref().unwrap_or("yok"));
    println!("Kilit        : {}", if st.grabbed { "açık" } else { "kapalı" });
    if st.latency_p50_us > 0 || st.latency_p99_us > 0 {
        println!("Gecikme      : p50 {:.2} ms   p99 {:.2} ms  (yalnızca liwinux katmanı)",
            st.latency_p50_us as f64 / 1000.0, st.latency_p99_us as f64 / 1000.0);
    }
    if !st.running {
        println!();
        println!("Başlatmak için: liw keymap start");
    }
    Ok(())
}
