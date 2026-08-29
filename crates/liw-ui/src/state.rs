//! Application state and everything that talks to `liwd`.
//!
//! # Push, not poll
//!
//! The daemon announces changes; this file subscribes. A UI that polled
//! `KeymapperStatus` would notice a game-mode toggle whenever its next tick
//! happened to land, and would keep the CPU awake to do it — while a game is
//! running, which is the one time that matters.

use gpui::{App, AppContext as _, Context, Entity, Task};
use gpui_tokio::Tokio;
use liw_core::apps::App as AndroidApp;
use liw_core::manager::{Manager, ProfileList, Snapshot};
use std::collections::HashMap;

use crate::tint::{self, Tint};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    Library,
    Keymap,
    Diagnostics,
    Settings,
}

impl Nav {
    pub const ALL: [Nav; 4] =
        [Nav::Library, Nav::Keymap, Nav::Diagnostics, Nav::Settings];
    pub fn label(self) -> &'static str {
        match self {
            Nav::Library => "Library",
            Nav::Keymap => "Key mapping",
            Nav::Diagnostics => "Diagnostics",
            Nav::Settings => "Settings",
        }
    }
}

/// Whether the daemon is reachable at all.
///
/// Kept apart from the snapshot: "liwd is not running" and "the session is
/// stopped" need different words and different buttons, and collapsing them
/// into one blank screen is how a user ends up restarting the wrong thing.
#[derive(Debug, Clone, PartialEq)]
pub enum Link {
    Connecting,
    Up,
    Down(String),
}

pub struct AppState {
    pub nav: Nav,
    pub link: Link,
    pub snapshot: Snapshot,
    pub apps: Vec<AndroidApp>,
    pub profiles: ProfileList,
    /// User-chosen key art per package, when there is any.
    pub art: HashMap<String, std::path::PathBuf>,
    /// Accent colour per package, read from the app's own icon.
    ///
    /// Computed once at load: decoding a 54x54 PNG is trivial, but doing it
    /// inside `render` would repeat it every frame for every card.
    pub tints: HashMap<String, Tint>,
    /// The app the hero shows. Follows the foreground package while a game
    /// is running, otherwise the last one the user launched.
    pub featured: Option<String>,
    /// Free-text filter from the search box.
    pub search: String,
    /// The user configuration, once loaded.
    pub config: Option<liw_core::Config>,
    /// Input devices that could be mapped from.
    pub devices: Vec<liw_core::manager::InputDevice>,
    /// Focus for the search field.
    ///
    /// gpui ships no text input — the editor lives in Zed's workspace
    /// crates, not in gpui — so this is a focusable div that collects key
    /// events. Enough for a filter; it is not an editor and does not pretend
    /// to be one (no selection, no caret movement, no clipboard).
    pub search_focus: gpui::FocusHandle,
    /// Show system apps in the library. Off by default: half the list is
    /// Settings and Calculator.
    pub show_system: bool,
    /// Last thing that went wrong, for the banner.
    pub error: Option<String>,
    /// A long call is in flight; the button that started it stays disabled.
    pub busy: Option<&'static str>,
    _watch: Option<Task<()>>,
}

impl AppState {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let search_focus = cx.focus_handle();
        let mut s = Self {
            nav: Nav::Library,
            link: Link::Connecting,
            snapshot: Snapshot::default(),
            apps: Vec::new(),
            profiles: ProfileList::default(),
            art: HashMap::new(),
            tints: HashMap::new(),
            featured: None,
            search: String::new(),
            config: None,
            devices: Vec::new(),
            search_focus,
            show_system: false,
            error: None,
            busy: None,
            _watch: None,
        };
        s.reload(cx);
        s
    }

    /// Apps the library grid shows: user apps, minus the search filter.
    pub fn visible_apps(&self) -> impl Iterator<Item = &AndroidApp> {
        let q = self.search.trim().to_lowercase();
        self.apps.iter()
            .filter(move |a| self.show_system || !a.system)
            .filter(move |a| q.is_empty()
                || a.name.to_lowercase().contains(&q)
                || a.package.to_lowercase().contains(&q))
    }

    pub fn art(&self, package: &str) -> Option<&std::path::Path> {
        self.art.get(package).map(|p| p.as_path())
    }

    /// Applies one key press to the search field.
    pub fn search_key(&mut self, ev: &gpui::KeyDownEvent, window: &mut gpui::Window) -> bool {
        let k = &ev.keystroke;
        let m = &k.modifiers;
        match search_edit(&mut self.search, &k.key, k.key_char.as_deref(),
                          m.control || m.alt || m.platform) {
            SearchAction::Changed => true,
            SearchAction::Blur => { window.blur(); false }
            SearchAction::Nothing => false,
        }
    }

    pub fn tint(&self, package: &str) -> Tint {
        self.tints.get(package).copied().unwrap_or(Tint::NEUTRAL)
    }

    /// The app the hero should show.
    ///
    /// Prefers whatever Android currently has in the foreground: while a game
    /// is running that is the one thing the user cares about. Falls back to
    /// the last launched, then to the first app with a key mapping — an
    /// empty hero on first run would look broken.
    pub fn hero(&self) -> Option<&AndroidApp> {
        // While searching, the hero follows the search. Showing a game the
        // filter just excluded reads as the page ignoring you.
        if !self.search.trim().is_empty() {
            return self.visible_apps().find(|a| a.package != "com.android.vending");
        }
        let pick = self.snapshot.foreground.as_deref()
            .filter(|p| self.apps.iter().any(|a| a.package == *p))
            .or(self.featured.as_deref())
            .map(str::to_string);
        if let Some(p) = pick {
            if let Some(a) = self.apps.iter().find(|a| a.package == p) { return Some(a); }
        }
        // The store is never the hero: it is a way to get games, not one.
        let is_game = |a: &&AndroidApp| !a.system && a.package != "com.android.vending";
        self.apps.iter().find(|a| is_game(a) && self.has_profile(&a.package))
            .or_else(|| self.apps.iter().find(is_game))
    }

    pub fn has_profile(&self, package: &str) -> bool {
        self.profiles.profiles.iter().any(|p| p.package == package)
    }

    /// Loads everything and then keeps the snapshot fresh.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let load = Tokio::spawn(cx, async move {
            let m = Manager::connect().await.map_err(|e| e.to_string())?;
            // The app list comes from `.desktop` files, so it works with the
            // session stopped — the library must draw before Android is up.
            let apps = m.apps().await.unwrap_or_default();
            let profiles = m.profiles().await.unwrap_or_default();
            let snap = m.snapshot().await.map_err(|e| e.to_string())?;
            // Icon colours are read here, off the UI thread: `render` runs
            // per frame and must never touch the disk.
            let tints: HashMap<String, Tint> = apps.iter()
                .filter_map(|a| {
                    let p = a.icon.as_ref()?;
                    let bytes = std::fs::read(p).ok()?;
                    Some((a.package.clone(), tint::from_bytes(&bytes)))
                })
                .collect();
            let config = m.config().await.ok();
            let devices = m.input_devices().await.unwrap_or_default();
            let art: HashMap<String, std::path::PathBuf> = apps.iter()
                .filter_map(|a| Some((a.package.clone(),
                                      liw_core::art::art_for(&a.package)?)))
                .collect();
            Ok::<_, String>((apps, profiles, snap, tints, art, config, devices))
        });

        cx.spawn(async move |this, cx| {
            let outcome = match load.await {
                Ok(Ok(v)) => Ok(v),
                Ok(Err(e)) => Err(e),
                Err(e) => Err(e.to_string()),
            };
            let _ = this.update(cx, |s, cx| {
                match outcome {
                    Ok((apps, profiles, snap, tints, art, config, devices)) => {
                        s.apps = apps;
                        s.profiles = profiles;
                        s.snapshot = snap;
                        s.tints = tints;
                        s.art = art;
                        s.config = config;
                        s.devices = devices;
                        s.link = Link::Up;
                        s.start_watching(cx);
                    }
                    Err(e) => s.link = Link::Down(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Subscribes to the daemon's change signals.
    ///
    /// Any announced change re-reads the whole snapshot rather than patching
    /// one field. Eight properties patched individually can render a frame
    /// where half are updated, which looks like flicker between two
    /// contradictory states; one read is cheap and always self-consistent.
    fn start_watching(&mut self, cx: &mut Context<Self>) {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Snapshot>(8);
        let pump = Tokio::spawn(cx, async move {
            use futures::StreamExt as _;
            let Ok(m) = Manager::connect().await else { return };
            let Ok(conn) = zbus::Connection::session().await else { return };
            // ONE stream for all properties. `PropertiesChanged` carries
            // whatever changed; subscribing per property would mean eight
            // streams and eight wake-ups for what is one event on the wire.
            let props = match zbus::fdo::PropertiesProxy::builder(&conn)
                .destination("id.liwinux.Manager1")
                .and_then(|b| b.path("/id/liwinux/Manager1"))
            {
                Ok(b) => match b.build().await { Ok(p) => p, Err(_) => return },
                Err(_) => return,
            };
            let Ok(mut changes) = props.receive_properties_changed().await else { return };
            // Latency is the one figure that has to be POLLED.
            //
            // Everything else is a fact that changes when something happens,
            // so it arrives as a signal. Latency changes continuously —
            // making it a property would mean a change signal several times a
            // second for a number nobody is watching most of the time. So it
            // is read on a slow tick, and only while the keymapper is
            // actually running.
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut mapping = false;
            loop {
                tokio::select! {
                    ev = changes.next() => {
                        if ev.is_none() { return }
                    }
                    _ = tick.tick(), if mapping => {}
                }
                let Ok(s) = m.snapshot().await else { continue };
                mapping = s.keymapper_running;
                if tx.send(s).await.is_err() { return; }
            }
        });

        let task = cx.spawn(async move |this, cx| {
            // Hold the pump for as long as the view lives; dropping the task
            // would cancel the subscription and the UI would silently freeze
            // on its last known state.
            let _pump = pump;
            while let Some(s) = rx.recv().await {
                if this.update(cx, |st, cx| {
                    if st.snapshot != s {
                        st.snapshot = s;
                        cx.notify();
                    }
                })
                .is_err()
                {
                    return;
                }
            }
        });
        self._watch = Some(task);
    }

    pub fn launch(&mut self, package: String, cx: &mut Context<Self>) {
        self.featured = Some(package.clone());
        self.error = None;
        self.busy = Some("launch");
        cx.notify();
        let t = Tokio::spawn(cx, async move {
            let m = Manager::connect().await.map_err(|e| e.to_string())?;
            m.launch(&package).await.map_err(|e| e.to_string())
        });
        cx.spawn(async move |this, cx| {
            let r = match t.await {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(e),
                Err(e) => Some(e.to_string()),
            };
            let _ = this.update(cx, |s, cx| { s.busy = None; s.error = r; cx.notify(); });
        })
        .detach();
    }

    /// Opens the Play Store at a package's page.
    pub fn open_store(&mut self, package: String, cx: &mut Context<Self>) {
        self.error = None;
        self.busy = Some("store");
        cx.notify();
        let t = Tokio::spawn(cx, async move {
            let m = Manager::connect().await.map_err(|e| e.to_string())?;
            m.open_store(&package).await.map_err(|e| e.to_string())
        });
        cx.spawn(async move |this, cx| {
            let r = match t.await {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(e),
                Err(e) => Some(e.to_string()),
            };
            let _ = this.update(cx, |s, cx| { s.busy = None; s.error = r; cx.notify(); });
        })
        .detach();
    }

    /// Asks for an APK and installs it.
    ///
    /// A picker rather than a text field: the path is the one thing a person
    /// cannot be expected to type correctly, and the daemon rejects anything
    /// that is not a readable .apk anyway.
    pub fn pick_and_install(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true, directories: false, multiple: false, prompt: None,
        });
        cx.spawn(async move |this, cx| {
            let picked = match paths.await {
                Ok(Ok(Some(p))) => p.into_iter().next(),
                _ => None,
            };
            let Some(path) = picked else { return };
            let path = path.display().to_string();
            let _ = this.update(cx, |s, cx| {
                s.busy = Some("install");
                cx.notify();
            });
            let r = match Manager::connect().await {
                Ok(m) => m.install_apk(&path).await.err().map(|e| e.to_string()),
                Err(e) => Some(e.to_string()),
            };
            let _ = this.update(cx, |s, cx| {
                s.busy = None;
                s.error = r;
                cx.notify();
            });
            // The library is read from Waydroid's desktop entries, which it
            // writes a moment after the install finishes.
            let _ = this.update(cx, |s, cx| s.reload(cx));
        })
        .detach();
    }

    /// Saves the configuration after `edit` has changed it.
    ///
    /// Optimistic: the local copy is updated first so the control responds at
    /// once. A save that fails puts the message in the banner rather than
    /// silently reverting the switch under the cursor.
    pub fn edit_config<F>(&mut self, edit: F, cx: &mut Context<Self>)
    where
        F: FnOnce(&mut liw_core::Config),
    {
        let Some(cfg) = self.config.as_mut() else { return };
        edit(cfg);
        let snapshot = cfg.clone();
        self.error = None;
        cx.notify();
        let t = Tokio::spawn(cx, async move {
            let m = Manager::connect().await.map_err(|e| e.to_string())?;
            m.set_config(&snapshot).await.map_err(|e| e.to_string())
        });
        cx.spawn(async move |this, cx| {
            let r = match t.await {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(e),
                Err(e) => Some(e.to_string()),
            };
            let _ = this.update(cx, |s, cx| { s.error = r; cx.notify(); });
        })
        .detach();
    }

    pub fn session(&mut self, start: bool, cx: &mut Context<Self>) {
        self.error = None;
        self.busy = Some(if start { "session-start" } else { "session-stop" });
        cx.notify();
        let t = Tokio::spawn(cx, async move {
            let m = Manager::connect().await.map_err(|e| e.to_string())?;
            if start { m.session_start().await } else { m.session_stop().await }
                .map_err(|e| e.to_string())
        });
        cx.spawn(async move |this, cx| {
            let r = match t.await {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(e),
                Err(e) => Some(e.to_string()),
            };
            let _ = this.update(cx, |s, cx| { s.busy = None; s.error = r; cx.notify(); });
        })
        .detach();
    }
}

/// What one key press does to the search text.
#[derive(Debug, PartialEq, Eq)]
pub enum SearchAction {
    Changed,
    /// Give focus back to the page.
    Blur,
    Nothing,
}

/// The whole text-editing rule, separated from gpui so it can be tested.
///
/// It is not an editor: no selection, no caret movement, no clipboard. It is
/// a filter box, and the parts worth getting right are the ones that fail
/// silently — a modifier chord typing a letter, a control character landing
/// in the string, an unbounded field.
pub fn search_edit(
    text: &mut String, key: &str, key_char: Option<&str>, modified: bool,
) -> SearchAction {
    match key {
        "backspace" => {
            if text.pop().is_some() { SearchAction::Changed } else { SearchAction::Nothing }
        }
        "escape" => {
            // Clears if there is anything, otherwise hands focus back. Making
            // an empty field take two presses to escape would be one too many.
            if text.is_empty() {
                SearchAction::Blur
            } else {
                text.clear();
                SearchAction::Changed
            }
        }
        "enter" | "tab" => SearchAction::Blur,
        _ => {
            // Modifier chords are shortcuts, not text: ctrl-a must not append
            // an "a".
            if modified { return SearchAction::Nothing }
            // `key_char` is what the layout would actually have typed, so a
            // Turkish keyboard gives ğ and ş rather than the ASCII key
            // underneath them.
            let Some(c) = key_char else { return SearchAction::Nothing };
            if c.is_empty() || c.chars().any(char::is_control) {
                return SearchAction::Nothing;
            }
            // Bounded: a filter box has no business holding a novel, and an
            // unbounded one is a slow leak nobody notices.
            if text.chars().count() >= 64 { return SearchAction::Nothing }
            text.push_str(c);
            SearchAction::Changed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn type_str(t: &mut String, s: &str) {
        for ch in s.chars() {
            let c = ch.to_string();
            search_edit(t, &c, Some(&c), false);
        }
    }

    #[test]
    fn typing_appends_and_backspace_removes() {
        let mut t = String::new();
        type_str(&mut t, "sub");
        assert_eq!(t, "sub");
        assert_eq!(search_edit(&mut t, "backspace", None, false), SearchAction::Changed);
        assert_eq!(t, "su");
    }

    /// Backspace on an empty field must not claim a change; the caller
    /// redraws on Changed and a held key would repaint the library forever.
    #[test]
    fn backspace_on_empty_changes_nothing() {
        let mut t = String::new();
        assert_eq!(search_edit(&mut t, "backspace", None, false), SearchAction::Nothing);
    }

    /// The layout's character is what gets typed, not the ASCII key under it.
    #[test]
    fn layout_characters_are_kept() {
        let mut t = String::new();
        // A Turkish keyboard reports key "g" with key_char "ğ".
        search_edit(&mut t, "g", Some("ğ"), false);
        search_edit(&mut t, "s", Some("ş"), false);
        assert_eq!(t, "ğş");
    }

    /// ctrl-a is a shortcut. Appending "a" for it is the kind of bug that
    /// only shows up as "the search box does weird things".
    #[test]
    fn modifier_chords_are_not_text() {
        let mut t = String::new();
        assert_eq!(search_edit(&mut t, "a", Some("a"), true), SearchAction::Nothing);
        assert!(t.is_empty());
    }

    #[test]
    fn control_characters_never_enter_the_string() {
        let mut t = String::new();
        for c in ["\u{7f}", "\n", "\t", ""] {
            assert_eq!(search_edit(&mut t, "x", Some(c), false), SearchAction::Nothing, "{c:?}");
        }
        assert!(t.is_empty());
    }

    /// Escape clears first, and only gives focus back once there is nothing
    /// left — otherwise leaving a filled field takes two presses.
    #[test]
    fn escape_clears_then_blurs() {
        let mut t = String::from("abc");
        assert_eq!(search_edit(&mut t, "escape", None, false), SearchAction::Changed);
        assert!(t.is_empty());
        assert_eq!(search_edit(&mut t, "escape", None, false), SearchAction::Blur);
    }

    #[test]
    fn the_field_is_bounded() {
        let mut t = String::new();
        type_str(&mut t, &"x".repeat(200));
        assert_eq!(t.chars().count(), 64);
    }
}

pub fn build(cx: &mut App) -> Entity<AppState> {
    cx.new(AppState::new)
}
