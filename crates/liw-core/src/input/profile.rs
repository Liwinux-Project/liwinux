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

/// Kaydırmanın hız eğrisi.
///
/// Oyunlar kaydırma yönünü belirli bir mesafe eşiği aşılınca anlar. Doğrusal
/// eğride bu eşik jestin ortalarında aşılır; öne yüklenmiş eğride çok daha
/// erken. Yani eğri, oyunun tepki verme anını doğrudan etkiler.
///
/// Varsayılan `Linear` — hangi eğrinin hangi oyunda iyi hissettireceği
/// ölçülmeden bilinemez, o yüzden davranışı sessizce değiştirmiyoruz.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Easing {
    /// Sabit hız.
    #[default]
    Linear,
    /// Hızlı başlar, yavaşlar (quadratic ease-out). Mesafenin %75'i
    /// sürenin yarısında katedilir.
    EaseOut,
    /// Daha da öne yüklü (cubic). Mesafenin %87.5'i sürenin yarısında.
    /// Çok agresif olursa jest "ışınlanma" gibi görünüp tanınmayabilir.
    EaseOutStrong,
}

impl Easing {
    /// Normalize ilerlemeyi (0..1) katedilen mesafe oranına çevirir.
    ///
    /// Uç noktalar korunur: f(0)=0, f(1)=1. Aksi halde jest hedefe
    /// varmaz veya başlangıçtan sıçrar.
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Easing::Linear => t,
            Easing::EaseOut => 1.0 - (1.0 - t) * (1.0 - t),
            Easing::EaseOutStrong => {
                let inv = 1.0 - t;
                1.0 - inv * inv * inv
            }
        }
    }
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
    ///
    /// `toggle` YOKSA nişan her zaman etkindir — FPS'te fare bakışı
    /// sürekli olmalı, tuşa basılı tutmak gerekmemeli.
    Aim {
        #[serde(default)]
        toggle: Option<Trigger>,
        origin: Norm,
        /// Fare pikselini normalize mesafeye çevirir.
        sensitivity: f32,
        /// Bu eşiğin altındaki hareket yok sayılır (fare gürültüsü).
        deadzone: f32,
        /// Parmak ekran kenarına bu kadar yaklaşınca kaldırılıp merkeze
        /// geri konur.
        ///
        /// Bu ŞART: parmak sonsuza kadar sürüklenemez. Yeniden ortalama
        /// olmadan sınırlı açıdan fazla dönemezsin. Gerçek oyuncular da
        /// aynı şeyi yapar, oyunlar bunu bekler.
        #[serde(default = "default_recenter_margin")]
        recenter_margin: f32,
        /// Devir teslimle yeniden ortalama.
        ///
        /// Açıkken ikinci parmak, birincisi kenara VARMADAN merkeze iner ve
        /// ikisi birlikte hareket eder; birincisi ancak sonra bırakılır.
        /// Böylece ekranda her an hareket eden bir parmak olur.
        ///
        /// Kapalıyken basit yol: kaldır, merkeze koy, devam et. O yolda
        /// devir anında dönüş kesiliyor ve oyunun dokunuş yumuşatması
        /// sıfırlandığı için "duruyor sonra devam ediyor" hissi oluşuyor.
        #[serde(default = "default_true_handoff")]
        handoff: bool,
        /// Doğrusal olmayan ölçekleme.
        ///
        /// Parmak merkezden uzaklaştıkça hassasiyet `sqrt(min/uzaklık)` ile
        /// düşer; parmak kenara asimptotik yaklaşır ve pratikte hiç varmaz.
        /// Yeniden yerleşme ihtiyacı böylece BAŞTAN doğmaz.
        ///
        /// XtMapper'ın MouseAimHandler'ından alındı (GPL-3, aynı fikir).
        #[serde(default = "default_true_nonlinear")]
        nonlinear: bool,
        /// Kalkış ile inişin arasına konan gecikme (ms).
        ///
        /// Aynı karede göndermek Android'in kalkışı gerçek bir dokunuş sonu
        /// olarak işlemesine fırsat vermiyor; oyun ışınlanma görüyor.
        #[serde(default = "default_reset_delay_ms")]
        reset_delay_ms: u32,
        /// Parmak ekranın DIŞINA çıkabilsin mi.
        ///
        /// Açıkken yukarıdaki sıfırlama düzeneğinin tamamı (kenar payı,
        /// devir teslim, doğrusal olmayan ölçekleme, gecikmeli iniş)
        /// devre dışı kalır: parmak bir kez iner ve nişan bırakılana kadar
        /// sınırsız düzlemde gezer. Kaldırılacak parmak olmadığı için
        /// "algılamıyor" ve "aim kayıyor" belirtileri kökten kalkar.
        ///
        /// Dayanağı Waydroid'e özgü: dokunuş borusunda kırpma yapan
        /// katman yok ve `InputDispatcher` MOVE'da pencere aramıyor.
        /// Bu yüzden yalnızca kırpmayan arka uçlarda geçerlidir —
        /// `Engine::set_offscreen_ok` ile açılır. Kapalıysa motor sessizce
        /// sınırlı kipe düşer; uinput yolunda ekran dışı koordinat
        /// libinput'ta sıkışır ve parmak kenarda takılı kalırdı.
        #[serde(default = "default_unbounded")]
        unbounded: bool,
        /// Emniyet kutusunun yarı genişliği (ekran katı).
        ///
        /// Sınırsız kipin tek sayısal sınırı. Amacı his değil taşma
        /// koruması: `input_event.value` i32 ve konum f32. 32 ekran ≈
        /// 82000 piksel; f32 hassasiyeti orada hâlâ 0.01 pikselin
        /// altında. Bu kadar NET (gidiş-dönüş farkı) sürüklenmek pratikte
        /// olmaz; olursa fare ilk durduğunda sessizce ortalanır.
        #[serde(default = "default_safety_span")]
        safety_span: f32,
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
        #[serde(default)]
        easing: Easing,
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

fn default_recenter_margin() -> f32 { 0.12 }
/// Varsayılan KAPALI.
///
/// Denendi ve Special Forces Group 2'de çalışmadı: oyun iki parmağı ayrı
/// ayrı takip edip ortalıyor, ekranda iki dokunuş izi beliriyor. Oyuna
/// göre değişebilir, o yüzden seçenek duruyor ama varsayılan değil.
fn default_true_handoff() -> bool { false }
fn default_true_nonlinear() -> bool { true }
fn default_reset_delay_ms() -> u32 { 12 }
/// Varsayılan AÇIK.
///
/// Sınırlı kip bu belirtileri üretiyordu ve hepsi kaçınılmazdı; korunacak
/// bir davranış değil. Arka uç desteklemiyorsa motor zaten sınırlı kipe
/// düşüyor, yani açık varsayılan hiçbir yolda kırılmıyor.
fn default_unbounded() -> bool { true }
fn default_safety_span() -> f32 { 32.0 }

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
    /// NOT: bağlantı SAYISINA sınır konmaz. `MAX_POINTERS` eşzamanlı
    /// parmak sınırıdır; bir profilde 20 düğme olabilir ve aynı anda
    /// yalnızca birkaçı basılır. İkisini karıştırmak geçerli profilleri
    /// reddeder — gerçekte oldu: 11 düğmeli bir FPS profili reddedildi.
    /// Havuz tükenirse motor çalışma zamanında uyarır.
    ///
    /// En önemlisi: **aynı tetikleyici birden fazla bağlantıda kullanılamaz.**
    /// Kullanılırsa hangi bağlantının çalışacağı belirsizleşir ve kullanıcı
    /// bunu "bazen çalışıyor" diye yaşar — teşhisi en zor hata türü.
    pub fn validate(&self) -> Result<(), ProfileError> {
        if self.bindings.is_empty() {
            return Err(ProfileError::Invalid("profilde hiç bağlantı yok".into()));
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

    /// Çok düğmeli profil KABUL EDİLMELİ: eşzamanlı parmak sınırı
    /// toplam bağlantı sayısıyla aynı şey değil.
    #[test]
    fn accepts_profile_with_more_bindings_than_pointers() {
        let mut t = String::from("name = \"çok\"\npackage = \"p\"\n");
        for i in 0..15u16 {
            t.push_str(&format!(
                "[bindings.b{i}]\ntype = \"tap\"\ntrigger = {{ Key = {} }}\n\
                 at = {{ x = 0.5, y = 0.5 }}\n", 100 + i));
        }
        let p = Profile::from_toml(&t).expect("15 bağlantı reddedilmemeli");
        assert_eq!(p.bindings.len(), 15);
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
    fn easing_preserves_endpoints() {
        for e in [Easing::Linear, Easing::EaseOut, Easing::EaseOutStrong] {
            assert!((e.apply(0.0) - 0.0).abs() < 1e-6, "{e:?} f(0) != 0");
            assert!((e.apply(1.0) - 1.0).abs() < 1e-6, "{e:?} f(1) != 1");
        }
    }

    /// Öne yüklü eğri, doğrusaldan DAHA ERKEN mesafe katetmeli —
    /// oyunun kaydırma eşiğini daha çabuk aşması bunun tek amacı.
    #[test]
    fn ease_out_is_front_loaded() {
        for t in [0.1f32, 0.25, 0.5, 0.75] {
            let lin = Easing::Linear.apply(t);
            let eo = Easing::EaseOut.apply(t);
            let eos = Easing::EaseOutStrong.apply(t);
            assert!(eo > lin, "t={t}: ease_out {eo} > linear {lin} olmalı");
            assert!(eos > eo, "t={t}: strong {eos} > ease_out {eo} olmalı");
        }
    }

    #[test]
    fn ease_out_half_time_covers_three_quarters() {
        assert!((Easing::EaseOut.apply(0.5) - 0.75).abs() < 1e-5);
        assert!((Easing::EaseOutStrong.apply(0.5) - 0.875).abs() < 1e-5);
    }

    #[test]
    fn easing_clamps_out_of_range_progress() {
        assert_eq!(Easing::EaseOut.apply(-1.0), 0.0);
        assert_eq!(Easing::EaseOut.apply(2.0), 1.0);
    }

    /// Profilde belirtilmezse doğrusal olmalı — davranış sessizce değişmesin.
    #[test]
    fn easing_defaults_to_linear_when_absent() {
        let p = Profile::from_toml(SAMPLE).unwrap();
        match p.bindings.get("sol").unwrap() {
            Binding::Swipe { easing, .. } => assert_eq!(*easing, Easing::Linear),
            other => panic!("swipe bekleniyordu: {other:?}"),
        }
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
