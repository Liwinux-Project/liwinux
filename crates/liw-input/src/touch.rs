//! Touch model: pointer id allocation and event generation.
//!
//! In Android multi-touch every finger must have a **stable pointer id**. If an
//! id is reused the app sees the finger "teleport" and gesture recognition
//! breaks. Allocation is therefore central and explicit.

use std::collections::HashMap;

/// Number of simultaneous pointers Android supports.
pub const MAX_POINTERS: usize = 10;

/// Resolution-independent coordinate (0.0..1.0), so profiles do not depend on
/// the display size.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Norm {
    pub x: f32,
    pub y: f32,
}

impl Norm {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x: x.clamp(0.0, 1.0), y: y.clamp(0.0, 1.0) }
    }

    /// A coordinate allowed to go OFF-SCREEN.
    ///
    /// Normally the 0..1 constraint is right: a touch belongs on screen. But
    /// nothing on Waydroid's touch pipe clamps (kernel evdev is not involved,
    /// `TouchInputMapper` does not clamp, and `InputDispatcher` does not
    /// re-pick the window on MOVE). Once a finger goes down inside the game,
    /// its later moves reach the same window even off-screen.
    ///
    /// This removes any need to recenter at the edge for FPS aim; see
    /// `docs/mouse-aim.md`.
    ///
    /// Only meaningful with backends that do not clamp. On the uinput backend
    /// libinput and KWin already squeeze coordinates into screen space.
    pub fn unclamped(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    /// Is the coordinate off-screen (for diagnostics and backend choice)?
    pub fn is_offscreen(self) -> bool {
        !(0.0..=1.0).contains(&self.x) || !(0.0..=1.0).contains(&self.y)
    }
    /// Converts to pixel coordinates.
    pub fn to_px(self, w: u32, h: u32) -> (i32, i32) {
        ((self.x * w as f32).round() as i32, (self.y * h as f32).round() as i32)
    }
}

/// A single touch action to inject.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TouchAction {
    Down { id: u8, at: Norm },
    Move { id: u8, at: Norm },
    Up { id: u8 },
}

/// The pointer owned by a binding.
///
/// The id is kept **for as long as the binding is active**; on release it goes
/// back to the pool.
#[derive(Debug, Default)]
pub struct PointerPool {
    /// binding name -> pointer id
    assigned: HashMap<String, u8>,
    /// which pointer ids are in use
    in_use: [bool; MAX_POINTERS],
}

impl PointerPool {
    pub fn new() -> Self { Self::default() }

    /// Allocates a pointer to a binding. Idempotent: returns the existing one
    /// if any. `None` if the pool is full — silently handing out a wrong id
    /// would break gestures.
    pub fn acquire(&mut self, binding: &str) -> Option<u8> {
        if let Some(&id) = self.assigned.get(binding) {
            return Some(id);
        }
        let id = self.in_use.iter().position(|&used| !used)? as u8;
        self.in_use[id as usize] = true;
        self.assigned.insert(binding.to_string(), id);
        Some(id)
    }

    /// Releases the pointer and returns its id to the pool.
    pub fn release(&mut self, binding: &str) -> Option<u8> {
        let id = self.assigned.remove(binding)?;
        self.in_use[id as usize] = false;
        Some(id)
    }

    /// Gives the binding a NEW pointer and releases the old one.
    ///
    /// Order matters: the new id is acquired WHILE THE OLD ONE IS STILL HELD,
    /// which guarantees a different id and therefore a different MT slot.
    ///
    /// Why it is needed: in the same slot, writing `tracking_id = -1` and then
    /// a new id within a single SYN_REPORT means the kernel only sees the
    /// final state — the lift is lost and the app sees the finger teleport.
    /// Across different slots, "A lifted, B appeared" is visible separately.
    ///
    /// If the pool is full it returns `None` and the OLD id is kept; leaving
    /// it half-done would strand a finger on screen.
    pub fn rotate(&mut self, binding: &str) -> Option<(u8, u8)> {
        let old = self.assigned.get(binding).copied()?;
        let fresh = self.in_use.iter().position(|&u| !u)? as u8;
        self.in_use[fresh as usize] = true;
        self.in_use[old as usize] = false;
        self.assigned.insert(binding.to_string(), fresh);
        Some((old, fresh))
    }

    /// Hands a pointer over to another binding.
    ///
    /// On handoff the second finger takes the first one's place; the id (and
    /// therefore the MT slot) does NOT change, only its owner. Changing the id
    /// would break the finger the game is tracking.
    pub fn rename(&mut self, from: &str, to: &str) -> Option<u8> {
        let id = self.assigned.remove(from)?;
        self.assigned.insert(to.to_string(), id);
        Some(id)
    }

    pub fn get(&self, binding: &str) -> Option<u8> {
        self.assigned.get(binding).copied()
    }

    pub fn active_count(&self) -> usize {
        self.in_use.iter().filter(|&&u| u).count()
    }

    /// Releases every pointer, producing an Up action for each, so no finger
    /// is left stuck on a profile switch or emergency stop.
    #[must_use = "if the produced UP actions are not dispatched, fingers stay down"]
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

    /// Unbounded aim depends on this behaviour: there must be NO clamping.
    #[test]
    fn unclamped_keeps_offscreen_coordinates() {
        let n = Norm::unclamped(2.5, -0.4);
        assert_eq!(n.x, 2.5);
        assert_eq!(n.y, -0.4);
        assert!(n.is_offscreen());
        assert!(!Norm::new(0.5, 0.5).is_offscreen());
    }

    /// Off-screen coordinates must survive pixel conversion too; the backend
    /// passes the value to Android as-is.
    #[test]
    fn offscreen_pixels_are_not_clamped() {
        assert_eq!(Norm::unclamped(1.5, -0.5).to_px(2560, 1440), (3840, -720));
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
        assert_eq!(a, b, "the same binding must get the same pointer");
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
        let _ = p.release("x");
        assert_eq!(p.active_count(), 0);
        let b = p.acquire("y").unwrap();
        assert_eq!(a, b, "a freed id must become reusable");
    }

    #[test]
    fn pool_exhaustion_returns_none_not_wrong_id() {
        let mut p = PointerPool::new();
        for i in 0..MAX_POINTERS { assert!(p.acquire(&format!("b{i}")).is_some()); }
        assert!(p.acquire("one_too_many").is_none(), "a full pool must return None");
    }

    #[test]
    fn release_all_lifts_every_finger() {
        let mut p = PointerPool::new();
        let _ = p.acquire("a"); let _ = p.acquire("b"); let _ = p.acquire("c");
        let acts = p.release_all();
        assert_eq!(acts.len(), 3);
        assert!(acts.iter().all(|a| matches!(a, TouchAction::Up { .. })));
        assert_eq!(p.active_count(), 0);
    }

    /// Rotation must yield a DIFFERENT id: in the same slot a lift and a
    /// press collapse into one SYN and the app sees a teleport.
    #[test]
    fn rotate_yields_a_different_pointer() {
        let mut p = PointerPool::new();
        let a = p.acquire("aim").unwrap();
        let (old, new) = p.rotate("aim").unwrap();
        assert_eq!(old, a);
        assert_ne!(new, a, "the new id must differ");
        assert_eq!(p.get("aim"), Some(new));
        assert_eq!(p.active_count(), 1, "the old id must be released");
    }

    #[test]
    fn rotate_on_unknown_binding_is_none() {
        let mut p = PointerPool::new();
        assert!(p.rotate("missing").is_none());
    }

    /// Rotating a full pool must keep the OLD id — leaving it half-done
    /// strands a finger on screen.
    #[test]
    fn rotate_when_full_keeps_the_old_pointer() {
        let mut p = PointerPool::new();
        for i in 0..MAX_POINTERS { p.acquire(&format!("b{i}")); }
        let before = p.get("b0").unwrap();
        assert!(p.rotate("b0").is_none());
        assert_eq!(p.get("b0"), Some(before), "the old id must be kept");
        assert_eq!(p.active_count(), MAX_POINTERS);
    }

    #[test]
    fn releasing_unknown_binding_is_harmless() {
        let mut p = PointerPool::new();
        assert!(p.release("missing").is_none());
    }
}
