//! Typed client for `liwd` (`id.liwinux.Manager1`).
//!
//! Sits beside `helper.rs`, which is the client for the privileged system
//! service. Together they are the ONLY way anything should talk to liwinux's
//! moving parts — a UI that shells out to the `liw` binary and parses its
//! output is a UI that breaks whenever a message is reworded.
//!
//! # Why a proxy trait
//!
//! `zbus::proxy` generates the calls, the property getters AND the change
//! streams. Hand-rolling `Proxy::call` was fine while the CLI only made
//! one-shot calls, but a UI has to *subscribe*: it needs to know the moment
//! game mode flips, not on its next poll.

use crate::apps::App;
use crate::session::Health;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum ManagerError {
    #[error("could not reach liwd: {0}")]
    Connect(#[from] zbus::Error),
    #[error("liwd call failed: {0}")]
    Call(String),
    #[error("could not read liwd's reply: {0}")]
    Decode(#[from] serde_json::Error),
}

#[zbus::proxy(
    interface = "id.liwinux.Manager1",
    default_service = "id.liwinux.Manager1",
    default_path = "/id/liwinux/Manager1"
)]
pub trait Manager1 {
    fn start(&self) -> zbus::Result<()>;
    fn stop(&self) -> zbus::Result<()>;
    fn restart(&self) -> zbus::Result<()>;
    fn health(&self) -> zbus::Result<String>;
    fn status(&self) -> zbus::Result<String>;

    fn start_keymapper(&self, grab: bool) -> zbus::Result<()>;
    fn stop_keymapper(&self) -> zbus::Result<()>;
    fn keymapper_status(&self) -> zbus::Result<String>;

    fn list_apps(&self) -> zbus::Result<String>;
    fn launch_app(&self, package: &str) -> zbus::Result<()>;
    fn open_store_page(&self, package: &str) -> zbus::Result<()>;
    fn install_apk(&self, path: &str) -> zbus::Result<()>;
    fn get_config(&self) -> zbus::Result<String>;
    fn set_config(&self, json: &str) -> zbus::Result<()>;
    fn list_input_devices(&self) -> zbus::Result<String>;

    fn list_profiles(&self) -> zbus::Result<String>;
    fn get_profile(&self, package: &str) -> zbus::Result<String>;
    fn save_profile(&self, json: &str) -> zbus::Result<String>;
    fn delete_profile(&self, package: &str) -> zbus::Result<String>;

    fn fullscreen(&self) -> zbus::Result<bool>;
    fn activate_window(&self) -> zbus::Result<()>;

    #[zbus(property)]
    fn state(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn health_json(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn keymapper_running(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn game_mode(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn grabbed(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn host_focused(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn active_profile(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn foreground_package(&self) -> zbus::Result<String>;

    #[zbus(signal)]
    fn keymapper_event(&self, kind: String, detail: String) -> zbus::Result<()>;
}

/// One row of `ListInputDevices`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputDevice {
    pub path: String,
    /// A `/dev/input/by-id/...` name that survives a reboot, when udev
    /// provides one. eventN numbers are reassigned between boots.
    #[serde(default)]
    pub stable_path: Option<String>,
    pub name: String,
    /// `Keyboard`, `Pointer` or `Combo`.
    pub kind: String,
    /// Created by uinput — ours, or another mapper's. Never a valid source.
    #[serde(rename = "virtual")]
    pub is_virtual: bool,
    /// How much this looks like a real typing keyboard.
    pub typing_score: u32,
    /// Resolved by the daemon, which compares canonical paths — the config
    /// holds a by-id symlink and discovery reports eventN.
    #[serde(default)]
    pub is_keyboard: bool,
    #[serde(default)]
    pub is_mouse: bool,
}

impl InputDevice {
    /// What should be written to the config: the stable name when there is
    /// one, otherwise the node we found it at.
    pub fn config_path(&self) -> String {
        self.stable_path.clone().unwrap_or_else(|| self.path.clone())
    }

    /// The node, for telling two interfaces of one keyboard apart.
    pub fn node(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
}

/// One row of `ListProfiles`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileSummary {
    pub package: String,
    pub name: String,
    pub path: String,
    pub origin: String,
    pub bindings: usize,
    /// Only user profiles can be written or deleted. A UI that offers
    /// Delete on a system profile and then fails looks broken.
    pub editable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrokenProfile {
    pub path: String,
    pub error: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProfileList {
    pub profiles: Vec<ProfileSummary>,
    /// Files that failed to load. Carried alongside rather than dropped:
    /// "why is my profile not working" has to be answerable.
    pub problems: Vec<BrokenProfile>,
}

/// Everything a status bar needs, in one read.
///
/// Grouped into one struct on purpose: a UI that reads eight properties
/// one by one can render a frame with half of them updated, which shows up
/// as flicker between contradictory states.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    pub state: String,
    pub health: Health,
    pub keymapper_running: bool,
    pub game_mode: bool,
    pub grabbed: bool,
    pub host_focused: bool,
    pub active_profile: Option<String>,
    pub foreground: Option<String>,
    /// Our own layer's live latency (microseconds), and how many samples it
    /// rests on. Zero samples means "not measured yet" — a UI must not draw
    /// that as a real zero.
    pub latency_p50_us: u64,
    pub latency_p99_us: u64,
    pub latency_samples: u64,
}

pub struct Manager {
    proxy: Manager1Proxy<'static>,
}

impl Manager {
    pub async fn connect() -> Result<Self, ManagerError> {
        let conn = zbus::Connection::session().await?;
        Ok(Self { proxy: Manager1Proxy::new(&conn).await? })
    }

    pub fn proxy(&self) -> &Manager1Proxy<'static> { &self.proxy }

    /// Reads every status property at once.
    ///
    /// Latency is not a property: it changes continuously and announcing
    /// every tick would be a change signal a few times a second for a number
    /// nobody is watching most of the time. It comes from
    /// `KeymapperStatus`, which a caller reads when it wants it.
    pub async fn snapshot(&self) -> Result<Snapshot, ManagerError> {
        let km: serde_json::Value = self.proxy.keymapper_status().await.ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(serde_json::Value::Null);
        let num = |k: &str| km.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
        // Empty string means "none" on the wire: D-Bus has no null, and a
        // separate presence flag is one more thing for a client to get wrong.
        let none_if_empty = |s: String| if s.is_empty() { None } else { Some(s) };
        Ok(Snapshot {
            state: self.proxy.state().await.unwrap_or_default(),
            health: serde_json::from_str(
                &self.proxy.health_json().await.unwrap_or_default())
                .unwrap_or_default(),
            keymapper_running: self.proxy.keymapper_running().await.unwrap_or(false),
            game_mode: self.proxy.game_mode().await.unwrap_or(false),
            grabbed: self.proxy.grabbed().await.unwrap_or(false),
            host_focused: self.proxy.host_focused().await.unwrap_or(false),
            active_profile: none_if_empty(
                self.proxy.active_profile().await.unwrap_or_default()),
            foreground: none_if_empty(
                self.proxy.foreground_package().await.unwrap_or_default()),
            latency_p50_us: num("latency_p50_us"),
            latency_p99_us: num("latency_p99_us"),
            latency_samples: num("latency_samples"),
        })
    }

    pub async fn apps(&self) -> Result<Vec<App>, ManagerError> {
        let raw = self.proxy.list_apps().await
            .map_err(|e| ManagerError::Call(e.to_string()))?;
        Ok(serde_json::from_str(&raw)?)
    }

    pub async fn launch(&self, package: &str) -> Result<(), ManagerError> {
        self.proxy.launch_app(package).await
            .map_err(|e| ManagerError::Call(friendly(&e)))
    }

    pub async fn open_store(&self, package: &str) -> Result<(), ManagerError> {
        self.proxy.open_store_page(package).await
            .map_err(|e| ManagerError::Call(friendly(&e)))
    }

    pub async fn install_apk(&self, path: &str) -> Result<(), ManagerError> {
        self.proxy.install_apk(path).await
            .map_err(|e| ManagerError::Call(friendly(&e)))
    }

    pub async fn config(&self) -> Result<crate::Config, ManagerError> {
        let raw = self.proxy.get_config().await
            .map_err(|e| ManagerError::Call(friendly(&e)))?;
        Ok(serde_json::from_str(&raw)?)
    }

    pub async fn set_config(&self, c: &crate::Config) -> Result<(), ManagerError> {
        let json = serde_json::to_string(c)?;
        self.proxy.set_config(&json).await
            .map_err(|e| ManagerError::Call(friendly(&e)))
    }

    pub async fn input_devices(&self) -> Result<Vec<InputDevice>, ManagerError> {
        let raw = self.proxy.list_input_devices().await
            .map_err(|e| ManagerError::Call(friendly(&e)))?;
        Ok(serde_json::from_str(&raw)?)
    }

    pub async fn profiles(&self) -> Result<ProfileList, ManagerError> {
        let raw = self.proxy.list_profiles().await
            .map_err(|e| ManagerError::Call(e.to_string()))?;
        Ok(serde_json::from_str(&raw)?)
    }

    pub async fn session_start(&self) -> Result<(), ManagerError> {
        self.proxy.start().await.map_err(|e| ManagerError::Call(friendly(&e)))
    }
    pub async fn session_stop(&self) -> Result<(), ManagerError> {
        self.proxy.stop().await.map_err(|e| ManagerError::Call(friendly(&e)))
    }
    pub async fn keymapper(&self, on: bool, grab: bool) -> Result<(), ManagerError> {
        let r = if on { self.proxy.start_keymapper(grab).await }
                else { self.proxy.stop_keymapper().await };
        r.map_err(|e| ManagerError::Call(friendly(&e)))
    }
}

/// Turns a named D-Bus error into something worth showing a person.
///
/// The names exist so a client can BRANCH; this is the fallback for when it
/// just needs a sentence. Matching on the name rather than the message is the
/// point — the message is prose and may be reworded.
pub fn friendly(e: &zbus::Error) -> String {
    let zbus::Error::MethodError(name, detail, _) = e else {
        return e.to_string();
    };
    let detail = detail.clone().unwrap_or_default();
    match name.as_str() {
        "id.liwinux.Error.NoSession" =>
            "Android is not running. Start the session first.".into(),
        "id.liwinux.Error.NoHelper" =>
            "The privileged helper is not reachable — is liwd-helper installed \
             and started?".into(),
        "id.liwinux.Error.NoProfile" =>
            format!("No key mapping profile for {detail}."),
        "id.liwinux.Error.NoWindow" =>
            "The Waydroid window was not found — is the game open?".into(),
        "id.liwinux.Error.Invalid" => format!("Rejected: {detail}"),
        _ => if detail.is_empty() { name.to_string() } else { detail },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The named errors must become sentences a person can act on. If this
    /// falls back to the raw name, the UI shows "id.liwinux.Error.NoSession"
    /// to someone who just wants to press a button.
    #[test]
    fn named_errors_become_advice() {
        let mk = |n: &str, d: &str| zbus::Error::MethodError(
            zbus::names::OwnedErrorName::try_from(n.to_string()).unwrap(),
            Some(d.to_string()),
            zbus::message::Message::method_call("/x", "y").unwrap()
                .build(&()).unwrap());

        let s = friendly(&mk("id.liwinux.Error.NoSession", "stopped"));
        assert!(s.contains("Start the session"), "{s}");

        let s = friendly(&mk("id.liwinux.Error.NoProfile", "com.x.y"));
        assert!(s.contains("com.x.y"), "the package must survive: {s}");

        // An unknown name still shows the detail rather than the name.
        let s = friendly(&mk("org.other.Error", "something broke"));
        assert_eq!(s, "something broke");
    }

    #[test]
    fn profile_list_parses_the_daemon_shape() {
        let raw = r#"{"profiles":[{"package":"com.x","name":"X","path":"/p",
            "origin":"User","bindings":3,"editable":true}],
            "problems":[{"path":"/bad.toml","error":"boom"}]}"#;
        let l: ProfileList = serde_json::from_str(raw).unwrap();
        assert_eq!(l.profiles[0].package, "com.x");
        assert!(l.profiles[0].editable);
        assert_eq!(l.problems[0].error, "boom");
    }

    /// An empty string on the wire means "none"; it must not reach the UI as
    /// a profile literally named "".
    #[test]
    fn empty_strings_become_none() {
        let f = |s: String| if s.is_empty() { None } else { Some(s) };
        assert_eq!(f(String::new()), None);
        assert_eq!(f("P".into()), Some("P".into()));
    }
}
