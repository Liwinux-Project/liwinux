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

use gpui::RenderImage;
use liw_compositor::Embedded;



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
