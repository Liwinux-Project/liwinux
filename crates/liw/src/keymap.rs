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
    println!("{:<22} {:<10} {}", "YOL", "TÜR", "AD");
    for d in devs {
        println!("{:<22} {:<10} {}", d.path.display(), format!("{:?}", d.kind), d.name);
    }
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
    let target = match device {
        Some(p) => p,
        None => devs.iter()
            .find(|d| matches!(d.kind, capture::DeviceKind::Keyboard | capture::DeviceKind::Combo))
            .map(|d| d.path.clone())
            .context("klavye bulunamadı — 'liw keymap devices' ile bak")?,
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

                engine.tick(t0.elapsed().as_millis() as u64);
                let acts = engine.handle(input);
                for act in &acts { print_action(act, screen); }
                if let Some(b) = backend.as_mut() {
                    if let Err(e) = b.dispatch(&acts) {
                        eprintln!("  !! enjeksiyon hatası: {e}");
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
pub async fn poke(x: f32, y: f32, hold_ms: u64, drag_to: Option<(f32, f32)>) -> Result<()> {
    use liw_core::input::Norm;

    let mut b = UinputBackend::new(ScreenMap::default())
        .context("sanal dokunmatik ekran oluşturulamadı")?;
    println!("Sanal cihaz: {}", b.dev_nodes().join(", "));
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
