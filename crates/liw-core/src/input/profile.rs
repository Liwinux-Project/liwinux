//! Oyun başına tuş eşleme profili.
//!
//! Koordinatlar normalize (0..1) tutulur; böylece profil çözünürlükten ve
//! pencere boyutundan bağımsız olur. Bir profili başka bir makinede
//! kullanmak için düzenlemek gerekmez.

use super::touch::Norm;
use serde::{Deserialize, Serialize};

/// Klavye/fare üzerinde bir tetikleyici.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Trigger {
    /// evdev tuş kodu (KEY_W = 17 gibi). Ham kod tutuyoruz ki klavye
    /// düzeninden bağımsız olsun — "W" harfi değil, o fiziksel tuş.
    Key(u16),
    MouseLeft,
    MouseRight,
    MouseMiddle,
    WheelUp,
    WheelDown,
}

/// Bir bağlantının davranışı.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Binding {
    /// Tuş basılıyken parmak ekranda kalır.
    Tap { trigger: Trigger, at: Norm },

    /// Tuşa basıp bırakınca tek dokunuş (basılı tutmak tekrar etmez).
    Toggle { trigger: Trigger, at: Norm },

    /// WASD gibi dört yönlü sanal joystick.
    /// Merkez etrafında `radius` yarıçapında hareket eden tek parmak.
    Joystick {
        up: Trigger, down: Trigger, left: Trigger, right: Trigger,
        center: Norm,
        radius: f32,
    },

    /// FPS nişan alma: bağıl fare hareketi → sürekli sürükleme.
    /// `sensitivity` fare piksel'ini normalize mesafeye çevirir.
    Aim {
        toggle: Option<Trigger>,
        origin: Norm,
        sensitivity: f32,
        /// Bu eşiğin altındaki hareket yok sayılır (fare gürültüsü).
        deadzone: f32,
    },

    /// Kaydırma jesti (Subway Surfers gibi oyunlar için).
    ///
    /// `group` verilirse aynı gruptaki başka bir jest başlarken bu jest
    /// İPTAL EDİLİR. Subway Surfers gibi tek parmakla oynanan oyunlarda
    /// şart: A'ya sonra hızlıca W'ye basınca oyun iki ayrı parmak görmemeli.
    /// Nişancı oyunlarında ise joystick + nişan + ateş eşzamanlı olmalı,
    /// o yüzden dışlayıcılık varsayılan DEĞİL, açıkça istenir.
    Swipe {
        trigger: Trigger,
        from: Norm,
        to: Norm,
        duration_ms: u32,
        #[serde(default)]
        group: Option<String>,
    },
}

impl Binding {
    /// Bu bağlantıyı tetikleyen girdiler.
    pub fn triggers(&self) -> Vec<Trigger> {
        match self {
            Binding::Tap { trigger, .. }
            | Binding::Toggle { trigger, .. }
            | Binding::Swipe { trigger, .. } => vec![trigger.clone()],
            Binding::Joystick { up, down, left, right, .. } =>
                vec![up.clone(), down.clone(), left.clone(), right.clone()],
            Binding::Aim { toggle, .. } =>
                toggle.clone().into_iter().collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    /// Android paket adı; profil otomatik uygulansın diye.
    pub package: String,
    /// Bağlantı adı -> davranış. Ad, işaretçi tahsisinin anahtarıdır.
    pub bindings: std::collections::BTreeMap<String, Binding>,
}

#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error("profil okunamadı: {0}")]
    Io(#[from] std::io::Error),
    #[error("profil ayrıştırılamadı: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("geçersiz profil: {0}")]
    Invalid(String),
}

impl Profile {
    pub fn from_toml(s: &str) -> Result<Self, ProfileError> {
        let p: Profile = toml::from_str(s)?;
        p.validate()?;
        Ok(p)
    }

    /// Profili yüklemeden önce tutarlılığını dener.
    ///
    /// En önemlisi: **aynı tetikleyici birden fazla bağlantıda kullanılamaz.**
    /// Kullanılırsa hangi bağlantının çalışacağı belirsizleşir ve kullanıcı
    /// bunu "bazen çalışıyor" diye yaşar — teşhisi en zor hata türü.
    pub fn validate(&self) -> Result<(), ProfileError> {
        if self.bindings.is_empty() {
            return Err(ProfileError::Invalid("profilde hiç bağlantı yok".into()));
        }
        if self.bindings.len() > super::touch::MAX_POINTERS {
            return Err(ProfileError::Invalid(format!(
                "{} bağlantı var ama Android en fazla {} eşzamanlı işaretçi destekler",
                self.bindings.len(), super::touch::MAX_POINTERS)));
        }
        let mut seen: std::collections::HashMap<Trigger, &str> = Default::default();
        for (name, b) in &self.bindings {
            for t in b.triggers() {
                if let Some(prev) = seen.insert(t.clone(), name) {
                    return Err(ProfileError::Invalid(format!(
                        "aynı tetikleyici iki bağlantıda: '{prev}' ve '{name}'")));
                }
            }
            if let Binding::Joystick { radius, .. } = b {
                if !(0.0..=0.5).contains(radius) {
                    return Err(ProfileError::Invalid(format!(
                        "'{name}' joystick yarıçapı 0..0.5 aralığında olmalı, {radius} verildi")));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
name = "Subway Surfers"
package = "com.kiloo.subwaysurf"

[bindings.sol]
type = "swipe"
trigger = { Key = 30 }
from = { x = 0.5, y = 0.5 }
to = { x = 0.2, y = 0.5 }
duration_ms = 80

[bindings.zipla]
type = "swipe"
trigger = { Key = 57 }
from = { x = 0.5, y = 0.6 }
to = { x = 0.5, y = 0.3 }
duration_ms = 80
"#;

    #[test]
    fn parses_a_real_profile() {
        let p = Profile::from_toml(SAMPLE).unwrap();
        assert_eq!(p.package, "com.kiloo.subwaysurf");
        assert_eq!(p.bindings.len(), 2);
    }

    #[test]
    fn rejects_duplicate_trigger() {
        let dup = SAMPLE.replace("trigger = { Key = 57 }", "trigger = { Key = 30 }");
        let err = Profile::from_toml(&dup).unwrap_err();
        assert!(err.to_string().contains("aynı tetikleyici"), "{err}");
    }

    #[test]
    fn rejects_empty_profile() {
        let err = Profile::from_toml("name=\"x\"\npackage=\"y\"\n[bindings]\n").unwrap_err();
        assert!(err.to_string().contains("hiç bağlantı yok"));
    }

    #[test]
    fn rejects_out_of_range_joystick_radius() {
        let t = r#"
name = "t"
package = "p"
[bindings.hareket]
type = "joystick"
up = { Key = 17 }
down = { Key = 31 }
left = { Key = 30 }
right = { Key = 32 }
center = { x = 0.2, y = 0.7 }
radius = 0.9
"#;
        let err = Profile::from_toml(t).unwrap_err();
        assert!(err.to_string().contains("yarıçapı"), "{err}");
    }

    #[test]
    fn joystick_reports_all_four_triggers() {
        let b = Binding::Joystick {
            up: Trigger::Key(17), down: Trigger::Key(31),
            left: Trigger::Key(30), right: Trigger::Key(32),
            center: Norm::new(0.2, 0.7), radius: 0.1,
        };
        assert_eq!(b.triggers().len(), 4);
    }
}
