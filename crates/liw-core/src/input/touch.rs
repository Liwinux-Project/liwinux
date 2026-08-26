//! Dokunuş modeli: işaretçi kimliği tahsisi ve olay üretimi.
//!
//! Android çoklu dokunuşta her parmağın **kararlı bir işaretçi kimliği** olmak
//! zorundadır. Kimlik yeniden kullanılırsa uygulama parmağın "ışınlandığını"
//! görür ve jest tanıma bozulur. Bu yüzden tahsis merkezi ve açıktır.

use std::collections::HashMap;

/// Android'in desteklediği eşzamanlı işaretçi sayısı.
pub const MAX_POINTERS: usize = 10;

/// Ekrandan bağımsız koordinat (0.0..1.0). Profiller çözünürlüğe bağlı olmasın diye.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Norm {
    pub x: f32,
    pub y: f32,
}

impl Norm {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x: x.clamp(0.0, 1.0), y: y.clamp(0.0, 1.0) }
    }
    /// Piksel koordinatına çevirir.
    pub fn to_px(self, w: u32, h: u32) -> (i32, i32) {
        ((self.x * w as f32).round() as i32, (self.y * h as f32).round() as i32)
    }
}

/// Enjekte edilecek tekil dokunuş eylemi.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TouchAction {
    Down { id: u8, at: Norm },
    Move { id: u8, at: Norm },
    Up { id: u8 },
}

/// Bir bağlantının (binding) sahip olduğu işaretçi.
///
/// Kimlik, bağlantı **etkin olduğu sürece** korunur; bırakılınca havuza döner.
#[derive(Debug, Default)]
pub struct PointerPool {
    /// binding kimliği -> işaretçi kimliği
    assigned: HashMap<String, u8>,
    /// hangi işaretçi kimlikleri kullanımda
    in_use: [bool; MAX_POINTERS],
}

impl PointerPool {
    pub fn new() -> Self { Self::default() }

    /// Bağlantıya işaretçi tahsis eder. Zaten varsa aynısını döner (idempotent).
    /// Havuz doluysa `None` — sessizce yanlış kimlik vermek jestleri bozar.
    pub fn acquire(&mut self, binding: &str) -> Option<u8> {
        if let Some(&id) = self.assigned.get(binding) {
            return Some(id);
        }
        let id = self.in_use.iter().position(|&used| !used)? as u8;
        self.in_use[id as usize] = true;
        self.assigned.insert(binding.to_string(), id);
        Some(id)
    }

    /// İşaretçiyi bırakır ve kimliği havuza döndürür.
    pub fn release(&mut self, binding: &str) -> Option<u8> {
        let id = self.assigned.remove(binding)?;
        self.in_use[id as usize] = false;
        Some(id)
    }

    pub fn get(&self, binding: &str) -> Option<u8> {
        self.assigned.get(binding).copied()
    }

    pub fn active_count(&self) -> usize {
        self.in_use.iter().filter(|&&u| u).count()
    }

    /// Tüm işaretçileri bırakır; her biri için Up eylemi üretir.
    /// Profil değişiminde veya acil durdurmada takılı parmak kalmasın diye.
    pub fn release_all(&mut self) -> Vec<TouchAction> {
        let mut acts: Vec<TouchAction> = self.assigned.values()
            .map(|&id| TouchAction::Up { id }).collect();
        acts.sort_by_key(|a| match a { TouchAction::Up { id } => *id, _ => 0 });
        self.assigned.clear();
        self.in_use = [false; MAX_POINTERS];
        acts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn norm_clamps_out_of_range() {
        assert_eq!(Norm::new(-0.5, 2.0), Norm { x: 0.0, y: 1.0 });
    }

    #[test]
    fn converts_to_pixels() {
        assert_eq!(Norm::new(0.5, 0.25).to_px(1920, 1080), (960, 270));
    }

    #[test]
    fn acquire_is_idempotent_per_binding() {
        let mut p = PointerPool::new();
        let a = p.acquire("fire").unwrap();
        let b = p.acquire("fire").unwrap();
        assert_eq!(a, b, "aynı bağlantı aynı işaretçiyi almalı");
        assert_eq!(p.active_count(), 1);
    }

    #[test]
    fn different_bindings_get_different_pointers() {
        let mut p = PointerPool::new();
        let a = p.acquire("move").unwrap();
        let b = p.acquire("fire").unwrap();
        assert_ne!(a, b);
        assert_eq!(p.active_count(), 2);
    }

    #[test]
    fn released_id_returns_to_pool() {
        let mut p = PointerPool::new();
        let a = p.acquire("x").unwrap();
        p.release("x");
        assert_eq!(p.active_count(), 0);
        let b = p.acquire("y").unwrap();
        assert_eq!(a, b, "boşalan kimlik tekrar kullanılabilmeli");
    }

    #[test]
    fn pool_exhaustion_returns_none_not_wrong_id() {
        let mut p = PointerPool::new();
        for i in 0..MAX_POINTERS { assert!(p.acquire(&format!("b{i}")).is_some()); }
        assert!(p.acquire("bir_fazla").is_none(), "havuz dolunca None dönmeli");
    }

    #[test]
    fn release_all_lifts_every_finger() {
        let mut p = PointerPool::new();
        p.acquire("a"); p.acquire("b"); p.acquire("c");
        let acts = p.release_all();
        assert_eq!(acts.len(), 3);
        assert!(acts.iter().all(|a| matches!(a, TouchAction::Up { .. })));
        assert_eq!(p.active_count(), 0);
    }

    #[test]
    fn releasing_unknown_binding_is_harmless() {
        let mut p = PointerPool::new();
        assert!(p.release("yok").is_none());
    }
}
