//! liw-core — the single layer that talks to Waydroid.
//!
//! Design note: `liwinux` treats Waydroid as a *process*, not as a library.
//! This module keeps everything learned about that process — argument
//! pitfalls, health signals, crash chains — in one place.

pub mod bench;
pub mod perf;
pub mod config;
pub mod hostsample;
pub mod helper;
pub mod input;
pub mod polkit;
pub mod session;
pub mod trace;
pub mod waydroid;

pub use config::Config;
pub use helper::{HelperClient, HelperError};
pub use polkit::{check as polkit_check, valid_prop_key, PolkitError};
pub use session::{Health, SessionState, Supervisor, SupervisorConfig};
pub use waydroid::{Status, Waydroid, WaydroidError};
