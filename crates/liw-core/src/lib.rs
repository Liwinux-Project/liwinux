//! liw-core — Waydroid ile konuşan tek katman.
//!
//! Tasarım notu: `liwinux` Waydroid'i bir kütüphane gibi değil, bir *süreç*
//! olarak kullanır. Bu modül o sürecin etrafındaki tüm bilgi birikimini
//! (argüman tuzakları, sağlık göstergeleri, çökme zincirleri) tek yerde tutar.

pub mod session;
pub mod waydroid;

pub use session::{Health, SessionState, Supervisor, SupervisorConfig};
pub use waydroid::{Status, Waydroid, WaydroidError};
