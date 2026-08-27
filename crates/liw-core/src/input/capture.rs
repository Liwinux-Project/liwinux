//! evdev yakalama: host klavye/faresini okur, isteğe bağlı olarak kilitler.
//!
//! # Güvenlik kısıtı
//!
//! `EVIOCGRAB` (grab) cihazı **münhasır** hale getirir: olaylar artık masaüstüne
//! gitmez. Keymapper için şart — yoksa oyunda WASD'ye basarken arkadaki
//! tarayıcıda da bir şeyler olur. Ama aynı nedenle tehlikelidir: grab
//! sırasında takılan bir süreç kullanıcının klavyesini rehin alır.
//!
//! Bu yüzden:
//! * Grab **`Drop`da** bırakılır; süreç ölürse çekirdek fd kapanışında zaten bırakır.
//! * Bir **kaçış tuşu** her zaman tanımlıdır ve grab'ı anında çözer.
//! * Grab varsayılan DEĞİLDİR; çağıran açıkça istemek zorundadır.

use evdev::{Device, EventSummary, KeyCode, RelativeAxisCode};
use std::path::{Path, PathBuf};

use super::engine::{InputEvent, TriggerKind};

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("cihaz açılamadı ({path}): {source}")]
    Open { path: PathBuf, #[source] source: std::io::Error },
    #[error("cihaz kilitlenemedi ({path}): {source} — başka bir program kilitlemiş olabilir")]
    Grab { path: PathBuf, #[source] source: std::io::Error },
    #[error("olay akışı kurulamadı: {0}")]
    Stream(#[source] std::io::Error),
    #[error("uygun girdi cihazı bulunamadı (kullanıcı 'input' grubunda mı?)")]
    NoDevices,
    /// Açılan cihaz beklenen türde değil.
    ///
    /// Ayrı bir varyant: "açılamadı" ile "yanlış cihaz" tamamen farklı
    /// sorunlar ve farklı çözümleri var. Aynı hataya gömmek, kullanıcıyı
    /// izin sorunu sanıp yanlış yere bakmaya iterdi.
    #[error("{0}")]
    WrongKind(String),
}

/// Cihazın bizim için ne işe yaradığı.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Keyboard,
    Pointer,
    /// Hem tuş hem bağıl eksen (oyuncu fareleri, bazı klavyeler).
    Combo,
}

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub path: PathBuf,
    pub name: String,
    pub kind: DeviceKind,
    /// uinput ile oluşturulmuş sanal cihaz mı.
    pub virtual_device: bool,
    /// Gerçek yazma klavyesi olma puanı (yüksek = daha olası).
    pub typing_score: u32,
}

/// Gerçek yazma klavyesini ayırt etmek için aranan tuşlar.
///
/// Sadece `KEY_A..KEY_Z`ye bakmak YETMEZ: çoklu arayüzlü klavyeler (Razer,
/// Logitech) medya/makro arayüzlerinde de bu aralığı bildirir ama gerçek
/// tuş olaylarını üretmez. Bu makinede `event18` ve `event23` aynı klavyenin
/// iki arayüzü; yazma olayları yalnızca ikincisinden geliyor.
const TYPING_KEYS: &[KeyCode] = &[
    KeyCode::KEY_A, KeyCode::KEY_Z, KeyCode::KEY_Q, KeyCode::KEY_M,
    KeyCode::KEY_0, KeyCode::KEY_9,
    KeyCode::KEY_SPACE, KeyCode::KEY_ENTER, KeyCode::KEY_TAB,
    KeyCode::KEY_LEFTSHIFT, KeyCode::KEY_RIGHTSHIFT,
    KeyCode::KEY_LEFTCTRL, KeyCode::KEY_LEFTALT,
    KeyCode::KEY_CAPSLOCK, KeyCode::KEY_ESC, KeyCode::KEY_BACKSPACE,
    KeyCode::KEY_F1, KeyCode::KEY_F12,
    KeyCode::KEY_UP, KeyCode::KEY_DOWN, KeyCode::KEY_LEFT, KeyCode::KEY_RIGHT,
];

/// Cihazın gerçek yazma klavyesi olma puanı.
pub fn typing_score(dev: &Device) -> u32 {
    let Some(keys) = dev.supported_keys() else { return 0 };
    TYPING_KEYS.iter().filter(|k| keys.contains(**k)).count() as u32
}

/// Bir cihazın türünü yeteneklerine bakarak belirler.
///
/// İsme bakmak güvenilmez ("Razer Keyboard" bir fare olabilir); yeteneğe
/// bakmak tek doğru yöntem.
fn classify(dev: &Device) -> Option<DeviceKind> {
    let keys = dev.supported_keys();
    let rels = dev.supported_relative_axes();

    // Gerçek klavye: harf tuşları var.
    let has_letters = keys.is_some_and(|k| k.contains(KeyCode::KEY_A) && k.contains(KeyCode::KEY_Z));
    // Gerçek işaretçi: X ve Y bağıl ekseni + sol tuş.
    let has_motion = rels.is_some_and(|r| {
        r.contains(RelativeAxisCode::REL_X) && r.contains(RelativeAxisCode::REL_Y)
    });
    let has_btn = keys.is_some_and(|k| k.contains(KeyCode::BTN_LEFT));

    match (has_letters, has_motion && has_btn) {
        (true, true) => Some(DeviceKind::Combo),
        (true, false) => Some(DeviceKind::Keyboard),
        (false, true) => Some(DeviceKind::Pointer),
        (false, false) => None,
    }
}

/// `/dev/input/eventN` için kararlı bir `by-id` yolu bulur.
///
/// Neden şart: `eventN` numaraları YENİDEN BAŞLATMALAR ARASI SABİT DEĞİL.
/// Gerçekte yaşandı — config'deki `event23` klavyeyken, yeniden başlatma
/// sonrası "HDA NVidia HDMI/DP,pcm=9" ses cihazı oldu. Keymapper sessizce
/// hiçbir şey yapmadı. Dağıtacağımız bir araçta bu her kullanıcıda her
/// yeniden başlatmada kırılırdı.
///
/// Birden fazla bağlantı aynı düğüme çıkabilir; `-event-kbd` /
/// `-event-mouse` sonekli olan tercih edilir çünkü udev bunları cihazın
/// birincil işlevi için üretir.
pub fn stable_path(event_path: &std::path::Path) -> Option<std::path::PathBuf> {
    let dir = std::path::Path::new("/dev/input/by-id");
    let target = event_path.canonicalize().ok()?;
    let mut best: Option<std::path::PathBuf> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let link = entry.path();
        if link.canonicalize().ok().as_deref() != Some(target.as_path()) { continue; }
        if best.is_none() || prefer_link(&link) { best = Some(link); }
    }
    best
}

/// udev'in birincil işlev için ürettiği sonekler önceliklidir.
fn prefer_link(p: &std::path::Path) -> bool {
    let n = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
    n.ends_with("-event-kbd") || n.ends_with("-event-mouse")
}

/// Açılan cihazın beklenen türde olduğunu doğrular.
///
/// Doğrulamamak, yanlış cihaz açıldığında SESSİZ başarısızlık demekti:
/// ses jakı cihazından tuş beklemek sonsuza kadar bekler ve kullanıcı
/// yalnızca "keymapper çalışmıyor" görür.
pub fn verify_kind(dev: &Device, want_pointer: bool) -> Result<(), CaptureError> {
    let name = dev.name().unwrap_or("(isimsiz)").to_string();
    let kind = classify(dev);
    let ok = match (want_pointer, kind) {
        (true, Some(DeviceKind::Pointer | DeviceKind::Combo)) => true,
        (false, Some(DeviceKind::Keyboard | DeviceKind::Combo)) => true,
        _ => false,
    };
    if ok { return Ok(()); }
    let istenen = if want_pointer { "fare" } else { "klavye" };
    Err(CaptureError::WrongKind(format!(
        "cihaz {istenen} değil: \"{name}\" (tür: {kind:?}). \
         eventN numaraları yeniden başlatmalar arası değişir — \
         `liw keymap detect --save` ile yeniden kalibre et")))
}

/// Sistemdeki kullanılabilir girdi cihazlarını listeler.
pub fn discover() -> Vec<DeviceInfo> {
    let mut out = Vec::new();
    for (path, dev) in evdev::enumerate() {
        let name = dev.name().unwrap_or("(isimsiz)").to_string();
        // Kendi sanal dokunmatik ekranımızı asla yakalamayalım: geri besleme olur.
        if name.starts_with("liwinux") { continue; }
        let Some(kind) = classify(&dev) else { continue };
        // uinput cihazlarının fiziksel yolu yoktur. Ad'a bakmak güvenilmez
        // (herkes istediği adı verebilir); fiziksel yol yokluğu ise
        // doğrudan uinput'tan gelir.
        let virtual_device = dev.physical_path().is_none_or(str::is_empty);
        out.push(DeviceInfo {
            path, name, kind, virtual_device,
            typing_score: typing_score(&dev),
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Yazmak için en olası klavyeyi seçer.
///
/// Sıralama: sanal olmayan > yüksek yazma puanı > düşük event numarası.
/// Berabere kalırsa seçim keyfidir; o durumda kullanıcı `liw keymap watch`
/// ile doğru cihazı kendisi belirlemelidir — tahmin etmektense sormak iyidir.
pub fn best_keyboard(devs: &[DeviceInfo]) -> Option<&DeviceInfo> {
    devs.iter()
        .filter(|d| !d.virtual_device)
        .filter(|d| matches!(d.kind, DeviceKind::Keyboard | DeviceKind::Combo))
        .max_by_key(|d| d.typing_score)
}

/// Kilitlenmiş tek bir cihaz. `Drop`da kilit çözülür.
pub struct GrabbedDevice {
    dev: Option<Device>,
    path: PathBuf,
    grabbed: bool,
}

impl GrabbedDevice {
    pub fn open(path: &Path, grab: bool) -> Result<Self, CaptureError> {
        let mut dev = Device::open(path)
            .map_err(|source| CaptureError::Open { path: path.into(), source })?;
        let mut grabbed = false;
        if grab {
            dev.grab().map_err(|source| CaptureError::Grab { path: path.into(), source })?;
            grabbed = true;
        }
        Ok(Self { dev: Some(dev), path: path.into(), grabbed })
    }

    pub fn is_grabbed(&self) -> bool { self.grabbed }
    pub fn path(&self) -> &Path { &self.path }

    /// Kilidi erkenden çözer (kaçış tuşu için).
    pub fn release(&mut self) {
        if self.grabbed {
            if let Some(d) = self.dev.as_mut() {
                let _ = d.ungrab();
            }
            self.grabbed = false;
            tracing::info!(path = ?self.path, "cihaz kilidi çözüldü");
        }
    }

    /// Asenkron olay akışına dönüştürür.
    pub fn into_stream(mut self) -> Result<evdev::EventStream, CaptureError> {
        let dev = self.dev.take().expect("cihaz iki kez alınamaz");
        // Drop artık ungrab yapmayacak; akış fd sahipliğini devralıyor ve
        // fd kapanınca çekirdek kilidi zaten bırakıyor.
        self.grabbed = false;
        dev.into_event_stream().map_err(CaptureError::Stream)
    }
}

impl Drop for GrabbedDevice {
    fn drop(&mut self) {
        self.release();
    }
}

/// evdev olayını motorun anladığı olaya çevirir.
///
/// `None` dönerse olay bizi ilgilendirmiyor demektir (senkronizasyon,
/// LED, tuş tekrarı gibi).
pub fn translate(ev: &evdev::InputEvent) -> Option<InputEvent> {
    match ev.destructure() {
        EventSummary::Key(_, code, value) => {
            let t = match code {
                KeyCode::BTN_LEFT => TriggerKind::MouseLeft,
                KeyCode::BTN_RIGHT => TriggerKind::MouseRight,
                KeyCode::BTN_MIDDLE => TriggerKind::MouseMiddle,
                other => TriggerKind::Key(other.code()),
            };
            match value {
                1 => Some(InputEvent::Press(t)),
                0 => Some(InputEvent::Release(t)),
                // value == 2 çekirdek tuş tekrarı; motor zaten yutuyor ama
                // burada da elemek gereksiz iş yapmayı önler.
                _ => None,
            }
        }
        EventSummary::RelativeAxis(_, RelativeAxisCode::REL_X, v) =>
            Some(InputEvent::MouseMove { dx: v as f32, dy: 0.0 }),
        EventSummary::RelativeAxis(_, RelativeAxisCode::REL_Y, v) =>
            Some(InputEvent::MouseMove { dx: 0.0, dy: v as f32 }),
        EventSummary::RelativeAxis(_, RelativeAxisCode::REL_WHEEL, v) => {
            let t = if v > 0 { TriggerKind::WheelUp } else { TriggerKind::WheelDown };
            // Tekerlek anlıktır: bas-bırak çifti yerine tek bas üretiyoruz;
            // çağıran bunu hemen bırakma ile eşlemeli.
            Some(InputEvent::Press(t))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    /// udev'in birincil işlev bağlantıları tercih edilmeli.
    ///
    /// Aynı düğüme birden fazla bağlantı çıkabiliyor; `-event-if03` gibi
    /// arayüz bağlantıları cihazın ikincil işlevlerini gösterir.
    #[test]
    fn primary_udev_links_are_preferred() {
        use std::path::Path;
        assert!(super::prefer_link(Path::new(
            "/dev/input/by-id/usb-Razer_BlackWidow_V3-event-kbd")));
        assert!(super::prefer_link(Path::new(
            "/dev/input/by-id/usb-Razer_Basilisk_V3-event-mouse")));
        assert!(!super::prefer_link(Path::new(
            "/dev/input/by-id/usb-Razer_BlackWidow_V3-event-if03")));
        assert!(!super::prefer_link(Path::new(
            "/dev/input/by-id/usb-Compx_Receiver-if01-event")));
    }

    /// Yanlış cihaz açıldığında hata "açılamadı" ile KARIŞMAMALI:
    /// ikisi farklı sorunlar ve kullanıcı yanlış yere bakar.
    #[test]
    fn wrong_kind_is_a_distinct_error() {
        let e = super::CaptureError::WrongKind("cihaz klavye değil".into());
        let msg = e.to_string();
        assert!(msg.contains("klavye değil"), "{msg}");
        assert!(!msg.contains("açılamadı"), "izin sorunu sanılmamalı: {msg}");
    }

    use super::*;
    use evdev::{EventType, InputEvent as RawEvent};

    fn key_ev(code: u16, value: i32) -> RawEvent {
        RawEvent::new(EventType::KEY.0, code, value)
    }
    fn rel_ev(code: u16, value: i32) -> RawEvent {
        RawEvent::new(EventType::RELATIVE.0, code, value)
    }

    #[test]
    fn key_press_and_release_translate() {
        assert_eq!(translate(&key_ev(KeyCode::KEY_W.code(), 1)),
                   Some(InputEvent::Press(TriggerKind::Key(KeyCode::KEY_W.code()))));
        assert_eq!(translate(&key_ev(KeyCode::KEY_W.code(), 0)),
                   Some(InputEvent::Release(TriggerKind::Key(KeyCode::KEY_W.code()))));
    }

    /// Çekirdek tuş tekrarı (value=2) olaya çevrilmemeli.
    #[test]
    fn kernel_autorepeat_is_dropped() {
        assert_eq!(translate(&key_ev(KeyCode::KEY_W.code(), 2)), None);
    }

    #[test]
    fn mouse_buttons_map_to_their_own_kinds() {
        assert_eq!(translate(&key_ev(KeyCode::BTN_LEFT.code(), 1)),
                   Some(InputEvent::Press(TriggerKind::MouseLeft)));
        assert_eq!(translate(&key_ev(KeyCode::BTN_RIGHT.code(), 1)),
                   Some(InputEvent::Press(TriggerKind::MouseRight)));
    }

    #[test]
    fn relative_axes_become_mouse_motion() {
        assert_eq!(translate(&rel_ev(RelativeAxisCode::REL_X.0, 12)),
                   Some(InputEvent::MouseMove { dx: 12.0, dy: 0.0 }));
        assert_eq!(translate(&rel_ev(RelativeAxisCode::REL_Y.0, -4)),
                   Some(InputEvent::MouseMove { dx: 0.0, dy: -4.0 }));
    }

    #[test]
    fn wheel_direction_is_signed() {
        assert_eq!(translate(&rel_ev(RelativeAxisCode::REL_WHEEL.0, 1)),
                   Some(InputEvent::Press(TriggerKind::WheelUp)));
        assert_eq!(translate(&rel_ev(RelativeAxisCode::REL_WHEEL.0, -1)),
                   Some(InputEvent::Press(TriggerKind::WheelDown)));
    }

    #[test]
    fn irrelevant_events_are_ignored() {
        assert_eq!(translate(&RawEvent::new(EventType::LED.0, 0, 1)), None);
        assert_eq!(translate(&RawEvent::new(EventType::SYNCHRONIZATION.0, 0, 0)), None);
    }
}
