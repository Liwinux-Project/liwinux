//! Per-app accent colour, derived from the app's own icon.
//!
//! # Why this exists
//!
//! GameLoop looks rich because every tile carries cover art. Waydroid caches
//! icons at 54x54 and there is no art to be had, so a poster grid is off the
//! table. What IS available is the icon's colour — and a card washed in the
//! game's own hue reads as designed rather than as a list row, without
//! pretending to be artwork it does not have.
//!
//! Pure: bytes in, colour out. The interesting part (hue averaging) is easy to
//! get subtly wrong and impossible to eyeball afterwards, so it is tested.

use gpui::Hsla;

/// A colour taken from an icon, already shaped for use behind content.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tint {
    pub hue: f32,
    pub sat: f32,
}

impl Tint {
    /// Neutral fallback for icons with no usable colour.
    pub const NEUTRAL: Tint = Tint { hue: 0.62, sat: 0.0 };

    /// Backdrop wash. Kept dark and desaturated: this sits BEHIND text, and a
    /// vivid card is a card you cannot read.
    pub fn wash(self, alpha: f32) -> Hsla {
        Hsla { h: self.hue, s: (self.sat * 0.55).min(0.5), l: 0.30, a: alpha }
    }

    /// Panel background: the tint fading into the page.
    ///
    /// A flat fill reads as a coloured box; a gradient reads as a designed
    /// surface. It also keeps the right-hand side dark, which is where the
    /// secondary list sits.
    pub fn gradient(self, angle: f32) -> gpui::Background {
        gpui::linear_gradient(
            angle,
            gpui::linear_color_stop(self.wash(0.85), 0.0),
            gpui::linear_color_stop(Hsla { h: self.hue, s: self.sat * 0.25,
                                           l: 0.09, a: 1.0 }, 1.0),
        )
    }

    /// Foreground use — a badge or hairline. Bright enough to register.
    pub fn accent(self) -> Hsla {
        Hsla { h: self.hue, s: (self.sat * 0.9).clamp(0.35, 0.85), l: 0.62, a: 1.0 }
    }
}

/// Extracts a tint from encoded image bytes.
///
/// Returns `NEUTRAL` when the image cannot be decoded or carries no colour
/// worth using (a greyscale icon), rather than inventing a hue — a wrong
/// colour is worse than no colour.
pub fn from_bytes(bytes: &[u8]) -> Tint {
    let Ok(img) = image::load_from_memory(bytes) else { return Tint::NEUTRAL };
    from_rgba(img.to_rgba8().as_raw(), 4)
}

/// `stride` is bytes per pixel (4 for RGBA).
///
/// Hue is averaged as a CIRCLE, not as a number. Averaging 0.02 and 0.98
/// arithmetically gives 0.5 — cyan from two reds. Summing unit vectors and
/// taking the angle back is the only correct way, and the bug it prevents is
/// invisible in code review.
pub fn from_rgba(px: &[u8], stride: usize) -> Tint {
    let (mut x, mut y, mut sat_sum, mut weight) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    for p in px.chunks_exact(stride) {
        let a = if stride == 4 { p[3] as f32 / 255.0 } else { 1.0 };
        // Transparent corners are most of an app icon; counting them drags
        // every tint toward the same grey.
        if a < 0.5 { continue; }
        let (h, s, l) = rgb_to_hsl(p[0], p[1], p[2]);
        // Near-black and near-white carry no hue worth having, and icons are
        // full of both (outlines, highlights).
        if !(0.12..=0.92).contains(&l) || s < 0.15 { continue; }
        // Weight by saturation: one vivid pixel says more about an icon's
        // colour than ten washed-out ones.
        let w = s * a;
        let ang = h * std::f32::consts::TAU;
        x += ang.cos() * w;
        y += ang.sin() * w;
        sat_sum += s * w;
        weight += w;
    }
    if weight <= 0.0 { return Tint::NEUTRAL; }
    let hue = {
        let a = y.atan2(x) / std::f32::consts::TAU;
        if a < 0.0 { a + 1.0 } else { a }
    };
    Tint { hue, sat: (sat_sum / weight).clamp(0.0, 1.0) }
}

fn rgb_to_hsl(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d <= f32::EPSILON { return (0.0, 0.0, l); }
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == r {
        ((g - b) / d + if g < b { 6.0 } else { 0.0 }) / 6.0
    } else if max == g {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };
    (h, s, l)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(r: u8, g: u8, b: u8, n: usize) -> Vec<u8> {
        [r, g, b, 255].repeat(n)
    }

    #[test]
    fn a_solid_colour_yields_its_own_hue() {
        // Pure red is hue 0.
        let t = from_rgba(&solid(220, 30, 30, 64), 4);
        assert!(t.hue < 0.03 || t.hue > 0.97, "red should stay red: {t:?}");
        assert!(t.sat > 0.5, "{t:?}");

        // Pure blue is hue 2/3.
        let t = from_rgba(&solid(30, 60, 220, 64), 4);
        assert!((t.hue - 0.666).abs() < 0.05, "{t:?}");
    }

    /// The bug this whole module is shaped around: two reds either side of the
    /// wrap point must average to red, not to cyan.
    #[test]
    fn hue_averages_around_the_circle() {
        let mut px = solid(255, 0, 10, 32);   // hue ~0.99
        px.extend(solid(255, 10, 0, 32));      // hue ~0.01
        let t = from_rgba(&px, 4);
        assert!(t.hue < 0.06 || t.hue > 0.94,
                "wrapped hues must average to red, got {t:?}");
    }

    /// A greyscale icon must come back neutral. Inventing a hue for it puts a
    /// confident wrong colour on the card.
    #[test]
    fn greyscale_is_neutral() {
        assert_eq!(from_rgba(&solid(128, 128, 128, 64), 4), Tint::NEUTRAL);
        assert_eq!(from_rgba(&solid(255, 255, 255, 64), 4), Tint::NEUTRAL);
        assert_eq!(from_rgba(&solid(0, 0, 0, 64), 4), Tint::NEUTRAL);
    }

    /// Transparent padding is most of an app icon; it must not count.
    #[test]
    fn transparent_pixels_are_skipped() {
        let mut px = [0u8, 0, 255, 0].repeat(200);   // transparent blue
        px.extend(solid(220, 30, 30, 8));            // a little opaque red
        let t = from_rgba(&px, 4);
        assert!(t.hue < 0.05 || t.hue > 0.95,
                "transparent blue must not win: {t:?}");
    }

    #[test]
    fn empty_input_is_neutral() {
        assert_eq!(from_rgba(&[], 4), Tint::NEUTRAL);
        assert_eq!(from_bytes(b"not an image"), Tint::NEUTRAL);
    }

    /// The wash sits behind text, so it must stay dark and never fully
    /// saturated whatever the icon looks like.
    #[test]
    fn the_wash_stays_readable() {
        let vivid = Tint { hue: 0.0, sat: 1.0 };
        let w = vivid.wash(1.0);
        assert!(w.l <= 0.35, "too light behind text: {w:?}");
        assert!(w.s <= 0.5, "too saturated behind text: {w:?}");
    }
}
