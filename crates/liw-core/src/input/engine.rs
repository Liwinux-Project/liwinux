//! Eşleme motoru: host girdisi → dokunuş eylemleri.
//!
//! Saf ve senkron tutulmuştur: içeride I/O yok, zaman kaynağı dışarıdan
//! verilir. Böylece tüm davranış birim testlerle doğrulanabilir — bir
//! keymapper'da "his" hatalarının çoğu durum makinesinde saklanır ve
//! elle denemekle yakalanamaz.

use super::profile::{Binding, Profile, Trigger};
use super::touch::{Norm, PointerPool, TouchAction};
use std::collections::HashSet;

/// Motora gelen ham girdi olayı.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputEvent {
    Press(Trigger2),
    Release(Trigger2),
    /// Bağıl fare hareketi (piksel).
    MouseMove { dx: f32, dy: f32 },
}

/// `Trigger`ın Copy olabilen ikizi (motor içi kullanım).
pub type Trigger2 = TriggerKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TriggerKind {
    Key(u16),
    MouseLeft,
    MouseRight,
    MouseMiddle,
    WheelUp,
    WheelDown,
}

impl From<&Trigger> for TriggerKind {
    fn from(t: &Trigger) -> Self {
        match t {
            Trigger::Key(k) => TriggerKind::Key(*k),
            Trigger::MouseLeft => TriggerKind::MouseLeft,
            Trigger::MouseRight => TriggerKind::MouseRight,
            Trigger::MouseMiddle => TriggerKind::MouseMiddle,
            Trigger::WheelUp => TriggerKind::WheelUp,
            Trigger::WheelDown => TriggerKind::WheelDown,
        }
    }
}

/// Devam eden bir kaydırma jesti.
///
/// Kaydırma "ateşle ve unut"tur: tuşa basınca tam jest oynar, tuşun ne kadar
/// basılı tutulduğu önemli değildir. Oyunlar kaydırmayı ara hareketlerden
/// tanır; tek sıçrama jest sayılmaz.
#[derive(Debug, Clone)]
struct ActiveSwipe {
    binding: String,
    id: u8,
    from: Norm,
    to: Norm,
    start_ms: u64,
    duration_ms: u64,
    /// Son gönderilen ara adım; aynı konumu tekrar göndermemek için.
    last_step: u32,
}

/// Kaydırma kaç ara adıma bölünsün. Az olursa jest tanınmaz, çok olursa
/// gereksiz olay üretilir. 12 adım 80ms'de ~7ms aralık demek — 144Hz'de bile yeterli.
const SWIPE_STEPS: u32 = 12;

pub struct Engine {
    profile: Profile,
    pool: PointerPool,
    held: HashSet<TriggerKind>,
    swipes: Vec<ActiveSwipe>,
    now_ms: u64,
    /// Aim modunda mevcut parmak konumu.
    aim_pos: Option<Norm>,
    /// Motor etkin mi. Kapalıyken hiçbir olay üretilmez ama takılı
    /// parmaklar bırakılır — aksi halde oyunda parmak asılı kalır.
    enabled: bool,
}

impl Engine {
    pub fn new(profile: Profile) -> Self {
        Self {
            profile, pool: PointerPool::new(), held: HashSet::new(),
            swipes: Vec::new(), now_ms: 0,
            aim_pos: None, enabled: true,
        }
    }

    pub fn profile(&self) -> &Profile { &self.profile }
    pub fn is_enabled(&self) -> bool { self.enabled }

    /// Motoru açar/kapatır. Kapatırken tüm parmakları kaldırır.
    pub fn set_enabled(&mut self, on: bool) -> Vec<TouchAction> {
        if self.enabled == on { return Vec::new(); }
        self.enabled = on;
        if !on {
            self.held.clear();
            self.aim_pos = None;
            self.swipes.clear();
            self.pool.release_all()
        } else { Vec::new() }
    }

    /// Bir tetikleyiciyi hangi bağlantının kullandığını bulur.
    fn owner(&self, t: TriggerKind) -> Option<(&str, &Binding)> {
        self.profile.bindings.iter()
            .find(|(_, b)| b.triggers().iter().any(|x| TriggerKind::from(x) == t))
            .map(|(n, b)| (n.as_str(), b))
    }

    /// Zamanı ilerletir ve devam eden jestlerin ara adımlarını üretir.
    ///
    /// Motor saf kalsın diye zaman DIŞARIDAN verilir; böylece jest zamanlaması
    /// gerçek saate ihtiyaç duymadan test edilebilir.
    pub fn tick(&mut self, now_ms: u64) -> Vec<TouchAction> {
        self.now_ms = now_ms;
        if !self.enabled { return Vec::new(); }
        let mut acts = Vec::new();
        let mut finished: Vec<String> = Vec::new();

        for sw in &mut self.swipes {
            let elapsed = now_ms.saturating_sub(sw.start_ms);
            let step = if sw.duration_ms == 0 { SWIPE_STEPS } else {
                ((elapsed * SWIPE_STEPS as u64) / sw.duration_ms).min(SWIPE_STEPS as u64) as u32
            };
            if step == sw.last_step { continue; }
            sw.last_step = step;
            let t = step as f32 / SWIPE_STEPS as f32;
            let at = Norm::new(
                sw.from.x + (sw.to.x - sw.from.x) * t,
                sw.from.y + (sw.to.y - sw.from.y) * t,
            );
            acts.push(TouchAction::Move { id: sw.id, at });
            if step >= SWIPE_STEPS {
                acts.push(TouchAction::Up { id: sw.id });
                finished.push(sw.binding.clone());
            }
        }
        for name in finished {
            self.pool.release(&name);
            self.swipes.retain(|s| s.binding != name);
        }
        acts
    }

    /// Devam eden bir jest var mı? Varsa çağıran `tick`i sık çağırmalı.
    pub fn has_pending(&self) -> bool { !self.swipes.is_empty() }

    pub fn handle(&mut self, ev: InputEvent) -> Vec<TouchAction> {
        if !self.enabled { return Vec::new(); }
        match ev {
            InputEvent::Press(t) => self.on_press(t),
            InputEvent::Release(t) => self.on_release(t),
            InputEvent::MouseMove { dx, dy } => self.on_mouse(dx, dy),
        }
    }

    fn on_press(&mut self, t: TriggerKind) -> Vec<TouchAction> {
        // Tuş tekrarını (auto-repeat) yut: aksi halde Down olayları yığılır.
        if !self.held.insert(t) { return Vec::new(); }
        let Some((name, binding)) = self.owner(t) else { return Vec::new() };
        let (name, binding) = (name.to_string(), binding.clone());
        match binding {
            Binding::Tap { at, .. } | Binding::Toggle { at, .. } => {
                match self.pool.acquire(&name) {
                    Some(id) => vec![TouchAction::Down { id, at }],
                    None => Vec::new(),
                }
            }
            Binding::Joystick { .. } => self.recompute_joystick(&name),
            Binding::Aim { origin, .. } => {
                self.aim_pos = Some(origin);
                match self.pool.acquire(&name) {
                    Some(id) => vec![TouchAction::Down { id, at: origin }],
                    None => Vec::new(),
                }
            }
            Binding::Swipe { from, to, duration_ms, .. } => {
                // Aynı kaydırma zaten oynuyorsa yenisini başlatma.
                if self.swipes.iter().any(|s| s.binding == name) { return Vec::new(); }
                match self.pool.acquire(&name) {
                    Some(id) => {
                        self.swipes.push(ActiveSwipe {
                            binding: name.clone(), id, from, to,
                            start_ms: self.now_ms,
                            duration_ms: duration_ms as u64,
                            last_step: 0,
                        });
                        vec![TouchAction::Down { id, at: from }]
                    }
                    None => Vec::new(),
                }
            }
        }
    }

    fn on_release(&mut self, t: TriggerKind) -> Vec<TouchAction> {
        if !self.held.remove(&t) { return Vec::new(); }
        let Some((name, binding)) = self.owner(t) else { return Vec::new() };
        let (name, binding) = (name.to_string(), binding.clone());
        match binding {
            Binding::Joystick { .. } => {
                // Hâlâ basılı yön varsa parmak kalkmaz, sadece yeniden konumlanır.
                let acts = self.recompute_joystick(&name);
                if acts.is_empty() {
                    self.pool.release(&name).map(|id| vec![TouchAction::Up { id }])
                        .unwrap_or_default()
                } else { acts }
            }
            Binding::Aim { .. } => {
                self.aim_pos = None;
                self.pool.release(&name).map(|id| vec![TouchAction::Up { id }])
                    .unwrap_or_default()
            }
            // Kaydırma ateşle-ve-unut: tuşun bırakılması jesti etkilemez.
            // Yarıda kesmek oyunlarda "yanlış kaydırma" olarak algılanır.
            Binding::Swipe { .. } => Vec::new(),
            _ => self.pool.release(&name).map(|id| vec![TouchAction::Up { id }])
                    .unwrap_or_default(),
        }
    }

    /// Basılı yönlere göre joystick parmağını konumlandırır.
    /// Hiç yön basılı değilse boş döner (çağıran parmağı kaldırır).
    fn recompute_joystick(&mut self, name: &str) -> Vec<TouchAction> {
        let Some(Binding::Joystick { up, down, left, right, center, radius }) =
            self.profile.bindings.get(name).cloned() else { return Vec::new() };
        let mut dx = 0.0f32;
        let mut dy = 0.0f32;
        if self.held.contains(&TriggerKind::from(&up))    { dy -= 1.0; }
        if self.held.contains(&TriggerKind::from(&down))  { dy += 1.0; }
        if self.held.contains(&TriggerKind::from(&left))  { dx -= 1.0; }
        if self.held.contains(&TriggerKind::from(&right)) { dx += 1.0; }
        if dx == 0.0 && dy == 0.0 { return Vec::new(); }

        // Köşegende hız artmasın diye normalize et — aksi halde çapraz
        // hareket %41 daha hızlı olur ve oyuncu bunu "kayıyor" diye hisseder.
        let len = (dx * dx + dy * dy).sqrt();
        let at = Norm::new(center.x + dx / len * radius, center.y + dy / len * radius);

        let first = self.pool.get(name).is_none();
        match self.pool.acquire(name) {
            Some(id) if first => vec![TouchAction::Down { id, at }],
            Some(id) => vec![TouchAction::Move { id, at }],
            None => Vec::new(),
        }
    }

    fn on_mouse(&mut self, dx: f32, dy: f32) -> Vec<TouchAction> {
        let Some((name, Binding::Aim { sensitivity, deadzone, .. })) =
            self.profile.bindings.iter()
                .find(|(_, b)| matches!(b, Binding::Aim { .. }))
                .map(|(n, b)| (n.clone(), b.clone()))
        else { return Vec::new() };

        let Some(pos) = self.aim_pos else { return Vec::new() };
        if (dx * dx + dy * dy).sqrt() < deadzone { return Vec::new(); }

        let next = Norm::new(pos.x + dx * sensitivity, pos.y + dy * sensitivity);
        self.aim_pos = Some(next);
        match self.pool.get(&name) {
            Some(id) => vec![TouchAction::Move { id, at: next }],
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::profile::Binding;
    use std::collections::BTreeMap;

    const W: u16 = 17; const A: u16 = 30; const S: u16 = 31; const D: u16 = 32;
    const SPACE: u16 = 57;

    fn joystick_profile() -> Profile {
        let mut b = BTreeMap::new();
        b.insert("hareket".into(), Binding::Joystick {
            up: Trigger::Key(W), down: Trigger::Key(S),
            left: Trigger::Key(A), right: Trigger::Key(D),
            center: Norm::new(0.2, 0.7), radius: 0.1,
        });
        b.insert("zipla".into(), Binding::Tap {
            trigger: Trigger::Key(SPACE), at: Norm::new(0.9, 0.8),
        });
        Profile { name: "t".into(), package: "p".into(), bindings: b }
    }

    fn aim_profile() -> Profile {
        let mut b = BTreeMap::new();
        b.insert("nisan".into(), Binding::Aim {
            toggle: Some(Trigger::MouseRight),
            origin: Norm::new(0.5, 0.5),
            sensitivity: 0.001,
            deadzone: 0.5,
        });
        Profile { name: "t".into(), package: "p".into(), bindings: b }
    }

    fn key(k: u16) -> TriggerKind { TriggerKind::Key(k) }

    #[test]
    fn tap_presses_and_lifts() {
        let mut e = Engine::new(joystick_profile());
        let down = e.handle(InputEvent::Press(key(SPACE)));
        assert!(matches!(down[..], [TouchAction::Down { .. }]));
        let up = e.handle(InputEvent::Release(key(SPACE)));
        assert!(matches!(up[..], [TouchAction::Up { .. }]));
    }

    /// Klavye auto-repeat Down olaylarını yığmamalı.
    #[test]
    fn key_autorepeat_is_swallowed() {
        let mut e = Engine::new(joystick_profile());
        assert_eq!(e.handle(InputEvent::Press(key(SPACE))).len(), 1);
        assert!(e.handle(InputEvent::Press(key(SPACE))).is_empty(), "tekrar Down üretmemeli");
        assert!(e.handle(InputEvent::Press(key(SPACE))).is_empty());
    }

    #[test]
    fn joystick_first_direction_puts_finger_down() {
        let mut e = Engine::new(joystick_profile());
        let a = e.handle(InputEvent::Press(key(W)));
        match a[..] {
            [TouchAction::Down { at, .. }] => {
                assert!((at.y - 0.6).abs() < 1e-5, "yukarı = merkez - yarıçap, {at:?}");
                assert!((at.x - 0.2).abs() < 1e-5);
            }
            _ => panic!("Down bekleniyordu: {a:?}"),
        }
    }

    #[test]
    fn joystick_second_direction_moves_not_redowns() {
        let mut e = Engine::new(joystick_profile());
        e.handle(InputEvent::Press(key(W)));
        let a = e.handle(InputEvent::Press(key(D)));
        assert!(matches!(a[..], [TouchAction::Move { .. }]), "ikinci yön Move olmalı: {a:?}");
    }

    /// Köşegen hareket hızlanmamalı — normalize edilmiş olmalı.
    #[test]
    fn diagonal_is_normalised_not_faster() {
        let mut e = Engine::new(joystick_profile());
        e.handle(InputEvent::Press(key(W)));
        let a = e.handle(InputEvent::Press(key(D)));
        let at = match a[..] { [TouchAction::Move { at, .. }] => at, _ => panic!() };
        let dist = ((at.x - 0.2).powi(2) + (at.y - 0.7).powi(2)).sqrt();
        assert!((dist - 0.1).abs() < 1e-4,
                "çapraz mesafe yarıçapa eşit olmalı, {dist} bulundu");
    }

    /// Bir yön bırakılınca diğeri basılıysa parmak KALKMAMALI.
    #[test]
    fn releasing_one_direction_keeps_finger_down() {
        let mut e = Engine::new(joystick_profile());
        e.handle(InputEvent::Press(key(W)));
        e.handle(InputEvent::Press(key(D)));
        let a = e.handle(InputEvent::Release(key(W)));
        assert!(matches!(a[..], [TouchAction::Move { .. }]),
                "hâlâ D basılı, parmak kalkmamalı: {a:?}");
    }

    #[test]
    fn releasing_last_direction_lifts_finger() {
        let mut e = Engine::new(joystick_profile());
        e.handle(InputEvent::Press(key(W)));
        let a = e.handle(InputEvent::Release(key(W)));
        assert!(matches!(a[..], [TouchAction::Up { .. }]), "{a:?}");
    }

    #[test]
    fn joystick_and_tap_use_separate_pointers() {
        let mut e = Engine::new(joystick_profile());
        let j = e.handle(InputEvent::Press(key(W)));
        let t = e.handle(InputEvent::Press(key(SPACE)));
        let jid = match j[..] { [TouchAction::Down { id, .. }] => id, _ => panic!() };
        let tid = match t[..] { [TouchAction::Down { id, .. }] => id, _ => panic!() };
        assert_ne!(jid, tid, "eşzamanlı bağlantılar ayrı işaretçi almalı");
    }

    #[test]
    fn unmapped_key_produces_nothing() {
        let mut e = Engine::new(joystick_profile());
        assert!(e.handle(InputEvent::Press(key(99))).is_empty());
    }

    #[test]
    fn aim_moves_finger_from_origin() {
        let mut e = Engine::new(aim_profile());
        e.handle(InputEvent::Press(TriggerKind::MouseRight));
        let a = e.handle(InputEvent::MouseMove { dx: 100.0, dy: 0.0 });
        match a[..] {
            [TouchAction::Move { at, .. }] =>
                assert!((at.x - 0.6).abs() < 1e-5, "0.5 + 100*0.001 = 0.6, {at:?}"),
            _ => panic!("Move bekleniyordu: {a:?}"),
        }
    }

    /// Deadzone altındaki gürültü olay üretmemeli.
    #[test]
    fn aim_ignores_jitter_below_deadzone() {
        let mut e = Engine::new(aim_profile());
        e.handle(InputEvent::Press(TriggerKind::MouseRight));
        assert!(e.handle(InputEvent::MouseMove { dx: 0.2, dy: 0.1 }).is_empty());
    }

    /// Aim etkin değilken fare hareketi dokunuş üretmemeli.
    #[test]
    fn mouse_move_without_aim_active_does_nothing() {
        let mut e = Engine::new(aim_profile());
        assert!(e.handle(InputEvent::MouseMove { dx: 100.0, dy: 0.0 }).is_empty());
    }

    /// Motor kapatılınca takılı parmak kalmamalı.
    #[test]
    fn disabling_lifts_every_held_finger() {
        let mut e = Engine::new(joystick_profile());
        e.handle(InputEvent::Press(key(W)));
        e.handle(InputEvent::Press(key(SPACE)));
        let acts = e.set_enabled(false);
        assert_eq!(acts.len(), 2, "iki parmak da kalkmalı: {acts:?}");
        assert!(acts.iter().all(|a| matches!(a, TouchAction::Up { .. })));
    }

    #[test]
    fn disabled_engine_produces_nothing() {
        let mut e = Engine::new(joystick_profile());
        e.set_enabled(false);
        assert!(e.handle(InputEvent::Press(key(SPACE))).is_empty());
    }

    /// Basılmamış tuşun bırakılması olay üretmemeli (odak kaybı sonrası).
    #[test]
    fn release_without_press_is_ignored() {
        let mut e = Engine::new(joystick_profile());
        assert!(e.handle(InputEvent::Release(key(SPACE))).is_empty());
    }

    fn swipe_engine() -> Engine {
        let mut b = BTreeMap::new();
        b.insert("sol".into(), Binding::Swipe {
            trigger: Trigger::Key(A),
            from: Norm::new(0.5, 0.5), to: Norm::new(0.2, 0.5), duration_ms: 80,
        });
        Engine::new(Profile { name: "t".into(), package: "p".into(), bindings: b })
    }

    #[test]
    fn swipe_starts_with_finger_down_at_origin() {
        let mut e = swipe_engine();
        let a = e.handle(InputEvent::Press(key(A)));
        match a[..] {
            [TouchAction::Down { at, .. }] => assert!((at.x - 0.5).abs() < 1e-5),
            _ => panic!("{a:?}"),
        }
        assert!(e.has_pending(), "jest devam etmeli");
    }

    /// Kaydırma ARA ADIMLAR üretmeli — tek sıçrama oyunlarda jest sayılmaz.
    #[test]
    fn swipe_emits_intermediate_steps() {
        let mut e = swipe_engine();
        e.handle(InputEvent::Press(key(A)));
        let mut moves = 0;
        for ms in (0..=80).step_by(5) {
            for act in e.tick(ms) {
                if matches!(act, TouchAction::Move { .. }) { moves += 1; }
            }
        }
        assert!(moves >= 8, "en az 8 ara adım bekleniyordu, {moves} üretildi");
    }

    #[test]
    fn swipe_reaches_target_and_lifts() {
        let mut e = swipe_engine();
        e.handle(InputEvent::Press(key(A)));
        let mut last_pos = None;
        let mut lifted = false;
        for ms in (0..=100).step_by(5) {
            for act in e.tick(ms) {
                match act {
                    TouchAction::Move { at, .. } => last_pos = Some(at),
                    TouchAction::Up { .. } => lifted = true,
                    _ => {}
                }
            }
        }
        assert!(lifted, "jest sonunda parmak kalkmalı");
        let at = last_pos.expect("hiç hareket üretilmedi");
        assert!((at.x - 0.2).abs() < 1e-4, "hedefe ulaşmalı, {at:?}");
        assert!(!e.has_pending(), "biten jest listede kalmamalı");
    }

    /// Ateşle-ve-unut: tuşu bırakmak jesti kesmemeli.
    #[test]
    fn releasing_key_does_not_abort_swipe() {
        let mut e = swipe_engine();
        e.handle(InputEvent::Press(key(A)));
        e.tick(20);
        let a = e.handle(InputEvent::Release(key(A)));
        assert!(a.is_empty(), "bırakma olay üretmemeli: {a:?}");
        assert!(e.has_pending(), "jest devam etmeli");
    }

    /// Aynı kaydırma oynarken tekrar basmak ikinci jest başlatmamalı.
    #[test]
    fn repeated_press_does_not_stack_swipes() {
        let mut e = swipe_engine();
        e.handle(InputEvent::Press(key(A)));
        e.handle(InputEvent::Release(key(A)));
        let a = e.handle(InputEvent::Press(key(A)));
        assert!(a.is_empty(), "ikinci jest başlamamalı: {a:?}");
    }

    #[test]
    fn tick_without_pending_gesture_is_silent() {
        let mut e = swipe_engine();
        assert!(e.tick(1000).is_empty());
    }

    /// Motor kapatılınca devam eden jest de temizlenmeli.
    #[test]
    fn disabling_clears_pending_swipes() {
        let mut e = swipe_engine();
        e.handle(InputEvent::Press(key(A)));
        assert!(e.has_pending());
        e.set_enabled(false);
        assert!(!e.has_pending());
    }
}
