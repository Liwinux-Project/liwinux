//! Root view: a top navigation strip that doubles as the titlebar, then content.
//!
//! # Why the nav is on top
//!
//! A left sidebar reads as a settings dialog; a horizontal strip with the mark,
//! the sections and a search field reads as an application. It also lets the
//! strip BE the titlebar, which is most of the difference between "a window"
//! and "a product".

use gpui::{
    Context, Decorations, IntoElement, Render, SharedString, Window, WindowControlArea,
    div, prelude::*, px,
};

use crate::library;
use crate::state::{AppState, Link, Nav};
use crate::theme::{Theme, HEADER_H, RADIUS, S1, S2, S3, S4, S6};

impl Render for AppState {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::dark();
        let body = content(self, &t, window, cx);
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(t.bg)
            .text_color(t.text)
            .font_family("sans-serif")
            .child(nav(self, &t, window, cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .px(px(S6))
                    .py(px(S4))
                    .gap(px(S3))
                    .when_some(self.error.clone(), |el, e| el.child(banner(&t, &e)))
                    .child(body),
            )
    }
}

/// Mark, sections, search, live status, window controls — one strip.
fn nav(
    s: &AppState, t: &Theme, window: &mut Window, cx: &mut Context<AppState>,
) -> gpui::AnyElement {
    let tabs: Vec<gpui::AnyElement> = Nav::ALL.into_iter().map(|n| tab(s, t, n, cx)).collect();
    let status = status(s, t, cx);
    let caption = caption(t, window);

    div()
        .h(px(HEADER_H))
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(S4))
        .pl(px(S4))
        .pr(px(S2))
        .bg(t.surface)
        .border_b_1()
        .border_color(t.border)
        // Dragging the strip moves the window, the way every real titlebar
        // behaves. Without this a client-side-decorated window cannot be
        // moved at all.
        .window_control_area(WindowControlArea::Drag)
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(S2))
                .child(
                    div()
                        .w(px(20.0))
                        .h(px(20.0))
                        .rounded(px(6.0))
                        .bg(t.accent)
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(11.0))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(t.bg)
                        .child("L"),
                )
                .child(
                    div()
                        .text_size(px(14.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("liwinux"),
                ),
        )
        .child(div().flex().flex_row().items_center().gap(px(S1)).children(tabs))
        .child(search(s, t, cx))
        .child(div().flex_1())
        .child(status)
        .when_some(caption, |el, c| el.child(c))
        .into_any_element()
}

fn tab(s: &AppState, t: &Theme, n: Nav, cx: &mut Context<AppState>) -> gpui::AnyElement {
    let on = s.nav == n;
    div()
        .id(SharedString::from(n.label()))
        .px(px(S3))
        .py(px(S1 + 2.0))
        .rounded(px(RADIUS - 2.0))
        .text_size(px(13.0))
        .font_weight(if on { gpui::FontWeight::SEMIBOLD } else { gpui::FontWeight::NORMAL })
        .text_color(if on { t.text } else { t.text_muted })
        .when(on, |e| e.bg(t.raised))
        .cursor_pointer()
        .hover(|x| x.text_color(t.text))
        .child(n.label())
        .on_click(cx.listener(move |st, _, _, cx| {
            st.nav = n;
            cx.notify();
        }))
        .into_any_element()
}

/// Search field.
///
/// Not a real text input yet: gpui's editor lives in Zed's workspace crates,
/// not in gpui itself, so a proper one is its own piece of work. Typing is
/// handled as key events on the focused strip, which covers filtering — the
/// only thing search does here.
fn search(s: &AppState, t: &Theme, cx: &mut Context<AppState>) -> gpui::AnyElement {
    let q = s.search.clone();
    let has = !q.is_empty();
    div()
        .id("search")
        .w(px(190.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(S2))
        .px(px(S3))
        .py(px(S1 + 1.0))
        .rounded(px(RADIUS))
        .bg(t.bg)
        .border_1()
        .border_color(t.border)
        .text_size(px(12.0))
        .text_color(if has { t.text } else { t.text_faint })
        .child(div().text_color(t.text_faint).child("⌕"))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(if has { SharedString::from(q) } else { SharedString::from("Search") }),
        )
        .when(has, |el| {
            el.child(
                div()
                    .id("search-clear")
                    .text_color(t.text_faint)
                    .cursor_pointer()
                    .hover(|x| x.text_color(t.text))
                    .child("×")
                    .on_click(cx.listener(|st, _, _, cx| {
                        st.search.clear();
                        cx.notify();
                    })),
            )
        })
        .into_any_element()
}

/// Live status. Every pill is a daemon property with a change signal.
fn status(s: &AppState, t: &Theme, cx: &mut Context<AppState>) -> gpui::AnyElement {
    let (label, colour, running) = match &s.link {
        Link::Connecting => ("connecting…", t.text_faint, false),
        Link::Down(_) => ("liwd offline", t.bad, false),
        Link::Up => match s.snapshot.state.as_str() {
            "RUNNING" => ("Android running", t.ok, true),
            "DEGRADED" => ("degraded", t.warn, true),
            "RECOVERING" => ("recovering…", t.warn, true),
            _ => ("Android stopped", t.text_faint, false),
        },
    };
    let busy = s.busy.is_some();

    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(S2))
        .when_some(latency(s, t), |el, l| el.child(l))
        .when(s.snapshot.game_mode, |el| el.child(pill(t, "game mode", t.accent)))
        .child(pill(t, label, colour))
        .when(matches!(s.link, Link::Up), |el| {
            el.child(
                div()
                    .id("session-toggle")
                    .px(px(S3))
                    .py(px(S1 + 1.0))
                    .rounded(px(RADIUS))
                    .border_1()
                    .border_color(t.border)
                    .text_size(px(12.0))
                    .text_color(if busy { t.text_faint } else { t.text })
                    .when(!busy, |e| {
                        e.cursor_pointer().hover(|x| x.bg(t.raised).border_color(t.accent))
                    })
                    .child(if busy {
                        SharedString::from("…")
                    } else if running {
                        SharedString::from("Stop")
                    } else {
                        SharedString::from("Start Android")
                    })
                    .when(!busy, |e| {
                        e.on_click(cx.listener(move |st, _, _, cx| st.session(!running, cx)))
                    }),
            )
        })
        .into_any_element()
}

/// Our own layer's latency, shown only once it has been measured.
///
/// This is the number liwinux exists to keep small, and it is the one thing
/// no other launcher will tell you. It is hidden rather than shown as zero
/// when there are no samples: "0.00 ms" reads as a measurement, and a
/// measurement that has not happened is not one.
fn latency(s: &AppState, t: &Theme) -> Option<gpui::AnyElement> {
    if s.snapshot.latency_samples == 0 { return None }
    let p50 = s.snapshot.latency_p50_us as f32 / 1000.0;
    let p99 = s.snapshot.latency_p99_us as f32 / 1000.0;
    // Thresholds against the frame budget rather than against a feeling: a
    // millisecond is nothing at 60 Hz and a third of the budget at 240.
    let colour = if p99 < 2.0 { t.ok } else if p99 < 6.0 { t.warn } else { t.bad };
    Some(
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(S1 + 2.0))
            .px(px(S2 + 2.0))
            .py(px(S1))
            .rounded(px(RADIUS))
            .bg(t.raised)
            .child(div().w(px(6.0)).h(px(6.0)).rounded(px(3.0)).bg(colour))
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(t.text_muted)
                    .child(SharedString::from(format!("input {p50:.1} / {p99:.1} ms"))),
            )
            .into_any_element(),
    )
}

fn pill(t: &Theme, text: &str, colour: gpui::Hsla) -> gpui::AnyElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(S1 + 2.0))
        .px(px(S2 + 2.0))
        .py(px(S1))
        .rounded(px(RADIUS))
        .bg(t.raised)
        .child(div().w(px(6.0)).h(px(6.0)).rounded(px(3.0)).bg(colour))
        .child(
            div()
                .text_size(px(11.0))
                .text_color(t.text_muted)
                .child(SharedString::from(text.to_string())),
        )
        .into_any_element()
}

/// Minimise / maximise / close — drawn ONLY under client-side decorations.
///
/// Under CSD (the Wayland default) nobody else draws them, and a window with
/// no way to close it is a bug you cannot recover from without a terminal.
/// Under server-side decorations the compositor already drew real ones, so
/// adding our own would give the window two sets.
fn caption(t: &Theme, window: &mut Window) -> Option<gpui::AnyElement> {
    if !matches!(window.window_decorations(), Decorations::Client { .. }) {
        return None;
    }
    let btn = |id: &'static str, glyph: &'static str, area: WindowControlArea, danger: bool| {
        div()
            .id(id)
            .w(px(34.0))
            .h(px(28.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.0))
            .text_size(px(12.0))
            .text_color(t.text_muted)
            .cursor_pointer()
            .window_control_area(area)
            .hover(|x| if danger { x.bg(t.bad).text_color(t.text) } else { x.bg(t.raised) })
            .child(glyph)
    };
    Some(
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.0))
            .ml(px(S2))
            .child(btn("win-min", "—", WindowControlArea::Min, false))
            .child(btn("win-max", "▢", WindowControlArea::Max, false))
            .child(btn("win-close", "✕", WindowControlArea::Close, true))
            .into_any_element(),
    )
}

fn banner(t: &Theme, msg: &str) -> gpui::AnyElement {
    div()
        .p(px(S3))
        .rounded(px(RADIUS))
        .bg(t.bad.opacity(0.12))
        .border_1()
        .border_color(t.bad.opacity(0.4))
        .text_size(px(12.0))
        .text_color(t.text)
        .child(SharedString::from(msg.to_string()))
        .into_any_element()
}

fn content(
    s: &AppState, t: &Theme, window: &Window, cx: &mut Context<AppState>,
) -> gpui::AnyElement {
    if let Link::Down(why) = &s.link {
        return offline(t, why);
    }
    match s.nav {
        Nav::Library => library::render(s, t, window, cx),
        Nav::Keymap => crate::keymap::render(s, t, cx),
        other => placeholder(t, other),
    }
}

/// liwd unreachable. Names the service and the command — a blank window has
/// people restarting the wrong thing.
fn offline(t: &Theme, why: &str) -> gpui::AnyElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .size_full()
        .gap(px(S2))
        .child(div().text_size(px(15.0)).text_color(t.text).child("liwd is not reachable"))
        .child(
            div()
                .text_size(px(12.0))
                .text_color(t.text_faint)
                .child(SharedString::from(why.to_string())),
        )
        .child(
            div()
                .mt(px(S2))
                .px(px(S3))
                .py(px(S2))
                .rounded(px(RADIUS))
                .bg(t.raised)
                .text_size(px(12.0))
                .text_color(t.text_muted)
                .child("systemctl --user start liwd"),
        )
        .into_any_element()
}

fn placeholder(t: &Theme, n: Nav) -> gpui::AnyElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .size_full()
        .gap(px(S2))
        .child(div().text_size(px(15.0)).text_color(t.text_muted).child(n.label()))
        .child(div().text_size(px(12.0)).text_color(t.text_faint).child("Not built yet."))
        .into_any_element()
}
