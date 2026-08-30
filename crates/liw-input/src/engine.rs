//! Mapping engine: host input -> touch actions.
//!
//! Kept pure and synchronous: no I/O inside, the time source comes from
//! outside. That makes all behaviour verifiable with unit tests — most "feel"
//! bugs in a keymapper hide in the state machine and
//! elle denemekle yakalanamaz.

use crate::profile::{Binding, Easing, Profile, Trigger};
use crate::touch::{Norm, PointerPool, TouchAction, MAX_POINTERS};
use std::collections::HashSet;

/// A raw input event arriving at the engine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputEvent {
    Press(Trigger2),
    Release(Trigger2),
    /// Relative mouse motion (pixels).
    MouseMove { dx: f32, dy: f32 },
}

/// The Copy-able twin of `Trigger` (for internal engine use).
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

/// A swipe gesture in progress.
///
/// A swipe is "fire and forget": pressing the key plays the whole gesture, and
/// how long the key is held does not matter. Games recognise a swipe from its
/// intermediate movements; a single jump does not count as a gesture.
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
    /// Last intermediate step sent; to avoid resending the same position.
    last_step: u32,
    /// Last position SENT. On front-loaded curves the final steps move only a
    /// few pixels; these carry no information and give the impression the finger
    /// is stuck, which can make the gesture look like a hold.
    last_sent: Norm,
}

/// If two consecutive steps are closer than this, no intermediate step is sent.
/// 0.002 normalized ~ 5 pixels at 2540 pixels wide.
const MIN_STEP_DELTA: f32 = 0.002;

/// Pool key of the second finger used for handoff.
fn second_key(name: &str) -> String { format!("{name}\u{0}2") }

/// One step of unbounded aim: NO clamping, only the safety box.
///
/// The box exists for overflow protection, not feel. The position is f32 while
/// the wire value is i32; leaving it unbounded would erode precision in a long
/// enough session. If aim leans on the box it sticks there, but that is
/// silently repaired by `idle_recenter` the moment the mouse stops.
fn free_step(pos: Norm, origin: Norm, safety_span: f32, dx: f32, dy: f32) -> Norm {
    let lim = safety_span.max(1.0);
    Norm::unclamped(
        (pos.x + dx).clamp(origin.x - lim, origin.x + lim),
        (pos.y + dy).clamp(origin.y - lim, origin.y + lim),
    )
}

/// How many intermediate steps a swipe is split into. Too few and the gesture
/// is not recognised, too many and it produces needless events. 12 steps over
/// 80ms is ~7ms apart — enough even at 144Hz.
const SWIPE_STEPS: u32 = 12;

pub struct Engine {
    profile: Profile,
    pool: PointerPool,
    held: HashSet<TriggerKind>,
    swipes: Vec<ActiveSwipe>,
    now_ms: u64,
    /// Screen aspect ratio (width / height).
    ///
    /// Normalized coordinates are scaled per axis: at 2560x1440, 0.085 is 218
    /// pixels horizontally and 122 vertically. Uncorrected, the joystick
    /// becomes an ellipse — A/D reach 1.78x further than W/S.
    aspect: f32,
    /// Mouse motion deferred to the next frame after a recentre.
    ///
    /// If Down and Move go in the SAME SYN_REPORT the game sees one frame and
    /// the finger appears directly at the target; with no previous position for
    /// the new touch, no delta can be computed and the turn is lost.
    aim_pending: Option<(f32, f32)>,
    /// Time of the last mouse motion; for idle recentring.
    aim_last_move_ms: u64,
    /// When the press after a lift is due (ms) — the delayed reset.
    aim_down_at: Option<u64>,
    /// Accumulated mouse motion; applied once per frame.
    ///
    /// The mouse samples at 1000 Hz but real touchscreens report at 60-240 Hz.
    /// Sending a Move per mouse event floods Android's input pipeline: events
    /// get batched, the Up/Down pair loses its order inside the flood and the
    /// turn sticks at the edge. Accumulating and sending one move per frame is
    /// what real hardware does.
    /// what real hardware does.
    aim_accum: (f32, f32),
    /// Position of the pre-placed second finger used for handoff.
    ///
    /// It goes down before the first reaches the edge and moves TOGETHER with
    /// it. At the moment of handover a moving finger remains on screen and the
    /// turn is not cut.
    aim_second: Option<Norm>,
    /// Deferred move for a joystick that goes down at the centre on first press.
    ///
    /// A real player presses the middle of the joystick and drags. Putting the
    /// finger straight on the edge fails to start movement in some games.
    joystick_pending: Option<String>,
    /// Aim modunda mevcut parmak konumu.
    aim_pos: Option<Norm>,
    /// Is the engine enabled? While off it produces no events, but stuck
    /// fingers are released — otherwise a finger hangs in the game.
    enabled: bool,
    /// Can the backend carry OFF-SCREEN coordinates?
    ///
    /// `unbounded` in the profile is not enough on its own: on the uinput path
    /// libinput and KWin clamp the coordinate to the screen, the finger sticks
    /// at the edge and aim dies completely. The decision therefore belongs to
    /// the backend and defaults to OFF — when unknown, bounded mode works
    /// on every path.
    offscreen_ok: bool,
}

impl Engine {
    pub fn new(profile: Profile) -> Self {
        Self {
            profile, pool: PointerPool::new(), held: HashSet::new(),
            swipes: Vec::new(), now_ms: 0,
            aspect: 16.0 / 9.0,
            aim_pending: None, joystick_pending: None,
            aim_last_move_ms: 0,
            aim_accum: (0.0, 0.0),
            aim_down_at: None,
            aim_second: None,
            aim_pos: None, enabled: true,
            offscreen_ok: false,
        }
    }

    pub fn profile(&self) -> &Profile { &self.profile }

    /// Sets the screen aspect ratio. Whether the joystick circle is really a
    /// circle depends on this.
    pub fn set_aspect(&mut self, w: u32, h: u32) {
        if w > 0 && h > 0 { self.aspect = w as f32 / h as f32; }
    }
    pub fn is_enabled(&self) -> bool { self.enabled }

    /// Declares that the backend can carry off-screen coordinates.
    ///
    /// True only for the backend writing directly to Waydroid's touch pipe;
    /// details in `docs/mouse-aim.md`. Even with `unbounded = true` in the
    /// profile, unbounded aim does not engage until this is set.
    pub fn set_offscreen_ok(&mut self, ok: bool) { self.offscreen_ok = ok; }

    /// Is unbounded aim actually active right now (diagnostics and tests)?
    pub fn aim_is_unbounded(&self) -> bool {
        self.offscreen_ok && self.profile.bindings.values().any(|b|
            matches!(b, Binding::Aim { unbounded: true, .. }))
    }

    /// Enables/disables the engine. On disable it lifts every finger.
    #[must_use = "if the UP actions produced while disabling are not dispatched,                   fingers stay down"]
    pub fn set_enabled(&mut self, on: bool) -> Vec<TouchAction> {
        if self.enabled == on { return Vec::new(); }
        self.enabled = on;
        if !on {
            self.held.clear();
            self.aim_pos = None;
            self.aim_pending = None;
            self.aim_accum = (0.0, 0.0);
            self.aim_down_at = None;
            self.aim_second = None;
            self.joystick_pending = None;
            self.swipes.clear();
            self.pool.release_all()
        } else { Vec::new() }
    }

    /// Finds which binding uses a given trigger.
    fn owner(&self, t: TriggerKind) -> Option<(&str, &Binding)> {
        self.profile.bindings.iter()
            .find(|(_, b)| b.triggers().iter().any(|x| TriggerKind::from(x) == t))
            .map(|(n, b)| (n.as_str(), b))
    }

    /// Advances time and produces intermediate steps for ongoing gestures.
    ///
    /// Time comes from OUTSIDE so the engine stays pure; gesture timing can
    /// then be tested without a real clock.
    #[must_use = "the produced touch actions MUST be dispatched to the backend;                   dropping them leaves a finger stuck on screen"]
    pub fn tick(&mut self, now_ms: u64) -> Vec<TouchAction> {
        self.now_ms = now_ms;
        if !self.enabled { return Vec::new(); }
        let mut acts = Vec::new();

        // Deferred joystick direction: applied on the frame AFTER the Down.
        if let Some(name) = self.joystick_pending.take() {
            acts.extend(self.recompute_joystick(&name));
        }
        // Leak repair FIRST: if the pool fills, aim cannot get a pointer and
        // the mouse dies completely.
        acts.extend(self.reconcile_pointers());

        // Delayed press: gives Android time to genuinely finish the touch after
        // a lift. Sending them in the same frame makes the game see a teleport.
        // a lift. Sending them in the same frame makes the game see a teleport.
        if let Some(t) = self.aim_down_at {
            if now_ms >= t {
                self.aim_down_at = None;
                if let Some((name, Binding::Aim { origin, .. })) =
                    self.profile.bindings.iter()
                        .find(|(_, b)| matches!(b, Binding::Aim { .. }))
                        .map(|(n, b)| (n.clone(), b.clone()))
                {
                    match self.pool.acquire(&name) {
                        Some(id) => {
                            self.aim_pos = Some(origin);
                            acts.push(TouchAction::Down { id, at: origin });
                        }
                        None => tracing::error!(
                            in_use = self.pool.active_count(),
                            "could not put the aim finger down — pool full"),
                    }
                }
            }
            // In BOTH cases we skip the aim side and leave — but the pending
            // work of joysticks and swipes must still run, otherwise movement
            // stops while walking whenever aim resets.
            //
            // Leaving is MANDATORY even if the press happened on this tick. If
            // we continued, the accumulated mouse motion would emit a `Move` in
            // the same batch; both would land in one SYN_REPORT, the touch
            // would appear directly at the target and the game could not
            // compute a delta — that turn would be lost. On fast turns this fed
            // itself and produced dead zones lasting seconds. The accumulated
            // motion is kept for the next frame.
            return self.tick_gestures(now_ms, acts);
        }

        // ORDER MATTERS: the deferred motion is applied FIRST.
        //
        // The other way round, the deferred motion produced by a reseat is
        // consumed within the same tick; Down and Move merge into one
        // SYN_REPORT, the last position wins in the same slot and the game sees
        // a teleport again. While a deferred move exists the accumulated one
        // waits for the next frame — it is not lost.
        if let Some((dx, dy)) = self.aim_pending.take() {
            // The deferred motion is merged with whatever ACCUMULATED meanwhile:
            // both become one Move in the same slot. The Down went out on the
            // previous tick, so there is no collision.
            //
            // Applying them separately stretched the gap to two ticks: one for
            // the deferred motion and one for what accumulated meanwhile.
            let (ax, ay) = std::mem::take(&mut self.aim_accum);
            acts.extend(self.apply_aim_delta(dx + ax, dy + ay));
        } else if self.aim_accum != (0.0, 0.0) {
            // Accumulated mouse motion: ONE Move per frame.
            let (ax, ay) = std::mem::take(&mut self.aim_accum);
            acts.extend(self.on_mouse(ax, ay));
        } else {
            // IDLE recentring: pull the finger to the centre when the mouse stops.
            //
            // Recentring costs one frame of turning. Done during motion it is
            // felt as a pause; done at a standstill it is never noticed. That
            // also reduces how often recentring is needed on fast turns.
            // also reduces how often recentring is needed on fast turns.
            acts.extend(self.idle_recenter());
        }

        let mut finished: Vec<String> = Vec::new();

        for sw in &mut self.swipes {
            let elapsed = now_ms.saturating_sub(sw.start_ms);
            let step = if sw.duration_ms == 0 { SWIPE_STEPS } else {
                ((elapsed * SWIPE_STEPS as u64) / sw.duration_ms).min(SWIPE_STEPS as u64) as u32
            };
            if step == sw.last_step { continue; }
            sw.last_step = step;
            // The curve converts TIME progress into a DISTANCE ratio. The step
            // count does not change; what changes is how far each step travels.
            let t = sw.easing.apply(step as f32 / SWIPE_STEPS as f32);
            let at = Norm::new(
                sw.from.x + (sw.to.x - sw.from.x) * t,
                sw.from.y + (sw.to.y - sw.from.y) * t,
            );
            // The last step is ALWAYS sent: the gesture must reach its target.
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

    /// Movement must NOT STOP when aim resets while walking.
    ///
    /// Real bug: while waiting for the delayed press, tick returned early and
    /// the joystick's pending direction was never applied.
    /// Finds and releases orphaned pointers.
    ///
    /// `Tap`/`Toggle` bindings take a pointer on press and give it back on
    /// release. If the release event IS LOST (which can happen during a game
    /// mode switch, a focus change, or while grabbing/ungrabbing) the pointer
    /// is held forever. After a few leaks the pool fills, aim cannot get a
    /// pointer — and the mouse dies COMPLETELY. This actually happened.
    ///
    /// So on every tick the held pointers are compared against the held keys;
    /// any without a match is released.
    fn reconcile_pointers(&mut self) -> Vec<TouchAction> {
        let mut acts = Vec::new();
        let names: Vec<String> = self.profile.bindings.keys().cloned().collect();
        for name in names {
            // Aim, joysticks and running swipes manage their own lifetime.
            let Some(b) = self.profile.bindings.get(&name) else { continue };
            let expects_hold = match b {
                Binding::Tap { trigger, .. } | Binding::Toggle { trigger, .. } =>
                    Some(TriggerKind::from(trigger)),
                _ => None,
            };
            let Some(t) = expects_hold else { continue };
            if self.pool.get(&name).is_some() && !self.held.contains(&t) {
                if let Some(id) = self.pool.release(&name) {
                    tracing::warn!(binding = %name,
                        "released an orphaned pointer (lost key release event)");
                    acts.push(TouchAction::Up { id });
                }
            }
        }
        acts
    }

    /// Pending work other than aim (joystick direction, swipe steps).
    fn tick_gestures(&mut self, now_ms: u64, mut acts: Vec<TouchAction>) -> Vec<TouchAction> {
        if let Some(name) = self.joystick_pending.take() {
            acts.extend(self.recompute_joystick(&name));
        }
        let mut finished: Vec<String> = Vec::new();
        for sw in &mut self.swipes {
            let elapsed = now_ms.saturating_sub(sw.start_ms);
            let step = if sw.duration_ms == 0 { SWIPE_STEPS } else {
                ((elapsed * SWIPE_STEPS as u64) / sw.duration_ms)
                    .min(SWIPE_STEPS as u64) as u32
            };
            if step == sw.last_step { continue; }
            sw.last_step = step;
            let t = sw.easing.apply(step as f32 / SWIPE_STEPS as f32);
            let at = Norm::new(
                sw.from.x + (sw.to.x - sw.from.x) * t,
                sw.from.y + (sw.to.y - sw.from.y) * t,
            );
            let is_final = step >= SWIPE_STEPS;
            let dx = at.x - sw.last_sent.x;
            let dy = at.y - sw.last_sent.y;
            if is_final || (dx * dx + dy * dy).sqrt() >= MIN_STEP_DELTA {
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

    /// Is a gesture in progress? If so the caller must call `tick` frequently.
    pub fn has_pending(&self) -> bool {
        // A tick is also needed while aim_pos exists: idle recentring runs there.
        // aim_down_at is MANDATORY: after a reset aim_pos is None and the
        // accumulator may be empty. Not counting it closes the caller's clock
        // arm, tick is never called and the press NEVER happens — aim dies
        // completely.
        !self.swipes.is_empty() || self.aim_pending.is_some()
            || self.joystick_pending.is_some() || self.aim_pos.is_some()
            || self.aim_down_at.is_some()
            || self.aim_accum != (0.0, 0.0)
    }

    /// Number of gestures in progress (for tests and diagnostics).
    pub fn swipe_count(&self) -> usize { self.swipes.len() }

    /// The last time the engine knows about (tests/diagnostics).
    pub fn now_ms(&self) -> u64 { self.now_ms }

    /// Number of pointers in use (diagnostics).
    pub fn active_pointers(&self) -> usize { self.pool.active_count() }

    /// Current position of the aim finger (diagnostics and tests).
    /// May be off-screen in unbounded mode.
    pub fn aim_position(&self) -> Option<Norm> { self.aim_pos }

    #[cfg(test)]
    fn forget_held_for_test(&mut self) { self.held.clear(); }

    /// Is the second handoff finger down (tests/diagnostics)?
    pub fn aim_has_second(&self) -> bool { self.aim_second.is_some() }

    #[must_use = "the produced touch actions MUST be dispatched to the backend"]
    pub fn handle(&mut self, ev: InputEvent) -> Vec<TouchAction> {
        if !self.enabled { return Vec::new(); }
        match ev {
            InputEvent::Press(t) => self.on_press(t),
            InputEvent::Release(t) => self.on_release(t),
            InputEvent::MouseMove { dx, dy } => {
                // Applied on tick; here it only accumulates.
                self.aim_accum.0 += dx;
                self.aim_accum.1 += dy;
                self.aim_last_move_ms = self.now_ms;
                Vec::new()
            }
        }
    }

    fn on_press(&mut self, t: TriggerKind) -> Vec<TouchAction> {
        // Swallow key auto-repeat: otherwise Down events pile up.
        if !self.held.insert(t) { return Vec::new(); }
        let Some((name, binding)) = self.owner(t) else { return Vec::new() };
        let (name, binding) = (name.to_string(), binding.clone());
        match binding {
            Binding::Tap { at, .. } | Binding::Toggle { at, .. } => {
                match self.pool.acquire(&name) {
                    Some(id) => vec![TouchAction::Down { id, at }],
                    None => {
                        // Swallowing silently produces a "the key sometimes
                        // does not work" bug and the user cannot find out why.
                        tracing::warn!(binding = %name, limit = MAX_POINTERS,
                            "pointer pool full — this touch was skipped");
                        Vec::new()
                    }
                }
            }
            Binding::Joystick { .. } => self.recompute_joystick(&name),
            Binding::Aim { origin, .. } => {
                // Cancel a pending delayed press: we are going down here
                // already, and both running would leave two aim fingers on
                // screen.
                self.aim_down_at = None;
                self.aim_pos = Some(origin);
                match self.pool.acquire(&name) {
                    Some(id) => vec![TouchAction::Down { id, at: origin }],
                    None => Vec::new(),
                }
            }
            Binding::Swipe { from, to, duration_ms, ref group, easing, .. } => {
                // Do not start a new swipe if the same one is already playing.
                if self.swipes.iter().any(|s| s.binding == name) { return Vec::new(); }

                // CANCEL running gestures in the same group: lift the finger
                // where it is, do not complete the gesture. The user changed
                // their mind; a half-finished swipe usually stays below the
                // game's threshold and produces no wrong movement.
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
                // With a direction still held the finger does not lift, it only
                // repositions.
                let acts = self.recompute_joystick(&name);
                if acts.is_empty() {
                    self.pool.release(&name).map(|id| vec![TouchAction::Up { id }])
                        .unwrap_or_default()
                } else { acts }
            }
            Binding::Aim { .. } => {
                // Cancel a pending press too: a finger going down AFTER aim is
                // released leaves an orphaned touch hanging in the game.
                self.aim_down_at = None;
                self.aim_pos = None;
                self.aim_accum = (0.0, 0.0);
                self.pool.release(&name).map(|id| vec![TouchAction::Up { id }])
                    .unwrap_or_default()
            }
            // A swipe is fire-and-forget: releasing the key does not affect it.
            // Cutting it short reads as a "wrong swipe" in games.
            Binding::Swipe { .. } => Vec::new(),
            _ => self.pool.release(&name).map(|id| vec![TouchAction::Up { id }])
                    .unwrap_or_default(),
        }
    }

    /// Positions the joystick finger according to the held directions.
    /// Returns empty if no direction is held (the caller lifts the finger).
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

        // Normalize so diagonals are not faster — otherwise diagonal movement
        // is 41% faster and the player feels it as "sliding".
        let len = (dx * dx + dy * dy).sqrt();
        // The vertical radius is scaled by the aspect ratio, or the circle
        // becomes an ellipse.
        let at = Norm::new(
            center.x + dx / len * radius,
            center.y + dy / len * radius * self.aspect,
        );

        let first = self.pool.get(name).is_none();
        match self.pool.acquire(name) {
            Some(id) if first => {
                // Go down at the CENTRE on first press; the direction is
                // applied on the next frame. Putting the finger straight on
                // the edge fails to start movement in some games — a real
                // player presses the middle and drags.
                self.joystick_pending = Some(name.to_string());
                vec![TouchAction::Down { id, at: center }]
            }
            Some(id) => vec![TouchAction::Move { id, at }],
            None => Vec::new(),
        }
    }

    fn on_mouse(&mut self, dx: f32, dy: f32) -> Vec<TouchAction> {
        let Some((name, Binding::Aim {
            toggle, origin, sensitivity, deadzone, recenter_margin, handoff,
            nonlinear, reset_delay_ms, unbounded, safety_span,
        })) = self.profile.bindings.iter()
                .find(|(_, b)| matches!(b, Binding::Aim { .. }))
                .map(|(n, b)| (n.clone(), b.clone()))
        else { return Vec::new() };

        if (dx * dx + dy * dy).sqrt() < deadzone { return Vec::new(); }

        // Unbounded mode: the ENTIRE reset mechanism below is skipped.
        // The edge margin, handoff, non-linear scaling and delayed press all
        // existed to hide a single constraint — and on this path that
        // constraint does not exist (`docs/mouse-aim.md`).
        if unbounded && self.offscreen_ok {
            if toggle.is_some() && self.aim_pos.is_none() { return Vec::new(); }
            return self.on_mouse_free(&name, origin, sensitivity, safety_span, dx, dy);
        }

        // Without a toggle, aim is ALWAYS active: put the finger down on the
        // first mouse movement. In an FPS, looking must not require holding a key.
        let mut acts = Vec::new();
        if self.aim_pos.is_none() {
            if toggle.is_some() { return acts; }
            let Some(id) = self.pool.acquire(&name) else {
                tracing::error!(
                    in_use = self.pool.active_count(), limit = MAX_POINTERS,
                    "aim could not get a pointer — pool full, the mouse will not work");
                return acts;
            };
            self.aim_pos = Some(origin);
            acts.push(TouchAction::Down { id, at: origin });
        }
        let pos = self.aim_pos.expect("established above");
        let Some(id) = self.pool.get(&name) else { return acts };

        let m = recenter_margin.clamp(0.01, 0.45);
        let lo = m;
        let hi = 1.0 - m;
        let span = hi - lo;

        // Non-linear scaling: sensitivity falls as the finger moves away from
        // the centre, it approaches the edge asymptotically and in practice
        // never arrives. The need to reseat never arises in the first place.
        let scale = if nonlinear {
            let d = ((pos.x - origin.x).powi(2)
                   + ((pos.y - origin.y) / self.aspect.max(0.01)).powi(2)).sqrt();
            let min_d = span / 20.0;
            if d > min_d { (min_d / d).sqrt() } else { 1.0 }
        } else { 1.0 };

        let nx = pos.x + dx * sensitivity * scale;
        let ny = pos.y + dy * sensitivity * scale;

        if handoff {
            // --- Handoff path: NEVER lift while moving ---
            //
            // The second finger goes down at the centre before the first is
            // lifted, so for a moment both are down and moving together. The
            // first is only released once it leans on the edge; by then the
            // second is already tracking, so the game never loses the aim.
            const PREPARE: f32 = 0.62;

            let far = ((pos.x - origin.x).powi(2)
                     + (pos.y - origin.y).powi(2)).sqrt() > span * 0.5 * PREPARE;

            // Preparation: put the second finger down (nobody lifts yet).
            if far && self.aim_second.is_none() {
                // NOT the centre: the farthest point opposite the direction of
                // travel. The centre gives half the remaining distance;
                // the far end gives all of it and halves the handoff rate.
                let seat = self.reseat_point(origin, m, dx, dy);
                if let Some(id2) = self.pool.acquire(&second_key(&name)) {
                    acts.push(TouchAction::Down { id: id2, at: seat });
                    self.aim_second = Some(seat);
                }
            }

            let out = nx < lo || nx > hi || ny < lo || ny > hi;

            // If a second finger exists, move it by the SAME delta.
            if let Some(sp) = self.aim_second {
                let s_next = Norm::new(
                    (sp.x + dx * sensitivity).clamp(lo, hi),
                    (sp.y + dy * sensitivity).clamp(lo, hi),
                );
                if let Some(id2) = self.pool.get(&second_key(&name)) {
                    acts.push(TouchAction::Move { id: id2, at: s_next });
                }
                self.aim_second = Some(s_next);
            }

            if out {
                // Handover: release the first, the second is now the first.
                // The second already moved this frame — no gap.
                if let Some(sp) = self.aim_second.take() {
                    acts.push(TouchAction::Up { id });
                    self.pool.release(&name);
                    self.pool.rename(&second_key(&name), &name);
                    self.aim_pos = Some(sp);
                } else {
                    // The second is not ready (a sudden jump): fall back to
                    // the simple path.
                    if let Some((old_id, new_id)) = self.pool.rotate(&name) {
                        acts.push(TouchAction::Up { id: old_id });
                        acts.push(TouchAction::Down { id: new_id, at: origin });
                        self.aim_pos = Some(origin);
                        let (px, py) = self.aim_pending.unwrap_or((0.0, 0.0));
                        self.aim_pending = Some((px + dx, py + dy));
                    }
                }
                return acts;
            }

            let next = Norm::new(nx, ny);
            self.aim_pos = Some(next);
            acts.push(TouchAction::Move { id, at: next });
            return acts;
        }

        if nx < lo || nx > hi || ny < lo || ny > hi {
            // Simple path: a different slot is mandatory, otherwise the lift
            // and the press merge into one SYN_REPORT and the game sees a
            // teleport. Send the APPLICABLE part first: move the finger to the
            // limit. Dropping it loses the turn; the part up to the limit is valid.
            let edge = Norm::new(nx.clamp(lo, hi), ny.clamp(lo, hi));
            if (edge.x - pos.x).abs() > f32::EPSILON
                || (edge.y - pos.y).abs() > f32::EPSILON
            {
                acts.push(TouchAction::Move { id, at: edge });
            }

            // Lift NOW, press DELAYED.
            //
            // Sending them in the same frame gives Android no chance to treat
            // the lift as a genuine touch end; the game sees a teleport.
            // XtMapper inserts a delay too.
            acts.push(TouchAction::Up { id });
            self.pool.release(&name);
            self.aim_pos = None;
            self.aim_down_at = Some(self.now_ms + reset_delay_ms.max(1) as u64);

            // The OVERFLOWING part is DISCARDED, not added to the accumulator.
            //
            // Adding it would exceed the limit again the moment the finger goes
            // down and immediately trigger another reset — it loops and the
            // user says "it does not detect my movement at all". The overflow
            // temsil edilemez.
            return acts;
        }

        let next = Norm::new(nx, ny);
        self.aim_pos = Some(next);
        acts.push(TouchAction::Move { id, at: next });
        acts
    }

    /// Unbounded aim: the finger goes down once, then only moves.
    ///
    /// No lift, no recentring, no handoff. All three symptoms (no detection at
    /// all, dead zones lasting seconds, aim drift) came from those; removing
    /// them removes the need to mitigate anything.
    ///
    /// Sensitivity is CONSTANT: `nonlinear` is deliberately not applied.
    /// Sensitivity varying with the finger's invisible position makes muscle
    /// memory impossible in an FPS, and the user experienced it as "aim drifts".
    fn on_mouse_free(
        &mut self, name: &str, origin: Norm, sensitivity: f32,
        safety_span: f32, dx: f32, dy: f32,
    ) -> Vec<TouchAction> {
        let mut acts = Vec::new();

        if self.aim_pos.is_none() {
            // Do not interfere while a safety reset is in flight: it would
            // leave two aim fingers.
            if self.aim_down_at.is_some() { return acts; }
            let Some(id) = self.pool.acquire(name) else {
                tracing::error!(
                    in_use = self.pool.active_count(), limit = MAX_POINTERS,
                    "aim could not get a pointer — pool full, the mouse will not work");
                return acts;
            };
            self.aim_pos = Some(origin);
            acts.push(TouchAction::Down { id, at: origin });
            // The press must be ALONE in its own frame; motion is deferred to
            // the next. In the same SYN_REPORT the game would see the touch
            // directly at the target and could not compute a delta.
            let (px, py) = self.aim_pending.unwrap_or((0.0, 0.0));
            self.aim_pending = Some((px + dx, py + dy));
            return acts;
        }

        let (Some(pos), Some(id)) = (self.aim_pos, self.pool.get(name))
        else { return acts };
        let next = free_step(pos, origin, safety_span, dx * sensitivity, dy * sensitivity);
        self.aim_pos = Some(next);
        acts.push(TouchAction::Move { id, at: next });
        acts
    }

    /// Silently recentres if the mouse stopped and the finger drifted.
    ///
    /// Returning to the centre while idle is right: the direction of the next
    /// movement is unknown and the centre leaves equal room in every direction.
    fn idle_recenter(&mut self) -> Vec<TouchAction> {
    /// In a real game there are many micro-pauses: aiming, turning a corner,
    /// firing all leave 30-40 ms gaps. Lowering the threshold moves most resets
    /// into those gaps and reduces the need to reset during motion.
    /// into those gaps and reduces the need to reset during motion.
        const IDLE_MS: u64 = 35;
        /// Drifting this far from the centre makes a reset worthwhile.
        const FAR: f32 = 0.15;

        let Some((name, Binding::Aim {
            origin, recenter_margin, reset_delay_ms, unbounded, safety_span, ..
        })) = self.profile.bindings.iter()
                .find(|(_, b)| matches!(b, Binding::Aim { .. }))
                .map(|(n, b)| (n.clone(), b.clone()))
        else { return Vec::new() };
        let Some(pos) = self.aim_pos else { return Vec::new() };
        if self.now_ms.saturating_sub(self.aim_last_move_ms) < IDLE_MS {
            return Vec::new();
        }
        let d = ((pos.x - origin.x).powi(2) + (pos.y - origin.y).powi(2)).sqrt();
        let threshold = if unbounded && self.offscreen_ok {
            // In unbounded mode the ONLY reason to recentre is numeric range.
            // The threshold is half the safety box: never triggered in normal
            // play, and when it is, the mouse has already stopped.
            safety_span.max(1.0) * 0.5
        } else {
            let m = recenter_margin.clamp(0.01, 0.45);
            (1.0 - 2.0 * m) * FAR
        };
        if d < threshold { return Vec::new(); }

        // Lift NOW, press DELAYED and in ITS OWN frame.
        //
        // These used to be returned in one batch; because the backend turns
        // that into one SYN_REPORT, Android could not see the touch end and the
        // game saw a teleport — the very bug warned about repeatedly elsewhere
        // in this file.
        let Some(id) = self.pool.release(&name) else { return Vec::new() };
        self.aim_pos = None;
        // Discard the accumulated motion: the mouse has stopped so it carries
        // no information, but it could produce a fake jump after the press.
        self.aim_accum = (0.0, 0.0);
        self.aim_last_move_ms = self.now_ms;
        self.aim_down_at = Some(self.now_ms + reset_delay_ms.max(1) as u64);
        vec![TouchAction::Up { id }]
    }

    /// Picks the reseat point according to the DIRECTION OF TRAVEL.
    ///
    /// Returning to the centre spends half the distance. If the player is
    /// turning right, putting the finger on the LEFT edge uses the full width
    /// and roughly halves the recentring rate.
    ///
    /// This matters because every recentre has two costs and neither can be
    /// removed: (1) a one-frame delta gap, because the new touch has no previous
    /// position, and (2) Android's touch slop — an app swallows a certain
    /// distance before counting a drag. The only remedy is reducing the NUMBER
    /// of these events.
    fn reseat_point(&self, origin: Norm, m: f32, dx: f32, dy: f32) -> Norm {
        let len = (dx * dx + dy * dy).sqrt();
        if len < f32::EPSILON { return origin; }
        let (ux, uy) = (dx / len, dy / len);
        let lo = m;
        let hi = 1.0 - m;
        // Go as far as we can OPPOSITE the direction of travel.
        let t_axis = |p: f32, u: f32| -> f32 {
            if u.abs() < 1e-6 { f32::INFINITY }
            else if u > 0.0 { (p - lo) / u }   // opposite direction: decreasing
            else { (hi - p) / -u }
        };
        let t = t_axis(origin.x, ux).min(t_axis(origin.y, uy)).max(0.0);
        if !t.is_finite() { return origin; }

        // Sitting EXACTLY on the edge leaves no room in the opposite direction
        // and the slightest backward movement triggers another reseat. The user
        // experiences this as "it does not detect my movements" — back-to-back
        // reseats, each swallowing a frame.
        //
        // Use 80%: it keeps most of the remaining distance while leaving about
        // a 20% buffer behind.
        const USE: f32 = 0.80;
        Norm::new(origin.x - ux * t * USE, origin.y - uy * t * USE)
    }

    /// Applies the deferred mouse motion (the first frame after a press).
    fn apply_aim_delta(&mut self, dx: f32, dy: f32) -> Vec<TouchAction> {
        let Some((name, Binding::Aim {
            origin, sensitivity, recenter_margin, unbounded, safety_span, ..
        })) = self.profile.bindings.iter()
                .find(|(_, b)| matches!(b, Binding::Aim { .. }))
                .map(|(n, b)| (n.clone(), b.clone()))
        else { return Vec::new() };
        let (Some(pos), Some(id)) = (self.aim_pos, self.pool.get(&name))
        else { return Vec::new() };
        let next = if unbounded && self.offscreen_ok {
            free_step(pos, origin, safety_span, dx * sensitivity, dy * sensitivity)
        } else {
            let m = recenter_margin.clamp(0.01, 0.45);
            Norm::new(
                (pos.x + dx * sensitivity).clamp(m, 1.0 - m),
                (pos.y + dy * sensitivity).clamp(m, 1.0 - m),
            )
        };
        self.aim_pos = Some(next);
        vec![TouchAction::Move { id, at: next }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::Binding;
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
        b.insert("aim".into(), Binding::Aim {
            toggle: Some(Trigger::MouseRight),
            origin: Norm::new(0.5, 0.5),
            sensitivity: 0.001,
            deadzone: 0.5,
            recenter_margin: 0.12,
            handoff: false, nonlinear: false, reset_delay_ms: 0,
            // These tests exercise the BOUNDED path.
            unbounded: false, safety_span: 32.0,
        });
        Profile { name: "t".into(), package: "p".into(), bindings: b }
    }

    fn key(k: u16) -> TriggerKind { TriggerKind::Key(k) }

    /// Mouse motion is now applied per frame: the event accumulates and the
    /// tick applies it. Tests must mimic the real flow.
    fn mouse(e: &mut Engine, dx: f32, dy: f32) -> Vec<TouchAction> {
        let _ = e.handle(InputEvent::MouseMove { dx, dy });
        e.tick(e.now_ms() + 1)
    }

    #[test]
    fn tap_presses_and_lifts() {
        let mut e = Engine::new(joystick_profile());
        let down = e.handle(InputEvent::Press(key(SPACE)));
        assert!(matches!(down[..], [TouchAction::Down { .. }]));
        let up = e.handle(InputEvent::Release(key(SPACE)));
        assert!(matches!(up[..], [TouchAction::Up { .. }]));
    }

    /// Keyboard auto-repeat must not pile up Down events.
    #[test]
    fn key_autorepeat_is_swallowed() {
        let mut e = Engine::new(joystick_profile());
        assert_eq!(e.handle(InputEvent::Press(key(SPACE))).len(), 1);
        assert!(e.handle(InputEvent::Press(key(SPACE))).is_empty(), "a repeat must not produce a Down");
        assert!(e.handle(InputEvent::Press(key(SPACE))).is_empty());
    }

    /// The first press goes down at the CENTRE; the direction is applied on the
    /// next frame. Putting the finger straight on the edge fails to start
    /// movement in some games.
    #[test]
    fn joystick_first_press_lands_on_center() {
        let mut e = Engine::new(joystick_profile());
        let a = e.handle(InputEvent::Press(key(W)));
        match a[..] {
            [TouchAction::Down { at, .. }] => {
                assert!((at.x - 0.2).abs() < 1e-5, "merkez x: {at:?}");
                assert!((at.y - 0.7).abs() < 1e-5, "merkez y: {at:?}");
            }
            _ => panic!("expected a Down: {a:?}"),
        }
        assert!(e.has_pending(), "the direction must be deferred to the next frame");
    }

    #[test]
    fn joystick_direction_applied_on_next_tick() {
        let mut e = Engine::new(joystick_profile());
        let _ = e.handle(InputEvent::Press(key(W)));
        let a = e.tick(5);
        match a[..] {
            [TouchAction::Move { at, .. }] =>
                assert!(at.y < 0.7, "must go up: {at:?}"),
            _ => panic!("expected a Move: {a:?}"),
        }
    }

    /// The radius must be a circle in PIXEL space. Because normalized
    /// coordinates are scaled per axis, the vertical component is multiplied by
    /// the aspect ratio; otherwise at 2560x1440 A/D reach 1.78x further than W/S.
    #[test]
    fn joystick_is_circular_in_pixels_not_normalised_units() {
        let mut e = Engine::new(joystick_profile());
        e.set_aspect(2560, 1440);
        let (w, h) = (2560.0f32, 1440.0f32);

        let _ = e.handle(InputEvent::Press(key(W)));
        let up = match e.tick(5)[..] { [TouchAction::Move { at, .. }] => at, _ => panic!() };
        let dy_px = (0.7 - up.y) * h;

        let mut e2 = Engine::new(joystick_profile());
        e2.set_aspect(2560, 1440);
        let _ = e2.handle(InputEvent::Press(key(D)));
        let right = match e2.tick(5)[..] { [TouchAction::Move { at, .. }] => at, _ => panic!() };
        let dx_px = (right.x - 0.2) * w;

        assert!((dx_px - dy_px).abs() < 2.0,
            "pixel distances must be equal: horizontal {dx_px:.1}px, vertical {dy_px:.1}px");
    }

    #[test]
    fn joystick_second_direction_moves_not_redowns() {
        let mut e = Engine::new(joystick_profile());
        let _ = e.handle(InputEvent::Press(key(W)));
        let a = e.handle(InputEvent::Press(key(D)));
        assert!(matches!(a[..], [TouchAction::Move { .. }]), "the second direction must be a Move: {a:?}");
    }

    /// Diagonal movement must not speed up — it must be normalized.
    #[test]
    fn diagonal_is_normalised_not_faster() {
        let mut e = Engine::new(joystick_profile());
        e.set_aspect(1000, 1000);   // square screen: aspect correction is 1
        let _ = e.handle(InputEvent::Press(key(W)));
        let _ = e.tick(5);          // first press goes to the centre, direction after
        let a = e.handle(InputEvent::Press(key(D)));
        let at = match a[..] { [TouchAction::Move { at, .. }] => at, _ => panic!() };
        let dist = ((at.x - 0.2).powi(2) + (at.y - 0.7).powi(2)).sqrt();
        assert!((dist - 0.1).abs() < 1e-4,
                "the diagonal distance must equal the radius, found {dist}");
    }

    /// Releasing one direction while another is held must NOT lift the finger.
    #[test]
    fn releasing_one_direction_keeps_finger_down() {
        let mut e = Engine::new(joystick_profile());
        let _ = e.handle(InputEvent::Press(key(W)));
        let _ = e.handle(InputEvent::Press(key(D)));
        let a = e.handle(InputEvent::Release(key(W)));
        assert!(matches!(a[..], [TouchAction::Move { .. }]),
                "D is still held, the finger must not lift: {a:?}");
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
        assert_ne!(jid, tid, "concurrent bindings must get separate pointers");
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
        let a = mouse(&mut e, 100.0, 0.0);
        match a[..] {
            [TouchAction::Move { at, .. }] =>
                assert!((at.x - 0.6).abs() < 1e-5, "0.5 + 100*0.001 = 0.6, {at:?}"),
            _ => panic!("expected a Move: {a:?}"),
        }
    }

    /// Noise below the deadzone must produce no events.
    #[test]
    fn aim_ignores_jitter_below_deadzone() {
        let mut e = Engine::new(aim_profile());
        let _ = e.handle(InputEvent::Press(TriggerKind::MouseRight));
        assert!(mouse(&mut e, 0.2, 0.1).is_empty());
    }

    /// WITH a toggle defined, mouse motion without pressing it must produce no touch.
    #[test]
    fn mouse_move_without_aim_active_does_nothing() {
        let mut e = Engine::new(aim_profile());
        assert!(mouse(&mut e, 100.0, 0.0).is_empty());
    }

    /// WITHOUT a toggle, aim is always active: the first mouse motion puts the
    /// finger down. In an FPS, looking must not require holding a key.
    fn aim_engine(handoff: bool) -> Engine {
        let mut b = BTreeMap::new();
        b.insert("bakis".into(), Binding::Aim {
            toggle: None,
            origin: Norm::new(0.5, 0.5),
            sensitivity: 0.001,
            deadzone: 0.5,
            recenter_margin: 0.12,
            handoff, nonlinear: false, reset_delay_ms: 0,
            unbounded: false, safety_span: 32.0,
        });
        Engine::new(Profile { name: "t".into(), package: "p".into(), bindings: b })
    }
    fn always_on_aim() -> Engine { aim_engine(false) }

    /// HANDOFF: a finger must NEVER be lifted while moving.
    ///
    /// The user experienced this as "it stops at the corner then continues": on
    /// the lift-and-place path the turn is cut at handover and the game's touch
    /// smoothing resets.
    #[test]
    fn handoff_never_lifts_without_a_moving_finger() {
        let mut e = aim_engine(true);
        let mut lifted_alone = 0usize;
        for _ in 0..60 {
            let acts = mouse(&mut e, 120.0, 0.0);
            let has_up = acts.iter().any(|a| matches!(a, TouchAction::Up { .. }));
            let has_move = acts.iter().any(|a| matches!(a, TouchAction::Move { .. }));
            if has_up && !has_move { lifted_alone += 1; }
            let _ = e.tick(0);
        }
        assert_eq!(lifted_alone, 0,
            "a moving finger must exist on every lift frame");
    }

    /// The second finger must be PLACED before the handover.
    #[test]
    fn handoff_places_second_finger_before_the_edge() {
        let mut e = aim_engine(true);
        let mut saw_prepare = false;
        for _ in 0..40 {
            let acts = mouse(&mut e, 120.0, 0.0);
            // A finger going down WITHOUT a lift = preparation.
            if acts.iter().any(|a| matches!(a, TouchAction::Down { .. }))
                && !acts.iter().any(|a| matches!(a, TouchAction::Up { .. }))
                && saw_prepare == false
            {
                // The first Down may be the initial touch; look for the second.
                saw_prepare = e.aim_has_second();
            }
            if saw_prepare { break; }
        }
        assert!(saw_prepare, "kenara varmadan ikinci parmak inmeli");
    }

    /// The turn must continue uninterrupted after a handover.
    #[test]
    fn handoff_keeps_rotating_across_the_edge() {
        let mut e = aim_engine(true);
        let mut frames_without_move = 0usize;
        for _ in 0..80 {
            let acts = mouse(&mut e, 150.0, 0.0);
            if !acts.iter().any(|a| matches!(a, TouchAction::Move { .. })) {
                frames_without_move += 1;
            }
            let _ = e.tick(0);
        }
        // Every frame except the first (Down) must contain movement.
        assert!(frames_without_move <= 1,
            "the turn was cut on {frames_without_move} frames");
    }

    /// The first movement puts the finger down AND moves it in the same call:
    /// waiting a frame would be needless latency.
    #[test]
    fn aim_without_toggle_activates_on_first_motion() {
        let mut e = always_on_aim();
        let a = mouse(&mut e, 50.0, 0.0);
        match a[..] {
            [TouchAction::Down { at: d, .. }, TouchAction::Move { at: m, .. }] => {
                assert!((d.x - 0.5).abs() < 1e-5, "must start at the centre: {d:?}");
                assert!((m.x - 0.55).abs() < 1e-5, "0.5 + 50*0.001: {m:?}");
            }
            _ => panic!("expected Down+Move: {a:?}"),
        }
    }

    /// At the edge the finger must be LIFTED and placed at the centre;
    /// otherwise you cannot turn past a limited angle.
    #[test]
    fn aim_recenters_at_edge() {
        let mut e = always_on_aim();
        let _ = mouse(&mut e, 10.0, 0.0);   // -> 0.51
        // 0.51 + 400*0.001 = 0.91 > 1 - 0.12 = 0.88  -> yeniden ortala
        let a = mouse(&mut e, 400.0, 0.0);
        // The applicable part plus the lift; the press is delayed (a separate
        // SYN_REPORT is mandatory).
        assert!(a.iter().any(|x| matches!(x, TouchAction::Up { .. })),
            "a lift must occur: {a:?}");
        assert!(!a.iter().any(|x| matches!(x, TouchAction::Down { .. })),
            "the press must NOT be in the same frame: {a:?}");
        // The press arrives once the delay elapses.
        let b = e.tick(e.now_ms() + 20);
        assert!(b.iter().any(|x| matches!(x, TouchAction::Down { .. })),
            "the press must arrive after the delay: {b:?}");
    }

    /// The reseat gap must be ONE tick.
    ///
    /// Applying the deferred motion and what accumulated meanwhile separately
    /// stretches the gap to two ticks, which the user experiences as "it does
    /// not detect my movements at the edge".
    #[test]
    fn recenter_gap_is_a_single_tick() {
        let mut e = always_on_aim();
        let _ = mouse(&mut e, 10.0, 0.0);
        // A movement large enough to reach the edge -> a reseat.
        let a = mouse(&mut e, 400.0, 0.0);
        assert!(!a.iter().any(|x| matches!(x, TouchAction::Down { .. })),
            "the press must not be in the same frame: {a:?}");
        let b = e.tick(e.now_ms() + 20);
        assert!(b.iter().any(|x| matches!(x, TouchAction::Down { .. })),
            "the press after the delay: {b:?}");
    }

    /// Recentring must NOT LOSE MOVEMENT.
    ///
    /// If it does, some rotation disappears on every recentre and the mouse and
    /// the view drift apart — the user experiences this as "aim drift".
    #[test]
    fn recentering_preserves_the_frames_motion() {
        let mut e = always_on_aim();
        let _ = mouse(&mut e, 10.0, 0.0);   // -> 0.51
        // On the frame that crosses the limit the APPLICABLE part must be sent,
        // not dropped.
        let a = mouse(&mut e, 400.0, 0.0);
        let moved = a.iter().find_map(|x| match x {
            TouchAction::Move { at, .. } => Some(*at), _ => None,
        }).expect("the movement up to the limit must be applied");
        assert!(moved.x > 0.5, "ileri gitmeli: {moved:?}");
    }

    /// Movement must NOT BE LOST during a long turn.
    ///
    /// Motion accumulates during the delayed reset and is applied after the
    /// press. The total distance must stay proportional to the mouse distance sent.
    #[test]
    fn long_turn_loses_no_motion() {
        let mut e = always_on_aim();
        let mut moves = 0usize;
        let mut downs = 0usize;
        for _ in 0..60 {
            for act in mouse(&mut e, 200.0, 0.0) {
                match act {
                    TouchAction::Move { .. } => moves += 1,
                    TouchAction::Down { .. } => downs += 1,
                    _ => {}
                }
            }
            let t = e.now_ms() + 20;
            for act in e.tick(t) {
                match act {
                    TouchAction::Move { .. } => moves += 1,
                    TouchAction::Down { .. } => downs += 1,
                    _ => {}
                }
            }
        }
        assert!(downs >= 1, "at least one reset must occur");
        assert!(moves > 40, "most movements must be applied, found {moves}");
    }

    /// The turn must CONTINUE after a recentre.
    #[test]
    fn aim_continues_after_recenter() {
        let mut e = always_on_aim();
        let _ = mouse(&mut e, 10.0, 0.0);
        let _ = mouse(&mut e, 400.0, 0.0);       // lift
        let _ = e.tick(e.now_ms() + 20);         // delayed press
        // Movement after the press must produce a normal Move.
        let a = mouse(&mut e, 100.0, 0.0);
        assert!(a.iter().any(|x| matches!(x, TouchAction::Move { .. })),
            "movement after the press must produce a Move: {a:?}");
    }

    /// The vertical edge must recentre too.
    #[test]
    fn aim_recenters_on_vertical_edge() {
        let mut e = always_on_aim();
        let _ = mouse(&mut e, 0.0, 10.0);   // -> 0.51
        // 0.51 - 450*0.001 = 0.06 < 0.12  -> yeniden ortala
        let a = mouse(&mut e, 0.0, -450.0);
        assert!(a.iter().any(|x| matches!(x, TouchAction::Up { .. })), "{a:?}");
        assert!(!a.iter().any(|x| matches!(x, TouchAction::Down { .. })), "{a:?}");
        let b = e.tick(e.now_ms() + 20);
        assert!(b.iter().any(|x| matches!(x, TouchAction::Down { .. })), "{b:?}");
    }

    /// Recentring must not consume pointer ids.
    #[test]
    fn recentering_does_not_leak_pointers() {
        let mut e = always_on_aim();
        let _ = mouse(&mut e, 10.0, 0.0);
        for _ in 0..50 {
            let _ = mouse(&mut e, 400.0, 0.0);
        }
        // It must still be working: an exhausted pool would return empty.
        let a = mouse(&mut e, 20.0, 0.0);
        assert!(!a.is_empty(), "the pool may have leaked");
    }

    /// No finger may stay down when the engine is disabled.
    #[test]
    fn disabling_lifts_every_held_finger() {
        let mut e = Engine::new(joystick_profile());
        let _ = e.handle(InputEvent::Press(key(W)));
        let _ = e.handle(InputEvent::Press(key(SPACE)));
        let acts = e.set_enabled(false);
        assert_eq!(acts.len(), 2, "both fingers must lift: {acts:?}");
        assert!(acts.iter().all(|a| matches!(a, TouchAction::Up { .. })));
    }

    #[test]
    fn disabled_engine_produces_nothing() {
        let mut e = Engine::new(joystick_profile());
        let _ = e.set_enabled(false);
        assert!(e.handle(InputEvent::Press(key(SPACE))).is_empty());
    }

    /// Releasing an unpressed key must produce no events (after focus loss).
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

    /// Two swipes in one group: the second must CANCEL the first.
    /// Real in-game feedback: pressing A then quickly W made the game see two
    /// separate fingers and the movements got confused.
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
        // Cancel first (Up), then the new gesture (Down).
        match w[..] {
            [TouchAction::Up { id }, TouchAction::Down { .. }] =>
                assert_eq!(id, first_id, "the cancelled finger must be the first one"),
            _ => panic!("expected Up then Down: {w:?}"),
        }
        assert_eq!(e.swipe_count(), 1, "only the new gesture may remain");
    }

    /// A cancelled gesture must no longer advance.
    #[test]
    fn cancelled_gesture_stops_ticking() {
        let mut e = grouped_engine();
        let _ = e.handle(InputEvent::Press(key(A)));
        let _ = e.tick(20);
        let _ = e.handle(InputEvent::Press(key(W)));
        // Later ticks must advance only W's gesture: vertical movement.
        let mut moves = Vec::new();
        for ms in (25..=110).step_by(5) {
            for act in e.tick(ms) {
                if let TouchAction::Move { at, .. } = act { moves.push(at); }
            }
        }
        assert!(!moves.is_empty());
        assert!(moves.iter().all(|m| (m.x - 0.5).abs() < 1e-4),
                "only vertical movement is allowed, no horizontal trace may remain");
    }

    /// An ungrouped gesture must NOT BE AFFECTED by grouped ones — in shooters
    /// joystick + aim + fire must be simultaneous.
    #[test]
    fn ungrouped_gesture_is_not_cancelled() {
        let mut e = grouped_engine();
        let _ = e.handle(InputEvent::Press(key(SPACE)));
        let _ = e.tick(10);
        let a = e.handle(InputEvent::Press(key(A)));
        assert!(matches!(a[..], [TouchAction::Down { .. }]),
                "an ungrouped gesture must not be cancelled: {a:?}");
        assert_eq!(e.swipe_count(), 2, "both gestures must run together");
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

    /// A swipe must produce INTERMEDIATE STEPS — a single jump is not a gesture.
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
        assert!(moves >= 8, "at least 8 intermediate steps expected, {moves} produced");
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
        assert!(lifted, "the finger must lift at the end of the gesture");
        let at = last_pos.expect("no movement was produced");
        assert!((at.x - 0.2).abs() < 1e-4, "it must reach the target, {at:?}");
        assert!(!e.has_pending(), "a finished gesture must not stay in the list");
    }

    /// Fire and forget: releasing the key must not cut the gesture.
    #[test]
    fn releasing_key_does_not_abort_swipe() {
        let mut e = swipe_engine();
        let _ = e.handle(InputEvent::Press(key(A)));
        let _ = e.tick(20);
        let a = e.handle(InputEvent::Release(key(A)));
        assert!(a.is_empty(), "the release must produce no events: {a:?}");
        assert!(e.has_pending(), "jest devam etmeli");
    }

    /// Pressing again while the same swipe plays must not start a second gesture.
    #[test]
    fn repeated_press_does_not_stack_swipes() {
        let mut e = swipe_engine();
        let _ = e.handle(InputEvent::Press(key(A)));
        let _ = e.handle(InputEvent::Release(key(A)));
        let a = e.handle(InputEvent::Press(key(A)));
        assert!(a.is_empty(), "a second gesture must not start: {a:?}");
    }

    /// On a front-loaded curve the final micro-steps must be filtered out, but
    /// the gesture must still REACH its target and lift the finger.
    #[test]
    fn tiny_tail_steps_are_dropped_but_target_is_reached() {
        use crate::profile::Easing;
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
        assert!(lifted, "the finger must lift");
        let last = moves.last().expect("no movement");
        assert!((last.x - 0.2).abs() < 1e-4, "it must reach the target: {last:?}");

        // No meaningless distance may remain between consecutive steps (except the last).
        for w in moves.windows(2).take(moves.len().saturating_sub(2)) {
            let d = ((w[1].x - w[0].x).powi(2) + (w[1].y - w[0].y).powi(2)).sqrt();
            assert!(d >= MIN_STEP_DELTA * 0.99,
                "a meaningless intermediate step remained: {d} < {MIN_STEP_DELTA}");
        }
    }



    /// Idle recentring: the lift and the press must be in SEPARATE frames.
    ///
    /// Returned in one batch, the backend puts them into one `SYN_REPORT`;
    /// Android cannot see the touch end and the game sees a teleport. The bug
    /// warned about repeatedly elsewhere in the code was here.
    #[test]
    fn idle_recenter_lifts_and_lands_in_separate_frames() {
        let mut e = always_on_aim();
        let _ = e.tick(0);
        let _ = mouse(&mut e, 300.0, 0.0);
        // Immediately: not idle yet.
        assert!(e.tick(10).is_empty(), "must not recentre right after movement");

        // Idle: only the LIFT.
        let a = e.tick(100);
        assert!(matches!(a[..], [TouchAction::Up { .. }]),
                "only the lift may come first: {a:?}");
        assert!(e.has_pending(), "a delayed press must count as pending");

        // Next frame: only the PRESS, and at the centre.
        let b = e.tick(101);
        match b[..] {
            [TouchAction::Down { at, .. }] => {
                assert!((at.x - 0.5).abs() < 1e-4 && (at.y - 0.5).abs() < 1e-4,
                        "merkeze inmeli: {at:?}");
            }
            _ => panic!("only the press was expected in a separate frame: {b:?}"),
        }
    }

    // ---------------------------------------------------------------
    // UNBOUNDED AIM
    //
    // All three symptoms the user reported came from recentring at the edge:
    // lifting the finger cuts the game's tracking ("does not detect at all"),
    // the delayed press feeding itself produces dead zones lasting seconds, and
    // non-linear scaling tied sensitivity to the finger's invisible position
    // ("aim drifts").
    //
    // In unbounded mode there is NO recentring. The following guard that.
    // ---------------------------------------------------------------

    fn free_aim() -> Engine {
        let mut b = BTreeMap::new();
        b.insert("bakis".into(), Binding::Aim {
            toggle: None,
            origin: Norm::new(0.72, 0.5),
            sensitivity: 0.001,
            deadzone: 0.5,
            recenter_margin: 0.12,
            handoff: false, nonlinear: true, reset_delay_ms: 12,
            unbounded: true, safety_span: 32.0,
        });
        let mut e = Engine::new(Profile {
            name: "t".into(), package: "p".into(), bindings: b });
        e.set_offscreen_ok(true);
        e
    }

    /// During a continuous turn the finger must NEVER lift and must go off-screen.
    ///
    /// On the bounded path the same movement produced dozens of lifts/presses;
    /// each cut the game's tracking and swallowed a frame of rotation.
    #[test]
    fn unbounded_aim_never_lifts_the_finger() {
        let mut e = free_aim();
        let mut last = Norm::new(0.0, 0.0);
        for _ in 0..400 {
            for a in mouse(&mut e, 40.0, 0.0) {
                assert!(!matches!(a, TouchAction::Up { .. }),
                        "the finger must not lift in unbounded mode");
                if let TouchAction::Move { at, .. } = a { last = at; }
            }
        }
        assert!(last.is_offscreen(), "the finger must be able to go off-screen: {last:?}");
        assert!(last.x > 5.0, "400 x 40 counts x 0.001 ~ 16 screens: {last:?}");
        assert_eq!(e.active_pointers(), 1, "only one aim finger may remain");
    }

    /// Sensitivity must be CONSTANT: the same mouse movement must cover the
    /// same distance at every position. If it varies, muscle memory is impossible.
    #[test]
    fn unbounded_aim_sensitivity_does_not_drift() {
        let mut e = free_aim();
        let step_at = |e: &mut Engine| -> f32 {
            let before = e.aim_position().expect("the finger must be down").x;
            let _ = mouse(e, 50.0, 0.0);
            e.aim_position().unwrap().x - before
        };
        let _ = mouse(&mut e, 50.0, 0.0);   // press
        let _ = mouse(&mut e, 50.0, 0.0);   // the deferred motion is applied
        let near = step_at(&mut e);
        for _ in 0..200 { let _ = mouse(&mut e, 50.0, 0.0); }
        let far = step_at(&mut e);
        assert!((near - far).abs() < 1e-5,
                "{near} at the centre, {far} far out — sensitivity must not change");
        assert!((near - 0.05).abs() < 1e-5, "50 counts x 0.001 = 0.05: {near}");
    }

    /// The press and the first movement must not be in the SAME frame: the game
    /// could not compute a delta and that turn would be lost.
    #[test]
    fn unbounded_aim_lands_alone_then_moves() {
        let mut e = free_aim();
        let first = mouse(&mut e, 60.0, 0.0);
        match first[..] {
            [TouchAction::Down { at, .. }] => {
                assert!((at.x - 0.72).abs() < 1e-4, "must land in the look area: {at:?}");
            }
            _ => panic!("only the press was expected first: {first:?}"),
        }
        let second = e.tick(e.now_ms() + 1);
        match second[..] {
            [TouchAction::Move { at, .. }] =>
                assert!((at.x - (0.72 + 0.06)).abs() < 1e-4,
                        "the deferred movement must not be lost: {at:?}"),
            _ => panic!("expected a move afterwards: {second:?}"),
        }
    }

    /// If the backend cannot carry off-screen coordinates, unbounded mode must
    /// NOT ENGAGE.
    ///
    /// On the uinput path libinput clamps the coordinate to the screen; trusting
    /// unbounded mode would leave the finger stuck at the edge forever — worse
    /// than the bounded path.
    #[test]
    fn unbounded_needs_backend_support() {
        let mut e = free_aim();
        e.set_offscreen_ok(false);
        assert!(!e.aim_is_unbounded());
        let mut saw_lift = false;
        for _ in 0..400 {
            for a in mouse(&mut e, 40.0, 0.0) {
                if matches!(a, TouchAction::Up { .. }) { saw_lift = true; }
                if let TouchAction::Move { at, .. } = a {
                    assert!(!at.is_offscreen(),
                            "must not go off-screen on an unsupported backend: {at:?}");
                }
            }
        }
        assert!(saw_lift, "a reset is expected on the bounded path");
    }

    /// Even leaning on the safety box, a reset happens ONLY while the mouse is
    /// stopped.
    ///
    /// Resetting during motion is felt as a pause; at a standstill it is never
    /// noticed.
    #[test]
    fn safety_reset_waits_for_the_mouse_to_stop() {
        let mut e = free_aim();
        // Definitively pass half the box (16 screens).
        for _ in 0..600 { let _ = mouse(&mut e, 50.0, 0.0); }
        let far = e.aim_position().expect("the finger must be down");
        assert!(far.x - 0.72 > 16.0, "must be beyond the threshold: {far:?}");

        // The mouse is still moving: no reset.
        for a in mouse(&mut e, 50.0, 0.0) {
            assert!(!matches!(a, TouchAction::Up { .. }),
                    "must not reset while moving");
        }
        // The mouse stopped: a lift, then the press in a separate frame.
        let lift = e.tick(e.now_ms() + 200);
        assert!(matches!(lift[..], [TouchAction::Up { .. }]),
                "must reset when idle: {lift:?}");
        let land = e.tick(e.now_ms() + 20);
        match land[..] {
            [TouchAction::Down { at, .. }] =>
                assert!((at.x - 0.72).abs() < 1e-4, "merkeze inmeli: {at:?}"),
            _ => panic!("the press was expected in a separate frame: {land:?}"),
        }
    }

    /// Idle recentring must NOT happen near the centre — a needless Up/Down is
    /// taken for a tap in some games.
    #[test]
    fn idle_recenter_skips_when_already_near_centre() {
        let mut e = always_on_aim();
        let _ = e.tick(0);
        let _ = mouse(&mut e, 5.0, 0.0);
        assert!(e.tick(500).is_empty(), "must not recentre near the centre");
    }

    /// Pending work after a reset MUST be counted — otherwise the caller stops
    /// calling tick and aim dies completely.
    #[test]
    fn pending_includes_delayed_down() {
        let mut e = always_on_aim();
        let _ = mouse(&mut e, 10.0, 0.0);
        let a = mouse(&mut e, 400.0, 0.0);
        assert!(a.iter().any(|x| matches!(x, TouchAction::Up { .. })));
        assert!(e.has_pending(),
            "has_pending must be TRUE while a delayed press is due");
    }

    /// The joystick must NOT STOP when aim resets while walking.
    #[test]
    fn joystick_keeps_working_during_aim_reset() {
        let mut b = BTreeMap::new();
        b.insert("bakis".into(), Binding::Aim {
            toggle: None, origin: Norm::new(0.5, 0.5),
            sensitivity: 0.001, deadzone: 0.5, recenter_margin: 0.12,
            handoff: false, nonlinear: false, reset_delay_ms: 20,
            unbounded: false, safety_span: 32.0,
        });
        b.insert("hareket".into(), Binding::Joystick {
            up: Trigger::Key(W), down: Trigger::Key(S),
            left: Trigger::Key(A), right: Trigger::Key(D),
            center: Norm::new(0.2, 0.7), radius: 0.1,
        });
        let mut e = Engine::new(Profile {
            name: "t".into(), package: "p".into(), bindings: b });

        let _ = mouse(&mut e, 10.0, 0.0);
        let _ = mouse(&mut e, 400.0, 0.0);      // aim resets
        // The joystick is pressed while the reset is pending.
        let _ = e.handle(InputEvent::Press(key(W)));
        let a = e.tick(e.now_ms() + 1);          // the press is not due yet
        assert!(a.iter().any(|x| matches!(x, TouchAction::Move { .. })),
            "the joystick direction must be applied while aim resets: {a:?}");
    }

    /// A lost key release must NOT LEAK a pointer.
    ///
    /// If leaks accumulate the pool fills and aim cannot get a pointer — the
    /// mouse dies completely. This actually happened.
    #[test]
    fn orphaned_pointers_are_reclaimed() {
        let mut e = Engine::new(joystick_profile());
        let _ = e.handle(InputEvent::Press(key(SPACE)));
        assert_eq!(e.active_pointers(), 1);

        // "Lose" the release event: clear the held set directly.
        e.forget_held_for_test();

        let a = e.tick(10);
        assert!(a.iter().any(|x| matches!(x, TouchAction::Up { .. })),
            "the orphaned pointer must be released: {a:?}");
        assert_eq!(e.active_pointers(), 0, "havuz temizlenmeli");
    }

    #[test]
    fn tick_without_pending_gesture_is_silent() {
        let mut e = swipe_engine();
        assert!(e.tick(1000).is_empty());
    }

    /// A gesture in progress must also be cleaned up when the engine is disabled.
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
    //! Dropping the return of `tick` and `handle` caused a real bug today: the
    //! previous gesture's UP was lost and the finger stayed on screen.
    //! `#[must_use]` catches that at compile time; this module documents the intent.
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
        assert!(!down.is_empty(), "actions must be RETURNED to the caller, not kept inside");
        let up = e.handle(InputEvent::Release(TriggerKind::Key(57)));
        assert!(matches!(up[..], [TouchAction::Up { .. }]));
    }
}
