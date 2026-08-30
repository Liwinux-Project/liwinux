//! liwd — liwinux session daemon.
//!
//! Runs in the user context (a systemd user service); it cannot run as root
//! because the session needs access to the Wayland display. Privileged
//! operations live in a separate system helper behind polkit.

mod keymapper;
mod window;

use anyhow::Result;
use liw_core::input::RunnerEvent;
use liw_core::{Error, Health, SessionState, Supervisor, SupervisorConfig};
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
    /// Last health computed by the supervision loop.
    ///
    /// Cached on purpose. `Supervisor::health()` shells out to
    /// `waydroid status` and asks the helper for `sys.boot_completed`
    /// over lxc-attach; it costs hundreds of milliseconds. A UI that
    /// polls it once a second would keep the machine busy doing nothing.
    /// The loop already computes it every 5 s — serve that.
    health: Arc<RwLock<Health>>,
}

#[interface(name = "id.liwinux.Manager1")]
impl Manager {
    /// Starts the session detached. A no-op if it is already running.
    async fn start(&self) -> Result<(), Error> {
        self.sup.start_detached().await.map_err(|e| Error::Failed(e.to_string()))
    }

    /// Points new sessions at a window's Wayland socket.
    ///
    /// An empty name restores the default. Held only in memory: the name
    /// belongs to a window that is open right now, and it is checked before
    /// every use so a closed window cannot strand the next session on a
    /// socket nobody is listening to.
    async fn set_embedded_display(&self, name: &str) -> Result<(), Error> {
        let name = name.trim();
        self.sup.set_embedded_display(
            (!name.is_empty()).then(|| name.to_string()));
        tracing::info!(display = name, "embedded display set");
        Ok(())
    }

    async fn stop(&self) -> Result<(), Error> {
        self.sup.stop().await.map_err(|e| Error::Failed(e.to_string()))
    }

    async fn restart(&self) -> Result<(), Error> {
        self.sup.recover().await.map_err(|e| Error::Failed(e.to_string()))
    }

    /// The last state observed by the supervisor.
    #[zbus(property)]
    async fn state(&self) -> String {
        self.state.read().await.as_str().to_string()
    }

    /// Detailed health as JSON, MEASURED NOW.
    ///
    /// Costly (see `Manager::health`); for continuous display use the
    /// `HealthJson` property instead, which the loop keeps fresh.
    async fn health(&self) -> Result<String, Error> {
        let h: Health = self.sup.health().await;
        *self.health.write().await = h.clone();
        serde_json::to_string(&h).map_err(Error::from)
    }

    /// Cached health as JSON; refreshed by the supervision loop and
    /// announced with a change signal. Free to read.
    #[zbus(property)]
    async fn health_json(&self) -> String {
        serde_json::to_string(&*self.health.read().await).unwrap_or_default()
    }

    /// Is game mode on (grab + mapping active)?
    #[zbus(property)]
    async fn game_mode(&self) -> bool { self.km.state().await.game_mode }

    /// Are the input devices grabbed right now?
    #[zbus(property)]
    async fn grabbed(&self) -> bool { self.km.state().await.grabbed }

    /// Is the Waydroid window focused on the host?
    #[zbus(property)]
    async fn host_focused(&self) -> bool { self.km.state().await.host_focused }

    /// Active profile name; empty when there is none.
    ///
    /// Empty rather than absent: D-Bus has no null, and an "absent" flag
    /// alongside would be one more thing for a client to get wrong.
    #[zbus(property)]
    async fn active_profile(&self) -> String {
        self.km.state().await.active_profile.unwrap_or_default()
    }

    /// Foreground Android package; empty when unknown.
    #[zbus(property)]
    async fn foreground_package(&self) -> String {
        self.km.state().await.foreground.unwrap_or_default()
    }

    /// Installed Android apps, as a JSON array.
    ///
    /// Read from the `.desktop` files Waydroid writes, so this works with
    /// the session stopped — a UI can draw its library before Android is
    /// even up, which is when the user is most likely looking at it.
    async fn list_apps(&self) -> Result<String, Error> {
        Ok(serde_json::to_string(&liw_core::apps::discover())?)
    }

    /// Launches an Android app.
    async fn launch_app(&self, package: &str) -> Result<(), Error> {
        if !liw_core::apps::valid_package(package) {
            return Err(Error::Invalid(format!("not a package name: {package:?}")));
        }
        // Refuse early with a NAME the client can act on. `waydroid app
        // launch` against a stopped session fails with prose about D-Bus
        // that tells the user nothing about what to do.
        if !self.health.read().await.session_running {
            return Err(Error::NoSession(
                "the session is stopped — start it before launching an app".into()));
        }
        let out = tokio::process::Command::new("waydroid")
            .args(["app", "launch", package])
            .stdin(std::process::Stdio::null())
            .output().await
            .map_err(|e| Error::Failed(e.to_string()))?;
        if !out.status.success() {
            return Err(Error::Failed(format!(
                "waydroid app launch failed: {}",
                String::from_utf8_lossy(&out.stderr).trim())));
        }
        Ok(())
    }

    /// The user configuration, as JSON.
    async fn get_config(&self) -> Result<String, Error> {
        Ok(serde_json::to_string(&liw_core::Config::load())?)
    }

    /// Replaces the user configuration.
    ///
    /// Whole-document rather than per-key: a settings screen edits a form and
    /// saves it, and a per-key interface would need every field named twice
    /// and kept in step.
    ///
    /// Does NOT restart the keymapper. Several of these only take effect at
    /// its next start, and restarting it as a side effect of a save would
    /// yank the devices out from under a running game.
    async fn set_config(
        &self, json: &str,
        #[zbus(signal_emitter)] em: zbus::object_server::SignalEmitter<'_>,
    ) -> Result<(), Error> {
        let cfg: liw_core::Config = serde_json::from_str(json)
            .map_err(|e| Error::Invalid(e.to_string()))?;
        cfg.save().map_err(|e| Error::Failed(e.to_string()))?;
        let _ = Manager::keymapper_event(&em, "config-changed", "").await;
        Ok(())
    }

    /// Input devices we could map from, as JSON.
    ///
    /// Read here rather than in the client because it needs `/dev/input`
    /// access, and because the daemon already knows which one is in use.
    async fn list_input_devices(&self) -> Result<String, Error> {
        let cfg = liw_core::Config::load();
        // Comparison is by CANONICAL path, not by string.
        //
        // The config stores a stable `/dev/input/by-id/...` symlink while
        // discovery reports `/dev/input/eventN`; comparing the two as text
        // never matches, and the settings screen showed nothing selected
        // while the mapper was happily using that very device.
        let real = |p: &std::path::Path| std::fs::canonicalize(p).ok();
        let kb = cfg.keyboard.as_deref().and_then(real);
        let ms = cfg.mouse.as_deref().and_then(real);

        let devs = liw_core::input::discover();
        let items: Vec<_> = devs.iter().map(|d| {
            let canon = real(&d.path);
            serde_json::json!({
                "path": d.path.display().to_string(),
                // The stable name, so a saved choice survives a reboot —
                // eventN numbers are reassigned and a config pointing at one
                // silently ends up on a sound card.
                "stable_path": liw_core::input::capture::stable_path(&d.path)
                    .map(|p| p.display().to_string()),
                "name": d.name,
                "kind": format!("{:?}", d.kind),
                // A device we created ourselves must never be offered as a
                // source: mapping our own virtual touchscreen back into
                // itself is a feedback loop.
                "virtual": d.virtual_device,
                "typing_score": d.typing_score,
                "is_keyboard": canon.is_some() && canon == kb,
                "is_mouse": canon.is_some() && canon == ms,
            })
        }).collect();
        Ok(serde_json::to_string(&items)?)
    }

    /// Installs an APK the user pointed at.
    ///
    /// The path comes from the caller, so it is checked here rather than
    /// trusted: a UI file picker cannot hand us something else, but the bus
    /// is open to anything on the session and the error should say what is
    /// wrong instead of letting `waydroid` fail obscurely.
    async fn install_apk(&self, path: &str) -> Result<(), Error> {
        let p = std::path::Path::new(path);
        if !p.is_file() {
            return Err(Error::Invalid(format!("no such file: {path}")));
        }
        if !p.extension().is_some_and(|e| e.eq_ignore_ascii_case("apk")) {
            return Err(Error::Invalid("not an .apk".into()));
        }
        if !self.health.read().await.session_running {
            return Err(Error::NoSession(
                "the session is stopped — start it before installing".into()));
        }
        let out = tokio::process::Command::new("waydroid")
            .args(["app", "install", path])
            .stdin(std::process::Stdio::null())
            .output().await
            .map_err(|e| Error::Failed(e.to_string()))?;
        if !out.status.success() {
            return Err(Error::Failed(format!(
                "install failed: {}", String::from_utf8_lossy(&out.stderr).trim())));
        }
        Ok(())
    }

    /// Opens an app's Play Store page inside Android.
    ///
    /// There is no catalogue of our own to install from, and there will not
    /// be one — that is a content operation. What we DO have is the list of
    /// games with a tested key mapping, and the store already knows how to
    /// install those: `market://details` opens straight at the page.
    async fn open_store_page(&self, package: &str) -> Result<(), Error> {
        if !liw_core::apps::valid_package(package) {
            return Err(Error::Invalid(format!("not a package name: {package:?}")));
        }
        if !self.health.read().await.session_running {
            return Err(Error::NoSession(
                "the session is stopped — start it before opening the store".into()));
        }
        let out = tokio::process::Command::new("waydroid")
            .args(["app", "intent", "android.intent.action.VIEW",
                   &format!("market://details?id={package}")])
            .stdin(std::process::Stdio::null())
            .output().await
            .map_err(|e| Error::Failed(e.to_string()))?;
        if !out.status.success() {
            return Err(Error::Failed(format!(
                "could not open the store: {}",
                String::from_utf8_lossy(&out.stderr).trim())));
        }
        Ok(())
    }

    /// Every known profile, as a JSON array.
    ///
    /// Summaries only — a UI listing them does not need every binding, and
    /// sending them all would make the list call grow with the profile
    /// count. `GetProfile` returns the full thing.
    #[allow(clippy::unused_async)]
    async fn list_profiles(&self) -> Result<String, Error> {
        let store = liw_core::input::Store::discover();
        let items: Vec<_> = store.entries().map(|e| serde_json::json!({
            "package": e.profile.package,
            "name": e.profile.name,
            "path": e.path.display().to_string(),
            "origin": format!("{:?}", e.origin),
            "bindings": e.profile.bindings.len(),
            // Only user profiles can be written or removed. Without this a
            // UI would offer Delete on a system profile and then fail.
            "editable": e.origin == liw_core::input::Origin::User,
        })).collect();
        // Broken files are reported too. Swallowing them leaves "why is my
        // profile not working" unanswered.
        let problems: Vec<_> = store.problems.iter().map(|p| serde_json::json!({
            "path": p.path.display().to_string(),
            "error": p.error,
        })).collect();
        Ok(serde_json::to_string(&serde_json::json!({
            "profiles": items, "problems": problems }))?)
    }

    /// One profile in full, as JSON.
    async fn get_profile(&self, package: &str) -> Result<String, Error> {
        let store = liw_core::input::Store::discover();
        let e = store.for_package(package)
            .ok_or_else(|| Error::NoProfile(package.to_string()))?;
        Ok(serde_json::to_string(&e.profile)?)
    }

    /// Saves a profile given as JSON. Returns the path written.
    ///
    /// The package comes from the payload, so a client cannot save a
    /// profile under one name while it declares another.
    ///
    /// Writing a system profile edits a NEW user file that shadows it; the
    /// original is left alone so a package update neither loses the user's
    /// edits nor overwrites them.
    async fn save_profile(
        &self, json: &str,
        #[zbus(signal_emitter)] em: zbus::object_server::SignalEmitter<'_>,
    ) -> Result<String, Error> {
        let p: liw_core::input::Profile = serde_json::from_str(json)
            .map_err(|e| Error::Invalid(e.to_string()))?;
        let store = liw_core::input::Store::discover();
        let path = store.save(&p).map_err(|e| match e {
            liw_core::input::ProfileError::Invalid(m) => Error::Invalid(m),
            other => Error::Failed(other.to_string()),
        })?;
        // The running keymapper still holds the OLD profile: it loads the
        // store when it starts. Restarting it here would be a surprising
        // side effect of a save, so say it happened and let the client
        // decide.
        let _ = Manager::keymapper_event(
            &em, "profiles-changed", &path.display().to_string()).await;
        Ok(path.display().to_string())
    }

    /// Deletes the user profile for a package. Returns the path removed.
    async fn delete_profile(
        &self, package: &str,
        #[zbus(signal_emitter)] em: zbus::object_server::SignalEmitter<'_>,
    ) -> Result<String, Error> {
        let store = liw_core::input::Store::discover();
        let path = store.delete(package).map_err(|e| match e {
            liw_core::input::ProfileError::Invalid(m) => Error::Invalid(m),
            other => Error::Failed(other.to_string()),
        })?;
        let _ = Manager::keymapper_event(
            &em, "profiles-changed", &path.display().to_string()).await;
        Ok(path.display().to_string())
    }

    /// A transient keymapper event.
    ///
    /// Only for things that are NOT state: a system overlay covering the
    /// game, an escape request. Everything durable is a property, so a
    /// client that misses a signal can still read the truth.
    #[zbus(signal)]
    async fn keymapper_event(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        kind: &str, detail: &str,
    ) -> zbus::Result<()>;

    /// Starts the keymapper. A no-op if it is already running.
    ///
    /// With `grab` true the device is grabbed ONLY while a profile is active;
    /// released on exit. A non-working keyboard on the desktop is unacceptable.
    async fn start_keymapper(
        &self, grab: bool,
        #[zbus(signal_emitter)] em: zbus::object_server::SignalEmitter<'_>,
    ) -> Result<(), Error> {
        self.km.start(grab).await.map_err(|e| Error::Failed(e.to_string()))?;
        // Announce HERE, not from the event forwarder.
        //
        // The forwarder only runs once the runner emits something, and a
        // keymapper that started with no game in the foreground emits
        // nothing for a while. A UI would show it as stopped until the
        // first unrelated event happened to arrive.
        let _ = self.keymapper_running_changed(&em).await;
        Ok(())
    }

    async fn stop_keymapper(
        &self,
        #[zbus(signal_emitter)] em: zbus::object_server::SignalEmitter<'_>,
    ) -> Result<(), Error> {
        self.km.stop().await;
        // On stop the runner is gone, so no event will ever follow.
        let _ = self.keymapper_running_changed(&em).await;
        let _ = self.game_mode_changed(&em).await;
        let _ = self.grabbed_changed(&em).await;
        let _ = self.active_profile_changed(&em).await;
        Ok(())
    }

    /// Window-geometry feedback from the KWin script.
    async fn report_window_geometry(
        &self, found: bool, x: i32, y: i32, width: i32, height: i32, fullscreen: bool,
    ) -> Result<(), Error> {
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
    async fn fullscreen(&self) -> Result<bool, Error> {
        Ok(window::fullscreen_with_retry(
            self.win.clone(), 5, std::time::Duration::from_millis(700)).await)
    }

    /// Raises and focuses the Waydroid window.
    async fn activate_window(&self) -> Result<(), Error> {
        window::activate().await.map_err(|e| Error::NoWindow(e.to_string()))
    }

    /// Window geometry (JSON).
    async fn window_geometry(&self) -> Result<String, Error> {
        let g = self.win.get().await;
        Ok(serde_json::to_string(&g)?)
    }

    /// Callback invoked by the KWin script: which window has focus.
    ///
    /// Android does not know the window was minimised; without this the mapping
    /// keeps running with the game in the background and touches land on the
    /// desktop.
    async fn set_active_window(&self, class: &str) -> Result<(), Error> {
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
    async fn keymapper_status(&self) -> Result<String, Error> {
        let st = self.km.state().await;
        Ok(serde_json::to_string(&st)?)
    }

    #[zbus(property)]
    async fn keymapper_running(&self) -> bool {
        self.km.state().await.running
    }

    async fn status(&self) -> Result<String, Error> {
        let s = self.sup.status().await
            .map_err(|e| Error::NoSession(e.to_string()))?;
        Ok(serde_json::to_string(&s)?)
    }
}

/// Forwards keymapper events onto the bus.
///
/// Two shapes, on purpose. Transient things (an overlay covering the game,
/// an escape request) go out as a SIGNAL because there is no state to read
/// afterwards. Durable things (game mode, grab, focus, active profile) go
/// out as PROPERTY changes, so a client that missed a signal — or that just
/// connected — can still read the truth.
///
/// Only fields that actually changed are announced; zbus does not diff, and
/// re-announcing everything on every event would make a UI redraw
/// constantly during normal play.
async fn emit_keymapper(
    iface: zbus::object_server::InterfaceRef<Manager>,
    km: Arc<keymapper::Handle>,
    mut rx: tokio::sync::mpsc::Receiver<RunnerEvent>,
) {
    let mut last = km.state().await;
    while let Some(ev) = rx.recv().await {
        let (kind, detail) = match &ev {
            RunnerEvent::ProfileActivated { package, profile } =>
                ("profile-activated", format!("{package}\t{profile}")),
            RunnerEvent::ProfileCleared { package } =>
                ("profile-cleared", package.clone()),
            RunnerEvent::OverlayPaused { package } =>
                ("overlay-paused", package.clone()),
            RunnerEvent::Grabbed => ("grabbed", String::new()),
            RunnerEvent::Ungrabbed => ("ungrabbed", String::new()),
            RunnerEvent::GameModeOn => ("game-mode-on", String::new()),
            RunnerEvent::GameModeOff => ("game-mode-off", String::new()),
            RunnerEvent::FocusGained => ("focus-gained", String::new()),
            RunnerEvent::FocusLost => ("focus-lost", String::new()),
            RunnerEvent::EscapeRequested => ("escape-requested", String::new()),
        };
        let em = iface.signal_emitter();
        if let Err(e) = Manager::keymapper_event(em, kind, &detail).await {
            tracing::debug!(error = %e, "could not emit keymapper event");
        }

        let now = km.state().await;
        let m = iface.get().await;
        if now.running != last.running { let _ = m.keymapper_running_changed(em).await; }
        if now.game_mode != last.game_mode { let _ = m.game_mode_changed(em).await; }
        if now.grabbed != last.grabbed { let _ = m.grabbed_changed(em).await; }
        if now.host_focused != last.host_focused {
            let _ = m.host_focused_changed(em).await;
        }
        if now.active_profile != last.active_profile {
            let _ = m.active_profile_changed(em).await;
        }
        if now.foreground != last.foreground {
            let _ = m.foreground_package_changed(em).await;
        }
        drop(m);
        last = now;
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
    // Capacity 64: bursts happen (profile change + grab + focus in one go)
    // and the sender drops rather than blocking, so a little slack keeps
    // status updates from being lost during normal transitions.
    let (ev_tx, ev_rx) = tokio::sync::mpsc::channel::<RunnerEvent>(64);
    let km = Arc::new(keymapper::Handle::new(ev_tx));
    let win = window::WindowState::new();
    let health = Arc::new(RwLock::new(Health::default()));

    let conn = connection::Builder::session()?
        .name(BUS_NAME)?
        .serve_at(OBJ_PATH, Manager {
            sup: sup.clone(), state: state.clone(),
            km: km.clone(), win: win.clone(), health: health.clone(),
        })?
        .build()
        .await?;

    // Push keymapper state outwards. Without this a UI has to poll
    // `KeymapperStatus`, and a game-mode toggle would show up whenever the
    // next poll happened to land rather than when it happened.
    let iface = conn.object_server()
        .interface::<_, Manager>(OBJ_PATH).await?;
    tokio::spawn(emit_keymapper(iface.clone(), km.clone(), ev_rx));
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
                // Serve the cached copy to clients and announce a change
                // only when it actually differs; a UI bound to this must
                // not redraw every five seconds for nothing.
                {
                    let mut slot = health.write().await;
                    if *slot != h {
                        *slot = h.clone();
                        drop(slot);
                        let m = iface.get().await;
                        let _ = m.health_json_changed(iface.signal_emitter()).await;
                    }
                }
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
                // Announce session-state transitions. The property was
                // already declared as emits-change but nothing ever emitted,
                // so a client saw the first value and never an update.
                {
                    let mut slot = state.write().await;
                    if *slot != next {
                        *slot = next;
                        drop(slot);
                        let m = iface.get().await;
                        let _ = m.state_changed(iface.signal_emitter()).await;
                    }
                }

                // Threshold: do not restart on a single dip.
                if cfg.auto_recover
                    && unhealthy >= cfg.unhealthy_threshold
                    && attempts < cfg.max_recovery_attempts
                {
                    attempts += 1;
                    tracing::error!(attempt = attempts, "starting recovery");
                    {
                        *state.write().await = SessionState::Recovering;
                        let m = iface.get().await;
                        let _ = m.state_changed(iface.signal_emitter()).await;
                    }
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
