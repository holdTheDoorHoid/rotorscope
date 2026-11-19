//! Page layouts at 800x480.
//!
//! Every page starts at `top` (under the status bar, and under the warning
//! band while one holds) and ends at `bottom` (above the fault and notes
//! lines and the keys). While measuring for [`super::tap_target`] the
//! canvas has no rows and `hits` collects where tappable things are.

use embedded_graphics::pixelcolor::Rgb565;
use heapless::String;

use super::canvas::Canvas;
use super::font::{Font, FontSet};
use super::look::{KeyStyle, Look};
use super::{Arrow, BAND_H, BAR_H, CAVEAT_H, H, HOLD_MS, Hits, KEYS_TOP, KeyLabel, NOTE_H, Spot, W, fmt, ink, key_rect};
use crate::draw::{age, compass, degrees, distance, distance_short, metres, short_id, uptime};
use crate::model::*;
use crate::ui::{ActionState, AutoStatus, Button, Mode, Page, SettingsRow, Tap, ThemeMode, TitleToast, Ui};

/// Range rings, metres. The outer one is the plot's edge.
const RINGS: [(f32, &str); 4] = [(100.0, "100 m"), (300.0, "300 m"), (1000.0, "1 km"), (3000.0, "3 km")];
const LOG_K: f32 = 30.0;
/// Page margin.
const M: i32 = 18;
/// Touch rows are at least this tall.
const ROW_H: i32 = 44;
/// A finger may miss a drone marker by this much.
const REACH: f32 = 26.0;

/// One band's drawing: the canvas, the theme, where the page starts and
/// ends, the state machine (for modes, toasts, NEW marks), and, while
/// measuring for a touch, where the tappable things are.
pub(crate) struct P<'x, 'a> {
    pub c: &'x mut Canvas<'a>,
    pub l: &'static Look,
    pub top: i32,
    pub bottom: i32,
    pub ui: &'x Ui,
    pub hits: Option<&'x mut Hits>,
}

/// Uppercase copy (labels are set in capitals).
fn upper<const N: usize>(s: &str) -> String<N> {
    let mut o = String::new();
    for ch in s.chars() {
        if o.push(ch.to_ascii_uppercase()).is_err() {
            break;
        }
    }
    o
}

/// A radio's name for sentences ("Wi-Fi", "BT LR").
fn radio_name(r: Radio) -> &'static str {
    match r {
        Radio::WifiBeacon => "Wi-Fi",
        Radio::WifiNan => "Wi-Fi NAN",
        Radio::Ble => "BT",
        Radio::BleLongRange => "BT LR",
        Radio::Wifi5 => "5 GHz",
        Radio::DroneId => "DroneID",
    }
}

/// "Wi-Fi, BT LR"; letters ("W L") when that does not fit in `max` chars.
fn radio_words(r: Radios, max: usize) -> String<40> {
    let mut s: String<40> = String::new();
    for x in Radio::ALL.iter().filter(|x| r.contains(**x)) {
        if !s.is_empty() {
            let _ = s.push_str(", ");
        }
        let _ = s.push_str(radio_name(*x));
    }
    if s.len() > max {
        s.clear();
        for x in Radio::ALL.iter().filter(|x| r.contains(**x)) {
            if !s.is_empty() {
                let _ = s.push(' ');
            }
            let _ = s.push(x.letter());
        }
    }
    if s.is_empty() {
        let _ = s.push_str("--");
    }
    s
}

/// A drone's height in words: above you when the hub has a 3D fix (the
/// drone's altitude minus the hub's), else as the drone reports it.
fn height_words(dv: &DroneView) -> String<32> {
    match (dv.above_you_m, dv.height_m) {
        (Some(a), _) if a >= 0.0 => fmt(format_args!("{} m above you", libm::roundf(a) as i32)),
        (Some(a), _) => fmt(format_args!("{} m below you", -(libm::roundf(a) as i32))),
        (None, Some(_)) => fmt(format_args!(
            "{} above {}",
            metres(dv.height_m).as_str(),
            match dv.height_ref {
                HeightRef::Takeoff => "its takeoff",
                HeightRef::Ground => "ground",
            }
        )),
        (None, None) => fmt(format_args!("--")),
    }
}

/// Whether a control link's detail is raw stick values ("1500 1500 988"):
/// numbers only, which mean nothing to a reader.
fn raw_numbers(t: &str) -> bool {
    !t.is_empty() && t.chars().all(|c| c.is_ascii_digit() || c == ' ')
}

fn radius(m: f32, r: f32) -> f32 {
    let max = RINGS[RINGS.len() - 1].0;
    let f = libm::logf(1.0 + m.max(0.0) / LOG_K) / libm::logf(1.0 + max / LOG_K);
    f.min(1.0) * r
}

/// Unit vector for a compass bearing (north up, y down).
fn dir(deg: f32) -> (f32, f32) {
    let r = deg.to_radians();
    (libm::sinf(r), -libm::cosf(r))
}

/// RSSI as four signal bars' level (a dBm number means little to most).
fn bar_level(rssi: Option<i16>) -> i32 {
    match rssi {
        None => 0,
        Some(r) if r >= -60 => 4,
        Some(r) if r >= -70 => 3,
        Some(r) if r >= -80 => 2,
        Some(_) => 1,
    }
}

impl P<'_, '_> {
    fn f(&self) -> &'static FontSet {
        self.l.fonts
    }

    /// Record a tappable spot (measuring only).
    fn hit(&mut self, spot: Spot, tap: Tap) {
        if let Some(h) = self.hits.as_deref_mut() {
            let _ = h.push((spot, tap));
        }
    }

    // ------------------------------------------------------------ text --

    fn t(&mut self, s: &str, x: i32, y: i32, f: &Font, c: Rgb565) -> i32 {
        self.c.text(s, x, y, f, c, 0)
    }
    /// Right edge at `right`; returns the left x.
    fn tr(&mut self, s: &str, right: i32, y: i32, f: &Font, c: Rgb565) -> i32 {
        let x = right - f.width(s, 0);
        self.c.text(s, x, y, f, c, 0);
        x
    }
    fn tc(&mut self, s: &str, cx: i32, y: i32, f: &Font, c: Rgb565) {
        let x = cx - f.width(s, 0) / 2;
        self.c.text(s, x, y, f, c, 0);
    }
    /// `s` in the first of `fonts` it fits `maxw` in; cut short with ".."
    /// in the last one if none. Vertically centred on line `y`..`y + h`.
    #[allow(clippy::too_many_arguments)]
    fn t_fit(&mut self, s: &str, x: i32, y: i32, h: i32, maxw: i32, fonts: &[Font], c: Rgb565) -> i32 {
        let f = fonts.iter().find(|f| f.width(s, 0) <= maxw).copied();
        let (f, cut) = match f {
            Some(f) => (f, s.len()),
            None => {
                let f = fonts[fonts.len() - 1];
                let mut cut = s.len();
                while cut > 0 && (!s.is_char_boundary(cut) || f.width(&s[..cut], 0) + f.width("..", 0) > maxw) {
                    cut -= 1;
                }
                (f, cut)
            }
        };
        let ty = y + (h - i32::from(f.height)) / 2;
        let e = self.c.text(&s[..cut], x, ty, &f, c, 0);
        if cut < s.len() { self.c.text("..", e, ty, &f, c, 0) } else { e }
    }
    /// A label (field name, column head): capitals, spaced out, in the
    /// label face and colour.
    fn label(&mut self, s: &str, x: i32, y: i32) -> i32 {
        let u: String<48> = upper(s);
        let f = self.f().label;
        self.c.text(&u, x, y, &f, self.l.label, 1)
    }
    fn label_r(&mut self, s: &str, right: i32, y: i32) -> i32 {
        let u: String<48> = upper(s);
        let f = self.f().label;
        let x = right - f.width(&u, 1);
        self.c.text(&u, x, y, &f, self.l.label, 1);
        x
    }
    /// Text in an inverse box (`pad` around it). Returns the box's right
    /// edge.
    #[allow(clippy::too_many_arguments)]
    fn boxed(&mut self, s: &str, x: i32, y: i32, f: &Font, fg: Rgb565, bg: Rgb565, pad: i32) -> i32 {
        let w = f.width(s, 0) + 2 * pad;
        let h = i32::from(f.height) + pad;
        let r = self.l.radius.min(4);
        self.c.rrect(x, y, w, h, r, bg);
        self.c.text(s, x + pad, y + pad / 2, f, fg, 0);
        x + w
    }
    /// Text in an outlined box. Returns the box's right edge.
    #[allow(clippy::too_many_arguments)]
    fn outlined(&mut self, s: &str, x: i32, y: i32, f: &Font, c: Rgb565, pad: i32) -> i32 {
        let w = f.width(s, 0) + 2 * pad;
        let h = i32::from(f.height) + pad;
        self.c.rrect_stroke(x, y, w, h, self.l.radius.min(4), 1.5, c);
        self.c.text(s, x + pad, y + pad / 2, f, c, 0);
        x + w
    }
    /// A caution: inverse box in the warning colour, capitals.
    fn caution(&mut self, s: &str, x: i32, y: i32) -> i32 {
        let f = self.f().small_b;
        let u: String<40> = upper(s);
        self.boxed(&u, x, y, &f, self.l.warn_ink, self.l.warn, 6)
    }
    /// "NEW": a drone announced in the last minute (a banner missed still
    /// leaves a trace).
    fn new_chip(&mut self, x: i32, y: i32) -> i32 {
        let f = self.f().label;
        let w = f.width("NEW", 1) + 10;
        self.c.rrect(x, y, w, i32::from(f.height) + 2, 3, self.l.accent);
        self.c.text("NEW", x + 5, y + 1, &f, self.l.accent_ink, 1);
        x + w
    }
    /// Big numerals placed by the top of the digits.
    fn num(&mut self, s: &str, x: i32, cap_top: i32, f: &Font, c: Rgb565) -> i32 {
        let y = cap_top + i32::from(f.cap) - i32::from(f.ascent);
        self.c.text(s, x, y, f, c, 0)
    }
    /// A unit set on the baseline of big numerals placed at `cap_top`.
    fn unit_on(&mut self, s: &str, x: i32, cap_top: i32, big: &Font, f: &Font, c: Rgb565) -> i32 {
        let base = cap_top + i32::from(big.cap);
        self.c.text(s, x, base - i32::from(f.ascent), f, c, 0)
    }

    // ---------------------------------------------------------- shapes --

    fn card(&mut self, x: i32, y: i32, w: i32, h: i32) {
        if let Some(p) = self.l.panel {
            self.c.rrect(x, y, w, h, self.l.radius, p);
        }
        self.c.rrect_stroke(x, y, w, h, self.l.radius, 1.0, self.l.edge);
    }
    fn hline(&mut self, x0: i32, x1: i32, y: i32, c: Rgb565) {
        self.c.fill(x0, y, x1 - x0, 1, c);
    }
    fn diamond(&mut self, x: f32, y: f32, r: f32, c: Rgb565, filled: bool) {
        let pts = [(x, y - r), (x + r, y), (x, y + r), (x - r, y)];
        if filled {
            self.c.poly(&pts, c);
        } else {
            for i in 0..4 {
                let (a, b) = (pts[i], pts[(i + 1) % 4]);
                self.c.line(a.0, a.1, b.0, b.1, 2.0, c);
            }
        }
    }
    /// The pilot: an X.
    fn cross(&mut self, x: f32, y: f32, r: f32, c: Rgb565) {
        self.c.line(x - r, y - r, x + r, y + r, 3.0, c);
        self.c.line(x - r, y + r, x + r, y - r, 3.0, c);
    }
    /// A check mark (the font has none).
    fn check(&mut self, x: f32, y: f32, s: f32, c: Rgb565) {
        self.c.line(x, y, x + s * 0.4, y + s * 0.4, 2.5, c);
        self.c.line(x + s * 0.4, y + s * 0.4, x + s, y - s * 0.5, 2.5, c);
    }
    /// A chevron "›" (the font has none).
    fn chevron(&mut self, x: f32, y: f32, s: f32, c: Rgb565) {
        self.c.line(x, y - s, x + s * 0.8, y, 2.5, c);
        self.c.line(x + s * 0.8, y, x, y + s, 2.5, c);
    }
    /// An arrow `len` long centred on (x, y), pointing at bearing `deg`.
    fn arrow(&mut self, x: f32, y: f32, len: f32, deg: f32, c: Rgb565, w: f32) {
        let (dx, dy) = dir(deg);
        let (tx, ty) = (x + dx * len / 2.0, y + dy * len / 2.0);
        let (bx, by) = (x - dx * len / 2.0, y - dy * len / 2.0);
        self.c.line(bx, by, tx - dx * len * 0.3, ty - dy * len * 0.3, w, c);
        let hw = len * 0.3;
        let (px, py) = (-dy, dx);
        self.c.poly(
            &[
                (tx, ty),
                (tx - dx * len * 0.45 + px * hw, ty - dy * len * 0.45 + py * hw),
                (tx - dx * len * 0.45 - px * hw, ty - dy * len * 0.45 - py * hw),
            ],
            c,
        );
    }
    /// A north-up rose: a ring with N on top and an arrow to `deg`.
    fn rose(&mut self, x: f32, y: f32, r: f32, deg: f32, c: Rgb565) {
        let l = self.l;
        self.c.ring(x, y, r, 2.0, l.ring, None);
        let lf = self.f().label;
        let nw = lf.width("N", 0);
        let ny = y as i32 - r as i32 - i32::from(lf.height) / 2;
        self.c.fill(x as i32 - nw / 2 - 2, ny, nw + 4, i32::from(lf.height), l.bg);
        self.tc("N", x as i32, ny, &lf, l.label);
        self.arrow(x, y, r * 1.35, deg, c, 3.5);
    }
    /// A small triangle for key labels.
    fn tri(&mut self, x: f32, y: f32, s: f32, a: Arrow, c: Rgb565) {
        let pts = match a {
            Arrow::Left => [(x - s, y), (x + s * 0.7, y - s), (x + s * 0.7, y + s)],
            Arrow::Right => [(x + s, y), (x - s * 0.7, y - s), (x - s * 0.7, y + s)],
            Arrow::Up => [(x, y - s), (x + s, y + s * 0.7), (x - s, y + s * 0.7)],
            Arrow::Down => [(x, y + s), (x + s, y - s * 0.7), (x - s, y - s * 0.7)],
        };
        self.c.poly(&pts, c);
    }
    /// Four bars of rising height, filled up to the level: shape carries
    /// the value. Empty bars are outlined in the ring colour (3:1).
    fn bars(&mut self, x: i32, bottom: i32, level: i32, on: Rgb565) {
        for i in 0..4 {
            let h = 5 + i * 4;
            let bx = x + i * 7;
            if i < level {
                self.c.fill(bx, bottom - h, 5, h, on);
            } else {
                self.c.frame(bx, bottom - h, 5, h, 1, self.l.ring);
            }
        }
    }
    /// "W N · L 5 ·": each radio in a fixed column, a dot where not heard.
    fn radio_letters(&mut self, r: Radios, x: i32, y: i32, on: Rgb565) {
        let f = self.f().data;
        let cw = f.width("W", 0) + 2;
        for (i, radio) in Radio::ALL.iter().enumerate() {
            let mut s: String<4> = String::new();
            let heard = r.contains(*radio);
            let _ = s.push(if heard { radio.letter() } else { '\u{b7}' });
            self.c.text(&s, x + i as i32 * cw, y, &f, if heard { on } else { self.l.faint }, 0);
        }
    }
    /// A node's state: filled dot alive, hollow ring alive but deaf, hollow
    /// slashed dot dead.
    #[allow(clippy::too_many_arguments)]
    fn node_dot(&mut self, x: f32, y: f32, r: f32, n: &NodeStatus, ok: Rgb565, bad: Rgb565) {
        if n.deaf() {
            self.c.ring(x, y, r - 1.0, 2.0, bad, None);
        } else if n.alive {
            self.c.disc(x, y, r, ok);
        } else {
            self.c.ring(x, y, r - 1.0, 2.0, bad, None);
            self.c.line(x - r * 0.7, y + r * 0.7, x + r * 0.7, y - r * 0.7, 2.0, bad);
        }
    }
    /// A large caution area: filled, or (Red Night) outlined.
    fn caution_area(&mut self, x: i32, y: i32, w: i32, h: i32) -> (Rgb565, Rgb565) {
        let l = self.l;
        if l.one_hue {
            self.c.fill(x, y, w, h, l.bg);
            self.c.frame(x, y, w, h, 2, l.warn);
            (l.fg, l.dim)
        } else {
            self.c.fill(x, y, w, h, l.warn);
            (l.warn_ink, l.warn_ink)
        }
    }

    // ---------------------------------------------------------- header --

    /// The page's title block at the top left: the title, the page's place
    /// in the cycle ("3/6") and, while choosing or editing, the mode in one
    /// word. For a moment after a page flip the title inverts (naming the
    /// page without covering anything); a touch that does nothing shows
    /// what touch can do, and an automatic theme change says so, in the
    /// same block. Returns the x past it.
    fn header(&mut self, title: &str, s: &UiState) -> i32 {
        self.header_in(title, s, 520)
    }

    /// [`Self::header`] in a block at most `maxw` wide (the radar's corner):
    /// a longer message goes on two lines.
    fn header_in(&mut self, title: &str, s: &UiState, maxw: i32) -> i32 {
        let l = self.l;
        let f = self.f();
        let y = self.top + 6;
        let toast = self.ui.title_toast();
        let msg: Option<String<48>> = match toast {
            Some(TitleToast::TapHint) => Some(fmt(format_args!("Tap a drone, or use the keys below"))),
            Some(TitleToast::ThemeSwitched { to, why: "" }) => Some(fmt(format_args!("Switched to {}", to.name()))),
            Some(TitleToast::ThemeSwitched { to, why }) => Some(fmt(format_args!("Switched to {} ({why})", to.name()))),
            _ => None,
        };
        let th = i32::from(f.title.height);
        let mut x = if let Some(m) = &msg {
            let bf = f.body_b;
            if bf.width(m, 0) + 20 <= maxw {
                let w = bf.width(m, 0) + 20;
                self.c.rrect(M - 8, y - 2, w, th + 4, l.radius.min(4), l.fg);
                self.t(m, M + 2, y + (th - i32::from(bf.height)) / 2, &bf, l.bg);
                M - 8 + w
            } else {
                // Two lines, broken at the space nearest the middle.
                let mid = m.len() / 2;
                let cut = m.char_indices().filter(|(_, c)| *c == ' ').map(|(i, _)| i).min_by_key(|i| i.abs_diff(mid)).unwrap_or(mid);
                let (a, b) = (&m[..cut], m[cut..].trim_start());
                let w = bf.width(a, 0).max(bf.width(b, 0)) + 20;
                let lh = i32::from(bf.height);
                self.c.rrect(M - 8, y - 2, w, 2 * lh + 8, l.radius.min(4), l.fg);
                self.t(a, M + 2, y + 2, &bf, l.bg);
                self.t(b, M + 2, y + 2 + lh, &bf, l.bg);
                M - 8 + w
            }
        } else if toast == Some(TitleToast::Page) {
            let w = f.title.width(title, 0) + 20;
            self.c.rrect(M - 8, y - 2, w, th + 4, l.radius.min(4), l.fg);
            self.t(title, M + 2, y, &f.title, l.bg);
            M - 8 + w
        } else {
            self.t(title, M, y, &f.title, l.fg)
        };
        if msg.is_none() {
            let (i, n) = self.ui.page().position(s.flags.is_some());
            let t: String<8> = fmt(format_args!("{i}/{n}"));
            x = self.t(&t, x + 10, y + th - i32::from(f.small.height) - 3, &f.small, l.dim);
        }
        x
    }

    /// The mode in one word ("CHOOSING", "EDITING"), outlined, at (x, y).
    fn mode_chip(&mut self, x: i32, y: i32) -> i32 {
        let w = match (self.ui.mode(), self.ui.page()) {
            (Mode::Pick, _) => "CHOOSING",
            (Mode::Edit, _) if self.ui.adjusting() => "ADJUSTING",
            (Mode::Edit, _) => "EDITING",
            (Mode::Scroll, Page::Flags) => "CHOOSING",
            (Mode::Scroll, _) => "SCROLLING",
            _ => return x,
        };
        let f = self.f().small_b;
        self.outlined(w, x, y, &f, self.l.accent, 6)
    }

    // ------------------------------------------------------ status bar --

    /// GPS, then every warning as a chip (shortened), the NEW chip, then
    /// the nodes (a dead one as a boxed "5 DOWN"); clock, power and
    /// recording on the right. What does not fit ends in "+N".
    pub fn status_bar(&mut self, s: &UiState, warns: &[Warning]) {
        let l = self.l;
        if !self.c.touches(0, BAR_H) {
            return;
        }
        self.c.fill(0, 0, W, BAR_H, l.bar);
        let (lab, data, nf) = (self.f().label, self.f().data, self.f().small);
        let mid = |f: &Font| (BAR_H - i32::from(f.height)) / 2;

        // Right side first, inwards: clock, power, recording.
        let mut rx = W - 16;
        rx = self.c.text("UTC", rx - lab.width("UTC", 1), mid(&lab) + 1, &lab, l.bar_dim, 1) - lab.width("UTC", 1) - 6;
        let clock: String<8> = match s.utc_ms {
            Some(ms) => {
                let t = (ms / 1000) % 86_400;
                fmt(format_args!("{:02}:{:02}", t / 3600, t % 3600 / 60))
            }
            None => fmt(format_args!("--:--")),
        };
        rx = self.tr(&clock, rx, mid(&data), &data, l.bar_fg) - 20;
        match s.power {
            Power::Usb { .. } => rx = self.tr("USB", rx, mid(&nf), &nf, l.bar_fg) - 20,
            Power::Battery { pct } => {
                let low = pct <= 15;
                let c = if low { l.bar_warn } else { l.bar_fg };
                let bx = rx - 30;
                let by = BAR_H / 2 - 7;
                self.c.rrect_stroke(bx, by, 26, 14, 2, 1.5, c);
                self.c.fill(bx + 26, by + 4, 3, 6, c);
                let lev = (i32::from(pct.min(100)) * 20 + 50) / 100;
                self.c.fill(bx + 3, by + 3, lev, 8, c);
                let t: String<6> = fmt(format_args!("{pct}%"));
                rx = if low {
                    let w = self.f().small_b.width(&t, 0) + 12;
                    self.chip(&t, bx - 8 - w);
                    bx - 8 - w - 20
                } else {
                    self.tr(&t, bx - 8, mid(&nf), &nf, c) - 20
                };
            }
        }
        match s.sd {
            SdLog::Off => {}
            // The CSV log records but evidence is not reaching the card:
            // never REC for that.
            SdLog::Recording if s.caveats.evidence.is_some() => rx = self.chip_r("NO EVIDENCE", rx) - 16,
            SdLog::Recording => {
                let lx = self.c.text("REC", rx - lab.width("REC", 1), mid(&lab) + 1, &lab, l.bar_fg, 1) - lab.width("REC", 1);
                self.c.disc(lx as f32 - 12.0, BAR_H as f32 / 2.0, 6.0, l.bar_rec);
                rx = lx - 30;
            }
            SdLog::NoCard => rx = self.tr("no SD", rx, mid(&nf), &nf, l.bar_dim) - 16,
            SdLog::Error => rx = self.chip_r("SD ERROR", rx) - 16,
        }
        let limit = rx;

        // Left: the GPS item is itself the warning when the fix is doubtful.
        let mut x = 14;
        x = match s.gps.integrity {
            GpsIntegrity::SuspectSpoofing => self.chip("GPS FAKE?", x),
            GpsIntegrity::Spoofing => self.chip("GPS FAKE", x),
            GpsIntegrity::SuspectJamming => self.chip("GPS JAM?", x),
            GpsIntegrity::Jamming => self.chip("GPS JAM", x),
            GpsIntegrity::Ok => match s.gps.fix {
                Fix::Fix2D | Fix::Fix3D => {
                    let mut x = self.c.text("GPS", x, mid(&lab) + 1, &lab, l.bar_dim, 1) + 6;
                    let n: String<6> = fmt(format_args!("{}", s.gps.sats));
                    x = self.t(&n, x, mid(&data), &data, l.bar_fg);
                    if s.gps.fix == Fix::Fix2D {
                        x = self.c.text("2D", x + 6, mid(&lab) + 1, &lab, l.bar_dim, 1);
                    }
                    x
                }
                Fix::Manual => {
                    let x = self.c.text("POS", x, mid(&lab) + 1, &lab, l.bar_dim, 1) + 6;
                    self.t("TYPED", x, mid(&nf), &nf, l.bar_fg)
                }
                Fix::None => self.chip("NO FIX", x),
                Fix::Absent => self.chip("NO GPS", x),
            },
        } + 12;

        // Every warning, shortened (the GPS ones are the GPS item above;
        // dead nodes are their own chips below).
        let mut items: heapless::Vec<String<16>, 16> = heapless::Vec::new();
        for w in warns {
            let t = match w {
                Warning::NotListening { .. } => "NOT LISTENING",
                Warning::NearAircraft => "PLANE NEAR",
                Warning::Jamming => "JAMMED",
                Warning::AreaFaked => "AREA GPS FAKE",
                Warning::IdFlood => "FAKE IDS",
                Warning::SeriousFlag => "FLAG",
                Warning::AreaJammed => "AREA GPS JAM",
                Warning::GpsFaked | Warning::GpsJammed | Warning::NodesDown { .. } | Warning::NoPosition => continue,
            };
            let _ = items.push(fmt(format_args!("{t}")));
        }
        if s.threats.active > 0 && !s.threats.jammed && s.threats.flood_per_s == 0 {
            let _ = items.push(fmt(format_args!("ATTACK")));
        }
        // Receivers not answering: one boxed chip naming them ("5 DOWN",
        // "DOWN 2.4 5 BT"), never only a slashed dot.
        let mut dead: String<16> = String::new();
        let n_dead = s.nodes.iter().filter(|n| !n.alive).count();
        if n_dead == 1 {
            dead = fmt(format_args!("{} DOWN", s.nodes.iter().find(|n| !n.alive).map_or("", |n| n.kind.short())));
        } else if n_dead > 1 {
            let _ = dead.push_str("DOWN");
            for n in s.nodes.iter().filter(|n| !n.alive) {
                let _ = dead.push(' ');
                if dead.push_str(n.kind.short()).is_err() {
                    break;
                }
            }
        }
        if !dead.is_empty() {
            let _ = items.push(dead);
        }
        let mut hidden = 0;
        let sb = self.f().small_b;
        let room = |x: i32, t: &str| x + sb.width(t, 0) + 12 + 44 <= limit;
        for t in &items {
            if hidden == 0 && room(x, t) {
                x = self.chip(t, x) + 8;
            } else {
                hidden += 1;
            }
        }
        if let Some(n) = self.ui.new_chip() {
            let t: String<12> = fmt(format_args!("NEW {n}"));
            if hidden == 0 && room(x, &t) {
                let w = sb.width(&t, 0) + 12;
                let y = (BAR_H - i32::from(sb.height) - 6) / 2;
                self.c.rrect(x, y, w, i32::from(sb.height) + 6, 3, l.accent);
                self.t(&t, x + 6, y + 3, &sb, l.accent_ink);
                x += w + 8;
            } else {
                hidden += 1;
            }
        }
        x += 6;
        // Live nodes last: a dot and the label (a ring when hearing
        // nothing). The least urgent, so the first to go for lack of room.
        for n in s.nodes.iter().filter(|n| n.alive) {
            let w = 15 + nf.width(n.kind.short(), 0) + 12;
            if hidden > 0 || x + w + 44 > limit {
                hidden += 1;
                continue;
            }
            self.node_dot(x as f32 + 5.0, BAR_H as f32 / 2.0, 5.0, n, l.bar_ok, l.bar_warn);
            let c = if n.deaf() { l.bar_warn } else { l.bar_fg };
            x = self.t(n.kind.short(), x + 15, mid(&nf), &nf, c) + 12;
        }
        if hidden > 0 {
            let t: String<8> = fmt(format_args!("+{hidden}"));
            self.chip(&t, x);
        }
    }

    /// A warning chip in the status bar: dark ink on the amber fill.
    fn chip(&mut self, t: &str, x: i32) -> i32 {
        let f = self.f().small_b;
        let u: String<24> = upper(t);
        let l = self.l;
        self.boxed(&u, x, (BAR_H - i32::from(f.height) - 6) / 2, &f, l.bar_warn_ink, l.bar_warn, 6)
    }
    /// A chip with its right edge at `right`; returns its left x.
    fn chip_r(&mut self, t: &str, right: i32) -> i32 {
        let w = self.f().small_b.width(t, 0) + 12;
        self.chip(t, right - w);
        right - w
    }

    // ---------------------------------------------------- warning band --

    /// The highest-priority warning in plain words under the status bar,
    /// "+N more" when there are others (each is a chip in the bar).
    pub fn band(&mut self, s: &UiState, warns: &[Warning]) {
        let Some(w) = warns.first() else { return };
        if !self.c.touches(BAR_H, BAR_H + BAND_H) {
            return;
        }
        let f = self.f();
        let (ink, ink2) = self.caution_area(0, BAR_H, W, BAND_H);
        let (head, tail) = w.text(s);
        let mut right = W - M;
        if warns.len() > 1 {
            let t: String<16> = fmt(format_args!("+{} more", warns.len() - 1));
            right = self.tr(&t, right, BAR_H + (BAND_H - i32::from(f.small_b.height)) / 2, &f.small_b, ink) - 16;
        }
        let hf = f.title.width(&head, 0) <= right - M - 30;
        let hfont = if hf { f.title } else { f.body_b };
        let y = BAR_H + (BAND_H - i32::from(hfont.height)) / 2;
        let e = self.t("!", M, y, &hfont, ink);
        let e = self.t(&head, e + 12, y, &hfont, ink);
        let tail_t: String<64> = fmt(format_args!("\u{b7} {tail}"));
        if e + 12 + f.body.width(&tail_t, 0) <= right {
            self.t(&tail_t, e + 12, BAR_H + (BAND_H - i32::from(f.body.height)) / 2, &f.body, ink2);
        }
    }

    // ------------------------------------------------------------ keys --

    pub fn keys(&mut self, labels: &[KeyLabel; 3], lit: Option<Button>) {
        let l = self.l;
        if !self.c.touches(KEYS_TOP, H) {
            return;
        }
        self.c.fill(0, KEYS_TOP, W, H - KEYS_TOP, l.strip);
        // A touch that did nothing: the strip flashes, pointing at the keys.
        if self.ui.title_toast() == Some(TitleToast::TapHint) {
            self.c.fill(0, KEYS_TOP, W, 2, l.accent);
        }
        let (kf, hf) = (self.f().small_b, self.f().label);
        for (i, k) in labels.iter().enumerate() {
            let (x, y, w, h) = key_rect(i);
            let b = [Button::Up, Button::Select, Button::Down][i];
            let pressed = lit == Some(b);
            let r = l.radius.max(4);
            let text = match (l.keys, pressed, k.active) {
                (_, true, _) if l.one_hue => {
                    // Red Night: a heavy outline, never a lit slab.
                    self.c.rrect(x, y, w, h, r, l.press_fill);
                    self.c.rrect_stroke(x, y, w, h, r, 4.0, l.fg);
                    l.press_ink
                }
                (_, true, _) => {
                    self.c.rrect(x, y, w, h, r, l.press_fill);
                    self.c.rrect_stroke(x, y, w, h, r, 2.0, l.fg);
                    l.press_ink
                }
                (KeyStyle::Outline, _, false) => {
                    self.c.rrect_stroke(x, y, w, h, r, 1.0, l.grid);
                    l.faint
                }
                (KeyStyle::Outline, _, true) if k.primary => {
                    self.c.rrect(x, y, w, h, r, l.key_fill);
                    self.c.rrect_stroke(x, y, w, h, r, 2.0, l.accent);
                    l.accent
                }
                (KeyStyle::Outline, _, true) => {
                    self.c.rrect(x, y, w, h, r, l.key_fill);
                    self.c.rrect_stroke(x, y, w, h, r, 1.0, l.key_edge);
                    l.key_text
                }
                (KeyStyle::Filled, _, false) => {
                    self.c.rrect_stroke(x, y, w, h, r, 1.0, l.key_fill);
                    l.bar_dim
                }
                (KeyStyle::Filled, _, true) if k.primary => {
                    self.c.rrect(x, y, w, h, r, l.accent);
                    l.accent_ink
                }
                (KeyStyle::Filled, _, true) => {
                    self.c.rrect(x, y, w, h, r, l.key_fill);
                    l.key_text
                }
            };
            // Holding the middle key: it fills up towards the hold.
            if i == 1
                && let Some(ms) = self.ui.held_ms()
            {
                let frac = (ms.min(HOLD_MS) as i32 * (w - 12)) / HOLD_MS as i32;
                self.c.fill(x + 6, y + h - 9, frac, 5, text);
                self.c.frame(x + 6, y + h - 9, w - 12, 5, 1, text);
            }
            let word: String<24> = upper(&k.text);
            let tw = kf.width(&word, 1);
            let aw = if k.arrow.is_some() || k.check { 20 } else { 0 };
            let total = tw + aw;
            let ty = match k.sub {
                Some(_) => y + 9,
                None => y + (h - i32::from(kf.height)) / 2,
            };
            let mut tx = x + (w - total) / 2;
            let ay = (ty + i32::from(kf.ascent) - i32::from(kf.cap) / 2) as f32;
            match k.arrow {
                Some(a @ (Arrow::Left | Arrow::Up)) => {
                    self.tri(tx as f32 + 6.0, ay, 7.0, a, text);
                    tx += aw;
                    self.c.text(&word, tx, ty, &kf, text, 1);
                }
                Some(a) => {
                    let e = self.c.text(&word, tx, ty, &kf, text, 1);
                    self.tri(e as f32 + 13.0, ay, 7.0, a, text);
                }
                None => {
                    let e = self.c.text(&word, tx, ty, &kf, text, 1);
                    if k.check {
                        self.check(e as f32 + 6.0, ay, 12.0, text);
                    }
                }
            }
            if let Some(sub) = &k.sub {
                let hw = hf.width(sub, 1);
                self.c.text(sub, x + (w - hw) / 2, y + h - i32::from(hf.height) - 12, &hf, text, 1);
            }
        }
    }

    // --------------------------------------------- faults and notes --

    /// Above the keys: each fault (evidence not being written, a problem
    /// with the keyed node link) as a filled bar; standing notes as at most two muted lines, "+N notes" pointing
    /// to the health page for the rest.
    pub fn strip(&mut self, s: &UiState, faults: &[String<64>], notes: &[String<160>]) {
        let l = self.l;
        let f = self.f();
        let mut y = self.bottom;
        for t in faults {
            let (ink, _) = self.caution_area(0, y, W, CAVEAT_H - 1);
            let fb = f.small_b;
            let mut cut = t.len();
            while cut > 0 && fb.width(&t[..cut], 0) > W - 32 {
                cut -= 1;
            }
            self.t(&t[..cut], 16, y + (CAVEAT_H - 1 - i32::from(fb.height)) / 2, &fb, ink);
            y += CAVEAT_H;
        }
        if notes.is_empty() {
            return;
        }
        let (_, hidden) = super::note_lines(s, &f.small);
        self.hline(0, W, y, l.edge);
        for (i, n) in notes.iter().enumerate() {
            // A small amber tick marks a note, not a fault.
            self.c.fill(16, y + 6, 4, NOTE_H - 10, l.warn);
            self.t(n, 28, y + (NOTE_H - i32::from(f.small.height)) / 2, &f.small, l.dim);
            if i + 1 == notes.len() && hidden > 0 {
                let t: String<24> = fmt(format_args!("+{hidden} notes: see Health"));
                self.tr(&t, W - 16, y + (NOTE_H - i32::from(f.small.height)) / 2, &f.small, l.dim);
            }
            y += NOTE_H;
        }
    }

    // ----------------------------------------------------------- radar --

    pub fn radar(&mut self, s: &UiState) {
        let l = self.l;
        let top = self.top;
        let order = s.order();
        let h = self.bottom - top;
        let r = (h / 2 - 30).min(158) as f32;
        let (cx, cy) = (236.0f32, (top + self.bottom) as f32 / 2.0 + 2.0);

        // Rings: solid, 3:1 against the page; each labelled where it
        // crosses the 135° line, on a knockout.
        for (m, _) in RINGS {
            self.c.ring(cx, cy, radius(m, r), 1.0, l.ring, None);
        }
        self.c.fill(cx as i32, (cy - r) as i32, 1, 2 * r as i32, l.grid);
        self.c.fill((cx - r) as i32, cy as i32, 2 * r as i32, 1, l.grid);
        for a in (0..360).step_by(30) {
            let (dx, dy) = dir(a as f32);
            self.c.line(cx + dx * (r - 7.0), cy + dy * (r - 7.0), cx + dx * r, cy + dy * r, 1.2, l.ring);
        }
        let sf = self.f().small;
        let lf = self.f().label;
        let (kx, ky) = dir(135.0);
        for (m, label) in RINGS {
            let rr = radius(m, r);
            let (x, y) = ((cx + kx * rr) as i32, (cy + ky * rr) as i32);
            let w = lf.width(label, 0);
            let hh = i32::from(lf.height);
            self.c.fill(x - w / 2 - 3, y - hh / 2, w + 6, hh, l.bg);
            self.t(label, x - w / 2, y - hh / 2, &lf, l.dim);
        }
        // N boxed: this plot is north-up, not "the way you face".
        let bf = self.f().body_b;
        let ny = (cy - r) as i32 - i32::from(bf.height) - 6;
        let nw = bf.width("N", 0);
        self.boxed("N", cx as i32 - nw / 2 - 5, ny, &bf, l.bg, l.fg, 5);
        self.tc("S", cx as i32, (cy + r) as i32 + 3, &bf, l.dim);
        self.t("E", (cx + r) as i32 + 6, cy as i32 - i32::from(bf.height) / 2, &bf, l.dim);
        self.tr("W", (cx - r) as i32 - 6, cy as i32 - i32::from(bf.height) / 2, &bf, l.dim);
        self.header_in("RADAR", s, (cx - r * 0.45) as i32 - M);
        self.label("North up", M, top + 62);
        self.t("not the way", M, top + 80, &sf, l.dim);
        self.t("you face", M, top + 98, &sf, l.dim);
        self.t("rings not", M, self.bottom - 64, &sf, l.dim);
        self.t("to scale", M, self.bottom - 46, &sf, l.dim);
        self.mode_chip(M, self.bottom - 26);

        let foc = focus(self.ui, s, &order);
        let focus_key = foc.map(|f| f.1.key);
        let plot = |p: Polar| {
            let (dx, dy) = dir(p.bearing_deg);
            let rr = radius(p.distance_m, r);
            (cx + dx * rr, cy + dy * rr)
        };
        // The focused drone's reported pilot, joined to it by a dashed line.
        if let Some((_, f, _)) = foc
            && let (Some(dp), Some(op)) = (f.drone, f.operator)
        {
            let (a, b) = (plot(dp), plot(op));
            self.c.line_dashed(a.0, a.1, b.0, b.1, 1.2, l.operator, Some((4.0, 4.0)));
            self.cross(b.0, b.1, 6.0, l.operator);
            self.t("P", b.0 as i32 + 10, b.1 as i32 - 20, &lf, l.operator);
        }
        // A disputed, focused drone's other claimed position: the same
        // mark as the first (no verdict on which is real), with "?".
        if let Some((_, f, _)) = foc
            && let (Some(dp), Some(other)) = (f.drone, f.other_claim)
        {
            let (a, b) = (plot(dp), plot(other));
            self.c.line_dashed(a.0, a.1, b.0, b.1, 1.2, l.dim, Some((4.0, 4.0)));
            self.diamond(b.0, b.1, 8.0, l.accent, true);
            let qf = self.f().small_b;
            self.t("?", b.0 as i32 + 11, b.1 as i32 - 24, &qf, l.fg);
            self.t("?", a.0 as i32 - 22, a.1 as i32 - 24, &qf, l.fg);
        }
        // Unconfirmed drones claiming to be within 20 m of the hub (a flood
        // of fakes): one "?58" chip instead of a pile of markers.
        let near_fakes = order
            .iter()
            .map(|&i| &s.drones[usize::from(i)])
            .filter(|d| d.unconfirmed && d.drone.is_some_and(|p| p.distance_m < 20.0))
            .count();
        let hidden_fakes = usize::from(s.total_unconfirmed).saturating_sub(s.drones.iter().filter(|d| d.unconfirmed).count());
        // Stale and unconfirmed first, then active, the focus on top.
        let mut unplaced = 0u32;
        let nf = self.f().small_b;
        let tapped = self.ui.tapped();
        for pass in 0..3 {
            for (rank, &i) in order.iter().enumerate() {
                let dv = &s.drones[usize::from(i)];
                let Some(pos) = dv.drone else {
                    if pass == 0 && !dv.stale {
                        unplaced += 1;
                    }
                    continue;
                };
                if near_fakes > 0 && dv.unconfirmed && pos.distance_m < 20.0 {
                    continue;
                }
                let is_focus = Some(dv.key) == focus_key;
                let faint = dv.stale || dv.unconfirmed;
                let want = if is_focus {
                    2
                } else if faint {
                    0
                } else {
                    1
                };
                if want != pass {
                    continue;
                }
                let col = if faint { l.faint } else { l.accent };
                let (mut x, mut y) = plot(pos);
                if pos.distance_m > RINGS[RINGS.len() - 1].0 {
                    // Past the outer ring: a marker on the rim with its
                    // distance beside it (an arrow there read as a heading).
                    let (dx, dy) = dir(pos.bearing_deg);
                    (x, y) = (cx + dx * r, cy + dy * r);
                    self.diamond(x, y, 7.0, col, !faint);
                    self.c.line(x + dx * 8.0, y + dy * 8.0, x + dx * 16.0, y + dy * 16.0, 2.0, col);
                    let d = distance_short(pos.distance_m);
                    let tw = sf.width(&d, 0);
                    // Beside the marker, below it (the rank number sits above).
                    let lx = if dx >= 0.0 { x as i32 + 4 } else { x as i32 - 4 - tw };
                    let ly = if y < cy + r * 0.8 { y as i32 + 10 } else { y as i32 - 34 };
                    self.c.fill(lx - 2, ly, tw + 4, i32::from(sf.height), l.bg);
                    self.t(&d, lx, ly, &sf, if faint { l.faint } else { l.fg });
                } else {
                    let rr = if is_focus { 10.0 } else { 8.0 };
                    self.diamond(x, y, rr, col, !faint);
                }
                self.hit(Spot::Circle(x, y, REACH), Tap::Drone(dv.key));
                if is_focus || tapped == Some(Tap::Drone(dv.key)) {
                    self.c.ring(x, y, 16.0, 2.0, l.fg, None);
                }
                let o = if is_focus { 16 } else { 11 };
                let mut lx = x as i32 + o;
                if dv.unconfirmed {
                    lx = self.t("?", lx, y as i32 - o - 14, &nf, l.faint);
                } else if rank < 9 {
                    let n: String<4> = fmt(format_args!("{}", rank + 1));
                    lx = self.t(&n, lx, y as i32 - o - 14, &nf, if dv.stale { l.faint } else { l.fg });
                }
                if self.ui.is_new(dv.key) && !dv.unconfirmed {
                    self.new_chip(lx + 4, y as i32 - o - 13);
                }
            }
        }
        if near_fakes > 0 || hidden_fakes > 0 {
            let t: String<12> = fmt(format_args!("?{}", near_fakes + hidden_fakes));
            let w = nf.width(&t, 0) + 12;
            self.outlined(&t, cx as i32 + 12, cy as i32 + 10, &nf, l.faint, 6);
            let _ = w;
        }
        // You: a ring and a dot, never a triangle (no facing is known),
        // drawn last so nothing covers it.
        self.c.ring(cx, cy, 7.0, 2.0, l.bg, None);
        self.c.ring(cx, cy, 6.0, 1.5, l.fg, None);
        self.c.disc(cx, cy, 2.5, l.fg);
        if unplaced > 0 {
            let t: String<32> = fmt(format_args!("+{unplaced} without position"));
            self.t(&t, M, self.bottom - 88, &sf, l.dim);
        }
        let tight = h < 330;
        let count_h = if tight { 78 } else { 100 };
        self.nearest_card(s, foc, self.bottom - count_h - 8);
        self.count_card(s, 452, self.bottom - count_h, 332, count_h - 6);
    }

    /// The radar's readout: the nearest drone with a position (or the one
    /// being chosen), as broadcast, from `top + 8` to `end`. Keeps its
    /// height and ID rows when space is short (the number shrinks first).
    fn nearest_card(&mut self, s: &UiState, foc: Option<(usize, &DroneView, bool)>, end: i32) {
        let l = self.l;
        let (x, y, w) = (452, self.top + 8, 332);
        let h = end - y;
        self.card(x, y, w, h);
        let p = 16;
        let f = self.f();
        let Some((rank, dv, picked)) = foc else {
            // Never "all clear" while partly blind.
            let ws = s.warnings();
            let not = ws.iter().any(|w| matches!(w, Warning::NotListening { .. }));
            let some = ws.iter().any(|w| matches!(w, Warning::NodesDown { .. }));
            let (a, b, c, warn): (&str, &str, &str, bool) = if not {
                ("NOT LISTENING:", "no receivers", "answering", true)
            } else if s.threats.jammed {
                ("JAMMED:", "drones may", "be missed", true)
            } else if some {
                ("NOT ALL", "RECEIVERS", "LISTENING", true)
            } else if s.drones.is_empty() {
                ("No drone IDs", "heard.", "", false)
            } else {
                ("No drone", "position yet", "", false)
            };
            self.label("Nearest", x + p, y + 12);
            let col = if warn { l.warn_text } else { l.dim };
            let tf = f.title;
            self.t(a, x + p, y + 40, &tf, col);
            self.t(b, x + p, y + 72, &tf, col);
            if !c.is_empty() {
                self.t(c, x + p, y + 104, &tf, col);
            }
            if !warn && s.drones.is_empty() && h > 150 {
                self.t("Drones without Remote ID", x + p, y + 112, &f.small, l.dim);
                self.t("may only show on Signals.", x + p, y + 132, &f.small, l.dim);
            }
            return;
        };
        self.hit(Spot::Rect(x, y, w, h), Tap::Drone(dv.key));
        let col = ink(l, dv.stale);
        // Label slot: what this is, and that it is the drone's own claim.
        let lab_end = if dv.disputed {
            let e = self.caution("! 2 transmitters", x + p - 4, y + 7);
            self.t("one ID", e + 8, y + 10, &f.small, l.dim)
        } else if picked {
            let t: String<16> = fmt(format_args!("DRONE {}", rank + 1));
            self.boxed(&t, x + p - 4, y + 7, &f.small_b, l.bg, l.fg, 6)
        } else {
            let any_unplaced = s.drones.iter().any(|d| !d.stale && d.drone.is_none());
            self.label(if any_unplaced { "Nearest with position" } else { "Nearest" }, x + p, y + 12)
        };
        if !dv.disputed {
            self.t("as broadcast", lab_end + 10, y + 9, &f.small, l.dim);
        }
        let pos = dv.drone.unwrap_or_default();
        let (n, unit) = distance(pos.distance_m);
        // Rows that must stay: height and ID; pilot and heard when room.
        let rows_room = |rows: i32| rows * 25 + 12;
        let head = 36;
        let avail = h - head;
        let room = w - 2 * p - 96;
        let mut fonts = [f.num_xl, f.num_l, f.num_m].into_iter().filter(|b| b.width(&n, 0) <= room);
        let mut big = fonts.next().unwrap_or(f.num_m);
        let mut nrows = 4;
        loop {
            let need = i32::from(big.cap) + 26 + rows_room(nrows);
            if need <= avail {
                break;
            }
            match fonts.next() {
                Some(b) => big = b,
                None if nrows > 2 => nrows -= 1,
                None => break,
            }
        }
        let num_col = if dv.disputed { l.dim } else { col };
        let ntop = y + head + 8;
        let e = self.num(&n, x + p - 2, ntop, &big, num_col);
        self.unit_on(unit, e + 8, ntop, &big, &f.head, l.dim);
        // Bearing: an arrow in a north-up rose, the words under it.
        let rr = (i32::from(big.cap) / 2).clamp(16, 28) as f32;
        let (ax, ay) = ((x + w - p - 40) as f32, (ntop + i32::from(big.cap) / 2 - 6) as f32);
        self.rose(ax, ay, rr, pos.bearing_deg, num_col);
        let t: String<16> = fmt(format_args!("{} {}", compass(pos.bearing_deg), degrees(pos.bearing_deg).as_str()));
        self.tc(&t, ax as i32, ay as i32 + rr as i32 + 4, &f.small_b, col);

        let mut ry = ntop + i32::from(big.cap) + 26;
        self.hline(x + p, x + w - p, ry - 8, l.edge);
        let vx = x + p + 82;
        let mut kinds: heapless::Vec<&str, 4> = heapless::Vec::new();
        // The card says "as broadcast": the pilot's place is what the drone
        // reports, like everything else here.
        for k in ["Height", "Pilot", "ID", "Heard"] {
            let keep = match nrows {
                4 => true,
                3 => k != "Heard",
                _ => k == "Height" || k == "ID",
            };
            if keep {
                let _ = kinds.push(k);
            }
        }
        for k in kinds {
            self.label(k, x + p, ry + 4);
            let maxw = x + w - p - vx;
            match k {
                "Height" => {
                    self.t_fit(&height_words(dv), vx, ry, i32::from(f.body.height), maxw, &[f.body, f.small], col);
                }
                "Pilot" => match dv.operator {
                    Some(o) => {
                        let t: String<32> = fmt(format_args!("{} {} of you", distance_short(o.distance_m).as_str(), compass(o.bearing_deg)));
                        let e = self.t_fit(&t, vx, ry, i32::from(f.body.height), maxw - 24, &[f.body, f.small], l.operator);
                        self.cross((e + 14) as f32, (ry + i32::from(f.body.height) / 2) as f32, 5.0, l.operator);
                    }
                    None => {
                        self.t("not sent", vx, ry + 2, &f.small, l.dim);
                    }
                },
                "ID" => {
                    let idw = if dv.disputed { 10 } else { 18 };
                    let e = self.t_fit(&short_id(dv, idw), vx, ry, i32::from(f.body.height), maxw, &[f.body, f.small], col);
                    if dv.disputed {
                        self.t("! 2 senders", e + 8, ry + 2, &f.small_b, l.warn_text);
                    }
                }
                _ => {
                    let t: String<48> =
                        fmt(format_args!("{} \u{b7} {}", age(s.now_ms.saturating_sub(dv.last_seen_ms)).as_str(), radio_words(dv.radios, 12).as_str()));
                    let e = self.t_fit(&t, vx, ry, i32::from(f.body.height), maxw - 50, &[f.body, f.small], col);
                    if self.ui.is_new(dv.key) {
                        self.new_chip(e + 8, ry + 3);
                    }
                }
            }
            ry += 25;
        }
    }

    /// "5 DRONES SENDING ID": only drones that broadcast an ID count here;
    /// radio links with no ID are on the Signals page and said so.
    fn count_card(&mut self, s: &UiState, x: i32, y: i32, w: i32, h: i32) {
        use core::fmt::Write;
        let l = self.l;
        self.card(x, y, w, h);
        let f = self.f();
        let active = s.confirmed_active_count();
        let unconf = usize::from(s.total_unconfirmed);
        let total = usize::from(s.total_drones).max(s.drones.len()).saturating_sub(unconf);
        let deaf = s.warnings().iter().any(|w| matches!(w, Warning::NotListening { .. }));
        let n: String<8> = fmt(format_args!("{active}"));
        let num = if h >= 90 { f.num_l } else { f.num_m };
        let ncol = if deaf {
            l.faint
        } else if active > 0 {
            l.accent
        } else {
            l.dim
        };
        let e = self.num(&n, x + 16, y + 12, &num, ncol);
        self.label(if active == 1 { "Drone sending ID" } else { "Drones sending ID" }, e + 14, y + 12);
        let stale = total.saturating_sub(active);
        let unplaced = s.drones.iter().filter(|d| !d.stale && d.drone.is_none()).count();
        let mut t: String<64> = String::new();
        if deaf {
            let _ = t.push_str("receivers down: may be out of date");
        } else if unconf > 0 {
            let _ = write!(t, "+{unconf} unconfirmed");
        } else if stale > 0 {
            let _ = write!(t, "+{stale} gone quiet (60 s)");
        } else {
            let _ = t.push_str(if active == 0 { "none heard" } else { "heard now" });
        }
        let links = s.links_without_position();
        let tw = x + w - 12 - (e + 14);
        self.t_fit(&t, e + 14, y + 28, 22, tw, &[f.small, f.label], if deaf { l.warn_text } else { l.dim });
        // A second line when there is room for it above the links line.
        if unplaced > 0 && s.gps.has_position() && !deaf && (links == 0 || h >= 90) {
            let t: String<32> = fmt(format_args!("{unplaced} without position"));
            self.t_fit(&t, e + 14, y + 48, 22, tw, &[f.small, f.label], l.dim);
        }
        if links > 0 {
            // The longest wording that fits.
            let pl = if links == 1 { "" } else { "s" };
            let long: String<64> = fmt(format_args!("+{links} drone radio link{pl} without position (see Signals)"));
            let mid: String<64> = fmt(format_args!("+{links} radio link{pl} without position: see Signals"));
            let short: String<64> = fmt(format_args!("+{links} radio link{pl}, no ID: see Signals"));
            let t = [&long, &mid, &short].into_iter().find(|t| f.small.width(t, 0) <= w - 28).unwrap_or(&short);
            self.t_fit(t, x + 16, y + h - 26, 22, w - 28, &[f.small, f.label], l.fg);
        }
    }

    // ------------------------------------------------ no fix: list first --

    pub fn no_fix_home(&mut self, s: &UiState) {
        let l = self.l;
        self.header("HOME \u{b7} NO POSITION", s);
        let y = self.top + 42;
        self.count_card(s, 16, y, 440, 66);
        let f = self.f().small;
        self.tr("newest first", W - M, y + 2, &f, l.dim);
        self.tr("signal strength is not distance", W - M, y + 22, &f, l.dim);
        self.tr("(Wi-Fi and Bluetooth send at different powers)", W - M, y + 42, &f, l.dim);
        self.rows(s, y + 74);
    }

    // ------------------------------------------------------------ list --

    pub fn list_page(&mut self, s: &UiState) {
        let l = self.l;
        let f = self.f();
        let y = self.top + 6;
        let e = self.header("DRONE LIST", s);
        let n: String<8> = fmt(format_args!("{}", s.confirmed_active_count()));
        let mut e = self.t(&n, e + 14, y, &f.title, l.accent);
        if s.total_unconfirmed > 0 {
            let t: String<24> = fmt(format_args!("+{} unconfirmed", s.total_unconfirmed));
            e = self.t(&t, e + 14, y + 8, &f.small, l.dim);
        }
        self.mode_chip(e + 16, y + 2);
        let sort = if s.gps.has_position() { "nearest first" } else { "newest first" };
        self.tr(sort, W - M, y + 8, &f.small, l.dim);
        self.rows(s, self.top + 46);
    }

    /// Column header, rows, stale divider, from `top` down. Every row is a
    /// touch target ([`ROW_H`] tall) that opens its drone.
    fn rows(&mut self, s: &UiState, top: i32) {
        let l = self.l;
        let f = self.f();
        let fix = s.gps.has_position();
        let order = s.order();
        // Columns.
        const C_N: i32 = 40; // right
        const C_ID: i32 = 76;
        const C_DIST: i32 = 330; // right
        const C_DIR: i32 = 348;
        const C_SIG: i32 = 300;
        const C_HGT: i32 = 592; // right
        // Radio letters end at the margin, whatever the face's width.
        let cw = f.data.width("W", 0) + 2;
        let c_radio = W - M - 5 * cw - f.data.width("W", 0);
        let c_age = c_radio - 14; // right
        self.label_r("#", C_N, top);
        self.label("ID", C_ID, top);
        if fix {
            self.label_r("Distance", C_DIST, top);
            self.label("Direction", C_DIR, top);
        } else {
            self.label("Signal", C_SIG, top);
        }
        self.label_r("Height (takeoff)", C_HGT, top);
        self.label_r("Heard", c_age, top);
        self.label("Radio", c_radio, top);
        self.hline(16, W - 16, top + 18, l.edge);

        if order.is_empty() {
            let ws = s.warnings();
            let blind = ws.iter().any(|w| matches!(w, Warning::NotListening { .. } | Warning::Jamming | Warning::NodesDown { .. }));
            self.tc("No drone IDs heard yet", W / 2, top + 50, &f.title, l.dim);
            let why = if s.threats.jammed {
                "Jamming: the hub may be missing drones"
            } else if blind {
                "Not every receiver is listening: drones may be missed"
            } else {
                "Drones without Remote ID may only show on Signals"
            };
            self.tc(why, W / 2, top + 88, &f.body, if blind || s.threats.jammed { l.warn_text } else { l.dim });
            return;
        }
        let first = top + 22;
        let stale_at = order.iter().position(|&i| s.drones[usize::from(i)].stale);
        let divider = if stale_at.is_some_and(|i| i > 0) { 24 } else { 0 };
        // Every row that fits; room for "+N more" only when some do not.
        let all = ((self.bottom - first - divider) / ROW_H).max(1) as usize;
        let fit = if order.len() <= all { all } else { ((self.bottom - first - divider - 20) / ROW_H).max(1) as usize };
        let cur = self.ui.cursor().and_then(|k| order.iter().position(|&i| s.drones[usize::from(i)].key == k));
        let start = match cur {
            Some(c) if c >= fit => c + 1 - fit,
            _ => 0,
        };
        let tapped = self.ui.tapped();
        let mut y = first;
        for (rank, &i) in order.iter().enumerate().skip(start).take(fit) {
            let dv = &s.drones[usize::from(i)];
            if Some(rank) == stale_at && rank > 0 {
                self.hline(16, 260, y + 12, l.faint);
                self.hline(W - 260, W - 16, y + 12, l.faint);
                self.tc("not heard for 60 s", W / 2, y + 2, &f.small, l.faint);
                y += 24;
            }
            self.hit(Spot::Rect(0, y, W, ROW_H), Tap::Drone(dv.key));
            let selected = Some(rank) == cur || tapped == Some(Tap::Drone(dv.key));
            if selected {
                self.c.rrect(8, y + 1, W - 16, ROW_H - 3, l.radius.min(8), l.sel_fill);
                self.c.rrect_stroke(8, y + 1, W - 16, ROW_H - 3, l.radius.min(8), 2.0, l.sel_edge);
                self.tri(18.0, (y + ROW_H / 2) as f32, 6.0, Arrow::Right, l.sel_edge);
            }
            // Unconfirmed (heard once, the easiest to fake): stale grey
            // with "?", like a drone gone quiet.
            let faint = dv.stale || dv.unconfirmed;
            let col = if faint { l.faint } else { l.fg };
            let ty = y + (ROW_H - i32::from(f.data.height)) / 2;
            let n: String<6> = if dv.unconfirmed { fmt(format_args!("?")) } else { fmt(format_args!("{}", rank + 1)) };
            self.tr(&n, C_N, ty, &f.data, if faint { l.faint } else { l.dim });
            let my = (y + ROW_H / 2) as f32;
            self.diamond(C_ID as f32 - 14.0, my, 6.0, if faint { l.faint } else { l.accent }, !faint);
            if dv.disputed {
                // Two transmitters use this ID: it stays visible, and says so.
                let e = self.t(&short_id(dv, 8), C_ID, ty, &f.data, col);
                self.outlined("2 senders", e + 6, y + (ROW_H - i32::from(f.label.height) - 4) / 2, &f.label, l.warn_text, 4);
            } else {
                self.t(&short_id(dv, 14), C_ID, ty, &f.data, col);
            }
            if fix {
                match dv.drone {
                    Some(p) => {
                        self.tr(&distance_short(p.distance_m), C_DIST, ty, &f.data, col);
                        self.arrow(C_DIR as f32 + 11.0, my, 22.0, p.bearing_deg, col, 2.5);
                        self.t(compass(p.bearing_deg), C_DIR + 30, ty, &f.data, col);
                        self.tr(&degrees(p.bearing_deg), C_DIR + 124, ty, &f.data, col);
                    }
                    None => {
                        self.tr("--", C_DIST, ty, &f.data, col);
                        self.t("no position yet", C_DIR, ty + 2, &f.small, l.dim);
                    }
                }
            } else {
                let rssi = dv.best_rssi();
                self.bars(C_SIG, y + ROW_H - 12, bar_level(rssi.map(i16::from)), col);
                if let Some(r) = rssi {
                    let t: String<8> = fmt(format_args!("{r} dBm"));
                    self.t(&t, C_SIG + 36, ty, &f.data, col);
                }
            }
            self.tr(&metres(dv.height_m), C_HGT, ty, &f.data, col);
            let a = age(s.now_ms.saturating_sub(dv.last_seen_ms));
            if self.ui.is_new(dv.key) && !dv.unconfirmed {
                // NEW in the Heard column, the age small beside it.
                let w = f.label.width("NEW", 1) + 10;
                let cx = c_age - w;
                self.new_chip(cx, y + (ROW_H - i32::from(f.label.height) - 2) / 2);
                if cx - 6 - f.small.width(&a, 0) > C_HGT + 8 {
                    self.tr(&a, cx - 6, y + (ROW_H - i32::from(f.small.height)) / 2, &f.small, col);
                }
            } else {
                self.tr(&a, c_age, ty, &f.data, if a.as_str() == "now" && !faint { l.fg } else { col });
            }
            self.radio_letters(dv.radios, c_radio, ty, col);
            y += ROW_H;
        }
        let more = order.len().saturating_sub(start + fit);
        if more > 0 {
            let t: String<16> = fmt(format_args!("+{more} more"));
            self.tr(&t, W - M, self.bottom - 20, &f.small, l.dim);
        }
    }

    // ---------------------------------------------------------- detail --

    pub fn detail(&mut self, s: &UiState) {
        use core::fmt::Write;
        let l = self.l;
        let f = self.f();
        let order = s.order();
        let found = self
            .ui
            .cursor()
            .and_then(|k| order.iter().position(|&i| s.drones[usize::from(i)].key == k).map(|r| (r, &s.drones[usize::from(order[r])])));
        let Some((rank, dv)) = found else {
            self.header("DRONE GONE", s);
            self.tc("Drone no longer tracked", W / 2, self.top + 140, &f.title, l.fg);
            self.tc("tap or hold the middle key to go back", W / 2, self.top + 180, &f.body, l.dim);
            return;
        };
        let col = ink(l, dv.stale);
        let y0 = self.top + 6;
        let head: String<24> = fmt(format_args!("DRONE {} OF {}", rank + 1, order.len()));
        let e = self.header(&head, s);
        self.t(dv.id_kind.label(), e + 14, y0 + 10, &f.small, l.dim);
        // The overall picture, so a detail never hides a closer drone.
        let near = order.iter().map(|&i| &s.drones[usize::from(i)]).find(|d| !d.stale && d.drone.is_some());
        let mut mini: String<48> = String::new();
        if let Some(n) = near.and_then(|n| n.drone) {
            let _ = write!(mini, "NEAREST {} {} \u{b7} ", distance_short(n.distance_m).as_str(), compass(n.bearing_deg));
        }
        let _ = write!(mini, "{} drone{}", s.confirmed_active_count(), if s.confirmed_active_count() == 1 { "" } else { "s" });
        self.tr(&mini, W - M, y0 + 2, &f.small_b, l.dim);
        let idy = y0 + 36;
        let id = if dv.id.is_empty() { "(none)" } else { dv.id.as_str() };
        let right_note: String<24> = if dv.stale {
            fmt(format_args!("GONE QUIET"))
        } else if !dv.model.is_empty() {
            fmt(format_args!("{}", dv.model))
        } else {
            fmt(format_args!("{}", dv.ua.label()))
        };
        let rw = if dv.disputed { f.small_b.width("! 2 TRANSMITTERS USE THIS ID", 0) + 40 } else { f.body.width(&right_note, 0) + 70 };
        let idf = if f.id.width(id, 0) <= W - 2 * M - rw {
            f.id
        } else if f.data_l.width(id, 0) <= W - 2 * M - rw {
            f.data_l
        } else {
            f.data
        };
        let e = self.t(id, M, idy, &idf, col);
        if self.ui.is_new(dv.key) {
            self.new_chip(e + 10, idy + 8);
        }
        if dv.disputed {
            // Two transmitters use this ID: said beside it.
            let w = f.small_b.width("! 2 TRANSMITTERS USE THIS ID", 0) + 12;
            self.caution("! 2 transmitters use this ID", W - M - w, idy + 4);
        } else if dv.stale {
            let w = f.small_b.width(&right_note, 0) + 12;
            self.boxed(&right_note, W - M - w, idy + 4, &f.small_b, l.bg, l.faint, 6);
        } else {
            self.tr(&right_note, W - M, idy + 6, &f.body, l.dim);
        }
        let rule = idy + i32::from(idf.height) + 4;
        self.hline(16, W - 16, rule, l.edge);

        // Right: where it is, big.
        let (cx0, cw) = (W - 16 - 236, 236);
        let ch = self.bottom - 6 - (rule + 8);
        self.card(cx0, rule + 8, cw, ch);
        self.direction_card(s, dv, cx0, rule + 8, cw, ch);

        // Left: label / value rows. The claim sits right under where the
        // drone and its pilot are, in body size: those are its claims.
        let rows = 9;
        let pitch = ((self.bottom - rule - 10) / rows).min(34);
        let (lx, vx) = (M, 178);
        let maxw = cx0 - 12 - vx;
        let mut y = rule + 8;
        let fix = s.gps.has_position();
        let vy = |y: i32, f: &Font| y + (pitch - i32::from(f.height)) / 2;
        let ly = |y: i32| y + (pitch - i32::from(f.label.height)) / 2;
        for (name, pos, pilot) in [("Drone", dv.drone, false), ("Pilot (reported)", dv.operator, true)] {
            self.label(name, lx, ly(y));
            let pc = if pilot { l.operator } else { col };
            match pos {
                Some(p) => {
                    let t: String<32> = fmt(format_args!(
                        "{} {} {}{}",
                        distance_short(p.distance_m).as_str(),
                        compass(p.bearing_deg),
                        degrees(p.bearing_deg).as_str(),
                        if pilot { " of you" } else { "" }
                    ));
                    let e = self.t(&t, vx, vy(y, &f.body), &f.body, pc);
                    if pilot {
                        self.cross(e as f32 + 16.0, (y + pitch / 2) as f32, 5.0, l.operator);
                    }
                }
                None if pilot && dv.drone_to_operator_m.is_some() => {
                    // No own fix, but both drone and pilot positions are known.
                    let t: String<32> = fmt(format_args!("{} from the drone", distance_short(dv.drone_to_operator_m.unwrap_or(0.0)).as_str()));
                    let e = self.t(&t, vx, vy(y, &f.body), &f.body, pc);
                    self.cross(e as f32 + 16.0, (y + pitch / 2) as f32, 5.0, l.operator);
                }
                None => {
                    let why = if !fix {
                        "unknown: this hub has no GPS position"
                    } else if pilot {
                        "pilot position not sent"
                    } else {
                        "drone sent no position"
                    };
                    self.t_fit(why, vx, y, pitch, maxw, &[f.body, f.small], l.dim);
                }
            }
            y += pitch;
        }
        self.label("Claim", lx, ly(y));
        if dv.disputed {
            let t: String<64> = match (dv.drone, dv.other_claim, dv.claims_apart_m) {
                (Some(a), Some(o), _) => fmt(format_args!(
                    "One at {} {}, one at {} {}: either may be fake",
                    distance_short(a.distance_m).as_str(),
                    compass(a.bearing_deg),
                    distance_short(o.distance_m).as_str(),
                    compass(o.bearing_deg)
                )),
                (_, _, Some(m)) => fmt(format_args!("Two claims {} apart: either may be fake", distance_short(m).as_str())),
                _ => fmt(format_args!("Two claims: either may be fake")),
            };
            self.t_fit(&t, vx, y, pitch, maxw, &[f.body, f.small, f.label], l.warn_text);
        } else {
            let t = if dv.auth_seen {
                "Sent by the drone itself; can be faked (auth seen, not checked)"
            } else {
                "Sent by the drone itself; can be faked"
            };
            self.t_fit(t, vx, y, pitch, maxw, &[f.body, f.small, f.label], col);
        }
        y += pitch;

        self.label("Height", lx, ly(y));
        let mut b: String<64> = fmt(format_args!("{}", height_words(dv)));
        if dv.above_you_m.is_some() && dv.height_m.is_some() {
            let _ = write!(
                b,
                " ({} above {})",
                metres(dv.height_m).as_str(),
                match dv.height_ref {
                    HeightRef::Takeoff => "takeoff",
                    HeightRef::Ground => "ground",
                }
            );
        }
        self.t_fit(&b, vx, y, pitch, maxw, &[f.body, f.small], col);
        y += pitch;

        self.label("Speed", lx, ly(y));
        let mut sp: String<64> = match dv.speed_mps {
            Some(v) => fmt(format_args!("{v:.1} m/s")),
            None => fmt(format_args!("--")),
        };
        if let Some(t) = dv.track_deg {
            let _ = write!(sp, " heading {}", compass(t));
        }
        if let Some(v) = dv.vspeed_mps {
            let r = libm::roundf(v * 10.0) / 10.0;
            let _ = if r > 0.0 {
                write!(sp, " \u{b7} climbing {r:.1} m/s")
            } else if r < 0.0 {
                write!(sp, " \u{b7} sinking {:.1} m/s", -r)
            } else {
                write!(sp, " \u{b7} level")
            };
        }
        self.t_fit(&sp, vx, y, pitch, maxw, &[f.body, f.small], col);
        y += pitch;

        self.label("Pilot ID", lx, ly(y));
        self.t(if dv.operator_id.is_empty() { "--" } else { dv.operator_id.as_str() }, vx, vy(y, &f.body), &f.body, col);
        y += pitch;
        // The operator's own words.
        self.label("Says", lx, ly(y));
        if dv.self_id.is_empty() {
            self.t("--", vx, vy(y, &f.body), &f.body, col);
        } else {
            let t: String<32> = fmt(format_args!("\"{}\"", dv.self_id));
            self.t_fit(&t, vx, y, pitch, maxw, &[f.body, f.small], col);
        }
        y += pitch;

        // Signal: each radio that heard it, as letter and bars.
        self.label("Signal", lx, ly(y));
        let mut x = vx;
        let mut any = false;
        for r in Radio::ALL {
            let Some(rssi) = dv.rssi[r.index()] else { continue };
            any = true;
            let mut t: String<2> = String::new();
            let _ = t.push(r.letter());
            x = self.t(&t, x, vy(y, &f.body_b), &f.body_b, col) + 6;
            self.bars(x, y + pitch / 2 + 9, bar_level(Some(i16::from(rssi))), col);
            let d: String<8> = fmt(format_args!("{rssi}"));
            x = self.t(&d, x + 32, vy(y, &f.small) + 2, &f.small, l.dim) + 18;
            if x > vx + maxw - 60 {
                break;
            }
        }
        if !any {
            self.t("--", vx, vy(y, &f.body), &f.body, col);
        }
        y += pitch;

        self.label("Heard", lx, ly(y));
        let last = s.now_ms.saturating_sub(dv.last_seen_ms);
        let first = age(s.now_ms.saturating_sub(dv.first_seen_ms));
        let t: String<48> = if last < 2000 {
            fmt(format_args!("now, first {} ago", first.as_str()))
        } else {
            fmt(format_args!("{} ago, first {} ago", age(last).as_str(), first.as_str()))
        };
        self.t_fit(&t, vx, y, pitch, maxw, &[f.body, f.small], col);
    }

    /// The detail page's right-hand card: distance and direction, large;
    /// the reported pilot and what that is worth.
    fn direction_card(&mut self, s: &UiState, dv: &DroneView, x: i32, y: i32, w: i32, h: i32) {
        let l = self.l;
        let f = self.f();
        let col = ink(l, dv.stale);
        let p = 16;
        self.label("Drone, as broadcast", x + p, y + 12);
        let Some(pos) = dv.drone else {
            let why = if s.gps.has_position() { "No position sent" } else { "Hub has no GPS position" };
            self.t(why, x + p, y + 44, &f.body_b, l.dim);
            self.t("distance unknown", x + p, y + 70, &f.small, l.dim);
            return;
        };
        let (n, unit) = distance(pos.distance_m);
        let ncol = if dv.disputed { l.dim } else { col };
        let big = if f.num_l.width(&n, 0) <= w - 2 * p - 50 && h > 250 { f.num_l } else { f.num_m };
        let ntop = y + 40;
        let e = self.num(&n, x + p, ntop, &big, ncol);
        self.unit_on(unit, e + 6, ntop, &big, &f.title, l.dim);
        let ry = ntop + i32::from(big.cap) + 44;
        if ry + 34 < y + h {
            let (ax, ay) = ((x + 52) as f32, ry as f32);
            self.rose(ax, ay, 28.0, pos.bearing_deg, col);
            self.t(compass(pos.bearing_deg), x + 100, ry - 26, &f.data_l, col);
            self.t(&degrees(pos.bearing_deg), x + 100, ry + 4, &f.body, l.dim);
        }
        let py = ry + 40;
        if let Some(o) = dv.operator
            && py + 46 < y + h
        {
            self.label("Pilot (reported)", x + p, py);
            let t: String<24> = fmt(format_args!("{} {}", distance_short(o.distance_m).as_str(), compass(o.bearing_deg)));
            let e = self.t(&t, x + p, py + 18, &f.body_b, l.operator);
            self.cross(e as f32 + 16.0, (py + 18 + i32::from(f.body_b.height) / 2) as f32, 6.0, l.operator);
            let mut ny = py + 44;
            for line in ["May be old or faked.", "Do not approach: report it."] {
                if ny + 18 > y + h - 4 {
                    break;
                }
                self.t(line, x + p, ny, &f.small, l.dim);
                ny += 18;
            }
        }
    }

    // --------------------------------------------------------- signals --

    /// Column headers: (x, text, right-aligned).
    fn header_row(&mut self, y: i32, cols: &[(i32, &str, bool)]) {
        for &(x, h, right) in cols {
            if right {
                self.label_r(h, x, y);
            } else {
                self.label(h, x, y);
            }
        }
        self.hline(16, W - 16, y + 18, self.l.edge);
    }

    fn sig_head(&mut self, y: i32, which: u8) {
        match which {
            0 => self.header_row(y, &[(M, "Control link", false), (210, "Band", true), (232, "How sure", false), (400, "Signal", false), (470, "Detail", false), (W - M, "Heard", true)]),
            1 => self.header_row(y, &[(M, "Drone video / RF", false), (375, "MHz", true), (490, "Level", true), (630, "Duty / line", true), (W - M, "Heard", true)]),
            _ => self.header_row(y, &[(M, "Drone Wi-Fi", false), (275, "Network", false), (630, "Ch", true), (660, "Signal", false), (W - M, "Heard", true)]),
        }
    }

    /// One row of section `which` (0 control, 1 RF, 2 Wi-Fi), item `i`.
    fn sig_row(&mut self, s: &UiState, y: i32, which: u8, i: usize) {
        let l = self.l;
        let f = self.f();
        match which {
            0 => {
                let c = &s.control[i];
                let col = ink(l, c.stale);
                self.t(&c.system, M, y, &f.body_b, col);
                self.tr(c.band, 210, y, &f.data, col);
                self.t(c.sure.label(), 232, y, &f.body, col);
                self.bars(400, y + 20, bar_level(Some(c.rssi)), col);
                if !raw_numbers(&c.detail) {
                    self.t(&c.detail, 470, y, &f.small, col);
                }
                self.tr(&age(s.now_ms.saturating_sub(c.last_seen_ms)), W - M, y, &f.data, col);
            }
            1 => {
                let e = &s.emitters[i];
                let col = ink(l, e.stale);
                self.t(e.kind.label(), M, y, &f.body_b, col);
                let t: String<12> = fmt(format_args!("{:.1}", e.freq_mhz));
                self.tr(&t, 375, y, &f.data, col);
                let t: String<12> = fmt(format_args!("+{} dB", e.snr_db.max(0)));
                self.tr(&t, 490, y, &f.data, col);
                let t: String<12> = if e.line_hz != 0 {
                    fmt(format_args!("{:.1} kHz", f32::from(e.line_hz) / 1000.0))
                } else {
                    fmt(format_args!("{}%", e.duty_pct))
                };
                self.tr(&t, 630, y, &f.data, col);
                self.tr(&age(s.now_ms.saturating_sub(e.last_seen_ms)), W - M, y, &f.data, col);
            }
            _ => {
                let w = &s.wifi_drones[i];
                let col = ink(l, w.stale);
                self.t(&w.maker, M, y, &f.body_b, col);
                self.t(&w.ssid, 275, y, &f.data, col);
                let t: String<8> = fmt(format_args!("{}", w.channel));
                self.tr(&t, 630, y, &f.data, col);
                self.bars(660, y + 20, bar_level(Some(i16::from(w.rssi))), col);
                self.tr(&age(s.now_ms.saturating_sub(w.last_seen_ms)), W - M, y, &f.data, col);
            }
        }
    }

    pub fn signals(&mut self, s: &UiState) {
        let l = self.l;
        let f = self.f();
        const RH: i32 = 26;
        let y0 = self.top + 6;
        let e = self.header("SIGNALS", s);
        self.mode_chip(e + 16, y0 + 2);
        let t: String<64> = fmt(format_args!(
            "{} control links \u{b7} {} RF \u{b7} {} Wi-Fi",
            s.total_control.max(s.control.len() as u16),
            s.total_emitters.max(s.emitters.len() as u16),
            s.total_wifi_drones.max(s.wifi_drones.len() as u16)
        ));
        self.tr(&t, W - M, y0 + 8, &f.small, l.dim);
        self.t(
            "Drone radio links with no ID or position. A control link can also be an RC car or plane.",
            M,
            y0 + 36,
            &f.small,
            l.dim,
        );
        let mut y = y0 + 62;
        let counts = [s.control.len(), s.emitters.len(), s.wifi_drones.len()];
        // Scrolling shows one list: three section heads and every row.
        let scroll_rows = 3 + counts.iter().sum::<usize>();
        let scroll_fit = ((self.bottom - y - 20) / RH).max(1) as usize;
        let scroll_max = scroll_rows.saturating_sub(scroll_fit).min(255) as u8;
        if self.ui.mode() == Mode::Scroll {
            // One list, all three sections, from the scroll offset.
            let mut items: heapless::Vec<(u8, Option<usize>), 32> = heapless::Vec::new();
            for (which, &n) in counts.iter().enumerate() {
                let _ = items.push((which as u8, None));
                for i in 0..n {
                    let _ = items.push((which as u8, Some(i)));
                }
            }
            let fit = scroll_fit;
            let max = usize::from(scroll_max);
            self.ui.note_overflow(Page::Signals, true, scroll_max);
            let start = usize::from(self.ui.scroll()).min(max);
            for &(which, i) in items.iter().skip(start).take(fit) {
                match i {
                    None => self.sig_head(y + 4, which),
                    Some(i) => self.sig_row(s, y, which, i),
                }
                y += RH;
            }
            let t: String<24> = fmt(format_args!("rows {}-{} of {}", start + 1, (start + fit).min(items.len()), items.len()));
            self.tr(&t, W - M, self.bottom - 20, &f.small, l.dim);
            return;
        }
        let mut hidden = false;
        self.sig_head(y, 0);
        y += 24;
        if s.control.is_empty() {
            self.t("none heard", 40, y, &f.body, l.dim);
            y += RH;
        }
        for i in 0..s.control.len().min(3) {
            self.sig_row(s, y, 0, i);
            y += RH;
        }
        y += 6;
        self.sig_head(y, 1);
        y += 24;
        // Leave room for the Wi-Fi table: header and two rows.
        let rf_rows = ((self.bottom - y - 8 - 24 - 2 * RH - 20) / RH).max(1) as usize;
        if s.emitters.is_empty() {
            self.t("none heard", 40, y, &f.body, l.dim);
            y += RH;
        }
        for i in 0..s.emitters.len().min(rf_rows) {
            self.sig_row(s, y, 1, i);
            y += RH;
        }
        let more = usize::from(s.total_emitters).max(s.emitters.len()).saturating_sub(rf_rows.min(s.emitters.len()));
        if more > 0 {
            hidden |= s.emitters.len() > rf_rows;
            let t: String<16> = fmt(format_args!("+{more} more"));
            self.tr(&t, W - M, y, &f.small, l.dim);
            y += 20;
        }
        y += 6;
        self.sig_head(y, 2);
        y += 24;
        if s.wifi_drones.is_empty() {
            self.t("none heard", 40, y, &f.body, l.dim);
        }
        let mut shown = 0;
        for i in 0..s.wifi_drones.len() {
            if y + RH > self.bottom - 20 {
                break;
            }
            self.sig_row(s, y, 2, i);
            y += RH;
            shown += 1;
        }
        hidden |= shown < s.wifi_drones.len();
        let more = usize::from(s.total_wifi_drones).max(s.wifi_drones.len()).saturating_sub(shown);
        if more > 0 {
            let t: String<16> = fmt(format_args!("+{more} more"));
            self.tr(&t, W - M, self.bottom - 20, &f.small, l.dim);
        }
        self.ui.note_overflow(Page::Signals, hidden && scroll_max > 0, scroll_max);
    }

    // ----------------------------------------------------------- flags --

    pub fn flags(&mut self, s: &UiState) {
        let l = self.l;
        let f = self.f();
        let y0 = self.top + 6;
        let e = self.header("FLAGS", s);
        self.mode_chip(e + 16, y0 + 2);
        let Some(fl) = &s.flags else {
            self.t("no rule engine on this hub", M, y0 + 50, &f.body, l.dim);
            return;
        };
        self.tr(&fl.pack, W - M, y0 + 8, &f.small, l.dim);
        // The wording rule (PLAN.md): possible, never "illegal".
        self.t("Possible rule issues, based on what drones broadcast (IDs and positions can be faked).", M, y0 + 36, &f.small, l.dim);
        self.t("The hub can't see waivers or LAANC.", M, y0 + 54, &f.small, l.dim);
        let mut y = y0 + 80;
        const C_WHO: i32 = 120;
        const C_RULE: i32 = 316;
        const C_EV: i32 = 400;
        self.header_row(y, &[(M, "Level", false), (C_WHO, "Who", false), (C_RULE, "Rule", false), (C_EV, "Evidence", false), (W - M, "For", true)]);
        y += 22;
        if fl.rows.is_empty() {
            self.t("none", 40, y + 8, &f.body, l.dim);
        }
        // Worst first, then longest held.
        let idx = fl.order();
        let rh = 40;
        let choosing = self.ui.mode() == Mode::Scroll;
        let cur = choosing.then(|| usize::from(self.ui.scroll()));
        let fit = ((self.bottom - y - 20) / rh).max(1) as usize;
        let start = match cur {
            Some(c) if c >= fit => c + 1 - fit,
            _ => 0,
        };
        let pill_w = f.small_b.width("SERIOUS", 0) + 16;
        // One evidence face for the page: the body face unless a row would
        // not fit in it.
        let ev_w = |r: &FlagRow| W - M - f.data.width(&age(u64::from(r.secs) * 1000), 0) - 14 - C_EV;
        let ev_f = if fl.rows.iter().all(|r| f.body.width(&r.text, 0) <= ev_w(r)) { f.body } else { f.small };
        let tapped = self.ui.tapped();
        let mut shown = 0;
        for (pos, &i) in idx.iter().enumerate().skip(start).take(fit) {
            let r = &fl.rows[usize::from(i)];
            let ui_row = pos;
            self.hit(Spot::Rect(0, y, W, rh), Tap::Flag(usize::from(i)));
            if cur == Some(ui_row) || tapped == Some(Tap::Flag(usize::from(i))) {
                self.c.rrect(8, y + 1, W - 16, rh - 3, l.radius.min(8), l.sel_fill);
                self.c.rrect_stroke(8, y + 1, W - 16, rh - 3, l.radius.min(8), 2.0, l.sel_edge);
            }
            let word: String<12> = upper(FlagRow::level_word(r.level));
            let py = y + (rh - i32::from(f.small_b.height) - 6) / 2;
            let ty = y + (rh - i32::from(f.body.height)) / 2;
            let tw = f.small_b.width(&word, 0);
            // Fixed-width pills: serious filled, likely outlined, the rest
            // muted.
            match r.level {
                3.. => {
                    self.c.rrect(M, py, pill_w, i32::from(f.small_b.height) + 6, 4, l.warn);
                    self.t(&word, M + (pill_w - tw) / 2, py + 3, &f.small_b, l.warn_ink);
                }
                2 => {
                    self.c.rrect_stroke(M, py, pill_w, i32::from(f.small_b.height) + 6, 4, 1.5, l.warn_text);
                    self.t(&word, M + (pill_w - tw) / 2, py + 3, &f.small_b, l.warn_text);
                }
                1 => {
                    self.c.rrect_stroke(M, py, pill_w, i32::from(f.small_b.height) + 6, 4, 1.0, l.faint);
                    self.t(&word, M + (pill_w - tw) / 2, py + 3, &f.small_b, l.dim);
                }
                _ => {
                    self.t(&word, M + (pill_w - tw) / 2, py + 3, &f.small_b, l.dim);
                }
            }
            let col = if r.level >= 2 { l.fg } else { l.dim };
            let e = self.t_fit(&r.who, C_WHO, y, rh, C_RULE - C_WHO - (if r.aircraft { 54 } else { 6 }), &[f.small_b, f.small], col);
            if r.aircraft {
                self.outlined("plane", e + 6, y + (rh - i32::from(f.label.height) - 4) / 2, &f.label, l.dim, 4);
            }
            self.t(r.rule, C_RULE, ty, &f.body_b, col);
            let a = age(u64::from(r.secs) * 1000);
            let aw = f.data.width(&a, 0);
            self.t_fit(&r.text, C_EV, y, rh, W - M - aw - 14 - C_EV, &[ev_f], col);
            self.tr(&a, W - M, y + (rh - i32::from(f.data.height)) / 2, &f.data, col);
            y += rh;
            shown += 1;
        }
        let more = usize::from(fl.total).max(fl.rows.len()).saturating_sub(shown);
        if more > 0 {
            let t: String<16> = fmt(format_args!("+{more} more"));
            self.tr(&t, W - M, self.bottom - 20, &f.small, l.dim);
        }
    }

    // ---------------------------------------------------------- health --

    pub fn health(&mut self, s: &UiState) {
        use core::fmt::Write;
        let l = self.l;
        let f = self.f();
        let y0 = self.top + 6;
        self.header("HEALTH", s);
        let up: String<32> = fmt(format_args!("hub up {}", uptime(s.hub_uptime_s).as_str()));
        self.tr(&up, W - M, y0 + 8, &f.small, l.dim);

        let roles = s.assigned.iter().any(|a| a.is_some());
        let dead = s.nodes.iter().filter(|n| !n.alive).count() as i32;
        let rh = if s.nodes.len() > 4 { 28 } else { 32 };
        let card_y = y0 + 38;
        let card_h = 30 + rh * (s.nodes.len().max(1) as i32) + dead * 20 + 4;
        self.card(16, card_y, W - 32, card_h);
        let hy = card_y + 8;
        self.label("Node", 40, hy);
        if roles {
            self.label("Assigned role", 200, hy);
        }
        for (x, h) in [(410, "Frames/s"), (540, "ID messages"), (615, "Lost"), (W - 36, "Up")] {
            self.label_r(h, x, hy);
        }
        let mut y = card_y + 30;
        for (i, n) in s.nodes.iter().enumerate() {
            self.hline(32, W - 32, y, l.edge);
            let ty = y + (rh - i32::from(f.body_b.height)) / 2;
            self.node_dot(46.0, (y + rh / 2) as f32, 7.0, n, l.ok, l.warn_text);
            let nc = if n.alive && !n.deaf() { l.fg } else { l.warn_text };
            self.t(n.kind.long(), 62, ty, &f.body_b, nc);
            if let Some(Some(a)) = s.assigned.get(i) {
                let t: String<48> = fmt(format_args!("{} plan {:04x} {}", a.role, a.plan, if a.applied { "running" } else { "sent" }));
                self.t(&t, 200, y + (rh - i32::from(f.small.height)) / 2, &f.small, if a.applied { l.dim } else { l.warn_text });
            }
            let dy = y + (rh - i32::from(f.data.height)) / 2;
            if n.alive {
                let fr: String<12> = match n.frames_per_s {
                    Some(v) => fmt(format_args!("{v}")),
                    None => fmt(format_args!("--")),
                };
                if n.deaf() {
                    // Alive, hearing nothing: a caution of its own.
                    let w = f.small_b.width("HEARS NOTHING", 0) + 12;
                    self.outlined("HEARS NOTHING", 410 - w, y + (rh - i32::from(f.small_b.height) - 6) / 2, &f.small_b, l.warn_text, 6);
                } else {
                    self.tr(&fr, 410, dy, &f.data, l.fg);
                }
                for (x, v) in [(540, n.rid_found), (615, n.drops)] {
                    let t: String<12> = fmt(format_args!("{v}"));
                    self.tr(&t, x, dy, &f.data, l.fg);
                }
                self.tr(&uptime(n.uptime_s), W - 36, dy, &f.data, l.dim);
                y += rh;
            } else {
                let w = f.small_b.width("NOT RESPONDING", 0) + 12;
                self.caution("Not responding", W - 36 - w, y + (rh - i32::from(f.small_b.height) - 6) / 2);
                let mut t: String<96> = String::new();
                if let Some(q) = n.silent_s {
                    let _ = write!(t, "last heard {} ago \u{b7} ", uptime(q).as_str());
                }
                let _ = write!(t, "{} won't be heard \u{b7} check power, cable, range", n.kind.unheard());
                self.t_fit(&t, 62, y + rh - 4, 20, W - 36 - 62, &[f.small, f.label], l.warn_text);
                y += rh + 20;
            }
        }
        if s.nodes.is_empty() {
            self.t("no radio nodes reported: the hub hears nothing", 62, y + 6, &f.body_b, l.warn_text);
        }

        // Tiles: label and value, two columns, one value size for all.
        let mut items: heapless::Vec<(&str, String<96>, bool), 10> = heapless::Vec::new();
        let gps: String<96> = if s.gps.untrusted_pos || s.gps.untrusted_time {
            fmt(format_args!(
                "no trusted {}",
                match (s.gps.untrusted_pos, s.gps.untrusted_time) {
                    (true, true) => "position or time",
                    (true, false) => "position",
                    _ => "time",
                }
            ))
        } else {
            match s.gps.fix {
                Fix::None => fmt(format_args!("no fix \u{b7} {} satellites", s.gps.sats)),
                Fix::Fix2D => fmt(format_args!("2D fix \u{b7} {} satellites", s.gps.sats)),
                Fix::Fix3D => fmt(format_args!("3D fix \u{b7} {} satellites", s.gps.sats)),
                Fix::Manual => fmt(format_args!("no fix; typed position in use")),
                Fix::Absent => fmt(format_args!("no receiver found")),
            }
        };
        let bad_gps = s.gps.untrusted_pos || s.gps.untrusted_time || matches!(s.gps.fix, Fix::None | Fix::Absent);
        let _ = items.push(("GPS", gps, bad_gps));
        let total = usize::from(s.total_drones).max(s.drones.len());
        let _ = items.push(("Drones seen", fmt(format_args!("{} ({} now)", total, s.active_count())), false));
        let (sd, sd_bad) = match s.sd {
            SdLog::Off => ("off", false),
            SdLog::Recording if s.caveats.evidence.is_some() => ("CSV only \u{b7} NO EVIDENCE", true),
            SdLog::Recording => ("recording", false),
            SdLog::NoCard => ("no card", false),
            SdLog::Error => ("ERROR \u{b7} not recording", true),
        };
        let _ = items.push(("SD log", fmt(format_args!("{sd}")), sd_bad));
        let power: String<96> = match s.power {
            Power::Usb { battery_pct: Some(p) } => fmt(format_args!("USB \u{b7} battery {p}%")),
            Power::Usb { battery_pct: None } => fmt(format_args!("USB")),
            Power::Battery { pct } => fmt(format_args!("battery {pct}%")),
        };
        let _ = items.push(("Power", power, matches!(s.power, Power::Battery { pct } if pct <= 15)));
        if let Some(c) = &s.conflict {
            let mut t: String<96> = fmt(format_args!("drone {} from {}", distance_short(c.range_m).as_str(), c.aircraft));
            if let Some(z) = c.dz_m {
                let _ = write!(t, ", {:+} m ({:+} ft) height", libm::roundf(z) as i32, (z * 3.28084) as i32);
            }
            let _ = items.push(("Near a plane", t, c.level >= 2));
        }
        if let Some(g) = &s.area_gnss {
            let mut t: String<96> = String::new();
            if g.spoofing {
                let _ = t.push_str("FAKED:");
                if g.drones_moved > 0 {
                    let _ = write!(t, " {} drones", g.drones_moved);
                }
                if g.aircraft_moved > 0 {
                    let _ = write!(t, " {} planes", g.aircraft_moved);
                }
                let _ = t.push_str(" moved together");
            } else {
                let _ = write!(t, "JAMMED: {}/{} planes report it", g.degraded, g.counted);
            }
            if g.own_agrees {
                let _ = t.push_str(", hub agrees");
            }
            let _ = items.push(("Area GPS", t, true));
        }
        let (th, q) = (s.threats, s.caveats.silenced);
        if th.active > 0 || th.muted > 0 {
            let mut t: String<96> = fmt(format_args!("{} active \u{b7} {} silenced", th.active, th.muted));
            if q.snoozed > 0 {
                let _ = t.push_str(" (snooze)");
            }
            let _ = items.push(("Alarms", t, th.active > 0));
        }
        let notes = s.caveats.notes();
        if !notes.is_empty() {
            let mut t: String<96> = String::new();
            for (i, n) in notes.iter().enumerate() {
                if i > 0 && t.push_str("; ").is_err() {
                    break;
                }
                if t.push_str(n).is_err() {
                    break;
                }
            }
            let _ = items.push(("Notes", t, false));
        }
        // The aircraft line gets the full width (it is long).
        let aircraft: Option<String<96>> = s.aircraft.as_ref().map(|a| {
            let mut t: String<96> = String::new();
            if a.total == 0 {
                let _ = t.push_str("none heard");
            } else {
                let _ = write!(t, "{} active", a.active);
            }
            if a.low_nearby > 0 {
                let _ = write!(t, " \u{b7} {} LOW nearby", a.low_nearby);
            }
            if let Some(n) = &a.nearest {
                let _ = write!(t, " \u{b7} nearest {} {}", n.name, distance_short(n.distance_m).as_str());
                if let Some(ft) = n.alt_ft {
                    let _ = write!(t, " at {ft} ft");
                }
            }
            t
        });
        let ty0 = card_y + card_h + 8;
        // Warnings first, so a cramped page still shows what is wrong; the
        // aircraft line takes a whole row.
        let low = s.aircraft.as_ref().is_some_and(|a| a.low_nearby > 0);
        let mut order: heapless::Vec<(bool, &str, &str, bool), 11> = heapless::Vec::new();
        if let (Some(a), true) = (&aircraft, low) {
            let _ = order.push((true, "Planes", a.as_str(), true));
        }
        for (k, v, bad) in items.iter().filter(|i| i.2) {
            let _ = order.push((false, k, v.as_str(), *bad));
        }
        for (k, v, bad) in items.iter().filter(|i| !i.2) {
            let _ = order.push((false, k, v.as_str(), *bad));
        }
        if let (Some(a), false) = (&aircraft, low) {
            let _ = order.push((true, "Planes", a.as_str(), false));
        }
        // Rows needed: full-width ones, then pairs.
        let mut need = 0;
        let mut half = false;
        for (full, ..) in &order {
            if *full {
                need += 1;
                half = false;
            } else if half {
                half = false;
            } else {
                need += 1;
                half = true;
            }
        }
        let avail = self.bottom - 4 - ty0;
        // As many rows as fit at 30 px or more, sharing what is left.
        let rows_fit = (avail / 30).min(need.max(1));
        if rows_fit <= 0 {
            return;
        }
        let th_ = (avail / rows_fit).min(58);
        let tw = (W - 32 - 10) / 2;
        let compact = th_ < 50;
        // One value font for every tile: the body face unless one value
        // would not fit, then the small face for all.
        let vw = |full: bool| {
            let w = if full { W - 32 } else { tw };
            if compact { w - 160 } else { w - 28 }
        };
        let all_fit = order.iter().all(|(full, _, v, _)| f.body.width(v, 0) <= vw(*full));
        let vf = if all_fit { f.body } else { f.small };
        let tile = |p: &mut Self, k: &str, v: &str, bad: bool, x: i32, y: i32, w: i32| {
            let h = th_ - 6;
            p.card(x, y, w, h);
            let c = if bad { l.warn_text } else { l.fg };
            if bad {
                // Not colour alone: a heavy bar down the tile's left edge
                // (and the words say what is wrong).
                p.c.rrect(x, y, 6, h, 2, l.warn);
            }
            if compact {
                p.label(k, x + 16, y + (h - i32::from(f.label.height)) / 2);
                p.t_fit(v, x + 150, y, h, w - 160, &[vf], c);
            } else {
                p.label(k, x + 16, y + 7);
                p.t_fit(v, x + 16, y + 22, h - 26, w - 28, &[vf], c);
            }
        };
        let (mut row, mut col, mut shown) = (0, 0, 0);
        for (full, k, v, bad) in &order {
            if *full && col == 1 {
                row += 1;
                col = 0;
            }
            if row >= rows_fit {
                break;
            }
            let y = ty0 + row * th_;
            if *full {
                tile(self, k, v, *bad, 16, y, W - 32);
                row += 1;
            } else {
                tile(self, k, v, *bad, 16 + col * (tw + 10), y, tw);
                col += 1;
                if col == 2 {
                    col = 0;
                    row += 1;
                }
            }
            shown += 1;
        }
        if shown < order.len() {
            // No room for every tile: say so (the status bar and band still
            // carry the warnings).
            let t: String<32> = fmt(format_args!("{} more lines: no room \u{b7} ", order.len() - shown));
            self.tr(&t, W - M - f.small.width(&up, 0), y0 + 8, &f.small, l.warn_text);
        }
    }

    // -------------------------------------------------------- settings --

    pub fn settings(&mut self, s: &UiState) {
        use core::fmt::Write;
        let l = self.l;
        let f = self.f();
        let ui = self.ui;
        let y0 = self.top + 6;
        let e = self.header("SETTINGS", s);
        self.mode_chip(e + 16, y0 + 2);
        let st = ui.settings();
        let editing = ui.mode() == Mode::Edit;
        let rows = ui.rows();
        let auto = st.theme == ThemeMode::Auto;
        let note_h = 28;
        let top = y0 + 42;
        let divider = 22;
        let avail = self.bottom - top - note_h - divider - 2;
        // All rows at up to 40 px each; when there is too little room,
        // 30 px rows that scroll to keep the cursor in view.
        let (pitch, fit) = if avail / rows.len() as i32 >= 30 {
            ((avail / rows.len() as i32).min(40), rows.len())
        } else {
            (30, (avail / 30).max(1) as usize)
        };
        let cur = if editing { ui.settings_row() } else { 0 };
        let start = if cur >= fit { cur + 1 - fit } else { 0 };
        if fit < rows.len() {
            // Scrolled: say which rows show, beside the title.
            let t: String<24> = fmt(format_args!("rows {}-{} of {}", start + 1, start + fit, rows.len()));
            self.tr(&t, W - M, y0 + 8, &f.small, l.dim);
        }
        let mut v: String<64> = String::new();
        let mut y = top;
        let tapped = ui.tapped();
        let first_action = rows.iter().position(|r| r.is_action());
        for (i, row) in rows.iter().enumerate().skip(start).take(fit) {
            if Some(i) == first_action {
                // Actions: things the hub does once, apart from settings.
                self.label("Actions", 48, y + 6);
                self.hline(150, W - 16, y + 12, l.edge);
                y += divider;
            }
            self.hit(Spot::Rect(0, y, W, pitch), Tap::SettingsRow(i));
            let sel = (editing && ui.settings_row() == i) || tapped == Some(Tap::SettingsRow(i));
            if sel {
                self.c.rrect(8, y, W - 16, pitch - 4, l.radius.min(8), l.sel_fill);
                self.c.rrect_stroke(8, y, W - 16, pitch - 4, l.radius.min(8), 2.0, l.sel_edge);
                self.tri(24.0, (y + pitch / 2 - 2) as f32, 7.0, Arrow::Right, l.sel_edge);
            }
            // Auto's own rows are indented under Theme.
            let sub = matches!(row, SettingsRow::DayTheme | SettingsRow::NightTheme | SettingsRow::SwitchAt);
            let k = crate::render::settings_text(ui, s, *row, false, &mut v);
            let ty = y + (pitch - 4 - i32::from(f.body_b.height)) / 2;
            let nx = if sub { 72 } else { 48 };
            if sub {
                self.c.fill(56, y + 4, 2, pitch - 12, l.grid);
            }
            self.t(k, nx, ty, if sub { &f.body } else { &f.body_b }, if sub { l.dim } else { l.fg });
            let vx = 330;
            if *row == SettingsRow::Brightness {
                for lvl in 0..5u8 {
                    let x = vx + i32::from(lvl) * 30;
                    let h = 8 + i32::from(lvl) * 4;
                    let b = y + pitch - 10;
                    if lvl < st.brightness {
                        self.c.fill(x, b - h, 22, h, l.fg);
                    } else {
                        self.c.frame(x, b - h, 22, h, 1, l.ring);
                    }
                }
                // "3", or "3 (2 in Red Night)".
                self.t(&v, vx + 160, ty, &f.body, if v.len() > 1 { l.dim } else { l.fg });
                if ui.adjusting() && vx + 160 + f.body.width(&v, 0) + 16 + f.small.width("left / right keys", 0) < W - M {
                    self.t("left / right keys", vx + 160 + f.body.width(&v, 0) + 16, ty + 2, &f.small, l.dim);
                }
            } else if row.is_action() {
                // An action chip: outline and a chevron, not a value.
                let act = ui.action().filter(|a| a.row == *row);
                let failed = matches!(act.map(|a| a.state), Some(ActionState::Failed(_)));
                let done = matches!(act.map(|a| a.state), Some(ActionState::Done));
                let confirm = ui.confirming() == Some(*row);
                let idle = act.is_none() && Ui::row_request(*row, s).is_none();
                let c = if failed || confirm {
                    l.warn_text
                } else if idle {
                    l.dim
                } else {
                    l.fg
                };
                let w = f.body_b.width(&v, 0) + 50;
                let ch = pitch - 12;
                self.c.rrect_stroke(vx - 10, y + 4, w, ch, l.radius.min(6), 1.5, if confirm { l.warn_text } else { l.ring });
                let e = self.t(&v, vx, ty, &f.body_b, c);
                if done {
                    self.check(e as f32 + 10.0, (y + pitch / 2 - 2) as f32, 12.0, l.fg);
                } else if !idle {
                    self.chevron(e as f32 + 14.0, (y + pitch / 2 - 2) as f32, 6.0, c);
                }
            } else {
                let vv: String<64> = if *row == SettingsRow::Theme && auto { fmt(format_args!("Auto")) } else { v.clone() };
                self.t(&vv, vx, ty, &f.body_b, l.fg);
            }
            y += pitch;
        }
        // The note line: a warning for "Use this spot" while GPS may be
        // faked, else what Auto is doing.
        let doubtful = s.gps.home_pending && s.gps.integrity != GpsIntegrity::Ok;
        let mut t: String<128> = String::new();
        let mut warn = false;
        if doubtful {
            let _ = t.push_str("GPS may be faked: use this spot only if you really moved the hub");
            warn = true;
        } else if auto {
            let theme = ui.effective_theme(s).name();
            let hhmm = |ms: u64| {
                let t = (ms / 1000) % 86_400;
                fmt::<8>(format_args!("{:02}:{:02}", t / 3600, t % 3600 / 60))
            };
            match ui.auto_status(s) {
                AutoStatus::Waiting { day: None } => {
                    let _ = write!(t, "Waiting for GPS \u{b7} showing {theme} until it knows the time");
                    warn = true;
                }
                AutoStatus::Waiting { day: Some(_) } => {
                    let _ = write!(t, "No GPS time \u{b7} keeping {theme}");
                    warn = true;
                }
                AutoStatus::Sun { day, next_utc_ms, stale_position } => {
                    let _ = write!(t, "Now: {theme} ({})", if day { "daytime" } else { "night" });
                    match (next_utc_ms, s.utc_ms) {
                        (Some(n), Some(now)) => {
                            let other = if day { st.night_theme } else { st.day_theme }.name();
                            let mins = n.saturating_sub(now) / 60_000;
                            let _ = write!(t, " \u{b7} {other} at {}, in ", st.switch_at.moment(!day));
                            let _ = if mins >= 60 { write!(t, "{} h {:02} min", mins / 60, mins % 60) } else { write!(t, "{mins} min") };
                            let _ = write!(t, " ({} UTC)", hhmm(n).as_str());
                        }
                        _ => {
                            let _ = write!(t, " \u{b7} no switch within a day (polar {})", if day { "day" } else { "night" });
                        }
                    }
                    if stale_position {
                        let _ = t.push_str(", last fix");
                    }
                }
                AutoStatus::Off => {}
            }
        }
        if !t.is_empty() {
            self.t_fit(&t, M, self.bottom - note_h, note_h, W - 2 * M, &[f.body, f.small], if warn { l.warn_text } else { l.dim });
        }
    }

    // -------------------------------------------------------- overlays --

    /// The new-drone banner, flush under the status bar (and band): the
    /// nearest or most serious new drone, as broadcast.
    pub fn alert(&mut self, s: &UiState) {
        use core::fmt::Write;
        let l = self.l;
        let f = self.f();
        let Some(a) = self.ui.alert() else { return };
        // The page behind, faded: the banner is the thing to look at.
        self.c.veil(0, self.top, W, self.bottom - self.top, l.bg, 150);
        let (x, y, w) = (8, self.top, W - 16);
        let h = 214.min(self.bottom - self.top - 4);
        self.hit(Spot::Rect(x, y, w, h), Tap::Banner);
        let r = l.radius + 4;
        let ink = if l.one_hue {
            // One hue: an outlined banner with an inverse title block.
            self.c.rrect(x, y, w, h, r, l.bg);
            self.c.rrect_stroke(x, y, w, h, r, 4.0, l.accent);
            let tw = f.head.width("NEW DRONE", 0) + 70;
            self.c.rrect(x + 16, y + 12, tw, 44, 2, l.accent);
            self.diamond((x + 40) as f32, (y + 34) as f32, 11.0, l.bg, true);
            self.t("NEW DRONE", x + 60, y + 34 - i32::from(f.head.height) / 2, &f.head, l.bg);
            l.accent
        } else {
            self.c.rrect(x, y, w, h, r, l.accent);
            self.diamond((x + 34) as f32, (y + 34) as f32, 12.0, l.accent_ink, true);
            self.t("NEW DRONE", x + 58, y + 34 - i32::from(f.head.height) / 2, &f.head, l.accent_ink);
            l.accent_ink
        };
        let back = if l.one_hue { l.bg } else { l.accent };
        let mut tx = x + 60 + f.head.width("NEW DRONE", 0) + 40;
        let more = a.more;
        if more > 0 {
            let t: String<20> = fmt(format_args!("+{more} more new"));
            tx = self.boxed(&t, tx, y + 20, &f.small_b, back, ink, 6) + 12;
        }
        if a.sticky {
            self.t("close by: stays until you press a key", tx, y + 24, &f.small, ink);
        }
        let Some(dv) = self.ui.alert_drone(s) else {
            self.t("(already gone)", x + 24, y + 90, &f.title, ink);
            return;
        };
        let lx = x + 22;
        self.t(&short_id(dv, 22), lx, y + 64, &f.id, ink);
        let mut t: String<96> = fmt(format_args!("{} \u{b7} {}", dv.id_kind.label(), radio_words(dv.radios, 20).as_str()));
        if let Some(o) = dv.drone_to_operator_m {
            let _ = write!(t, " \u{b7} pilot {} from it", distance_short(o).as_str());
        }
        let textw = w - 300;
        self.t_fit(&t, lx, y + 106, 24, textw, &[f.body, f.small], ink);
        let b: String<64> = fmt(format_args!("{} \u{b7} heard {}", height_words(dv).as_str(), age(s.now_ms.saturating_sub(dv.last_seen_ms)).as_str()));
        self.t_fit(&b, lx, y + 134, 24, textw, &[f.body, f.small], ink);
        self.t("Position as broadcast by the drone", lx, y + 166, &f.small, ink);
        match dv.drone {
            Some(pos) => {
                let (n, unit) = distance(pos.distance_m);
                let big = if h >= 200 { f.num_xl } else { f.num_l };
                let right = x + w - 80;
                let nx = right - big.width(&n, 0);
                let ntop = y + 62;
                self.num(&n, nx, ntop, &big, ink);
                self.unit_on(unit, right + 8, ntop, &big, &f.head, ink);
                // Direction: a north-up rose and the words.
                let ry = ntop + i32::from(big.cap) + 40;
                if ry + 30 < y + h {
                    let rx = (x + w - 250) as f32;
                    self.c.ring(rx, ry as f32, 22.0, 2.0, ink, None);
                    let lf = f.label;
                    let nw = lf.width("N", 0);
                    let ny = ry - 22 - i32::from(lf.height) / 2;
                    self.c.fill(rx as i32 - nw / 2 - 2, ny, nw + 4, i32::from(lf.height), back);
                    self.tc("N", rx as i32, ny, &lf, ink);
                    self.arrow(rx, ry as f32, 30.0, pos.bearing_deg, ink, 3.5);
                    let t: String<24> = fmt(format_args!("to the {} ({})", compass(pos.bearing_deg), degrees(pos.bearing_deg).as_str()));
                    self.t(&t, rx as i32 + 34, ry - i32::from(f.body_b.height) / 2, &f.body_b, ink);
                }
            }
            None => {
                let why = if s.gps.has_position() { "no position yet" } else { "this hub has no GPS position" };
                self.tr(why, x + w - 24, y + 110, &f.body_b, ink);
            }
        }
    }
}

/// The drone the radar's readout describes: the cursor while choosing,
/// otherwise the nearest active drone with a position.
fn focus<'a>(ui: &Ui, s: &'a UiState, order: &[u8]) -> Option<(usize, &'a DroneView, bool)> {
    if let Some(k) = ui.cursor()
        && let Some(i) = order.iter().position(|&i| s.drones[usize::from(i)].key == k)
    {
        return Some((i, &s.drones[usize::from(order[i])], true));
    }
    order
        .iter()
        .enumerate()
        .map(|(r, &i)| (r, &s.drones[usize::from(i)]))
        .find(|(_, dv)| !dv.stale && dv.drone.is_some())
        .map(|(r, dv)| (r, dv, false))
}
