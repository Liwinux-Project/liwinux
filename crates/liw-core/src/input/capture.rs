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

/// Sistemdeki kullanılabilir girdi cihazlarını listeler.
pub fn discover() -> Vec<DeviceInfo> {
    let mut out = Vec::new();
    for (path, dev) in evdev::enumerate() {
        // Kendi sanal cihazlarımızı asla yakalamayalım: geri besleme döngüsü olur.
        let name = dev.name().unwrap_or("(isimsiz)").to_string();
        if name.starts_with("liwinux") { continue; }
        if let Some(kind) = classify(&dev) {
            out.push(DeviceInfo { path, name, kind });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
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
