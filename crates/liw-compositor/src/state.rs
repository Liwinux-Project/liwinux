//! The compositor state and the Wayland protocols it speaks.
//!
//! # What is advertised, and why each one
//!
//! * `wl_compositor` / `wl_subcompositor` — surfaces exist at all.
//! * `xdg_shell` — hwcomposer creates a toplevel and waits to be told its
//!   size. Without a configure it will sit there and never draw.
//! * `wl_output` — Android reads the mode to size its display.
//! * `wl_shm` — advertised because a client that finds nothing else will use
//!   it, and a slow picture beats no picture while this is being brought up.
//! * `linux-dmabuf` — the one that matters. Waydroid's hwcomposer hands over
//!   dmabuf; with no such global there is nowhere to put a frame.
//!
//! * `wl_seat` — advertised because hwcomposer will not come up without it.
//!   The first version left it out, reasoning that game input already
//!   bypasses the compositor through Waydroid's touch pipe so a seat bought
//!   nothing. Measured: with no seat the composer HAL never registers at all,
//!   SurfaceFlinger waits for it forever and Android never finishes booting.
//!   A control run against KWin's socket, everything else identical, booted
//!   in 15 seconds — so the seat, not the environment, was the difference.
//!   It carries keyboard, pointer and touch capabilities and delivers no
//!   events; being there is what the client needs.

use std::sync::{Arc, Mutex};

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::{Format, Fourcc, Modifier};
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::protocol::{wl_buffer, wl_seat, wl_surface::WlSurface};
use smithay::reexports::wayland_server::{Client, DisplayHandle, Resource};
use smithay::utils::Serial;
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    with_states, CompositorClientState, CompositorHandler, CompositorState, SurfaceAttributes,
    BufferAssignment,
};
use smithay::wayland::dmabuf::{
    DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier,
};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
};
use smithay::wayland::output::OutputHandler;
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::{
    delegate_compositor, delegate_dmabuf, delegate_output, delegate_seat, delegate_shm,
    delegate_xdg_shell,
};

use crate::{ClientState, Guest, GuestHandle};

/// What we know about the single surface we host.
#[derive(Debug, Default, Clone)]
pub struct Surface {
    pub width: i32,
    pub height: i32,
}

pub struct Compositor {
    pub dh: DisplayHandle,
    pub compositor: CompositorState,
    pub shm: ShmState,
    pub xdg: XdgShellState,
    pub dmabuf: DmabufState,
    pub dmabuf_global: DmabufGlobal,
    pub output: Output,
    pub seat_state: SeatState<Self>,
    /// The seat itself, kept alive for as long as the compositor is. Dropping
    /// it would remove the global from under a client that needs it.
    pub seat: Seat<Self>,
    /// What the host side is allowed to see. Nothing else crosses the thread.
    pub guest: GuestHandle,
    /// The surface being hosted, once there is one. There is only ever one:
    /// this is not a desktop, it shows Android and nothing else.
    pub surface: Option<WlSurface>,
    /// Size we tell the client to be.
    pub size: (i32, i32),
    pub running: bool,
}

impl Compositor {
    pub fn new(dh: &DisplayHandle, size: (i32, i32), refresh_mhz: i32) -> Self {
        let compositor = CompositorState::new::<Self>(dh);
        let shm = ShmState::new::<Self>(dh, Vec::new());
        let xdg = XdgShellState::new::<Self>(dh);

        let mut dmabuf = DmabufState::new();
        let dmabuf_global = dmabuf.create_global::<Self>(dh, dmabuf_formats());

        let output = Output::new(
            "liwinux".to_string(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "liwinux".into(),
                model: "embedded".into(),
            },
        );
        let mode = Mode { size: size.into(), refresh: refresh_mhz };
        output.change_current_state(Some(mode), None, None, Some((0, 0).into()));
        output.set_preferred(mode);
        output.create_global::<Self>(dh);

        // The capabilities are advertised even though nothing is ever sent
        // through them. hwcomposer checks that a seat exists and has the
        // capability before it will proceed; an empty seat is not enough.
        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(dh, "liwinux");
        seat.add_pointer();
        seat.add_touch();
        if let Err(e) = seat.add_keyboard(Default::default(), 200, 25) {
            tracing::warn!(error = %e, "no keyboard on the seat");
        }

        Self {
            dh: dh.clone(),
            compositor,
            shm,
            xdg,
            dmabuf,
            dmabuf_global,
            output,
            seat_state,
            seat,
            guest: Arc::new(Mutex::new(Guest::default())),
            surface: None,
            size,
            running: true,
        }
    }

    /// Edits the shared guest state. Held for as little time as possible: the
    /// UI thread reads this and must never wait on the event loop.
    fn with_guest(&self, f: impl FnOnce(&mut Guest)) {
        if let Ok(mut g) = self.guest.lock() {
            f(&mut g);
        }
    }
}

/// Formats offered over `linux-dmabuf`.
///
/// Advertised without a renderer behind them, so this list is a claim about
/// what we will ACCEPT rather than what some GPU reported.
///
/// The channel order was MEASURED, not reasoned about. The first attempt
/// offered ARGB8888 and XRGB8888 — the guess a Linux compositor makes — and
/// hwcomposer answered with a protocol error naming exactly what it wanted:
///
/// ```text
/// zwp_linux_buffer_params_v1: Format DrmFourcc(AB24)/34324241 is not supported.
/// ```
///
/// AB24 is ABGR8888. Android's `HAL_PIXEL_FORMAT_RGBA_8888` is DRM's
/// ABGR8888, and getting that backwards costs nothing but a wrong guess and
/// a client that hangs up. Both orders are offered now: the BGR pair because
/// the client asked for it, the RGB pair because advertising it costs nothing
/// and a different client may want it.
fn dmabuf_formats() -> Vec<Format> {
    [
        Fourcc::Abgr8888,
        Fourcc::Xbgr8888,
        Fourcc::Argb8888,
        Fourcc::Xrgb8888,
    ]
    .into_iter()
    .map(|code| Format { code, modifier: Modifier::Invalid })
    .collect()
}

// ---------------------------------------------------------------------------
// wl_compositor
// ---------------------------------------------------------------------------

impl CompositorHandler for Compositor {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor
    }

    fn commit(&mut self, surface: &WlSurface) {
        // Read the buffer FIRST. on_commit_buffer_handler takes the pending
        // state as it hands it to the renderer, so a read afterwards finds
        // nothing — which is why the report said buffer=None for a whole run
        // while frames were arriving perfectly well.
        let attached = with_states(surface, |states| {
            let mut guard = states.cached_state.get::<SurfaceAttributes>();
            match &guard.current().buffer {
                Some(BufferAssignment::NewBuffer(buf)) => describe_buffer(buf),
                Some(BufferAssignment::Removed) => None,
                None => None,
            }
        });

        // This is what makes the attached buffer visible to the renderer.
        // Without it the surface tree yields no elements and the window stays
        // blank while every counter here still climbs — a commit is recorded,
        // a dmabuf is named, and nothing is drawn.
        smithay::backend::renderer::utils::on_commit_buffer_handler::<Self>(surface);

        self.with_guest(|g| {
            g.commits += 1;
            if let Some((kind, w, h)) = attached {
                g.buffer = Some(kind);
                g.size = Some((w, h));
            }
        });
    }
}

/// What kind of buffer was attached, and how big.
///
/// A dmabuf carries its own dimensions, which is the whole reason this path
/// can report a size without a renderer. Anything else is named rather than
/// guessed at — reporting an unknown buffer as shm would be a lie that only
/// shows up much later as a wrong picture.
fn describe_buffer(buf: &wl_buffer::WlBuffer) -> Option<(String, i32, i32)> {
    if let Some(dma) = buf.data::<Dmabuf>() {
        use smithay::backend::allocator::Buffer;
        let (w, h) = (dma.width() as i32, dma.height() as i32);
        return Some((format!("dmabuf {:?}", dma.format().code), w, h));
    }
    // Naming shm separately is not pedantry. "not a dmabuf" covers both a
    // client that fell back to software copies and one that attached
    // something we failed to recognise, and those call for opposite fixes.
    if let Ok(d) = smithay::wayland::shm::with_buffer_contents(buf, |_, _, d| d) {
        return Some((format!("shm {:?}", d.format), d.width, d.height));
    }
    Some(("unrecognised buffer type".to_string(), 0, 0))
}

// ---------------------------------------------------------------------------
// buffers
// ---------------------------------------------------------------------------

impl BufferHandler for Compositor {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl ShmHandler for Compositor {
    fn shm_state(&self) -> &ShmState {
        &self.shm
    }
}

// ---------------------------------------------------------------------------
// linux-dmabuf
// ---------------------------------------------------------------------------

impl DmabufHandler for Compositor {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        use smithay::backend::allocator::Buffer;
        tracing::debug!(
            width = dmabuf.width(),
            height = dmabuf.height(),
            format = ?dmabuf.format(),
            "dmabuf imported"
        );
        // Accepted without a renderer to hand it to. That is honest for this
        // stage — the point is to prove the client gets a buffer across at
        // all. Drawing it is the next step and needs the gpui join.
        if notifier.successful::<Self>().is_err() {
            self.with_guest(|g| {
                g.error = Some("client vanished before the dmabuf was acknowledged".into())
            });
        }
    }
}

// ---------------------------------------------------------------------------
// xdg_shell
// ---------------------------------------------------------------------------

impl XdgShellHandler for Compositor {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        // The client waits for a configure before it draws anything. Sending
        // it here, with a size and fullscreen set, is what turns a connected
        // client into a drawing one.
        let size = self.size;
        surface.with_pending_state(|state| {
            state.size = Some(size.into());
            state.states.set(xdg_toplevel::State::Fullscreen);
            state.states.set(xdg_toplevel::State::Activated);
        });
        surface.send_configure();

        self.surface = Some(surface.wl_surface().clone());
        self.with_guest(|g| {
            g.connected = true;
            g.size = Some(size);
        });
        tracing::info!(?size, "toplevel created and configured");
    }

    fn new_popup(&mut self, _surface: PopupSurface, _positioner: PositionerState) {}

    fn grab(&mut self, _surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {}

    fn reposition_request(
        &mut self,
        _surface: PopupSurface,
        _positioner: PositionerState,
        _token: u32,
    ) {
    }

    fn toplevel_destroyed(&mut self, _surface: ToplevelSurface) {
        self.surface = None;
        self.with_guest(|g| {
            g.connected = false;
            g.buffer = None;
        });
        tracing::info!("toplevel destroyed");
    }
}

impl OutputHandler for Compositor {}

impl SeatHandler for Compositor {
    // The surface is its own focus target; smithay implements all three
    // target traits for WlSurface, so nothing else has to exist for the
    // xdg_shell bound to be satisfied.
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }
}

delegate_compositor!(Compositor);
delegate_shm!(Compositor);
delegate_xdg_shell!(Compositor);
delegate_output!(Compositor);
delegate_seat!(Compositor);
delegate_dmabuf!(Compositor);

#[cfg(test)]
mod tests {
    use super::*;

    /// The advertised list is a promise to the client. An empty one leaves
    /// hwcomposer with nothing to pick and it silently never draws.
    #[test]
    fn dmabuf_formats_are_not_empty() {
        assert!(!dmabuf_formats().is_empty());
    }

    /// The format hwcomposer NAMED in its protocol error must be offered.
    /// Without it the client hangs up mid-handshake, which is how this was
    /// found in the first place.
    #[test]
    fn the_format_hwcomposer_asked_for_is_offered() {
        let f = dmabuf_formats();
        assert!(f.iter().any(|x| x.code == Fourcc::Abgr8888),
                "ABGR8888 is the one AB24 in the error meant");
    }

    /// Both channel orders, so a wrong guess about which one a client wants
    /// cannot cost a connection again.
    #[test]
    fn both_channel_orders_are_offered() {
        let f = dmabuf_formats();
        for want in [Fourcc::Abgr8888, Fourcc::Xbgr8888,
                     Fourcc::Argb8888, Fourcc::Xrgb8888] {
            assert!(f.iter().any(|x| x.code == want), "{want:?} missing");
        }
    }

    /// Every advertised format uses the implicit modifier. Claiming an
    /// explicit one we have never imported would be support we invented.
    #[test]
    fn only_the_implicit_modifier_is_claimed() {
        assert!(dmabuf_formats().iter().all(|f| f.modifier == Modifier::Invalid));
    }
}
