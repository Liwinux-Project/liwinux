//! A nested Wayland compositor for Waydroid to connect to.
//!
//! # Why liwinux hosts a compositor
//!
//! Waydroid's hwcomposer is an ordinary Wayland client, and which compositor
//! it connects to is chosen from `WAYLAND_DISPLAY` when the session starts
//! (measured; see `docs/embedded.md`). So putting Android inside our window
//! needs no patching of Waydroid at all — it needs us to be the compositor it
//! connects to.
//!
//! # Scope
//!
//! This is deliberately not a desktop. It hosts exactly one client and shows
//! it in one place. There is no window management, no stacking, no
//! decorations: Android draws its own everything, and every feature we do not
//! implement is one that cannot be wrong.

use std::sync::{Arc, Mutex};

use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};

pub mod embedded;
pub mod headless;
pub mod state;

pub use embedded::{spawn, Embedded};
pub use headless::{fit, Frame, FrameSlot, Headless, REAL_SCREEN};
pub use state::{Compositor, Surface};

/// What the host needs to know about the guest, without holding Wayland types.
///
/// The event loop runs on its own thread; the UI must not reach into it. This
/// is the whole shared surface between them, and keeping it this small is
/// what stops the compositor from leaking into the renderer.
#[derive(Debug, Default, Clone)]
pub struct Guest {
    /// A client is connected.
    pub connected: bool,
    /// Size of its surface, in its own pixels.
    pub size: Option<(i32, i32)>,
    /// Commits seen. Rising means it is drawing.
    pub commits: u64,
    /// What kind of buffer it attached, once we have seen one.
    pub buffer: Option<String>,
    /// Last error worth telling a person about.
    pub error: Option<String>,
}

impl Guest {
    /// A commit arrived from SOME surface the client owns.
    ///
    /// Counting every one is right: a subsurface changing is still a reason
    /// to repaint. Measuring the screen from every one is not.
    pub fn saw_commit(&mut self) {
        self.commits += 1;
    }

    /// The TOPLEVEL committed a buffer — this is what sizes the screen.
    ///
    /// Kept apart from `saw_commit` because mixing them cost a real bug: the
    /// client sets a cursor on our seat, that cursor surface commits at
    /// 37x37, and a view that fits itself to the last size recorded leapt to
    /// thirty-odd times scale for as long as a finger was down. It looked
    /// like the game was zooming. It was the wrong surface being measured.
    pub fn saw_screen(&mut self, buffer: String, width: i32, height: i32) {
        self.buffer = Some(buffer);
        self.size = Some((width, height));
    }
}

/// Shared handle to the guest's state.
pub type GuestHandle = Arc<Mutex<Guest>>;

/// Per-client bookkeeping smithay requires.
#[derive(Default)]
pub struct ClientState {
    pub compositor: smithay::wayland::compositor::CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _: ClientId) {}
    fn disconnected(&self, _: ClientId, reason: DisconnectReason) {
        tracing::info!(?reason, "client disconnected");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A commit from a surface that is not the screen must not resize it.
    ///
    /// This is the "touching the game zooms it" bug. The client sets a cursor
    /// on our seat; that surface commits at 37x37; the view fits itself to
    /// the last recorded size and blows the picture up while the finger is
    /// down. Counting the commit is right — it is still a reason to repaint —
    /// but measuring the screen from it is not.
    #[test]
    fn a_commit_alone_never_changes_the_screen_size() {
        let mut g = Guest::default();
        g.saw_screen("dmabuf".into(), 2560, 1440);
        for _ in 0..5 {
            g.saw_commit();
        }
        assert_eq!(g.size, Some((2560, 1440)), "the cursor must not resize the screen");
        assert_eq!(g.commits, 5, "every commit still counts, for the repaint");
    }

    #[test]
    fn the_screen_size_follows_the_toplevel() {
        let mut g = Guest::default();
        g.saw_screen("dmabuf".into(), 1280, 720);
        assert_eq!(g.size, Some((1280, 720)));
        g.saw_screen("dmabuf".into(), 2560, 1440);
        assert_eq!(g.size, Some((2560, 1440)), "a real resize still lands");
    }

    #[test]
    fn nothing_is_known_before_the_first_frame() {
        let g = Guest::default();
        assert_eq!(g.size, None);
        assert_eq!(g.commits, 0);
        assert!(g.buffer.is_none());
    }
}
