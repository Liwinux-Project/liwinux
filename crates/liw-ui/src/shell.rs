//! Root view: header strip, sidebar, content.

use gpui::{Context, IntoElement, Render, SharedString, Window, div, prelude::*, px};

use crate::library;
use crate::state::{AppState, Link, Nav};
use crate::theme::{Theme, HEADER_H, RADIUS, S1, S2, S3, S4, S6, SIDEBAR_W};

impl Render for AppState {
    fn render(&mut self, _w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::dark();
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(t.bg)
            .text_color(t.text)
            .font_family("sans-serif")
            .child(header(self, &t, cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .child(sidebar(self, &t, cx))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .p(px(S6))
                            .gap(px(S3))
                            .when_some(self.error.clone(), |el, e| el.child(banner(&t, &e)))
                            .child(content(self, &t, cx)),
                    ),
            )
    }
}

fn header(s: &AppState, t: &Theme, cx: &mut Context<AppState>) -> impl IntoElement {
    div()
        .h(px(HEADER_H))
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .px(px(S4))
        .bg(t.surface)
        .border_b_1()
        .border_color(t.border)
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(S2))
                .child(div().text_size(px(15.0)).text_color(t.text).child("liwinux"))
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(t.text_faint)
                        .child("Android games on Linux"),
                ),
        )
        .child(status(s, t, cx))
}

/// The live status strip.
///
/// Every pill here is a daemon property with a change signal, so this redraws
/// when the fact changes rather than on a timer.
fn status(s: &AppState, t: &Theme, cx: &mut Context<AppState>) -> impl IntoElement {
    let (session_label, session_colour, running) = match &s.link {
        Link::Connecting => ("connecting…", t.text_faint, false),
        Link::Down(_) => ("liwd not running", t.bad, false),
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
        .when(s.snapshot.keymapper_running, |el| {
            el.child(pill(
                t,
                if s.snapshot.game_mode { "game mode" } else { "mapping idle" },
                if s.snapshot.game_mode { t.accent } else { t.text_faint },
            ))
        })
        .when_some(s.snapshot.active_profile.clone(), |el, p| {
            el.child(pill(t, &p, t.text_muted))
        })
        .child(pill(t, session_label, session_colour))
        .when(matches!(s.link, Link::Up), |el| {
            el.child(
                div()
                    .id("session-toggle")
                    .px(px(S3))
                    .py(px(S1 + 1.0))
                    .rounded(px(RADIUS))
                    .bg(if busy { t.surface } else { t.raised })
                    .border_1()
                    .border_color(t.border)
                    .text_size(px(12.0))
                    .text_color(if busy { t.text_faint } else { t.text })
                    .when(!busy, |e| e.cursor_pointer().hover(|x| x.border_color(t.accent)))
                    .child(if busy {
                        SharedString::from("working…")
                    } else if running {
                        SharedString::from("Stop")
                    } else {
                        SharedString::from("Start Android")
                    })
                    .when(!busy, |e| {
                        e.on_click(cx.listener(move |st, _, _, cx| {
                            st.session(!running, cx);
                        }))
                    }),
            )
        })
}

fn pill(t: &Theme, text: &str, colour: gpui::Hsla) -> impl IntoElement {
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
}

fn sidebar(s: &AppState, t: &Theme, cx: &mut Context<AppState>) -> impl IntoElement {
    div()
        .w(px(SIDEBAR_W))
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(S1))
        .p(px(S2))
        .bg(t.surface)
        .border_r_1()
        .border_color(t.border)
        .children(Nav::ALL.into_iter().map(|n| {
            let on = s.nav == n;
            div()
                .id(SharedString::from(n.label()))
                .px(px(S3))
                .py(px(S2))
                .rounded(px(RADIUS))
                .text_size(px(13.0))
                .bg(if on { t.raised } else { t.surface })
                .text_color(if on { t.text } else { t.text_muted })
                .cursor_pointer()
                .hover(|x| x.bg(t.raised))
                .child(n.label())
                .on_click(cx.listener(move |st, _, _, cx| {
                    st.nav = n;
                    cx.notify();
                }))
        }))
}

fn banner(t: &Theme, msg: &str) -> impl IntoElement {
    div()
        .p(px(S3))
        .rounded(px(RADIUS))
        .bg(t.bad.opacity(0.12))
        .border_1()
        .border_color(t.bad.opacity(0.4))
        .text_size(px(12.0))
        .text_color(t.text)
        .child(SharedString::from(msg.to_string()))
}

fn content(s: &AppState, t: &Theme, cx: &mut Context<AppState>) -> gpui::AnyElement {
    if let Link::Down(why) = &s.link {
        return offline(t, why).into_any_element();
    }
    match s.nav {
        Nav::Library => library::render(s, t, cx).into_any_element(),
        other => placeholder(t, other).into_any_element(),
    }
}

/// liwd unreachable. Says which thing is missing and what to run — a blank
/// window would leave the user restarting the wrong service.
fn offline(t: &Theme, why: &str) -> impl IntoElement {
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
}

fn placeholder(t: &Theme, n: Nav) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .size_full()
        .gap(px(S2))
        .child(div().text_size(px(15.0)).text_color(t.text_muted).child(n.label()))
        .child(div().text_size(px(12.0)).text_color(t.text_faint).child("Not built yet."))
}
