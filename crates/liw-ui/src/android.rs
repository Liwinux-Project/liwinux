//! Android, drawn inside this window.
//!
//! The compositor runs on a thread of its own and leaves the newest frame in
//! a slot. This module reads that slot, turns the bytes into something gpui
//! can draw, and asks for a render size that matches the view.
//!
//! # Why bytes and not a texture
//!
//! gpui has no external-texture path on Linux: its `surface` element is
//! macOS-only and takes a `CVPixelBuffer`. Handing it a texture would mean
//! patching the pinned Zed fork. Reading each frame back instead was measured
//! first — 0.8 to 2.0 ms at 1280x720, 5 to 12 per cent of a frame — and that
//! is the price of not needing the patch yet.

use std::sync::Arc;

use gpui::{div, prelude::*, px, Context, ImageSource, IntoElement, RenderImage};
use gpui::img;
use liw_compositor::Embedded;

use crate::state::AppState;
use crate::theme::Theme;

gpui::actions!(liwinux, [
    /// Hide the chrome and give the whole window to Android.
    ToggleImmersive,
]);

/// The socket Waydroid is pointed at.
///
/// A fixed name rather than an automatic one: the whole mechanism is putting
/// this in front of `waydroid session start`, and a name chosen for us would
/// have to be read back out of a log before anything could use it.
pub const SOCKET: &str = "wayland-liw";

/// What the embedded view needs to remember between frames.
#[derive(Default)]
pub struct Android {
    pub embedded: Option<Arc<Embedded>>,
    /// The last frame turned into a gpui image, with the serial it came from.
    pub image: Option<(u64, Arc<RenderImage>)>,
    /// Chrome hidden, Android filling the window. Toggled with F11.
    pub immersive: bool,
    pub error: Option<String>,
}

impl Android {
    pub fn running(&self) -> bool {
        self.embedded.as_ref().is_some_and(|e| e.is_running())
    }

    /// Starts the compositor if it is not already up.
    pub fn start(&mut self, width: i32, height: i32) {
        if self.running() {
            return;
        }
        match liw_compositor::spawn(SOCKET, width, height) {
            Ok(e) => {
                self.embedded = Some(Arc::new(e));
                self.error = None;
            }
            // The failure worth naming separately is a socket already in use,
            // because it means a previous run is still holding it and the fix
            // is to stop that, not to retry.
            Err(e) => self.error = Some(e),
        }
    }

    /// Picks up a new frame, if there is one.
    ///
    /// Returns true when the picture changed, so the caller can skip a
    /// repaint it does not need. Converting costs a copy; doing it for a
    /// frame already on screen would pay that copy for nothing.
    pub fn poll(&mut self) -> bool {
        let Some(e) = &self.embedded else { return false };
        let Ok(slot) = e.frames.lock() else { return false };
        let Some(frame) = slot.as_ref() else { return false };
        if self.image.as_ref().is_some_and(|(s, _)| *s == frame.serial) {
            return false;
        }
        let Some(buf) =
            image::RgbaImage::from_raw(frame.width, frame.height, frame.bgra.clone())
        else {
            return false;
        };
        let img = RenderImage::new(vec![image::Frame::new(buf)]);
        self.image = Some((frame.serial, Arc::new(img)));
        true
    }

    /// The guest's own screen size, once it has told us one.
    ///
    /// Needed to place markers: a binding is stored normalised against the
    /// GUEST, and drawing it means knowing how large the guest lands in the
    /// view.
    pub fn guest_size(&self) -> Option<(i32, i32)> {
        let e = self.embedded.as_ref()?;
        let g = e.guest.lock().ok()?;
        g.size
    }

    /// Tells the compositor how big to render.
    pub fn resize(&self, width: f32, height: f32) {
        if let Some(e) = &self.embedded {
            e.request_size(width.max(1.0) as i32, height.max(1.0) as i32);
        }
    }
}

/// The Android view.
///
/// The render size is taken from the window rather than measured from the
/// element, because the element fills whatever is left after the nav strip
/// and that is the same arithmetic. Asking every frame is free: the request
/// is two atomics, and the compositor only rebuilds its target when the
/// numbers actually change.
pub fn render(
    s: &AppState,
    t: &Theme,
    window: &gpui::Window,
    cx: &mut Context<AppState>,
) -> gpui::AnyElement {
    // The picture is the window minus the chrome above it and the rail beside
    // it. The same arithmetic decides what the compositor renders and where a
    // click lands, so it happens once, here.
    let win = window.viewport_size();
    let chrome = if s.android.immersive { 0.0 } else { crate::theme::HEADER_H };
    let rail = crate::sidebar::width(s.sidebar_open);
    let view = (f32::from(win.width) - rail, f32::from(win.height) - chrome);
    s.android.resize(view.0, view.1);

    let picture = picture(s, t, view, cx);
    return div()
        .flex()
        .flex_row()
        .size_full()
        .bg(t.bg)
        .child(div().flex_1().h_full().child(picture))
        .child(crate::sidebar::render(s, t, cx))
        .into_any_element();
}

/// The Android picture, with the mapping markers over it when editing.
fn picture(
    s: &AppState,
    t: &Theme,
    view: (f32, f32),
    cx: &mut Context<AppState>,
) -> gpui::AnyElement {
    // Where the guest actually lands inside the view, from the same function
    // the compositor fits with.
    let guest = s.android.guest_size().unwrap_or((view.0 as i32, view.1 as i32));
    let (_, shown) = liw_compositor::fit(guest, (view.0 as i32, view.1 as i32));

    let body = match (&s.android.error, &s.android.image) {
        (Some(why), _) => message(t, "The compositor did not start", why),
        (None, Some((_, image))) => div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            // object_fit is not set: the compositor already fits the guest
            // into the size it was asked for, so letting gpui scale again
            // would resample a picture that is already the right shape.
            .child(img(ImageSource::Render(image.clone())).size_full())
            .into_any_element(),
        (None, None) if s.android.running() => message(
            t,
            "Waiting for Android",
            "The compositor is up and nothing has connected to it yet. Start a \
             session against it:  WAYLAND_DISPLAY=wayland-liw waydroid session start",
        ),
        (None, None) => message(
            t,
            "Not running",
            "Nothing is hosting Android yet.",
        ),
    };

    let editing = s.mapper.is_on();
    let mut layer = div().relative().size_full().child(body);

    if editing {
        for (name, at, key) in s.mapper.markers() {
            layer = layer.child(
                div()
                    .absolute()
                    .left(px(at.x * shown.0 - 14.0))
                    .top(px(at.y * shown.1 - 14.0))
                    .size(px(28.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(14.0))
                    .bg(t.accent)
                    .text_size(px(11.0))
                    .text_color(t.bg)
                    .child(gpui::SharedString::from(key))
                    .id(gpui::SharedString::from(format!("marker-{name}"))),
            );
        }
        // The click catcher goes ON TOP of the markers, so a click near an
        // existing binding still places a new one rather than being swallowed
        // by the marker's own hit area.
        let chrome = if s.android.immersive { 0.0 } else { crate::theme::HEADER_H };
        layer = layer.child(
            div()
                .id("map-catch")
                .absolute()
                .inset_0()
                .cursor_crosshair()
                .track_focus(&s.map_focus)
                .on_mouse_down(gpui::MouseButton::Left, cx.listener(
                    move |st: &mut AppState, ev: &gpui::MouseDownEvent, window, cx| {
                        // Take focus on the click too. Without it the first
                        // key after a click goes wherever focus happened to
                        // be, which is usually the search box.
                        window.focus(&st.map_focus, cx);
                        let p = ev.position;
                        st.mapper.place(
                            (f32::from(p.x), f32::from(p.y) - chrome),
                            shown,
                        );
                        cx.notify();
                    },
                ))
                .on_key_down(cx.listener(
                    move |st: &mut AppState, ev: &gpui::KeyDownEvent, _, cx| {
                        if st.mapper.assign(&ev.keystroke.key).is_some() {
                            cx.notify();
                        }
                    },
                )),
        );
    }

    layer.into_any_element()
}

fn message(t: &Theme, title: &str, detail: &str) -> gpui::AnyElement {
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .child(div().text_color(t.text).child(title.to_string()))
        .child(
            div()
                .max_w(px(520.))
                .text_color(t.text_faint)
                .child(detail.to_string()),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_running_before_it_starts() {
        let a = Android::default();
        assert!(!a.running());
        assert!(a.image.is_none());
    }

    /// Polling with no compositor must not panic and must report no change.
    #[test]
    fn polling_without_a_compositor_is_quiet() {
        let mut a = Android::default();
        assert!(!a.poll());
    }

    /// Resizing before the compositor exists is a no-op, not a crash: the
    /// view is laid out before anything is started.
    #[test]
    fn resizing_before_start_is_harmless() {
        let a = Android::default();
        a.resize(800.0, 600.0);
    }

    /// The socket name is what the user has to type in front of waydroid.
    /// If it ever changes, the message in the view has to change with it.
    #[test]
    fn the_socket_name_matches_what_the_view_tells_people_to_type() {
        assert_eq!(SOCKET, "wayland-liw");
    }
}
