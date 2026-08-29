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
mod shell;
mod state;
mod store;
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
                ..Default::default()
            },
            |_, _| state,
        )
        .expect("could not open the window");
        cx.activate(true);
    });
}
