//! liwd — liwinux session daemon.
//!
//! Kullanıcı bağlamında (systemd user servisi) çalışır; session Wayland
//! görüntüsüne eriştiği için root'ta olamaz. Ayrıcalıklı işlemler ileride
//! ayrı bir sistem yardımcısına + polkit'e taşınacak.

mod keymapper;

use anyhow::Result;
use liw_core::{Health, SessionState, Supervisor, SupervisorConfig};
use std::sync::Arc;
use tokio::sync::RwLock;
use zbus::{connection, interface};

const BUS_NAME: &str = "id.liwinux.Manager1";
const OBJ_PATH: &str = "/id/liwinux/Manager1";

struct Manager {
    sup: Arc<Supervisor>,
    state: Arc<RwLock<SessionState>>,
    km: Arc<keymapper::Handle>,
}

#[interface(name = "id.liwinux.Manager1")]
impl Manager {
    /// Session'ı ayrık başlatır. Zaten çalışıyorsa işlemsizdir.
    async fn start(&self) -> zbus::fdo::Result<()> {
        self.sup.start_detached().await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    async fn stop(&self) -> zbus::fdo::Result<()> {
        self.sup.stop().await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    async fn restart(&self) -> zbus::fdo::Result<()> {
        self.sup.recover().await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    /// Süpervizörün en son gözlemlediği durum.
    #[zbus(property)]
    async fn state(&self) -> String {
        self.state.read().await.as_str().to_string()
    }

    /// Ayrıntılı sağlık: JSON. Hangi göstergenin düştüğünü tek tek verir,
    /// çünkü "çalışmıyor" tek başına teşhis için yetersiz.
    async fn health(&self) -> zbus::fdo::Result<String> {
        let h: Health = self.sup.health().await;
        serde_json::to_string(&h).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    /// Keymapper'ı başlatır. Zaten çalışıyorsa işlemsizdir.
    ///
    /// `grab` true ise cihaz YALNIZCA profil etkinken kilitlenir; oyundan
    /// çıkınca bırakılır. Masaüstünde klavyenin çalışmaması kabul edilemez.
    async fn start_keymapper(&self, grab: bool) -> zbus::fdo::Result<()> {
        self.km.start(grab).await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    async fn stop_keymapper(&self) -> zbus::fdo::Result<()> {
        self.km.stop().await;
        Ok(())
    }

    /// Keymapper durumu (JSON): çalışıyor mu, ön plan, etkin profil, gecikme.
    async fn keymapper_status(&self) -> zbus::fdo::Result<String> {
        let st = self.km.state().await;
        serde_json::to_string(&st).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    #[zbus(property)]
    async fn keymapper_running(&self) -> bool {
        self.km.state().await.running
    }

    async fn status(&self) -> zbus::fdo::Result<String> {
        let s = self.sup.status().await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        serde_json::to_string(&s).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("LIWD_LOG").unwrap_or_else(|_| "info".into()))
        .init();

    let cfg = SupervisorConfig::default();
    let sup = Arc::new(Supervisor::new(cfg.clone()).with_helper().await);
    let state = Arc::new(RwLock::new(SessionState::Stopped));
    let km = Arc::new(keymapper::Handle::new());

    let _conn = connection::Builder::session()?
        .name(BUS_NAME)?
        .serve_at(OBJ_PATH, Manager {
            sup: sup.clone(), state: state.clone(), km: km.clone(),
        })?
        .build()
        .await?;
    tracing::info!("liwd hazır — {BUS_NAME} {OBJ_PATH}");

    // --- gözetim döngüsü ---
    let mut unhealthy = 0u32;
    let mut attempts = 0u32;
    let mut ticker = tokio::time::interval(cfg.poll_interval);
    let mut sigterm = tokio::signal::unix::signal(
        tokio::signal::unix::SignalKind::terminate())?;

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let h = sup.health().await;
                let next = if !h.session_running {
                    unhealthy = 0; attempts = 0;
                    SessionState::Stopped
                } else if h.is_healthy() {
                    if unhealthy > 0 { tracing::info!("session toparlandı"); }
                    unhealthy = 0; attempts = 0;
                    SessionState::Running
                } else {
                    unhealthy += 1;
                    tracing::warn!(
                        strike = unhealthy, threshold = cfg.unhealthy_threshold,
                        sorunlar = ?h.failures(), "session sağlıksız");
                    SessionState::Degraded
                };
                *state.write().await = next;

                // Eşik: tek bir dalgalanmada yeniden başlatma yapma.
                if cfg.auto_recover
                    && unhealthy >= cfg.unhealthy_threshold
                    && attempts < cfg.max_recovery_attempts
                {
                    attempts += 1;
                    tracing::error!(deneme = attempts, "kurtarma başlatılıyor");
                    *state.write().await = SessionState::Recovering;
                    if let Err(e) = sup.recover().await {
                        tracing::error!(hata = %e, "kurtarma başarısız");
                    }
                    unhealthy = 0;
                } else if attempts >= cfg.max_recovery_attempts && unhealthy > 0 {
                    tracing::error!(
                        "kurtarma denemeleri tükendi ({}); elle müdahale gerekiyor",
                        cfg.max_recovery_attempts);
                }
            }
            _ = sigterm.recv() => { tracing::info!("SIGTERM — çıkılıyor"); break; }
            _ = tokio::signal::ctrl_c() => { tracing::info!("SIGINT — çıkılıyor"); break; }
        }
    }
    // Keymapper'ı DURDURUYORUZ: kilitli bir cihazı sahipsiz bırakmak
    // kullanıcının klavyesini rehin alır.
    km.stop().await;
    // Not: session'ı kasıtlı olarak durdurmuyoruz. Daemon'un yeniden
    // başlatılması çalışan Android'i öldürmemeli.
    Ok(())
}
