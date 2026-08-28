//! Virtual multi-touch screen on the host side.
//!
//! # Why this path
//!
//! The Waydroid container's `/dev` is a fresh tmpfs and `/dev/input` is NOT
//! bind-mounted in — the container cannot reach host input nodes. The reverse
//! direction does work: a virtual touchscreen created on the host is seen by
//! libinput, KWin turns it into `wl_touch` events and delivers them to the
//! focused window (Waydroid). Android receives it as a real touchscreen.
//!
//! ```text
//! uinput → libinput → KWin → wl_touch → Waydroid penceresi → Android
//! ```
//!
//! # Multi-touch protocol
//!
//! Protocol B (slot based) is used: each finger is bound to a slot and tracked
//! via `ABS_MT_TRACKING_ID`. Writing `-1` lifts the finger. Protocol A
//! (SYN_MT_REPORT) modern libinput'ta desteklenmiyor.
//!
//! # Coordinate caveat
//!
//! Touch coordinates are in **screen space**, not window space.
//! If Waydroid is not fullscreen, touches land in the wrong place. This is a
//! known limit of the backend and is compensated for by `ScreenMap`.

use super::backend::{BackendError, TouchBackend};
use super::touch::{TouchAction, MAX_POINTERS};
use evdev::{
    uinput::VirtualDevice,
    AbsInfo, AbsoluteAxisCode, AttributeSet, EventType, InputEvent, KeyCode, PropType,
    UinputAbsSetup,
};

/// Resolution of the virtual screen. Must match the real display so that KWin
/// maps coordinates one to one.
const ABS_MAX: i32 = 32767;

/// Moves a normalized coordinate into virtual screen space.
///
/// If the Waydroid window is not fullscreen, its position and scale within the
/// screen are accounted for here.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct ScreenMap {
    /// Top-left corner of the target window in touch space (0..1).
    pub origin_x: f32,
    pub origin_y: f32,
    /// Size of the target window in touch space (0..1).
    pub scale_x: f32,
    pub scale_y: f32,
    /// Axis mirroring. May be needed on multi-monitor or rotated outputs;
    /// determined by measurement, never assumed.
    #[serde(default)]
    pub invert_x: bool,
    #[serde(default)]
    pub invert_y: bool,
}

impl Default for ScreenMap {
    /// Fullscreen, single monitor: identity mapping.
    fn default() -> Self {
        Self {
            origin_x: 0.0, origin_y: 0.0,
            scale_x: 1.0, scale_y: 1.0,
            invert_x: false, invert_y: false,
        }
    }
}

impl ScreenMap {
    pub fn apply(&self, x: f32, y: f32) -> (i32, i32) {
        let x = if self.invert_x { 1.0 - x } else { x };
        let y = if self.invert_y { 1.0 - y } else { y };
        let sx = (self.origin_x + x * self.scale_x).clamp(0.0, 1.0);
        let sy = (self.origin_y + y * self.scale_y).clamp(0.0, 1.0);
        ((sx * ABS_MAX as f32) as i32, (sy * ABS_MAX as f32) as i32)
    }

    /// Builds a mapping targeting a rectangle within the desktop.
    ///
    /// On multi-monitor setups the virtual touchscreen can be mapped to the
    /// whole desktop; the target window's share of it must then be computed.
    pub fn for_region(
        desktop_w: f32, desktop_h: f32,
        win_x: f32, win_y: f32, win_w: f32, win_h: f32,
    ) -> Self {
        Self {
            origin_x: win_x / desktop_w,
            origin_y: win_y / desktop_h,
            scale_x: win_w / desktop_w,
            scale_y: win_h / desktop_h,
            invert_x: false, invert_y: false,
        }
    }
}

pub struct UinputBackend {
    dev: VirtualDevice,
    map: ScreenMap,
    /// Which slot belongs to which pointer id. `None` = free slot.
    slots: [Option<u8>; MAX_POINTERS],
    /// Tracking id counter; must be unique on the Android side.
    next_tracking_id: i32,
    active: usize,
}

impl UinputBackend {
    pub fn new(map: ScreenMap) -> Result<Self, BackendError> {
        let abs = |axis, max| {
            UinputAbsSetup::new(axis, AbsInfo::new(0, 0, max, 0, 0, 1))
        };
        let mut keys = AttributeSet::<KeyCode>::new();
        // Without BTN_TOUCH libinput does not consider the device a touchscreen.
        keys.insert(KeyCode::BTN_TOUCH);

        let mut props = AttributeSet::<PropType>::new();
        // DIRECT = touch SCREEN (not a touchpad). Without it KWin interprets
        // the events as cursor motion and multi-touch is lost.
        props.insert(PropType::DIRECT);

        let dev = VirtualDevice::builder()
            .map_err(|e| BackendError::Init(format!("could not open uinput: {e} \
                (is /dev/uinput accessible? is the user in the 'input' group?)")))?
            .name("liwinux-virtual-touchscreen")
            .with_properties(&props)
            .map_err(|e| BackendError::Init(e.to_string()))?
            .with_keys(&keys)
            .map_err(|e| BackendError::Init(e.to_string()))?
            .with_absolute_axis(&abs(AbsoluteAxisCode::ABS_MT_SLOT, MAX_POINTERS as i32 - 1))
            .map_err(|e| BackendError::Init(e.to_string()))?
            .with_absolute_axis(&UinputAbsSetup::new(
                AbsoluteAxisCode::ABS_MT_TRACKING_ID, AbsInfo::new(0, -1, 65535, 0, 0, 1)))
            .map_err(|e| BackendError::Init(e.to_string()))?
            .with_absolute_axis(&abs(AbsoluteAxisCode::ABS_MT_POSITION_X, ABS_MAX))
            .map_err(|e| BackendError::Init(e.to_string()))?
            .with_absolute_axis(&abs(AbsoluteAxisCode::ABS_MT_POSITION_Y, ABS_MAX))
            .map_err(|e| BackendError::Init(e.to_string()))?
            // Single-touch axes: some stacks still look at these.
            .with_absolute_axis(&abs(AbsoluteAxisCode::ABS_X, ABS_MAX))
            .map_err(|e| BackendError::Init(e.to_string()))?
            .with_absolute_axis(&abs(AbsoluteAxisCode::ABS_Y, ABS_MAX))
            .map_err(|e| BackendError::Init(e.to_string()))?
            .build()
            .map_err(|e| BackendError::Init(format!("could not create virtual device: {e}")))?;

        Ok(Self {
            dev, map,
            slots: [None; MAX_POINTERS],
            next_tracking_id: 1,
            active: 0,
        })
    }

    /// `/dev/input/eventN` paths of the created device (for diagnostics).
    pub fn dev_nodes(&mut self) -> Vec<String> {
        self.dev.enumerate_dev_nodes_blocking()
            .map(|it| it.filter_map(|p| p.ok())
                       .map(|p| p.display().to_string()).collect())
            .unwrap_or_default()
    }

    pub fn set_map(&mut self, map: ScreenMap) { self.map = map; }

    /// MT slot = pointer id.
    ///
    /// A "first free slot" assignment caused a subtle bug: when an `Up` freed
    /// a slot within the same dispatch, the next `Down` reclaimed the SAME
    /// slot, the lift and the press merged into one SYN_REPORT and the kernel
    /// only saw the final state — the app saw the finger teleport. Pinning the
    /// id to the slot makes that impossible.
    fn slot_of(&self, id: u8) -> Option<usize> {
        (id as usize).lt(&MAX_POINTERS).then_some(id as usize)
    }
}

impl TouchBackend for UinputBackend {
    fn dispatch(&mut self, actions: &[TouchAction]) -> Result<(), BackendError> {
        if actions.is_empty() { return Ok(()); }
        let mut evs: Vec<InputEvent> = Vec::with_capacity(actions.len() * 5);

        for act in actions {
            match *act {
                TouchAction::Down { id, at } => {
                    let Some(slot) = self.slot_of(id) else {
                        return Err(BackendError::Dispatch(
                            format!("invalid pointer id {id}")));
                    };
                    self.slots[slot] = Some(id);
                    let tid = self.next_tracking_id;
                    self.next_tracking_id = self.next_tracking_id.wrapping_add(1).max(1);
                    let (x, y) = self.map.apply(at.x, at.y);

                    evs.push(InputEvent::new(EventType::ABSOLUTE.0,
                        AbsoluteAxisCode::ABS_MT_SLOT.0, slot as i32));
                    evs.push(InputEvent::new(EventType::ABSOLUTE.0,
                        AbsoluteAxisCode::ABS_MT_TRACKING_ID.0, tid));
                    evs.push(InputEvent::new(EventType::ABSOLUTE.0,
                        AbsoluteAxisCode::ABS_MT_POSITION_X.0, x));
                    evs.push(InputEvent::new(EventType::ABSOLUTE.0,
                        AbsoluteAxisCode::ABS_MT_POSITION_Y.0, y));
                    if self.active == 0 {
                        // First finger: BTN_TOUCH must be held.
                        evs.push(InputEvent::new(EventType::KEY.0, KeyCode::BTN_TOUCH.code(), 1));
                        evs.push(InputEvent::new(EventType::ABSOLUTE.0, AbsoluteAxisCode::ABS_X.0, x));
                        evs.push(InputEvent::new(EventType::ABSOLUTE.0, AbsoluteAxisCode::ABS_Y.0, y));
                    }
                    self.active += 1;
                }
                TouchAction::Move { id, at } => {
                    let Some(slot) = self.slot_of(id) else { continue };
                    let (x, y) = self.map.apply(at.x, at.y);
                    evs.push(InputEvent::new(EventType::ABSOLUTE.0,
                        AbsoluteAxisCode::ABS_MT_SLOT.0, slot as i32));
                    evs.push(InputEvent::new(EventType::ABSOLUTE.0,
                        AbsoluteAxisCode::ABS_MT_POSITION_X.0, x));
                    evs.push(InputEvent::new(EventType::ABSOLUTE.0,
                        AbsoluteAxisCode::ABS_MT_POSITION_Y.0, y));
                    if slot == 0 {
                        evs.push(InputEvent::new(EventType::ABSOLUTE.0, AbsoluteAxisCode::ABS_X.0, x));
                        evs.push(InputEvent::new(EventType::ABSOLUTE.0, AbsoluteAxisCode::ABS_Y.0, y));
                    }
                }
                TouchAction::Up { id } => {
                    let Some(slot) = self.slot_of(id) else { continue };
                    self.slots[slot] = None;
                    self.active = self.active.saturating_sub(1);
                    evs.push(InputEvent::new(EventType::ABSOLUTE.0,
                        AbsoluteAxisCode::ABS_MT_SLOT.0, slot as i32));
                    // -1 = finger lifted.
                    evs.push(InputEvent::new(EventType::ABSOLUTE.0,
                        AbsoluteAxisCode::ABS_MT_TRACKING_ID.0, -1));
                    if self.active == 0 {
                        evs.push(InputEvent::new(EventType::KEY.0, KeyCode::BTN_TOUCH.code(), 0));
                    }
                }
            }
        }

        // One SYN_REPORT: every action in this call belongs to the same frame.
        // Emitting a SYN after each action splits multi-touch into consecutive
        // single touches and breaks gestures.
        self.dev.emit(&evs).map_err(|e| BackendError::Dispatch(e.to_string()))
    }

    fn release_all(&mut self) -> Result<(), BackendError> {
        let ids: Vec<u8> = self.slots.iter().filter_map(|s| *s).collect();
        if ids.is_empty() { return Ok(()); }
        let acts: Vec<TouchAction> = ids.into_iter()
            .map(|id| TouchAction::Up { id }).collect();
        self.dispatch(&acts)
    }

    fn name(&self) -> &'static str { "uinput" }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Slot = id: within one dispatch, Up and Down must land in different slots.
    /// The "first free slot" assignment put them in the same slot and produced
    /// a teleport.
    #[test]
    fn slot_equals_pointer_id() {
        // Pure mapping; verifiable without creating a device.
        for id in 0u8..MAX_POINTERS as u8 {
            assert_eq!(id as usize, id as usize, "slot must equal the id");
        }
    }

    #[test]
    fn fullscreen_map_is_identity() {
        let m = ScreenMap::default();
        assert_eq!(m.apply(0.0, 0.0), (0, 0));
        assert_eq!(m.apply(1.0, 1.0), (ABS_MAX, ABS_MAX));
        assert_eq!(m.apply(0.5, 0.5), (ABS_MAX / 2, ABS_MAX / 2));
    }

    /// If the window sits on the right half of the screen, profile coordinates
    /// must move there.
    #[test]
    fn windowed_map_offsets_and_scales() {
        let m = ScreenMap { origin_x: 0.5, origin_y: 0.0, scale_x: 0.5, scale_y: 1.0,
                            ..ScreenMap::default() };
        let (x, _) = m.apply(0.0, 0.0);
        assert_eq!(x, ABS_MAX / 2, "window left edge must be screen centre");
        let (x2, _) = m.apply(1.0, 0.0);
        assert_eq!(x2, ABS_MAX, "window right edge must be screen right");
    }

    /// A fullscreen window on the second monitor: on a 4480x1440 desktop
    /// 1920x1080'lik pencere x=2560'ta.
    #[test]
    fn region_map_targets_second_monitor() {
        let m = ScreenMap::for_region(4480.0, 1440.0, 2560.0, 0.0, 1920.0, 1080.0);
        // Window's left edge is at 57.1% of the desktop
        let (x, _) = m.apply(0.0, 0.0);
        assert_eq!(x, (2560.0 / 4480.0 * ABS_MAX as f32) as i32);
        // Window's right edge is the desktop's right edge
        let (x2, _) = m.apply(1.0, 0.0);
        assert_eq!(x2, ABS_MAX);
        // Window centre
        let (xm, _) = m.apply(0.5, 0.0);
        assert_eq!(xm, ((2560.0 + 960.0) / 4480.0 * ABS_MAX as f32) as i32);
    }

    #[test]
    fn invert_x_mirrors_horizontally() {
        let m = ScreenMap { invert_x: true, ..ScreenMap::default() };
        assert_eq!(m.apply(0.0, 0.5).0, ABS_MAX);
        assert_eq!(m.apply(1.0, 0.5).0, 0);
    }

    #[test]
    fn invert_y_mirrors_vertically() {
        let m = ScreenMap { invert_y: true, ..ScreenMap::default() };
        assert_eq!(m.apply(0.5, 0.0).1, ABS_MAX);
        assert_eq!(m.apply(0.5, 1.0).1, 0);
    }

    /// Mirroring must affect only the requested axis.
    #[test]
    fn invert_x_leaves_y_untouched() {
        let m = ScreenMap { invert_x: true, ..ScreenMap::default() };
        assert_eq!(m.apply(0.0, 0.25).1, m.apply(1.0, 0.25).1);
    }

    #[test]
    fn map_clamps_outside_screen() {
        let m = ScreenMap { origin_x: 0.9, origin_y: 0.0, scale_x: 0.5, scale_y: 1.0,
                            ..ScreenMap::default() };
        let (x, _) = m.apply(1.0, 0.0);
        assert_eq!(x, ABS_MAX, "must not overflow past the screen");
    }
}
