//! Touch injection backends.
//!
//! The engine produces `TouchAction`s; a backend delivers them to Android.
//! Backends are pluggable because which path actually works is decided by
//! measurement:
//!
//! * `uinput`  — a virtual touchscreen on the host; libinput -> KWin ->
//!               wl_touch -> Waydroid. Needs no container changes and no Java.
//! * `debug`   — prints only; verifies mapping without touching Android.
//! * (future) `android_socket` — app_process server + injectInputEvent().

use crate::touch::TouchAction;

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("backend init failed: {0}")]
    Init(String),
    #[error("touch dispatch failed: {0}")]
    Dispatch(String),
}

pub trait TouchBackend: Send {
    /// Applies the actions in order. Actions arriving in a single call belong
    /// to the SAME frame and must be terminated by one SYN_REPORT; otherwise a
    /// multi-touch gesture looks like "separate fingers" to Android.
    fn dispatch(&mut self, actions: &[TouchAction]) -> Result<(), BackendError>;

    /// Lifts every finger (emergency stop, profile switch).
    fn release_all(&mut self) -> Result<(), BackendError>;

    fn name(&self) -> &'static str;
}

/// A backend that injects nowhere and only records.
#[derive(Debug, Default)]
pub struct DebugBackend {
    pub log: Vec<TouchAction>,
}

impl TouchBackend for DebugBackend {
    fn dispatch(&mut self, actions: &[TouchAction]) -> Result<(), BackendError> {
        self.log.extend_from_slice(actions);
        Ok(())
    }
    fn release_all(&mut self) -> Result<(), BackendError> { Ok(()) }
    fn name(&self) -> &'static str { "debug" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::touch::Norm;

    #[test]
    fn debug_backend_records_in_order() {
        let mut b = DebugBackend::default();
        b.dispatch(&[
            TouchAction::Down { id: 0, at: Norm::new(0.1, 0.2) },
            TouchAction::Move { id: 0, at: Norm::new(0.3, 0.4) },
        ]).unwrap();
        b.dispatch(&[TouchAction::Up { id: 0 }]).unwrap();
        assert_eq!(b.log.len(), 3);
        assert!(matches!(b.log[2], TouchAction::Up { id: 0 }));
    }
}
