//! Kullanıcı yapılandırması.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Kalibrasyonla belirlenmiş klavye. Otomatik tespit güvenilmez olduğu
    /// için (çoklu arayüzlü klavyelerde yetenekler ayırt edici değil)
    /// kullanıcının seçimi kalıcı olarak saklanır.
    pub keyboard: Option<PathBuf>,
    pub mouse: Option<PathBuf>,
    /// Session ayağa kalkınca Waydroid penceresini tam ekran yap.
    ///
    /// Varsayılan açık: dokunuşlar ekran uzayında gittiği için pencere
    /// çıkışla hizalı olmazsa profil koordinatları kayar. Yine de bir
    /// tercih meselesi — kullanıcı pencere modunda çalışmak isteyebilir.
    #[serde(default = "default_true")]
    pub fullscreen_on_start: bool,
    /// Oyun kipini açıp kapatan tuşun evdev kodu.
    ///
    /// Oyun kipi = cihazlar kilitli + eşleme etkin. Kapalıyken fare
    /// serbesttir ve menülerde doğal çalışır. Profil etkinleşir etkinleşmez
    /// kilitlemek yanlış: maç başlamadan fare kilitleniyor ve kullanıcı
    /// menüde sıkışıyor.
    #[serde(default)]
    pub hotkey_game_mode: Option<u16>,
}

fn default_true() -> bool { true }

impl Default for Config {
    fn default() -> Self {
        Self {
            keyboard: None, mouse: None,
            fullscreen_on_start: true, hotkey_game_mode: None,
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
    use super::*;

    #[test]
    fn missing_file_yields_defaults() {
        let c = Config::load_from(Path::new("/olmayan/yol/config.toml"));
        assert!(c.keyboard.is_none());
    }

    /// Eski yapılandırmada alan yoksa varsayılan AÇIK olmalı, false değil.
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
            hotkey_game_mode: Some(40),
        };
        let s = toml::to_string(&c).unwrap();
        let back: Config = toml::from_str(&s).unwrap();
        assert_eq!(back.keyboard, c.keyboard);
        assert_eq!(back.hotkey_game_mode, Some(40));
    }

    /// Bozuk yapılandırma çökmemeli, varsayılana dönmeli.
    #[test]
    fn corrupt_file_falls_back_to_defaults() {
        let dir = std::env::temp_dir().join("liw-test-cfg");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("bozuk.toml");
        std::fs::write(&p, "bu = geçerli toml değil [[[").unwrap();
        assert!(Config::load_from(&p).keyboard.is_none());
        let _ = std::fs::remove_file(&p);
    }
}
