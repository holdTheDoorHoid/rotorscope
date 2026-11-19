//! The three themes at 800x480: colours, lettering and finishing touches.
//! Colours are the owner's chosen mockups (2026-11-18). Meaning never rests
//! on colour alone: drones are filled diamonds (hollow when stale), the
//! pilot an X, cautions inverse boxes with words, the selection a framed
//! row with a pointer.

use embedded_graphics::pixelcolor::Rgb565;

use super::font::FontSet;
use super::glyphs::{B612, PLEX};
use crate::theme::rgb;
use crate::ui::Theme;

/// How the context keys are drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyStyle {
    /// Outlined keys on the page background; the middle key in the accent.
    Outline,
    /// Filled dark keys on a dark strip (Field Day: the strip matches its
    /// dark status bar).
    Filled,
}

#[derive(Debug, Clone, Copy)]
pub struct Look {
    pub bg: Rgb565,
    /// Data and headings.
    pub fg: Rgb565,
    /// Field names and column heads only.
    pub label: Rgb565,
    /// Secondary text: notes, units, "+N more", degrees.
    pub dim: Rgb565,
    /// Stale things: readable, clearly quieter.
    pub faint: Rgb565,
    /// Rules and quiet structure.
    pub grid: Rgb565,
    /// Range rings and empty level boxes: at least 3:1 against `bg`.
    pub ring: Rgb565,
    /// Drones (and the new-drone banner).
    pub accent: Rgb565,
    pub accent_ink: Rgb565,
    pub operator: Rgb565,
    pub ok: Rgb565,
    /// Cautions: fills (warning band, boxes) and the ink on them.
    pub warn: Rgb565,
    pub warn_ink: Rgb565,
    /// Caution text drawn straight on `bg`.
    pub warn_text: Rgb565,
    /// Cards: fill (`None`: outline only) and edge.
    pub panel: Option<Rgb565>,
    pub edge: Rgb565,
    /// The selected row: fill and frame.
    pub sel_fill: Rgb565,
    pub sel_edge: Rgb565,
    /// Status bar.
    pub bar: Rgb565,
    pub bar_fg: Rgb565,
    pub bar_dim: Rgb565,
    pub bar_ok: Rgb565,
    pub bar_warn: Rgb565,
    /// Ink on a `bar_warn` chip.
    pub bar_warn_ink: Rgb565,
    pub bar_rec: Rgb565,
    /// Context keys.
    pub keys: KeyStyle,
    pub strip: Rgb565,
    pub key_fill: Rgb565,
    pub key_edge: Rgb565,
    pub key_text: Rgb565,
    /// A key being pressed: fill and ink (Field Day inverts to light, so a
    /// pressed key never looks switched off).
    pub press_fill: Rgb565,
    pub press_ink: Rgb565,
    /// Corner radius of cards and keys.
    pub radius: i32,
    /// Range rings dashed (on, off) or solid.
    pub dash: Option<(f32, f32)>,
    /// A single hue (Red Night): large areas (banner, warning band, faults,
    /// pressed keys, toasts) are outlined rather than filled, keeping
    /// inverse only for small titles and chips.
    pub one_hue: bool,
    pub fonts: &'static FontSet,
}

pub const GLASS_COCKPIT: Look = Look {
    bg: rgb(4, 7, 12),
    fg: rgb(255, 255, 255),
    label: rgb(70, 200, 240),
    dim: rgb(143, 163, 181),
    faint: rgb(110, 128, 144),
    grid: rgb(29, 56, 80),
    ring: rgb(58, 106, 144),
    accent: rgb(255, 79, 216),
    accent_ink: rgb(10, 0, 16),
    // Lavender: the pilot is never green (green is node health only).
    operator: rgb(201, 160, 255),
    ok: rgb(61, 255, 122),
    warn: rgb(255, 179, 0),
    warn_ink: rgb(0, 0, 0),
    warn_text: rgb(255, 179, 0),
    panel: Some(rgb(7, 16, 28)),
    edge: rgb(29, 56, 80),
    sel_fill: rgb(20, 36, 58),
    sel_edge: rgb(255, 79, 216),
    bar: rgb(10, 18, 32),
    bar_fg: rgb(216, 243, 255),
    bar_dim: rgb(70, 200, 240),
    bar_ok: rgb(61, 255, 122),
    bar_warn: rgb(255, 179, 0),
    bar_warn_ink: rgb(0, 0, 0),
    bar_rec: rgb(255, 59, 59),
    keys: KeyStyle::Outline,
    strip: rgb(4, 7, 12),
    key_fill: rgb(7, 16, 28),
    key_edge: rgb(70, 200, 240),
    key_text: rgb(255, 255, 255),
    press_fill: rgb(255, 255, 255),
    press_ink: rgb(4, 7, 12),
    radius: 4,
    dash: None,
    one_hue: false,
    fonts: &B612,
};

pub const FIELD_DAY: Look = Look {
    bg: rgb(233, 237, 241),
    fg: rgb(11, 15, 20),
    label: rgb(74, 85, 99),
    dim: rgb(74, 85, 99),
    faint: rgb(118, 128, 142),
    grid: rgb(170, 180, 193),
    ring: rgb(118, 128, 142),
    // Drones a darker orange, kept apart from the amber cautions.
    accent: rgb(194, 65, 12),
    accent_ink: rgb(255, 255, 255),
    operator: rgb(0, 80, 179),
    ok: rgb(15, 123, 60),
    warn: rgb(242, 178, 0),
    warn_ink: rgb(11, 15, 20),
    warn_text: rgb(146, 64, 14),
    panel: Some(rgb(255, 255, 255)),
    edge: rgb(201, 208, 217),
    sel_fill: rgb(255, 255, 255),
    sel_edge: rgb(11, 15, 20),
    bar: rgb(29, 36, 48),
    bar_fg: rgb(255, 255, 255),
    bar_dim: rgb(154, 166, 182),
    bar_ok: rgb(74, 222, 128),
    bar_warn: rgb(251, 191, 36),
    bar_warn_ink: rgb(11, 15, 20),
    bar_rec: rgb(255, 77, 94),
    keys: KeyStyle::Filled,
    strip: rgb(29, 36, 48),
    key_fill: rgb(44, 53, 66),
    key_edge: rgb(44, 53, 66),
    key_text: rgb(255, 255, 255),
    press_fill: rgb(233, 237, 241),
    press_ink: rgb(11, 15, 20),
    radius: 6,
    dash: None,
    one_hue: false,
    fonts: &PLEX,
};

/// Pure red only: every colour is (R, 0, 0). Dim text stays at least 3:1
/// against black, faint about 2.3:1, rings 3:1.
pub const RED_NIGHT: Look = Look {
    bg: rgb(0, 0, 0),
    fg: rgb(255, 0, 0),
    label: rgb(190, 0, 0),
    dim: rgb(190, 0, 0),
    faint: rgb(150, 0, 0),
    grid: rgb(90, 0, 0),
    ring: rgb(186, 0, 0),
    accent: rgb(255, 0, 0),
    accent_ink: rgb(0, 0, 0),
    operator: rgb(255, 0, 0),
    ok: rgb(190, 0, 0),
    warn: rgb(255, 0, 0),
    warn_ink: rgb(0, 0, 0),
    warn_text: rgb(255, 0, 0),
    panel: None,
    edge: rgb(90, 0, 0),
    sel_fill: rgb(40, 0, 0),
    sel_edge: rgb(255, 0, 0),
    bar: rgb(24, 0, 0),
    bar_fg: rgb(230, 0, 0),
    bar_dim: rgb(190, 0, 0),
    bar_ok: rgb(190, 0, 0),
    bar_warn: rgb(255, 0, 0),
    bar_warn_ink: rgb(0, 0, 0),
    bar_rec: rgb(255, 0, 0),
    keys: KeyStyle::Outline,
    strip: rgb(0, 0, 0),
    key_fill: rgb(0, 0, 0),
    key_edge: rgb(190, 0, 0),
    key_text: rgb(255, 0, 0),
    press_fill: rgb(40, 0, 0),
    press_ink: rgb(255, 0, 0),
    radius: 2,
    dash: None,
    one_hue: true,
    fonts: &B612,
};

impl Theme {
    /// This theme at 800x480.
    pub const fn look(self) -> &'static Look {
        match self {
            Theme::GlassCockpit => &GLASS_COCKPIT,
            Theme::FieldDay => &FIELD_DAY,
            Theme::RedNight => &RED_NIGHT,
        }
    }
}
