//! Colour and spacing tokens.
//!
//! One dark palette, no light mode. This is a window you keep open next to a
//! game; a light theme would be the wrong tool and a second palette to keep
//! honest for no one's benefit.

use gpui::Hsla;

/// h in turns (0..1), s/l/a in 0..1.
///
/// A struct literal rather than `hsla()`: that helper is not const, and these
/// tokens should be compile-time constants so a typo is a build error rather
/// than a colour nobody notices.
const fn c(h: f32, s: f32, l: f32, a: f32) -> Hsla {
    Hsla { h, s, l, a }
}

pub struct Theme {
    /// Window background — the deepest plane.
    pub bg: Hsla,
    /// Sidebar / header. One step up from `bg`: chrome recedes from content.
    pub surface: Hsla,
    /// Cards and pills that sit proud of the panel.
    pub raised: Hsla,
    pub border: Hsla,

    pub text: Hsla,
    pub text_muted: Hsla,
    pub text_faint: Hsla,

    pub accent: Hsla,
    pub ok: Hsla,
    pub warn: Hsla,
    pub bad: Hsla,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            bg: c(0.62, 0.10, 0.05, 1.0),
            surface: c(0.62, 0.09, 0.08, 1.0),
            raised: c(0.62, 0.08, 0.12, 1.0),
            border: c(0.62, 0.08, 0.18, 1.0),

            text: c(0.62, 0.06, 0.92, 1.0),
            text_muted: c(0.62, 0.05, 0.62, 1.0),
            text_faint: c(0.62, 0.05, 0.42, 1.0),

            accent: c(0.58, 0.85, 0.62, 1.0),
            ok: c(0.38, 0.62, 0.52, 1.0),
            warn: c(0.11, 0.85, 0.60, 1.0),
            bad: c(0.99, 0.70, 0.58, 1.0),
        }
    }
}

/// Spacing scale. Named so a reviewer can see when a number is off-grid.
pub const S1: f32 = 4.0;
pub const S2: f32 = 8.0;
pub const S3: f32 = 12.0;
pub const S4: f32 = 16.0;
pub const S6: f32 = 24.0;

pub const RADIUS: f32 = 10.0;
pub const SIDEBAR_W: f32 = 196.0;
pub const HEADER_H: f32 = 48.0;
