//! The native 800x480 layout (the P4 hub's panel), feature `wide`.
//!
//! The same pages, modes and overlays as the 320x240 [`crate::render`],
//! laid out for the full width with anti-aliased lettering
//! ([`font`], rasterised from B612 / B612 Mono and IBM Plex Sans Condensed
//! by `tools/fontgen`), in the three themes ([`look`]). Along the bottom,
//! three context keys say what each touch key does on the screen showing
//! ([`key_labels`]); [`key_at`] hit-tests a touch against them, and
//! [`tap_target`] a touch on the page (drones, list rows, flag rows,
//! settings rows, the banner) against what the layout drew.
//!
//! Drawing is banded like the 320x240 screen ([`render_banded`]): each band
//! re-runs the layout and keeps only its rows, and every primitive rejects
//! what lies outside the band before touching pixels.
//!
//! ```text
//!   0 +--------------------------------------------------+
//!     | status bar: GPS, every warning as a chip, nodes  |  38
//!  38 +--------------------------------------------------+
//!     | warning band (only while a warning holds)        |  +34
//!     +--------------------------------------------------+
//!     | page (title block, then content)                 |
//!     |                                                  |
//!     | (fault bar and notes lines, when there are any)  |
//! 410 +--------------------------------------------------+
//!     | [ key: Up ]      [ key: Select ]    [ key: Down ]|  414..476
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
use crate::ui::{ActionState, Button, Mode, Page, Request, SettingsRow, Tap, Ui};

/// Panel size.
pub const W: i32 = 800;
pub const H: i32 = 480;
/// Status bar height.
pub const BAR_H: i32 = 38;
/// The warning band under the status bar, while any warning holds.
pub const BAND_H: i32 = 34;
/// Top of the key strip; pages end above it. Touches from here down land
/// on a key; above it, on the page.
pub const KEYS_TOP: i32 = 410;
/// The keys themselves: they fill the touch zone (gloved fingers).
pub const KEY_Y: i32 = 414;
pub const KEY_H: i32 = 62;
const KEY_GAP: i32 = 8;
const KEY_X0: i32 = 16;
const KEY_W: i32 = (W - 2 * KEY_X0 - 2 * KEY_GAP) / 3;
/// Touches this low or lower land on a key.
pub const TOUCH_TOP: i32 = KEYS_TOP;
/// Rows per band the P4 draws: 800 x 48 x 2 bytes = 75 KB, 10 bands.
pub const BAND_ROWS: usize = 48;
/// One fault bar (evidence not written; the keyed link's problems) above
/// the keys.
pub const CAVEAT_H: i32 = 22;
/// One muted notes line above the keys.
pub const NOTE_H: i32 = 20;
/// Holding the middle key this long is "hold" (Back). The key fills up
/// while it is held ([`Ui::set_held`]).
pub const HOLD_MS: u32 = 1_000;

/// The key a touch at panel (x, y) presses: the bottom strip split in
/// thirds edge to edge (no dead gaps between keys), Up, Select, Down left to
/// right. `None` for a touch in the page (see [`tap_target`]).
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
    /// A second, smaller line under the word, always in the same place:
    /// what holding does ("hold: back to radar"), what a tap does when it
    /// is the same ("tap or hold"), or what a confirm will do.
    pub sub: Option<String<32>>,
    /// Pressing it does something (an inactive key is drawn quieter).
    pub active: bool,
    pub arrow: Option<Arrow>,
    /// The page's main action (drawn in the accent); never for a key that
    /// only goes back.
    pub primary: bool,
    /// A finished action: a check mark after the word.
    pub check: bool,
}

impl KeyLabel {
    fn new(text: &str, active: bool) -> Self {
        let mut t = String::new();
        for c in text.chars() {
            if t.push(c).is_err() {
                break;
            }
        }
        Self { text: t, sub: None, active, arrow: None, primary: false, check: false }
    }
    fn arrow(mut self, a: Arrow) -> Self {
        self.arrow = Some(a);
        self
    }
    fn sub(mut self, h: &str) -> Self {
        let mut t: String<32> = String::new();
        for c in h.chars() {
            if t.push(c).is_err() {
                break;
            }
        }
        self.sub = Some(t);
        self
    }
    fn primary(mut self) -> Self {
        self.primary = true;
        self
    }
}

/// The three keys' labels for what is on screen now. Each names what the
/// state machine ([`Ui::button`]) really does with that key here.
pub fn key_labels(ui: &Ui, s: &UiState) -> [KeyLabel; 3] {
    // The banner takes the first press: Select opens the drone, the others
    // only dismiss it (so a press meant for the page underneath never does
    // something unexpected).
    if ui.alert().is_some() {
        let open = ui.alert_drone(s).is_some();
        let mid = if open { KeyLabel::new("Show details", true).primary() } else { KeyLabel::new("Dismiss", true) };
        return [KeyLabel::new("Dismiss", true), mid, KeyLabel::new("Dismiss", true)];
    }
    let back = |p: Page| -> String<32> { fmt(format_args!("hold: back to {}", p.name())) };
    match ui.mode() {
        Mode::Browse => {
            let page = |d| ui.page_after(d, s).title();
            let empty = s.order().is_empty();
            let mid = match ui.page() {
                Page::Radar if empty => KeyLabel::new("No drones yet", false),
                Page::Radar => KeyLabel::new("Choose drone", true).primary(),
                Page::List if empty => KeyLabel::new("No drones yet", false).sub(&back(Page::Radar)),
                Page::List => KeyLabel::new("Choose drone", true).primary().sub(&back(Page::Radar)),
                Page::Settings => KeyLabel::new("Edit settings", true).primary().sub(&back(Page::Radar)),
                Page::Flags if s.flags.as_ref().is_some_and(|f| !f.rows.is_empty()) => {
                    KeyLabel::new("Choose flag", true).primary().sub(&back(Page::Radar))
                }
                Page::Signals if ui.overflows(Page::Signals) => KeyLabel::new("Scroll list", true).primary().sub(&back(Page::Radar)),
                // No short-press job here: a tap does what holding does.
                Page::Flags | Page::Signals | Page::Health => KeyLabel::new("Back to radar", true).sub("tap or hold"),
            };
            [KeyLabel::new(page(-1), true).arrow(Arrow::Left), mid, KeyLabel::new(page(1), true).arrow(Arrow::Right)]
        }
        Mode::Pick => [
            KeyLabel::new("Previous", true).arrow(Arrow::Up),
            KeyLabel::new("Open", true).primary().sub("hold: stop choosing"),
            KeyLabel::new("Next", true).arrow(Arrow::Down),
        ],
        Mode::Detail => {
            let t: String<24> = fmt(format_args!("Back to {}", ui.page().name()));
            [
                KeyLabel::new("Prev drone", true).arrow(Arrow::Left),
                KeyLabel::new(&t, true).sub("tap or hold"),
                KeyLabel::new("Next drone", true).arrow(Arrow::Right),
            ]
        }
        Mode::Scroll if ui.page() == Page::Flags => {
            let row = s.flags.as_ref().and_then(|f| f.listed(usize::from(ui.scroll())));
            let open = row.and_then(|r| r.drone).is_some_and(|k| s.drone(k).is_some());
            let n = s.flags.as_ref().map_or(0, |f| f.rows.len());
            let mid = if open {
                KeyLabel::new("Open drone", true).primary()
            } else if row.is_some_and(|r| r.aircraft) {
                KeyLabel::new("A plane: no detail", false)
            } else {
                KeyLabel::new("Drone gone", false)
            };
            [
                KeyLabel::new("Previous", usize::from(ui.scroll()) > 0).arrow(Arrow::Up),
                mid.sub(&back(Page::Flags)),
                KeyLabel::new("Next", usize::from(ui.scroll()) + 1 < n).arrow(Arrow::Down),
            ]
        }
        Mode::Scroll => [
            KeyLabel::new("Scroll up", ui.scroll() > 0).arrow(Arrow::Up),
            KeyLabel::new("Done", true).sub("tap or hold"),
            KeyLabel::new("Scroll down", true).arrow(Arrow::Down),
        ],
        Mode::Edit if ui.adjusting() => {
            let b = ui.settings().brightness;
            [
                KeyLabel::new("Dimmer", b > 1).arrow(Arrow::Left),
                KeyLabel::new("Done", true).primary().sub("hold: finish"),
                KeyLabel::new("Brighter", b < 5).arrow(Arrow::Right),
            ]
        }
        Mode::Edit => {
            let row = ui.settings_row_kind();
            let n = ui.rows().len();
            let mid = edit_key(ui, s, row);
            [
                KeyLabel::new("Up", ui.settings_row() > 0).arrow(Arrow::Up),
                mid,
                KeyLabel::new("Down", ui.settings_row() + 1 < n).arrow(Arrow::Down),
            ]
        }
    }
}

/// The middle key while editing: what Select does on this row.
fn edit_key(ui: &Ui, s: &UiState, row: SettingsRow) -> KeyLabel {
    let finish = "hold: finish";
    if let Some(a) = ui.action().filter(|a| a.row == row) {
        let mut k = match a.state {
            ActionState::Pending => KeyLabel::new("Working...", false),
            ActionState::Done => {
                let w = match a.request {
                    Request::MuteAlarms { .. } => "Muted",
                    Request::SnoozeAlarms { .. } => "Snoozed",
                    Request::UnmuteAlarms => "Unmuted",
                    Request::HubMoved => "Learning",
                    Request::AcceptHome => "Spot in use",
                    Request::NodesAtHub => "Placed",
                };
                let mut k = KeyLabel::new(w, false);
                k.check = true;
                k
            }
            ActionState::Failed(_) => KeyLabel::new("Did not work", false),
        };
        k = if matches!(a.state, ActionState::Failed(_)) { k.sub("move off the row to retry") } else { k.sub(finish) };
        return k;
    }
    if ui.confirming() == Some(row) {
        let what: String<32> = match Ui::row_request(row, s) {
            Some(Request::NodesAtHub) => {
                let n = s.nodes.iter().filter(|n| n.kind.hears_drones()).count();
                fmt(format_args!("place {n} nodes here"))
            }
            Some(Request::HubMoved) => fmt(format_args!("forget the learned spot")),
            Some(Request::AcceptHome) => fmt(format_args!("use this spot as home")),
            _ => fmt(format_args!("")),
        };
        return KeyLabel::new("Press again to confirm", true).primary().sub(&what);
    }
    let t = s.threats;
    let k = match row {
        SettingsRow::HubMoved if s.gps.home_pending => KeyLabel::new("Use this spot", true),
        SettingsRow::HubMoved => KeyLabel::new("I moved the hub", true),
        SettingsRow::NodesAtHub => KeyLabel::new("Place all here", true),
        SettingsRow::Alarms if t.mutable > 0 => KeyLabel::new("Mute 2 h", true),
        SettingsRow::Alarms if t.snoozable > 0 => KeyLabel::new("Snooze 5 min", true),
        SettingsRow::Alarms if s.caveats.silenced.operator() > 0 => KeyLabel::new("Unmute", true),
        SettingsRow::Alarms => KeyLabel::new("None active", false),
        SettingsRow::Brightness => KeyLabel::new("Adjust", true),
        _ => KeyLabel::new("Change", true),
    };
    let active = k.active;
    let k = k.sub(finish);
    if active { k.primary() } else { k }
}

/// What the layout drew where, for [`tap_target`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Spot {
    /// A drone marker: centre and the radius a finger may miss it by.
    Circle(f32, f32, f32),
    Rect(i32, i32, i32, i32),
}

/// Spots the layout records while measuring (no pixels drawn).
pub(crate) type Hits = heapless::Vec<(Spot, Tap), 48>;

/// What a touch at panel (x, y) on the page landed on: the same layout
/// that draws the frame is run without pixels to find it. `None` on the
/// key strip (that is [`key_at`]). While the banner is up only the banner
/// counts ([`Tap::Banner`]; anywhere else [`Tap::Nothing`], which
/// dismisses it). Of several drone markers the nearest within reach wins.
pub fn tap_target(ui: &Ui, s: &UiState, x: i32, y: i32) -> Option<Tap> {
    if y >= TOUCH_TOP || !(0..W).contains(&x) || y < 0 {
        return None;
    }
    let mut hits = Hits::new();
    let mut none: [u16; 0] = [];
    let mut c = Canvas::new(&mut none, H);
    draw(ui, s, None, &mut c, Some(&mut hits));
    let (fx, fy) = (x as f32, y as f32);
    let mut best: Option<(f32, Tap)> = None;
    for (spot, tap) in &hits {
        if let Spot::Circle(cx, cy, r) = *spot {
            let d = libm::sqrtf((cx - fx) * (cx - fx) + (cy - fy) * (cy - fy));
            if d <= r && best.is_none_or(|b| d < b.0) {
                best = Some((d, *tap));
            }
        }
    }
    if let Some((_, t)) = best {
        return Some(t);
    }
    for (spot, tap) in hits.iter().rev() {
        if let Spot::Rect(rx, ry, rw, rh) = *spot
            && (rx..rx + rw).contains(&x)
            && (ry..ry + rh).contains(&y)
        {
            return Some(*tap);
        }
    }
    Some(Tap::Nothing)
}

/// The notes lines (standing caveats) as packed for the strip: up to two
/// lines, and how many notes did not fit.
pub(crate) fn note_lines(s: &UiState, f: &font::Font) -> (heapless::Vec<String<160>, 2>, usize) {
    let notes = s.caveats.notes();
    let mut lines: heapless::Vec<String<160>, 2> = heapless::Vec::new();
    let maxw = W - 2 * 16 - 24;
    let mut placed = 0;
    for n in &notes {
        let sep = "   \u{b7}   ";
        let fits_last = lines.last().is_some_and(|l| f.width(l, 0) + f.width(sep, 0) + f.width(n, 0) <= maxw - 150);
        if fits_last {
            let l = lines.last_mut().unwrap();
            let _ = l.push_str(sep);
            let _ = l.push_str(n);
        } else if lines.len() < 2 {
            let _ = lines.push(fmt(format_args!("{n}")));
        } else {
            break;
        }
        placed += 1;
    }
    (lines, notes.len() - placed)
}

/// Draw the part of one frame that falls in `c`'s band. `lit`: the key
/// being pressed (drawn pressed; Back lights Select, the key being held).
pub fn render(ui: &Ui, s: &UiState, lit: Option<Button>, c: &mut Canvas<'_>) {
    draw(ui, s, lit, c, None);
}

fn draw(ui: &Ui, s: &UiState, lit: Option<Button>, c: &mut Canvas<'_>, hits: Option<&mut Hits>) {
    let l = ui.effective_theme(s).look();
    c.clear(l.bg);
    let warns = s.warnings();
    let top = BAR_H + if warns.is_empty() { 0 } else { BAND_H };
    let faults = s.caveats.faults();
    let (notes, _) = note_lines(s, &l.fonts.small);
    // A threat level (tracker `behaviour`) is one more filled bar, above
    // the faults: it is about a drone now, they are about the unit.
    let threat = i32::from(s.threat_strip.is_some());
    let strip = (threat + faults.len() as i32) * CAVEAT_H + notes.len() as i32 * NOTE_H;
    let bottom = KEYS_TOP - strip;
    let mut p = pages::P { c, l, top, bottom, ui, hits };
    match ui.mode() {
        Mode::Detail => p.detail(s),
        _ => match ui.page() {
            Page::Radar if s.gps.has_position() => p.radar(s),
            Page::Radar => p.no_fix_home(s),
            Page::List => p.list_page(s),
            Page::Signals => p.signals(s),
            Page::Flags => p.flags(s),
            Page::Health => p.health(s),
            Page::Settings => p.settings(s),
        },
    }
    if ui.alert().is_some() {
        p.alert(s);
    }
    p.strip(s, &faults, &notes);
    p.band(s, &warns);
    p.status_bar(s, &warns);
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
