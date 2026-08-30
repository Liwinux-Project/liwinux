//! Settings: which devices to map from, and what happens at startup.
//!
//! Everything here already existed in `config.toml`; what was missing was any
//! way to change it without a text editor and a manual `liw keymap detect`.

use gpui::{Context, ElementId, IntoElement, SharedString, div, prelude::*, px};
use liw_core::manager::InputDevice;

use crate::state::{AppState, Link};
use crate::theme::{Theme, RADIUS, S1, S2, S3, S4, S6};

pub fn render(s: &AppState, t: &Theme, cx: &mut Context<AppState>) -> gpui::AnyElement {
    let body: gpui::AnyElement = match (&s.link, &s.config) {
        (Link::Down(_), _) => note(t, "liwd is not running, so there is nothing to edit."),
        (_, None) => note(t, "Loading…"),
        (_, Some(c)) => div()
            .flex()
            .flex_col()
            .gap(px(S4))
            .child(devices_panel(s, t, "Keyboard", true, c.keyboard.clone(), cx))
            .child(devices_panel(s, t, "Mouse", false, c.mouse.clone(), cx))
            .child(toggles(s, t, cx))
            .child(hotkey(s, t, cx))
            .into_any_element(),
    };

    div()
        .id("settings")
        .flex()
        .flex_col()
        .size_full()
        .gap(px(S4))
        .overflow_y_scroll()
        .child(
            div()
                .flex()
                .flex_col()
                .flex_none()
                .gap(px(S1))
                .child(
                    div()
                        .text_size(px(18.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("Settings"),
                )
                .child(
                    div().text_size(px(12.0)).text_color(t.text_faint).child(
                        "Device changes take effect the next time the key \
                         mapper starts.",
                    ),
                ),
        )
        .child(body)
        .into_any_element()
}

/// Picking the keyboard or the mouse.
///
/// A list rather than auto-detection. Auto-detection is genuinely unreliable
/// here: a gaming keyboard presents several interfaces that all claim letter
/// keys and only one of them sends anything. The typing score is shown so the
/// likely one is visible, but the choice stays the user's.
fn devices_panel(
    s: &AppState, t: &Theme, title: &'static str, want_keyboard: bool,
    current: Option<std::path::PathBuf>, cx: &mut Context<AppState>,
) -> gpui::AnyElement {
    let _ = current;
    let usable: Vec<&InputDevice> = s.devices.iter()
        .filter(|d| !d.is_virtual)
        .filter(|d| if want_keyboard {
            d.kind == "Keyboard" || d.kind == "Combo"
        } else {
            d.kind == "Pointer" || d.kind == "Combo"
        })
        .collect();

    let mut rows = Vec::with_capacity(usable.len());
    for d in usable {
        // Resolved by the daemon against canonical paths; the UI must not
        // try to match a by-id symlink against an eventN node itself.
        let selected = if want_keyboard { d.is_keyboard } else { d.is_mouse };
        let path = d.config_path();
        let node = d.node().to_string();
        let id: ElementId =
            SharedString::from(format!("dev-{title}-{}", d.path)).into();
        rows.push(
            div()
                .id(id)
                .flex()
                .flex_row()
                .items_center()
                .gap(px(S3))
                .px(px(S3))
                .py(px(S2))
                .rounded(px(RADIUS))
                .when(selected, |e| e.bg(t.raised))
                .cursor_pointer()
                .hover(|x| x.bg(t.raised))
                .child(
                    div()
                        .w(px(8.0))
                        .h(px(8.0))
                        .flex_none()
                        .rounded(px(4.0))
                        .bg(if selected { t.accent } else { t.border }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(13.0))
                        .text_color(if selected { t.text } else { t.text_muted })
                        .truncate()
                        .child(SharedString::from(d.name.clone())),
                )
                // The node is shown because one keyboard appears several
                // times with identical names and identical scores — that is
                // exactly the trap this list exists to expose.
                .child(
                    div()
                        .text_size(px(10.0))
                        .text_color(t.text_faint)
                        .child(SharedString::from(node)),
                )
                .when(want_keyboard && d.typing_score > 0, |el| {
                    el.child(
                        div()
                            .w(px(96.0))
                            .text_size(px(10.0))
                            .text_color(t.text_faint)
                            .child(SharedString::from(
                                format!("{}/22 keys", d.typing_score))),
                    )
                })
                // The same trap on the mouse side: a keyboard publishes a
                // pointer node that looks like a mouse until you compare
                // scores. Showing it is what makes a wrong pick visible.
                .when(!want_keyboard && d.pointer_score > 0, |el| {
                    el.child(
                        div()
                            .w(px(96.0))
                            .text_size(px(10.0))
                            .text_color(t.text_faint)
                            .child(SharedString::from(
                                format!("mouse {}/15", d.pointer_score))),
                    )
                })
                .on_click(cx.listener(move |st: &mut AppState, _, _, cx| {
                    let p = std::path::PathBuf::from(path.clone());
                    st.edit_config(|c| {
                        if want_keyboard { c.keyboard = Some(p) } else { c.mouse = Some(p) }
                    }, cx);
                    // Saving alone changes nothing that is already running.
                    // Choosing a different keyboard is the one setting whose
                    // whole point is to take effect now.
                    st.restart_keymapper(cx);
                }))
                .into_any_element(),
        );
    }

    let detect = div()
        .id(if want_keyboard { "det-kb" } else { "det-mouse" })
        .px(px(S3))
        .py(px(S1))
        .rounded(px(RADIUS))
        .border_1()
        .border_color(t.border)
        .text_size(px(12.0))
        .cursor_pointer()
        .hover(|x| x.bg(t.raised))
        .child("Detect")
        .on_click(cx.listener(|st: &mut AppState, _, _, cx| st.autodetect_devices(cx)));

    panel(t, title,
        if rows.is_empty() {
            "None found. Is the user in the `input` group?"
        } else if want_keyboard {
            "Multi-interface keyboards claim letter keys on interfaces that \
             never send any — the score is a hint, not an answer."
        } else {
            "The device the aim binding reads relative motion from."
        },
        div()
            .flex()
            .flex_col()
            .gap(px(S2))
            .when(!rows.is_empty(), |el| {
                el.child(div().flex().flex_col().gap(px(2.0)).children(rows))
            })
            .child(detect)
            .into_any_element())
}

fn toggles(s: &AppState, t: &Theme, cx: &mut Context<AppState>) -> gpui::AnyElement {
    let Some(c) = s.config.as_ref() else { return div().into_any_element() };
    let (fs, km) = (c.fullscreen_on_start, c.keymapper_on_start);
    panel(t, "Startup", "What liwd does when it comes up.",
        div()
            .flex()
            .flex_col()
            .gap(px(S2))
            .child(toggle(t, "tg-fs", "Fullscreen the window", fs,
                "Touches travel in screen space, so a window that is not \
                 aligned with the output shifts every profile coordinate.",
                cx, |c| c.fullscreen_on_start = !c.fullscreen_on_start))
            .child(toggle(t, "tg-km", "Start the key mapper", km,
                "With this off the mapper vanishes silently after every \
                 restart, and all you see is that input stopped working.",
                cx, |c| c.keymapper_on_start = !c.keymapper_on_start))
            .into_any_element())
}

fn toggle<F>(
    t: &Theme, id: &'static str, label: &'static str, on: bool, why: &'static str,
    cx: &mut Context<AppState>, edit: F,
) -> gpui::AnyElement
where
    F: Fn(&mut liw_core::Config) + Copy + 'static,
{
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(S3))
        .p(px(S2))
        .rounded(px(RADIUS))
        .cursor_pointer()
        .hover(|x| x.bg(t.raised))
        .child(
            div()
                .w(px(32.0))
                .h(px(18.0))
                .flex_none()
                .rounded(px(9.0))
                .p(px(2.0))
                .bg(if on { t.accent } else { t.border })
                .flex()
                .when(on, |e| e.justify_end())
                .child(div().w(px(14.0)).h(px(14.0)).rounded(px(7.0)).bg(t.bg)),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(1.0))
                .child(div().text_size(px(13.0)).text_color(t.text).child(label))
                .child(div().text_size(px(10.0)).text_color(t.text_faint).child(why)),
        )
        .on_click(cx.listener(move |st, _, _, cx| st.edit_config(edit, cx)))
        .into_any_element()
}

/// Game mode hotkey.
///
/// Read-only for now. Capturing it here would mean grabbing the keyboard from
/// inside this window, and `liw keymap detect --hotkey --save` already does
/// that properly from a terminal where nothing else is listening.
fn hotkey(s: &AppState, t: &Theme, cx: &mut Context<AppState>) -> gpui::AnyElement {
    let code = s.config.as_ref().and_then(|c| c.hotkey_game_mode);
    let capturing = s.capturing_hotkey;
    panel(t, "Game mode hotkey",
        "Game mode grabs the devices and turns mapping on. Without a hotkey \
         it grabs as soon as a profile activates, which locks the mouse \
         before the match has started.",
        div()
            .flex()
            .flex_col()
            .gap(px(S2))
            .child(match (capturing, code) {
                (true, _) => div()
                    .text_size(px(13.0))
                    .text_color(t.accent)
                    .child("Press the key you want…")
                    .into_any_element(),
                // The name, not the number. "evdev code 40" is what the file
                // holds; it is not what anyone pressed.
                (false, Some(c)) => div()
                    .text_size(px(13.0))
                    .text_color(t.text)
                    .child(SharedString::from(crate::keys::label(c)))
                    .into_any_element(),
                (false, None) => div()
                    .text_size(px(13.0))
                    .text_color(t.warn)
                    .child("Not set — the mapper will grab as soon as a game opens")
                    .into_any_element(),
            })
            .child(
                div()
                    .id("hk-set")
                    .track_focus(&s.hotkey_focus)
                    .px(px(S3))
                    .py(px(S1))
                    .rounded(px(RADIUS))
                    .border_1()
                    .border_color(if capturing { t.accent } else { t.border })
                    .text_size(px(12.0))
                    .cursor_pointer()
                    .hover(|x| x.bg(t.raised))
                    .child(SharedString::from(if capturing { "Cancel" } else { "Set a key" }))
                    .on_click(cx.listener(|st: &mut AppState, _, window, cx| {
                        st.capturing_hotkey = !st.capturing_hotkey;
                        if st.capturing_hotkey {
                            window.focus(&st.hotkey_focus, cx);
                        }
                        cx.notify();
                    }))
                    .on_key_down(cx.listener(
                        |st: &mut AppState, ev: &gpui::KeyDownEvent, _, cx| {
                            st.take_hotkey(&ev.keystroke.key, cx);
                        },
                    )),
            )
            .into_any_element())
}


fn panel(t: &Theme, title: &str, note: &str, body: gpui::AnyElement) -> gpui::AnyElement {
    div()
        .flex()
        .flex_col()
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

fn note(t: &Theme, text: &str) -> gpui::AnyElement {
    div()
        .p(px(S6))
        .rounded(px(RADIUS))
        .bg(t.surface)
        .text_size(px(13.0))
        .text_color(t.text_muted)
        .child(SharedString::from(text.to_string()))
        .into_any_element()
}
