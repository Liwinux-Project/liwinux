//! The library: a hero for what you are playing, then your games, then the rest.
//!
//! # Working without cover art
//!
//! Waydroid caches icons at 54x54, so the poster grid a store front normally
//! leans on is not available. Instead each surface is washed in the colour of
//! the app's own icon (`tint`), which gives the page the same variety and
//! richness without pretending to artwork we do not have. The icon itself is
//! always drawn near its native size — scaled up it looks like a mistake.

use gpui::{Context, ElementId, IntoElement, SharedString, div, img, prelude::*, px};
use liw_core::apps::App as AndroidApp;

use crate::state::AppState;
use crate::theme::{Theme, RADIUS, S1, S2, S3, S4, S6};
use crate::tint::Tint;

/// The store is not a game. It sits at the end of the row as its own action
/// so "Your games" means what it says, and so there is somewhere obvious to
/// go when the library is empty.
const STORE: &str = "com.android.vending";

const CARD_W: f32 = 184.0;
/// Art area of a card, 16:9 — the shape key art actually comes in.
const CARD_ART_H: f32 = CARD_W * 9.0 / 16.0;
const CARD_ICON: f32 = 46.0;
const HERO_ICON: f32 = 54.0;

/// How much of the hero's width becomes its height.
///
/// Key art is 16:9 and a full-width hero is far wider than that, so it is
/// always cropped — the question is only how brutally. At a fixed 188px the
/// 1280x720 art showed as a thin band across the middle. Deriving the height
/// from the width keeps the crop proportional as the window resizes, and the
/// clamp stops it from eating the page on a wide monitor.
const HERO_RATIO: f32 = 3.1;
const HERO_MIN: f32 = 176.0;
const HERO_MAX: f32 = 330.0;

fn hero_height(window: &gpui::Window) -> f32 {
    let w = f32::from(window.viewport_size().width);
    // Minus the page padding either side, so the ratio is against the panel
    // and not the window.
    ((w - S6 * 2.0) / HERO_RATIO).clamp(HERO_MIN, HERO_MAX)
}

pub fn render(
    s: &AppState, t: &Theme, window: &gpui::Window, cx: &mut Context<AppState>,
) -> gpui::AnyElement {
    let hero_h = hero_height(window);
    let hero_el = s.hero().cloned().map(|a| hero(a, s, t, hero_h, cx));

    let games: Vec<AndroidApp> = s.visible_apps()
        .filter(|a| !a.system && a.package != STORE)
        .cloned()
        .collect();
    let mut cards = Vec::with_capacity(games.len() + 1);
    for a in games.iter().cloned() {
        cards.push(card(a, s, t, cx));
    }
    // Always last, so they do not move as games come and go.
    if let Some(store) = s.apps.iter().find(|a| a.package == STORE).cloned() {
        cards.push(store_card(store, s, t, cx));
    }
    cards.push(install_card(s, t, cx));

    let system: Vec<AndroidApp> = s.visible_apps().filter(|a| a.system).cloned().collect();
    let mut rows = Vec::with_capacity(system.len());
    for a in system.iter().cloned() {
        rows.push(row(a, s, t, cx));
    }
    let hidden = s.apps.iter().filter(|a| a.system).count();
    let toggle = system_toggle(s, t, hidden, cx);

    div()
        .id("library")
        .flex()
        .flex_col()
        .size_full()
        .gap(px(S6))
        .overflow_y_scroll()
        .when_some(hero_el, |el, h| el.child(h))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(S3))
                .child(section(t, "Your games", games.len()))
                .child(if games.is_empty() && cards.len() <= 2 {
                    empty(t, s).into_any_element()
                } else {
                    div()
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .gap(px(S3))
                        .children(cards)
                        .into_any_element()
                }),
        )
        .when(!rows.is_empty() || hidden > 0, |el| {
            el.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(S2))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            // Show the HIDDEN count when they are hidden:
                            // "System apps 0" next to a "Show 12" button
                            // contradicts itself.
                            .child(section(t, "System apps",
                                if s.show_system { rows.len() } else { hidden }))
                            .child(toggle),
                    )
                    .children(rows),
            )
        })
        .into_any_element()
}

fn section(t: &Theme, title: &str, n: usize) -> gpui::AnyElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(S2))
        .child(
            div()
                .text_size(px(15.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(t.text)
                .child(SharedString::from(title.to_string())),
        )
        .child(
            div()
                .text_size(px(12.0))
                .text_color(t.text_faint)
                .child(SharedString::from(n.to_string())),
        )
        .into_any_element()
}

/// The hero. Big, tinted, one primary action.
///
/// Follows the foreground app while Android is running, so during a session
/// the page is about the game you are in rather than a static banner.
fn hero(
    a: AndroidApp, s: &AppState, t: &Theme, height: f32, cx: &mut Context<AppState>,
) -> gpui::AnyElement {
    let side = hero_side(&a, s, t, cx);
    let tint = s.tint(&a.package);
    let mapped = s.has_profile(&a.package);
    let running = s.snapshot.foreground.as_deref() == Some(a.package.as_str());
    let busy = s.busy.is_some();
    let package = a.package.clone();

    let art = s.art(&a.package).map(|p| p.to_path_buf());

    div()
        .h(px(height))
        .flex_none()
        .relative()
        .overflow_hidden()
        .rounded(px(RADIUS + 6.0))
        // Without artwork the panel takes the game's own colour instead, so
        // the layout never has a hole where a picture should be.
        .bg(tint.gradient(110.0))
        .border_1()
        .border_color(tint.accent().opacity(0.22))
        .when_some(art, |el, p| {
            el.child(
                img(gpui::ImageSource::Resource(gpui::Resource::Path(
                    std::sync::Arc::from(p.as_path()),
                )))
                .absolute()
                .inset_0()
                .size_full()
                // Cover, not contain: key art is 16:9 and the panel is wider
                // and shorter, so letterboxing would show the page behind it.
                .object_fit(gpui::ObjectFit::Cover),
            )
            // Scrim, in two passes. Key art is busy and bright and white text
            // on it is unreadable without one, but a flat overlay kills the
            // picture. Horizontal first, heavy where the title sits and
            // clearing to the right...
            .child(
                div().absolute().inset_0().bg(gpui::linear_gradient(
                    90.0,
                    gpui::linear_color_stop(gpui::hsla(0.62, 0.10, 0.04, 0.90), 0.0),
                    gpui::linear_color_stop(gpui::hsla(0.62, 0.10, 0.04, 0.10), 1.0),
                )),
            )
            // ...then vertical, because the content sits along the bottom and
            // a tall hero leaves the art bright exactly there. One pass could
            // not do both without darkening the whole picture.
            .child(
                div().absolute().inset_0().bg(gpui::linear_gradient(
                    180.0,
                    gpui::linear_color_stop(gpui::hsla(0.62, 0.10, 0.04, 0.0), 0.0),
                    gpui::linear_color_stop(gpui::hsla(0.62, 0.10, 0.04, 0.85), 1.0),
                )),
            )
        })
        .child(
            div()
                .relative()
                .size_full()
                .flex()
                .flex_row()
                .items_end()
                .justify_between()
                .p(px(S6))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(S3))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(S3))
                        .child(icon_well(&a, HERO_ICON, HERO_ICON + 20.0, t, tint))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(S1))
                                .child(
                                    div()
                                        .text_size(px(24.0))
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .text_color(t.text)
                                        .child(SharedString::from(a.name.clone())),
                                )
                                .child(
                                    div()
                                        .text_size(px(12.0))
                                        .text_color(t.text_muted)
                                        .child(SharedString::from(if running {
                                            "Running now".to_string()
                                        } else if mapped {
                                            "Key mapping ready".to_string()
                                        } else {
                                            a.package.clone()
                                        })),
                                ),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(S2))
                        .child(primary(t, if running { "Bring to front" } else { "Play" },
                                       busy, "hero-play", cx, package))
                        .when(!mapped, |el| {
                            el.child(ghost(t, "No key mapping yet"))
                        }),
                ),
        )
                .when_some(side, |el, sd| el.child(sd)),
        )
        .into_any_element()
}

/// The other games, listed down the right of the hero.
///
/// Mirrors what a store front does with its featured rail: it fills the panel
/// and gives the hero somewhere to go. Clicking one makes it the hero rather
/// than launching it — a launch is a big action and belongs behind the button.
fn hero_side(
    current: &AndroidApp, s: &AppState, t: &Theme, cx: &mut Context<AppState>,
) -> Option<gpui::AnyElement> {
    let others: Vec<AndroidApp> = s.apps.iter()
        .filter(|a| !a.system && a.package != STORE && a.package != current.package)
        .take(5)
        .cloned()
        .collect();
    // Below a handful of games this list is just the cards underneath it,
    // written out again. It earns its place once the row starts to overflow.
    const WORTH_LISTING: usize = 4;
    if others.len() < WORTH_LISTING { return None; }

    let mut rows = Vec::with_capacity(others.len());
    for a in others {
        let pkg = a.package.clone();
        let id: ElementId = SharedString::from(format!("hero-side-{}", a.package)).into();
        rows.push(
            div()
                .id(id)
                .px(px(S2))
                .py(px(S1))
                .rounded(px(RADIUS - 2.0))
                .text_size(px(13.0))
                .text_color(t.text_muted)
                .text_right()
                .cursor_pointer()
                .hover(|x| x.text_color(t.text))
                .child(SharedString::from(a.name.clone()))
                .on_click(cx.listener(move |st, _, _, cx| {
                    st.featured = Some(pkg.clone());
                    cx.notify();
                }))
                .into_any_element(),
        );
    }
    Some(
        div()
            .flex()
            .flex_col()
            .items_end()
            .gap(px(S1))
            .child(
                div()
                    .text_size(px(10.0))
                    .text_color(t.text_faint)
                    .pb(px(S1))
                    .child("ALSO INSTALLED"),
            )
            .children(rows)
            .into_any_element(),
    )
}

/// Filled accent button — the one call to action on the page.
fn primary(
    t: &Theme, label: &str, busy: bool, id: &'static str,
    cx: &mut Context<AppState>, package: String,
) -> gpui::AnyElement {
    div()
        .id(id)
        .px(px(S6))
        .py(px(S2 + 2.0))
        .rounded(px(RADIUS))
        .bg(if busy { t.raised } else { t.accent })
        .text_size(px(13.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        // Dark ink on the accent fill: white on this blue is unreadable.
        .text_color(if busy { t.text_faint } else { t.bg })
        .when(!busy, |e| {
            e.cursor_pointer()
                .hover(|x| x.bg(t.accent.opacity(0.85)))
                .on_click(cx.listener(move |st, _, _, cx| st.launch(package.clone(), cx)))
        })
        .child(SharedString::from(if busy { "working…" } else { label }))
        .into_any_element()
}

fn ghost(t: &Theme, label: &str) -> gpui::AnyElement {
    div()
        .px(px(S4))
        .py(px(S2 + 2.0))
        .rounded(px(RADIUS))
        .border_1()
        .border_color(t.border)
        .text_size(px(12.0))
        .text_color(t.text_muted)
        .child(SharedString::from(label.to_string()))
        .into_any_element()
}

fn icon_well(
    a: &AndroidApp, icon: f32, well: f32, t: &Theme, tint: Tint,
) -> gpui::AnyElement {
    div()
        .w(px(well))
        .h(px(well))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(px(RADIUS + 4.0))
        .bg(tint.wash(0.5))
        .child(match &a.icon {
            Some(p) => img(gpui::ImageSource::Resource(gpui::Resource::Path(
                std::sync::Arc::from(p.as_path()),
            )))
            .w(px(icon))
            .h(px(icon))
            .into_any_element(),
            None => div()
                .text_size(px(icon * 0.42))
                .text_color(t.text_muted)
                .child(initial(&a.name))
                .into_any_element(),
        })
        .into_any_element()
}

/// The store, drawn as an invitation rather than as another game.
fn store_card(
    a: AndroidApp, s: &AppState, t: &Theme, cx: &mut Context<AppState>,
) -> gpui::AnyElement {
    let busy = s.busy.is_some();
    let tint = s.tint(&a.package);
    let package = a.package.clone();
    div()
        .id("card-store")
        .w(px(CARD_W))
        .flex()
        .flex_col()
        .rounded(px(RADIUS + 4.0))
        .overflow_hidden()
        // Dashed and unfilled: it reads as "add something", not as content.
        .border_dashed()
        .border_1()
        .border_color(t.border)
        .cursor_pointer()
        .hover(|x| x.border_color(t.accent.opacity(0.6)).bg(t.surface))
        .child(
            div()
                .w_full()
                .h(px(CARD_ART_H))
                .flex()
                .items_center()
                .justify_center()
                .child(icon_well(&a, CARD_ICON, CARD_ICON + 18.0, t, tint)),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(S1))
                .px(px(S3))
                .py(px(S2 + 2.0))
                .child(
                    div()
                        .text_size(px(13.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(t.text)
                        .child("Get more games"),
                )
                .child(
                    div()
                        .h(px(13.0))
                        .text_size(px(10.0))
                        .text_color(t.text_faint)
                        .child("Play Store"),
                ),
        )
        .when(!busy, |el| {
            el.on_click(cx.listener(move |st, _, _, cx| st.launch(package.clone(), cx)))
        })
        .into_any_element()
}

/// Install an APK the user already has.
///
/// Lives beside the store card because both answer the same question — "how
/// do I get another game?" — and that question belongs on the library, next
/// to what you already have.
fn install_card(s: &AppState, t: &Theme, cx: &mut Context<AppState>) -> gpui::AnyElement {
    let busy = s.busy.is_some();
    div()
        .id("card-install")
        .w(px(CARD_W))
        .flex()
        .flex_col()
        .rounded(px(RADIUS + 4.0))
        .overflow_hidden()
        .border_dashed()
        .border_1()
        .border_color(t.border)
        .cursor_pointer()
        .hover(|x| x.border_color(t.accent.opacity(0.6)).bg(t.surface))
        .child(
            div()
                .w_full()
                .h(px(CARD_ART_H))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(28.0))
                .text_color(t.text_faint)
                .child("+"),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(S1))
                .px(px(S3))
                .py(px(S2 + 2.0))
                .child(
                    div()
                        .text_size(px(13.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(t.text)
                        .child("Install an APK"),
                )
                .child(
                    div()
                        .h(px(13.0))
                        .text_size(px(10.0))
                        .text_color(t.text_faint)
                        .child("Or Aurora Store"),
                ),
        )
        .when(!busy, |el| {
            el.on_click(cx.listener(|st, _, _, cx| st.pick_and_install(cx)))
        })
        .into_any_element()
}

fn card(
    a: AndroidApp, s: &AppState, t: &Theme, cx: &mut Context<AppState>,
) -> gpui::AnyElement {
    let tint = s.tint(&a.package);
    let mapped = s.has_profile(&a.package);
    let busy = s.busy.is_some();
    let package = a.package.clone();
    let art = s.art(&a.package).map(|p| p.to_path_buf());
    let id: ElementId = SharedString::from(format!("card-{}", a.package)).into();

    // Every card is the same shape whether or not it has art. Mixing a tall
    // banner tile with a short icon tile in one row reads as a layout bug,
    // and most packages will never have artwork.
    let head = div()
        .w_full()
        .h(px(CARD_ART_H))
        .relative()
        .overflow_hidden()
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
        .when(s.art(&a.package).is_none(), |el| {
            el.child(icon_well(&a, CARD_ICON, CARD_ICON + 18.0, t, tint))
        });

    div()
        .id(id)
        .w(px(CARD_W))
        .flex()
        .flex_col()
        .rounded(px(RADIUS + 4.0))
        .overflow_hidden()
        .bg(t.surface)
        .border_1()
        .border_color(t.border)
        .cursor_pointer()
        .hover(|x| x.border_color(tint.accent().opacity(0.55)))
        .child(head)
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(S1))
                .px(px(S3))
                .py(px(S2 + 2.0))
                .child(
                    div()
                        .w_full()
                        .text_size(px(13.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(t.text)
                        .truncate()
                        .child(SharedString::from(a.name.clone())),
                )
                .child(
                    div()
                        .h(px(13.0))
                        .text_size(px(10.0))
                        .text_color(if mapped { tint.accent() } else { t.text_faint })
                        .child(SharedString::from(if mapped { "key mapping" } else { "—" })),
                ),
        )
        .when(!busy, |el| {
            el.on_click(cx.listener(move |st, _, _, cx| st.launch(package.clone(), cx)))
        })
        .into_any_element()
}

/// Compact row for the long tail — system apps that need a line, not a tile.
fn row(
    a: AndroidApp, s: &AppState, t: &Theme, cx: &mut Context<AppState>,
) -> gpui::AnyElement {
    let tint = s.tint(&a.package);
    let busy = s.busy.is_some();
    let package = a.package.clone();
    let id: ElementId = SharedString::from(format!("row-{}", a.package)).into();

    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(S3))
        .p(px(S2))
        .rounded(px(RADIUS))
        .hover(|x| x.bg(t.surface))
        .cursor_pointer()
        .child(icon_well(&a, 26.0, 38.0, t, tint))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(13.0))
                .text_color(t.text)
                .truncate()
                .child(SharedString::from(a.name.clone())),
        )
        .child(
            div()
                .text_size(px(11.0))
                .text_color(t.text_faint)
                .child(SharedString::from(a.package.clone())),
        )
        .when(!busy, |el| {
            el.on_click(cx.listener(move |st, _, _, cx| st.launch(package.clone(), cx)))
        })
        .into_any_element()
}

fn system_toggle(
    s: &AppState, t: &Theme, hidden: usize, cx: &mut Context<AppState>,
) -> gpui::AnyElement {
    let show = s.show_system;
    div()
        .id("toggle-system")
        .px(px(S3))
        .py(px(S1 + 2.0))
        .rounded(px(RADIUS))
        .border_1()
        .border_color(t.border)
        .text_size(px(12.0))
        .text_color(t.text_muted)
        .cursor_pointer()
        .hover(|x| x.bg(t.surface).text_color(t.text))
        .child(SharedString::from(if show {
            "Hide".to_string()
        } else {
            format!("Show {hidden}")
        }))
        .on_click(cx.listener(|st, _, _, cx| {
            st.show_system = !st.show_system;
            cx.notify();
        }))
        .into_any_element()
}

fn empty(t: &Theme, s: &AppState) -> gpui::AnyElement {
    let searching = !s.search.trim().is_empty();
    div()
        .flex()
        .flex_col()
        .gap(px(S1))
        .p(px(S6))
        .rounded(px(RADIUS))
        .bg(t.surface)
        .child(
            div().text_size(px(13.0)).text_color(t.text_muted).child(if searching {
                SharedString::from("Nothing matches that")
            } else {
                SharedString::from("No games yet")
            }),
        )
        .when(!searching, |el| {
            el.child(div().text_size(px(12.0)).text_color(t.text_faint).child(
                "Install something from the Play Store inside Android; it \
                 appears here once Waydroid writes its desktop entry.",
            ))
        })
        .into_any_element()
}

fn initial(name: &str) -> SharedString {
    name.chars().next().map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".into())
        .into()
}
