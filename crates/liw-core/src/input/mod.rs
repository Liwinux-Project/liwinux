//! Girdi motoru: host klavye/faresini Android dokunuşlarına çevirir.

pub mod backend;
pub mod capture;
pub mod engine;
pub mod profile;
pub mod touch;
pub mod uinput;

pub use backend::{BackendError, DebugBackend, TouchBackend};
pub use capture::{discover, DeviceInfo, DeviceKind, GrabbedDevice};
pub use engine::{Engine, InputEvent, TriggerKind};
pub use profile::{Binding, Profile, ProfileError, Trigger};
pub use touch::{Norm, PointerPool, TouchAction, MAX_POINTERS};
pub use uinput::{ScreenMap, UinputBackend};
