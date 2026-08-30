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
pub use headless::{Frame, FrameSlot, Headless};
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
