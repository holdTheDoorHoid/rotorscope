//! Colours. Every distinction that matters is also carried by shape or text
//! (filled vs hollow, a letter, an inverse box), so these can be swapped for
//! a monochrome or red-only palette without losing information.

use embedded_graphics::pixelcolor::Rgb565;

use crate::ui::Theme;

/// 8-bit-per-channel colour, quantised to RGB565.
pub const fn rgb(r: u8, g: u8, b: u8) -> Rgb565 {
    Rgb565::new(r >> 3, g >> 2, b >> 3)
}

/// The 320x240 screen's colours for one [`Theme`].
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
    /// Cautions: no fix, dead nodes, warning boxes, the caveats strip.
    pub warn: Rgb565,
    /// Text drawn on `warn`.
    pub warn_ink: Rgb565,
    /// Operator (pilot) markers.
    pub operator: Rgb565,
    /// The recording dot (always next to the word REC).
    pub rec: Rgb565,
    /// Status bar background.
    pub bar: Rgb565,
}

/// Glass Cockpit: cyan labels, white data, magenta drones, amber cautions.
pub const GLASS_COCKPIT: Palette = Palette {
    bg: rgb(4, 7, 12),
    fg: rgb(255, 255, 255),
    dim: rgb(70, 200, 240),
    stale: rgb(110, 128, 144),
    grid: rgb(29, 56, 80),
    accent: rgb(255, 79, 216),
    accent_ink: rgb(10, 0, 16),
    warn: rgb(255, 179, 0),
    warn_ink: rgb(0, 0, 0),
    // Lavender: the pilot is never green (green means healthy).
    operator: rgb(201, 160, 255),
    rec: rgb(255, 59, 59),
    bar: rgb(10, 18, 32),
};

/// Field Day: dark ink on light grey for direct sun, dark orange drones,
/// amber cautions with black ink (kept apart from the drones' orange).
pub const FIELD_DAY: Palette = Palette {
    bg: rgb(233, 237, 241),
    fg: rgb(11, 15, 20),
    dim: rgb(74, 85, 99),
    stale: rgb(118, 128, 142),
    grid: rgb(170, 180, 193),
    accent: rgb(194, 65, 12),
    accent_ink: rgb(255, 255, 255),
    warn: rgb(242, 178, 0),
    warn_ink: rgb(11, 15, 20),
    operator: rgb(0, 80, 179),
    rec: rgb(200, 16, 46),
    bar: rgb(213, 219, 226),
};

/// Red Night: pure red only (no green or blue at all, which is what costs
/// night vision). Alerts are inverse boxes and shapes, never a different
/// colour; dim text stays at least 3:1 against black.
pub const RED_NIGHT: Palette = Palette {
    bg: rgb(0, 0, 0),
    fg: rgb(255, 0, 0),
    dim: rgb(190, 0, 0),
    stale: rgb(150, 0, 0),
    grid: rgb(96, 0, 0),
    accent: rgb(255, 0, 0),
    accent_ink: rgb(0, 0, 0),
    warn: rgb(255, 0, 0),
    warn_ink: rgb(0, 0, 0),
    operator: rgb(255, 0, 0),
    rec: rgb(255, 0, 0),
    bar: rgb(24, 0, 0),
};

impl Theme {
    pub const fn palette(self) -> &'static Palette {
        match self {
            Theme::GlassCockpit => &GLASS_COCKPIT,
            Theme::FieldDay => &FIELD_DAY,
            Theme::RedNight => &RED_NIGHT,
        }
    }
}
