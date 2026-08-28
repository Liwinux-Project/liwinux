//! Backend that writes DIRECTLY to Waydroid's touch pipe.
//!
//! # Why this path
//!
//! Waydroid patches Android's `EventHub` and listens on three named pipes
//! inside the container
//! (`anbox-patches/frameworks/native/0006-EventHub-Add-wayland-inputs-support.patch`):
//!
//! ```text
//! /dev/input/wl_touch_events     -> EventHub device "wayland_touch"
//! /dev/input/wl_pointer_events   → "wayland_pointer"
//! /dev/input/wl_keyboard_events  → "wayland_keyboard"
//! ```
//!
//! Normally **hwcomposer** writes to this pipe: it receives `wl_touch` from
//! the compositor and converts it into `input_event` records. In other words
//! yolu (uinput → libinput → KWin → wl_touch → hwcomposer → boru) sonunda
//! already ended up here. Writing directly skips four links at once.
//!
//! # The real win: no clamping
//!
//! Nothing on the pipe path squeezes coordinates into the screen:
//!
//! * **No kernel.** The pipe is a FIFO; the evdev driver layer is not
//!   involved, so `ABS` range clamping never runs.
//! * **`TouchInputMapper::cookPointerData()` does not clamp.** It only applies
//!   an affine transform plus scaling; there is no surface bounds test.
//! * **`InputDispatcher` does not re-pick a window on MOVE.** The target is
//!   chosen only at `ACTION_DOWN`/`ACTION_POINTER_DOWN`; afterwards it goes
//!   to latched state.
//!
//! So a finger that goes down inside the game window keeps reaching that same
//! window even when later moves leave the screen. For FPS aim this **removes**
//! the need to "lift and recenter at the edge" — the common source of all
//! three symptoms (no detection at all, a dead zone lasting seconds, aim
//! drift). Details: `docs/mouse-aim.md`.
//!
//! # Privilege
//!
//! Boruyu hwcomposer `mkfifo(..., 0660)` + `chown(..., 1000, 1000)` ile
//! it, owned by Android's `system` user. Writing from the host needs root.
//! That is why this backend does NOT open the file itself: it receives an
//! already-open handle. `liwd-helper` opens the pipe and passes the fd over
//! D-Bus, so the 200 Hz write traffic never goes through IPC.

use crate::backend::{BackendError, TouchBackend};
use crate::touch::{TouchAction, MAX_POINTERS};
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

/// Path of the touch pipe inside the container.
pub const TOUCH_PIPE: &str = "dev/input/wl_touch_events";

/// The pressure hwcomposer writes for every touch. We use the same value: a
/// different one changes pressure-sensitive behaviour in some games.
const PRESSURE: i32 = 50;

// evdev constants. The `evdev` crate exposes these as types, but the wire
// format here is a raw number; we write it directly rather than converting.
const EV_SYN: u16 = 0x00;
const EV_ABS: u16 = 0x03;
const SYN_REPORT: u16 = 0;
const ABS_MT_SLOT: u16 = 0x2f;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;
const ABS_MT_PRESSURE: u16 = 0x3a;

/// On-the-wire size of `struct input_event` (x86_64/aarch64).
/// `timeval` (2 × i64) + type + code + value.
const EVENT_SIZE: usize = 24;

/// POSIX only guarantees atomicity for writes up to this size.
///
/// Mandatory: hwcomposer writes to the same pipe. Splitting a frame in two
/// lets its record slip in between and EventHub reads a corrupt `input_event`.
const PIPE_BUF: usize = 4096;

/// Waydroid container's touch pipe.
#[derive(Debug)]
pub struct WlTouchBackend {
    pipe: File,
    /// Android display resolution (`waydroid.display_width`/`height`).
    /// This is the coordinate space; EventHub derives axis information from
    /// those properties rather than asking the device.
    w: u32,
    h: u32,
    /// Which slots are in use. Needed by `release_all` on shutdown.
    slots: [bool; MAX_POINTERS],
    /// Frames dropped because the pipe was full (diagnostics).
    dropped: u64,
}

impl WlTouchBackend {
    /// Builds from an already-open pipe handle.
    ///
    /// The handle is expected to be opened `O_WRONLY | O_NONBLOCK`: blocking
    /// would lock the input loop and stop the mouse entirely. Dropping a frame
    /// when the pipe is full is the right call — Android is behind and sending
    /// a stale position is worthless.
    pub fn from_pipe(pipe: File, w: u32, h: u32) -> Result<Self, BackendError> {
        if w == 0 || h == 0 {
            return Err(BackendError::Init(
                "display size is 0 — could not read waydroid.display_width/height".into()));
        }
        Ok(Self { pipe, w, h, slots: [false; MAX_POINTERS], dropped: 0 })
    }

    /// Host-visible path of the container's touch pipe.
    ///
    /// `nsenter` is not needed: as root, `/proc/<pid>/root/...` opens a file in
    /// another mount namespace directly.
    pub fn pipe_path(container_pid: u32) -> PathBuf {
        PathBuf::from(format!("/proc/{container_pid}/root/{TOUCH_PIPE}"))
    }

    /// Number of dropped frames (diagnostics).
    pub fn dropped(&self) -> u64 { self.dropped }

    /// Converts a normalized coordinate to an Android screen pixel.
    ///
    /// **NO clamping.** This is the whole basis of unbounded aim; off-screen
    /// values arriving via `Norm::unclamped` must pass through untouched.
    fn to_px(&self, at: crate::touch::Norm) -> (i32, i32) {
        at.to_px(self.w, self.h)
    }
}

/// CLOCK_MONOTONIC saniye/mikrosaniye.
///
/// NOT wall clock: the Android input pipeline uses the event timestamp for
/// resampling and velocity tracking. A wrong clock means a mouse that
/// technically "works" but stutters.
fn monotonic() -> (i64, i64) {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: writing into a valid timespec; clock_gettime has no other side
    // effects.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    (ts.tv_sec as i64, (ts.tv_nsec / 1000) as i64)
}

/// Writes a single `input_event` record in wire format.
fn push_event(buf: &mut Vec<u8>, sec: i64, usec: i64, kind: u16, code: u16, value: i32) {
    buf.extend_from_slice(&sec.to_ne_bytes());
    buf.extend_from_slice(&usec.to_ne_bytes());
    buf.extend_from_slice(&kind.to_ne_bytes());
    buf.extend_from_slice(&code.to_ne_bytes());
    buf.extend_from_slice(&value.to_ne_bytes());
}

impl TouchBackend for WlTouchBackend {
    fn dispatch(&mut self, actions: &[TouchAction]) -> Result<(), BackendError> {
        if actions.is_empty() { return Ok(()); }
        let (sec, usec) = monotonic();
        let mut buf: Vec<u8> = Vec::with_capacity((actions.len() * 5 + 1) * EVENT_SIZE);

        for act in actions {
            match *act {
                TouchAction::Down { id, at } | TouchAction::Move { id, at } => {
                    let slot = id as usize;
                    if slot >= MAX_POINTERS {
                        return Err(BackendError::Dispatch(
                            format!("invalid pointer id {id}")));
                    }
                    self.slots[slot] = true;
                    let (x, y) = self.to_px(at);
                    // hwcomposer's exact ordering. Press and move are the same
                    // sequence: in protocol B a new tracking_id means a press,
                    // repeating the same one means a move.
                    push_event(&mut buf, sec, usec, EV_ABS, ABS_MT_SLOT, slot as i32);
                    push_event(&mut buf, sec, usec, EV_ABS, ABS_MT_TRACKING_ID, slot as i32);
                    push_event(&mut buf, sec, usec, EV_ABS, ABS_MT_POSITION_X, x);
                    push_event(&mut buf, sec, usec, EV_ABS, ABS_MT_POSITION_Y, y);
                    push_event(&mut buf, sec, usec, EV_ABS, ABS_MT_PRESSURE, PRESSURE);
                }
                TouchAction::Up { id } => {
                    let slot = id as usize;
                    if slot >= MAX_POINTERS { continue }
                    self.slots[slot] = false;
                    push_event(&mut buf, sec, usec, EV_ABS, ABS_MT_SLOT, slot as i32);
                    push_event(&mut buf, sec, usec, EV_ABS, ABS_MT_TRACKING_ID, -1);
                }
            }
        }
        // One SYN_REPORT: the actions in this call belong to the same frame.
        push_event(&mut buf, sec, usec, EV_SYN, SYN_REPORT, 0);

        if buf.len() > PIPE_BUF {
            // Rejecting the frame beats losing atomicity: a corrupt record
            // knocks Android's input reader out of alignment and afterwards
            // NO touch works at all.
            return Err(BackendError::Dispatch(format!(
                "frame is {} bytes — exceeds PIPE_BUF ({PIPE_BUF}), atomicity would be lost",
                buf.len())));
        }

        match self.pipe.write(&buf) {
            Ok(n) if n == buf.len() => Ok(()),
            Ok(n) => Err(BackendError::Dispatch(format!(
                "partial write: {n}/{} bytes — pipe alignment broken", buf.len()))),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // Android is behind. Waiting would lock the input loop;
                // dropping only costs one frame.
                self.dropped += 1;
                Ok(())
            }
            Err(e) => Err(BackendError::Dispatch(format!("could not write to pipe: {e}"))),
        }
    }

    fn release_all(&mut self) -> Result<(), BackendError> {
        let ids: Vec<u8> = (0..MAX_POINTERS)
            .filter(|&i| self.slots[i]).map(|i| i as u8).collect();
        if ids.is_empty() { return Ok(()); }
        let acts: Vec<TouchAction> = ids.into_iter()
            .map(|id| TouchAction::Up { id }).collect();
        self.dispatch(&acts)
    }

    fn name(&self) -> &'static str { "wl_touch" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::touch::Norm;

    /// The wire format must be 24 bytes; Android reads it by dividing with
    /// `sizeof(input_event)`. A wrong size means silent, total corruption.
    #[test]
    fn event_record_is_24_bytes() {
        let mut b = Vec::new();
        push_event(&mut b, 1, 2, EV_ABS, ABS_MT_SLOT, 3);
        assert_eq!(b.len(), EVENT_SIZE);
        assert_eq!(std::mem::size_of::<libc::timeval>() + 8, EVENT_SIZE,
                   "must match the wire size of timeval + type/code/value");
    }

    #[test]
    fn pipe_path_reaches_into_the_container() {
        assert_eq!(WlTouchBackend::pipe_path(1234),
                   PathBuf::from("/proc/1234/root/dev/input/wl_touch_events"));
    }

    /// Off-screen coordinates must NOT be clamped — the sole basis of unbounded aim.
    #[test]
    fn offscreen_coordinates_survive_conversion() {
        let (_p, f) = tempfile("offscreen");
        let b = WlTouchBackend::from_pipe(f, 2560, 1440).unwrap();
        assert_eq!(b.to_px(Norm::unclamped(1.5, 0.5)), (3840, 720));
        assert_eq!(b.to_px(Norm::unclamped(-0.25, 0.5)), (-640, 720));
    }

    #[test]
    fn zero_screen_is_rejected_loudly() {
        let e = WlTouchBackend::from_pipe(tempfile("zero").1, 0, 1440).unwrap_err();
        assert!(e.to_string().contains("display_width"), "{e}");
    }

    /// One frame must be ONE write and must end with `SYN_REPORT`.
    #[test]
    fn a_frame_ends_with_one_syn_report() {
        let (p, f) = tempfile("frame");
        let mut b = WlTouchBackend::from_pipe(f, 2560, 1440).unwrap();
        b.dispatch(&[
            TouchAction::Down { id: 0, at: Norm::new(0.5, 0.5) },
            TouchAction::Move { id: 1, at: Norm::new(0.2, 0.3) },
        ]).unwrap();
        let raw = std::fs::read(&p).unwrap();
        assert_eq!(raw.len() % EVENT_SIZE, 0, "record alignment must hold");
        let n = raw.len() / EVENT_SIZE;
        assert_eq!(n, 11, "2 eylem × 5 olay + 1 SYN");
        let last = &raw[(n - 1) * EVENT_SIZE..];
        assert_eq!(u16::from_ne_bytes([last[16], last[17]]), EV_SYN);
        assert_eq!(u16::from_ne_bytes([last[18], last[19]]), SYN_REPORT);
    }

    /// A lift must write `tracking_id = -1`; any other value strands the finger
    /// on screen.
    #[test]
    fn up_writes_tracking_id_minus_one() {
        let (p, f) = tempfile("up");
        let mut b = WlTouchBackend::from_pipe(f, 2560, 1440).unwrap();
        b.dispatch(&[TouchAction::Up { id: 3 }]).unwrap();
        let raw = std::fs::read(&p).unwrap();
        // slot, tracking_id, syn
        assert_eq!(raw.len() / EVENT_SIZE, 3);
        let tid = &raw[EVENT_SIZE..2 * EVENT_SIZE];
        assert_eq!(u16::from_ne_bytes([tid[18], tid[19]]), ABS_MT_TRACKING_ID);
        assert_eq!(i32::from_ne_bytes([tid[20], tid[21], tid[22], tid[23]]), -1);
    }

    /// `release_all` must lift only fingers that are ACTUALLY down.
    ///
    /// The second call staying silent matters: it is invoked repeatedly on
    /// clean shutdown and profile switches, and lifting a finger each time
    /// would send the game bogus touch endings.
    #[test]
    fn release_all_lifts_only_active_slots() {
        let (p, f) = tempfile("release");
        let mut b = WlTouchBackend::from_pipe(f, 2560, 1440).unwrap();
        b.dispatch(&[TouchAction::Down { id: 2, at: Norm::new(0.5, 0.5) }]).unwrap();
        let after_down = std::fs::metadata(&p).unwrap().len() as usize;

        b.release_all().unwrap();
        let after_release = std::fs::metadata(&p).unwrap().len() as usize;
        assert_eq!((after_release - after_down) / EVENT_SIZE, 3,
                   "tek parmak: slot + tracking_id + syn");

        b.release_all().unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().len() as usize, after_release,
                   "the second call must produce no new events");
    }

    // --- test helpers ---
    // A plain file instead of a real FIFO: we can read back the wire format.
    // The name is test-specific: tests run in PARALLEL in the same process, so
    // a shared file would let them read each other's output.
    fn tempfile(tag: &str) -> (std::path::PathBuf, File) {
        let p = std::env::temp_dir()
            .join(format!("liw-wl-touch-{}-{tag}.bin", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let f = File::options().create(true).write(true).truncate(true)
            .open(&p).unwrap();
        (p, f)
    }
}
