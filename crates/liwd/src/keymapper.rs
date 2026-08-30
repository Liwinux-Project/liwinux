//! The keymapper task inside liwd.
//!
//! Wraps the Runner and provides foreground polling. Polling runs in a SEPARATE
//! TASK: `waydroid shell dumpsys` takes 100-200 ms and awaiting it in the input
//! loop pushed p99 latency to 212 ms (measured).

use anyhow::{Context, Result};
use liw_core::input::{Runner, RunnerConfig, RunnerEvent, RunnerState, ScreenMap, Store};
use liw_core::HelperClient;
use std::sync::Arc;
use tokio::sync::{mpsc, watch, Mutex};

const POLL_MS: u64 = 1000;

/// KWin class name of the Waydroid window. Measured with `find-waydroid.js`:
/// cls='waydroid'.
/// Window classes that mean "Android's screen has the focus".
///
/// There are two ways to be looking at Android. `waydroid` is its own window,
/// from `waydroid show-full-ui` — the path that existed before this program
/// had a window. `liwinux-game` is that window: Android is rendered inside it
/// through the embedded compositor, so focusing it IS focusing Android.
///
/// Measured, and the reason this list exists: with only `waydroid` here, KWin
/// reported `liwinux` the instant the game window took the focus, mapping was
/// switched off 40ms after it came on, and the grab was dropped. Game mode
/// looked on — the hotkey was seen and the UI said so — while no key reached
/// the game.
///
/// The launcher's own `liwinux` is deliberately absent: it shows a library,
/// not a game, and grabbing the keyboard there would stop the user typing.
const ANDROID_CLASSES: [&str; 2] = ["waydroid", "liwinux-game"];
/// Registration name of the KWin script; unloaded under the same name.
const KWIN_SCRIPT: &str = "liwinux-focus";

struct Running {
    shutdown: watch::Sender<bool>,
    focus: watch::Sender<bool>,
    state: Arc<tokio::sync::RwLock<RunnerState>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

pub struct Handle {
    inner: Mutex<Option<Running>>,
    /// Window size; handed to the engine as an aspect ratio at startup.
    screen_px: Mutex<(u32, u32)>,
    /// Where runner events are forwarded so they can leave the daemon.
    ///
    /// They used to end in the log only. A UI cannot poll fast enough to
    /// see a game-mode toggle, and polling `KeymapperStatus` at that rate
    /// would be wasteful — these events are exactly what a status bar
    /// needs, so they must be pushed.
    events: mpsc::Sender<RunnerEvent>,
}

impl Handle {
    pub fn new(events: mpsc::Sender<RunnerEvent>) -> Self {
        Self {
            inner: Mutex::new(None),
            screen_px: Mutex::new((2560, 1440)),
            events,
        }
    }

    /// Updated as the window geometry changes.
    pub async fn set_screen_px(&self, w: u32, h: u32) {
        if w > 0 && h > 0 { *self.screen_px.lock().await = (w, h); }
    }

    /// Acquires the touch pipe; WAITS a while if it is not ready.
    ///
    /// One attempt is not enough. hwcomposer only publishes
    /// `waydroid.display_width`
    /// on the first hotplug; right after a session start that takes a few
    /// seconds. This actually happened: after `liw session restart` the
    /// keymapper silently fell back to uinput every time and aim stayed in
    /// bounded mode — all the user saw was "it went back to how it was".
    async fn acquire_pipe(helper: &HelperClient)
        -> Option<(std::fs::File, (u32, u32))>
    {
        /// About 15 s in total. Waydroid's full startup is around there.
        const TRIES: u32 = 15;
        let mut last = String::new();
        for i in 0..TRIES {
            match helper.open_touch_pipe().await {
                Ok((f, w, h)) => {
                    tracing::info!(width = w, height = h, attempt = i + 1,
                        "touch pipe acquired — bypassing the compositor, \
                         unbounded aim enabled");
                    return Some((f, (w, h)));
                }
                Err(e) => {
                    last = e.to_string();
                    if i == 0 {
                        tracing::info!(error = %last,
                            "touch pipe not ready yet, waiting");
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
        }
        tracing::warn!(error = %last, attempts = TRIES,
            "could not acquire the touch pipe — falling back to uinput. \
             Aim will run in BOUNDED mode: the finger is reset at the screen \
             edge and fast turns lose rotation.");
        None
    }

    /// Focus notification coming from KWin.
    pub async fn set_active_window(&self, class: &str) {
        let guard = self.inner.lock().await;
        let Some(r) = guard.as_ref() else { return };
        let focused = ANDROID_CLASSES.iter().any(|c| class.eq_ignore_ascii_case(c));
        // Only send on change: if the watch channel re-emits the same value
        // the Runner does pointless work.
        if *r.focus.borrow() != focused {
            tracing::info!(window = class, waydroid_focused = focused, "focus changed");
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
            tracing::info!("keymapper already running");
            return Ok(());
        }

        let cfg = liw_core::Config::load();
        let devs = liw_core::input::discover();
        let device = cfg.keyboard.clone()
            .or_else(|| liw_core::input::capture::best_keyboard(&devs).map(|d| d.path.clone()))
            .context("no keyboard — calibrate with 'liw keymap detect --save'")?;

        let store = Store::discover();
        for p in &store.problems {
            tracing::warn!(file = %p.path.display(), error = %p.error, "could not load profile");
        }
        tracing::info!(
            keyboard = %device.display(),
            mouse = ?cfg.mouse.as_ref().map(|p| p.display().to_string()),
            profiles = store.len(), grab,
            hotkey = ?cfg.hotkey_game_mode,
            "starting keymapper");
        if grab && cfg.hotkey_game_mode.is_none() {
            tracing::warn!(
                "grab is on but no game-mode hotkey is set — devices will be \
                 grabbed as soon as a profile activates. Pick a key with \
                 'liw keymap detect --hotkey --save'.");
        }

        // The helper is MANDATORY for foreground detection and the touch pipe.
        let helper = HelperClient::connect().await
            .context("could not connect to liwd-helper — required for foreground detection")?;

        // Touch pipe: on success the compositor chain is bypassed and, because
        // coordinates are not clamped, unbounded aim kicks in. On failure we
        // fall back to uinput — but NOT SILENTLY: how aim feels depends
        // entirely on this and the user must know which path is active.
        // bilmeli (`docs/mouse-aim.md`).
        let pipe = Self::acquire_pipe(&helper).await;

        let mut runner = Runner::new(
            RunnerConfig {
                device, mouse: cfg.mouse.clone(), grab,
                hotkey: cfg.hotkey_game_mode,
                screen_map: ScreenMap::default(),
                screen_px: match pipe {
                    // On the pipe path the aspect ratio must come from the
                    // ANDROID display: that is where coordinates go, not the
                    // host window.
                    Some((_, px)) => px,
                    None => *self.screen_px.lock().await,
                },
            },
            store,
        );
        if let Some((f, px)) = pipe {
            runner = runner.with_touch_pipe(f, px);
        }

        // So the Runner can request a new pipe when it breaks (a display
        // hotplug deletes the FIFO). The task holding the channel owns the
        // helper; that is how the Runner is kept ignorant of D-Bus.
        let (pipe_tx, mut pipe_rx) =
            mpsc::channel::<liw_core::input::PipeRequest>(2);
        let helper_pipe = helper.clone();
        let pipe_task = tokio::spawn(async move {
            while let Some(reply) = pipe_rx.recv().await {
                let _ = reply.send(Self::acquire_pipe(&helper_pipe).await);
            }
        });
        runner = runner.with_pipe_provider(pipe_tx);

        let state = runner.state();

        // Focus is UNKNOWN at startup; we assume off until the KWin script
        // makes its first report. A false positive (thinking it is on while it
        // is off) means injecting touches into the desktop — the reverse only
        // costs latency.
        let (focus_tx, focus_rx) = watch::channel(false);
        let (fg_tx, fg_rx) = mpsc::channel::<String>(4);
        let (ev_tx, mut ev_rx) = mpsc::channel::<RunnerEvent>(16);
        let (sd_tx, sd_rx) = watch::channel(false);

        // Foreground polling — must NEVER block the input path.
        let poll = tokio::spawn(async move {
            let mut t = tokio::time::interval(std::time::Duration::from_millis(POLL_MS));
            loop {
                t.tick().await;
                match helper.foreground_package().await {
                    Ok(p) if !p.is_empty() => { let _ = fg_tx.try_send(p); }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "could not query foreground"),
                }
            }
        });

        let sink = self.events.clone();
        let log = tokio::spawn(async move {
            while let Some(e) = ev_rx.recv().await {
                let e2 = e.clone();
                match e {
                    RunnerEvent::ProfileActivated { package, profile } =>
                        tracing::info!(package = %package, %profile,
                                       "profile active"),
                    RunnerEvent::ProfileCleared { package } =>
                        tracing::info!(package = %package, "no profile — mapping off"),
                    RunnerEvent::OverlayPaused { package } => tracing::warn!(
                        package = %package,
                        "a system layer came over the game — mapping paused, \
                         mouse free"),
                    RunnerEvent::Grabbed => tracing::info!("device grabbed"),
                    RunnerEvent::Ungrabbed => tracing::info!("device grab released"),
                    RunnerEvent::GameModeOn =>
                        tracing::info!("game mode ON — grab + mapping"),
                    RunnerEvent::GameModeOff =>
                        tracing::info!("game mode off — mouse free"),
                    RunnerEvent::FocusGained =>
                        tracing::info!("Waydroid focused — mapping on"),
                    RunnerEvent::FocusLost =>
                        tracing::info!("Waydroid not focused — mapping off"),
                    RunnerEvent::EscapeRequested =>
                        tracing::info!("ESC ×3 — stopping the keymapper"),
                }
                // Forward outwards. `try_send` on purpose: a slow or absent
                // consumer must never stall the input path, and a dropped
                // status update is recoverable — the properties still carry
                // the current truth.
                if sink.try_send(e2).is_err() {
                    tracing::debug!("event sink full or closed — dropped");
                }
            }
        });

        let main = tokio::spawn(async move {
            match runner.run(fg_rx, focus_rx, sd_rx, Some(ev_tx)).await {
                Ok(lat) => tracing::info!("keymapper durdu — {}", lat.report("gecikme")),
                Err(e) => tracing::error!(error = %e, "the keymapper stopped with an error"),
            }
        });

        if let Err(e) = load_kwin_script().await {
            // Not fatal but DANGEROUS: without focus information mapping never
            // turns on (it starts at focus=false). Tell the user.
            tracing::error!(error = %e,
                "could not load the KWin focus script — mapping will not turn on");
        }

        *guard = Some(Running {
            shutdown: sd_tx, focus: focus_tx, state,
            tasks: vec![poll, log, main, pipe_task],
        });
        Ok(())
    }

    pub async fn stop(&self) {
        let mut guard = self.inner.lock().await;
        let Some(r) = guard.take() else { return };
        tracing::info!("keymapper durduruluyor");
        let _ = r.shutdown.send(true);
        // Give the Runner a chance to exit cleanly, lift fingers and release
        // the grab; only then cancel the tasks.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        for t in r.tasks { t.abort(); }
        if let Err(e) = unload_kwin_script().await {
            tracing::warn!(error = %e, "could not unload the KWin script");
        }
    }
}

/// Loads the focus-reporting script through KWin's scripting interface.
async fn load_kwin_script() -> Result<()> {
    let path = kwin_script_path().context("focus.js not found")?;
    let conn = zbus::Connection::session().await?;
    let p = zbus::Proxy::new(&conn, "org.kde.KWin", "/Scripting",
                             "org.kde.kwin.Scripting").await?;
    // Unload a leftover copy; two copies mean twice the notifications.
    let _: Result<bool, _> = p.call("unloadScript", &(KWIN_SCRIPT,)).await;
    let _id: i32 = p.call("loadScript", &(path.to_string_lossy().as_ref(), KWIN_SCRIPT))
        .await.context("loadScript failed")?;
    let _: () = p.call("start", &()).await.context("could not start script")?;
    tracing::info!(script = %path.display(), "KWin focus script loaded");
    Ok(())
}

async fn unload_kwin_script() -> Result<()> {
    let conn = zbus::Connection::session().await?;
    let p = zbus::Proxy::new(&conn, "org.kde.KWin", "/Scripting",
                             "org.kde.kwin.Scripting").await?;
    let _: bool = p.call("unloadScript", &(KWIN_SCRIPT,)).await?;
    Ok(())
}

/// Looks for focus.js: the installed location, then the repository next to the
/// executable. The working directory is NOT consulted (same lesson as the
/// profile store).
fn kwin_script_path() -> Option<std::path::PathBuf> {
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    // User installation first, so it can shadow the system package.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn is_android(class: &str) -> bool {
        ANDROID_CLASSES.iter().any(|c| class.eq_ignore_ascii_case(c))
    }

    /// Both ways of looking at Android count.
    #[test]
    fn android_has_the_focus_in_its_own_window_and_in_ours() {
        assert!(is_android("waydroid"));
        assert!(is_android("liwinux-game"));
    }

    /// The launcher must NOT. It is a library of games, and a grab there is a
    /// keyboard the user cannot type on.
    #[test]
    fn the_launcher_is_not_android() {
        assert!(!is_android("liwinux"));
    }

    /// KWin lowercases before reporting, but nothing guarantees it always
    /// will, and a case change must not silently turn mapping off.
    #[test]
    fn the_class_is_matched_regardless_of_case() {
        assert!(is_android("Waydroid"));
        assert!(is_android("liwinux-Game"));
    }

    /// No focused window at all reports an empty class.
    #[test]
    fn nothing_focused_is_not_android() {
        assert!(!is_android(""));
    }
}
