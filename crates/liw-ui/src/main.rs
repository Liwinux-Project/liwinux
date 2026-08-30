//! liw-ui — the liwinux desktop front end.
//!
//! # Not in the render path
//!
//! This window never sits on top of the game. KWin can hand a fullscreen
//! window straight to the display; any always-on-top overlay closes that path
//! and forces full composition, which costs frames in the one place they
//! matter. So: a normal window, and it stays out of the way while you play.
//!
//! It is also disposable. All state lives in `liwd`, so closing this changes
//! nothing about a running session — the daemon is the product, this is a
//! viewport onto it.

mod library;
mod tint;
mod android;
mod keys;
mod game;
mod mapper;
mod touch;
mod shell;
mod diagnostics;
mod keymap;
mod settings;
mod state;
mod theme;

use gpui::{App, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("LIW_UI_LOG").unwrap_or_else(|_| "warn".into()))
        .init();

    // Pinned-rev API: the platform crate builds the application, there is no
    // `Application::new()`.
    gpui_platform::application().run(|cx: &mut App| {
        gpui_tokio::init(cx);
        let bounds = Bounds::centered(None, size(px(1120.), px(720.)), cx);
        let state = state::build(cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(880.), px(560.))),
                // The nav strip IS the titlebar: transparent so the system
                // does not draw a second one above ours. Under CSD we draw
                // our own caption buttons (see `shell::caption`); under SSD
                // the compositor still draws real ones.
                titlebar: Some(TitlebarOptions {
                    title: Some("liwinux".into()),
                    appears_transparent: true,
                    traffic_light_position: None,
                }),
                // Wayland hands a window no picture: the compositor looks
                // the app_id up in the desktop entries and takes the icon
                // from there. This must match StartupWMClass in
                // dist/desktop/liwinux.desktop.
                app_id: Some("liwinux".into()),
                ..Default::default()
            },
            |_, _| state,
        )
        .expect("could not open the window");

        // `liw-ui --game <package>` opens the game window straight away.
        // Useful on its own, and it is the only way to reach that window
        // without a mouse — which matters when the point is to test it.
        let mut args = std::env::args().skip(1);
        while let Some(a) = args.next() {
            if a == "--game" {
                if let Some(pkg) = args.next() {
                    let title = pkg.clone();
                    if let Err(e) = game::open(pkg, title, None, cx) {
                        tracing::error!(error = %e, "could not open the game window");
                    }
                }
            }
        }
        // With no windows there is nothing left to do and nothing to show.
        // gpui does not assume that — an app can legitimately live in a tray —
        // so it has to be said. Without it, closing the launcher leaves a
        // resident process and the next start from KDE adds another beside it.
        cx.on_window_closed(|cx, _id| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        cx.activate(true);
    });
}
