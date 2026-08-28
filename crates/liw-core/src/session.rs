//! Session lifecycle and health supervision.
//!
//! # Why this module exists
//!
//! `waydroid session start` runs in the foreground. The moment it dies the
//! chain is:
//!
//! ```text
//! composer HAL dies
//!   -> SurfaceFlinger SIGABRT ("HIDL return status ... DEAD_OBJECT")
//!   -> system_server dies
//!   -> every app gets DeadSystemException
//! ```
//!
//! Android never fully recovers: the system restarts but the **default route
//! does not come back**, leaving a network-less zombie. The visible symptom
//! ("Play Store has no internet") looks nothing like the root cause.
//!
//! That is why the session must never be tied to a terminal, and `liwd` must
//! own it.

use crate::helper::HelperClient;
use crate::waydroid::{Status, Waydroid, WaydroidError};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

/// Observed state of the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SessionState {
    Stopped,
    Starting,
    Running,
    /// Processes are up but health checks fail (the network-less zombie state).
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

/// Individual health signals. Each can break on its own, which is why we keep
/// the detail rather than a single flag — diagnostic experience showed a
/// "works / does not work" binary is not enough.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Health {
    pub session_running: bool,
    pub container_running: bool,
    pub has_ip: bool,
    pub boot_completed: bool,
    /// Is the Waydroid composer HAL process alive? Its death starts the chain.
    pub composer_alive: bool,
    /// Did composer restart AFTER the session?
    ///
    /// If so the session's binder connection is stale: processes look alive,
    /// there is an IP, boot completed — but no window appears and
    /// `waydroid app launch` says "Sending reply failed". A health check
    /// reporting "all good" would be misleading.
    pub composer_stale: bool,
}

impl Health {
    /// Is the game playable? Every signal is required.
    pub fn is_healthy(&self) -> bool {
        self.session_running && self.container_running
            && self.has_ip && self.boot_completed
            && self.composer_alive && !self.composer_stale
    }
    /// So a human can read what is missing.
    pub fn failures(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if !self.session_running { v.push("session not running"); }
        if !self.container_running { v.push("container not running"); }
        if !self.composer_alive { v.push("composer HAL dead (crash-chain risk)"); }
        if self.composer_stale {
            v.push("composer restarted after the session — stale binder connection                     (no window appears, app launch says 'Sending reply failed')");
        }
        if !self.boot_completed { v.push("Android boot did not complete"); }
        if !self.has_ip { v.push("no IP assigned"); }
        v
    }
}

/// Marker in the Android log for AudioFlinger failing to publish.
const AF_MARKER: &str = "AudioFlinger not published";

/// Detects from the log whether the audio stack is wedged.
///
/// The chain works like this and all of it was measured: the audio HAL wedges
/// -> `audioserver` blocks in its `registerClient` call -> the `mediautils`
/// watchdog aborts the process after 5 seconds -> it restarts and wedges in
/// the same place. Result: `AudioFlinger` NEVER publishes to the service
/// manager and every app that touches audio waits forever.
///
/// The symptom the user sees is "the game freezes on its loading screen"; there
/// is nothing anywhere to suggest audio is involved. Hence the detection.
///
/// ONE line is NOT enough: during boot AudioFlinger genuinely is waited for a
/// few times and that is normal. Only persistent repetition indicates a fault.
pub fn audio_flinger_stalled(log: &str) -> bool {
    const MIN_REPEATS: usize = 5;
    log.lines().filter(|l| l.contains(AF_MARKER)).count() >= MIN_REPEATS
}

#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    pub poll_interval: Duration,
    pub boot_timeout: Duration,
    /// After how many consecutive unhealthy polls to attempt recovery.
    /// 1 is too aggressive: transient dips would trigger pointless restarts.
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
    /// For privileged reads. Without it the boot state cannot be MEASURED, only
    /// inferred.
    helper: Option<HelperClient>,
}

impl Supervisor {
    pub fn new(cfg: SupervisorConfig) -> Self {
        Self { wd: Waydroid::new(), cfg, helper: None }
    }

    /// Tries to connect to liwd-helper. Failure is not fatal: the supervisor
    /// works without it, only the boot state cannot be measured.
    pub async fn with_helper(mut self) -> Self {
        match HelperClient::connect().await {
            Ok(h) => { tracing::info!("liwd-helper connected — boot state will be measured"); self.helper = Some(h); }
            Err(e) => tracing::warn!(error = %e, "no liwd-helper — boot state will be inferred"),
        }
        self
    }

    pub fn has_helper(&self) -> bool { self.helper.is_some() }

    pub fn config(&self) -> &SupervisorConfig { &self.cfg }

    pub async fn status(&self) -> Result<Status, WaydroidError> {
        self.wd.status().await
    }

    /// Starts the session **detached from any terminal**.
    ///
    /// `setsid` makes it a new session leader and stdio is closed, so the
    /// session survives the calling shell dying (Ctrl+C, closing the window).
    pub async fn start_detached(&self) -> Result<(), WaydroidError> {
        let st = self.wd.status().await.unwrap_or_default();
        if st.session_running() {
            tracing::info!("session already running");
            return Ok(());
        }
        tracing::info!("starting session (detached)");
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

    /// Is the composer HAL process alive?
    ///
    /// This is the first link of the crash chain — SurfaceFlinger getting
    /// DEAD_OBJECT comes after it. The process name varies by vendor image, so
    /// we pattern-match rather than compare exactly.
    async fn composer_alive(&self) -> bool {
        Command::new("pgrep")
            .args(["-f", "hardware.graphics.composer"])
            .stdout(Stdio::null()).stderr(Stdio::null())
            .status().await
            .map(|s| s.success()).unwrap_or(false)
    }

    /// Reads a process start time in jiffies (/proc/PID/stat, field 22).
    async fn start_time(pid: &str) -> Option<u64> {
        let stat = tokio::fs::read_to_string(format!("/proc/{pid}/stat")).await.ok()?;
        // The comm field may contain spaces; count from after the ')'.
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

    /// Did composer start noticeably later than the session process?
    ///
    /// A threshold is needed: on a normal start composer appears a few seconds
    /// after the session. The problem is it being reborn LONG AFTERWARDS.
    /// 60 seconds is far above the normal startup delay.
    async fn composer_stale(&self) -> bool {
        const HZ: u64 = 100; // kernel USER_HZ
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
        // boot_completed needs root. Ask the helper first (a REAL measurement);
        // without a helper try directly (usually fails because liwd does not run
        // as root); failing that, INFER from the session state.
        // Inference is the last resort: a false negative causes a pointless restart.
        let boot = match &self.helper {
            Some(h) => match h.boot_completed().await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, "helper boot query failed — inferring");
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

    /// Recovers a broken session with a full cycle.
    ///
    /// Partial recovery does not work: once composer dies Android cannot
    /// recover from the inside; the session must be rebuilt completely.
    pub async fn recover(&self) -> Result<(), WaydroidError> {
        tracing::warn!("recovering session (full restart)");
        let _ = self.wd.session_stop().await;
        tokio::time::sleep(Duration::from_secs(5)).await;
        self.start_detached().await
    }
}

#[cfg(test)]
mod tests {
    /// Real measurement: with the HAL wedged the log fills with this line.
    #[test]
    fn detects_wedged_audio_from_real_log() {
        let real = "08-27 17:26:45.329  1086 20805 W AudioSystem: AudioFlinger not published, waiting...\n\
                    08-27 17:26:50.830  1086 20805 W AudioSystem: AudioFlinger not published, waiting...\n\
                    08-27 17:26:56.331  1086 20805 W AudioSystem: AudioFlinger not published, waiting...\n\
                    08-27 17:27:01.832  1086 20805 W AudioSystem: AudioFlinger not published, waiting...\n\
                    08-27 17:27:07.333  1086 20805 W AudioSystem: AudioFlinger not published, waiting...\n";
        assert!(super::audio_flinger_stalled(real));
    }

    /// Waiting a few times at boot is NORMAL — it must not panic.
    #[test]
    fn a_few_waits_during_boot_are_normal() {
        let boot = "W AudioSystem: AudioFlinger not published, waiting...\n\
                    I boot: devam\n\
                    W AudioSystem: AudioFlinger not published, waiting...\n";
        assert!(!super::audio_flinger_stalled(boot));
    }

    #[test]
    fn healthy_log_is_not_flagged() {
        assert!(!super::audio_flinger_stalled("I ActivityManager: all is well"));
        assert!(!super::audio_flinger_stalled(""));
    }

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

    /// A real failure seen today: everything up but no IP (route lost).
    #[test]
    fn missing_ip_is_degraded_not_healthy() {
        let h = Health { has_ip: false, ..Health {
            session_running: true, container_running: true,
            has_ip: true, boot_completed: true,
            composer_alive: true, composer_stale: false } };
        assert!(!h.is_healthy());
        assert_eq!(h.failures(), vec!["no IP assigned"]);
    }

    /// Composer death must be reported separately: it is the root of the chain.
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

    /// Real case: everything up but composer restarted after the session.
    /// No window appears and app launch says "Sending reply failed".
    /// The health check MUST catch this, or it misleads by reporting "healthy".
    #[test]
    fn stale_composer_is_not_healthy() {
        let h = Health {
            session_running: true, container_running: true,
            has_ip: true, boot_completed: true,
            composer_alive: true, composer_stale: true,
        };
        assert!(!h.is_healthy(), "a stale composer must not count as healthy");
        assert!(h.failures().iter().any(|f| f.contains("stale")), "{:?}", h.failures());
    }

    #[test]
    fn threshold_avoids_restarting_on_a_single_blip() {
        assert!(SupervisorConfig::default().unhealthy_threshold > 1);
    }
}
