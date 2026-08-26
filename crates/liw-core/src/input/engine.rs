//! Eşleme motoru: host girdisi → dokunuş eylemleri.
//!
//! Saf ve senkron tutulmuştur: içeride I/O yok, zaman kaynağı dışarıdan
//! verilir. Böylece tüm davranış birim testlerle doğrulanabilir — bir
//! keymapper'da "his" hatalarının çoğu durum makinesinde saklanır ve
//! elle denemekle yakalanamaz.

use super::profile::{Binding, Easing, Profile, Trigger};
use super::touch::{Norm, PointerPool, TouchAction, MAX_POINTERS};
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
    group: Option<String>,
    easing: Easing,
    id: u8,
    from: Norm,
    to: Norm,
    start_ms: u64,
    duration_ms: u64,
    /// Son gönderilen ara adım; aynı konumu tekrar göndermemek için.
    last_step: u32,
    /// Son GÖNDERİLEN konum. Öne yüklü eğrilerde sondaki adımlar birkaç
    /// piksel oynar; bunlar bilgi taşımaz ve parmağın "takıldığı" izlenimi
    /// vererek jestin basılı tutma sanılmasına yol açabilir.
    last_sent: Norm,
}

/// Ardışık iki adım bundan daha yakınsa ara adım gönderilmez.
/// 0.002 normalize ≈ 2540 piksel genişlikte ~5 piksel.
const MIN_STEP_DELTA: f32 = 0.002;

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
    #[must_use = "kapatma sırasında üretilen UP eylemleri gönderilmezse                   parmaklar ekranda kalır"]
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
    #[must_use = "üretilen dokunuş eylemleri arka uca GÖNDERİLMELİ;                   atılırsa parmak ekranda asılı kalır"]
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
            // Eğri, ZAMAN ilerlemesini MESAFE oranına çevirir. Adım sayısı
            // değişmez; değişen, her adımda ne kadar yol katedildiğidir.
            let t = sw.easing.apply(step as f32 / SWIPE_STEPS as f32);
            let at = Norm::new(
                sw.from.x + (sw.to.x - sw.from.x) * t,
                sw.from.y + (sw.to.y - sw.from.y) * t,
            );
            // Son adım HER ZAMAN gönderilir: jest hedefe varmalı.
            let is_final = step >= SWIPE_STEPS;
            let dx = at.x - sw.last_sent.x;
            let dy = at.y - sw.last_sent.y;
            let moved_enough = (dx * dx + dy * dy).sqrt() >= MIN_STEP_DELTA;
            if is_final || moved_enough {
                acts.push(TouchAction::Move { id: sw.id, at });
                sw.last_sent = at;
            }
            if is_final {
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

    /// Devam eden jest sayısı (test ve teşhis için).
    pub fn swipe_count(&self) -> usize { self.swipes.len() }

    #[must_use = "üretilen dokunuş eylemleri arka uca GÖNDERİLMELİ"]
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
                    None => {
                        // Sessizce yutmak "tuş bazen çalışmıyor" hatası
                        // üretir ve kullanıcı nedenini bulamaz.
                        tracing::warn!(bağlantı = %name, sınır = MAX_POINTERS,
                            "işaretçi havuzu dolu — bu dokunuş atlandı");
                        Vec::new()
                    }
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
            Binding::Swipe { from, to, duration_ms, ref group, easing, .. } => {
                // Aynı kaydırma zaten oynuyorsa yenisini başlatma.
                if self.swipes.iter().any(|s| s.binding == name) { return Vec::new(); }

                // Aynı gruptaki devam eden jestleri İPTAL ET: parmağı
                // bulunduğu yerden kaldır, jesti tamamlama. Kullanıcı fikrini
                // değiştirmiştir; yarım kalan kaydırma oyunda çoğu zaman
                // eşiğin altında kalır ve yanlış hareket üretmez.
                let mut acts = Vec::new();
                if let Some(g) = group {
                    let cancelled: Vec<(String, u8)> = self.swipes.iter()
                        .filter(|s| s.group.as_deref() == Some(g.as_str()))
                        .map(|s| (s.binding.clone(), s.id))
                        .collect();
                    for (b, id) in cancelled {
                        acts.push(TouchAction::Up { id });
                        self.pool.release(&b);
                        self.swipes.retain(|s| s.binding != b);
                    }
                }

                match self.pool.acquire(&name) {
                    Some(id) => {
                        self.swipes.push(ActiveSwipe {
                            binding: name.clone(), group: group.clone(), easing, id, from, to,
                            start_ms: self.now_ms,
                            duration_ms: duration_ms as u64,
                            last_step: 0,
                            last_sent: from,
                        });
                        acts.push(TouchAction::Down { id, at: from });
                        acts
                    }
                    None => acts,
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
        let Some((name, Binding::Aim {
            toggle, origin, sensitivity, deadzone, recenter_margin,
        })) = self.profile.bindings.iter()
                .find(|(_, b)| matches!(b, Binding::Aim { .. }))
                .map(|(n, b)| (n.clone(), b.clone()))
        else { return Vec::new() };

        if (dx * dx + dy * dy).sqrt() < deadzone { return Vec::new(); }

        // toggle yoksa nişan HER ZAMAN etkin: ilk fare hareketinde parmağı
        // indir. FPS'te bakış için tuşa basılı tutmak gerekmemeli.
        let mut acts = Vec::new();
        if self.aim_pos.is_none() {
            if toggle.is_some() { return acts; }
            let Some(id) = self.pool.acquire(&name) else { return acts };
            self.aim_pos = Some(origin);
            acts.push(TouchAction::Down { id, at: origin });
        }
        let pos = self.aim_pos.expect("yukarıda kuruldu");
        let Some(id) = self.pool.get(&name) else { return acts };

        let nx = pos.x + dx * sensitivity;
        let ny = pos.y + dy * sensitivity;
        let m = recenter_margin.clamp(0.01, 0.45);

        if nx < m || nx > 1.0 - m || ny < m || ny > 1.0 - m {
            // Kenara gelindi: parmağı kaldır, merkeze koy VE bu karenin
            // hareketini merkezden uygula.
            //
            // Hareketi atmak birikimli kaymaya yol açıyor: her ortalamada
            // bir miktar dönüş kayboluyor ve fare ile görüş birbirinden
            // ayrılıyor. Oyun `Down`'da referansı sıfırladığı için
            // `origin + delta` doğru dönüşü verir — hiçbir şey kaybolmaz.
            acts.push(TouchAction::Up { id });
            self.pool.release(&name);
            let Some(id2) = self.pool.acquire(&name) else {
                self.aim_pos = None;
                return acts;
            };
            acts.push(TouchAction::Down { id: id2, at: origin });
            let after = Norm::new(
                (origin.x + dx * sensitivity).clamp(m, 1.0 - m),
                (origin.y + dy * sensitivity).clamp(m, 1.0 - m),
            );
            self.aim_pos = Some(after);
            acts.push(TouchAction::Move { id: id2, at: after });
            return acts;
        }

        let next = Norm::new(nx, ny);
        self.aim_pos = Some(next);
        acts.push(TouchAction::Move { id, at: next });
        acts
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
            recenter_margin: 0.12,
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
        let _ = e.handle(InputEvent::Press(key(W)));
        let a = e.handle(InputEvent::Press(key(D)));
        assert!(matches!(a[..], [TouchAction::Move { .. }]), "ikinci yön Move olmalı: {a:?}");
    }

    /// Köşegen hareket hızlanmamalı — normalize edilmiş olmalı.
    #[test]
    fn diagonal_is_normalised_not_faster() {
        let mut e = Engine::new(joystick_profile());
        let _ = e.handle(InputEvent::Press(key(W)));
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
        let _ = e.handle(InputEvent::Press(key(W)));
        let _ = e.handle(InputEvent::Press(key(D)));
        let a = e.handle(InputEvent::Release(key(W)));
        assert!(matches!(a[..], [TouchAction::Move { .. }]),
                "hâlâ D basılı, parmak kalkmamalı: {a:?}");
    }

    #[test]
    fn releasing_last_direction_lifts_finger() {
        let mut e = Engine::new(joystick_profile());
        let _ = e.handle(InputEvent::Press(key(W)));
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
        let _ = e.handle(InputEvent::Press(TriggerKind::MouseRight));
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
        let _ = e.handle(InputEvent::Press(TriggerKind::MouseRight));
        assert!(e.handle(InputEvent::MouseMove { dx: 0.2, dy: 0.1 }).is_empty());
    }

    /// toggle TANIMLIYKEN, basılmadan fare hareketi dokunuş üretmemeli.
    #[test]
    fn mouse_move_without_aim_active_does_nothing() {
        let mut e = Engine::new(aim_profile());
        assert!(e.handle(InputEvent::MouseMove { dx: 100.0, dy: 0.0 }).is_empty());
    }

    /// toggle YOKSA nişan her zaman etkin: ilk fare hareketi parmağı indirir.
    /// FPS'te bakış için tuşa basılı tutmak gerekmemeli.
    fn always_on_aim() -> Engine {
        let mut b = BTreeMap::new();
        b.insert("bakis".into(), Binding::Aim {
            toggle: None,
            origin: Norm::new(0.5, 0.5),
            sensitivity: 0.001,
            deadzone: 0.5,
            recenter_margin: 0.12,
        });
        Engine::new(Profile { name: "t".into(), package: "p".into(), bindings: b })
    }

    /// İlk hareket parmağı indirir VE aynı çağrıda hareket ettirir:
    /// bir kare beklemek gereksiz gecikme olurdu.
    #[test]
    fn aim_without_toggle_activates_on_first_motion() {
        let mut e = always_on_aim();
        let a = e.handle(InputEvent::MouseMove { dx: 50.0, dy: 0.0 });
        match a[..] {
            [TouchAction::Down { at: d, .. }, TouchAction::Move { at: m, .. }] => {
                assert!((d.x - 0.5).abs() < 1e-5, "merkezde başlamalı: {d:?}");
                assert!((m.x - 0.55).abs() < 1e-5, "0.5 + 50*0.001: {m:?}");
            }
            _ => panic!("Down+Move bekleniyordu: {a:?}"),
        }
    }

    /// Kenara gelince parmak KALDIRILIP merkeze konmalı; yoksa sınırlı
    /// açıdan fazla dönülemez.
    #[test]
    fn aim_recenters_at_edge() {
        let mut e = always_on_aim();
        e.handle(InputEvent::MouseMove { dx: 10.0, dy: 0.0 });   // -> 0.51
        // 0.51 + 400*0.001 = 0.91 > 1 - 0.12 = 0.88  -> yeniden ortala
        let a = e.handle(InputEvent::MouseMove { dx: 400.0, dy: 0.0 });
        match a[..] {
            [TouchAction::Up { .. }, TouchAction::Down { at: d, .. },
             TouchAction::Move { .. }] =>
                assert!((d.x - 0.5).abs() < 1e-5, "merkeze dönmeli: {d:?}"),
            _ => panic!("Up+Down+Move bekleniyordu: {a:?}"),
        }
    }

    /// Yeniden ortalama HAREKET KAYBETMEMELİ.
    ///
    /// Kaybederse her ortalamada bir miktar dönüş yok olur ve fare ile
    /// görüş birbirinden ayrılır — kullanıcı bunu "aimde kayma" diye yaşar.
    #[test]
    fn recentering_preserves_the_frames_motion() {
        let mut e = always_on_aim();
        e.handle(InputEvent::MouseMove { dx: 10.0, dy: 0.0 });   // -> 0.51
        let a = e.handle(InputEvent::MouseMove { dx: 400.0, dy: 0.0 });
        let moved = a.iter().find_map(|x| match x {
            TouchAction::Move { at, .. } => Some(*at), _ => None,
        }).expect("ortalama sonrası hareket uygulanmalı");
        // origin 0.5 + 400*0.001 = 0.9 -> güvenli alana kırpılır (0.88)
        assert!(moved.x > 0.5 + 1e-6,
            "merkezden ileri gitmeli, {} bulundu", moved.x);
    }

    /// Toplam dönüş, ortalama olsun olmasın fare mesafesiyle ORANTILI kalmalı.
    #[test]
    fn total_rotation_survives_many_recenters() {
        let mut e = always_on_aim();
        let mut moves = 0usize;
        let mut recenters = 0usize;
        for _ in 0..40 {
            for act in e.handle(InputEvent::MouseMove { dx: 300.0, dy: 0.0 }) {
                match act {
                    TouchAction::Move { .. } => moves += 1,
                    TouchAction::Down { .. } => recenters += 1,
                    _ => {}
                }
            }
        }
        assert!(recenters > 3, "bu mesafede birden çok ortalama olmalı");
        // Her karede bir hareket üretilmeli; hiçbiri atlanmamalı.
        assert_eq!(moves, 40, "her karede hareket uygulanmalı, {moves} bulundu");
    }

    /// Yeniden ortalama sonrası dönüş DEVAM etmeli.
    #[test]
    fn aim_continues_after_recenter() {
        let mut e = always_on_aim();
        e.handle(InputEvent::MouseMove { dx: 10.0, dy: 0.0 });
        e.handle(InputEvent::MouseMove { dx: 400.0, dy: 0.0 });  // yeniden ortalar
        // Küçük bir hareket: ortalama sonrası normal Move üretmeli.
        let a = e.handle(InputEvent::MouseMove { dx: -100.0, dy: 0.0 });
        match a[..] {
            [TouchAction::Move { at, .. }] =>
                assert!(at.x < 0.88, "geriye hareket etmeli: {at:?}"),
            _ => panic!("tek Move bekleniyordu: {a:?}"),
        }
    }

    /// Dikey kenar da yeniden ortalamalı.
    #[test]
    fn aim_recenters_on_vertical_edge() {
        let mut e = always_on_aim();
        e.handle(InputEvent::MouseMove { dx: 0.0, dy: 10.0 });   // -> 0.51
        // 0.51 - 450*0.001 = 0.06 < 0.12  -> yeniden ortala
        let a = e.handle(InputEvent::MouseMove { dx: 0.0, dy: -450.0 });
        assert!(matches!(a[..], [TouchAction::Up { .. }, TouchAction::Down { .. },
                                 TouchAction::Move { .. }]), "{a:?}");
    }

    /// Yeniden ortalama işaretçi kimliğini tüketmemeli.
    #[test]
    fn recentering_does_not_leak_pointers() {
        let mut e = always_on_aim();
        e.handle(InputEvent::MouseMove { dx: 10.0, dy: 0.0 });
        for _ in 0..50 {
            e.handle(InputEvent::MouseMove { dx: 400.0, dy: 0.0 });
        }
        // Hâlâ çalışıyor olmalı: havuz tükenmiş olsaydı boş dönerdi.
        let a = e.handle(InputEvent::MouseMove { dx: 20.0, dy: 0.0 });
        assert!(!a.is_empty(), "havuz sızdırmış olabilir");
    }

    /// Motor kapatılınca takılı parmak kalmamalı.
    #[test]
    fn disabling_lifts_every_held_finger() {
        let mut e = Engine::new(joystick_profile());
        let _ = e.handle(InputEvent::Press(key(W)));
        let _ = e.handle(InputEvent::Press(key(SPACE)));
        let acts = e.set_enabled(false);
        assert_eq!(acts.len(), 2, "iki parmak da kalkmalı: {acts:?}");
        assert!(acts.iter().all(|a| matches!(a, TouchAction::Up { .. })));
    }

    #[test]
    fn disabled_engine_produces_nothing() {
        let mut e = Engine::new(joystick_profile());
        let _ = e.set_enabled(false);
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
            group: None, easing: Easing::Linear,
        });
        Engine::new(Profile { name: "t".into(), package: "p".into(), bindings: b })
    }

    /// Aynı grupta iki kaydırma: ikincisi birincisini İPTAL etmeli.
    /// Gerçek oyun geri bildirimi: A'ya sonra hızlıca W'ye basınca
    /// oyun iki ayrı parmak görüyordu ve hareketler karışıyordu.
    fn grouped_engine() -> Engine {
        let mut b = BTreeMap::new();
        b.insert("sol".into(), Binding::Swipe {
            trigger: Trigger::Key(A),
            from: Norm::new(0.5, 0.5), to: Norm::new(0.2, 0.5), duration_ms: 80,
            group: Some("hareket".into()), easing: Easing::Linear,
        });
        b.insert("zipla".into(), Binding::Swipe {
            trigger: Trigger::Key(W),
            from: Norm::new(0.5, 0.6), to: Norm::new(0.5, 0.3), duration_ms: 80,
            group: Some("hareket".into()), easing: Easing::Linear,
        });
        b.insert("ates".into(), Binding::Swipe {
            trigger: Trigger::Key(SPACE),
            from: Norm::new(0.9, 0.8), to: Norm::new(0.9, 0.7), duration_ms: 80,
            group: None, easing: Easing::Linear,
        });
        Engine::new(Profile { name: "t".into(), package: "p".into(), bindings: b })
    }

    #[test]
    fn second_gesture_in_group_cancels_the_first() {
        let mut e = grouped_engine();
        let a = e.handle(InputEvent::Press(key(A)));
        let first_id = match a[..] { [TouchAction::Down { id, .. }] => id, _ => panic!("{a:?}") };
        let _ = e.tick(20);
        let w = e.handle(InputEvent::Press(key(W)));
        // Önce iptal (Up), sonra yeni jest (Down).
        match w[..] {
            [TouchAction::Up { id }, TouchAction::Down { .. }] =>
                assert_eq!(id, first_id, "iptal edilen parmak ilki olmalı"),
            _ => panic!("Up sonra Down bekleniyordu: {w:?}"),
        }
        assert_eq!(e.swipe_count(), 1, "yalnızca yeni jest kalmalı");
    }

    /// İptal edilen jest artık ilerlememeli.
    #[test]
    fn cancelled_gesture_stops_ticking() {
        let mut e = grouped_engine();
        let _ = e.handle(InputEvent::Press(key(A)));
        let _ = e.tick(20);
        let _ = e.handle(InputEvent::Press(key(W)));
        // Sonraki tick'ler yalnızca W'nin jestini ilerletmeli: dikey hareket.
        let mut moves = Vec::new();
        for ms in (25..=110).step_by(5) {
            for act in e.tick(ms) {
                if let TouchAction::Move { at, .. } = act { moves.push(at); }
            }
        }
        assert!(!moves.is_empty());
        assert!(moves.iter().all(|m| (m.x - 0.5).abs() < 1e-4),
                "yalnızca dikey hareket olmalı, yatay iz kalmamalı");
    }

    /// Grupsuz jest gruptakilerden ETKİLENMEMELİ — nişancı oyunlarında
    /// joystick + nişan + ateş eşzamanlı olmalı.
    #[test]
    fn ungrouped_gesture_is_not_cancelled() {
        let mut e = grouped_engine();
        let _ = e.handle(InputEvent::Press(key(SPACE)));
        let _ = e.tick(10);
        let a = e.handle(InputEvent::Press(key(A)));
        assert!(matches!(a[..], [TouchAction::Down { .. }]),
                "grupsuz jest iptal edilmemeli: {a:?}");
        assert_eq!(e.swipe_count(), 2, "iki jest birlikte sürmeli");
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
        let _ = e.handle(InputEvent::Press(key(A)));
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
        let _ = e.handle(InputEvent::Press(key(A)));
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
        let _ = e.handle(InputEvent::Press(key(A)));
        let _ = e.tick(20);
        let a = e.handle(InputEvent::Release(key(A)));
        assert!(a.is_empty(), "bırakma olay üretmemeli: {a:?}");
        assert!(e.has_pending(), "jest devam etmeli");
    }

    /// Aynı kaydırma oynarken tekrar basmak ikinci jest başlatmamalı.
    #[test]
    fn repeated_press_does_not_stack_swipes() {
        let mut e = swipe_engine();
        let _ = e.handle(InputEvent::Press(key(A)));
        let _ = e.handle(InputEvent::Release(key(A)));
        let a = e.handle(InputEvent::Press(key(A)));
        assert!(a.is_empty(), "ikinci jest başlamamalı: {a:?}");
    }

    /// Öne yüklü eğride sondaki mikro adımlar elenmeli, ama jest yine
    /// hedefe VARMALI ve parmak kalkmalı.
    #[test]
    fn tiny_tail_steps_are_dropped_but_target_is_reached() {
        use crate::input::profile::Easing;
        let mut b = BTreeMap::new();
        b.insert("sol".into(), Binding::Swipe {
            trigger: Trigger::Key(A),
            from: Norm::new(0.5, 0.5), to: Norm::new(0.2, 0.5), duration_ms: 80,
            group: None, easing: Easing::EaseOutStrong,
        });
        let mut e = Engine::new(Profile { name: "t".into(), package: "p".into(), bindings: b });
        let _ = e.handle(InputEvent::Press(key(A)));

        let mut moves: Vec<Norm> = Vec::new();
        let mut lifted = false;
        for ms in (0..=100).step_by(2) {
            for act in e.tick(ms) {
                match act {
                    TouchAction::Move { at, .. } => moves.push(at),
                    TouchAction::Up { .. } => lifted = true,
                    _ => {}
                }
            }
        }
        assert!(lifted, "parmak kalkmalı");
        let last = moves.last().expect("hareket yok");
        assert!((last.x - 0.2).abs() < 1e-4, "hedefe varmalı: {last:?}");

        // Ardışık adımlar arasında (son hariç) anlamsız mesafe kalmamalı.
        for w in moves.windows(2).take(moves.len().saturating_sub(2)) {
            let d = ((w[1].x - w[0].x).powi(2) + (w[1].y - w[0].y).powi(2)).sqrt();
            assert!(d >= MIN_STEP_DELTA * 0.99,
                "anlamsız ara adım kaldı: {d} < {MIN_STEP_DELTA}");
        }
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
        let _ = e.handle(InputEvent::Press(key(A)));
        assert!(e.has_pending());
        let _ = e.set_enabled(false);
        assert!(!e.has_pending());
    }
}

#[cfg(test)]
mod must_use_guard {
    //! `tick` ve `handle` dönüşlerinin atılması bugün gerçek bir hataya yol
    //! açtı: önceki jestin UP'ı kayboluyor ve parmak ekranda asılı kalıyordu.
    //! `#[must_use]` bunu derleme zamanında yakalar; bu modül niyeti belgeler.
    use super::*;

    #[test]
    fn tick_and_handle_return_actions_that_must_be_dispatched() {
        let mut b = std::collections::BTreeMap::new();
        b.insert("t".into(), Binding::Tap {
            trigger: Trigger::Key(57), at: Norm::new(0.5, 0.5),
        });
        let mut e = Engine::new(Profile {
            name: "t".into(), package: "p".into(), bindings: b,
        });
        let down = e.handle(InputEvent::Press(TriggerKind::Key(57)));
        assert!(!down.is_empty(), "eylemler çağırana DÖNMELİ, içeride tutulmamalı");
        let up = e.handle(InputEvent::Release(TriggerKind::Key(57)));
        assert!(matches!(up[..], [TouchAction::Up { .. }]));
    }
}
