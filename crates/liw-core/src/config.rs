//! User configuration.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// The keyboard determined by calibration. Auto-detection is unreliable
    /// (on multi-interface keyboards the capabilities are not distinctive),
    /// so the user's choice is stored permanently.
    pub keyboard: Option<PathBuf>,
    pub mouse: Option<PathBuf>,
    /// Make the Waydroid window fullscreen once the session comes up.
    ///
    /// Default on: touches travel in screen space, so if the window is not
    /// aligned with the output the profile coordinates shift. Still a matter
    /// of preference — a user may want to work in windowed mode.
    #[serde(default = "default_true")]
    pub fullscreen_on_start: bool,
    /// Whether the keymapper starts along with liwd.
    ///
    /// Default on: mapping is one of the daemon's reasons to exist. With it
    /// off, the keymapper vanished SILENTLY after every
    /// `systemctl --user restart liwd` — the user saw "it isn't taking input"
    /// with no way to find out why. This actually happened.
    #[serde(default = "default_true")]
    pub keymapper_on_start: bool,
    /// evdev code of the key that toggles game mode.
    ///
    /// Game mode = devices grabbed + mapping active. With it off the mouse is
    /// free and behaves naturally in menus. Grabbing as soon as a profile
    /// activates is wrong: the mouse locks before the match starts and the
    /// user gets stuck in the menu.
    #[serde(default)]
    pub hotkey_game_mode: Option<u16>,
}

fn default_true() -> bool { true }

impl Default for Config {
    fn default() -> Self {
        Self {
            keyboard: None, mouse: None,
            fullscreen_on_start: true, keymapper_on_start: true,
            hotkey_game_mode: None,
        }
    }
}

impl Config {
    pub fn path() -> PathBuf {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
            })
            .join("liwinux")
            .join("config.toml")
    }

    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    pub fn load_from(p: &Path) -> Self {
        std::fs::read_to_string(p)
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<PathBuf> {
        let p = Self::path();
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(self)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        std::fs::write(&p, text)?;
        Ok(p)
    }
}

#[cfg(test)]
mod tests {
    /// A missing field must default to ON: defaulting to off meant the
    /// keymapper silently not starting for users with an older config.
    #[test]
    fn missing_keymapper_field_defaults_to_true() {
        let c: super::Config = toml::from_str("keyboard = \"/dev/input/event1\"").unwrap();
        assert!(c.keymapper_on_start);
    }

    #[test]
    fn keymapper_autostart_can_be_disabled() {
        let c: super::Config = toml::from_str("keymapper_on_start = false").unwrap();
        assert!(!c.keymapper_on_start);
    }

    use super::*;

    #[test]
    fn missing_file_yields_defaults() {
        let c = Config::load_from(Path::new("/nonexistent/path/config.toml"));
        assert!(c.keyboard.is_none());
    }

    /// If an older config lacks the field the default must be ON, not false.
    #[test]
    fn missing_fullscreen_field_defaults_to_true() {
        let c: Config = toml::from_str("keyboard = \"/dev/input/event23\"").unwrap();
        assert!(c.fullscreen_on_start);
    }

    #[test]
    fn roundtrips_through_toml() {
        let c = Config {
            keyboard: Some(PathBuf::from("/dev/input/event23")),
            mouse: None,
            fullscreen_on_start: true,
            keymapper_on_start: true,
            hotkey_game_mode: Some(40),
        };
        let s = toml::to_string(&c).unwrap();
        let back: Config = toml::from_str(&s).unwrap();
        assert_eq!(back.keyboard, c.keyboard);
        assert_eq!(back.hotkey_game_mode, Some(40));
    }

    /// A corrupt config must not panic; it must fall back to defaults.
    #[test]
    fn corrupt_file_falls_back_to_defaults() {
        let dir = std::env::temp_dir().join("liw-test-cfg");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("corrupt.toml");
        std::fs::write(&p, "this = is not valid toml [[[").unwrap();
        assert!(Config::load_from(&p).keyboard.is_none());
        let _ = std::fs::remove_file(&p);
    }
}
