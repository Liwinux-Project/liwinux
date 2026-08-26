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

pub struct Engine {
    profile: Profile,
    pool: PointerPool,
    held: HashSet<TriggerKind>,
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
            self.pool.release_all()
        } else { Vec::new() }
    }

    /// Bir tetikleyiciyi hangi bağlantının kullandığını bulur.
    fn owner(&self, t: TriggerKind) -> Option<(&str, &Binding)> {
        self.profile.bindings.iter()
            .find(|(_, b)| b.triggers().iter().any(|x| TriggerKind::from(x) == t))
            .map(|(n, b)| (n.as_str(), b))
    }

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
            Binding::Swipe { from, .. } => {
                match self.pool.acquire(&name) {
                    Some(id) => vec![TouchAction::Down { id, at: from }],
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
            Binding::Swipe { to, .. } => {
                // Basitleştirme: bırakınca hedefe taşı ve kaldır.
                // Zamanlanmış ara adımlar zamanlayıcı katmanında üretilecek.
                match self.pool.get(&name) {
                    Some(id) => {
                        self.pool.release(&name);
                        vec![TouchAction::Move { id, at: to }, TouchAction::Up { id }]
                    }
                    None => Vec::new(),
                }
            }
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

    #[test]
    fn swipe_moves_to_target_then_lifts() {
        let mut b = BTreeMap::new();
        b.insert("sol".into(), Binding::Swipe {
            trigger: Trigger::Key(A),
            from: Norm::new(0.5, 0.5), to: Norm::new(0.2, 0.5), duration_ms: 80,
        });
        let mut e = Engine::new(Profile { name: "t".into(), package: "p".into(), bindings: b });
        assert!(matches!(e.handle(InputEvent::Press(key(A)))[..], [TouchAction::Down { .. }]));
        let a = e.handle(InputEvent::Release(key(A)));
        assert!(matches!(a[..], [TouchAction::Move { .. }, TouchAction::Up { .. }]), "{a:?}");
    }
}
