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
    pub const ALL: [Nav; 4] = [Nav::Library, Nav::Keymap, Nav::Diagnostics, Nav::Settings];
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
        let mut s = Self {
            nav: Nav::Library,
            link: Link::Connecting,
            snapshot: Snapshot::default(),
            apps: Vec::new(),
            profiles: ProfileList::default(),
            tints: HashMap::new(),
            featured: None,
            search: String::new(),
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
        let pick = self.snapshot.foreground.as_deref()
            .filter(|p| self.apps.iter().any(|a| a.package == *p))
            .or(self.featured.as_deref())
            .map(str::to_string);
        if let Some(p) = pick {
            if let Some(a) = self.apps.iter().find(|a| a.package == p) { return Some(a); }
        }
        self.apps.iter().find(|a| !a.system && self.has_profile(&a.package))
            .or_else(|| self.apps.iter().find(|a| !a.system))
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
            Ok::<_, String>((apps, profiles, snap, tints))
        });

        cx.spawn(async move |this, cx| {
            let outcome = match load.await {
                Ok(Ok(v)) => Ok(v),
                Ok(Err(e)) => Err(e),
                Err(e) => Err(e.to_string()),
            };
            let _ = this.update(cx, |s, cx| {
                match outcome {
                    Ok((apps, profiles, snap, tints)) => {
                        s.apps = apps;
                        s.profiles = profiles;
                        s.snapshot = snap;
                        s.tints = tints;
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
            while changes.next().await.is_some() {
                let Ok(s) = m.snapshot().await else { continue };
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

pub fn build(cx: &mut App) -> Entity<AppState> {
    cx.new(AppState::new)
}
