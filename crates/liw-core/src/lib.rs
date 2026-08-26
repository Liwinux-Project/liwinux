//! liw-core — Waydroid ile konuşan tek katman.
//!
//! Tasarım notu: `liwinux` Waydroid'i bir kütüphane gibi değil, bir *süreç*
//! olarak kullanır. Bu modül o sürecin etrafındaki tüm bilgi birikimini
//! (argüman tuzakları, sağlık göstergeleri, çökme zincirleri) tek yerde tutar.

pub mod bench;
pub mod config;
pub mod hostsample;
pub mod helper;
pub mod input;
pub mod polkit;
pub mod session;
pub mod waydroid;

pub use config::Config;
pub use helper::{HelperClient, HelperError};
pub use polkit::{check as polkit_check, valid_prop_key, PolkitError};
pub use session::{Health, SessionState, Supervisor, SupervisorConfig};
pub use waydroid::{Status, Waydroid, WaydroidError};
