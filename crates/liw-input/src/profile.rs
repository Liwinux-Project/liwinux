//! Per-game key mapping profile.
//!
//! Coordinates are kept normalized (0..1) so a profile is independent of
//! resolution and window size. Using a profile on another machine needs no
//! editing.

use crate::touch::Norm;
use serde::{Deserialize, Serialize};

/// A trigger on the keyboard or mouse.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Trigger {
    /// evdev key code (e.g. KEY_W = 17). The raw code is stored so it stays
    /// layout-independent — not the letter "W", but that physical key.
    Key(u16),
    MouseLeft,
    MouseRight,
    MouseMiddle,
    WheelUp,
    WheelDown,
}

/// Velocity curve of a swipe.
///
/// Games recognise a swipe direction once a distance threshold is crossed. On a
/// linear curve that happens mid-gesture; on a front-loaded curve much earlier.
/// The curve therefore directly affects when the game reacts.
///
/// Default `Linear` — which curve feels right in which game cannot be known
/// without measuring, so we do not change behaviour silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Easing {
    /// Constant speed.
    #[default]
    Linear,
    /// Fast start, slowing down (quadratic ease-out). 75% of the distance is
    /// covered in half the time.
    EaseOut,
    /// Even more front-loaded (cubic). 87.5% of the distance in half the time.
    /// Too aggressive and the gesture looks like a "teleport" and may not be
    /// recognised at all.
    EaseOutStrong,
}

impl Easing {
    /// Maps normalized progress (0..1) to the fraction of distance covered.
    ///
    /// Endpoints are preserved: f(0)=0, f(1)=1. Otherwise the gesture never
    /// reaches its target, or jumps away from its start.
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Easing::Linear => t,
            Easing::EaseOut => 1.0 - (1.0 - t) * (1.0 - t),
            Easing::EaseOutStrong => {
                let inv = 1.0 - t;
                1.0 - inv * inv * inv
            }
        }
    }
}

/// Behaviour of a binding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Binding {
    /// The finger stays down while the key is held.
    Tap { trigger: Trigger, at: Norm },

    /// One tap on press-and-release (holding does not repeat).
    Toggle { trigger: Trigger, at: Norm },

    /// A four-way virtual joystick such as WASD.
    /// A single finger moving within `radius` around the centre.
    Joystick {
        up: Trigger, down: Trigger, left: Trigger, right: Trigger,
        center: Norm,
        radius: f32,
    },

    /// FPS aiming: relative mouse motion -> continuous drag.
    ///
    /// Without `toggle`, aim is always active — in an FPS the mouse look must
    /// be continuous and should not require holding a key.
    Aim {
        #[serde(default)]
        toggle: Option<Trigger>,
        origin: Norm,
        /// Converts a mouse pixel into normalized distance.
        sensitivity: f32,
        /// Motion below this threshold is ignored (mouse noise).
        deadzone: f32,
        /// When the finger gets this close to the screen edge it is lifted and
        /// placed back at the centre.
        ///
        /// This is MANDATORY: a finger cannot be dragged forever. Without
        /// recentring you cannot turn past a limited angle. Real players do the
        /// same thing and games expect it.
        #[serde(default = "default_recenter_margin")]
        recenter_margin: f32,
        /// Devir teslimle yeniden ortalama.
        ///
        /// When on, a second finger goes down at the centre BEFORE the first
        /// reaches the edge and both move together; the first is released only
        /// afterwards. That way a moving finger is on screen at every instant.
        ///
        /// When off, the simple path: lift, place at centre, continue. On that
        /// path the turn is cut at the moment of handover and, because the
        /// game's touch smoothing resets, it feels like "it stops then resumes".
        #[serde(default = "default_true_handoff")]
        handoff: bool,
        /// Non-linear scaling.
        ///
        /// Sensitivity falls off as the finger moves away from the centre, by
        /// `sqrt(min/distance)`; the finger approaches the edge asymptotically
        /// and never really arrives. The need to reseat therefore never arises
        /// in the first place.
        ///
        /// Taken from XtMapper's MouseAimHandler (GPL-3, same idea).
        #[serde(default = "default_true_nonlinear")]
        nonlinear: bool,
        /// Delay inserted between the lift and the press (ms).
        ///
        /// Sending them in the same frame gives Android no chance to treat the
        /// lift as a genuine touch end; the game sees a teleport.
        #[serde(default = "default_reset_delay_ms")]
        reset_delay_ms: u32,
        /// Whether the finger may travel OFF-SCREEN.
        ///
        /// When on, the whole recentring mechanism above (edge margin, handoff,
        /// non-linear scaling, delayed press) is disabled: the finger goes down
        /// once and roams an unbounded plane until aim is released. With no
        /// finger to lift, the "not detecting" and "aim drifts" symptoms
        /// disappear at the root.
        ///
        /// The justification is Waydroid-specific: nothing on the touch pipe
        /// clamps, and `InputDispatcher` does not re-pick a window on MOVE.
        /// It is therefore only valid on backends that do not clamp — enabled
        /// via `Engine::set_offscreen_ok`. If off, the engine silently falls
        /// back to bounded mode; on the uinput path an off-screen coordinate
        /// gets squeezed by libinput and the finger would stick at the edge.
        #[serde(default = "default_unbounded")]
        unbounded: bool,
        /// Half-width of the safety box (in screens).
        ///
        /// The only numeric limit of unbounded mode. Its purpose is overflow
        /// protection, not feel: `input_event.value` is i32 and the position is
        /// f32. 32 screens is about 82000 pixels; f32 precision there is still
        /// below 0.01 pixel. Drifting this far NET (outbound minus return) does not
        /// happen in practice; if it does, the finger recentres silently when
        #[serde(default = "default_safety_span")]
        safety_span: f32,
    },

    /// Swipe gesture (for games such as Subway Surfers).
    ///
    /// With `group` set, this gesture is CANCELLED when another gesture in the
    /// same group starts. Mandatory in single-finger games like Subway Surfers:
    /// pressing A then quickly W must not look like two separate fingers to the
    /// game. In shooters, joystick + aim + fire must be simultaneous, so
    /// exclusivity is NOT the default — it is requested explicitly.
    Swipe {
        trigger: Trigger,
        from: Norm,
        to: Norm,
        duration_ms: u32,
        #[serde(default)]
        group: Option<String>,
        #[serde(default)]
        easing: Easing,
    },
}

impl Binding {
    /// The inputs that trigger this binding.
    pub fn triggers(&self) -> Vec<Trigger> {
        match self {
            Binding::Tap { trigger, .. }
            | Binding::Toggle { trigger, .. }
            | Binding::Swipe { trigger, .. } => vec![trigger.clone()],
            Binding::Joystick { up, down, left, right, .. } =>
                vec![up.clone(), down.clone(), left.clone(), right.clone()],
            Binding::Aim { toggle, .. } =>
                toggle.clone().into_iter().collect(),
        }
    }
}

fn default_recenter_margin() -> f32 { 0.12 }
/// Default OFF.
///
/// Tried and it did not work in Special Forces Group 2: the game tracks the
/// two fingers separately and averages them, and two touch traces appear on
/// screen. It may differ per game, so the option stays — just not by default.
fn default_true_handoff() -> bool { false }
fn default_true_nonlinear() -> bool { true }
fn default_reset_delay_ms() -> u32 { 12 }
/// Default ON.
///
/// Bounded mode produced those symptoms and all of them were unavoidable; it is
/// not behaviour worth preserving. If the backend does not support it the
/// engine falls back to bounded mode anyway, so an ON default breaks no path.
fn default_unbounded() -> bool { true }
fn default_safety_span() -> f32 { 32.0 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    /// Android package name, so the profile can be applied automatically.
    pub package: String,
    /// Binding name -> behaviour. The name is the key for pointer allocation.
    pub bindings: std::collections::BTreeMap<String, Binding>,
}

#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error("could not read profile: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not parse profile: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("invalid profile: {0}")]
    Invalid(String),
}

impl Profile {
    pub fn from_toml(s: &str) -> Result<Self, ProfileError> {
        let p: Profile = toml::from_str(s)?;
        p.validate()?;
        Ok(p)
    }

    /// Checks the profile's consistency before loading it.
    ///
    /// NOTE: there is no limit on the NUMBER of bindings. `MAX_POINTERS` is the
    /// simultaneous-finger limit; a profile may have 20 buttons of which only a
    /// few are pressed at once. Conflating the two rejects valid profiles — it
    /// actually happened: an FPS profile with 11 buttons was rejected. If the
    /// pool runs dry the engine warns at runtime.
    ///
    /// Most important: **the same trigger cannot be used in two bindings.**
    /// If it is, which binding runs becomes undefined and the user experiences
    /// it as "sometimes it works" — the hardest class of bug to diagnose.
    pub fn validate(&self) -> Result<(), ProfileError> {
        if self.bindings.is_empty() {
            return Err(ProfileError::Invalid("profile has no bindings".into()));
        }
        let mut seen: std::collections::HashMap<Trigger, &str> = Default::default();
        for (name, b) in &self.bindings {
            for t in b.triggers() {
                if let Some(prev) = seen.insert(t.clone(), name) {
                    return Err(ProfileError::Invalid(format!(
                        "duplicate trigger in two bindings: '{prev}' and '{name}'")));
                }
            }
            if let Binding::Joystick { radius, .. } = b {
                if !(0.0..=0.5).contains(radius) {
                    return Err(ProfileError::Invalid(format!(
                        "joystick radius of '{name}' must be within 0..0.5, got {radius}")));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
name = "Subway Surfers"
package = "com.kiloo.subwaysurf"

[bindings.sol]
type = "swipe"
trigger = { Key = 30 }
from = { x = 0.5, y = 0.5 }
to = { x = 0.2, y = 0.5 }
duration_ms = 80

[bindings.zipla]
type = "swipe"
trigger = { Key = 57 }
from = { x = 0.5, y = 0.6 }
to = { x = 0.5, y = 0.3 }
duration_ms = 80
"#;

    #[test]
    fn parses_a_real_profile() {
        let p = Profile::from_toml(SAMPLE).unwrap();
        assert_eq!(p.package, "com.kiloo.subwaysurf");
        assert_eq!(p.bindings.len(), 2);
    }

    #[test]
    fn rejects_duplicate_trigger() {
        let dup = SAMPLE.replace("trigger = { Key = 57 }", "trigger = { Key = 30 }");
        let err = Profile::from_toml(&dup).unwrap_err();
        assert!(err.to_string().contains("duplicate trigger"), "{err}");
    }

    /// A profile with many buttons must be ACCEPTED: the simultaneous-finger
    /// limit is not the same thing as the total number of bindings.
    #[test]
    fn accepts_profile_with_more_bindings_than_pointers() {
        let mut t = String::from("name = \"many\"\npackage = \"p\"\n");
        for i in 0..15u16 {
            t.push_str(&format!(
                "[bindings.b{i}]\ntype = \"tap\"\ntrigger = {{ Key = {} }}\n\
                 at = {{ x = 0.5, y = 0.5 }}\n", 100 + i));
        }
        let p = Profile::from_toml(&t).expect("15 bindings must not be rejected");
        assert_eq!(p.bindings.len(), 15);
    }

    #[test]
    fn rejects_empty_profile() {
        let err = Profile::from_toml("name=\"x\"\npackage=\"y\"\n[bindings]\n").unwrap_err();
        assert!(err.to_string().contains("no bindings"));
    }

    #[test]
    fn rejects_out_of_range_joystick_radius() {
        let t = r#"
name = "t"
package = "p"
[bindings.hareket]
type = "joystick"
up = { Key = 17 }
down = { Key = 31 }
left = { Key = 30 }
right = { Key = 32 }
center = { x = 0.2, y = 0.7 }
radius = 0.9
"#;
        let err = Profile::from_toml(t).unwrap_err();
        assert!(err.to_string().contains("radius"), "{err}");
    }

    #[test]
    fn easing_preserves_endpoints() {
        for e in [Easing::Linear, Easing::EaseOut, Easing::EaseOutStrong] {
            assert!((e.apply(0.0) - 0.0).abs() < 1e-6, "{e:?} f(0) != 0");
            assert!((e.apply(1.0) - 1.0).abs() < 1e-6, "{e:?} f(1) != 1");
        }
    }

    /// A front-loaded curve must cover distance EARLIER than linear — that is
    /// its only purpose: crossing the game's swipe threshold sooner.
    #[test]
    fn ease_out_is_front_loaded() {
        for t in [0.1f32, 0.25, 0.5, 0.75] {
            let lin = Easing::Linear.apply(t);
            let eo = Easing::EaseOut.apply(t);
            let eos = Easing::EaseOutStrong.apply(t);
            assert!(eo > lin, "t={t}: ease_out {eo} > linear {lin} expected");
            assert!(eos > eo, "t={t}: strong {eos} > ease_out {eo} expected");
        }
    }

    #[test]
    fn ease_out_half_time_covers_three_quarters() {
        assert!((Easing::EaseOut.apply(0.5) - 0.75).abs() < 1e-5);
        assert!((Easing::EaseOutStrong.apply(0.5) - 0.875).abs() < 1e-5);
    }

    #[test]
    fn easing_clamps_out_of_range_progress() {
        assert_eq!(Easing::EaseOut.apply(-1.0), 0.0);
        assert_eq!(Easing::EaseOut.apply(2.0), 1.0);
    }

    /// Unspecified in the profile it must be linear — behaviour must not change
    #[test]
    fn easing_defaults_to_linear_when_absent() {
        let p = Profile::from_toml(SAMPLE).unwrap();
        match p.bindings.get("sol").unwrap() {
            Binding::Swipe { easing, .. } => assert_eq!(*easing, Easing::Linear),
            other => panic!("swipe bekleniyordu: {other:?}"),
        }
    }

    #[test]
    fn joystick_reports_all_four_triggers() {
        let b = Binding::Joystick {
            up: Trigger::Key(17), down: Trigger::Key(31),
            left: Trigger::Key(30), right: Trigger::Key(32),
            center: Norm::new(0.2, 0.7), radius: 0.1,
        };
        assert_eq!(b.triggers().len(), 4);
    }
}
