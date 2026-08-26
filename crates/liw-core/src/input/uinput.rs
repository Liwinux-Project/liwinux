//! Host tarafında sanal çoklu dokunmatik ekran.
//!
//! # Neden bu yol
//!
//! Waydroid konteynerinin `/dev`'i taze bir tmpfs'tir ve `/dev/input` içeri
//! bind EDİLMEZ — yani konteynerden host girdi düğümlerine erişilemez. Ama
//! ters yön çalışır: host'ta oluşturduğumuz sanal dokunmatik ekranı libinput
//! görür, KWin `wl_touch` olaylarına çevirir ve odaktaki pencereye (Waydroid)
//! iletir. Android bunu gerçek bir dokunmatik ekran olarak alır.
//!
//! ```text
//! uinput → libinput → KWin → wl_touch → Waydroid penceresi → Android
//! ```
//!
//! # Multi-touch protokolü
//!
//! Protokol B (slot tabanlı) kullanılır: her parmak bir slota bağlanır ve
//! `ABS_MT_TRACKING_ID` ile izlenir. `-1` yazmak parmağı kaldırır. Protokol A
//! (SYN_MT_REPORT) modern libinput'ta desteklenmiyor.
//!
//! # Koordinat uyarısı
//!
//! Dokunuş koordinatları **ekran uzayındadır**, pencere uzayında değil.
//! Waydroid tam ekran değilse dokunuşlar yanlış yere gider. Bu, arka ucun
//! bilinen sınırıdır ve `ScreenMap` ile telafi edilir.

use super::backend::{BackendError, TouchBackend};
use super::touch::{TouchAction, MAX_POINTERS};
use evdev::{
    uinput::VirtualDevice,
    AbsInfo, AbsoluteAxisCode, AttributeSet, EventType, InputEvent, KeyCode, PropType,
    UinputAbsSetup,
};

/// Sanal ekranın çözünürlüğü. Gerçek ekranla aynı olmalı ki KWin
/// koordinatları birebir eşlesin.
const ABS_MAX: i32 = 32767;

/// Normalize koordinatı sanal ekran uzayına taşır.
///
/// Waydroid penceresi tam ekran değilse, pencerenin ekran içindeki
/// konumu/oranı burada hesaba katılır.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct ScreenMap {
    /// Hedef pencerenin dokunmatik uzayındaki sol üst köşesi (0..1).
    pub origin_x: f32,
    pub origin_y: f32,
    /// Hedef pencerenin dokunmatik uzayındaki oranı (0..1).
    pub scale_x: f32,
    pub scale_y: f32,
    /// Eksen aynalaması. Çoklu monitör veya döndürülmüş çıkışlarda
    /// gerekebilir; ölçümle belirlenir, varsayılmaz.
    #[serde(default)]
    pub invert_x: bool,
    #[serde(default)]
    pub invert_y: bool,
}

impl Default for ScreenMap {
    /// Tam ekran, tek monitör: birebir eşleme.
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

    /// Masaüstü içindeki bir dikdörtgeni hedefleyen eşleme kurar.
    ///
    /// Çoklu monitörde sanal dokunmatik ekran tüm masaüstüne eşlenebilir;
    /// o durumda hedef pencerenin masaüstü içindeki payı hesaplanmalıdır.
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
    /// Hangi slot hangi işaretçi kimliğine ait. `None` = boş slot.
    slots: [Option<u8>; MAX_POINTERS],
    /// Android tarafında benzersiz olması gereken izleme kimliği sayacı.
    next_tracking_id: i32,
    active: usize,
}

impl UinputBackend {
    pub fn new(map: ScreenMap) -> Result<Self, BackendError> {
        let abs = |axis, max| {
            UinputAbsSetup::new(axis, AbsInfo::new(0, 0, max, 0, 0, 1))
        };
        let mut keys = AttributeSet::<KeyCode>::new();
        // BTN_TOUCH olmadan libinput cihazı dokunmatik saymaz.
        keys.insert(KeyCode::BTN_TOUCH);

        let mut props = AttributeSet::<PropType>::new();
        // DIRECT = dokunmatik EKRAN (touchpad değil). Bu olmadan KWin
        // olayları imleç hareketi gibi yorumlar ve çoklu dokunuş kaybolur.
        props.insert(PropType::DIRECT);

        let dev = VirtualDevice::builder()
            .map_err(|e| BackendError::Init(format!("uinput açılamadı: {e} \
                (/dev/uinput erişimi var mı? kullanıcı 'input' grubunda mı?)")))?
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
            // Tek dokunuş eksenleri: bazı yığınlar hâlâ bunlara bakıyor.
            .with_absolute_axis(&abs(AbsoluteAxisCode::ABS_X, ABS_MAX))
            .map_err(|e| BackendError::Init(e.to_string()))?
            .with_absolute_axis(&abs(AbsoluteAxisCode::ABS_Y, ABS_MAX))
            .map_err(|e| BackendError::Init(e.to_string()))?
            .build()
            .map_err(|e| BackendError::Init(format!("sanal cihaz oluşturulamadı: {e}")))?;

        Ok(Self {
            dev, map,
            slots: [None; MAX_POINTERS],
            next_tracking_id: 1,
            active: 0,
        })
    }

    /// Oluşan cihazın `/dev/input/eventN` yolları (teşhis için).
    pub fn dev_nodes(&mut self) -> Vec<String> {
        self.dev.enumerate_dev_nodes_blocking()
            .map(|it| it.filter_map(|p| p.ok())
                       .map(|p| p.display().to_string()).collect())
            .unwrap_or_default()
    }

    pub fn set_map(&mut self, map: ScreenMap) { self.map = map; }

    fn slot_of(&self, id: u8) -> Option<usize> {
        self.slots.iter().position(|s| *s == Some(id))
    }
    fn free_slot(&self) -> Option<usize> {
        self.slots.iter().position(Option::is_none)
    }
}

impl TouchBackend for UinputBackend {
    fn dispatch(&mut self, actions: &[TouchAction]) -> Result<(), BackendError> {
        if actions.is_empty() { return Ok(()); }
        let mut evs: Vec<InputEvent> = Vec::with_capacity(actions.len() * 5);

        for act in actions {
            match *act {
                TouchAction::Down { id, at } => {
                    let Some(slot) = self.slot_of(id).or_else(|| self.free_slot()) else {
                        return Err(BackendError::Dispatch(
                            "boş MT slotu kalmadı".into()));
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
                        // İlk parmak: BTN_TOUCH basılı olmalı.
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
                    // -1 = parmak kalktı.
                    evs.push(InputEvent::new(EventType::ABSOLUTE.0,
                        AbsoluteAxisCode::ABS_MT_TRACKING_ID.0, -1));
                    if self.active == 0 {
                        evs.push(InputEvent::new(EventType::KEY.0, KeyCode::BTN_TOUCH.code(), 0));
                    }
                }
            }
        }

        // Tek SYN_REPORT: bu çağrıdaki tüm eylemler aynı kareye ait.
        // Her eylemden sonra SYN atmak çoklu dokunuşu ardışık tek
        // dokunuşlara böler ve jestleri bozar.
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

    #[test]
    fn fullscreen_map_is_identity() {
        let m = ScreenMap::default();
        assert_eq!(m.apply(0.0, 0.0), (0, 0));
        assert_eq!(m.apply(1.0, 1.0), (ABS_MAX, ABS_MAX));
        assert_eq!(m.apply(0.5, 0.5), (ABS_MAX / 2, ABS_MAX / 2));
    }

    /// Pencere ekranın sağ yarısındaysa, profil koordinatı oraya taşınmalı.
    #[test]
    fn windowed_map_offsets_and_scales() {
        let m = ScreenMap { origin_x: 0.5, origin_y: 0.0, scale_x: 0.5, scale_y: 1.0,
                            ..ScreenMap::default() };
        let (x, _) = m.apply(0.0, 0.0);
        assert_eq!(x, ABS_MAX / 2, "pencerenin solu ekranın ortası olmalı");
        let (x2, _) = m.apply(1.0, 0.0);
        assert_eq!(x2, ABS_MAX, "pencerenin sağı ekranın sağı olmalı");
    }

    /// İkinci monitördeki tam ekran pencere: 4480x1440 masaüstünde
    /// 1920x1080'lik pencere x=2560'ta.
    #[test]
    fn region_map_targets_second_monitor() {
        let m = ScreenMap::for_region(4480.0, 1440.0, 2560.0, 0.0, 1920.0, 1080.0);
        // Pencerenin solu masaüstünün %57.1'i
        let (x, _) = m.apply(0.0, 0.0);
        assert_eq!(x, (2560.0 / 4480.0 * ABS_MAX as f32) as i32);
        // Pencerenin sağı masaüstünün sağ kenarı
        let (x2, _) = m.apply(1.0, 0.0);
        assert_eq!(x2, ABS_MAX);
        // Pencerenin ortası
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

    /// Aynalama yalnızca istenen ekseni etkilemeli.
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
        assert_eq!(x, ABS_MAX, "ekran dışına taşmamalı");
    }
}
