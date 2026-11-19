//! Page layouts at 800x480.

use embedded_graphics::pixelcolor::Rgb565;
use heapless::String;

use super::canvas::Canvas;
use super::font::{Font, FontSet};
use super::look::{KeyStyle, Look};
use super::{Arrow, BAR_H, CAVEAT_H, KEYS_TOP, KeyLabel, W, fmt, ink, key_rect};
use crate::draw::{age, compass, degrees, distance, distance_short, metres, short_id, uptime};
use crate::model::*;
use crate::ui::{AutoStatus, Button, Mode, SettingsRow, ThemeMode, Ui};

/// Range rings, metres. The outer one is the plot's edge.
const RINGS: [(f32, &str); 4] = [(100.0, "100 m"), (300.0, "300 m"), (1000.0, "1 km"), (3000.0, "3 km")];
const LOG_K: f32 = 30.0;
/// Page margin.
const M: i32 = 18;

/// One band's drawing: the canvas, the theme, and where the page ends
/// (above the caveats strip and the keys).
pub(crate) struct P<'x, 'a> {
    pub c: &'x mut Canvas<'a>,
    pub l: &'static Look,
    pub bottom: i32,
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

impl P<'_, '_> {
    fn f(&self) -> &'static FontSet {
        self.l.fonts
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
    /// A label: capitals, spaced out, in the label face.
    fn label(&mut self, s: &str, x: i32, y: i32, c: Rgb565) -> i32 {
        let u: String<48> = upper(s);
        let f = self.f().label;
        self.c.text(&u, x, y, &f, c, 1)
    }
    fn label_r(&mut self, s: &str, right: i32, y: i32, c: Rgb565) -> i32 {
        let u: String<48> = upper(s);
        let f = self.f().label;
        let x = right - f.width(&u, 1);
        self.c.text(&u, x, y, &f, c, 1);
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
    /// A caution: inverse box in the warning colour, capitals.
    fn caution(&mut self, s: &str, x: i32, y: i32) -> i32 {
        let f = self.f().small_b;
        let u: String<40> = upper(s);
        self.boxed(&u, x, y, &f, self.l.warn_ink, self.l.warn, 6)
    }
    /// Big numerals placed by the top of the digits.
    fn num(&mut self, s: &str, x: i32, cap_top: i32, f: &Font, c: Rgb565) -> i32 {
        let y = cap_top + i32::from(f.cap) - i32::from(f.ascent);
        self.c.text(s, x, y, f, c, 0)
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
    /// An arrow `len` long centred on (x, y), pointing at bearing `deg`.
    fn arrow(&mut self, x: f32, y: f32, len: f32, deg: f32, c: Rgb565, w: f32) {
        let (dx, dy) = dir(deg);
        let (tx, ty) = (x + dx * len / 2.0, y + dy * len / 2.0);
        let (bx, by) = (x - dx * len / 2.0, y - dy * len / 2.0);
        self.c.line(bx, by, tx - dx * len * 0.3, ty - dy * len * 0.3, w, c);
        let hw = len * 0.28;
        let (px, py) = (-dy, dx);
        self.c.poly(
            &[
                (tx, ty),
                (tx - dx * len * 0.42 + px * hw, ty - dy * len * 0.42 + py * hw),
                (tx - dx * len * 0.42 - px * hw, ty - dy * len * 0.42 - py * hw),
            ],
            c,
        );
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
    /// the value.
    fn bars(&mut self, x: i32, bottom: i32, rssi: Option<i8>, on: Rgb565, off: Rgb565) {
        let level = match rssi {
            None => 0,
            Some(r) if r >= -60 => 4,
            Some(r) if r >= -70 => 3,
            Some(r) if r >= -80 => 2,
            Some(_) => 1,
        };
        for i in 0..4 {
            let h = 5 + i * 4;
            let bx = x + i * 7;
            if i < level {
                self.c.fill(bx, bottom - h, 5, h, on);
            } else {
                self.c.frame(bx, bottom - h, 5, h, 1, off);
            }
        }
    }
    /// "W N · L 5 ·": each radio in a fixed column, a dot where not heard.
    fn radio_letters(&mut self, r: Radios, x: i32, y: i32, on: Rgb565, off: Rgb565) {
        let f = self.f().data;
        let cw = f.width("W", 0) + 3;
        for (i, radio) in Radio::ALL.iter().enumerate() {
            let mut s: String<4> = String::new();
            let heard = r.contains(*radio);
            let _ = s.push(if heard { radio.letter() } else { '\u{b7}' });
            self.c.text(&s, x + i as i32 * cw, y, &f, if heard { on } else { off }, 0);
        }
    }
    /// A node's state dot: filled when alive, hollow and slashed when not.
    fn node_dot(&mut self, x: f32, y: f32, r: f32, alive: bool, ok: Rgb565, bad: Rgb565) {
        if alive {
            self.c.disc(x, y, r, ok);
        } else {
            self.c.ring(x, y, r - 1.0, 2.0, bad, None);
            self.c.line(x - r * 0.7, y + r * 0.7, x + r * 0.7, y - r * 0.7, 2.0, bad);
        }
    }

    // ------------------------------------------------------ status bar --

    pub fn status_bar(&mut self, s: &UiState) {
        let l = self.l;
        if !self.c.touches(0, BAR_H) {
            return;
        }
        self.c.fill(0, 0, W, BAR_H, l.bar);
        let (lab, data) = (self.f().label, self.f().data);
        let mid = |f: &Font| (BAR_H - i32::from(f.height)) / 2;
        let warn_box = |p: &mut Self, t: &str, x: i32| -> i32 {
            let f = p.f().small_b;
            let u: String<24> = upper(t);
            p.boxed(&u, x, (BAR_H - i32::from(f.height) - 6) / 2, &f, l.warn_ink, l.bar_warn, 6)
        };
        let mut x = 16;
        if let Some(b) = s.gps.integrity.badge() {
            x = warn_box(self, b, x);
        } else {
            match s.gps.fix {
                Fix::Fix2D | Fix::Fix3D => {
                    x = self.c.text("GPS", x, mid(&lab) + 1, &lab, l.bar_dim, 1) + 6;
                    let n: String<6> = fmt(format_args!("{}", s.gps.sats));
                    x = self.t(&n, x, mid(&data), &data, l.bar_fg);
                    if s.gps.fix == Fix::Fix2D {
                        x = self.c.text("2D", x + 6, mid(&lab) + 1, &lab, l.bar_dim, 1);
                    }
                }
                Fix::Manual => {
                    x = self.c.text("POS", x, mid(&lab) + 1, &lab, l.bar_dim, 1) + 6;
                    x = self.t("MAN", x, mid(&data), &data, l.bar_fg);
                }
                Fix::None => x = warn_box(self, "No fix", x),
                Fix::Absent => x = warn_box(self, "No GPS", x),
            }
        }
        x += 18;
        // A drone near a crewed aircraft outranks everything, then the
        // signs that the unit may be missing drones or placing them wrong.
        let near = s.conflict.as_ref().is_some_and(|c| c.level >= 2);
        let threat = if near {
            Some("Aircraft")
        } else if s.threats.jammed {
            Some("Jammed")
        } else if s.area_gnss.is_some_and(|g| g.spoofing) {
            Some("Area fake")
        } else if s.area_gnss.is_some_and(|g| g.jamming) {
            Some("Area jam")
        } else if s.threats.flood_per_s > 0 {
            Some("ID flood")
        } else if s.threats.active > 0 {
            Some("Attack")
        } else {
            None
        };
        if let Some(t) = threat {
            x = warn_box(self, t, x) + 18;
        }
        // Nodes: filled dot alive, hollow slashed dot dead, label always.
        let nf = self.f().small;
        for n in &s.nodes {
            self.node_dot(x as f32 + 5.0, BAR_H as f32 / 2.0, 5.0, n.alive, l.bar_ok, l.bar_warn);
            let c = if n.alive { l.bar_fg } else { l.bar_warn };
            x = self.t(n.kind.short(), x + 15, mid(&nf), &nf, c) + 14;
        }

        // Right side, inwards: clock, power, recording.
        let mut rx = W - 16;
        rx = self.c.text("UTC", rx - lab.width("UTC", 1), mid(&lab) + 1, &lab, l.bar_dim, 1) - lab.width("UTC", 1) - 6;
        let clock: String<8> = match s.utc_ms {
            Some(ms) => {
                let t = (ms / 1000) % 86_400;
                fmt(format_args!("{:02}:{:02}", t / 3600, t % 3600 / 60))
            }
            None => fmt(format_args!("--:--")),
        };
        rx = self.tr(&clock, rx, mid(&data), &data, l.bar_fg) - 22;
        match s.power {
            Power::Usb { .. } => rx = self.tr("USB", rx, mid(&nf), &nf, l.bar_fg) - 22,
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
                    // Low: boxed, not just coloured.
                    let w = self.f().small_b.width(&t, 0) + 12;
                    warn_box(self, &t, bx - 8 - w);
                    bx - 8 - w - 22
                } else {
                    self.tr(&t, bx - 8, mid(&nf), &nf, c) - 22
                };
            }
        }
        match s.sd {
            SdLog::Off => {}
            // The CSV log records but evidence is not reaching the card:
            // never REC for that.
            SdLog::Recording if s.caveats.evidence.is_some() => {
                let w = self.f().small_b.width("NO EVIDENCE", 0) + 12;
                warn_box(self, "No evidence", rx - w);
            }
            SdLog::Recording => {
                let lx = self.label_r("REC", rx, mid(&lab) + 1, l.bar_fg);
                self.c.disc(lx as f32 - 12.0, BAR_H as f32 / 2.0, 6.0, l.bar_rec);
            }
            SdLog::NoCard => {
                self.tr("no SD", rx, mid(&nf), &nf, l.bar_dim);
            }
            SdLog::Error => {
                let w = self.f().small_b.width("SD ERROR", 0) + 12;
                warn_box(self, "SD error", rx - w);
            }
        }
        let _ = x;
    }

    // ------------------------------------------------------------ keys --

    pub fn keys(&mut self, labels: &[KeyLabel; 3], lit: Option<Button>) {
        let l = self.l;
        if !self.c.touches(KEYS_TOP, super::H) {
            return;
        }
        self.c.fill(0, KEYS_TOP, W, super::H - KEYS_TOP, l.strip);
        let (kf, hf) = (self.f().small_b, self.f().label);
        for (i, k) in labels.iter().enumerate() {
            let (x, y, w, h) = key_rect(i);
            let b = [Button::Up, Button::Select, Button::Down][i];
            let mid = i == 1;
            let pressed = lit == Some(b);
            let r = l.radius.max(4);
            let text = match (l.keys, pressed, k.active) {
                (_, true, _) => {
                    self.c.rrect(x, y, w, h, r, l.fg);
                    l.bg
                }
                (KeyStyle::Outline, _, false) => {
                    self.c.rrect_stroke(x, y, w, h, r, 1.0, l.grid);
                    l.faint
                }
                (KeyStyle::Outline, _, true) if mid => {
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
                (KeyStyle::Filled, _, true) if mid => {
                    self.c.rrect(x, y, w, h, r, l.accent);
                    l.accent_ink
                }
                (KeyStyle::Filled, _, true) => {
                    self.c.rrect(x, y, w, h, r, l.key_fill);
                    l.key_text
                }
            };
            let word: String<24> = upper(&k.text);
            let tw = kf.width(&word, 1);
            let aw = if k.arrow.is_some() { 18 } else { 0 };
            let total = tw + aw;
            let ty = match k.hold {
                Some(_) => y + 3,
                None => y + (h - i32::from(kf.height)) / 2,
            };
            let mut tx = x + (w - total) / 2;
            let ay = (ty + i32::from(kf.ascent) - i32::from(kf.cap) / 2) as f32;
            match k.arrow {
                Some(a @ (Arrow::Left | Arrow::Up)) => {
                    self.tri(tx as f32 + 6.0, ay, 6.0, a, text);
                    tx += aw;
                    self.c.text(&word, tx, ty, &kf, text, 1);
                }
                Some(a) => {
                    let e = self.c.text(&word, tx, ty, &kf, text, 1);
                    self.tri(e as f32 + 12.0, ay, 6.0, a, text);
                }
                None => {
                    self.c.text(&word, tx, ty, &kf, text, 1);
                }
            }
            if let Some(hold) = k.hold {
                let hw = hf.width(hold, 1);
                self.c.text(hold, x + (w - hw) / 2, y + h - i32::from(hf.height) - 2, &hf, text, 1);
            }
        }
    }

    // --------------------------------------------------------- caveats --

    pub fn caveats(&mut self, lines: &[Option<String<64>>; 4]) {
        let n = lines.iter().flatten().count() as i32;
        let mut y = KEYS_TOP - n * CAVEAT_H;
        let f = self.f().small_b;
        for t in lines.iter().flatten() {
            self.c.fill(0, y, W, CAVEAT_H - 1, self.l.warn);
            // Whole characters that fit.
            let mut cut = t.len();
            while cut > 0 && f.width(&t[..cut], 0) > W - 32 {
                cut -= 1;
            }
            self.t(&t[..cut], 16, y + (CAVEAT_H - 1 - i32::from(f.height)) / 2, &f, self.l.warn_ink);
            y += CAVEAT_H;
        }
    }

    // ----------------------------------------------------------- radar --

    pub fn radar(&mut self, ui: &Ui, s: &UiState) {
        let l = self.l;
        let top = BAR_H;
        let r = ((self.bottom - top) / 2 - 24).min(168) as f32;
        let (cx, cy) = (228.0f32, (top + self.bottom) as f32 / 2.0);
        let order = s.order();

        for (m, _) in RINGS {
            self.c.ring(cx, cy, radius(m, r), 1.2, l.grid, l.dash);
        }
        self.c.fill(cx as i32, (cy - r) as i32, 1, 2 * r as i32, l.grid);
        self.c.fill((cx - r) as i32, cy as i32, 2 * r as i32, 1, l.grid);
        for a in (0..360).step_by(30) {
            let (dx, dy) = dir(a as f32);
            self.c.line(cx + dx * (r - 7.0), cy + dy * (r - 7.0), cx + dx * r, cy + dy * r, 1.2, l.grid);
        }
        let sf = self.f().small;
        for (m, label) in RINGS {
            let rr = radius(m, r);
            self.t(label, (cx + rr * 0.72 + 4.0) as i32, (cy + rr * 0.69 - 4.0) as i32, &sf, l.dim);
        }
        // N boxed: this plot is north-up, not "the way you face".
        let bf = self.f().body_b;
        let ny = (cy - r) as i32 - 30;
        let nw = bf.width("N", 0);
        self.boxed("N", cx as i32 - nw / 2 - 5, ny + 2, &bf, l.bg, l.fg, 5);
        self.tc("S", cx as i32, (cy + r) as i32 + 4, &bf, l.dim);
        self.t("E", (cx + r) as i32 + 6, cy as i32 - i32::from(bf.height) / 2, &bf, l.dim);
        self.tr("W", (cx - r) as i32 - 6, cy as i32 - i32::from(bf.height) / 2, &bf, l.dim);
        self.label("North up", M, top + 12, l.fg);
        self.t("not the way", M, top + 32, &sf, l.dim);
        self.t("you face", M, top + 50, &sf, l.dim);

        // You: a ring and a dot, never a triangle (no facing is known).
        self.c.ring(cx, cy, 6.0, 1.5, l.fg, None);
        self.c.disc(cx, cy, 2.0, l.fg);

        let foc = focus(ui, s, &order);
        let focus_key = foc.map(|f| f.1.key);
        let plot = |p: Polar| {
            let (dx, dy) = dir(p.bearing_deg);
            let rr = radius(p.distance_m, r);
            (cx + dx * rr, cy + dy * rr)
        };
        // The focused drone's pilot, joined to it by a dashed line.
        if let Some((_, f, _)) = foc
            && let (Some(dp), Some(op)) = (f.drone, f.operator)
        {
            let (a, b) = (plot(dp), plot(op));
            self.c.line_dashed(a.0, a.1, b.0, b.1, 1.2, l.operator, Some((4.0, 4.0)));
            self.cross(b.0, b.1, 6.0, l.operator);
            let lf = self.f().label;
            self.t("P", b.0 as i32 + 10, b.1 as i32 - 20, &lf, l.operator);
        }
        // A disputed, focused drone's other claimed position: hollow "?".
        if let Some((_, f, _)) = foc
            && let (Some(dp), Some(other)) = (f.drone, f.other_claim)
        {
            let (a, b) = (plot(dp), plot(other));
            self.c.line_dashed(a.0, a.1, b.0, b.1, 1.2, l.dim, Some((4.0, 4.0)));
            self.diamond(b.0, b.1, 8.0, l.fg, false);
            let lf = self.f().small_b;
            self.t("?", b.0 as i32 + 11, b.1 as i32 - 24, &lf, l.fg);
        }
        // Stale and unconfirmed first, then active, the focus on top.
        let mut unplaced = 0u32;
        let nf = self.f().small_b;
        for pass in 0..3 {
            for (rank, &i) in order.iter().enumerate() {
                let dv = &s.drones[usize::from(i)];
                let Some(pos) = dv.drone else {
                    if pass == 0 && !dv.stale {
                        unplaced += 1;
                    }
                    continue;
                };
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
                let (x, y) = plot(pos);
                if pos.distance_m > RINGS[RINGS.len() - 1].0 {
                    // Past the outer ring: an arrow on the edge, pointing out.
                    let (dx, dy) = dir(pos.bearing_deg);
                    self.arrow(cx + dx * (r - 4.0), cy + dy * (r - 4.0), 22.0, pos.bearing_deg, col, 3.0);
                } else {
                    let rr = if is_focus { 10.0 } else { 8.0 };
                    self.diamond(x, y, rr, col, !faint);
                }
                if is_focus {
                    self.c.ring(x, y, 16.0, 2.0, l.fg, None);
                }
                if rank < 9 && !dv.unconfirmed {
                    let n: String<4> = fmt(format_args!("{}", rank + 1));
                    let o = if is_focus { 16 } else { 11 };
                    self.t(&n, x as i32 + o, y as i32 - o - 14, &nf, if dv.stale { l.faint } else { l.fg });
                }
            }
        }
        if unplaced > 0 {
            let t: String<32> = fmt(format_args!("+{unplaced} without position"));
            self.t(&t, M, self.bottom - 24, &sf, l.dim);
        }
        self.nearest_card(s, foc);
        self.count_card(s, 452, self.bottom - 100, 332, 92);
    }

    fn nearest_card(&mut self, s: &UiState, foc: Option<(usize, &DroneView, bool)>) {
        let l = self.l;
        let (x, y, w) = (452, BAR_H + 12, 332);
        let h = self.bottom - 108 - y;
        self.card(x, y, w, h);
        let p = 18;
        let f = self.f();
        let Some((rank, dv, picked)) = foc else {
            let msg: [&str; 2] = if s.drones.is_empty() { ["Listening", "for drones"] } else { ["No drone", "position yet"] };
            self.label("Nearest", x + p, y + 14, l.dim);
            self.t(msg[0], x + p, y + 60, &f.title, l.dim);
            self.t(msg[1], x + p, y + 92, &f.title, l.dim);
            return;
        };
        if picked {
            // Choosing: the readout is this drone, not the nearest.
            let t: String<16> = fmt(format_args!("DRONE {}", rank + 1));
            self.boxed(&t, x + p - 4, y + 10, &f.small_b, l.bg, l.fg, 6);
        } else {
            self.label("Nearest", x + p, y + 14, l.dim);
            self.diamond((x + w - p - 8) as f32, (y + 21) as f32, 8.0, l.accent, true);
        }
        let col = ink(l, dv.stale);
        let pos = dv.drone.unwrap_or_default();
        let (n, unit) = distance(pos.distance_m);
        let room = w - 2 * p - 100;
        let big = if f.num_xl.width(&n, 0) <= room { f.num_xl } else { f.num_l };
        let top = y + 44;
        let e = self.num(&n, x + p - 2, top, &big, col);
        let uf = f.head;
        self.t(if unit == "m" { "m" } else { "km" }, e + 6, top + i32::from(big.cap) - i32::from(uf.ascent), &uf, l.dim);
        // Bearing: an arrow in a north-up rose, then the words.
        let (ax, ay) = ((x + w - p - 40) as f32, (y + 70) as f32);
        self.c.ring(ax, ay, 30.0, 2.0, l.grid, None);
        let lf = f.label;
        self.tc("N", ax as i32, ay as i32 - 30 - i32::from(lf.height) / 2 - 1, &lf, l.dim);
        self.arrow(ax, ay, 40.0, pos.bearing_deg, col, 4.0);
        self.tc(compass(pos.bearing_deg), ax as i32, ay as i32 + 34, &f.data_l, col);
        self.tc(&degrees(pos.bearing_deg), ax as i32, ay as i32 + 60, &f.small, l.dim);

        let mut ry = y + 164;
        let max = y + h - 24;
        self.hline(x + p, x + w - p, ry - 8, l.edge);
        let vx = x + p + 78;
        let rows: [(&str, String<40>, Rgb565); 4] = [
            ("Height", fmt(format_args!("{} up", metres(dv.height_m))), col),
            (
                "Pilot",
                match dv.operator {
                    Some(o) => fmt(format_args!("{} {}", distance_short(o.distance_m), compass(o.bearing_deg))),
                    None => fmt(format_args!("--")),
                },
                if dv.operator.is_some() { l.operator } else { l.dim },
            ),
            ("ID", fmt(format_args!("{}", short_id(dv, 18))), col),
            (
                "Heard",
                fmt(format_args!("{} \u{b7} {}", age(s.now_ms.saturating_sub(dv.last_seen_ms)).as_str(), radio_words(dv.radios, 12).as_str())),
                col,
            ),
        ];
        for (k, v, c) in rows {
            if ry > max {
                break;
            }
            self.label(k, x + p, ry + 4, l.dim);
            if k == "ID" && dv.disputed {
                // Two transmitters claim this ID: say so where the ID is.
                self.caution("Disputed", vx, ry);
            } else {
                let e = self.t_fit(&v, vx, ry, i32::from(f.body.height), x + w - p - vx - 24, &[f.body, f.small], c);
                if k == "Pilot" && dv.operator.is_some() {
                    self.cross((e + 14) as f32, (ry + i32::from(f.body.height) / 2) as f32, 5.0, l.operator);
                }
            }
            ry += 25;
        }
    }

    /// "5 DRONES / +2 stale · 1 without position", the number big.
    fn count_card(&mut self, s: &UiState, x: i32, y: i32, w: i32, h: i32) {
        let l = self.l;
        self.card(x, y, w, h);
        let f = self.f();
        let active = s.confirmed_active_count();
        let unconf = usize::from(s.total_unconfirmed);
        let total = usize::from(s.total_drones).max(s.drones.len()).saturating_sub(unconf);
        let n: String<8> = fmt(format_args!("{active}"));
        let e = self.num(&n, x + 16, y + (h - i32::from(f.num_l.cap)) / 2, &f.num_l, if active > 0 { l.accent } else { l.dim });
        self.label(if active == 1 { "Drone" } else { "Drones" }, e + 14, y + h / 2 - 22, l.fg);
        let stale = total.saturating_sub(active);
        let unplaced = s.drones.iter().filter(|d| !d.stale && d.drone.is_none()).count();
        let mut t: String<48> = String::new();
        use core::fmt::Write;
        if unconf > 0 {
            let _ = write!(t, "+{unconf} unconfirmed");
        } else if stale > 0 {
            let _ = write!(t, "+{stale} stale");
        } else {
            let _ = t.push_str(if active == 0 { "none heard" } else { "active" });
        }
        if unplaced > 0 && s.gps.has_position() {
            let _ = write!(t, " \u{b7} {unplaced} without position");
        }
        let sf = if f.small.width(&t, 0) <= w - (e - x) - 30 { f.small } else { f.label };
        self.t(&t, e + 14, y + h / 2 + 2, &sf, l.dim);
    }

    // ------------------------------------------------ no fix: list first --

    fn no_fix_banner(&mut self, y: i32) -> i32 {
        let l = self.l;
        let f = self.f();
        self.c.fill(0, y, W, 34, l.warn);
        let e = self.t("!", M, y + (34 - i32::from(f.body_b.height)) / 2, &f.body_b, l.warn_ink);
        let e = self.t("NO POSITION", e + 12, y + (34 - i32::from(f.body_b.height)) / 2, &f.body_b, l.warn_ink);
        self.t("\u{b7} distances and directions unavailable", e + 10, y + (34 - i32::from(f.body.height)) / 2, &f.body, l.warn_ink);
        y + 34
    }

    pub fn no_fix_home(&mut self, ui: &Ui, s: &UiState) {
        let y = self.no_fix_banner(BAR_H + 4) + 8;
        self.count_card(s, 16, y, 420, 80);
        let f = self.f().small;
        self.tr("newest first", W - M, y + 8, &f, self.l.dim);
        self.tr("signal strength is not distance", W - M, y + 30, &f, self.l.dim);
        self.tr("(Wi-Fi and Bluetooth send at different powers)", W - M, y + 52, &f, self.l.dim);
        self.rows(ui, s, y + 92);
    }

    // ------------------------------------------------------------ list --

    pub fn list_page(&mut self, ui: &Ui, s: &UiState) {
        let l = self.l;
        let mut y = BAR_H + 6;
        if !s.gps.has_position() {
            y = self.no_fix_banner(y) + 4;
        }
        let f = self.f();
        let e = self.t("DRONES", M, y + 6, &f.title, l.fg);
        let n: String<8> = fmt(format_args!("{}", s.confirmed_active_count()));
        let e = self.t(&n, e + 12, y + 6, &f.title, l.accent);
        if s.total_unconfirmed > 0 {
            let t: String<24> = fmt(format_args!("+{} unconfirmed", s.total_unconfirmed));
            self.t(&t, e + 14, y + 12, &f.small, l.dim);
        }
        let sort = if s.gps.has_position() { "nearest first" } else { "newest first" };
        self.tr(sort, W - M, y + 12, &f.small, l.dim);
        self.rows(ui, s, y + 44);
    }

    /// Column header, rows, stale divider, from `top` down.
    fn rows(&mut self, ui: &Ui, s: &UiState, top: i32) {
        let l = self.l;
        let f = self.f();
        let fix = s.gps.has_position();
        let order = s.order();
        // Columns.
        const C_N: i32 = 40; // right
        const C_ID: i32 = 76;
        const C_DIST: i32 = 330; // right
        const C_DIR: i32 = 356;
        const C_SIG: i32 = 300;
        const C_HGT: i32 = 556; // right
        const C_AGE: i32 = 640; // right
        const C_RADIO: i32 = 664;
        self.label_r("#", C_N, top, l.dim);
        self.label("ID", C_ID, top, l.dim);
        if fix {
            self.label_r("Distance", C_DIST, top, l.dim);
            self.label("Direction", C_DIR, top, l.dim);
        } else {
            self.label("Signal", C_SIG, top, l.dim);
        }
        self.label_r("Height", C_HGT, top, l.dim);
        self.label_r("Heard", C_AGE, top, l.dim);
        self.label("Radio", C_RADIO, top, l.dim);
        self.hline(16, W - 16, top + 18, l.edge);

        if order.is_empty() {
            self.tc("No drones heard yet", W / 2, top + 60, &f.title, l.dim);
            let why = if s.threats.jammed { "Jamming: the hub may be missing drones" } else { "Listening on every radio" };
            self.tc(why, W / 2, top + 100, &f.body, if s.threats.jammed { l.warn } else { l.dim });
            return;
        }
        const RH: i32 = 40;
        let first = top + 24;
        let stale_at = order.iter().position(|&i| s.drones[usize::from(i)].stale);
        let divider = if stale_at.is_some_and(|i| i > 0) { 24 } else { 0 };
        let fit = ((self.bottom - 22 - first - divider) / RH).max(1) as usize;
        let cur = ui.cursor().and_then(|k| order.iter().position(|&i| s.drones[usize::from(i)].key == k));
        let start = match cur {
            Some(c) if c >= fit => c + 1 - fit,
            _ => 0,
        };
        let mut y = first;
        for (rank, &i) in order.iter().enumerate().skip(start).take(fit) {
            let dv = &s.drones[usize::from(i)];
            if Some(rank) == stale_at && rank > 0 {
                self.hline(16, 260, y + 12, l.faint);
                self.hline(W - 260, W - 16, y + 12, l.faint);
                self.tc("not heard for 60 s", W / 2, y + 2, &f.small, l.faint);
                y += 24;
            }
            let selected = Some(rank) == cur;
            if selected {
                self.c.rrect(8, y, W - 16, RH - 2, l.radius.min(8), l.sel_fill);
                self.c.rrect_stroke(8, y, W - 16, RH - 2, l.radius.min(8), 2.0, l.sel_edge);
                self.tri(18.0, (y + RH / 2 - 1) as f32, 6.0, Arrow::Right, l.sel_edge);
            }
            let col = if dv.stale {
                l.faint
            } else if dv.unconfirmed {
                l.dim
            } else {
                l.fg
            };
            let ty = y + (RH - 2 - i32::from(f.data.height)) / 2;
            let n: String<6> = if dv.unconfirmed { fmt(format_args!("?")) } else { fmt(format_args!("{}", rank + 1)) };
            self.tr(&n, C_N, ty, &f.data, if dv.stale { l.faint } else { l.dim });
            let my = (y + RH / 2 - 1) as f32;
            let faint = dv.stale || dv.unconfirmed;
            self.diamond(C_ID as f32 - 14.0, my, 6.0, if faint { l.faint } else { l.accent }, !faint);
            if dv.disputed {
                // Two transmitters claim this ID: "!" before it.
                let e = self.t("!", C_ID, ty, &f.data, l.warn);
                self.t(&short_id(dv, 13), e + 2, ty, &f.data, col);
            } else {
                self.t(&short_id(dv, 14), C_ID, ty, &f.data, col);
            }
            if fix {
                match dv.drone {
                    Some(p) => {
                        self.tr(&distance_short(p.distance_m), C_DIST, ty, &f.data, col);
                        self.arrow(C_DIR as f32 + 10.0, my, 18.0, p.bearing_deg, col, 2.5);
                        let t: String<16> = fmt(format_args!("{} {}", compass(p.bearing_deg), degrees(p.bearing_deg).as_str()));
                        self.t(&t, C_DIR + 28, ty, &f.data, col);
                    }
                    None => {
                        self.tr("--", C_DIST, ty, &f.data, col);
                        self.t("no position yet", C_DIR, ty + 2, &f.small, l.dim);
                    }
                }
            } else {
                self.bars(C_SIG, y + RH - 12, dv.best_rssi(), col, l.grid);
                if let Some(r) = dv.best_rssi() {
                    let t: String<8> = fmt(format_args!("{r} dBm"));
                    self.t(&t, C_SIG + 36, ty, &f.data, col);
                }
            }
            self.tr(&metres(dv.height_m), C_HGT, ty, &f.data, col);
            let a = age(s.now_ms.saturating_sub(dv.last_seen_ms));
            let ac = if a.as_str() == "now" && !faint { l.ok } else { col };
            self.tr(&a, C_AGE, ty, &f.data, ac);
            self.radio_letters(dv.radios, C_RADIO, ty, col, l.grid);
            y += RH;
        }
        let more = order.len().saturating_sub(start + fit);
        if more > 0 {
            let t: String<16> = fmt(format_args!("+{more} more"));
            self.tr(&t, W - M, self.bottom - 22, &f.small, l.dim);
        }
    }

    // ---------------------------------------------------------- detail --

    pub fn detail(&mut self, ui: &Ui, s: &UiState) {
        let l = self.l;
        let f = self.f();
        let order = s.order();
        let found = ui.cursor().and_then(|k| {
            order
                .iter()
                .position(|&i| s.drones[usize::from(i)].key == k)
                .map(|r| (r, &s.drones[usize::from(order[r])]))
        });
        let Some((rank, dv)) = found else {
            self.tc("Drone no longer tracked", W / 2, 180, &f.title, l.fg);
            self.tc("hold the middle key to go back", W / 2, 220, &f.body, l.dim);
            return;
        };
        let col = ink(l, dv.stale);
        let y0 = BAR_H + 10;
        let head: String<64> = fmt(format_args!("Drone {} of {} \u{b7} {}", rank + 1, order.len(), dv.id_kind.label()));
        self.label(&head, M, y0, l.dim);
        if dv.stale {
            let w = f.small_b.width("STALE", 0) + 12;
            self.boxed("STALE", W - M - w, y0 - 4, &f.small_b, l.bg, l.faint, 6);
        } else if !dv.model.is_empty() {
            self.tr(&dv.model, W - M, y0 - 2, &f.body, l.dim);
        } else {
            self.tr(dv.ua.label(), W - M, y0 - 2, &f.body, l.dim);
        }
        let id = if dv.id.is_empty() { "(none)" } else { dv.id.as_str() };
        let idf = if f.id.width(id, 0) <= W - 2 * M {
            f.id
        } else if f.data_l.width(id, 0) <= W - 2 * M {
            f.data_l
        } else {
            f.data
        };
        self.t(id, M, y0 + 20, &idf, col);
        let rule = y0 + 62;
        self.hline(16, W - 16, rule, l.edge);

        // Right: where it is, big.
        let (cx0, cw) = (W - 16 - 236, 236);
        let ch = self.bottom - 8 - (rule + 10);
        self.card(cx0, rule + 10, cw, ch);
        self.direction_card(s, dv, cx0, rule + 10, cw, ch);

        // Left: label / value rows.
        let rows = 9;
        let pitch = ((self.bottom - rule - 14) / rows).min(34);
        let (lx, vx) = (M, 130);
        let maxw = cx0 - 12 - vx;
        let mut y = rule + 12;
        let fix = s.gps.has_position();
        let vy = |y: i32| y + (pitch - i32::from(f.body.height)) / 2;
        let ly = |y: i32| y + (pitch - i32::from(f.label.height)) / 2;
        for (name, pos, pilot) in [("Drone", dv.drone, false), ("Pilot", dv.operator, true)] {
            self.label(name, lx, ly(y), l.dim);
            match pos {
                Some(p) => {
                    let e = self.t(&distance_short(p.distance_m), vx, vy(y), &f.body, col);
                    self.arrow(e as f32 + 16.0, (y + pitch / 2) as f32, 16.0, p.bearing_deg, col, 2.5);
                    let t: String<16> = fmt(format_args!("{} {}", compass(p.bearing_deg), degrees(p.bearing_deg).as_str()));
                    let e = self.t(&t, e + 32, vy(y), &f.body, col);
                    if pilot {
                        self.cross(e as f32 + 16.0, (y + pitch / 2) as f32, 5.0, l.operator);
                    }
                }
                None if pilot && dv.drone_to_operator_m.is_some() => {
                    // No own fix, but both drone and pilot positions are known.
                    let t: String<32> = fmt(format_args!("{} from the drone", distance_short(dv.drone_to_operator_m.unwrap_or(0.0)).as_str()));
                    let e = self.t(&t, vx, vy(y), &f.body, col);
                    self.cross(e as f32 + 16.0, (y + pitch / 2) as f32, 5.0, l.operator);
                }
                None => {
                    let why = if !fix {
                        "unknown: no own position"
                    } else if pilot {
                        "pilot position not sent"
                    } else {
                        "drone sent no position"
                    };
                    self.t(why, vx, vy(y), &f.small, l.dim);
                }
            }
            y += pitch;
        }
        self.label("Height", lx, ly(y), l.dim);
        let mut b: String<64> = fmt(format_args!(
            "{} above {}",
            metres(dv.height_m).as_str(),
            match dv.height_ref {
                HeightRef::Takeoff => "takeoff",
                HeightRef::Ground => "ground",
            }
        ));
        if let Some(a) = dv.above_you_m {
            use core::fmt::Write;
            let _ = write!(b, ", {:+} m vs you", libm::roundf(a) as i32);
        }
        self.t(&b, vx, vy(y), &f.body, col);
        y += pitch;

        self.label("Speed", lx, ly(y), l.dim);
        let sp: String<16> = match dv.speed_mps {
            Some(v) => fmt(format_args!("{v:.1} m/s")),
            None => fmt(format_args!("--")),
        };
        let mut e = self.t(&sp, vx, vy(y), &f.body, col);
        if let Some(t) = dv.track_deg {
            self.arrow(e as f32 + 16.0, (y + pitch / 2) as f32, 16.0, t, col, 2.5);
            e = self.t(compass(t), e + 32, vy(y), &f.body, col);
        }
        if let Some(v) = dv.vspeed_mps {
            let r = libm::roundf(v * 10.0) / 10.0;
            let t: String<16> = if r > 0.0 {
                fmt(format_args!("climb {r:.1}"))
            } else if r < 0.0 {
                fmt(format_args!("sink {:.1}", -r))
            } else {
                fmt(format_args!("level"))
            };
            self.t(&t, e + 16, vy(y), &f.body, col);
        }
        y += pitch;

        for (k, v) in [("Op ID", dv.operator_id.as_str()), ("Self-ID", dv.self_id.as_str())] {
            self.label(k, lx, ly(y), l.dim);
            self.t(if v.is_empty() { "--" } else { v }, vx, vy(y), &f.body, col);
            y += pitch;
        }

        // Signal: each radio that heard it, as letter, bars and dBm.
        self.label("Signal", lx, ly(y), l.dim);
        let mut x = vx;
        let mut any = false;
        for r in Radio::ALL {
            let Some(rssi) = dv.rssi[r.index()] else { continue };
            any = true;
            let mut t: String<2> = String::new();
            let _ = t.push(r.letter());
            x = self.t(&t, x, vy(y), &f.body_b, col) + 6;
            self.bars(x, y + pitch / 2 + 9, Some(rssi), col, l.grid);
            let d: String<8> = fmt(format_args!("{rssi}"));
            x = self.t(&d, x + 32, vy(y) + 2, &f.small, col) + 18;
            if x > vx + maxw - 60 {
                break;
            }
        }
        if !any {
            self.t("--", vx, vy(y), &f.body, col);
        }
        y += pitch;

        self.label("Heard", lx, ly(y), l.dim);
        let last = s.now_ms.saturating_sub(dv.last_seen_ms);
        let first = age(s.now_ms.saturating_sub(dv.first_seen_ms));
        let t: String<48> = if last < 2000 {
            fmt(format_args!("now, first {} ago", first.as_str()))
        } else {
            fmt(format_args!("{} ago, first {} ago", age(last).as_str(), first.as_str()))
        };
        self.t(&t, vx, vy(y), &f.body, col);
        y += pitch;

        // What the data is: a claim. Quiet unless two transmitters disagree.
        self.label("Claim", lx, ly(y), l.dim);
        if dv.disputed {
            let e = self.caution("Disputed", vx, y + (pitch - i32::from(f.small_b.height) - 6) / 2);
            let t: String<40> = match (dv.other_claim, dv.claims_apart_m) {
                (Some(o), _) => fmt(format_args!("other at {} {}", distance_short(o.distance_m).as_str(), compass(o.bearing_deg))),
                (None, Some(m)) => fmt(format_args!("other {} away", distance_short(m).as_str())),
                (None, None) => fmt(format_args!("2 transmitters, 1 ID")),
            };
            self.t(&t, e + 10, vy(y), &f.body, col);
        } else if dv.auth_seen {
            self.t("broadcast, not verified (auth message not checked)", vx, vy(y), &f.small, l.dim);
        } else {
            self.t("broadcast, not verified", vx, vy(y), &f.small, l.dim);
        }
    }

    /// The detail page's right-hand card: distance and direction, large.
    fn direction_card(&mut self, s: &UiState, dv: &DroneView, x: i32, y: i32, w: i32, h: i32) {
        let l = self.l;
        let f = self.f();
        let col = ink(l, dv.stale);
        let p = 16;
        self.label("Drone", x + p, y + 12, l.dim);
        self.diamond((x + w - p - 8) as f32, (y + 19) as f32, 7.0, if dv.stale { l.faint } else { l.accent }, !dv.stale);
        let Some(pos) = dv.drone else {
            let why = if s.gps.has_position() { "No position sent" } else { "No own position" };
            self.t(why, x + p, y + 44, &f.body_b, l.dim);
            self.t("distance unknown", x + p, y + 70, &f.small, l.dim);
            return;
        };
        let (n, unit) = distance(pos.distance_m);
        let big = if f.num_l.width(&n, 0) <= w - 2 * p - 50 { f.num_l } else { f.num_m };
        let e = self.num(&n, x + p, y + 40, &big, col);
        self.t(unit, e + 6, y + 40 + i32::from(big.cap) - i32::from(f.title.ascent), &f.title, l.dim);
        let (ax, ay) = ((x + 56) as f32, (y + 172) as f32);
        if y + 172 + 40 < y + h {
            self.c.ring(ax, ay, 36.0, 2.0, l.grid, None);
            self.tc("N", ax as i32, ay as i32 - 36 - i32::from(f.label.height) / 2 - 1, &f.label, l.dim);
            self.arrow(ax, ay, 48.0, pos.bearing_deg, col, 4.0);
            self.t(compass(pos.bearing_deg), x + 112, y + 146, &f.data_l, col);
            self.t(&degrees(pos.bearing_deg), x + 112, y + 178, &f.body, l.dim);
        }
        if let Some(o) = dv.operator
            && y + 230 + 30 < y + h
        {
            self.label("Pilot", x + p, y + 232, l.dim);
            let t: String<24> = fmt(format_args!("{} {}", distance_short(o.distance_m).as_str(), compass(o.bearing_deg)));
            let e = self.t(&t, x + p, y + 252, &f.body_b, l.operator);
            self.cross(e as f32 + 16.0, (y + 252 + i32::from(f.body_b.height) / 2) as f32, 6.0, l.operator);
        }
    }

    // --------------------------------------------------------- signals --

    /// Column headers: (x, text, right-aligned).
    fn header(&mut self, y: i32, cols: &[(i32, &str, bool)]) {
        for &(x, h, right) in cols {
            if right {
                self.label_r(h, x, y, self.l.dim);
            } else {
                self.label(h, x, y, self.l.dim);
            }
        }
        self.hline(16, W - 16, y + 18, self.l.edge);
    }

    pub fn signals(&mut self, s: &UiState) {
        let l = self.l;
        let f = self.f();
        const RH: i32 = 26;
        let mut y = BAR_H + 10;
        self.t("SIGNALS", M, y, &f.title, l.fg);
        let t: String<64> = fmt(format_args!(
            "{} control links \u{b7} {} RF emitters \u{b7} {} Wi-Fi drones",
            s.total_control.max(s.control.len() as u16),
            s.total_emitters.max(s.emitters.len() as u16),
            s.total_wifi_drones.max(s.wifi_drones.len() as u16)
        ));
        self.tr(&t, W - M, y + 6, &f.small, l.dim);
        y += 42;
        self.header(y, &[(M, "Control link", false), (210, "Band", true), (228, "Sure", false), (400, "dBm", true), (420, "Detail", false), (W - M, "Age", true)]);
        y += 24;
        if s.control.is_empty() {
            self.t("none heard", 40, y, &f.body, l.dim);
            y += RH;
        }
        for c in s.control.iter().take(3) {
            let col = ink(l, c.stale);
            self.t(&c.system, M, y, &f.body_b, col);
            self.tr(c.band, 210, y, &f.data, col);
            self.t(c.sure.label(), 228, y, &f.body, col);
            let r: String<8> = fmt(format_args!("{}", c.rssi));
            self.tr(&r, 400, y, &f.data, col);
            self.t(&c.detail, 420, y, &f.data, col);
            self.tr(&age(s.now_ms.saturating_sub(c.last_seen_ms)), W - M, y, &f.data, col);
            y += RH;
        }
        y += 8;
        self.header(y, &[(M, "RF emitter", false), (375, "MHz", true), (490, "Level", true), (630, "Duty / line", true), (W - M, "Age", true)]);
        y += 24;
        // Leave room for the Wi-Fi table: header and two rows.
        let rf_rows = ((self.bottom - y - 8 - 24 - 2 * RH - 20) / RH).max(1) as usize;
        if s.emitters.is_empty() {
            self.t("none heard", 40, y, &f.body, l.dim);
            y += RH;
        }
        for e in s.emitters.iter().take(rf_rows) {
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
            y += RH;
        }
        let more = usize::from(s.total_emitters).max(s.emitters.len()).saturating_sub(rf_rows.min(s.emitters.len()));
        if more > 0 {
            let t: String<16> = fmt(format_args!("+{more} more"));
            self.tr(&t, W - M, y, &f.small, l.dim);
            y += 20;
        }
        y += 8;
        self.header(y, &[(M, "Wi-Fi drone", false), (275, "Network", false), (630, "Ch", true), (705, "dBm", true), (W - M, "Age", true)]);
        y += 24;
        if s.wifi_drones.is_empty() {
            self.t("none heard", 40, y, &f.body, l.dim);
        }
        let mut shown = 0;
        for w in &s.wifi_drones {
            if y + RH > self.bottom {
                break;
            }
            let col = ink(l, w.stale);
            self.t(&w.maker, M, y, &f.body_b, col);
            self.t(&w.ssid, 275, y, &f.data, col);
            let t: String<8> = fmt(format_args!("{}", w.channel));
            self.tr(&t, 630, y, &f.data, col);
            let t: String<8> = fmt(format_args!("{}", w.rssi));
            self.tr(&t, 705, y, &f.data, col);
            self.tr(&age(s.now_ms.saturating_sub(w.last_seen_ms)), W - M, y, &f.data, col);
            y += RH;
            shown += 1;
        }
        let more = usize::from(s.total_wifi_drones).max(s.wifi_drones.len()).saturating_sub(shown);
        if more > 0 {
            let t: String<16> = fmt(format_args!("+{more} more"));
            self.tr(&t, W - M, self.bottom - 20, &f.small, l.dim);
        }
    }

    // ----------------------------------------------------------- flags --

    pub fn flags(&mut self, s: &UiState) {
        let l = self.l;
        let f = self.f();
        let mut y = BAR_H + 10;
        self.t("FLAGS", M, y, &f.title, l.fg);
        let Some(fl) = &s.flags else {
            self.t("no rule engine on this hub", M, y + 50, &f.body, l.dim);
            return;
        };
        self.tr(&fl.pack, W - M, y + 6, &f.small, l.dim);
        y += 36;
        // The wording rule (PLAN.md): possible, never "illegal".
        self.t("Possible violations. The hub cannot see waivers or LAANC authorisations.", M, y, &f.small, l.dim);
        y += 28;
        self.header(y, &[(M, "Level", false), (120, "Who", false), (330, "Rule", false), (440, "Evidence", false), (W - M, "For", true)]);
        y += 26;
        if fl.rows.is_empty() {
            self.t("none", 40, y, &f.body, l.dim);
        }
        const RH: i32 = 32;
        let mut shown = 0;
        for r in &fl.rows {
            if y + RH > self.bottom - 20 {
                break;
            }
            let word = match r.level {
                0 => "NOTE",
                1 => "WATCH",
                2 => "WARN",
                _ => "ALERT",
            };
            let col = if r.level >= 2 { l.fg } else { l.dim };
            if r.level >= 3 {
                // The worst level is boxed: never colour alone.
                self.caution(word, M, y - 3);
            } else {
                self.t(word, M, y, &f.body_b, col);
            }
            self.t(&r.who, 120, y, &f.data, col);
            self.t(r.rule, 330, y, &f.body_b, col);
            self.t(&r.text, 440, y, &f.body, col);
            let t: String<8> = fmt(format_args!("{}s", r.secs));
            self.tr(&t, W - M, y, &f.data, col);
            y += RH;
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
        let y0 = BAR_H + 10;
        self.t("HEALTH", M, y0, &f.title, l.fg);
        let up: String<24> = fmt(format_args!("hub up {}", uptime(s.hub_uptime_s).as_str()));
        self.tr(&up, W - M, y0 + 6, &f.small, l.dim);

        let roles = s.assigned.iter().any(|a| a.is_some());
        let rh = if s.nodes.len() > 4 { 30 } else { 34 };
        let card_y = y0 + 40;
        let card_h = 34 + rh * (s.nodes.len().max(1) as i32) + 4;
        self.card(16, card_y, W - 32, card_h);
        let cols: [(i32, &str, bool); 5] = [(40, "Node", false), (480, "Frames/s", true), (580, "Remote ID", true), (670, "Drops", true), (W - 36, "Up", true)];
        let hy = card_y + 10;
        for (x, h, right) in cols {
            if right {
                self.label_r(h, x, hy, l.dim);
            } else {
                self.label(h, x, hy, l.dim);
            }
        }
        if roles {
            self.label("Assigned role", 200, hy, l.dim);
        }
        let mut y = card_y + 34;
        for (i, n) in s.nodes.iter().enumerate() {
            self.hline(32, W - 32, y, l.edge);
            let ty = y + (rh - i32::from(f.body_b.height)) / 2;
            self.node_dot(46.0, (y + rh / 2) as f32, 7.0, n.alive, l.ok, l.warn);
            self.t(n.kind.long(), 62, ty, &f.body_b, if n.alive { l.fg } else { l.warn });
            if let Some(Some(a)) = s.assigned.get(i) {
                let t: String<48> = fmt(format_args!("{} plan {:04x} {}", a.role, a.plan, if a.applied { "running" } else { "sent" }));
                self.t(&t, 200, y + (rh - i32::from(f.small.height)) / 2, &f.small, if a.applied { l.dim } else { l.warn });
            }
            if n.alive {
                for (x, v) in [(480, u32::from(n.frames_per_s)), (580, n.rid_found), (670, n.drops)] {
                    let t: String<12> = fmt(format_args!("{v}"));
                    self.tr(&t, x, y + (rh - i32::from(f.data.height)) / 2, &f.data, l.fg);
                }
                self.tr(&uptime(n.uptime_s), W - 36, y + (rh - i32::from(f.data.height)) / 2, &f.data, l.dim);
            } else {
                let w = f.small_b.width("NOT RESPONDING", 0) + 12;
                self.caution("Not responding", W - 36 - w, y + (rh - i32::from(f.small_b.height) - 6) / 2);
            }
            y += rh;
        }
        if s.nodes.is_empty() {
            self.t("no radio nodes reported", 62, y + 6, &f.body_b, l.warn);
        }

        // Tiles: label and value, two columns.
        let mut items: heapless::Vec<(&str, String<64>, bool), 8> = heapless::Vec::new();
        let gps: String<64> = if s.gps.untrusted_pos || s.gps.untrusted_time {
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
        let _ = items.push(("Tracks", fmt(format_args!("{} ({} active)", total, s.active_count())), false));
        let (sd, sd_bad) = match s.sd {
            SdLog::Off => ("off", false),
            SdLog::Recording if s.caveats.evidence.is_some() => ("CSV only \u{b7} NO EVIDENCE", true),
            SdLog::Recording => ("recording", false),
            SdLog::NoCard => ("no card", false),
            SdLog::Error => ("ERROR \u{b7} not recording", true),
        };
        let _ = items.push(("SD log", fmt(format_args!("{sd}")), sd_bad));
        let power: String<64> = match s.power {
            Power::Usb { battery_pct: Some(p) } => fmt(format_args!("USB \u{b7} battery {p}%")),
            Power::Usb { battery_pct: None } => fmt(format_args!("USB")),
            Power::Battery { pct } => fmt(format_args!("battery {pct}%")),
        };
        let _ = items.push(("Power", power, matches!(s.power, Power::Battery { pct } if pct <= 15)));
        if let Some(a) = &s.aircraft {
            // A quiet line, never an alert: crewed aircraft are not what
            // the unit hunts. "LOW" is the emphasis, in words.
            let mut t: String<64> = String::new();
            if a.total == 0 {
                let _ = t.push_str("none heard");
            } else {
                let _ = write!(t, "{} active", a.active);
            }
            if a.low_nearby > 0 {
                let _ = write!(t, " \u{b7} {} LOW", a.low_nearby);
            }
            if let Some(n) = &a.nearest {
                let _ = write!(t, " \u{b7} {} {}", n.name, distance_short(n.distance_m).as_str());
                if let Some(ft) = n.alt_ft {
                    let _ = write!(t, " {ft} ft");
                }
            }
            let _ = items.push(("Aircraft", t, a.low_nearby > 0));
        }
        if let Some(c) = &s.conflict {
            let mut t: String<64> = fmt(format_args!("drone {} from {}", distance_short(c.range_m).as_str(), c.aircraft));
            if let Some(z) = c.dz_m {
                let _ = write!(t, " {:+} ft", (z * 3.28084) as i32);
            }
            let _ = items.push(("Near aircraft", t, c.level >= 2));
        }
        if let Some(g) = &s.area_gnss {
            let mut t: String<64> = String::new();
            if g.spoofing {
                let _ = t.push_str("FAKED:");
                if g.drones_moved > 0 {
                    let _ = write!(t, " {} drones", g.drones_moved);
                }
                if g.aircraft_moved > 0 {
                    let _ = write!(t, " {} aircraft", g.aircraft_moved);
                }
                let _ = t.push_str(" moved");
            } else {
                let _ = write!(t, "JAMMED: {}/{} aircraft", g.degraded, g.counted);
            }
            if g.own_agrees {
                let _ = t.push_str(" + hub");
            }
            let _ = items.push(("Area GPS", t, true));
        }
        let (th, q) = (s.threats, s.caveats.silenced);
        if th.active > 0 || th.muted > 0 {
            let mut t: String<64> = fmt(format_args!("{} sounding \u{b7} {} silenced", th.active, th.muted));
            if q.snoozed > 0 {
                let _ = t.push_str(" (snooze)");
            }
            let _ = items.push(("Alarms", t, th.active > 0));
        }
        let ty0 = card_y + card_h + 10;
        let rows = items.len().div_ceil(2) as i32;
        let th_ = ((self.bottom - 6 - ty0) / rows.max(1)).min(62);
        let tw = (W - 32 - 10) / 2;
        let compact = th_ < 52;
        for (i, (k, v, bad)) in items.iter().enumerate() {
            let (x, y) = (16 + (i as i32 % 2) * (tw + 10), ty0 + (i as i32 / 2) * th_);
            if th_ < 26 {
                break; // no room: the status bar still says the worst of it
            }
            let h = th_ - 6;
            self.card(x, y, tw, h);
            let c = if *bad { l.warn } else { l.fg };
            if *bad {
                // Not colour alone: a heavy bar down the tile's left edge
                // (and the words say what is wrong).
                self.c.rrect(x, y, 6, h, 2, l.warn);
            }
            let fonts = [f.body, f.small, f.label];
            if compact {
                self.label(k, x + 16, y + (h - i32::from(f.label.height)) / 2, l.dim);
                self.t_fit(v, x + 140, y, h, tw - 140 - 10, &fonts, c);
            } else {
                self.label(k, x + 16, y + 8, l.dim);
                self.t_fit(v, x + 16, y + 24, h - 28, tw - 28, &fonts, c);
            }
        }
    }

    // -------------------------------------------------------- settings --

    pub fn settings(&mut self, ui: &Ui, s: &UiState) {
        let l = self.l;
        let f = self.f();
        let y0 = BAR_H + 10;
        self.t("SETTINGS", M, y0, &f.title, l.fg);
        let st = ui.settings();
        let editing = ui.mode() == Mode::Edit;
        let rows = st.rows();
        let auto = st.theme == ThemeMode::Auto;
        let note_h = if auto { 30 } else { 0 };
        let top = y0 + 44;
        let avail = self.bottom - top - note_h - 4;
        // All rows at up to 38 px each; when the caveats strip leaves too
        // little room, 30 px rows that scroll to keep the cursor in view.
        let (pitch, fit) = if avail / rows.len() as i32 >= 30 {
            ((avail / rows.len() as i32).min(38), rows.len())
        } else {
            (30, (avail / 30).max(1) as usize)
        };
        let cur = if editing { ui.settings_row() } else { 0 };
        let start = if cur >= fit { cur + 1 - fit } else { 0 };
        if fit < rows.len() {
            // Scrolled: say which rows show, beside the title.
            let t: String<24> = fmt(format_args!("rows {}-{} of {}", start + 1, start + fit, rows.len()));
            self.tr(&t, W - M, y0 + 6, &f.small, l.dim);
        }
        let mut v: String<40> = String::new();
        let mut y = top;
        for (i, row) in rows.iter().enumerate().skip(start).take(fit) {
            let sel = editing && ui.settings_row() == i;
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
            if *row == SettingsRow::Brightness {
                for lvl in 0..5u8 {
                    let x = 330 + i32::from(lvl) * 30;
                    let h = 8 + i32::from(lvl) * 4;
                    let b = y + pitch - 10;
                    if lvl < st.brightness {
                        self.c.fill(x, b - h, 22, h, l.fg);
                    } else {
                        self.c.frame(x, b - h, 22, h, 1, l.grid);
                    }
                }
            } else {
                let vv: String<40> = if *row == SettingsRow::Theme && auto { fmt(format_args!("Auto")) } else { v.clone() };
                let done = ui.done_row() == Some(*row);
                self.t(&vv, 330, ty, &f.body_b, if done { l.ok } else { l.fg });
            }
            y += pitch;
        }
        if auto {
            let theme = ui.effective_theme(s).name();
            let mut t: String<96> = String::new();
            use core::fmt::Write;
            let hhmm = |ms: u64| {
                let t = (ms / 1000) % 86_400;
                fmt::<8>(format_args!("{:02}:{:02}", t / 3600, t % 3600 / 60))
            };
            let (warn, _) = match ui.auto_status(s) {
                AutoStatus::Waiting { day: None } => {
                    let _ = write!(t, "Auto is waiting for GPS (position and time): {theme} until then");
                    (true, ())
                }
                AutoStatus::Waiting { day: Some(_) } => {
                    let _ = write!(t, "Auto has no GPS time: keeping {theme}");
                    (true, ())
                }
                AutoStatus::Sun { day, next_utc_ms, stale_position } => {
                    let _ = write!(t, "Auto: now {theme} ({})", if day { "day" } else { "night" });
                    match next_utc_ms {
                        Some(n) => {
                            let what = match (day, st.switch_at) {
                                (true, crate::ui::SwitchAt::Twilight) => "dusk",
                                (true, crate::ui::SwitchAt::Sunrise) => "sunset",
                                (false, crate::ui::SwitchAt::Twilight) => "dawn",
                                (false, crate::ui::SwitchAt::Sunrise) => "sunrise",
                            };
                            let _ = write!(t, "; {} theme from {} UTC ({what})", if day { "night" } else { "day" }, hhmm(n).as_str());
                        }
                        None => {
                            let _ = write!(t, "; no switch within a day (polar {})", if day { "day" } else { "night" });
                        }
                    }
                    if stale_position {
                        let _ = t.push_str(", last fix");
                    }
                    (false, ())
                }
                AutoStatus::Off => (false, ()),
            };
            let nf = if f.body.width(&t, 0) <= W - 2 * M { f.body } else { f.small };
            self.t(&t, M, y + 4, &nf, if warn { l.warn } else { l.dim });
        }
    }

    // -------------------------------------------------------- overlays --

    pub fn alert(&mut self, key: u32, more: u8, s: &UiState) {
        let l = self.l;
        let f = self.f();
        // The page behind, faded: the banner is the thing to look at.
        self.c.veil(0, BAR_H, W, self.bottom - BAR_H, l.bg, 140);
        let (x, y, w, h) = (16, BAR_H + 14, W - 32, 196);
        let ink = if l.one_hue {
            // One hue: an outlined banner with an inverse title block.
            self.c.rrect(x, y, w, h, l.radius + 4, l.bg);
            self.c.rrect_stroke(x, y, w, h, l.radius + 4, 4.0, l.accent);
            let tw = f.head.width("NEW DRONE", 0) + 70;
            self.c.rrect(x + 18, y + 16, tw, 44, 2, l.accent);
            self.diamond((x + 42) as f32, (y + 38) as f32, 11.0, l.bg, true);
            self.t("NEW DRONE", x + 62, y + 38 - i32::from(f.head.height) / 2, &f.head, l.bg);
            l.accent
        } else {
            self.c.rrect(x, y, w, h, l.radius + 4, l.accent);
            self.diamond((x + 36) as f32, (y + 38) as f32, 12.0, l.accent_ink, true);
            self.t("NEW DRONE", x + 60, y + 38 - i32::from(f.head.height) / 2, &f.head, l.accent_ink);
            l.accent_ink
        };
        let back = if l.one_hue { l.bg } else { l.accent };
        if more > 0 {
            let t: String<20> = fmt(format_args!("+{more} more new"));
            let tx = x + 60 + f.head.width("NEW DRONE", 0) + 40;
            self.boxed(&t, tx, y + 24, &f.small_b, back, ink, 6);
        }
        let Some(dv) = s.drone(key) else {
            self.t("(already gone)", x + 24, y + 90, &f.title, ink);
            return;
        };
        self.t(&short_id(dv, 22), x + 24, y + 74, &f.id, ink);
        let mut a: String<64> = fmt(format_args!("{} \u{b7} {}", dv.id_kind.label(), radio_words(dv.radios, 20).as_str()));
        {
            use core::fmt::Write;
            if let Some(o) = dv.drone_to_operator_m {
                let _ = write!(a, " \u{b7} pilot {} away", distance_short(o).as_str());
            }
        }
        self.t(&a, x + 24, y + 118, &f.body, ink);
        let b: String<48> = fmt(format_args!(
            "Height {} \u{b7} heard {}",
            metres(dv.height_m).as_str(),
            age(s.now_ms.saturating_sub(dv.last_seen_ms)).as_str()
        ));
        self.t(&b, x + 24, y + 146, &f.body, ink);
        match dv.drone {
            Some(pos) => {
                let (n, unit) = distance(pos.distance_m);
                let big = f.num_xl;
                let right = x + w - 112;
                let nx = right - big.width(&n, 0);
                self.num(&n, nx, y + 76, &big, ink);
                self.t(unit, right + 8, y + 72, &f.head, ink);
                self.arrow((x + w - 48) as f32, (y + 126) as f32, 40.0, pos.bearing_deg, ink, 5.0);
                self.tc(compass(pos.bearing_deg), x + w - 48, y + 152, &f.data_l, ink);
            }
            None => {
                let why = if s.gps.has_position() { "no position yet" } else { "no own position" };
                self.tr(why, x + w - 24, y + 120, &f.title, ink);
            }
        }
    }

    pub fn toast(&mut self, title: &str) {
        let l = self.l;
        let f = self.f().title;
        let w = f.width(title, 2) + 64;
        let (x, y) = ((W - w) / 2, (BAR_H + self.bottom) / 2 - 30);
        self.c.rrect(x, y, w, 60, l.radius + 4, l.fg);
        self.c.text(title, x + 32, y + (60 - i32::from(f.height)) / 2, &f, l.bg, 2);
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
