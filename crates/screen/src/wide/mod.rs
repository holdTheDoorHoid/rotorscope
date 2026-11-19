//! The native 800x480 layout (the P4 hub's panel), feature `wide`.
//!
//! The same pages, modes and overlays as the 320x240 [`crate::render`],
//! laid out for the full width with anti-aliased lettering
//! ([`font`], rasterised from B612 / B612 Mono and IBM Plex Sans Condensed
//! by `tools/fontgen`), in the three themes ([`look`]). Along the bottom,
//! three context keys say what each touch key does on the screen showing
//! ([`key_labels`]); [`key_at`] hit-tests a touch against them.
//!
//! Drawing is banded like the 320x240 screen ([`render_banded`]): each band
//! re-runs the layout and keeps only its rows, and every primitive rejects
//! what lies outside the band before touching pixels.
//!
//! ```text
//!   0 +--------------------------------------------------+
//!     | status bar                                       |  38
//!  38 +--------------------------------------------------+
//!     | page                                             |
//!     |                                                  |
//!     | (caveats strip, when there is one)               |
//! 428 +--------------------------------------------------+
//!     | [ key: Up ]      [ key: Select ]    [ key: Down ]|  436..474
//! 480 +--------------------------------------------------+
//! ```

pub mod canvas;
pub mod font;
mod glyphs;
pub mod look;
mod pages;

use core::fmt::Write;

use embedded_graphics::pixelcolor::Rgb565;
use heapless::String;

pub use canvas::Canvas;
pub use look::Look;

use crate::model::UiState;
use crate::ui::{Button, Mode, Page, SettingsRow, Ui};

/// Panel size.
pub const W: i32 = 800;
pub const H: i32 = 480;
/// Status bar height.
pub const BAR_H: i32 = 38;
/// Top of the key strip; pages end above it.
pub const KEYS_TOP: i32 = 428;
/// The keys themselves.
pub const KEY_Y: i32 = 436;
pub const KEY_H: i32 = 38;
const KEY_GAP: i32 = 8;
const KEY_X0: i32 = 16;
const KEY_W: i32 = (W - 2 * KEY_X0 - 2 * KEY_GAP) / 3;
/// Touches this low or lower land on a key (generous: 12 px above the
/// strip as well). Touches in the page above are ignored.
pub const TOUCH_TOP: i32 = KEYS_TOP - 12;
/// Rows per band the P4 draws: 800 x 48 x 2 bytes = 75 KB, 10 bands.
pub const BAND_ROWS: usize = 48;
/// Height of one caveat line above the keys.
pub const CAVEAT_H: i32 = 22;

/// The key a touch at panel (x, y) presses: the bottom strip split in
/// thirds edge to edge (no dead gaps between keys), Up, Select, Down left to
/// right. `None` for a touch in the page.
pub fn key_at(x: i32, y: i32) -> Option<Button> {
    if !(TOUCH_TOP..H).contains(&y) || !(0..W).contains(&x) {
        return None;
    }
    Some(match x * 3 / W {
        0 => Button::Up,
        1 => Button::Select,
        _ => Button::Down,
    })
}

/// The panel rectangle of key `i` (0 left .. 2 right): x, y, w, h.
pub const fn key_rect(i: usize) -> (i32, i32, i32, i32) {
    (KEY_X0 + i as i32 * (KEY_W + KEY_GAP), KEY_Y, KEY_W, KEY_H)
}

/// A small arrow drawn beside a key's word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrow {
    Left,
    Right,
    Up,
    Down,
}

/// What one context key says.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyLabel {
    pub text: String<24>,
    /// A second, smaller line: what holding the key does.
    pub hold: Option<&'static str>,
    /// Pressing it does something (an inactive key is drawn quieter).
    pub active: bool,
    pub arrow: Option<Arrow>,
}

impl KeyLabel {
    fn new(text: &str, active: bool) -> Self {
        let mut t = String::new();
        for c in text.chars() {
            if t.push(c).is_err() {
                break;
            }
        }
        Self { text: t, hold: None, active, arrow: None }
    }
    fn arrow(mut self, a: Arrow) -> Self {
        self.arrow = Some(a);
        self
    }
    fn hold(mut self, h: &'static str) -> Self {
        self.hold = Some(h);
        self
    }
}

/// The three keys' labels for what is on screen now. Each names what the
/// state machine ([`Ui::button`]) really does with that key here.
pub fn key_labels(ui: &Ui, s: &UiState) -> [KeyLabel; 3] {
    // The banner takes the first press: Select opens the drone, the others
    // only dismiss it (so a press meant for the page underneath never does
    // something unexpected).
    if let Some(a) = ui.alert() {
        let open = s.drone(a.key).is_some();
        return [
            KeyLabel::new("Dismiss", true),
            if open { KeyLabel::new("Show details", true) } else { KeyLabel::new("Dismiss", true) },
            KeyLabel::new("Dismiss", true),
        ];
    }
    match ui.mode() {
        Mode::Browse => {
            let page = |d| ui.page_after(d, s).title();
            let mid = match ui.page() {
                Page::Radar | Page::List if s.order().is_empty() => KeyLabel::new("No drones yet", false),
                Page::Radar => KeyLabel::new("Choose drone", true),
                Page::List => KeyLabel::new("Choose drone", true).hold("hold: radar"),
                Page::Settings => KeyLabel::new("Edit settings", true).hold("hold: radar"),
                Page::Flags | Page::Signals | Page::Health => KeyLabel::new("Hold: radar", true),
            };
            [KeyLabel::new(page(-1), true).arrow(Arrow::Left), mid, KeyLabel::new(page(1), true).arrow(Arrow::Right)]
        }
        Mode::Pick => [
            KeyLabel::new("Previous", true).arrow(Arrow::Up),
            KeyLabel::new("Open", true).hold("hold: back"),
            KeyLabel::new("Next", true).arrow(Arrow::Down),
        ],
        Mode::Detail => [
            KeyLabel::new("Prev drone", true).arrow(Arrow::Left),
            KeyLabel::new("Hold: back", true),
            KeyLabel::new("Next drone", true).arrow(Arrow::Right),
        ],
        Mode::Edit => {
            let row = ui.settings_row_kind();
            let mut v: String<40> = String::new();
            crate::render::settings_text(ui, s, row, false, &mut v);
            let mid = match row {
                SettingsRow::HubMoved | SettingsRow::NodesAtHub if ui.done_row() == Some(row) => KeyLabel::new("Done", false),
                SettingsRow::HubMoved | SettingsRow::NodesAtHub => KeyLabel::new(&v, true),
                SettingsRow::Alarms if s.threats.mutable == 0 && s.threats.snoozable == 0 && s.caveats.silenced.operator() == 0 => {
                    KeyLabel::new("Nothing to do", false)
                }
                SettingsRow::Alarms if ui.done_row() == Some(row) => KeyLabel::new("Done", false),
                SettingsRow::Alarms => KeyLabel::new(&v, true),
                _ => KeyLabel::new("Change", true),
            };
            [KeyLabel::new("Up", true).arrow(Arrow::Up), mid.hold("hold: done"), KeyLabel::new("Down", true).arrow(Arrow::Down)]
        }
    }
}

/// Draw the part of one frame that falls in `c`'s band. `lit`: the key
/// being pressed (drawn pressed; Back lights Select, the key being held).
pub fn render(ui: &Ui, s: &UiState, lit: Option<Button>, c: &mut Canvas<'_>) {
    let l = ui.effective_theme(s).look();
    c.clear(l.bg);
    let caveats = s.caveats.lines();
    let n = caveats.iter().flatten().count() as i32;
    let bottom = KEYS_TOP - n * CAVEAT_H;
    let mut p = pages::P { c, l, bottom };
    match ui.mode() {
        Mode::Detail => p.detail(ui, s),
        _ => match ui.page() {
            Page::Radar if s.gps.has_position() => p.radar(ui, s),
            Page::Radar => p.no_fix_home(ui, s),
            Page::List => p.list_page(ui, s),
            Page::Signals => p.signals(s),
            Page::Flags => p.flags(s),
            Page::Health => p.health(s),
            Page::Settings => p.settings(ui, s),
        },
    }
    if let Some(a) = ui.alert() {
        p.alert(a.key, a.more, s);
    } else if ui.page_toast() {
        p.toast(ui.page().title());
    }
    p.caveats(&caveats);
    p.status_bar(s);
    let lit = lit.map(|b| if b == Button::Back { Button::Select } else { b });
    p.keys(&key_labels(ui, s), lit);
}

/// Render a whole frame band by band through `band` (whole rows of 800),
/// handing each finished band to `flush(top_row, pixels)`.
pub fn render_banded<E>(
    ui: &Ui,
    s: &UiState,
    lit: Option<Button>,
    band: &mut [u16],
    mut flush: impl FnMut(i32, &[u16]) -> Result<(), E>,
) -> Result<(), E> {
    let mut top = 0;
    while top < H {
        let mut c = Canvas::new(band, top);
        if c.rows() == 0 {
            break;
        }
        render(ui, s, lit, &mut c);
        let rows = c.rows();
        flush(top, c.pixels())?;
        top += rows;
    }
    Ok(())
}

/// A short formatted string.
pub(crate) fn fmt<const N: usize>(args: core::fmt::Arguments<'_>) -> String<N> {
    let mut s = String::new();
    let _ = s.write_fmt(args);
    s
}

/// Colour helper for the pages: `fg` unless stale.
pub(crate) fn ink(l: &Look, stale: bool) -> Rgb565 {
    if stale { l.faint } else { l.fg }
}
