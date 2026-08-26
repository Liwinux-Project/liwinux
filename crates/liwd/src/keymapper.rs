//! liwd içindeki keymapper görevi.
//!
//! Runner'ı sarmalar ve ön plan yoklamasını sağlar. Yoklama AYRI GÖREVDE
//! çalışır: `waydroid shell dumpsys` 100-200 ms sürüyor ve girdi döngüsünde
//! beklenirse gecikme p99'da 212 ms'ye çıkıyor (ölçüldü).

use anyhow::{Context, Result};
use liw_core::input::{Runner, RunnerConfig, RunnerEvent, RunnerState, ScreenMap, Store};
use liw_core::HelperClient;
use std::sync::Arc;
use tokio::sync::{mpsc, watch, Mutex};

const POLL_MS: u64 = 1000;

/// Waydroid penceresinin KWin'deki sınıf adı. `find-waydroid.js` ile
/// ölçüldü: cls='waydroid'.
const WAYDROID_CLASS: &str = "waydroid";
/// KWin script'inin kayıt adı; durdururken aynı adla kaldırılır.
const KWIN_SCRIPT: &str = "liwinux-focus";

struct Running {
    shutdown: watch::Sender<bool>,
    focus: watch::Sender<bool>,
    state: Arc<tokio::sync::RwLock<RunnerState>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

pub struct Handle {
    inner: Mutex<Option<Running>>,
}

impl Handle {
    pub fn new() -> Self { Self { inner: Mutex::new(None) } }

    /// KWin'den gelen odak bildirimi.
    pub async fn set_active_window(&self, class: &str) {
        let guard = self.inner.lock().await;
        let Some(r) = guard.as_ref() else { return };
        let focused = class.eq_ignore_ascii_case(WAYDROID_CLASS);
        // Yalnızca değişimde gönder: watch kanalı aynı değeri tekrar
        // yayarsa Runner gereksiz iş yapar.
        if *r.focus.borrow() != focused {
            tracing::info!(pencere = class, waydroid_odakta = focused, "odak değişti");
            let _ = r.focus.send(focused);
        }
    }

    pub async fn state(&self) -> RunnerState {
        match &*self.inner.lock().await {
            Some(r) => r.state.read().await.clone(),
            None => RunnerState::default(),
        }
    }

    pub async fn start(&self, grab: bool) -> Result<()> {
        let mut guard = self.inner.lock().await;
        if guard.is_some() {
            tracing::info!("keymapper zaten çalışıyor");
            return Ok(());
        }

        let cfg = liw_core::Config::load();
        let devs = liw_core::input::discover();
        let device = cfg.keyboard.clone()
            .or_else(|| liw_core::input::capture::best_keyboard(&devs).map(|d| d.path.clone()))
            .context("klavye yok — 'liw keymap detect --save' ile kalibre et")?;

        let store = Store::discover();
        for p in &store.problems {
            tracing::warn!(dosya = %p.path.display(), hata = %p.error, "profil yüklenemedi");
        }
        tracing::info!(
            klavye = %device.display(),
            fare = ?cfg.mouse.as_ref().map(|p| p.display().to_string()),
            profil = store.len(), grab,
            kısayol = ?cfg.hotkey_game_mode,
            "keymapper başlatılıyor");
        if grab && cfg.hotkey_game_mode.is_none() {
            tracing::warn!(
                "kilit açık ama oyun kipi kısayolu tanımlı değil — profil \
                 etkinleşir etkinleşmez kilitlenecek. 'liw keymap detect \
                 --hotkey --save' ile bir tuş belirle.");
        }

        let mut runner = Runner::new(
            RunnerConfig {
                device, mouse: cfg.mouse.clone(), grab,
                hotkey: cfg.hotkey_game_mode,
                screen_map: ScreenMap::default(),
            },
            store,
        );
        let state = runner.state();

        // Odak başlangıçta BİLİNMİYOR; KWin script'i ilk bildirimi yapana
        // kadar kapalı varsayıyoruz. Yanlış pozitif (kapalıyken açık sanmak)
        // masaüstüne dokunuş enjekte etmek demek — tersi yalnızca gecikme.
        let (focus_tx, focus_rx) = watch::channel(false);
        let (fg_tx, fg_rx) = mpsc::channel::<String>(4);
        let (ev_tx, mut ev_rx) = mpsc::channel::<RunnerEvent>(16);
        let (sd_tx, sd_rx) = watch::channel(false);

        // Ön plan yoklaması — girdi yolunu ASLA bloke etmemeli.
        let helper = HelperClient::connect().await
            .context("liwd-helper'a bağlanılamadı — ön plan tespiti için gerekli")?;
        let poll = tokio::spawn(async move {
            let mut t = tokio::time::interval(std::time::Duration::from_millis(POLL_MS));
            loop {
                t.tick().await;
                match helper.foreground_package().await {
                    Ok(p) if !p.is_empty() => { let _ = fg_tx.try_send(p); }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(hata = %e, "ön plan sorgulanamadı"),
                }
            }
        });

        let log = tokio::spawn(async move {
            while let Some(e) = ev_rx.recv().await {
                match e {
                    RunnerEvent::ProfileActivated { package, profile } =>
                        tracing::info!(paket = %package, %profile, "profil etkin"),
                    RunnerEvent::ProfileCleared { package } =>
                        tracing::info!(paket = %package, "profil yok — eşleme kapalı"),
                    RunnerEvent::Grabbed => tracing::info!("cihaz kilitlendi"),
                    RunnerEvent::Ungrabbed => tracing::info!("cihaz kilidi bırakıldı"),
                    RunnerEvent::GameModeOn =>
                        tracing::info!("oyun kipi AÇIK — kilit + eşleme"),
                    RunnerEvent::GameModeOff =>
                        tracing::info!("oyun kipi kapalı — fare serbest"),
                    RunnerEvent::FocusGained =>
                        tracing::info!("Waydroid odakta — eşleme açık"),
                    RunnerEvent::FocusLost =>
                        tracing::info!("Waydroid odakta değil — eşleme kapalı"),
                    RunnerEvent::EscapeRequested =>
                        tracing::info!("ESC ×3 — keymapper durduruluyor"),
                }
            }
        });

        let main = tokio::spawn(async move {
            match runner.run(fg_rx, focus_rx, sd_rx, Some(ev_tx)).await {
                Ok(lat) => tracing::info!("keymapper durdu — {}", lat.report("gecikme")),
                Err(e) => tracing::error!(hata = %e, "keymapper hatayla durdu"),
            }
        });

        if let Err(e) = load_kwin_script().await {
            // Ölümcül değil ama TEHLİKELİ: odak bilgisi olmadan eşleme
            // hiç açılmaz (focus=false başlıyor). Kullanıcıya söyle.
            tracing::error!(hata = %e,
                "KWin odak script'i yüklenemedi — eşleme açılmayacak");
        }

        *guard = Some(Running {
            shutdown: sd_tx, focus: focus_tx, state,
            tasks: vec![poll, log, main],
        });
        Ok(())
    }

    pub async fn stop(&self) {
        let mut guard = self.inner.lock().await;
        let Some(r) = guard.take() else { return };
        tracing::info!("keymapper durduruluyor");
        let _ = r.shutdown.send(true);
        // Runner'ın temiz çıkıp parmakları kaldırmasına ve kilidi
        // bırakmasına fırsat ver; ancak ondan sonra görevleri iptal et.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        for t in r.tasks { t.abort(); }
        if let Err(e) = unload_kwin_script().await {
            tracing::warn!(hata = %e, "KWin script'i kaldırılamadı");
        }
    }
}

/// KWin'in scripting arayüzü üzerinden odak bildirim script'ini yükler.
async fn load_kwin_script() -> Result<()> {
    let path = kwin_script_path().context("focus.js bulunamadı")?;
    let conn = zbus::Connection::session().await?;
    let p = zbus::Proxy::new(&conn, "org.kde.KWin", "/Scripting",
                             "org.kde.kwin.Scripting").await?;
    // Eskisi kalmışsa kaldır; iki kopya iki kat bildirim demek.
    let _: Result<bool, _> = p.call("unloadScript", &(KWIN_SCRIPT,)).await;
    let _id: i32 = p.call("loadScript", &(path.to_string_lossy().as_ref(), KWIN_SCRIPT))
        .await.context("loadScript başarısız")?;
    let _: () = p.call("start", &()).await.context("script başlatılamadı")?;
    tracing::info!(script = %path.display(), "KWin odak script'i yüklendi");
    Ok(())
}

async fn unload_kwin_script() -> Result<()> {
    let conn = zbus::Connection::session().await?;
    let p = zbus::Proxy::new(&conn, "org.kde.KWin", "/Scripting",
                             "org.kde.kwin.Scripting").await?;
    let _: bool = p.call("unloadScript", &(KWIN_SCRIPT,)).await?;
    Ok(())
}

/// focus.js'i arar: kurulu konum, sonra çalıştırılabilirin yanındaki depo.
/// Çalışma dizinine BAKILMAZ (bkz. profil deposundaki aynı ders).
fn kwin_script_path() -> Option<std::path::PathBuf> {
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    // Kullanıcı kurulumu önce: sistem paketini gölgeleyebilsin.
    if let Some(data) = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME")
            .map(|h| std::path::PathBuf::from(h).join(".local").join("share")))
    {
        candidates.push(data.join("liwinux").join("kwin").join("focus.js"));
    }
    candidates.push("/usr/share/liwinux/kwin/focus.js".into());
    candidates.push("/usr/local/share/liwinux/kwin/focus.js".into());
    for c in candidates {
        if c.is_file() { return Some(c); }
    }
    let exe = std::env::current_exe().ok()?;
    let mut dir = exe.parent()?;
    for _ in 0..4 {
        let cand = dir.join("scripts").join("kwin").join("focus.js");
        if cand.is_file() { return Some(cand); }
        dir = dir.parent()?;
    }
    None
}
