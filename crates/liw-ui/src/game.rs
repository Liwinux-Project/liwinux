//! The game window.
//!
//! A window of its own, the way GameLoop does it: the launcher stays a
//! launcher and the game gets its own frame, its own size and its own place on
//! the taskbar. Putting the game inside the launcher meant one of them always
//! had to be the wrong shape.
//!
//! It holds everything that belongs to a running game — the compositor
//! Waydroid draws into, the rail beside the picture, and the control editor —
//! and nothing that does not.

use gpui::{
    div, prelude::*, px, App, Context, FocusHandle, IntoElement, Render,
    SharedString, Window,
};

use crate::android::Android;
use crate::mapper::{Control, Kind, Mapper, NUDGE};
use crate::theme::{Theme, RADIUS, S1, S2, S3};

/// Width of the strip that is always there.
pub const RAIL_W: f32 = 44.0;
/// Width of the panel when it is open.
pub const PANEL_W: f32 = 248.0;

/// How much horizontal room the sidebar takes right now.
///
/// The picture's size is computed from this, so it lives in one place: a
/// second opinion about the rail's width would put every control a few pixels
/// from where it was clicked.
pub fn sidebar_width(open: bool) -> f32 {
    if open { RAIL_W + PANEL_W } else { RAIL_W }
}

pub struct GameView {
    pub android: Android,
    pub mapper: Mapper,
    pub sidebar_open: bool,
    /// The game window takes key events for the editor: arrows nudge, other
    /// keys bind. It has to hold focus for any of that to arrive.
    pub focus: FocusHandle,
    /// The package this window is showing, for the profile being edited.
    pub package: String,
    pub title: String,
    /// The game's own icon, drawn in the panel.
    ///
    /// Wayland has no per-window icon, so this is where the game's face
    /// actually appears — inside the window rather than on its frame.
    pub icon: Option<std::path::PathBuf>,
    /// Mouse presses, sent into Android as touches.
    pub touch: crate::touch::Touch,
    frame_pump: Option<gpui::Task<()>>,
    _pipe: Option<gpui::Task<()>>,
    /// The title has been put on the window.
    ///
    /// `TitlebarOptions.title` is not applied on Wayland in this gpui
    /// revision, so the name has to be set explicitly — otherwise the task
    /// switcher shows whatever the toolkit defaulted to.
    titled: bool,
}

impl GameView {
    pub fn new(
        package: String,
        title: String,
        icon: Option<std::path::PathBuf>,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut v = Self {
            android: Android::default(),
            mapper: Mapper::default(),
            sidebar_open: true,
            focus: cx.focus_handle(),
            package,
            title,
            icon,
            touch: Default::default(),
            frame_pump: None,
            _pipe: None,
            titled: false,
        };
        v.android.start(1280, 720);
        v.pump(cx);
        v.open_touch_pipe(cx);
        v.bring_up_android(cx);
        v.stop_android_on_close(cx);
        v.report_focus(window, cx);
        v
    }

    /// Tells the daemon when this window has the focus.
    ///
    /// The key mapper only maps while Android's screen is focused, and it
    /// learns that from the focused window's class. A KWin script reports
    /// those, which leaves two holes this closes:
    ///
    /// * The script samples focus when it LOADS and on changes after that.
    ///   Measured: this window already had the focus when the mapper started,
    ///   nothing changed afterwards, so nothing was ever reported. Game mode
    ///   turned on, said so, and mapped nothing.
    /// * The script is KWin's. On any other compositor there is no report at
    ///   all.
    ///
    /// This window knows the answer without asking anyone, so it says so —
    /// once at once, and on every change. It reports the same class KWin
    /// would, so the two agree rather than fight.
    fn report_focus(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        report(window.is_window_active(), cx);
        cx.observe_window_activation(window, |_v, window, cx| {
            report(window.is_window_active(), cx);
        })
        .detach();
    }

    /// Closing the window closes the game.
    ///
    /// The whole session goes, not just the package. Two reasons, and the
    /// second is the one that decides it:
    ///
    /// * `waydroid app` has no stop verb. Killing one package means
    ///   `am force-stop` through a root shell, which would mean a new
    ///   privileged method and a new polkit action for something the user can
    ///   already do by closing a window.
    /// * This window WAS Android's screen. With it gone the session is
    ///   drawing into a socket nobody is listening on — the same state the
    ///   embedded display is checked for before every start, because Android
    ///   boots happily into nothing and looks fine while doing it.
    ///
    /// It costs nothing either: the next game window would have had to stop
    /// and restart this session anyway to move it onto its own socket.
    fn stop_android_on_close(&mut self, cx: &mut Context<Self>) {
        cx.on_release(|v, cx| {
            // If the stop below fails — the daemon is gone, the call times out
            // — the session outlives the window. A finger left down in it is
            // held forever with nothing able to lift it.
            v.touch.release_all();
            // The compositor thread owns the listening socket, and it outlives
            // the window unless it is told not to. Measured: after closing one
            // game the socket file was still bound, so the next game window
            // could not take the name and never got a picture. Stopping is a
            // flag the thread reads every 4ms, so this returns at once.
            if let Some(e) = v.android.embedded.take() {
                e.stop();
            }
            let task = gpui_tokio::Tokio::spawn(cx, async move {
                let Ok(m) = liw_core::manager::Manager::connect().await else { return };
                // Clear the display FIRST. If the stop is what fails, the
                // name of a socket that no longer exists must not be left
                // behind for the next session to be started against.
                let _ = m.set_embedded_display("").await;
                let _ = m.session_stop().await;
                tracing::info!("game window closed — Android stopped");
            });
            task.detach();
        })
        .detach();
    }

    /// Points the daemon at this window's socket and starts Android on it.
    ///
    /// The order is the whole trick: the socket exists as soon as the window
    /// does, the daemon is told about it, and only then is the session
    /// started. Starting first would send Android to the desktop compositor,
    /// and telling the daemon afterwards would not move a session already
    /// running.
    ///
    /// Telling the DAEMON rather than setting an environment variable is what
    /// makes it survive: liwd restarts the session when it looks unhealthy,
    /// and a restart that forgot the socket would take the game out of this
    /// window without explanation.
    fn bring_up_android(&mut self, cx: &mut Context<Self>) {
        let package = self.package.clone();
        let task = gpui_tokio::Tokio::spawn(cx, async move {
            let m = liw_core::manager::Manager::connect().await
                .map_err(|e| e.to_string())?;
            m.set_embedded_display(crate::android::SOCKET).await
                .map_err(|e| e.to_string())?;

            // Waydroid reads WAYLAND_DISPLAY once, at session start, so a
            // session already attached elsewhere has to be restarted to move.
            let running = m.snapshot().await
                .map(|s| s.state == "RUNNING")
                .unwrap_or(false);
            tracing::debug!(running, "bring-up: session state read");
            if running {
                m.session_stop().await.map_err(|e| e.to_string())?;
                // Waydroid needs the old session to actually be gone before a
                // new one will bind; starting into the tail of a stop leaves
                // the container half up and the socket unused.
                for _ in 0..40 {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    if !m.snapshot().await.map(|s| s.state == "RUNNING").unwrap_or(true) {
                        break;
                    }
                }
            }
            m.session_start().await.map_err(|e| e.to_string())?;
            tracing::debug!("bring-up: session started");

            // Wait for Android before launching. This is the ordering that
            // was wrong: the launcher used to fire the launch at the same
            // time as the window opened, so the app went into whatever
            // session was already running — which is the OLD one, on the
            // desktop's socket. The game then appeared in a window of
            // Waydroid's own while this one stayed empty.
            let mut booted = false;
            for i in 0..90 {
                tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
                match m.snapshot().await {
                    Ok(s) if s.health.boot_completed => { booted = true; break; }
                    Ok(s) if i % 10 == 0 => tracing::debug!(
                        state = %s.state, booted = s.health.boot_completed,
                        "bring-up: waiting for Android"),
                    Err(e) if i % 10 == 0 => tracing::warn!(
                        error = %e, "bring-up: cannot read the daemon"),
                    _ => {}
                }
            }
            if !booted {
                return Err("Android did not finish booting".to_string());
            }
            tracing::debug!("bring-up: booted, launching the game");
            let r = m.launch(&package).await.map_err(|e| e.to_string());
            tracing::info!(ok = r.is_ok(), package = %package, "game launched");
            r
        });
        cx.spawn(async move |this, cx| {
            let r = match task.await {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(e),
                Err(e) => Some(e.to_string()),
            };
            if let Some(e) = r {
                let _ = this.update(cx, |v, cx| {
                    v.android.error = Some(format!("could not start Android: {e}"));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Asks the daemon's helper for the touch pipe.
    ///
    /// Opening it needs root — the FIFO lives inside the container's mount
    /// namespace — so the helper opens it and passes the descriptor over
    /// D-Bus. Retried until the session is up, because the window is normally
    /// open before Android has finished booting.
    fn open_touch_pipe(&mut self, cx: &mut Context<Self>) {
        self._pipe = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(2))
                    .await;

                // Only open when there is nothing usable. The pipe dies with
                // the container and the window restarts the container itself,
                // so this has to keep watch rather than succeed once and stop.
                match this.update(cx, |v, _| v.touch.is_ready()) {
                    Ok(true) => continue,
                    Ok(false) => {}
                    Err(_) => return, // the window is gone
                }

                let task = gpui_tokio::Tokio::spawn(cx, async move {
                    let h = liw_core::helper::HelperClient::connect().await.ok()?;
                    h.open_touch_pipe().await.ok()
                });
                let Ok(Some((pipe, w, h))) = task.await else { continue };
                if this.update(cx, |v, cx| { v.touch.attach(pipe, w, h); cx.notify(); }).is_err() {
                    return;
                }
            }
        }));
    }

    /// Repaints while frames are arriving.
    ///
    /// gpui redraws when something tells it to, and a frame produced on the
    /// compositor thread is not something it can see. This is that signal.
    fn pump(&mut self, cx: &mut Context<Self>) {
        if self.frame_pump.is_some() {
            return;
        }
        self.frame_pump = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(POLL).await;
            let keep = this.update(cx, |v, cx| {
                if !v.android.running() {
                    v.frame_pump = None;
                    return false;
                }
                if v.android.poll() {
                    cx.notify();
                }
                true
            });
            if !matches!(keep, Ok(true)) {
                break;
            }
        }));
    }

    fn toggle_mapping(&mut self, cx: &mut Context<Self>) {
        if self.mapper.is_on() {
            self.mapper.end();
        } else {
            self.mapper.begin(&self.package, &self.title);
            self.sidebar_open = true;
        }
        cx.notify();
    }

    /// Keys while the editor is open.
    ///
    /// Arrows nudge the selection, `[` and `]` size it, Delete removes it, and
    /// anything else becomes the binding. The arrows are deliberately not
    /// bindable for that reason: they are how a control is put exactly on a
    /// game button, and a click is only as accurate as the pointer.
    fn on_key(&mut self, ev: &gpui::KeyDownEvent, cx: &mut Context<Self>) {
        if !self.mapper.is_on() {
            return;
        }
        let k = ev.keystroke.key.as_str();
        let handled = match k {
            "up" => self.mapper.nudge(0.0, -NUDGE),
            "down" => self.mapper.nudge(0.0, NUDGE),
            "left" => self.mapper.nudge(-NUDGE, 0.0),
            "right" => self.mapper.nudge(NUDGE, 0.0),
            "[" | "leftbracket" => self.mapper.resize(-0.01),
            "]" | "rightbracket" => self.mapper.resize(0.01),
            "delete" | "backspace" => {
                self.mapper.remove_selected();
                true
            }
            "escape" => {
                self.mapper.selected = None;
                self.mapper.binding = false;
                true
            }
            other => self.mapper.take_key(other),
        };
        if handled {
            cx.notify();
        }
    }
}

impl Render for GameView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::dark();

        if !self.titled {
            window.set_window_title(&self.title);
            self.titled = true;
        }

        // The picture is the window minus the rail. One piece of arithmetic
        // decides what the compositor renders AND where a click lands.
        let win = window.viewport_size();
        let rail = if self.android.immersive { 0.0 } else { sidebar_width(self.sidebar_open) };
        let view = (f32::from(win.width) - rail, f32::from(win.height));
        self.android.resize(view.0, view.1);

        let guest = self
            .android
            .guest_size()
            .unwrap_or((view.0 as i32, view.1 as i32));
        let (_, shown) = liw_compositor::fit(guest, (view.0 as i32, view.1 as i32));

        div()
            .key_context("Game")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|v, ev: &gpui::KeyDownEvent, _, cx| v.on_key(ev, cx)))
            .flex()
            .flex_row()
            .size_full()
            .bg(t.bg)
            .text_color(t.text)
            .font_family("sans-serif")
            .child(div().flex_1().h_full().child(picture(self, &t, shown, cx)))
            .when(!self.android.immersive, |el| el.child(sidebar(self, &t, cx)))
    }
}

/// The Android picture, with the controls over it while editing.
fn picture(
    v: &GameView,
    t: &Theme,
    shown: (f32, f32),
    cx: &mut Context<GameView>,
) -> gpui::AnyElement {
    let body = match (&v.android.error, &v.android.image) {
        (Some(why), _) => message(t, "The compositor did not start", why),
        (None, Some((_, image))) => div()
            .size_full()
            .child(gpui::img(gpui::ImageSource::Render(image.clone())).size_full())
            .into_any_element(),
        (None, None) => message(
            t,
            "Waiting for Android",
            "Nothing has connected yet. Start a session against this window:\n\
             WAYLAND_DISPLAY=wayland-liw waydroid session start",
        ),
    };

    let mut layer = div().relative().size_full().child(body);

    if !v.mapper.is_on() {
        // Playing. The mouse is a finger: press, drag, release.
        //
        // Move is only listened for while something is down. Android has no
        // hover, so a stream of moves with no finger on the screen is work
        // for nothing — and on this path every one of them is a write into a
        // FIFO the guest has to read.
        return layer
            .id("play-surface")
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |v: &mut GameView, ev: &gpui::MouseDownEvent, _, cx| {
                    let p = (f32::from(ev.position.x), f32::from(ev.position.y));
                    let n = crate::touch::to_norm(p, shown);
                    // Both halves are logged because "the mouse does nothing"
                    // has two very different causes: the handler never fires,
                    // or it fires and the position falls outside the picture.
                    tracing::debug!(?p, ?shown, hit = n.is_some(), "mouse down");
                    if let Some(n) = n {
                        v.touch.press(n);
                        cx.notify();
                    }
                }),
            )
            .on_mouse_move(cx.listener(
                move |v: &mut GameView, ev: &gpui::MouseMoveEvent, _, _| {
                    if ev.pressed_button == Some(gpui::MouseButton::Left) {
                        if let Some(n) = crate::touch::to_norm(
                            (f32::from(ev.position.x), f32::from(ev.position.y)), shown)
                        {
                            v.touch.drag(n);
                        }
                    } else {
                        // Drag off the WINDOW and release there and no mouse-up
                        // reaches us at all — the pointer left first. The next
                        // move back in is the first news of it, and it says the
                        // button is up, so lift then.
                        v.touch.release();
                    }
                },
            ))
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(move |v: &mut GameView, _: &gpui::MouseUpEvent, _, cx| {
                    v.touch.release();
                    cx.notify();
                }),
            )
            // Press on the game, drag onto the rail, let go there: `on_mouse_up`
            // only fires inside the element's own bounds, so that release never
            // arrives and the finger stays down — the game keeps walking, or
            // keeps firing, with nothing touching the mouse. This catches it.
            // Harmless when no finger is down: `release` returns immediately.
            .on_mouse_up_out(
                gpui::MouseButton::Left,
                cx.listener(move |v: &mut GameView, _: &gpui::MouseUpEvent, _, cx| {
                    v.touch.release();
                    cx.notify();
                }),
            )
            .into_any_element();
    }

    // The catcher goes UNDER the controls, so clicking a control selects it
    // instead of placing another one on top of it.
    layer = layer.child(
        div()
            .id("place")
            .absolute()
            .inset_0()
            .cursor_crosshair()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |v: &mut GameView, ev: &gpui::MouseDownEvent, window, cx| {
                    window.focus(&v.focus, cx);
                    let p = ev.position;
                    v.mapper.place((f32::from(p.x), f32::from(p.y)), shown);
                    cx.notify();
                }),
            ),
    );

    for c in v.mapper.controls() {
        layer = layer.child(control(&c, t, shown, cx));
    }
    layer.into_any_element()
}

/// One control drawn over the picture.
fn control(
    c: &Control,
    t: &Theme,
    shown: (f32, f32),
    cx: &mut Context<GameView>,
) -> gpui::AnyElement {
    // A joystick is drawn at its real radius; everything else at a fixed size
    // that stays legible. Drawing a button at a made-up radius would suggest
    // a size that does not exist in the model.
    let d = match c.radius {
        Some(r) => (r * 2.0 * shown.0.min(shown.1)).max(28.0),
        None => 30.0,
    };
    let key = c.key.clone();
    div()
        .id(SharedString::from(format!("c-{}", c.key)))
        .absolute()
        .left(px(c.at.x * shown.0 - d / 2.0))
        .top(px(c.at.y * shown.1 - d / 2.0))
        .size(px(d))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(d / 2.0))
        .border_2()
        .border_color(if c.selected { t.accent } else { t.border })
        .bg(if c.selected { t.accent.opacity(0.28) } else { t.raised.opacity(0.7) })
        .text_size(px(11.0))
        .text_color(t.text)
        .cursor_pointer()
        .child(SharedString::from(c.label.clone()))
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(move |v: &mut GameView, _, window, cx| {
                window.focus(&v.focus, cx);
                v.mapper.select(&key);
                cx.notify();
            }),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// The rail and its panel
// ---------------------------------------------------------------------------

fn sidebar(v: &GameView, t: &Theme, cx: &mut Context<GameView>) -> gpui::AnyElement {
    div()
        .flex()
        .flex_row()
        .h_full()
        .when(v.sidebar_open, |el| el.child(panel(v, t, cx)))
        .child(rail(v, t, cx))
        .into_any_element()
}

fn rail(v: &GameView, t: &Theme, cx: &mut Context<GameView>) -> gpui::AnyElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(S2))
        .w(px(RAIL_W))
        .h_full()
        .py(px(S3))
        .bg(t.surface)
        .border_l_1()
        .border_color(t.border)
        .child(tool(t, "r-open", if v.sidebar_open { "›" } else { "‹" }, false, cx,
                    |v, cx| { v.sidebar_open = !v.sidebar_open; cx.notify(); }))
        .child(tool(t, "r-map", "⌨", v.mapper.is_on(), cx,
                    |v, cx| v.toggle_mapping(cx)))
        .child(tool(t, "r-full", "⛶", v.android.immersive, cx,
                    |v, cx| { v.android.immersive = !v.android.immersive; cx.notify(); }))
        .into_any_element()
}

/// One rail button. The glyph carries it: this gpui revision has no tooltip,
/// and a rail wide enough for a label would stop being a rail.
fn tool<F>(
    t: &Theme, id: &'static str, glyph: &str, on: bool,
    cx: &mut Context<GameView>, f: F,
) -> gpui::AnyElement
where
    F: Fn(&mut GameView, &mut Context<GameView>) + 'static,
{
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .size(px(32.0))
        .rounded(px(RADIUS))
        .text_size(px(15.0))
        .text_color(if on { t.accent } else { t.text_muted })
        .when(on, |e| e.bg(t.raised))
        .cursor_pointer()
        .hover(|x| x.bg(t.raised).text_color(t.text))
        .child(SharedString::from(glyph.to_string()))
        .on_click(cx.listener(move |v, _, _, cx| f(v, cx)))
        .into_any_element()
}

fn panel(v: &GameView, t: &Theme, cx: &mut Context<GameView>) -> gpui::AnyElement {
    let body = if v.mapper.is_on() { editor(v, t, cx) } else { idle(v, t) };
    div()
        .id("panel")
        .flex()
        .flex_col()
        .w(px(PANEL_W))
        .h_full()
        .p(px(S3))
        .gap(px(S2))
        .bg(t.surface)
        .border_l_1()
        .border_color(t.border)
        .overflow_y_scroll()
        .child(body)
        .into_any_element()
}

fn idle(v: &GameView, t: &Theme) -> gpui::AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(S2))
        .child(title_row(v, t))
        .child(note(t, if v.android.running() {
            "Android is drawing into this window."
        } else {
            "Nothing is hosted yet."
        }))
        .child(head(t, "Controls"))
        .child(note(t, "Open the keyboard to lay controls on the game."))
        .into_any_element()
}

/// The editor panel: a palette, then the selected control's settings.
fn editor(v: &GameView, t: &Theme, cx: &mut Context<GameView>) -> gpui::AnyElement {
    let mut palette = div().flex().flex_col().gap(px(S1));
    for kind in Kind::ALL {
        let on = v.mapper.palette == kind;
        palette = palette.child(
            div()
                .id(SharedString::from(format!("k-{}", kind.label())))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(S2))
                .px(px(S2))
                .py(px(S1))
                .rounded(px(RADIUS))
                .border_1()
                .border_color(if on { t.accent } else { t.border })
                .when(on, |e| e.bg(t.raised))
                .text_size(px(12.0))
                .cursor_pointer()
                .hover(|x| x.bg(t.raised))
                .child(SharedString::from(kind.glyph()))
                .child(SharedString::from(kind.label()))
                .on_click(cx.listener(move |v: &mut GameView, _, _, cx| {
                    v.mapper.palette = kind;
                    cx.notify();
                })),
        );
    }

    let selection = v.mapper.selection();
    let settings = match &selection {
        None => note(t, "Click the game to place one."),
        Some(c) => {
            let mut b = div().flex().flex_col().gap(px(S1));
            b = b.child(row(t, "Kind", c.kind.label()));
            let key_text = if v.mapper.binding {
                "press a key…".to_string()
            } else {
                c.label.clone()
            };
            b = b.child(row(t, "Key", &key_text));
            b = b.child(row(t, "At", &format!("{:.0}%, {:.0}%", c.at.x * 100.0, c.at.y * 100.0)));
            if let Some(r) = c.radius {
                b = b.child(row(t, "Size", &format!("{:.0}%   [ ]", r * 100.0)));
            }
            b.child(note(t, "Arrows nudge it. Delete removes it."))
                .into_any_element()
        }
    };

    div()
        .flex()
        .flex_col()
        .gap(px(S2))
        .child(head(t, "Place"))
        .child(palette)
        .child(head(t, "Selected"))
        .child(settings)
        .when_some(v.mapper.error.clone(), |el, e| {
            el.child(div().text_size(px(11.0)).text_color(t.bad).child(SharedString::from(e)))
        })
        .when(v.mapper.saved, |el| {
            el.child(div().text_size(px(11.0)).text_color(t.ok).child("Saved."))
        })
        .child(
            div()
                .flex()
                .flex_row()
                .gap(px(S1))
                .child(action(t, "e-save", "Save", cx, |v, cx| { v.mapper.save(); cx.notify(); }))
                .child(action(t, "e-done", "Done", cx, |v, cx| { v.mapper.end(); cx.notify(); })),
        )
        .into_any_element()
}

fn row(t: &Theme, k: &str, val: &str) -> gpui::AnyElement {
    div()
        .flex()
        .flex_row()
        .justify_between()
        .text_size(px(12.0))
        .child(div().text_color(t.text_faint).child(SharedString::from(k.to_string())))
        .child(div().text_color(t.text).child(SharedString::from(val.to_string())))
        .into_any_element()
}

fn action<F>(
    t: &Theme, id: &'static str, label: &str,
    cx: &mut Context<GameView>, f: F,
) -> gpui::AnyElement
where
    F: Fn(&mut GameView, &mut Context<GameView>) + 'static,
{
    div()
        .id(id)
        .px(px(S3))
        .py(px(S1))
        .rounded(px(RADIUS))
        .border_1()
        .border_color(t.border)
        .text_size(px(12.0))
        .cursor_pointer()
        .hover(|x| x.bg(t.raised))
        .child(SharedString::from(label.to_string()))
        .on_click(cx.listener(move |v, _, _, cx| f(v, cx)))
        .into_any_element()
}

/// The game's icon and name, at the top of the panel.
fn title_row(v: &GameView, t: &Theme) -> gpui::AnyElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(S2))
        .when_some(v.icon.clone(), |el, p| {
            el.child(
                gpui::img(gpui::ImageSource::Resource(gpui::Resource::Path(
                    std::sync::Arc::from(p.as_path()),
                )))
                .w(px(28.0))
                .h(px(28.0)),
            )
        })
        .child(
            div()
                .text_size(px(13.0))
                .text_color(t.text)
                .child(SharedString::from(v.title.clone())),
        )
        .into_any_element()
}

fn head(t: &Theme, s: &str) -> gpui::AnyElement {
    div()
        .text_size(px(11.0))
        .text_color(t.text_faint)
        .child(SharedString::from(s.to_uppercase()))
        .into_any_element()
}

fn note(t: &Theme, s: &str) -> gpui::AnyElement {
    div()
        .text_size(px(12.0))
        .text_color(t.text_muted)
        .child(SharedString::from(s.to_string()))
        .into_any_element()
}

fn message(t: &Theme, title: &str, detail: &str) -> gpui::AnyElement {
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(S2))
        .child(div().text_color(t.text).child(SharedString::from(title.to_string())))
        .child(
            div()
                .max_w(px(520.))
                .text_color(t.text_faint)
                .text_size(px(12.0))
                .child(SharedString::from(detail.to_string())),
        )
        .into_any_element()
}

/// Opens the game window.
///
/// `title` is the game's name rather than its package: a window called
/// `com.ForgeGames.SpecialForcesGroup2` in the task switcher is a package
/// manager's idea of a name, not a person's.
/// How often the view looks for a new frame.
///
/// Deliberately much shorter than a frame. At 16ms this polled at the same
/// rate the compositor produced, but the two clocks were not locked together:
/// measured, 60.0 frames a second were produced and 56.3 reached the window —
/// a tick would find nothing, and the tick after it would find a frame that
/// had already been overwritten by the next one. Four a second, lost to
/// beating.
///
/// At 4ms there are four looks per frame, so a frame can only be missed if
/// two arrive inside 4ms, which 60Hz cannot do. A look that finds nothing
/// costs one lock and one integer compare, and the window is only repainted
/// when a frame is actually taken.
const POLL: std::time::Duration = std::time::Duration::from_millis(4);

/// The window class this window carries.
///
/// It is NOT the launcher's. The key mapper decides whether Android has the
/// focus from the focused window's class, and this window is Android's screen
/// while the launcher is a list of games; one class for both would mean the
/// keyboard is grabbed over the library, where the user is trying to type.
///
/// Wayland also has no per-window icon, so a compositor looks this up in the
/// desktop entries — hence `dist/desktop/liwinux-game.desktop`, whose
/// StartupWMClass has to match.
pub const APP_ID: &str = "liwinux-game";

/// Sends the focus state to the daemon, as the class it would see from KWin.
///
/// Failures are ignored on purpose: no daemon means no key mapper either, and
/// a window that refused to draw because it could not report focus would be
/// worse than one that simply does not map keys.
fn report(active: bool, cx: &mut App) {
    let class = if active { APP_ID } else { "" };
    gpui_tokio::Tokio::spawn(cx, async move {
        if let Ok(m) = liw_core::manager::Manager::connect().await {
            let _ = m.set_active_window(class).await;
            tracing::debug!(class, "reported focus");
        }
    })
    .detach();
}

pub fn open(
    package: String,
    title: String,
    icon: Option<std::path::PathBuf>,
    cx: &mut App,
) -> anyhow::Result<()> {
    let bounds = gpui::Bounds::centered(None, gpui::size(px(1280.), px(760.)), cx);
    cx.open_window(
        gpui::WindowOptions {
            window_bounds: Some(gpui::WindowBounds::Windowed(bounds)),
            window_min_size: Some(gpui::size(px(640.), px(400.))),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some(SharedString::from(title.clone())),
                ..Default::default()
            }),
            // Same family as the launcher, so the two group together and the
            // window is recognisable before its own icon is drawn inside it.
            // A class of its OWN, not the launcher's. The keymapper decides
            // whether Android has the focus from the focused window's class,
            // and this window is Android's screen while the launcher is a
            // list of games. One class for both would mean the keyboard is
            // grabbed over the library, where the user is trying to type.
            app_id: Some(APP_ID.into()),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| GameView::new(package, title, icon, window, cx)),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The picture's size is computed from this. A second opinion about the
    /// rail's width would put every control a few pixels from its click.
    #[test]
    fn the_panel_widens_the_sidebar() {
        assert_eq!(sidebar_width(false), RAIL_W);
        assert_eq!(sidebar_width(true), RAIL_W + PANEL_W);
    }

    /// Collapsing hides the panel, never the way back to it.
    #[test]
    fn the_rail_never_disappears() {
        assert!(sidebar_width(false) > 0.0);
    }
}
