//! The mouse, as a finger.
//!
//! A click in the game window has to reach Android, and the compositor is not
//! the way to do it. Waydroid's `EventHub` is patched to read a FIFO at
//! `/dev/input/wl_touch_events` and the whole input engine already writes
//! there — that path is measured, it bypasses the compositor entirely, and it
//! is what the key mapper uses. Sending `wl_pointer` events instead would mean
//! a second, unproven route into the same place.
//!
//! So a press becomes a finger down, a drag becomes a move, and a release
//! becomes a finger up.

use liw_input::backend::TouchBackend;
use liw_input::touch::{Norm, TouchAction, MAX_POINTERS};
use liw_input::wl_touch::WlTouchBackend;

/// The pointer id the mouse uses.
///
/// The engine's pool allocates from 0 upward, so the highest id is the last it
/// will ever hand out — which makes this the least likely to collide. It is
/// not a guarantee: a profile holding ten bindings at once would take it, and
/// then a click and a mapped key would share a finger. Ten simultaneous held
/// bindings is not a real profile, but the limit is real and is written down
/// rather than assumed away.
const MOUSE_ID: u8 = MAX_POINTERS as u8 - 1;

/// Sends mouse presses into Android as touches.
#[derive(Default)]
pub struct Touch {
    backend: Option<WlTouchBackend>,
    /// A finger is currently down.
    down: bool,
    /// Why it is not working, if it is not. Shown rather than logged: a click
    /// that silently goes nowhere is the worst outcome here.
    pub error: Option<String>,
}

impl Touch {
    pub fn is_ready(&self) -> bool {
        self.backend.is_some()
    }

    /// Takes an already-opened pipe.
    ///
    /// Opening it needs root, so the daemon's helper does that and passes the
    /// descriptor over D-Bus; this only wraps it.
    pub fn attach(&mut self, pipe: std::fs::File, w: u32, h: u32) {
        match WlTouchBackend::from_pipe(pipe, w, h) {
            Ok(b) => {
                // Worth a line: without the pipe every click is silently
                // swallowed, and "the mouse does nothing" gives no clue
                // whether it never opened or opened and then broke.
                tracing::info!(width = w, height = h, "touch pipe attached");
                self.backend = Some(b);
                self.error = None;
            }
            Err(e) => {
                tracing::warn!(error = %e, "touch pipe unusable");
                self.error = Some(e.to_string());
            }
        }
    }

    /// Press at a normalised position.
    pub fn press(&mut self, at: Norm) {
        // A press while a finger is already down would leave the old one
        // stuck: Android holds a slot until it is lifted, and a stuck finger
        // is a game that stops responding.
        if self.down {
            self.release();
        }
        if self.send(&[TouchAction::Down { id: MOUSE_ID, at }]) {
            self.down = true;
        }
    }

    /// Drag. Ignored when nothing is pressed — Android has no hover.
    pub fn drag(&mut self, at: Norm) {
        if !self.down {
            return;
        }
        self.send(&[TouchAction::Move { id: MOUSE_ID, at }]);
    }

    pub fn release(&mut self) {
        if !self.down {
            return;
        }
        self.send(&[TouchAction::Up { id: MOUSE_ID }]);
        self.down = false;
    }

    /// Lifts everything. Called when the window loses the game, so a finger
    /// cannot be left down in a session the user has walked away from.
    pub fn release_all(&mut self) {
        if let Some(b) = self.backend.as_mut() {
            let _ = b.release_all();
        }
        self.down = false;
    }

    fn send(&mut self, actions: &[TouchAction]) -> bool {
        let Some(b) = self.backend.as_mut() else {
            self.error.get_or_insert_with(|| {
                "the touch pipe is not open — is the session running?".into()
            });
            return false;
        };
        match b.dispatch(actions) {
            Ok(()) => true,
            Err(e) => {
                self.error = Some(e.to_string());
                false
            }
        }
    }
}

/// Turns a click in the picture into a position on the Android screen.
///
/// `point` is relative to the picture's top-left corner and `shown` is how
/// large the guest is drawn. Returns `None` for a click in the letterboxing:
/// that is beside the game, not on it, and clamping would send a touch to the
/// edge of the screen instead.
pub fn to_norm(point: (f32, f32), shown: (f32, f32)) -> Option<Norm> {
    if shown.0 <= 0.0 || shown.1 <= 0.0 {
        return None;
    }
    let (x, y) = (point.0 / shown.0, point.1 / shown.1);
    if !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
        return None;
    }
    Some(Norm::new(x, y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_click_in_the_picture_becomes_a_position() {
        let n = to_norm((640.0, 360.0), (1280.0, 720.0)).unwrap();
        assert_eq!(n, Norm::new(0.5, 0.5));
    }

    /// The letterboxing is beside the game, not on it. Clamping would send a
    /// touch to the screen edge, which is a real tap somewhere unintended.
    #[test]
    fn a_click_in_the_letterboxing_is_not_a_touch() {
        assert!(to_norm((1400.0, 10.0), (1280.0, 720.0)).is_none());
        assert!(to_norm((10.0, -5.0), (1280.0, 720.0)).is_none());
    }

    #[test]
    fn a_picture_with_no_size_yields_nothing() {
        assert!(to_norm((1.0, 1.0), (0.0, 0.0)).is_none());
    }

    /// The corners are on the game, so they must map rather than be refused.
    #[test]
    fn the_corners_are_inside() {
        assert_eq!(to_norm((0.0, 0.0), (100.0, 100.0)), Some(Norm::new(0.0, 0.0)));
        assert_eq!(to_norm((100.0, 100.0), (100.0, 100.0)), Some(Norm::new(1.0, 1.0)));
    }

    /// The pool hands out from 0 upward, so the mouse takes the last id.
    #[test]
    fn the_mouse_uses_the_id_the_pool_reaches_last() {
        assert_eq!(MOUSE_ID as usize, MAX_POINTERS - 1);
    }

    /// Without a pipe nothing is sent, and the reason is recorded rather than
    /// dropped — a click that goes nowhere in silence is the worst outcome.
    #[test]
    fn a_press_without_a_pipe_reports_why() {
        let mut t = Touch::default();
        t.press(Norm::new(0.5, 0.5));
        assert!(!t.down, "no finger can be down without a pipe");
        assert!(t.error.is_some());
    }

    #[test]
    fn dragging_without_a_press_does_nothing() {
        let mut t = Touch::default();
        t.drag(Norm::new(0.5, 0.5));
        assert!(!t.down);
    }

    #[test]
    fn releasing_what_was_never_pressed_is_harmless() {
        let mut t = Touch::default();
        t.release();
        assert!(!t.down);
    }

    #[test]
    fn nothing_is_ready_before_a_pipe_arrives() {
        assert!(!Touch::default().is_ready());
    }
}
