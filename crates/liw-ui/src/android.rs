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
use std::time::Instant;

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
    /// How many frames the view actually picks up, per second.
    ///
    /// Not the same number as the compositor's: that one says how fast frames
    /// are PRODUCED, this one says how many reach the screen. They differ
    /// when the poll misses one, and the difference is what a player feels.
    meter: Meter,
}

/// Counts frames taken, and reports once a second.
#[derive(Default)]
struct Meter {
    taken: u32,
    polls: u32,
    since: Option<Instant>,
}

impl Meter {
    /// Records one poll; logs a line roughly once a second.
    fn tick(&mut self, took: bool) {
        self.polls += 1;
        if took {
            self.taken += 1;
        }
        let since = self.since.get_or_insert_with(Instant::now);
        let elapsed = since.elapsed();
        if elapsed < std::time::Duration::from_secs(1) {
            return;
        }
        tracing::info!(
            fps = self.taken as f32 / elapsed.as_secs_f32(),
            polls = self.polls as f32 / elapsed.as_secs_f32(),
            "frames reaching the window",
        );
        self.taken = 0;
        self.polls = 0;
        self.since = Some(Instant::now());
    }
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
        let took = self.take_frame();
        self.meter.tick(took);
        took
    }

    fn take_frame(&mut self) -> bool {
        let Some(e) = &self.embedded else { return false };
        let Ok(mut slot) = e.frames.lock() else { return false };
        // Read the serial before taking anything: an unchanged frame has to
        // be left in the slot, or a repaint that arrives before the next
        // commit would find it empty and blank the window.
        let serial = match slot.as_ref() {
            Some(f) => f.serial,
            None => return false,
        };
        if self.image.as_ref().is_some_and(|(s, _)| *s == serial) {
            return false;
        }
        // TAKE rather than clone. The frame is a full screen of pixels — at
        // 2560x1440 that is 14 MB, and cloning it paid for a second copy of
        // every frame on top of the readback that produced it. Nothing else
        // reads this slot, and the compositor overwrites it with the newest
        // frame regardless.
        let Some(frame) = slot.take() else { return false };
        drop(slot);
        let Some(buf) = image::RgbaImage::from_raw(frame.width, frame.height, frame.bgra)
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
