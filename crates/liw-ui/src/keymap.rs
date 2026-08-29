//! The key mapping page: every profile, and whether its game is installed.
//!
//! This used to be a "Store" tab. Without a catalogue behind it that name
//! promised something it could not do, so the tab is gone and its contents
//! moved to where they belong — the profile list here, and the install
//! actions onto the library.

use gpui::{Context, ElementId, IntoElement, SharedString, div, img, prelude::*, px};

use crate::state::AppState;
use crate::theme::{Theme, RADIUS, S1, S2, S3, S4, S6};

pub fn render(s: &AppState, t: &Theme, cx: &mut Context<AppState>) -> gpui::AnyElement {
    let mut rows = Vec::new();
    for p in &s.profiles.profiles {
        let installed = s.apps.iter().any(|a| a.package == p.package);
        rows.push(entry(
            p.package.clone(), p.name.clone(), p.bindings, installed, s, t, cx,
        ));
    }

    div()
        .id("keymap")
        .flex()
        .flex_col()
        .size_full()
        .gap(px(S4))
        .overflow_y_scroll()
        // The heading sits with its list, not above the section before it.
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(S1))
                .child(
                    div()
                        .text_size(px(18.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("Key mapping"),
                )
                .child(
                    div().text_size(px(12.0)).text_color(t.text_faint).child(
                        "Profiles turn the keyboard and mouse into touches. \
                         Editing is still `liw profile edit <package>` until \
                         the editor moves in here.",
                    ),
                ),
        )
        .child(if rows.is_empty() {
            div()
                .p(px(S6))
                .rounded(px(RADIUS))
                .bg(t.surface)
                .text_size(px(13.0))
                .text_color(t.text_muted)
                .child("No profiles yet.")
                .into_any_element()
        } else {
            div().flex().flex_col().gap(px(S2)).children(rows).into_any_element()
        })
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn entry(
    package: String, name: String, bindings: usize, installed: bool,
    s: &AppState, t: &Theme, cx: &mut Context<AppState>,
) -> gpui::AnyElement {
    let tint = s.tint(&package);
    let art = s.art(&package).map(|p| p.to_path_buf());
    let icon = s.apps.iter().find(|a| a.package == package)
        .and_then(|a| a.icon.clone());
    let busy = s.busy.is_some();
    let id: ElementId = SharedString::from(format!("km-{package}")).into();
    let pkg_for_action = package.clone();

    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(S3))
        .p(px(S2))
        .pr(px(S3))
        .rounded(px(RADIUS + 2.0))
        .bg(t.surface)
        .border_1()
        .border_color(t.border)
        .child(
            div()
                .w(px(112.0))
                .h(px(63.0))
                .flex_none()
                .relative()
                .overflow_hidden()
                .rounded(px(RADIUS))
                .flex()
                .items_center()
                .justify_center()
                .bg(tint.gradient(180.0))
                .when_some(art, |el, p| {
                    el.child(
                        img(gpui::ImageSource::Resource(gpui::Resource::Path(
                            std::sync::Arc::from(p.as_path()),
                        )))
                        .absolute()
                        .inset_0()
                        .size_full()
                        .object_fit(gpui::ObjectFit::Cover),
                    )
                })
                .when_some(icon.filter(|_| s.art(&package).is_none()), |el, p| {
                    el.child(
                        img(gpui::ImageSource::Resource(gpui::Resource::Path(
                            std::sync::Arc::from(p.as_path()),
                        )))
                        .w(px(34.0))
                        .h(px(34.0)),
                    )
                }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .text_size(px(14.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(t.text)
                        .truncate()
                        .child(SharedString::from(name)),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(t.text_faint)
                        .truncate()
                        .child(SharedString::from(package.clone())),
                )
                .child(
                    div()
                        .text_size(px(10.0))
                        .text_color(tint.accent())
                        .child(SharedString::from(format!("{bindings} bindings"))),
                ),
        )
        .child(action(t, installed, busy, pkg_for_action, cx))
        .into_any_element()
}

/// Play when the game is here; otherwise say it is not and offer the store.
///
/// "Install" does not install — it opens the store at that page. Pretending
/// otherwise would mean owning downloads, signatures and updates, all of
/// which the store already does properly.
fn action(
    t: &Theme, installed: bool, busy: bool, package: String, cx: &mut Context<AppState>,
) -> gpui::AnyElement {
    let id: ElementId = SharedString::from(format!("km-act-{package}")).into();
    div()
        .id(id)
        .flex_none()
        .px(px(S4))
        .py(px(S2))
        .rounded(px(RADIUS))
        .text_size(px(12.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .when(installed, |e| e.bg(t.accent).text_color(t.bg))
        .when(!installed, |e| {
            e.border_1().border_color(t.border).text_color(t.text_muted)
        })
        .when(!busy, |e| e.cursor_pointer().hover(|x| x.opacity(0.85)))
        .child(SharedString::from(if busy {
            "…"
        } else if installed {
            "Play"
        } else {
            "Install"
        }))
        .when(!busy, |e| {
            e.on_click(cx.listener(move |st, _, _, cx| {
                if installed {
                    st.launch(package.clone(), cx);
                } else {
                    st.open_store(package.clone(), cx);
                }
            }))
        })
        .into_any_element()
}
