//! Session yaşam döngüsü ve sağlık gözetimi.
//!
//! # Neden bu modül var
//!
//! `waydroid session start` ön planda çalışan bir süreçtir. Öldüğü anda zincir şu:
//!
//! ```text
//! composer HAL ölür
//!   -> SurfaceFlinger SIGABRT ("HIDL return status ... DEAD_OBJECT")
//!   -> system_server ölür
//!   -> tüm uygulamalar DeadSystemException
//! ```
//!
//! Android bundan tam kurtulamaz: sistem yeniden başlar ama **varsayılan rota
//! geri gelmez**, yani ağsız bir zombi kalır. Kullanıcıya görünen belirti
//! ("Play Store internet yok" gibi) kök nedene hiç benzemez.
//!
//! Bu yüzden session asla bir terminale bağlı olmamalı ve sahibi `liwd` olmalı.

use crate::helper::HelperClient;
use crate::waydroid::{Status, Waydroid, WaydroidError};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

/// Session'ın gözlemlenen durumu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SessionState {
    Stopped,
    Starting,
    Running,
    /// Süreçler ayakta ama sağlık kontrolleri geçmiyor (ağsız zombi hali).
    Degraded,
    Recovering,
}

impl SessionState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Stopped => "STOPPED",
            Self::Starting => "STARTING",
            Self::Running => "RUNNING",
            Self::Degraded => "DEGRADED",
            Self::Recovering => "RECOVERING",
        }
    }
}

/// Tek tek sağlık göstergeleri. Hepsi ayrı ayrı kırılabilir; bu yüzden
/// tek bir bayrak yerine ayrıntı tutuyoruz — bugünkü teşhis deneyimi
/// "çalışıyor/çalışmıyor" ikiliğinin yetmediğini gösterdi.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Health {
    pub session_running: bool,
    pub container_running: bool,
    pub has_ip: bool,
    pub boot_completed: bool,
    /// Waydroid composer HAL süreci yaşıyor mu. Ölürse çökme zinciri başlar.
    pub composer_alive: bool,
    /// composer, session'dan SONRA yeniden başlamış mı.
    ///
    /// Bu durumda session'ın binder bağlantısı bayattır: süreçler ayakta
    /// görünür, IP vardır, boot tamamlanmıştır — ama pencere oluşmaz ve
    /// `waydroid app launch` "Sending reply failed" der. Yani "her şey
    /// yolunda" diyen bir sağlık kontrolü yanıltıcı olur.
    pub composer_stale: bool,
}

impl Health {
    /// Oyun oynanabilir mi? Tüm göstergeler gerekli.
    pub fn is_healthy(&self) -> bool {
        self.session_running && self.container_running
            && self.has_ip && self.boot_completed
            && self.composer_alive && !self.composer_stale
    }
    /// Neyin eksik olduğunu insan okuyabilsin diye.
    pub fn failures(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if !self.session_running { v.push("session çalışmıyor"); }
        if !self.container_running { v.push("konteyner çalışmıyor"); }
        if !self.composer_alive { v.push("composer HAL ölü (çökme zinciri riski)"); }
        if self.composer_stale {
            v.push("composer session'dan sonra yeniden başlamış — bayat binder bağlantısı                     (pencere açılmaz, app launch 'Sending reply failed' der)");
        }
        if !self.boot_completed { v.push("Android boot tamamlanmadı"); }
        if !self.has_ip { v.push("IP atanmamış"); }
        v
    }
}

#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    pub poll_interval: Duration,
    pub boot_timeout: Duration,
    /// Ardışık kaç sağlıksız yoklamadan sonra kurtarma denensin.
    /// 1 çok agresif olur: geçici dalgalanmalarda gereksiz yeniden başlatma yapar.
    pub unhealthy_threshold: u32,
    pub max_recovery_attempts: u32,
    pub auto_recover: bool,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(5),
            boot_timeout: Duration::from_secs(180),
            unhealthy_threshold: 3,
            max_recovery_attempts: 3,
            auto_recover: true,
        }
    }
}

pub struct Supervisor {
    wd: Waydroid,
    cfg: SupervisorConfig,
    /// Ayrıcalıklı okumalar için. Yoksa boot durumu ÖLÇÜLEMEZ, çıkarsanır.
    helper: Option<HelperClient>,
}

impl Supervisor {
    pub fn new(cfg: SupervisorConfig) -> Self {
        Self { wd: Waydroid::new(), cfg, helper: None }
    }

    /// liwd-helper'a bağlanmayı dener. Başarısızlık ölümcül değildir:
    /// süpervizör helper'sız da çalışır, yalnızca boot durumu ölçülemez.
    pub async fn with_helper(mut self) -> Self {
        match HelperClient::connect().await {
            Ok(h) => { tracing::info!("liwd-helper bağlandı — boot durumu ölçülecek"); self.helper = Some(h); }
            Err(e) => tracing::warn!(hata = %e, "liwd-helper yok — boot durumu çıkarsanacak"),
        }
        self
    }

    pub fn has_helper(&self) -> bool { self.helper.is_some() }

    pub fn config(&self) -> &SupervisorConfig { &self.cfg }

    pub async fn status(&self) -> Result<Status, WaydroidError> {
        self.wd.status().await
    }

    /// Session'ı **terminalden bağımsız** başlatır.
    ///
    /// `setsid` ile yeni oturum lideri yapılır ve stdio kapatılır; böylece
    /// çağıran kabuk ölse (Ctrl+C, pencere kapatma) session ayakta kalır.
    pub async fn start_detached(&self) -> Result<(), WaydroidError> {
        let st = self.wd.status().await.unwrap_or_default();
        if st.session_running() {
            tracing::info!("session zaten çalışıyor");
            return Ok(());
        }
        tracing::info!("session başlatılıyor (ayrık)");
        Command::new("setsid")
            .args(["--fork", "waydroid", "session", "start"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?
            .wait()
            .await?;
        Ok(())
    }

    pub async fn stop(&self) -> Result<(), WaydroidError> {
        tracing::info!("session durduruluyor");
        self.wd.session_stop().await
    }

    /// Composer HAL süreci yaşıyor mu?
    ///
    /// Bu, çökme zincirinin ilk halkası — SurfaceFlinger'ın DEAD_OBJECT alması
    /// bundan sonra gelir. Süreç adı vendor imajına göre değişebildiği için
    /// tam eşleşme yerine desen araması yapıyoruz.
    async fn composer_alive(&self) -> bool {
        Command::new("pgrep")
            .args(["-f", "hardware.graphics.composer"])
            .stdout(Stdio::null()).stderr(Stdio::null())
            .status().await
            .map(|s| s.success()).unwrap_or(false)
    }

    /// Süreç başlangıç zamanını jiffy cinsinden okur (/proc/PID/stat, 22. alan).
    async fn start_time(pid: &str) -> Option<u64> {
        let stat = tokio::fs::read_to_string(format!("/proc/{pid}/stat")).await.ok()?;
        // comm alanı boşluk içerebilir; ')' sonrasından say.
        let rest = stat.rsplit_once(')')?.1;
        rest.split_whitespace().nth(19)?.parse().ok()
    }

    async fn newest_start(pattern: &str) -> Option<u64> {
        let out = Command::new("pgrep").args(["-f", pattern])
            .stderr(Stdio::null()).output().await.ok()?;
        let mut newest = None;
        for pid in String::from_utf8_lossy(&out.stdout).split_whitespace() {
            if let Some(t) = Self::start_time(pid).await {
                newest = Some(newest.map_or(t, |n: u64| n.max(t)));
            }
        }
        newest
    }

    /// composer, session sürecinden belirgin şekilde sonra mı başlamış?
    ///
    /// Eşik gerekli: normal başlatmada composer session'dan birkaç saniye
    /// sonra doğar. Sorun, ARADAN UZUN ZAMAN GEÇTİKTEN sonra yeniden
    /// doğmasıdır. 60 saniye, normal başlatma gecikmesinin çok üstünde.
    async fn composer_stale(&self) -> bool {
        const HZ: u64 = 100; // çekirdek USER_HZ
        const THRESHOLD_SEC: u64 = 60;
        let (Some(sess), Some(comp)) = (
            Self::newest_start("waydroid session start").await,
            Self::newest_start("hardware.graphics.composer").await,
        ) else { return false };
        comp.saturating_sub(sess) > THRESHOLD_SEC * HZ
    }

    pub async fn health(&self) -> Health {
        let st = self.wd.status().await.unwrap_or_default();
        let composer = self.composer_alive().await;
        let stale = composer && self.composer_stale().await;
        // boot_completed root ister. Önce helper'a sor (GERÇEK ölçüm);
        // helper yoksa doğrudan dene (liwd root çalışmadığı için genelde
        // başarısız); o da olmazsa session durumundan ÇIKARSA.
        // Çıkarsama son çare: yanlış negatif gereksiz yeniden başlatma yapar.
        let boot = match &self.helper {
            Some(h) => match h.boot_completed().await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(hata = %e, "helper boot sorgusu başarısız — çıkarsanıyor");
                    st.session_running()
                }
            },
            None => match self.wd.getprop("sys.boot_completed").await {
                Ok(v) => v.trim() == "1",
                Err(_) => st.session_running(),
            },
        };
        Health {
            session_running: st.session_running(),
            container_running: st.container_running(),
            has_ip: st.has_ip(),
            boot_completed: boot,
            composer_alive: composer,
            composer_stale: stale,
        }
    }

    pub async fn state(&self) -> SessionState {
        let h = self.health().await;
        if !h.session_running { return SessionState::Stopped; }
        if h.is_healthy() { SessionState::Running } else { SessionState::Degraded }
    }

    /// Bozulmuş session'ı tam döngüyle kurtarır.
    ///
    /// Kısmi kurtarma işe yaramaz: composer öldüğünde Android içeriden
    /// toparlanamaz, session'ın komple yeniden kurulması gerekir.
    pub async fn recover(&self) -> Result<(), WaydroidError> {
        tracing::warn!("session kurtarılıyor (tam yeniden başlatma)");
        let _ = self.wd.session_stop().await;
        tokio::time::sleep(Duration::from_secs(5)).await;
        self.start_detached().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn healthy_needs_every_signal() {
        let full = Health {
            session_running: true, container_running: true,
            has_ip: true, boot_completed: true,
            composer_alive: true, composer_stale: false,
        };
        assert!(full.is_healthy());
        assert!(full.failures().is_empty());
    }

    /// Bugünkü gerçek arıza: her şey ayakta ama IP yok (rota kaybolmuş).
    #[test]
    fn missing_ip_is_degraded_not_healthy() {
        let h = Health { has_ip: false, ..Health {
            session_running: true, container_running: true,
            has_ip: true, boot_completed: true,
            composer_alive: true, composer_stale: false } };
        assert!(!h.is_healthy());
        assert_eq!(h.failures(), vec!["IP atanmamış"]);
    }

    /// composer ölümü ayrıca raporlanmalı: çökme zincirinin kökü budur.
    #[test]
    fn dead_composer_is_reported_explicitly() {
        let h = Health {
            session_running: true, container_running: true,
            has_ip: true, boot_completed: true,
            composer_alive: false, composer_stale: false,
        };
        assert!(!h.is_healthy());
        assert!(h.failures().iter().any(|f| f.contains("composer")));
    }

    /// Gerçek vaka: her şey ayakta ama composer session'dan sonra yeniden
    /// başlamış. Pencere oluşmuyor, app launch "Sending reply failed" diyor.
    /// Sağlık kontrolü bunu YAKALAMALI, yoksa "sağlıklı" diyerek yanıltır.
    #[test]
    fn stale_composer_is_not_healthy() {
        let h = Health {
            session_running: true, container_running: true,
            has_ip: true, boot_completed: true,
            composer_alive: true, composer_stale: true,
        };
        assert!(!h.is_healthy(), "bayat composer sağlıklı sayılmamalı");
        assert!(h.failures().iter().any(|f| f.contains("bayat")), "{:?}", h.failures());
    }

    #[test]
    fn threshold_avoids_restarting_on_a_single_blip() {
        assert!(SupervisorConfig::default().unhealthy_threshold > 1);
    }
}
