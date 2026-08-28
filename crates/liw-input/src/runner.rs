//! Keymapper run loop.
//!
//! Moved here so `liw keymap run` and `liwd` share one engine.
//!
//! # Independence
//!
//! The runner does **not query the foreground app itself**; it receives it on a
//! channel. The keymapper therefore knows nothing about Waydroid, D-Bus or
//! polkit — it only consumes "package X is in the foreground now". How the
//! caller finds that out is the caller's problem.

use crate::backend::TouchBackend;
use crate::capture::{translate, GrabbedDevice};
use crate::engine::{Engine, InputEvent, TriggerKind};
use crate::latency::LatencyStats;
use crate::store::Store;
use crate::uinput::{ScreenMap, UinputBackend};
use crate::wl_touch::WlTouchBackend;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

/// evdev key code: ESC.
const KEY_ESC: u16 = 1;
/// How many ESC presses are needed to escape a grab. Prevents accidental
/// exits from pressing ESC inside the game.
const ESC_STREAK: u8 = 3;

#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error("could not open device: {0}")]
    Device(#[from] crate::capture::CaptureError),
    #[error("could not set up the touch backend: {0}")]
    Backend(#[from] super::backend::BackendError),
    #[error("event stream broke: {0}")]
    Stream(#[source] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct RunnerConfig {
    /// Dinlenecek klavye.
    pub device: PathBuf,
    /// Mouse to listen on. Without it mouse mapping (Aim, buttons) does not work.
    pub mouse: Option<PathBuf>,
    /// Whether grabbing is enabled.
    ///
    /// When on, the grab is NOT taken automatically: the user enters game mode
    /// with `hotkey`. Grabbing as soon as a profile activates got people stuck
    /// in menus.
    pub grab: bool,
    /// evdev key code that toggles game mode.
    pub hotkey: Option<u16>,
    /// Touch coordinate mapping.
    pub screen_map: ScreenMap,
    /// Hedef pencerenin piksel boyutu.
    ///
    /// Whether the joystick circle is really a circle depends on this: a
    /// normalized coordinate is scaled per axis, so at 2560x1440 the same
    /// normalized radius reaches 1.78x further horizontally.
    pub screen_px: (u32, u32),
}

/// Externally observable state.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct RunnerState {
    pub running: bool,
    /// Foreground package (with or without a profile).
    pub foreground: Option<String>,
    /// Name of the active profile; `None` if there is none.
    pub active_profile: Option<String>,
    pub grabbed: bool,
    /// Is game mode on (grab + mapping)? With it off the mouse is free.
    pub game_mode: bool,
    /// Is the Waydroid window focused on the host? If not, mapping stops.
    pub host_focused: bool,
    /// Latency of our own layer (microseconds, p50/p99).
    pub latency_p50_us: u64,
    pub latency_p99_us: u64,
}

/// Events reported out of the loop (for logging / UI).
#[derive(Debug, Clone)]
pub enum RunnerEvent {
    ProfileActivated { package: String, profile: String },
    ProfileCleared { package: String },
    Grabbed,
    Ungrabbed,
    GameModeOn,
    GameModeOff,
    FocusGained,
    FocusLost,
    EscapeRequested,
    /// A system layer (assistant, notification panel) came over the game.
    ///
    /// Kept separate from `ProfileCleared` because the cause is entirely
    /// different: the game did not close, something came over it. The user
    /// needs to know why the mouse died.
    OverlayPaused { package: String },
}

/// Transient system layers that come OVER the game.
///
/// When these move to the foreground the game has not closed, it is merely
/// covered. We pause rather than clear the profile, so it returns instantly
/// when the layer goes away.
///
/// The list is kept narrow: things that genuinely wait for user input, such as
/// permission dialogs, MUST NOT be in it.
pub fn is_system_overlay(pkg: &str) -> bool {
    matches!(pkg,
        // Google Assistant / search — opens by itself during play.
        "com.google.android.googlequicksearchbox"
        // Notification shade, recents, volume control.
        | "com.android.systemui")
}

/// A request for a new touch pipe, carrying its reply channel.
///
/// Keeping the Runner ignorant of D-Bus is deliberate: who opens the pipe and
/// with what authority is the caller's problem. The Runner only says "give me
/// diyebiliyor.
pub type PipeRequest =
    tokio::sync::oneshot::Sender<Option<(std::fs::File, (u32, u32))>>;

/// Does a dispatch error mean the pipe is DEAD?
///
/// The distinction is mandatory: a full pipe (`WouldBlock`) is transient and
/// already swallowed by the backend; a broken pipe is permanent and needs
/// reopening. Conflating them means either needless reopening or silent death.
fn pipe_is_dead(e: &super::backend::BackendError) -> bool {
    let s = e.to_string();
    s.contains("Broken pipe") || s.contains("os error 32")
        || s.contains("alignment broken")
}

pub struct Runner {
    cfg: RunnerConfig,
    store: Store,
    state: Arc<RwLock<RunnerState>>,
    /// Waydroid's touch pipe and the Android display size.
    ///
    /// Verilirse compositor zinciri (uinput → libinput → KWin → wl_touch)
    /// entirely and off-screen coordinates become possible — the prerequisite
    /// for unbounded aim. NOT inside `RunnerConfig` because `File` is not
    /// cloneable and the configuration must be.
    touch_pipe: Option<(std::fs::File, (u32, u32))>,
    /// For requesting a new one when the pipe breaks.
    pipe_provider: Option<mpsc::Sender<PipeRequest>>,
}

impl Runner {
    pub fn new(cfg: RunnerConfig, store: Store) -> Self {
        Self {
            cfg, store,
            state: Arc::new(RwLock::new(RunnerState::default())),
            touch_pipe: None,
            pipe_provider: None,
        }
    }

    /// Channel used to request a new pipe when this one breaks.
    ///
    /// This actually happened: on a display hotplug hwcomposer deletes and
    /// recreates the FIFO (`EventHub: Removing device
    /// '/dev/input/wl_touch_events'`). The handle we hold is orphaned and
    /// injection stops SILENTLY — the user experiences it as "the mouse
    /// suddenly stopped working".
    pub fn with_pipe_provider(mut self, tx: mpsc::Sender<PipeRequest>) -> Self {
        self.pipe_provider = Some(tx);
        self
    }

    /// Use Waydroid's touch pipe (bypass the compositor).
    ///
    /// `px` Android ekran boyutudur (`waydroid.display_width`/`height`),
    /// not the host window's: pipe coordinates are directly in Android screen
    /// space and window geometry is not taken into account.
    pub fn with_touch_pipe(mut self, pipe: std::fs::File, px: (u32, u32)) -> Self {
        self.touch_pipe = Some((pipe, px));
        self
    }

    pub fn state(&self) -> Arc<RwLock<RunnerState>> { self.state.clone() }
    pub fn store(&self) -> &Store { &self.store }

    /// Runs the loop. Exits cleanly when the `foreground` channel closes or
    /// `shutdown` fires.
    ///
    /// Fingers are ALWAYS lifted and the grab released on exit — even on a
    /// crash the kernel releases it on fd close, but on a clean exit
    /// there is no need to wait.
    /// `host_focused`: is the Waydroid window focused on the host?
    ///
    /// This gate is MANDATORY: Android does not know the window was minimised
    /// and thinks it is foreground even with the game in the background.
    /// Without the gate, touches land on the user's real desktop.
    pub async fn run(
        &mut self,
        mut foreground: mpsc::Receiver<String>,
        mut host_focused: tokio::sync::watch::Receiver<bool>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
        events: Option<mpsc::Sender<RunnerEvent>>,
    ) -> Result<LatencyStats, RunnerError> {
        // The backend choice determines the ENTIRE input path.
        //
        // With the touch pipe the compositor chain is out of the picture and
        // coordinates are not clamped; only then is unbounded aim safe. On the
        // uinput path libinput squeezes to the screen, so the engine must stay
        // in bounded mode (`docs/mouse-aim.md`).
        let (mut backend, offscreen_ok): (Box<dyn TouchBackend>, bool) =
            match self.touch_pipe.take() {
                Some((pipe, (w, h))) => {
                    tracing::info!(width = w, height = h,
                        "using the touch pipe — bypassing the compositor");
                    (Box::new(WlTouchBackend::from_pipe(pipe, w, h)?), true)
                }
                None => {
                    let b = UinputBackend::new(self.cfg.screen_map)?;
                    // libinput/KWin may take a few hundred ms to notice the
                    // device. On the pipe path there is no device to notice.
                    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                    (Box::new(b), false)
                }
            };

        // Fail LOUDLY when the wrong device is opened. Not verifying meant
        // waiting for keys from an audio-jack device and silently never
        // working — all the user saw was "the keymapper does not work".
        {
            let probe = evdev::Device::open(&self.cfg.device)
                .map_err(|e| crate::capture::CaptureError::Open {
                    path: self.cfg.device.clone(), source: e })?;
            crate::capture::verify_kind(&probe, false)?;
        }
        let dev = GrabbedDevice::open(&self.cfg.device, false)?;
        let mut stream = dev.into_stream()?;

        // The mouse is a SEPARATE device. It does not arrive on the keyboard's
        // stream; we must listen to both or Aim and mouse buttons never fire.
        let mut mouse_stream = match &self.cfg.mouse {
            Some(p) => match evdev::Device::open(p)
                .map_err(|e| crate::capture::CaptureError::Open {
                    path: p.clone(), source: e })
                .and_then(|d| crate::capture::verify_kind(&d, true))
                .and_then(|()| GrabbedDevice::open(p, false))
            {
                Ok(d) => match d.into_stream() {
                    Ok(s) => Some(s),
                    Err(e) => { tracing::warn!(error = %e, "could not set up the mouse stream"); None }
                },
                Err(e) => { tracing::warn!(error = %e, "could not open the mouse"); None }
            },
            None => None,
        };

        let mut engine: Option<Engine> = None;
        // Etkin profil, motordan AYRI tutulur: odak kaybolunca motoru
        // tear the engine down on focus loss but must not forget the profile,
        // so it can be rebuilt when focus returns.
        let mut profile: Option<crate::profile::Profile> = None;
        let mut focused = *host_focused.borrow();
        // With a hotkey set, game mode starts OFF and the user turns it on when
        // ready. Without one, the old behaviour: on whenever a profile is active.
        let mut game_mode = self.cfg.hotkey.is_none();
        let mut current: Option<String> = None;
        let mut grabbed = false;
        let mut esc = 0u8;
        let mut lat = LatencyStats::new();
        // Is the pipe broken? While it is, dispatching is pointless.
        let mut pipe_dead = false;
        // Dispatch wrapper: CLASSIFIES the error instead of swallowing it.
        // Every call used to be discarded with `let _ =`; when the pipe broke
        // nothing was left anywhere and injection died silently.
        macro_rules! emit_touch {
            ($acts:expr) => {{
                let acts = $acts;
                if acts.is_empty() { true } else {
                    match backend.dispatch(&acts) {
                        Ok(()) => true,
                        Err(e) => {
                            if pipe_is_dead(&e) {
                                pipe_dead = true;
                                tracing::error!(hata = %e,
                                    "touch pipe broke — a new one will be requested");
                            } else {
                                tracing::warn!(error = %e, "could not dispatch touch");
                            }
                            false
                        }
                    }
                }
            }};
        }

        let t0 = std::time::Instant::now();
        // To send one touch move per frame: real touchscreens report at
        // 60-240 Hz, not at a 1000 Hz mouse rate. 5 ms ~ 200 Hz.
        let mut repair_tick = tokio::time::interval(std::time::Duration::from_secs(2));
        repair_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(5));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        {
            let mut s = self.state.write().await;
            s.running = true;
        }
        let emit = |e: RunnerEvent| {
            if let Some(tx) = &events { let _ = tx.try_send(e); }
        };

        loop {
            tokio::select! {
                // --- foreground change ---
                Some(pkg) = foreground.recv() => {
                    if current.as_deref() == Some(pkg.as_str()) { continue; }

                    // System layer: the game did not close, it got covered.
                    // KEEP the profile so it returns instantly when the layer
                    // goes away. Release the grab so the user can dismiss the
                    // layer with the mouse — otherwise it stays locked.
                    if is_system_overlay(&pkg) && profile.is_some() {
                        if let Some(e) = engine.as_mut() {
                            let acts = e.set_enabled(false);
                            let _ = emit_touch!(acts);
                        }
                        let _ = backend.release_all();
                        engine = None;
                        if grabbed {
                            let _ = stream.device_mut().ungrab();
                            if let Some(m) = mouse_stream.as_mut() {
                                let _ = m.device_mut().ungrab();
                            }
                            grabbed = false;
                            emit(RunnerEvent::Ungrabbed);
                        }
                        emit(RunnerEvent::OverlayPaused { package: pkg.clone() });
                        let mut s = self.state.write().await;
                        s.foreground = Some(pkg);
                        s.grabbed = false;
                        continue;
                    }

                    // Close the old profile and release fingers: no finger may
                    // be left on screen while the app changes.
                    if let Some(e) = engine.as_mut() {
                        let acts = e.set_enabled(false);
                        let _ = emit_touch!(acts);
                    }
                    let _ = backend.release_all();

                    match self.store.for_package(&pkg) {
                        Some(entry) => {
                            profile = Some(entry.profile.clone());
                            emit(RunnerEvent::ProfileActivated {
                                package: pkg.clone(),
                                profile: entry.profile.name.clone(),
                            });
                            // The engine is only built while the host is focused.
                            if focused && game_mode {
                                engine = Some({
                                    let mut e = Engine::new(entry.profile.clone());
                                    e.set_aspect(self.cfg.screen_px.0, self.cfg.screen_px.1);
                                    e.set_offscreen_ok(offscreen_ok);
                                    e
                                });
                                if self.cfg.grab && !grabbed
                                    && stream.device_mut().grab().is_ok()
                                {
                                    // The mouse must be grabbed too: otherwise the
                                    // cursor roams the host and clicks hit the desktop.
                                    if let Some(m) = mouse_stream.as_mut() {
                                        let _ = m.device_mut().grab();
                                    }
                                    grabbed = true;
                                    emit(RunnerEvent::Grabbed);
                                }
                            }
                            let mut s = self.state.write().await;
                            s.active_profile = Some(entry.profile.name.clone());
                        }
                        None => {
                            profile = None;
                            engine = None;
                            emit(RunnerEvent::ProfileCleared { package: pkg.clone() });
                            if grabbed {
                                let _ = stream.device_mut().ungrab();
                                if let Some(m) = mouse_stream.as_mut() {
                                    let _ = m.device_mut().ungrab();
                                }
                                grabbed = false;
                                emit(RunnerEvent::Ungrabbed);
                            }
                            let mut s = self.state.write().await;
                            s.active_profile = None;
                        }
                    }
                    current = Some(pkg.clone());
                    let mut s = self.state.write().await;
                    s.foreground = Some(pkg);
                    s.grabbed = grabbed;
                }

                // --- host focus change ---
                _ = host_focused.changed() => {
                    let now_focused = *host_focused.borrow();
                    if now_focused == focused { continue; }
                    focused = now_focused;
                    if focused {
                        if let Some(p) = &profile.clone().filter(|_| game_mode) {
                            engine = Some({
                                let mut e = Engine::new(p.clone());
                                e.set_aspect(self.cfg.screen_px.0, self.cfg.screen_px.1);
                                e.set_offscreen_ok(offscreen_ok);
                                e
                            });
                            if self.cfg.grab && !grabbed
                                && stream.device_mut().grab().is_ok()
                            {
                                if let Some(m) = mouse_stream.as_mut() {
                                    let _ = m.device_mut().grab();
                                }
                                grabbed = true;
                                emit(RunnerEvent::Grabbed);
                            }
                        }
                        emit(RunnerEvent::FocusGained);
                    } else {
                        // On focus loss RELEASE the fingers and the grab:
                        // otherwise keys become touches on the user's desktop
                        // enjekte etmeye devam eder.
                        if let Some(e) = engine.as_mut() {
                            let acts = e.set_enabled(false);
                            let _ = emit_touch!(acts);
                        }
                        engine = None;
                        let _ = backend.release_all();
                        if grabbed {
                            let _ = stream.device_mut().ungrab();
                            if let Some(m) = mouse_stream.as_mut() {
                                let _ = m.device_mut().ungrab();
                            }
                            grabbed = false;
                            emit(RunnerEvent::Ungrabbed);
                        }
                        emit(RunnerEvent::FocusLost);
                    }
                    let mut s = self.state.write().await;
                    s.host_focused = focused;
                    s.grabbed = grabbed;
                }

                // --- renew a broken pipe ---
                //
                // On a separate, slower clock: tying it to the gesture clock
                // meant it never ran when there was no gesture — exactly when
                // injection had stopped.
                _ = repair_tick.tick(), if pipe_dead => {
                    let Some(tx) = self.pipe_provider.clone() else {
                        tracing::error!("the pipe broke and there is no channel to renew it \
                                         — the keymapper must be restarted");
                        pipe_dead = false;
                        continue;
                    };
                    let (rtx, rrx) = tokio::sync::oneshot::channel();
                    if tx.send(rtx).await.is_err() { continue; }
                    match rrx.await {
                        Ok(Some((f, (w, h)))) => {
                            match WlTouchBackend::from_pipe(f, w, h) {
                                Ok(b) => {
                                    backend = Box::new(b);
                                    pipe_dead = false;
                                    // The engine's finger state is now INVALID:
                                    // when Android removed the device it forgot
                                    // every touch of ours. Without starting
                                    // fresh the pool leaks and aim dies.
                                    if let Some(e) = engine.as_mut() {
                                        let _ = e.set_enabled(false);
                                        let _ = e.set_enabled(true);
                                    }
                                    tracing::info!(width = w, height = h,
                                        "touch pipe renewed");
                                }
                                Err(e) => tracing::error!(hata = %e,
                                    "could not set up the new pipe"),
                            }
                        }
                        Ok(None) => tracing::warn!("pipe not available yet, \
                                                    tekrar denenecek"),
                        Err(_) => {}
                    }
                }

                // --- jest saati ---
                _ = ticker.tick(), if engine.as_ref().is_some_and(|e| e.has_pending()) => {
                    if let Some(e) = engine.as_mut() {
                        let acts = e.tick(t0.elapsed().as_millis() as u64);
                        if !acts.is_empty() { let _ = emit_touch!(acts); }
                    }
                }

                // --- girdi ---
                ev = stream.next_event() => {
                    let ev = ev.map_err(RunnerError::Stream)?;
                    let ev_time = ev.timestamp();
                    let Some(input) = translate(&ev) else { continue };

                    // The hotkey is ALWAYS listened for — grabbed or not.
                    // Not listening while grabbed would trap the user in game mode.
                    if let (Some(hk), InputEvent::Press(TriggerKind::Key(k))) =
                        (self.cfg.hotkey, input)
                    {
                        if k == hk {
                            game_mode = !game_mode;
                            if game_mode {
                                if let Some(p) = &profile {
                                    if focused {
                                        engine = Some({
                                            let mut e = Engine::new(p.clone());
                                            e.set_aspect(
                                                self.cfg.screen_px.0, self.cfg.screen_px.1);
                                            e.set_offscreen_ok(offscreen_ok);
                                            e
                                        });
                                        if self.cfg.grab && !grabbed
                                            && stream.device_mut().grab().is_ok()
                                        {
                                            if let Some(m) = mouse_stream.as_mut() {
                                                let _ = m.device_mut().grab();
                                            }
                                            grabbed = true;
                                            emit(RunnerEvent::Grabbed);
                                        }
                                    }
                                }
                                emit(RunnerEvent::GameModeOn);
                            } else {
                                // RELEASE fingers when leaving the mode:
                                // otherwise they hang on screen back in the menu.
                                if let Some(e) = engine.as_mut() {
                                    let acts = e.set_enabled(false);
                                    let _ = emit_touch!(acts);
                                }
                                engine = None;
                                let _ = backend.release_all();
                                if grabbed {
                                    let _ = stream.device_mut().ungrab();
                                    if let Some(m) = mouse_stream.as_mut() {
                                        let _ = m.device_mut().ungrab();
                                    }
                                    grabbed = false;
                                    emit(RunnerEvent::Ungrabbed);
                                }
                                emit(RunnerEvent::GameModeOff);
                            }
                            let mut s = self.state.write().await;
                            s.game_mode = game_mode;
                            s.grabbed = grabbed;
                            continue;
                        }
                    }

                    if grabbed {
                        match input {
                            InputEvent::Press(TriggerKind::Key(KEY_ESC)) => {
                                esc += 1;
                                if esc >= ESC_STREAK {
                                    emit(RunnerEvent::EscapeRequested);
                                    break;
                                }
                                continue;
                            }
                            InputEvent::Press(_) => esc = 0,
                            _ => {}
                        }
                    }

                    if let Some(e) = engine.as_mut() {
                        // tick actions MUST NOT be dropped: the previous gesture's UP may be there.
                        let mut acts = e.tick(t0.elapsed().as_millis() as u64);
                        acts.extend(e.handle(input));
                        if !acts.is_empty() && emit_touch!(acts) {
                            lat.record(ev_time);
                        }
                    }
                }

                // --- fare girdisi ---
                ev = async {
                    match mouse_stream.as_mut() {
                        Some(m) => m.next_event().await,
                        // With no mouse this arm must never be ready.
                        None => std::future::pending().await,
                    }
                } => {
                    let ev = ev.map_err(RunnerError::Stream)?;
                    let ev_time = ev.timestamp();
                    if let (Some(input), Some(e)) = (translate(&ev), engine.as_mut()) {
                        // Mouse motion ACCUMULATES in the engine; applied on tick.
                        // Key events (mouse buttons) are handled immediately.
                        let acts = e.handle(input);
                        if !acts.is_empty() && emit_touch!(acts) {
                            lat.record(ev_time);
                        }
                    }
                }

                _ = shutdown.changed() => {
                    if *shutdown.borrow() { break; }
                }
            }
        }

        // Clean exit.
        //
        // `emit_touch!` is NOT used here: on the way out there is no value in
        // learning whether the pipe is dead, and classifying would only produce
        // an "assignment never read" warning.
        if let Some(e) = engine.as_mut() {
            let acts = e.set_enabled(false);
            if !acts.is_empty() { let _ = backend.dispatch(&acts); }
        }
        let _ = backend.release_all();
        if grabbed {
            let _ = stream.device_mut().ungrab();
            if let Some(m) = mouse_stream.as_mut() { let _ = m.device_mut().ungrab(); }
        }

        let (p50, _, p99, _) = lat.percentiles();
        let mut s = self.state.write().await;
        s.running = false;
        s.grabbed = false;
        s.active_profile = None;
        s.latency_p50_us = p50;
        s.latency_p99_us = p99;
        Ok(lat)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A broken pipe and a FULL pipe must be told apart.
    ///
    /// A full pipe is transient and already swallowed by the backend; counting
    /// it as "broken" would force a pointless reopen under every load spike.
    /// The reverse is worse: ignoring a real break killed injection silently
    /// (measured on a display hotplug).
    #[test]
    fn only_a_real_break_counts_as_dead() {
        use crate::backend::BackendError;
        let dead = BackendError::Dispatch(
            "could not write to pipe: Broken pipe (os error 32)".into());
        assert!(pipe_is_dead(&dead));
        let misaligned = BackendError::Dispatch(
            "partial write: 12/24 bytes — pipe alignment broken".into());
        assert!(pipe_is_dead(&misaligned), "a misaligned pipe is dead too");
        let busy = BackendError::Dispatch("invalid pointer id 12".into());
        assert!(!pipe_is_dead(&busy));
        // A full pipe returns Ok(()) in the backend; it never reaches here.
    }

    #[test]
    fn default_state_is_idle() {
        let s = RunnerState::default();
        assert!(!s.running);
        assert!(s.active_profile.is_none());
        assert!(!s.grabbed);
        assert!(!s.game_mode);
        // Focus defaults to OFF: assuming on while unknown risks injecting
        // touches into the desktop.
        assert!(!s.host_focused);
    }

    /// The escape threshold must not be 1: pressing ESC in-game must not exit.
    #[test]
    fn escape_requires_repeated_presses() {
        assert!(ESC_STREAK > 1);
    }

    /// The profile must NOT drop when the assistant comes over the game.
    ///
    /// This actually happened: the assistant opened by itself, the foreground
    /// package changed, the keymapper shut down and the mouse died silently.
    #[test]
    fn assistant_and_systemui_are_overlays() {
        assert!(is_system_overlay("com.google.android.googlequicksearchbox"));
        assert!(is_system_overlay("com.android.systemui"));
    }

    /// Real apps must not count as layers — otherwise mapping stays on after
    /// leaving the game and keys leak to the desktop.
    #[test]
    fn real_apps_are_not_overlays() {
        for p in ["com.ForgeGames.SpecialForcesGroup2", "com.android.settings",
                  "com.android.permissioncontroller", "com.android.vending"] {
            assert!(!is_system_overlay(p), "{p} must not count as a layer");
        }
    }

    #[test]
    fn state_is_serialisable_for_dbus() {
        let s = RunnerState { running: true, foreground: Some("com.x".into()),
            active_profile: Some("P".into()), grabbed: true, host_focused: true,
            game_mode: true, latency_p50_us: 80, latency_p99_us: 170 };
        let j = serde_json::to_string(&s).unwrap();
        let back: RunnerState = serde_json::from_str(&j).unwrap();
        assert_eq!(back.active_profile.as_deref(), Some("P"));
        assert_eq!(back.latency_p99_us, 170);
    }
}
