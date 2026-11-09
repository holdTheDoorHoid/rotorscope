//! Colours. Every distinction that matters is also carried by shape or text
//! (filled vs hollow, a letter, an inverse box), so these can be swapped for
//! a monochrome or red-only palette without losing information.

use embedded_graphics::pixelcolor::Rgb565;

use crate::ui::Theme;

/// 8-bit-per-channel colour, quantised to RGB565.
pub const fn rgb(r: u8, g: u8, b: u8) -> Rgb565 {
    Rgb565::new(r >> 3, g >> 2, b >> 3)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub bg: Rgb565,
    /// Primary text and markers.
    pub fg: Rgb565,
    /// Labels and secondary text.
    pub dim: Rgb565,
    /// Stale drones: still readable, clearly quieter.
    pub stale: Rgb565,
    /// Range rings and rules.
    pub grid: Rgb565,
    /// Drones, and the new-drone banner background.
    pub accent: Rgb565,
    /// Text drawn on `accent`.
    pub accent_ink: Rgb565,
    /// Operator (pilot) markers.
    pub operator: Rgb565,
    /// The recording dot (always next to the word REC).
    pub rec: Rgb565,
    /// Status bar background.
    pub bar: Rgb565,
}

pub const NIGHT: Palette = Palette {
    bg: rgb(0, 0, 0),
    fg: rgb(255, 255, 255),
    dim: rgb(170, 170, 170),
    stale: rgb(120, 120, 120),
    grid: rgb(70, 82, 96),
    accent: rgb(255, 184, 0),
    accent_ink: rgb(0, 0, 0),
    operator: rgb(80, 200, 255),
    rec: rgb(255, 64, 64),
    bar: rgb(24, 28, 34),
};

pub const DAY: Palette = Palette {
    bg: rgb(255, 255, 255),
    fg: rgb(0, 0, 0),
    dim: rgb(72, 72, 72),
    stale: rgb(130, 130, 130),
    grid: rgb(160, 170, 182),
    accent: rgb(176, 64, 0),
    accent_ink: rgb(255, 255, 255),
    operator: rgb(0, 84, 180),
    rec: rgb(200, 0, 0),
    bar: rgb(226, 230, 236),
};

impl Theme {
    pub const fn palette(self) -> &'static Palette {
        match self {
            Theme::Night => &NIGHT,
            Theme::Day => &DAY,
        }
    }
}
