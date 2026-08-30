//! The rail beside the game.
//!
//! A thin strip that is always there and a panel that is not, in the shape
//! GameLoop uses: the controls live next to the picture rather than on top of
//! it, so nothing a player needs is ever hidden behind the thing they are
//! playing.
//!
//! It carries only what belongs to a running game — mapping keys, filling the
//! window. Everything else stays on its own page: a rail that grows into a
//! second copy of the whole application is how this kind of panel goes wrong.

use gpui::{div, prelude::*, px, Context, IntoElement, SharedString};

use crate::mapper::Editing;
use crate::state::AppState;
use crate::theme::{Theme, RADIUS, S1, S2, S3};

/// Width of the always-visible strip.
pub const RAIL_W: f32 = 44.0;
/// Width of the panel when it is open.
pub const PANEL_W: f32 = 232.0;

/// How much horizontal room the sidebar takes from the picture right now.
///
/// The picture's size is computed from this, so it lives in one place: a
/// second opinion about how wide the rail is would put every binding a few
/// pixels off where it was clicked.
pub fn width(open: bool) -> f32 {
    if open { RAIL_W + PANEL_W } else { RAIL_W }
}

pub fn render(s: &AppState, t: &Theme, cx: &mut Context<AppState>) -> gpui::AnyElement {
    let open = s.sidebar_open;
    div()
        .flex()
        .flex_row()
        .h_full()
        .when(open, |el| el.child(panel(s, t, cx)))
        .child(rail(s, t, cx))
        .into_any_element()
}

/// The strip that is always visible.
fn rail(s: &AppState, t: &Theme, cx: &mut Context<AppState>) -> gpui::AnyElement {
    let mapping = s.mapper.is_on();
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(S2))
        .w(px(RAIL_W))
        .h_full()
        .py(px(S3))
        .bg(t.surface)
        .border_l_1()
        .border_color(t.border)
        .child(tool(t, "sb-open", if s.sidebar_open { "›" } else { "‹" }, false, cx,
                    |st, cx| { st.sidebar_open = !st.sidebar_open; cx.notify(); }))
        .child(tool(t, "sb-map", "⌨", mapping, cx,
                    |st, cx| { st.toggle_mapping(cx); }))
        .child(tool(t, "sb-full", "⛶", s.android.immersive, cx,
                    |st, cx| { st.android.immersive = !st.android.immersive; cx.notify(); }))
        .into_any_element()
}

/// One rail button.
///
/// The glyph carries it alone: this gpui revision has no tooltip, and a rail
/// wide enough for a label would stop being a rail.
fn tool<F>(
    t: &Theme, id: &'static str, glyph: &str, on: bool,
    cx: &mut Context<AppState>, f: F,
) -> gpui::AnyElement
where
    F: Fn(&mut AppState, &mut Context<AppState>) + 'static,
{
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .size(px(32.0))
        .rounded(px(RADIUS))
        .text_size(px(15.0))
        .text_color(if on { t.accent } else { t.text_muted })
        .when(on, |e| e.bg(t.raised))
        .cursor_pointer()
        .hover(|x| x.bg(t.raised).text_color(t.text))
        .child(SharedString::from(glyph.to_string()))
        .on_click(cx.listener(move |st, _, _, cx| f(st, cx)))
        .into_any_element()
}

/// The panel, open only when asked for.
fn panel(s: &AppState, t: &Theme, cx: &mut Context<AppState>) -> gpui::AnyElement {
    let body = if s.mapper.is_on() {
        mapping_panel(s, t, cx)
    } else {
        idle_panel(s, t)
    };
    div()
        .id("sb-panel")
        .flex()
        .flex_col()
        .w(px(PANEL_W))
        .h_full()
        .p(px(S3))
        .gap(px(S2))
        .bg(t.surface)
        .border_l_1()
        .border_color(t.border)
        .overflow_y_scroll()
        .child(body)
        .into_any_element()
}

fn idle_panel(s: &AppState, t: &Theme) -> gpui::AnyElement {
    let running = s.android.running();
    div()
        .flex()
        .flex_col()
        .gap(px(S2))
        .child(heading(t, "Game"))
        .child(note(t, if running {
            "Android is hosted in this window."
        } else {
            "Nothing is hosted yet."
        }))
        .child(heading(t, "Keys"))
        .child(note(t, "Press the keyboard button to place bindings on the \
                        picture: click where a button is, then press the key \
                        that should press it."))
        .into_any_element()
}

fn mapping_panel(s: &AppState, t: &Theme, cx: &mut Context<AppState>) -> gpui::AnyElement {
    let markers = s.mapper.markers();
    let waiting = matches!(s.mapper.editing, Editing::AwaitingKey(_));

    let mut list = div().flex().flex_col().gap(px(S1));
    if markers.is_empty() {
        list = list.child(note(t, "No bindings yet."));
    }
    for (name, _, key) in markers {
        let for_click = name.clone();
        list = list.child(
            div()
                .id(SharedString::from(format!("bind-{name}")))
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .px(px(S2))
                .py(px(S1))
                .rounded(px(RADIUS))
                .bg(t.raised)
                .text_size(px(12.0))
                .child(SharedString::from(key))
                .child(
                    div()
                        .id(SharedString::from(format!("del-{name}")))
                        .px(px(S1))
                        .text_color(t.text_faint)
                        .cursor_pointer()
                        .hover(|x| x.text_color(t.bad))
                        .child("✕")
                        .on_click(cx.listener(move |st, _, _, cx| {
                            st.mapper.remove(&for_click);
                            cx.notify();
                        })),
                ),
        );
    }

    div()
        .flex()
        .flex_col()
        .gap(px(S2))
        .child(heading(t, "Mapping"))
        .child(note(t, if waiting {
            "Now press the key for that spot."
        } else {
            "Click a button on the picture."
        }))
        .child(list)
        .when_some(s.mapper.error.clone(), |el, e| {
            el.child(div().text_size(px(11.0)).text_color(t.bad).child(SharedString::from(e)))
        })
        .when(s.mapper.saved, |el| {
            el.child(div().text_size(px(11.0)).text_color(t.ok).child("Saved."))
        })
        .child(
            div()
                .flex()
                .flex_row()
                .gap(px(S1))
                .child(action(t, "map-save", "Save", cx, |st, cx| {
                    st.mapper.save();
                    cx.notify();
                }))
                .child(action(t, "map-done", "Done", cx, |st, cx| {
                    st.mapper.end();
                    cx.notify();
                })),
        )
        .into_any_element()
}

fn action<F>(
    t: &Theme, id: &'static str, label: &str,
    cx: &mut Context<AppState>, f: F,
) -> gpui::AnyElement
where
    F: Fn(&mut AppState, &mut Context<AppState>) + 'static,
{
    div()
        .id(id)
        .px(px(S3))
        .py(px(S1))
        .rounded(px(RADIUS))
        .border_1()
        .border_color(t.border)
        .text_size(px(12.0))
        .text_color(t.text)
        .cursor_pointer()
        .hover(|x| x.bg(t.raised))
        .child(SharedString::from(label.to_string()))
        .on_click(cx.listener(move |st, _, _, cx| f(st, cx)))
        .into_any_element()
}

fn heading(t: &Theme, s: &str) -> gpui::AnyElement {
    div()
        .text_size(px(11.0))
        .text_color(t.text_faint)
        .child(SharedString::from(s.to_uppercase()))
        .into_any_element()
}

fn note(t: &Theme, s: &str) -> gpui::AnyElement {
    div()
        .text_size(px(12.0))
        .text_color(t.text_muted)
        .child(SharedString::from(s.to_string()))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The picture's width is computed from this. A second opinion about how
    /// wide the rail is would put every binding a few pixels from where it
    /// was clicked.
    #[test]
    fn the_open_panel_is_wider_than_the_rail_alone() {
        assert_eq!(width(false), RAIL_W);
        assert_eq!(width(true), RAIL_W + PANEL_W);
        assert!(width(true) > width(false));
    }

    /// The rail never disappears: collapsing hides the panel, not the way
    /// back to it.
    #[test]
    fn collapsing_leaves_the_rail_reachable() {
        assert!(width(false) > 0.0);
    }
}
