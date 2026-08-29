//! `liw-compositor` — run the nested compositor on its own and see what
//! Waydroid does with it.
//!
//! This binary exists so the Wayland half can be proved before it is joined to
//! the renderer. It hosts the socket, accepts the client, and prints what it
//! sees. It draws nothing: the point of running it is to find out whether
//! hwcomposer connects, gets its configure, and commits frames — three
//! questions that have nothing to do with drawing and everything to do with
//! whether the rest is worth building.
//!
//! ```text
//! liw-compositor --socket wayland-liw &
//! WAYLAND_DISPLAY=wayland-liw waydroid session start
//! ```

use std::time::Duration;

use anyhow::{Context, Result};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use smithay::wayland::socket::ListeningSocketSource;

use liw_compositor::{ClientState, Compositor};

/// Everything the binary takes, kept as one struct so main reads top-down.
struct Args {
    socket: String,
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
            "-h" | "--help" => {
                println!("liw-compositor [--socket NAME] [--width N] [--height N] \
                          [--refresh mHz]");
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

    let mut event_loop: EventLoop<Compositor> =
        EventLoop::try_new().context("could not create the event loop")?;
    let display: Display<Compositor> =
        Display::new().context("could not create the wayland display")?;
    let dh = display.handle();

    let mut state = Compositor::new(&dh, (args.width, args.height), args.refresh);

    // Bind the socket by NAME rather than letting it pick one. The whole
    // mechanism depends on pointing Waydroid at this exact socket through
    // WAYLAND_DISPLAY, and a name chosen for us would have to be read back
    // out of the log before anything could connect to it.
    let socket = ListeningSocketSource::with_name(&args.socket)
        .with_context(|| format!("could not bind the socket {}", args.socket))?;

    let handle = event_loop.handle();
    handle
        .insert_source(socket, move |client, _, state: &mut Compositor| {
            match state.dh.insert_client(client, std::sync::Arc::new(ClientState::default())) {
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
                // SAFETY: the display is owned by this source and is only
                // touched here, on the event loop thread.
                unsafe { display.get_mut().dispatch_clients(state)? };
                Ok(smithay::reexports::calloop::PostAction::Continue)
            },
        )
        .map_err(|e| anyhow::anyhow!("could not poll the display: {e}"))?;

    tracing::info!(
        socket = %args.socket,
        size = ?(args.width, args.height),
        "ready — point waydroid at it with WAYLAND_DISPLAY={}",
        args.socket
    );

    let guest = state.guest.clone();
    let mut last = String::new();

    while state.running {
        event_loop
            .dispatch(Some(Duration::from_millis(200)), &mut state)
            .context("event loop failed")?;

        // Report only when something CHANGED. A line per tick would bury the
        // one moment that matters — the first commit — in a scrolling wall.
        if let Ok(g) = guest.lock() {
            let now = format!(
                "connected={} size={:?} commits={} buffer={:?}",
                g.connected, g.size, g.commits, g.buffer
            );
            if now != last {
                tracing::info!("{now}");
                last = now;
            }
        }

        state.dh.flush_clients().ok();
    }
    Ok(())
}
