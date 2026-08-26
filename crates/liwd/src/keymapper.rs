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

struct Running {
    shutdown: watch::Sender<bool>,
    state: Arc<tokio::sync::RwLock<RunnerState>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

pub struct Handle {
    inner: Mutex<Option<Running>>,
}

impl Handle {
    pub fn new() -> Self { Self { inner: Mutex::new(None) } }

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
            cihaz = %device.display(), profil = store.len(), grab,
            "keymapper başlatılıyor");

        let mut runner = Runner::new(
            RunnerConfig { device, grab, screen_map: ScreenMap::default() },
            store,
        );
        let state = runner.state();

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
                    RunnerEvent::EscapeRequested =>
                        tracing::info!("ESC ×3 — keymapper durduruluyor"),
                }
            }
        });

        let main = tokio::spawn(async move {
            match runner.run(fg_rx, sd_rx, Some(ev_tx)).await {
                Ok(lat) => tracing::info!("keymapper durdu — {}", lat.report("gecikme")),
                Err(e) => tracing::error!(hata = %e, "keymapper hatayla durdu"),
            }
        });

        *guard = Some(Running { shutdown: sd_tx, state, tasks: vec![poll, log, main] });
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
    }
}
