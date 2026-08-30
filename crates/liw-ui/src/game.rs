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
    div, prelude::*, px, App, Context, Entity, FocusHandle, IntoElement, Render,
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
    frame_pump: Option<gpui::Task<()>>,
}

impl GameView {
    pub fn new(package: String, title: String, cx: &mut Context<Self>) -> Self {
        let mut v = Self {
            android: Android::default(),
            mapper: Mapper::default(),
            sidebar_open: true,
            focus: cx.focus_handle(),
            package,
            title,
            frame_pump: None,
        };
        v.android.start(1280, 720);
        v.pump(cx);
        v
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
            cx.background_executor()
                .timer(std::time::Duration::from_millis(16))
                .await;
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
        return layer.into_any_element();
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
        .child(head(t, &v.title))
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
pub fn open(package: String, title: String, cx: &mut App) -> anyhow::Result<()> {
    let bounds = gpui::Bounds::centered(None, gpui::size(px(1280.), px(760.)), cx);
    cx.open_window(
        gpui::WindowOptions {
            window_bounds: Some(gpui::WindowBounds::Windowed(bounds)),
            window_min_size: Some(gpui::size(px(640.), px(400.))),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some(SharedString::from(title.clone())),
                ..Default::default()
            }),
            ..Default::default()
        },
        |_, cx| cx.new(|cx| GameView::new(package, title, cx)),
    )?;
    Ok(())
}

/// A handle to keep, so the launcher does not open a second window for a game
/// that already has one.
pub type Open = Entity<GameView>;

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
