//! `liw-compositor` — host Waydroid's surface and draw it in a window.
//!
//! This binary opens a window, hosts the Wayland socket Waydroid connects to,
//! and paints the guest's frames into that window. It is the standalone half
//! of the embedded path: everything except the join to gpui.
//!
//! ```text
//! liw-compositor --socket wayland-liw &
//! WAYLAND_DISPLAY=wayland-liw waydroid session start
//! WAYLAND_DISPLAY=wayland-liw waydroid show-full-ui
//! ```
//!
//! `liwd` must be stopped while this runs. Its supervisor sees "Android boot
//! did not complete" during the guest's start, restarts the session, and the
//! restart carries liwd's own environment — which points back at the desktop
//! compositor and ends the experiment.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::{surface::render_elements_from_surface_tree, Kind};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::{ExportMem, Renderer};
use smithay::backend::winit::{self, WinitEvent};
use smithay::reexports::winit::window::WindowAttributes;
use smithay::desktop::utils::send_frames_surface_tree;
use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use smithay::utils::{Rectangle, Transform};
use smithay::wayland::socket::ListeningSocketSource;

use liw_compositor::{ClientState, Compositor};

struct Args {
    socket: String,
    /// Use the headless renderer and no window at all.
    ///
    /// This exists to bisect one specific failure: Android boots against the
    /// windowed compositor and does not boot against the embedded one. The
    /// two differ in the renderer (winit's window-backed EGL against a
    /// headless EGLDevice) and in the process they live in (a compositor
    /// alone against a gpui application). This flag changes only the first,
    /// so a run says which of the two matters instead of leaving it to a
    /// guess.
    headless: bool,
    /// Time a full-frame CPU readback each frame.
    ///
    /// This is a measurement, not a feature. gpui has no external-texture
    /// path on Linux — its `surface` element is macOS-only — so putting the
    /// guest inside the UI means either patching the pinned Zed fork or
    /// reading each frame back and handing gpui the bytes. The second costs
    /// something; this says how much before anything is built on it.
    readback: bool,
    width: i32,
    height: i32,
    /// Refresh in mHz, the unit wl_output uses. 60 Hz is 60_000.
    refresh: i32,
}

fn parse_args() -> Result<Args> {
    let mut a = Args {
        socket: "wayland-liw".into(),
        width: 1280,
        height: 720,
        refresh: 60_000,
        readback: false,
        headless: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = |name: &str| -> Result<String> {
            it.next().with_context(|| format!("{name} needs a value"))
        };
        match arg.as_str() {
            "--socket" => a.socket = value("--socket")?,
            "--width" => a.width = value("--width")?.parse()?,
            "--height" => a.height = value("--height")?.parse()?,
            "--refresh" => a.refresh = value("--refresh")?.parse()?,
            "--readback" => a.readback = true,
            "--headless" => a.headless = true,
            "-h" | "--help" => {
                println!(
                    "liw-compositor [--socket NAME] [--width N] [--height N] \
                     [--refresh mHz] [--readback]"
                );
                std::process::exit(0);
            }
            other => anyhow::bail!("unknown argument: {other}"),
        }
    }
    anyhow::ensure!(a.width > 0 && a.height > 0, "width and height must be positive");
    Ok(a)
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = parse_args()?;

    if args.headless {
        return run_headless(&args);
    }

    // The window comes first. If there is nowhere to draw, there is no point
    // accepting a client that expects its frames to go somewhere.
    let (mut backend, mut winit_loop) = winit::init_from_attributes::<GlesRenderer>(
        WindowAttributes::default()
            .with_title("liwinux — Android")
            .with_inner_size(smithay::reexports::winit::dpi::LogicalSize::new(
                args.width as f64,
                args.height as f64,
            )),
    )
    .map_err(|e| anyhow::anyhow!("could not open a window: {e}"))?;

    let mut event_loop: EventLoop<Compositor> =
        EventLoop::try_new().context("could not create the event loop")?;
    let display: Display<Compositor> =
        Display::new().context("could not create the wayland display")?;
    let dh = display.handle();

    let mut state = Compositor::new(&dh, (args.width, args.height), args.refresh);

    // A second output object, matching the window rather than the advertised
    // mode, so damage is tracked against what is actually on screen.
    let win_size = backend.window_size();
    let output = Output::new(
        "liw-window".into(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "liwinux".into(),
            model: "window".into(),
        },
    );
    output.change_current_state(
        Some(Mode { size: win_size, refresh: args.refresh }),
        Some(Transform::Flipped180),
        None,
        Some((0, 0).into()),
    );
    let mut damage = OutputDamageTracker::from_output(&output);

    // Bind the socket by NAME. The whole mechanism depends on pointing
    // Waydroid at this exact socket through WAYLAND_DISPLAY.
    let socket = ListeningSocketSource::with_name(&args.socket)
        .with_context(|| format!("could not bind the socket {}", args.socket))?;

    let handle = event_loop.handle();
    handle
        .insert_source(socket, move |client, _, state: &mut Compositor| {
            match state
                .dh
                .insert_client(client, std::sync::Arc::new(ClientState::default()))
            {
                Ok(_) => tracing::info!("client connected"),
                Err(e) => tracing::error!(error = %e, "could not insert the client"),
            }
        })
        .map_err(|e| anyhow::anyhow!("could not listen on the socket: {e}"))?;

    handle
        .insert_source(
            smithay::reexports::calloop::generic::Generic::new(
                display,
                smithay::reexports::calloop::Interest::READ,
                smithay::reexports::calloop::Mode::Level,
            ),
            |_, display, state: &mut Compositor| {
                // SAFETY: the display is owned by this source and only touched
                // here, on the event loop thread.
                unsafe { display.get_mut().dispatch_clients(state)? };
                Ok(smithay::reexports::calloop::PostAction::Continue)
            },
        )
        .map_err(|e| anyhow::anyhow!("could not poll the display: {e}"))?;

    tracing::info!(
        socket = %args.socket,
        window = ?win_size,
        "ready — point waydroid at it with WAYLAND_DISPLAY={}",
        args.socket
    );

    let start = Instant::now();
    let mut painted = 0u64;
    let mut last_report = Instant::now();
    // What was on screen last time we painted. Repainting an unchanged frame
    // costs a full GPU pass for nothing: the first version ran at 180 fps
    // against a static launcher that had stopped committing at 133.
    let mut drawn_commits = u64::MAX;
    let mut drawn_size = (0, 0);
    let mut applied_scale = f64::NAN;
    let mut applied_size = (0, 0);
    let mut readback_ns = 0u64;
    let mut readback_n = 0u64;
    let mut readback_bytes = 0usize;

    while state.running {
        // 1. Window events. A close request is the only one that matters here;
        //    resizing is handled by re-reading the size each frame.
        let status = winit_loop.dispatch_new_events(|event| {
            if let WinitEvent::CloseRequested = event {
                tracing::info!("window closed");
            }
        });
        if let smithay::reexports::winit::platform::pump_events::PumpStatus::Exit(_) = status {
            break;
        }

        // 2. Anything the guest sent.
        event_loop
            .dispatch(Some(Duration::from_millis(4)), &mut state)
            .context("event loop failed")?;

        // 3. Paint. The guest's dmabuf becomes a GLES texture here; that
        //    import is the step the whole embedded path rests on.
        let size = backend.window_size();
        let (commits, guest_size) = state
            .guest
            .lock()
            .map(|g| (g.commits, g.size))
            .unwrap_or((0, None));
        let resized = (size.w, size.h) != drawn_size;
        let fresh = commits != drawn_commits;

        // Fit the guest into the window.
        //
        // The scale passed to render_elements_from_surface_tree only moves the
        // element; its SIZE comes from the OUTPUT's scale, which the damage
        // tracker reads at render time. Passing 0.5 to the element function
        // and expecting a half-size picture is the mistake that left the
        // game's buttons running off the right edge for two runs.
        //
        // Android ignores our wl_output mode and sizes itself from
        // waydroid.display_width/height, taken from the host display, so a
        // 2560x1440 guest in a 1280x720 window is the normal case, not an
        // exception.
        // A guest smaller than this is not a screen. The session manager
        // attaches a 1x1 placeholder before Android is up, and fitting to it
        // gives a scale of 1280 — which is then advertised to the client as
        // its output scale. That is not a rounding error, it is nonsense sent
        // over the wire, and it stopped Android booting at all.
        const REAL_SCREEN: i32 = 64;
        let (gw, gh) = guest_size.unwrap_or((size.w, size.h));
        let fit = if gw >= REAL_SCREEN && gh >= REAL_SCREEN {
            (size.w as f64 / gw as f64).min(size.h as f64 / gh as f64)
        } else {
            1.0
        };

        // Only touch the output when something it describes actually changed.
        // The first version also fired on `resized`, which was true on every
        // pass because the size it compared against is only updated when a
        // frame is painted — so the client was sent a fresh wl_output state
        // hundreds of times a second.
        if (fit - applied_scale).abs() > 1e-9 || (size.w, size.h) != applied_size {
            output.change_current_state(
                Some(Mode { size, refresh: args.refresh }),
                None,
                Some(Scale::Fractional(fit)),
                None,
            );
            damage = OutputDamageTracker::from_output(&output);
            applied_scale = fit;
            applied_size = (size.w, size.h);
            tracing::info!(guest = ?(gw, gh), window = ?(size.w, size.h), fit, "fitted");
        }

        if let Some(surface) = state.surface.clone().filter(|_| fresh || resized) {
            drawn_commits = commits;
            drawn_size = (size.w, size.h);
            let (renderer, mut fb) = backend
                .bind()
                .map_err(|e| anyhow::anyhow!("could not bind the window: {e}"))?;

            let elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                render_elements_from_surface_tree(
                    renderer,
                    &surface,
                    (0, 0),
                    fit,
                    1.0,
                    Kind::Unspecified,
                );

            let res = damage.render_output(renderer, &mut fb, 0, &elements, [0.05, 0.05, 0.07, 1.0]);

            // Ask for the copy while the framebuffer is still bound, but do
            // NOT wait for it here. Waiting between the render and the swap
            // left EGL with a different current draw surface and the context
            // was lost on the next present — the whole compositor exited.
            let t0 = Instant::now();
            let mapping = if args.readback {
                let region = Rectangle::from_size((size.w, size.h).into());
                renderer
                    .copy_framebuffer(&fb, region, smithay::backend::allocator::Fourcc::Abgr8888)
                    .map_err(|e| tracing::warn!(error = %e, "readback failed"))
                    .ok()
            } else {
                None
            };

            drop(fb);
            match res {
                Ok(res) => {
                    let full = [Rectangle::from_size(size)];
                    backend
                        .submit(res.damage.map(|d| d.as_slice()).or(Some(&full)))
                        .map_err(|e| anyhow::anyhow!("could not present: {e}"))?;
                    painted += 1;

                    // The wait happens here, after the frame is on screen, so
                    // what is measured is the cost a CPU consumer would add
                    // rather than a stall inserted into the middle of drawing.
                    if let Some(m) = mapping {
                        match backend.renderer().map_texture(&m) {
                            Ok(bytes) => {
                                readback_ns += t0.elapsed().as_nanos() as u64;
                                readback_n += 1;
                                readback_bytes = bytes.len();
                            }
                            Err(e) => tracing::warn!(error = %e, "map failed"),
                        }
                    }
                }
                Err(e) => tracing::warn!(error = %e, "render failed"),
            }

            // 4. Tell the guest it may draw the next frame. Without this it
            //    paints once and then waits forever for permission.
            send_frames_surface_tree(
                &surface,
                &output,
                start.elapsed(),
                None,
                |_, _| Some(output.clone()),
            );
        }

        state.dh.flush_clients().ok();

        if last_report.elapsed() >= Duration::from_secs(5) {
            let g = state.guest.lock().ok().map(|g| {
                (g.connected, g.size, g.commits, g.buffer.clone())
            });
            if let Some((connected, gsize, commits, buffer)) = g {
                tracing::info!(
                    connected, ?gsize, commits, ?buffer, painted,
                    fps = format!("{:.1}", painted as f64 / start.elapsed().as_secs_f64()),
                    "state"
                );
            }
            if readback_n > 0 {
                let ms = readback_ns as f64 / readback_n as f64 / 1e6;
                tracing::info!(
                    frames = readback_n,
                    mean_ms = format!("{ms:.2}"),
                    mib = format!("{:.1}", readback_bytes as f64 / (1024.0 * 1024.0)),
                    budget_pct = format!("{:.0}", ms / 16.67 * 100.0),
                    "readback"
                );
                readback_ns = 0;
                readback_n = 0;
            }
            last_report = Instant::now();
        }
    }
    Ok(())
}

/// Runs exactly what `liw-ui` runs, with no window and no gpui around it.
///
/// If Android boots here, the renderer is not what breaks the embedded path
/// and the gpui process is. If it does not, the renderer is.
fn run_headless(args: &Args) -> Result<()> {
    let embedded = liw_compositor::spawn(&args.socket, args.width, args.height)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    tracing::info!(
        socket = %args.socket,
        size = ?(args.width, args.height),
        "headless — point waydroid at it with WAYLAND_DISPLAY={}",
        args.socket
    );

    let mut last = String::new();
    while embedded.is_running() {
        std::thread::sleep(Duration::from_millis(500));
        let frame = embedded
            .frames
            .lock()
            .ok()
            .and_then(|f| f.as_ref().map(|f| (f.width, f.height, f.serial)));
        let g = embedded.guest.lock().ok().map(|g| {
            (g.connected, g.size, g.commits, g.buffer.clone())
        });
        let now = format!("{g:?} frame={frame:?}");
        if now != last {
            tracing::info!("{now}");
            last = now;
        }
    }
    Ok(())
}
