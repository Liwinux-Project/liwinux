//! Diagnostics: what is actually wrong, and what to do about it.
//!
//! # Why this page carries weight
//!
//! Every launcher can start a game. None of them tell you why it stuttered.
//! liwinux measures the parts nobody else does — each health signal
//! separately rather than one "working / not working" light, and the latency
//! of its own injection layer — so this page shows those instead of a spinner
//! and a hope.
//!
//! What it does NOT do is pretend to be `liw trace`. Correlating a stutter
//! against Android's log needs a capture over time; this page is the live
//! view, and it says where the deeper tool is.

use gpui::{Context, IntoElement, SharedString, div, prelude::*, px};
use liw_core::session::Health;

use crate::state::{AppState, Link};
use crate::theme::{Theme, RADIUS, S1, S2, S3, S4, S6};

pub fn render(s: &AppState, t: &Theme, cx: &mut Context<AppState>) -> gpui::AnyElement {
    div()
        .id("diagnostics")
        .flex()
        .flex_col()
        .size_full()
        .gap(px(S4))
        .overflow_y_scroll()
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(S1))
                .child(
                    div()
                        .text_size(px(18.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("Diagnostics"),
                )
                .child(
                    div().text_size(px(12.0)).text_color(t.text_faint).child(
                        "Each signal separately. \"Not working\" on its own is \
                         not enough to fix anything.",
                    ),
                ),
        )
        .child(signals(s, t))
        .child(latency_panel(s, t))
        .child(actions(s, t, cx))
        .child(deeper(t))
        .into_any_element()
}

/// The health signals, one row each.
///
/// Separately on purpose. A single light cannot distinguish "Android is not
/// running" from "Android is running but its composer restarted underneath
/// the session", and those need completely different actions.
fn signals(s: &AppState, t: &Theme) -> gpui::AnyElement {
    if matches!(s.link, Link::Down(_)) {
        return note(t, "liwd is not running, so nothing can be measured.").into_any_element();
    }
    let h: &Health = &s.snapshot.health;
    let rows = [
        ("Session", h.session_running, "The Waydroid session process"),
        ("Container", h.container_running, "The LXC container Android runs in"),
        ("Network", h.has_ip, "Android has an address on its bridge"),
        ("Boot", h.boot_completed, "Android finished booting"),
        ("Composer", h.composer_alive, "The graphics HAL, whose death starts the chain"),
        // Inverted: staleness is the failure, and it is the one that looks
        // healthy from every other angle — processes alive, IP assigned, boot
        // done, and still no window.
        ("Composer fresh", !h.composer_stale,
         "The session's binder connection is not stale"),
    ];

    div()
        .flex()
        .flex_col()
        // flex_none, or the scroll container shrinks this panel to share the
        // height and `overflow_hidden` silently clips the last rows off.
        // Measured: three of six signals were simply missing.
        .flex_none()
        .rounded(px(RADIUS + 2.0))
        .overflow_hidden()
        .border_1()
        .border_color(t.border)
        .children(rows.into_iter().enumerate().map(|(i, (name, ok, why))| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(S3))
                .px(px(S3))
                .py(px(S2 + 2.0))
                .when(i % 2 == 1, |e| e.bg(t.surface))
                .child(div().w(px(8.0)).h(px(8.0)).flex_none().rounded(px(4.0))
                    .bg(if ok { t.ok } else { t.bad }))
                .child(
                    div()
                        .w(px(128.0))
                        .flex_none()
                        .text_size(px(13.0))
                        .text_color(t.text)
                        .child(name),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(11.0))
                        .text_color(t.text_faint)
                        .truncate()
                        .child(why),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(if ok { t.ok } else { t.bad })
                        .child(if ok { "ok" } else { "no" }),
                )
        }))
        .into_any_element()
}

/// Our own injection latency.
///
/// The only number here liwinux is directly responsible for. Everything
/// downstream — Android's input pipeline, the game's own handling — is
/// invisible from this side, and claiming otherwise would be a lie the whole
/// measurement rests on.
fn latency_panel(s: &AppState, t: &Theme) -> gpui::AnyElement {
    let samples = s.snapshot.latency_samples;
    let body: gpui::AnyElement = if samples == 0 {
        div()
            .text_size(px(12.0))
            .text_color(t.text_faint)
            .child(
                "No samples yet. This fills in while a mapped game is in the \
                 foreground and the key mapper is running.",
            )
            .into_any_element()
    } else {
        let p50 = s.snapshot.latency_p50_us as f32 / 1000.0;
        let p99 = s.snapshot.latency_p99_us as f32 / 1000.0;
        div()
            .flex()
            .flex_row()
            .gap(px(S6))
            .child(figure(t, "p50", p50, t.ok))
            .child(figure(t, "p99", p99,
                if p99 < 2.0 { t.ok } else if p99 < 6.0 { t.warn } else { t.bad }))
            .child(figure_n(t, "samples", samples))
            .into_any_element()
    };

    panel(t, "Input latency",
        "From the kernel timestamp of your key press to the moment we hand \
         Android a touch. The rest of the chain is not measurable from here.",
        body)
}

fn figure(t: &Theme, label: &str, ms: f32, colour: gpui::Hsla) -> gpui::AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .child(
            div()
                .text_size(px(22.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(colour)
                .child(SharedString::from(format!("{ms:.2} ms"))),
        )
        .child(div().text_size(px(10.0)).text_color(t.text_faint).child(
            SharedString::from(label.to_string())))
        .into_any_element()
}

fn figure_n(t: &Theme, label: &str, n: u64) -> gpui::AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .child(
            div()
                .text_size(px(22.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(t.text_muted)
                .child(SharedString::from(n.to_string())),
        )
        .child(div().text_size(px(10.0)).text_color(t.text_faint).child(
            SharedString::from(label.to_string())))
        .into_any_element()
}

fn actions(s: &AppState, t: &Theme, cx: &mut Context<AppState>) -> gpui::AnyElement {
    let up = matches!(s.link, Link::Up);
    let running = s.snapshot.state == "RUNNING";
    let busy = s.busy.is_some();

    panel(t, "Session",
        "Recovery restarts Android without touching this window — all the \
         state lives in the daemon.",
        div()
            .flex()
            .flex_row()
            .gap(px(S2))
            .when(up, |el| {
                el.child(button(t, "diag-session",
                    if running { "Stop Android" } else { "Start Android" },
                    busy, cx, move |st, cx| st.session(!running, cx)))
            })
            .when(!up, |el| {
                el.child(
                    div()
                        .text_size(px(12.0))
                        .text_color(t.text_faint)
                        .child("systemctl --user start liwd"),
                )
            })
            .into_any_element())
}

fn button<F>(
    t: &Theme, id: &'static str, label: &str, busy: bool,
    cx: &mut Context<AppState>, f: F,
) -> gpui::AnyElement
where
    F: Fn(&mut AppState, &mut Context<AppState>) + 'static,
{
    div()
        .id(id)
        .px(px(S4))
        .py(px(S2))
        .rounded(px(RADIUS))
        .border_1()
        .border_color(t.border)
        .text_size(px(12.0))
        .text_color(if busy { t.text_faint } else { t.text })
        .when(!busy, |e| e.cursor_pointer().hover(|x| x.bg(t.raised)))
        .child(SharedString::from(if busy { "…" } else { label }))
        .when(!busy, |e| e.on_click(cx.listener(move |st, _, _, cx| f(st, cx))))
        .into_any_element()
}

/// Points at the tool that answers the question this page cannot.
fn deeper(t: &Theme) -> gpui::AnyElement {
    panel(t, "Why did it stutter?",
        "That needs a capture over time, not a live reading: frame timing, \
         Android's log and host resources put on one clock and correlated.",
        div()
            .flex()
            .flex_col()
            .gap(px(S1))
            .child(mono(t, "liw trace <package> --duration 90"))
            .child(
                div().text_size(px(11.0)).text_color(t.text_faint).child(
                    "Detects stalls while they are happening and captures the \
                     log then — afterwards is usually too late, the ring has \
                     already dropped it.",
                ),
            )
            .into_any_element())
}

fn mono(t: &Theme, text: &str) -> gpui::AnyElement {
    div()
        .px(px(S3))
        .py(px(S2))
        .rounded(px(RADIUS))
        .bg(t.bg)
        .border_1()
        .border_color(t.border)
        .text_size(px(12.0))
        .text_color(t.text)
        .child(SharedString::from(text.to_string()))
        .into_any_element()
}

fn panel(t: &Theme, title: &str, note: &str, body: gpui::AnyElement) -> gpui::AnyElement {
    div()
        .flex()
        .flex_col()
        // Same reason as the signal list: inside a scroll container a flex
        // child that can shrink, will.
        .flex_none()
        .gap(px(S3))
        .p(px(S4))
        .rounded(px(RADIUS + 2.0))
        .bg(t.surface)
        .border_1()
        .border_color(t.border)
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .text_size(px(14.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(t.text)
                        .child(SharedString::from(title.to_string())),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(t.text_faint)
                        .child(SharedString::from(note.to_string())),
                ),
        )
        .child(body)
        .into_any_element()
}

fn note(t: &Theme, text: &str) -> impl IntoElement {
    div()
        .p(px(S4))
        .rounded(px(RADIUS))
        .bg(t.surface)
        .text_size(px(13.0))
        .text_color(t.text_muted)
        .child(SharedString::from(text.to_string()))
}
