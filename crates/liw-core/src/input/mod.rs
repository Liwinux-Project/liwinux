//! Girdi motoru: host klavye/faresini Android dokunuşlarına çevirir.

pub mod engine;
pub mod profile;
pub mod touch;

pub use engine::{Engine, InputEvent, TriggerKind};
pub use profile::{Binding, Profile, ProfileError, Trigger};
pub use touch::{Norm, PointerPool, TouchAction, MAX_POINTERS};
