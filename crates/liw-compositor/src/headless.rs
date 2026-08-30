//! Rendering the guest without a window of our own.
//!
//! # Why headless
//!
//! The standalone binary opens a winit window and paints into it, which is
//! how the path was proved. Inside `liw-ui` that is exactly wrong: the UI
//! already has a window, and a second one is not an embedded Android, it is
//! two windows.
//!
//! So the compositor renders into an offscreen texture and hands the pixels
//! over. gpui on Linux has no external-texture path — its `surface` element
//! is macOS-only, taking a `CVPixelBuffer` — so the pixels have to arrive as
//! bytes.
//!
//! # What that costs, measured
//!
//! Reading a 1280x720 frame back costs **0.8 to 2.0 ms**, 5 to 12 per cent of
//! a 16.67 ms frame. That was measured before this module was written,
//! because it is the number that decides whether the approach is worth
//! having at all: a zero-copy import would need a patch to the pinned Zed
//! fork, and paying two milliseconds to avoid that patch is a fair trade
//! while the rest is being built. It is not free, and when the fork does gain
//! a texture path this whole module becomes the slow way.

use std::sync::{Arc, Mutex};

use smithay::backend::allocator::Fourcc;
use smithay::backend::egl::{EGLContext, EGLDevice, EGLDisplay};
use smithay::backend::renderer::gles::{GlesRenderer, GlesTarget, GlesTexture};
use smithay::backend::renderer::{Bind, ExportMem, Offscreen};
use smithay::utils::{Buffer as BufferCoords, Rectangle, Size, Transform};

/// One decoded frame, in the layout gpui wants.
#[derive(Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// BGRA8, which is what gpui's `RenderImage` takes.
    ///
    /// The fourcc that produces it is `Argb8888`, not `Abgr8888`. DRM names
    /// the channels most-significant-first in a little-endian word, so
    /// ARGB8888 lands in memory as B, G, R, A — already the byte order gpui
    /// wants. Capturing as ABGR8888 (the format the guest sends) would give
    /// R, G, B, A and need a per-pixel swap on the CPU every frame, which at
    /// 1280x720 and 60 Hz is 55 million byte swaps a second for nothing.
    pub bgra: Vec<u8>,
    /// Rises once per delivered frame, so a consumer can tell a new frame
    /// from the same one read twice.
    pub serial: u64,
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Frame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &self.bgra.len())
            .field("serial", &self.serial)
            .finish()
    }
}

/// Where the newest frame is left for the UI thread to pick up.
///
/// One slot, not a queue. A UI that is behind wants the newest frame, not the
/// oldest one it has not seen yet; queueing would trade latency for frames
/// nobody will ever look at.
pub type FrameSlot = Arc<Mutex<Option<Frame>>>;

/// An EGL context and an offscreen target to draw the guest into.
pub struct Headless {
    renderer: GlesRenderer,
    target: GlesTexture,
    size: Size<i32, BufferCoords>,
    serial: u64,
}

impl Headless {
    /// Opens a headless EGL context on the first device that will give one.
    ///
    /// Enumerating devices rather than naming one keeps this working on a
    /// machine with a different GPU; the first that yields a context wins,
    /// and if none does the error says so rather than falling back to
    /// software that would never keep up.
    pub fn new(width: i32, height: i32) -> Result<Self, String> {
        let devices = EGLDevice::enumerate()
            .map_err(|e| format!("could not enumerate EGL devices: {e}"))?;

        let mut last = String::from("no EGL device was offered");
        for device in devices {
            // SAFETY: the display is owned by the context built from it and
            // outlives every use here.
            let display = match unsafe { EGLDisplay::new(device) } {
                Ok(d) => d,
                Err(e) => {
                    last = format!("EGLDisplay: {e}");
                    continue;
                }
            };
            let context = match EGLContext::new(&display) {
                Ok(c) => c,
                Err(e) => {
                    last = format!("EGLContext: {e}");
                    continue;
                }
            };
            // SAFETY: the context is current on this thread and not shared.
            let mut renderer = match unsafe { GlesRenderer::new(context) } {
                Ok(r) => r,
                Err(e) => {
                    last = format!("GlesRenderer: {e}");
                    continue;
                }
            };
            let size = Size::from((width.max(1), height.max(1)));
            let target = renderer
                .create_buffer(Fourcc::Abgr8888, size)
                .map_err(|e| format!("could not create the offscreen target: {e}"))?;
            return Ok(Self { renderer, target, size, serial: 0 });
        }
        Err(last)
    }

    pub fn renderer(&mut self) -> &mut GlesRenderer {
        &mut self.renderer
    }

    pub fn size(&self) -> (i32, i32) {
        (self.size.w, self.size.h)
    }

    /// Resizes the offscreen target. Cheap when the size has not changed.
    pub fn resize(&mut self, width: i32, height: i32) -> Result<(), String> {
        let want = Size::from((width.max(1), height.max(1)));
        if want == self.size {
            return Ok(());
        }
        self.target = self
            .renderer
            .create_buffer(Fourcc::Abgr8888, want)
            .map_err(|e| format!("could not resize the offscreen target: {e}"))?;
        self.size = want;
        Ok(())
    }

    /// Draws whatever `paint` puts on the target, then reads it back.
    ///
    /// The closure gets the renderer and its framebuffer so this module does
    /// not need to know anything about surfaces or damage; that stays with
    /// the caller, which is the only place that knows what a frame contains.
    pub fn capture<F>(&mut self, paint: F) -> Result<Frame, String>
    where
        F: FnOnce(&mut GlesRenderer, &mut GlesTarget<'_>) -> Result<(), String>,
    {
        let mut fb = self
            .renderer
            .bind(&mut self.target)
            .map_err(|e| format!("could not bind the offscreen target: {e}"))?;

        paint(&mut self.renderer, &mut fb)?;

        let region = Rectangle::from_size(self.size);
        let mapping = self
            .renderer
            .copy_framebuffer(&fb, region, Fourcc::Argb8888)
            .map_err(|e| format!("could not copy the frame: {e}"))?;
        drop(fb);

        let bytes = self
            .renderer
            .map_texture(&mapping)
            .map_err(|e| format!("could not map the frame: {e}"))?;

        self.serial += 1;
        Ok(Frame {
            width: self.size.w as u32,
            height: self.size.h as u32,
            bgra: bytes.to_vec(),
            serial: self.serial,
        })
    }
}

/// The transform the offscreen path renders with.
///
/// Unlike the winit backend, which hands back a framebuffer that is already
/// upside down relative to GL, an offscreen texture is read in the same
/// orientation it was drawn. Flipping here as well would put the picture on
/// its head — a mistake that looks like a driver bug and is not one.
pub const OFFSCREEN_TRANSFORM: Transform = Transform::Normal;

#[cfg(test)]
mod tests {
    use super::*;

    /// The slot keeps the NEWEST frame. A consumer that has fallen behind
    /// wants the current picture, not a backlog it will never catch up on.
    #[test]
    fn the_slot_holds_only_the_latest() {
        let slot: FrameSlot = Arc::new(Mutex::new(None));
        for serial in 1..=3 {
            *slot.lock().unwrap() = Some(Frame {
                width: 4,
                height: 4,
                bgra: vec![0; 64],
                serial,
            });
        }
        assert_eq!(slot.lock().unwrap().as_ref().unwrap().serial, 3);
    }

    /// The serial is what lets a consumer tell a new frame from the same one
    /// read twice; without it the UI cannot skip work it has already done.
    #[test]
    fn frames_carry_a_rising_serial() {
        let a = Frame { width: 1, height: 1, bgra: vec![0; 4], serial: 1 };
        let b = Frame { width: 1, height: 1, bgra: vec![0; 4], serial: 2 };
        assert!(b.serial > a.serial);
    }

    /// Offscreen rendering is not flipped. The winit path needs
    /// Transform::Flipped180 and copying that here would invert the picture.
    #[test]
    fn offscreen_is_not_flipped() {
        assert_eq!(OFFSCREEN_TRANSFORM, Transform::Normal);
    }

    /// Debug must not try to print several megabytes of pixels.
    #[test]
    fn debug_prints_the_size_not_the_pixels() {
        let f = Frame { width: 2, height: 2, bgra: vec![7; 16], serial: 9 };
        let s = format!("{f:?}");
        assert!(s.contains("bytes: 16"), "{s}");
        assert!(!s.contains("7, 7, 7"), "pixels must not be printed: {s}");
    }
}
