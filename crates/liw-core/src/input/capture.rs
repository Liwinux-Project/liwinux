//! evdev capture: reads the host keyboard/mouse, optionally grabbing it.
//!
//! # Safety constraint
//!
//! `EVIOCGRAB` makes the device **exclusive**: events no longer reach the
//! desktop. Mandatory for the keymapper — otherwise pressing WASD in the game
//! also does things in the browser behind it. But dangerous for the same
//! reason: a process that hangs while grabbing holds the user's keyboard
//! hostage.
//!
//! Therefore:
//! * The grab is released **in `Drop`**; if the process dies the kernel releases
//!   it on fd close anyway.
//! * An **escape key** is always defined and releases the grab instantly.
//! * Grabbing is NOT the default; the caller must ask for it explicitly.

use evdev::{Device, EventSummary, KeyCode, RelativeAxisCode};
use std::path::{Path, PathBuf};

use super::engine::{InputEvent, TriggerKind};

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("could not open device ({path}): {source}")]
    Open { path: PathBuf, #[source] source: std::io::Error },
    #[error("could not grab device ({path}): {source} — another program may hold it")]
    Grab { path: PathBuf, #[source] source: std::io::Error },
    #[error("could not set up event stream: {0}")]
    Stream(#[source] std::io::Error),
    #[error("no suitable input device found (is the user in the 'input' group?)")]
    NoDevices,
    /// The opened device is not of the expected kind.
    ///
    /// A separate variant: "could not open" and "wrong device" are entirely
    /// different problems with different fixes. Folding them into one error
    /// would push the user to suspect a permission problem and look in the
    #[error("{0}")]
    WrongKind(String),
}

/// What a device is good for, from our point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Keyboard,
    Pointer,
    /// Both keys and relative axes (gaming mice, some keyboards).
    Combo,
}

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub path: PathBuf,
    pub name: String,
    pub kind: DeviceKind,
    /// Whether this is a virtual device created via uinput.
    pub virtual_device: bool,
    /// Score for being a real typing keyboard (higher = more likely).
    pub typing_score: u32,
}

/// Keys looked for when identifying a real typing keyboard.
///
/// Looking at `KEY_A..KEY_Z` alone is NOT ENOUGH: multi-interface keyboards
/// (Razer, Logitech) also advertise that range on their media/macro interfaces
/// without producing real key events. On this machine `event18` and `event23`
/// are two interfaces of the same keyboard; typing events come only from one.
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

/// How likely the device is to be a real typing keyboard.
pub fn typing_score(dev: &Device) -> u32 {
    let Some(keys) = dev.supported_keys() else { return 0 };
    TYPING_KEYS.iter().filter(|k| keys.contains(**k)).count() as u32
}

/// Determines a device's kind from its capabilities.
///
/// Going by name is unreliable ("Razer Keyboard" may be a mouse); capabilities
/// are the only correct method.
fn classify(dev: &Device) -> Option<DeviceKind> {
    let keys = dev.supported_keys();
    let rels = dev.supported_relative_axes();

    // A real keyboard: it has letter keys.
    let has_letters = keys.is_some_and(|k| k.contains(KeyCode::KEY_A) && k.contains(KeyCode::KEY_Z));
    // A real pointer: relative X and Y axes plus a left button.
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

/// Finds a stable `by-id` path for a `/dev/input/eventN` node.
///
/// Why it matters: `eventN` numbers are NOT STABLE ACROSS REBOOTS. This
/// actually happened — `event23` in the config was the keyboard, and after a
/// reboot it became the "HDA NVidia HDMI/DP,pcm=9" audio device. The keymapper
/// silently did nothing. In a tool we distribute this would break for every
/// user on every reboot.
///
/// Several links can point at the same node; the one suffixed `-event-kbd` /
/// `-event-mouse` is preferred, because udev creates those for the device's
/// primary function.
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

/// Suffixes udev creates for the primary function take precedence.
fn prefer_link(p: &std::path::Path) -> bool {
    let n = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
    n.ends_with("-event-kbd") || n.ends_with("-event-mouse")
}

/// Verifies the opened device is of the expected kind.
///
/// Not verifying meant SILENT failure when the wrong device was opened:
/// waiting for keys from an audio-jack device waits forever and all the user
/// sees is "the keymapper does not work".
pub fn verify_kind(dev: &Device, want_pointer: bool) -> Result<(), CaptureError> {
    let name = dev.name().unwrap_or("(isimsiz)").to_string();
    let kind = classify(dev);
    let ok = match (want_pointer, kind) {
        (true, Some(DeviceKind::Pointer | DeviceKind::Combo)) => true,
        (false, Some(DeviceKind::Keyboard | DeviceKind::Combo)) => true,
        _ => false,
    };
    if ok { return Ok(()); }
    let wanted = if want_pointer { "mouse" } else { "keyboard" };
    Err(CaptureError::WrongKind(format!(
        "device is not a {wanted}: \"{name}\" (kind: {kind:?}). \
         eventN numbers change across reboots — \
         Recalibrate with `liw keymap detect --save`")))
}

/// Lists the usable input devices on the system.
pub fn discover() -> Vec<DeviceInfo> {
    let mut out = Vec::new();
    for (path, dev) in evdev::enumerate() {
        let name = dev.name().unwrap_or("(isimsiz)").to_string();
        // Never capture our own virtual touchscreen: that would be feedback.
        if name.starts_with("liwinux") { continue; }
        let Some(kind) = classify(&dev) else { continue };
        // uinput devices have no physical path. Going by name is unreliable
        // (anyone can pick a name); the absence of a physical path comes
        // straight from uinput.
        let virtual_device = dev.physical_path().is_none_or(str::is_empty);
        out.push(DeviceInfo {
            path, name, kind, virtual_device,
            typing_score: typing_score(&dev),
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Picks the most likely keyboard for typing.
///
/// Ordering: non-virtual > higher typing score > lower event number.
/// On a tie the choice is arbitrary; the user should then identify the right
/// device with `liw keymap watch` — asking beats guessing.
pub fn best_keyboard(devs: &[DeviceInfo]) -> Option<&DeviceInfo> {
    devs.iter()
        .filter(|d| !d.virtual_device)
        .filter(|d| matches!(d.kind, DeviceKind::Keyboard | DeviceKind::Combo))
        .max_by_key(|d| d.typing_score)
}

/// A single grabbed device. The grab is released on `Drop`.
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

    /// Releases the grab early (for the escape key).
    pub fn release(&mut self) {
        if self.grabbed {
            if let Some(d) = self.dev.as_mut() {
                let _ = d.ungrab();
            }
            self.grabbed = false;
            tracing::info!(path = ?self.path, "device grab released");
        }
    }

    /// Converts into an asynchronous event stream.
    pub fn into_stream(mut self) -> Result<evdev::EventStream, CaptureError> {
        let dev = self.dev.take().expect("device cannot be taken twice");
        // Drop will no longer ungrab; the stream takes ownership of the fd and
        // the kernel releases the grab when the fd closes.
        self.grabbed = false;
        dev.into_event_stream().map_err(CaptureError::Stream)
    }
}

impl Drop for GrabbedDevice {
    fn drop(&mut self) {
        self.release();
    }
}

/// Converts an evdev event into an event the engine understands.
///
/// `None` means the event does not concern us (synchronisation, LEDs, key
/// repeat and so on).
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
                // value == 2 is kernel key repeat; the engine swallows it
                // anyway, but filtering here avoids useless work.
                _ => None,
            }
        }
        EventSummary::RelativeAxis(_, RelativeAxisCode::REL_X, v) =>
            Some(InputEvent::MouseMove { dx: v as f32, dy: 0.0 }),
        EventSummary::RelativeAxis(_, RelativeAxisCode::REL_Y, v) =>
            Some(InputEvent::MouseMove { dx: 0.0, dy: v as f32 }),
        EventSummary::RelativeAxis(_, RelativeAxisCode::REL_WHEEL, v) => {
            let t = if v > 0 { TriggerKind::WheelUp } else { TriggerKind::WheelDown };
            // The wheel is instantaneous: instead of a press/release pair we
            // emit a single press; the caller must pair it with an immediate
            // release.
            Some(InputEvent::Press(t))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    /// udev's primary-function links must be preferred.
    ///
    /// Several links can point at the same node; interface links such as
    /// `-event-if03` describe the device's secondary functions.
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

    /// A wrong-device error must NOT be CONFUSED with "could not open":
    /// they are different problems and the user would look in the wrong place.
    #[test]
    fn wrong_kind_is_a_distinct_error() {
        let e = super::CaptureError::WrongKind("device is not a keyboard".into());
        let msg = e.to_string();
        assert!(msg.contains("not a keyboard"), "{msg}");
        assert!(!msg.contains("could not open"), "must not look like a permission problem: {msg}");
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

    /// Kernel key repeat (value=2) must not be turned into an event.
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
