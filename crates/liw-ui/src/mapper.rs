//! Placing key bindings on the Android picture.
//!
//! The model is the one GameLoop uses and it is the right one: you look at the
//! game, you point at the button you want, and you press the key that should
//! press it. Nothing is typed, no coordinates are entered, and the thing you
//! are aiming at is the actual game rather than a screenshot of it.
//!
//! What is stored is an evdev code and a NORMALISED position, so a binding
//! survives a resized window and a different keyboard layout. Turning a click
//! into that position is the only arithmetic here, and it uses
//! `liw_compositor::fit` — the same function the compositor renders with,
//! because two copies would drift and the symptom would be taps landing
//! somewhere other than where they were placed.

use liw_input::profile::{Binding, Profile, Trigger};
use liw_input::touch::Norm;

/// Where the editor is in the place-then-press cycle.
#[derive(Debug, Default, PartialEq)]
pub enum Editing {
    /// Not editing.
    #[default]
    Off,
    /// Editing, waiting for a click on the picture.
    Placing,
    /// A point has been placed; the next key press binds to it.
    AwaitingKey(Norm),
}

/// The editor's state, and the profile it is changing.
#[derive(Default)]
pub struct Mapper {
    pub editing: Editing,
    /// The profile being edited, loaded when editing starts.
    pub profile: Option<Profile>,
    /// What went wrong, if anything. Shown rather than logged: a save that
    /// silently failed is the worst outcome here.
    pub error: Option<String>,
    /// Set after a successful save, so the UI can say so.
    pub saved: bool,
}

impl Mapper {
    pub fn is_on(&self) -> bool {
        self.editing != Editing::Off
    }

    /// Starts editing the profile for a package, creating one if there is none.
    ///
    /// A missing profile is not an error. The whole point of this screen is
    /// making the first binding for a game that has none.
    pub fn begin(&mut self, package: &str, name: &str) {
        let store = liw_input::store::Store::discover();
        self.profile = Some(match store.for_package(package) {
            Some(entry) => entry.profile.clone(),
            None => Profile {
                name: name.to_string(),
                package: package.to_string(),
                bindings: Default::default(),
            },
        });
        self.editing = Editing::Placing;
        self.error = None;
        self.saved = false;
    }

    pub fn end(&mut self) {
        self.editing = Editing::Off;
        self.profile = None;
    }

    /// Records a click on the picture.
    ///
    /// `point` and `shown` are both in view pixels, with `point` already
    /// relative to the picture's top-left corner. A click outside the picture
    /// — in the letterboxing — is ignored rather than clamped: clamping would
    /// silently place a binding at the edge, which is not what was meant.
    pub fn place(&mut self, point: (f32, f32), shown: (f32, f32)) -> bool {
        if !matches!(self.editing, Editing::Placing) {
            return false;
        }
        if shown.0 <= 0.0 || shown.1 <= 0.0 {
            return false;
        }
        let (x, y) = (point.0 / shown.0, point.1 / shown.1);
        if !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
            return false;
        }
        self.editing = Editing::AwaitingKey(Norm::new(x, y));
        true
    }

    /// Binds a key to the point that is waiting for one.
    ///
    /// Returns the name the binding was stored under. The name matters beyond
    /// display: the input engine allocates one touch pointer per name, so two
    /// bindings sharing a name would fight over the same finger.
    pub fn assign(&mut self, key_name: &str) -> Option<String> {
        let Editing::AwaitingKey(at) = &self.editing else { return None };
        let at = *at;
        let code = crate::keys::evdev_code(key_name)?;
        let profile = self.profile.as_mut()?;

        let label = crate::keys::label(code);
        let name = unique_name(profile, &label);
        profile.bindings.insert(
            name.clone(),
            Binding::Tap { trigger: Trigger::Key(code), at },
        );
        self.editing = Editing::Placing;
        self.saved = false;
        Some(name)
    }

    /// Forgets a binding.
    pub fn remove(&mut self, name: &str) {
        if let Some(p) = self.profile.as_mut() {
            p.bindings.remove(name);
            self.saved = false;
        }
    }

    /// Writes the profile to disk.
    pub fn save(&mut self) {
        let Some(p) = &self.profile else { return };
        let store = liw_input::store::Store::discover();
        match store.save(p) {
            Ok(_) => {
                self.saved = true;
                self.error = None;
            }
            Err(e) => {
                self.saved = false;
                self.error = Some(format!("could not save: {e}"));
            }
        }
    }

    /// The bindings that have a position, for drawing markers.
    ///
    /// Only the ones a marker can be put on. An `Aim` binding has an origin
    /// but is not a button, and drawing it as one would invite a click that
    /// means nothing.
    pub fn markers(&self) -> Vec<(String, Norm, String)> {
        let Some(p) = &self.profile else { return Vec::new() };
        p.bindings
            .iter()
            .filter_map(|(name, b)| match b {
                Binding::Tap { trigger, at } | Binding::Toggle { trigger, at } => {
                    Some((name.clone(), *at, trigger_label(trigger)))
                }
                _ => None,
            })
            .collect()
    }
}

fn trigger_label(t: &Trigger) -> String {
    match t {
        Trigger::Key(c) => crate::keys::label(*c),
        Trigger::MouseLeft => "LMB".into(),
        Trigger::MouseRight => "RMB".into(),
        Trigger::MouseMiddle => "MMB".into(),
        Trigger::WheelUp => "Wheel↑".into(),
        Trigger::WheelDown => "Wheel↓".into(),
    }
}

/// A name not already in the profile.
///
/// Reusing a name would silently replace the binding that had it, and the
/// engine gives one touch pointer per name — so two under one name would
/// fight over the same finger.
fn unique_name(p: &Profile, base: &str) -> String {
    if !p.bindings.contains_key(base) {
        return base.to_string();
    }
    (2..)
        .map(|n| format!("{base} {n}"))
        .find(|c| !p.bindings.contains_key(c))
        .unwrap_or_else(|| base.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editing_profile() -> Mapper {
        Mapper {
            editing: Editing::Placing,
            profile: Some(Profile {
                name: "t".into(),
                package: "com.x".into(),
                bindings: Default::default(),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn a_click_becomes_a_normalised_point() {
        let mut m = editing_profile();
        assert!(m.place((640.0, 360.0), (1280.0, 720.0)));
        assert_eq!(m.editing, Editing::AwaitingKey(Norm::new(0.5, 0.5)));
    }

    /// A click in the letterboxing is not on the game. Clamping it would put
    /// a binding at the edge without saying so.
    #[test]
    fn a_click_outside_the_picture_is_ignored() {
        let mut m = editing_profile();
        assert!(!m.place((1400.0, 360.0), (1280.0, 720.0)));
        assert_eq!(m.editing, Editing::Placing, "still waiting for a real click");
    }

    #[test]
    fn nothing_is_placed_while_not_editing() {
        let mut m = Mapper::default();
        assert!(!m.place((10.0, 10.0), (100.0, 100.0)));
    }

    #[test]
    fn a_key_binds_to_the_placed_point() {
        let mut m = editing_profile();
        m.place((320.0, 180.0), (1280.0, 720.0));
        let name = m.assign("w").expect("w is mappable");
        assert_eq!(name, "W");
        let b = &m.profile.as_ref().unwrap().bindings["W"];
        assert!(matches!(b, Binding::Tap { trigger: Trigger::Key(17), .. }));
    }

    /// After binding, the editor goes back to placing — so several buttons
    /// can be mapped without leaving and re-entering.
    #[test]
    fn binding_returns_to_placing() {
        let mut m = editing_profile();
        m.place((10.0, 10.0), (100.0, 100.0));
        m.assign("a");
        assert_eq!(m.editing, Editing::Placing);
    }

    /// A key the table cannot map must leave the point waiting rather than
    /// dropping it: the user pressed something, and losing the placement
    /// without a word would look like the click never registered.
    #[test]
    fn an_unmappable_key_leaves_the_point_waiting() {
        let mut m = editing_profile();
        m.place((10.0, 10.0), (100.0, 100.0));
        assert_eq!(m.assign("printscreen"), None);
        assert!(matches!(m.editing, Editing::AwaitingKey(_)));
    }

    #[test]
    fn a_key_with_nothing_placed_does_nothing() {
        let mut m = editing_profile();
        assert_eq!(m.assign("w"), None);
    }

    /// Two bindings must never share a name: the engine gives one touch
    /// pointer per name, so they would fight over the same finger.
    #[test]
    fn the_same_key_twice_gets_a_second_name() {
        let mut m = editing_profile();
        m.place((10.0, 10.0), (100.0, 100.0));
        assert_eq!(m.assign("w").unwrap(), "W");
        m.place((20.0, 20.0), (100.0, 100.0));
        assert_eq!(m.assign("w").unwrap(), "W 2");
        assert_eq!(m.profile.unwrap().bindings.len(), 2);
    }

    #[test]
    fn removing_forgets_the_binding() {
        let mut m = editing_profile();
        m.place((10.0, 10.0), (100.0, 100.0));
        let n = m.assign("w").unwrap();
        m.remove(&n);
        assert!(m.profile.unwrap().bindings.is_empty());
    }

    /// Markers are only for bindings a marker means something for. An Aim
    /// origin is not a button and drawing one would invite a useless click.
    #[test]
    fn only_positioned_buttons_get_markers() {
        let mut m = editing_profile();
        let p = m.profile.as_mut().unwrap();
        p.bindings.insert(
            "W".into(),
            Binding::Tap { trigger: Trigger::Key(17), at: Norm::new(0.1, 0.2) },
        );
        // Built by deserialising so this test does not have to track every
        // field Aim gains; it cares that Aim is skipped, not how it is made.
        let aim: Binding = toml::from_str(
            "type = \"aim\"\norigin = { x = 0.5, y = 0.5 }\n\
             sensitivity = 1.0\ndeadzone = 0.0\n",
        )
        .expect("an Aim binding with only its required fields");
        p.bindings.insert("aim".into(), aim);
        let m = m.markers();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].0, "W");
        assert_eq!(m[0].2, "W");
    }

    #[test]
    fn editing_is_off_until_it_begins() {
        let m = Mapper::default();
        assert!(!m.is_on());
        assert!(m.markers().is_empty());
    }
}
