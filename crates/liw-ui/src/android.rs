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

use gpui::{
    div, img, px, Context, ImageSource, IntoElement, ParentElement, RenderImage, Styled,
};
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
    _cx: &mut Context<AppState>,
) -> gpui::AnyElement {
    let view = window.viewport_size();
    let chrome = if s.android.immersive { 0.0 } else { crate::theme::HEADER_H };
    s.android.resize(f32::from(view.width), f32::from(view.height) - chrome);

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

    div().size_full().bg(t.bg).child(body).into_any_element()
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
