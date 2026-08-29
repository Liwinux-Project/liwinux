//! The library screen: installed apps as cards.
//!
//! # Designed around a 54px icon
//!
//! Waydroid caches app icons at 54x54. There is no cover art to be had, so a
//! GameLoop-style poster grid is not on the table — enlarging a 54px PNG to
//! fill a tile looks worse than not trying. The card leans on typography and
//! a tinted icon well instead, and stays small enough that the icon is near
//! its native size.

use gpui::{Context, ElementId, IntoElement, SharedString, div, img, prelude::*, px};
use liw_core::apps::App as AndroidApp;

use crate::state::AppState;
use crate::theme::{Theme, RADIUS, S1, S2, S3, S4};

const CARD_W: f32 = 168.0;
const ICON_WELL: f32 = 64.0;
const ICON: f32 = 44.0;

pub fn render(state: &AppState, t: &Theme, cx: &mut Context<AppState>) -> impl IntoElement {
    let apps: Vec<AndroidApp> = state.visible_apps().cloned().collect();
    let hidden = state.apps.len() - apps.len();
    // Built with a loop rather than `.map()`: the closure would have to
    // capture `cx` mutably and gpui's `children` takes an iterator that
    // outlives the borrow.
    let head = header(state, t, hidden, cx);
    let mut cards = Vec::with_capacity(apps.len());
    for a in apps.iter().cloned() {
        cards.push(card(a, state, t, cx));
    }

    div()
        .flex()
        .flex_col()
        .size_full()
        .gap(px(S4))
        .child(head)
        .child(if apps.is_empty() {
            empty(t).into_any_element()
        } else {
            div()
                .id("library-grid")
                .flex()
                .flex_row()
                .flex_wrap()
                .gap(px(S3))
                .overflow_y_scroll()
                .children(cards)
                .into_any_element()
        })
}

/// Returns `AnyElement`, not `impl IntoElement`: under Rust 2024 the opaque
/// type captures `cx`'s lifetime, so the borrow would still be live when the
/// caller needs `cx` again.
fn header(
    state: &AppState, t: &Theme, hidden: usize, cx: &mut Context<AppState>,
) -> gpui::AnyElement {
    let show = state.show_system;
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .child(
            div()
                .flex()
                .flex_col()
                .child(div().text_size(px(18.0)).text_color(t.text).child("Library"))
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(t.text_faint)
                        // The list works with Android stopped, because it comes
                        // from Waydroid's .desktop files rather than from the
                        // running system. Worth saying: an empty library with a
                        // stopped session would otherwise look like a bug.
                        .child("From Waydroid's desktop entries — available even \
                                while Android is stopped"),
                ),
        )
        .when(hidden > 0 || show, |el| {
            el.child(
                div()
                    .id("toggle-system")
                    .px(px(S3))
                    .py(px(S1 + 2.0))
                    .rounded(px(RADIUS))
                    .bg(if show { t.raised } else { t.surface })
                    .border_1()
                    .border_color(t.border)
                    .text_size(px(12.0))
                    .text_color(if show { t.text } else { t.text_muted })
                    .cursor_pointer()
                    .child(if show {
                        SharedString::from("Hide system apps")
                    } else {
                        SharedString::from(format!("Show {hidden} system apps"))
                    })
                    .on_click(cx.listener(|s, _, _, cx| {
                        s.show_system = !s.show_system;
                        cx.notify();
                    })),
            )
        })
        .into_any_element()
}

fn empty(t: &Theme) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .size_full()
        .gap(px(S2))
        .child(div().text_size(px(15.0)).text_color(t.text_muted).child("No apps yet"))
        .child(
            div().text_size(px(12.0)).text_color(t.text_faint).child(
                "Install something from the Play Store inside Android; it shows \
                 up here once Waydroid writes its desktop entry.",
            ),
        )
}

fn card(
    a: AndroidApp, state: &AppState, t: &Theme, cx: &mut Context<AppState>,
) -> gpui::AnyElement {
    let package = a.package.clone();
    let mapped = state.has_profile(&a.package);
    let busy = state.busy.is_some();
    let id: ElementId = SharedString::from(a.package.clone()).into();

    div()
        .id(id)
        .w(px(CARD_W))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(S2))
        .p(px(S3))
        .rounded(px(RADIUS + 2.0))
        .bg(t.surface)
        .border_1()
        .border_color(t.border)
        .cursor_pointer()
        .hover(|s| s.bg(t.raised).border_color(t.accent.opacity(0.5)))
        .child(
            div()
                .w(px(ICON_WELL))
                .h(px(ICON_WELL))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(RADIUS + 4.0))
                .bg(t.raised)
                .child(match &a.icon {
                    // Drawn near its native 54px. Scaling it up to fill a
                    // poster tile looks worse than leaving it small.
                    Some(p) => img(gpui::ImageSource::Resource(gpui::Resource::Path(
                        std::sync::Arc::from(p.as_path()),
                    )))
                    .w(px(ICON))
                    .h(px(ICON))
                    .into_any_element(),
                    None => div()
                        .text_size(px(20.0))
                        .text_color(t.text_faint)
                        .child(initial(&a.name))
                        .into_any_element(),
                }),
        )
        .child(
            div()
                .w_full()
                .text_size(px(13.0))
                .text_color(t.text)
                .text_center()
                .truncate()
                .child(SharedString::from(a.name.clone())),
        )
        .child(
            div()
                .h(px(16.0))
                .text_size(px(10.0))
                .text_color(if mapped { t.accent } else { t.text_faint })
                .child(if mapped {
                    SharedString::from("key mapping")
                } else {
                    SharedString::from("no mapping")
                }),
        )
        .when(!busy, |el| {
            el.on_click(cx.listener(move |s, _, _, cx| {
                s.launch(package.clone(), cx);
            }))
        })
        .into_any_element()
}

/// First character of the name, for apps with no cached icon.
fn initial(name: &str) -> SharedString {
    name.chars().next().map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".into())
        .into()
}
