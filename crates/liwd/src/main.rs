//! liwd — liwinux session daemon.
//!
//! Runs in the user context (a systemd user service); it cannot run as root
//! because the session needs access to the Wayland display. Privileged
//! operations live in a separate system helper behind polkit.

mod keymapper;
mod window;

use anyhow::Result;
use liw_core::{Health, SessionState, Supervisor, SupervisorConfig};
use std::sync::Arc;
use tokio::sync::RwLock;
use zbus::{connection, interface};

/// How often, in health ticks, the window is polled.
///
/// A health tick is 5 s; every 6th tick = 30 s. Closing and reopening the
/// window is noticed within half a minute — fullscreen is applied after the
/// window appears anyway, so the delay is not felt.
const WINDOW_POLL_EVERY: u32 = 6;
/// How often to re-check the kernel's module tree, in ticks (~5 minutes).
///
/// A kernel update is rare, but it happens WHILE we run and gives no other
/// sign. Noticing within minutes beats finding out hours later from a session
/// that will not start and an error message that blames the firewall.
const KERNEL_CHECK_EVERY: u32 = 60;

const BUS_NAME: &str = "id.liwinux.Manager1";
const OBJ_PATH: &str = "/id/liwinux/Manager1";

struct Manager {
    sup: Arc<Supervisor>,
    state: Arc<RwLock<SessionState>>,
    km: Arc<keymapper::Handle>,
    win: Arc<window::WindowState>,
}

#[interface(name = "id.liwinux.Manager1")]
impl Manager {
    /// Starts the session detached. A no-op if it is already running.
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

    /// The last state observed by the supervisor.
    #[zbus(property)]
    async fn state(&self) -> String {
        self.state.read().await.as_str().to_string()
    }

    /// Detailed health as JSON. Reports each signal separately, because
    /// "not working" alone is not enough to diagnose.
    async fn health(&self) -> zbus::fdo::Result<String> {
        let h: Health = self.sup.health().await;
        serde_json::to_string(&h).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    /// Starts the keymapper. A no-op if it is already running.
    ///
    /// With `grab` true the device is grabbed ONLY while a profile is active;
    /// released on exit. A non-working keyboard on the desktop is unacceptable.
    async fn start_keymapper(&self, grab: bool) -> zbus::fdo::Result<()> {
        self.km.start(grab).await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    async fn stop_keymapper(&self) -> zbus::fdo::Result<()> {
        self.km.stop().await;
        Ok(())
    }

    /// KWin script'inin pencere geometrisi geri bildirimi.
    async fn report_window_geometry(
        &self, found: bool, x: i32, y: i32, width: i32, height: i32, fullscreen: bool,
    ) -> zbus::fdo::Result<()> {
        self.win.set(window::WindowGeometry {
            found, x, y, width, height, fullscreen,
        }).await;
        if found {
            // The engine needs the real pixel size for aspect correction.
            self.km.set_screen_px(width as u32, height as u32).await;
        }
        Ok(())
    }

    /// Tries to make the Waydroid window fullscreen.
    async fn fullscreen(&self) -> zbus::fdo::Result<bool> {
        Ok(window::fullscreen_with_retry(
            self.win.clone(), 5, std::time::Duration::from_millis(700)).await)
    }

    /// Raises and focuses the Waydroid window.
    async fn activate_window(&self) -> zbus::fdo::Result<()> {
        window::activate().await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    /// Pencere geometrisi (JSON).
    async fn window_geometry(&self) -> zbus::fdo::Result<String> {
        let g = self.win.get().await;
        serde_json::to_string(&g).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    /// Callback invoked by the KWin script: which window has focus.
    ///
    /// Android does not know the window was minimised; without this the mapping
    /// keeps running with the game in the background and touches land on the
    /// desktop.
    async fn set_active_window(&self, class: &str) -> zbus::fdo::Result<()> {
        self.km.set_active_window(class).await;

        // Make the window fullscreen the FIRST time it activates.
        //
        // Trying at session start is not enough: the window may not exist yet
        // (`show-full-ui` is a separate step and we cannot know when the user
        // will run it). Being event-driven is the only correct approach.
        //
        // The "first time only" condition is deliberate: if the user left
        // fullscreen on purpose, forcing it back on every focus would be hostile.
        if class.eq_ignore_ascii_case("waydroid")
            && !self.win.fullscreen_attempted().await
            && liw_core::Config::load().fullscreen_on_start
        {
            self.win.mark_fullscreen_attempted().await;
            let w = self.win.clone();
            tokio::spawn(async move {
                window::fullscreen_with_retry(
                    w, 3, std::time::Duration::from_millis(500)).await;
            });
        }
        Ok(())
    }

    /// Keymapper state as JSON: running, foreground, active profile, latency.
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
    let win = window::WindowState::new();

    let conn = connection::Builder::session()?
        .name(BUS_NAME)?
        .serve_at(OBJ_PATH, Manager {
            sup: sup.clone(), state: state.clone(),
            km: km.clone(), win: win.clone(),
        })?
        .build()
        .await?;
    tracing::info!("liwd ready — {BUS_NAME} {OBJ_PATH}");

    // Start the keymapper automatically.
    //
    // It used to start only on an explicit D-Bus call and vanished SILENTLY
    // after every `systemctl --user restart liwd`. All the user saw was
    // "it is not taking input".
    //
    // Trying with no keyboard configured is pointless — but passing over that
    // silently would repeat the same mistake, so say it.
    {
        let c = liw_core::Config::load();
        if !c.keymapper_on_start {
            tracing::info!("keymapper autostart disabled (keymapper_on_start = false)");
        } else if c.keyboard.is_none() {
            tracing::warn!(
                "keymapper not started: no keyboard configured \
                 — calibrate it with `liw keymap detect --save`");
        } else if let Err(e) = km.start(true).await {
            tracing::error!(error = %e, "could not autostart the keymapper");
        }
    }

    // If we lose our name we MUST EXIT.
    //
    // This actually happened: an old liwd lost its name but kept running. An
    // unreachable daemon can still grab devices and inject touches — the user's
    // keyboard stops working and they cannot find out why. systemd will restart
    // us anyway; dying beats lingering as a zombie.
    let dbus = zbus::fdo::DBusProxy::new(&conn).await?;
    let own_id = conn.unique_name().map(|n| n.to_string());

    // --- supervision loop ---
    let mut unhealthy = 0u32;
    let mut attempts = 0u32;
    let mut was_running = false;
    let mut win_tick: u32 = 0;
    let mut kern_tick: u32 = 0;
    let mut kernel = liw_core::host::KernelWatch::new();
    let mut ticker = tokio::time::interval(cfg.poll_interval);
    let mut sigterm = tokio::signal::unix::signal(
        tokio::signal::unix::SignalKind::terminate())?;

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                // The kernel's module tree can be deleted underneath a running
                // system by a package update. Report the change, not the state,
                // so a healthy machine stays quiet.
                if kern_tick % KERNEL_CHECK_EVERY == 0 {
                    let stale = liw_core::host::check_modules();
                    if let Some(changed) = kernel.poll(stale.is_some()) {
                        match (&stale, changed) {
                            (Some(s), true) => tracing::warn!(
                                running = %s.running,
                                on_disk = %s.available.join(", "),
                                "the running kernel's modules were removed by an \
                                 update; nothing new can be loaded until reboot, \
                                 and a session start will fail with an error that \
                                 does not mention modules"),
                            (_, false) => tracing::info!(
                                "kernel modules in place for the running kernel"),
                            _ => {}
                        }
                    }
                }
                kern_tick = kern_tick.wrapping_add(1);

                let h = sup.health().await;
                let next = if !h.session_running {
                    unhealthy = 0; attempts = 0;
                    if was_running { win.reset().await; }
                    was_running = false;
                    SessionState::Stopped
                } else if h.is_healthy() {
                    if unhealthy > 0 { tracing::info!("session recovered"); }
                    unhealthy = 0; attempts = 0;

                    // Poll the window state RARELY.
                    //
                    // The health loop ticks every 5 seconds; loading a KWin
                    // script on every tick meant 720 loads per hour. Keeping
                    // KWin's scripting engine that busy is needless risk —
                    // noticing the window disappeared does not need 5-second
                    // resolution.
                    //
                    // Also polled ONLY if fullscreen was attempted: if it was
                    // not, there is nothing to reset.
                    win_tick = win_tick.wrapping_add(1);
                    if was_running
                        && win_tick % WINDOW_POLL_EVERY == 0
                        && win.fullscreen_attempted().await
                    {
                        let _ = window::request_report().await;
                        if win.note_window_gone().await {
                            tracing::info!(
                                "Waydroid window closed — fullscreen will be retried");
                        }
                    }
                    // If the session just came up, make the window fullscreen.
                    // The window may NOT exist when boot completes, so it must
                    // retry; and we trigger ONLY on the transition, not on
                    // every loop.
                    if !was_running && liw_core::Config::load().fullscreen_on_start {
                        let w = win.clone();
                        tokio::spawn(async move {
                            window::fullscreen_with_retry(
                                w, 6, std::time::Duration::from_millis(1200)).await;
                        });
                    }
                    was_running = true;
                    SessionState::Running
                } else {
                    unhealthy += 1;
                    tracing::warn!(
                        strike = unhealthy, threshold = cfg.unhealthy_threshold,
                        failures = ?h.failures(), "session unhealthy");
                    SessionState::Degraded
                };
                *state.write().await = next;

                // Threshold: do not restart on a single dip.
                if cfg.auto_recover
                    && unhealthy >= cfg.unhealthy_threshold
                    && attempts < cfg.max_recovery_attempts
                {
                    attempts += 1;
                    tracing::error!(attempt = attempts, "starting recovery");
                    *state.write().await = SessionState::Recovering;
                    if let Err(e) = sup.recover().await {
                        tracing::error!(error = %e, "recovery failed");
                    }
                    unhealthy = 0;
                } else if attempts >= cfg.max_recovery_attempts && unhealthy > 0 {
                    tracing::error!(
                        "recovery attempts exhausted ({}); manual intervention needed",
                        cfg.max_recovery_attempts);
                }
            }
            _ = sigterm.recv() => { tracing::info!("SIGTERM — exiting"); break; }
            _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {
                // Ownership poll. Polling rather than listening for a signal:
                // basit, ve 10 saniyelik gecikme zombi riskini kapatmaya yeter.
                match dbus.get_name_owner(BUS_NAME.try_into().unwrap()).await {
                    Ok(owner) if Some(owner.to_string()) == own_id => {}
                    Ok(other) => {
                        tracing::error!(
                            sahip = %other, bizim = ?own_id,
                            "the D-Bus name went to someone else — exiting (to avoid a zombie)");
                        break;
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "could not query the D-Bus name — exiting");
                        break;
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => { tracing::info!("SIGINT — exiting"); break; }
        }
    }
    // We STOP the keymapper: leaving a grabbed device ownerless holds the
    // user's keyboard hostage.
    km.stop().await;
    // Note: we deliberately do not stop the session. Restarting the daemon
    // must not kill a running Android.
    Ok(())
}
