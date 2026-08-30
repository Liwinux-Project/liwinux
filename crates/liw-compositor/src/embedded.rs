//! Running the compositor inside another application.
//!
//! The whole Wayland side — socket, event loop, EGL context, rendering —
//! lives on a thread of its own. The host application never touches any of
//! it: it reads the newest frame out of a slot and writes the size it wants
//! into another. Those two handles are the entire surface between them, and
//! keeping it that small is what stops the compositor from leaking into a UI
//! that has no business knowing what a `wl_surface` is.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::surface::{
    render_elements_from_surface_tree, WaylandSurfaceRenderElement,
};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::desktop::utils::send_frames_surface_tree;
use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use smithay::wayland::socket::ListeningSocketSource;

use crate::headless::{Frame, FrameSlot, Headless, OFFSCREEN_TRANSFORM};
use crate::{ClientState, Compositor, GuestHandle};

/// Handles the host application holds onto.
pub struct Embedded {
    /// The newest frame, or `None` before the first one.
    pub frames: FrameSlot,
    /// What the guest is doing, for a status line.
    pub guest: GuestHandle,
    width: Arc<AtomicI32>,
    height: Arc<AtomicI32>,
    running: Arc<AtomicBool>,
}

impl Embedded {
    /// Asks for a new render size. Takes effect on the next frame.
    ///
    /// This is what makes the picture follow the window instead of being
    /// fixed at start-up.
    pub fn request_size(&self, width: i32, height: i32) {
        self.width.store(width.max(1), Ordering::Relaxed);
        self.height.store(height.max(1), Ordering::Relaxed);
    }

    pub fn requested_size(&self) -> (i32, i32) {
        (self.width.load(Ordering::Relaxed), self.height.load(Ordering::Relaxed))
    }

    /// Stops the thread. The compositor's socket goes with it, so anything
    /// still connected is disconnected.
    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }
}

/// Starts the compositor on its own thread.
///
/// Returns as soon as the socket is bound, so the caller can put
/// `WAYLAND_DISPLAY` in front of Waydroid without racing it.
pub fn spawn(socket_name: &str, width: i32, height: i32) -> Result<Embedded, String> {
    let frames: FrameSlot = Arc::new(Mutex::new(None));
    let w = Arc::new(AtomicI32::new(width.max(1)));
    let h = Arc::new(AtomicI32::new(height.max(1)));
    let running = Arc::new(AtomicBool::new(true));

    // The socket is bound HERE rather than on the thread, so a name already
    // in use is reported to the caller instead of disappearing into a log it
    // may never read.
    let socket = ListeningSocketSource::with_name(socket_name)
        .map_err(|e| format!("could not bind the socket {socket_name}: {e}"))?;

    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<GuestHandle, String>>();

    let (tf, tw, th, trunning) = (frames.clone(), w.clone(), h.clone(), running.clone());
    std::thread::Builder::new()
        .name("liw-compositor".into())
        .spawn(move || {
            match run(socket, tf, tw, th, trunning, ready_tx.clone()) {
                Ok(()) => tracing::info!("compositor thread finished"),
                Err(e) => {
                    tracing::error!(error = %e, "compositor thread failed");
                    // If it died before signalling readiness the caller is
                    // still waiting; tell it rather than leaving it blocked.
                    let _ = ready_tx.send(Err(e));
                }
            }
        })
        .map_err(|e| format!("could not start the compositor thread: {e}"))?;

    match ready_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(guest)) => Ok(Embedded { frames, guest, width: w, height: h, running }),
        Ok(Err(e)) => Err(e),
        Err(_) => Err("the compositor did not come up within ten seconds".into()),
    }
}

fn run(
    socket: ListeningSocketSource,
    frames: FrameSlot,
    want_w: Arc<AtomicI32>,
    want_h: Arc<AtomicI32>,
    running: Arc<AtomicBool>,
    ready: std::sync::mpsc::Sender<Result<GuestHandle, String>>,
) -> Result<(), String> {
    let (w0, h0) = (want_w.load(Ordering::Relaxed), want_h.load(Ordering::Relaxed));
    let mut headless = Headless::new(w0, h0)?;

    let mut event_loop: EventLoop<Compositor> =
        EventLoop::try_new().map_err(|e| format!("event loop: {e}"))?;
    let display: Display<Compositor> =
        Display::new().map_err(|e| format!("wayland display: {e}"))?;
    let dh = display.handle();

    let mut state = Compositor::new(&dh, (w0, h0), 60_000);
    let _ = ready.send(Ok(state.guest.clone()));

    let output = Output::new(
        "liw-embedded".into(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "liwinux".into(),
            model: "embedded".into(),
        },
    );
    output.change_current_state(
        Some(Mode { size: (w0, h0).into(), refresh: 60_000 }),
        Some(OFFSCREEN_TRANSFORM),
        Some(Scale::Fractional(1.0)),
        Some((0, 0).into()),
    );
    let mut damage = OutputDamageTracker::from_output(&output);

    let handle = event_loop.handle();
    handle
        .insert_source(socket, move |client, _, state: &mut Compositor| {
            match state.dh.insert_client(client, Arc::new(ClientState::default())) {
                Ok(_) => tracing::info!("guest connected"),
                Err(e) => tracing::error!(error = %e, "could not insert the guest"),
            }
        })
        .map_err(|e| format!("socket source: {e}"))?;

    handle
        .insert_source(
            smithay::reexports::calloop::generic::Generic::new(
                display,
                smithay::reexports::calloop::Interest::READ,
                smithay::reexports::calloop::Mode::Level,
            ),
            |_, display, state: &mut Compositor| {
                // SAFETY: the display is owned by this source and touched
                // only here, on this thread.
                unsafe { display.get_mut().dispatch_clients(state)? };
                Ok(smithay::reexports::calloop::PostAction::Continue)
            },
        )
        .map_err(|e| format!("display source: {e}"))?;

    let start = Instant::now();
    let mut drawn_commits = u64::MAX;
    let mut applied = (0i32, 0i32, f64::NAN);

    while running.load(Ordering::Relaxed) && state.running {
        event_loop
            .dispatch(Some(Duration::from_millis(4)), &mut state)
            .map_err(|e| format!("dispatch: {e}"))?;

        let size = (want_w.load(Ordering::Relaxed), want_h.load(Ordering::Relaxed));
        headless.resize(size.0, size.1)?;

        let (commits, guest_size) = state
            .guest
            .lock()
            .map(|g| (g.commits, g.size))
            .unwrap_or((0, None));

        // Fit the guest into whatever size the host asked for. Android does
        // not take its size from our wl_output — it reads
        // waydroid.display_width/height, set from the host display — so the
        // guest is almost always a different size from the view.
        let (gw, gh) = guest_size.unwrap_or(size);
        let (fit, _shown) = crate::headless::fit((gw, gh), size);

        if (size.0, size.1) != (applied.0, applied.1) || (fit - applied.2).abs() > 1e-9 {
            output.change_current_state(
                Some(Mode { size: size.into(), refresh: 60_000 }),
                None,
                Some(Scale::Fractional(fit)),
                None,
            );
            damage = OutputDamageTracker::from_output(&output);
            applied = (size.0, size.1, fit);
            tracing::info!(guest = ?(gw, gh), view = ?size, fit, "fitted");
        }

        let Some(surface) = state.surface.clone() else {
            state.dh.flush_clients().ok();
            continue;
        };
        // Nothing new to draw. Repainting an unchanged frame costs a full GPU
        // pass and a readback for a picture nobody asked for again.
        if commits == drawn_commits {
            state.dh.flush_clients().ok();
            continue;
        }
        drawn_commits = commits;

        let mut element_count = 0usize;
        let result = headless.capture(|renderer, fb| {
            let elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                render_elements_from_surface_tree(
                    renderer,
                    &surface,
                    (0, 0),
                    fit,
                    1.0,
                    Kind::Unspecified,
                );
            // An empty list renders a blank frame perfectly successfully. If
            // the buffer could not be imported the client is left waiting for
            // a wl_buffer.release that never comes, and the only visible
            // symptom is a guest that connects and then does nothing.
            element_count = elements.len();
            damage
                .render_output(renderer, fb, 0, &elements, [0.02, 0.02, 0.03, 1.0])
                .map(|_| ())
                .map_err(|e| format!("render: {e}"))
        });

        match result {
            Ok(frame) => {
                if element_count == 0 {
                    tracing::warn!(
                        commits,
                        "rendered nothing: the surface produced no elements"
                    );
                }
                publish(&frames, frame)
            }
            Err(e) => tracing::warn!(error = %e, "frame dropped"),
        }

        send_frames_surface_tree(&surface, &output, start.elapsed(), None, |_, _| {
            Some(output.clone())
        });
        state.dh.flush_clients().ok();
    }
    Ok(())
}

/// Leaves a frame where the host will find it.
///
/// The lock is held for the swap alone. Holding it across the render would
/// stall the UI thread on the compositor, which is exactly what putting them
/// on separate threads was meant to avoid.
fn publish(slot: &FrameSlot, frame: Frame) {
    if let Ok(mut s) = slot.lock() {
        *s = Some(frame);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_requested_size_is_read_back() {
        let e = Embedded {
            frames: Arc::new(Mutex::new(None)),
            guest: Arc::new(Mutex::new(Default::default())),
            width: Arc::new(AtomicI32::new(1)),
            height: Arc::new(AtomicI32::new(1)),
            running: Arc::new(AtomicBool::new(true)),
        };
        e.request_size(1600, 900);
        assert_eq!(e.requested_size(), (1600, 900));
    }

    /// A zero or negative size would make an invalid render target. The host
    /// can legitimately report one while a window is being laid out.
    #[test]
    fn a_size_is_never_allowed_to_reach_zero() {
        let e = Embedded {
            frames: Arc::new(Mutex::new(None)),
            guest: Arc::new(Mutex::new(Default::default())),
            width: Arc::new(AtomicI32::new(1)),
            height: Arc::new(AtomicI32::new(1)),
            running: Arc::new(AtomicBool::new(true)),
        };
        e.request_size(0, -5);
        assert_eq!(e.requested_size(), (1, 1));
    }

    #[test]
    fn stopping_is_visible_to_the_thread() {
        let e = Embedded {
            frames: Arc::new(Mutex::new(None)),
            guest: Arc::new(Mutex::new(Default::default())),
            width: Arc::new(AtomicI32::new(1)),
            height: Arc::new(AtomicI32::new(1)),
            running: Arc::new(AtomicBool::new(true)),
        };
        assert!(e.is_running());
        e.stop();
        assert!(!e.is_running());
    }

    /// Publishing replaces rather than queues: the host wants the current
    /// picture, not a backlog.
    #[test]
    fn publishing_replaces_the_previous_frame() {
        let slot: FrameSlot = Arc::new(Mutex::new(None));
        publish(&slot, Frame { width: 1, height: 1, bgra: vec![0; 4], serial: 1 });
        publish(&slot, Frame { width: 2, height: 2, bgra: vec![0; 16], serial: 2 });
        let g = slot.lock().unwrap();
        assert_eq!(g.as_ref().unwrap().serial, 2);
        assert_eq!(g.as_ref().unwrap().width, 2);
    }

    /// The placeholder guard is what keeps a 1x1 surface from producing an
    /// absurd output scale. Android stopped booting when it did.
    #[test]
    fn a_placeholder_surface_does_not_drive_the_scale() {
        assert_eq!(crate::headless::fit((1, 1), (1280, 720)).0, 1.0,
                   "1x1 must not scale");
        assert_eq!(crate::headless::fit((2560, 1440), (1280, 720)).0, 0.5,
                   "a real screen must");
    }
}
