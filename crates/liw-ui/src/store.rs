//! The Store page: games liwinux has a tested key mapping for.
//!
//! # What this is not
//!
//! It is not a catalogue. GameLoop can show a wall of games because Tencent
//! runs a catalogue service, which is a content operation rather than a
//! feature — nothing here can produce that.
//!
//! What liwinux genuinely has is the list of games it ships a key-mapping
//! profile for: titles somebody actually sat down and tuned. That is a
//! smaller list and an honest one, and it answers the question a store page
//! is really for — "what works well here?" Installing is handed to the Play
//! Store, which already does it.

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
        .id("store")
        .flex()
        .flex_col()
        .size_full()
        .gap(px(S4))
        .overflow_y_scroll()
        .child(sources(s, t, cx))
        // The heading sits with its list, not above the section before it.
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(S1))
                .child(
                    div()
                        .text_size(px(15.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("Tested games"),
                )
                .child(
                    div().text_size(px(11.0)).text_color(t.text_faint).child(
                        "Titles liwinux ships a key mapping for — someone has \
                         actually tuned these.",
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
                .child("No profiles are installed yet.")
                .into_any_element()
        } else {
            div().flex().flex_col().gap(px(S2)).children(rows).into_any_element()
        })
        .into_any_element()
}

/// Where games actually come from.
///
/// liwinux does not run a catalogue and will not: a browsable Play catalogue
/// needs Play API auth, which means either the user's Google credentials or
/// somebody else's pooled accounts. Aurora Store solves that properly, in a
/// client built to ask that question — so the answer is to point at it, not
/// to reimplement it here and quietly borrow its token dispenser.
fn sources(s: &AppState, t: &Theme, cx: &mut Context<AppState>) -> gpui::AnyElement {
    let has = |p: &str| s.apps.iter().any(|a| a.package == p);
    let play = has("com.android.vending");
    let aurora = has("com.aurora.store");

    div()
        .flex()
        .flex_col()
        .gap(px(S2))
        .child(
            div()
                .text_size(px(15.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child("Where games come from"),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .gap(px(S2))
                .child(source_card(
                    "src-play", "Play Store",
                    if play { "Installed — browse and install there" }
                    else { "Not installed on this image" },
                    play.then(|| "com.android.vending".to_string()),
                    t, cx,
                ))
                .child(source_card(
                    "src-aurora", "Aurora Store",
                    if aurora { "Installed — full catalogue, no Google account needed" }
                    else { "Not installed — get the APK from auroraoss.com, then \
                            Install APK below" },
                    aurora.then(|| "com.aurora.store".to_string()),
                    t, cx,
                ))
                .child(
                    div()
                        .id("src-apk")
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap(px(S1))
                        .p(px(S3))
                        .rounded(px(RADIUS + 2.0))
                        .border_dashed()
                        .border_1()
                        .border_color(t.border)
                        .cursor_pointer()
                        .hover(|x| x.border_color(t.accent.opacity(0.6)).bg(t.surface))
                        .child(
                            div()
                                .text_size(px(13.0))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(t.text)
                                .child("Install an APK…"),
                        )
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(t.text_faint)
                                .child("Pick a file you already downloaded"),
                        )
                        .on_click(cx.listener(|st, _, _, cx| st.pick_and_install(cx))),
                ),
        )
        .into_any_element()
}

fn source_card(
    id: &'static str, title: &'static str, note: &str,
    open: Option<String>, t: &Theme, cx: &mut Context<AppState>,
) -> gpui::AnyElement {
    let clickable = open.is_some();
    div()
        .id(id)
        .flex_1()
        .flex()
        .flex_col()
        .gap(px(S1))
        .p(px(S3))
        .rounded(px(RADIUS + 2.0))
        .bg(t.surface)
        .border_1()
        .border_color(t.border)
        .when(clickable, |e| {
            e.cursor_pointer().hover(|x| x.border_color(t.accent.opacity(0.6)))
        })
        .child(
            div()
                .text_size(px(13.0))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(if clickable { t.text } else { t.text_muted })
                .child(title),
        )
        .child(
            div()
                .text_size(px(11.0))
                .text_color(t.text_faint)
                .child(SharedString::from(note.to_string())),
        )
        .when_some(open, |el, pkg| {
            el.on_click(cx.listener(move |st, _, _, cx| st.launch(pkg.clone(), cx)))
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
    let id: ElementId = SharedString::from(format!("store-{package}")).into();
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

/// Play when it is here, Install when it is not.
///
/// Install does not install: it opens the store at that page. Pretending to
/// install would mean owning downloads, signatures and updates — all of which
/// the Play Store already does properly.
fn action(
    t: &Theme, installed: bool, busy: bool, package: String, cx: &mut Context<AppState>,
) -> gpui::AnyElement {
    let id: ElementId = SharedString::from(format!("store-act-{package}")).into();
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
