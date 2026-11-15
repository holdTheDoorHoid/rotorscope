//! Page layouts for a 320x240 landscape panel.

use embedded_graphics::{
    mono_font::{MonoFont, iso_8859_1::*},
    pixelcolor::Rgb565,
    prelude::*,
};
use heapless::String;

use core::fmt::Write;

use crate::draw::{self, *};
use crate::model::*;
use crate::theme::Palette;
use crate::ui::{Mode, Page, Theme, Ui};

pub const WIDTH: i32 = 320;
pub const HEIGHT: i32 = 240;
/// Status bar height.
pub const BAR_H: i32 = 18;

// Fonts, by role. All are X11 "misc-fixed" bitmaps shipped with
// embedded-graphics (public domain), in Latin-1 so "°" is available.
const F_BAR: &MonoFont = &FONT_7X13_BOLD;
const F_SMALL: &MonoFont = &FONT_6X10;
const F_BODY: &MonoFont = &FONT_8X13_BOLD;
const F_LABEL: &MonoFont = &FONT_7X13;
const F_MED: &MonoFont = &FONT_9X15_BOLD;
const F_LARGE: &MonoFont = &FONT_10X20;

/// Draw one complete frame: clears, then the page, the status bar and any
/// overlay. Draw into a framebuffer and flush it, so the panel never shows
/// a half-drawn frame.
pub fn render<D: DrawTarget<Color = Rgb565>>(
    ui: &Ui,
    s: &UiState,
    d: &mut D,
) -> Result<(), D::Error> {
    let p = ui.settings().theme.palette();
    d.clear(p.bg)?;

    match ui.mode() {
        Mode::Detail => detail(ui, s, p, d)?,
        _ => match ui.page() {
            Page::Radar if s.gps.has_position() => radar(ui, s, p, d)?,
            Page::Radar => no_fix_home(ui, s, p, d)?,
            Page::List => list_page(ui, s, p, d)?,
            Page::Signals => signals(s, p, d)?,
            Page::Flags => flags_page(s, p, d)?,
            Page::Health => health(s, p, d)?,
            Page::Settings => settings(ui, p, d)?,
        },
    }
    status_bar(s, p, d)?;
    if let Some(a) = ui.alert() {
        alert(a.key, a.more, s, p, d)?;
    } else if ui.page_toast() {
        toast(ui.page().title(), p, d)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Status bar
// ---------------------------------------------------------------------------

fn status_bar<D: DrawTarget<Color = Rgb565>>(s: &UiState, p: &Palette, d: &mut D) -> Res<D::Error> {
    fill(d, 0, 0, WIDTH, BAR_H, p.bar)?;
    let y = 3;
    let mut b: String<24> = String::new();

    // GPS: plain when fixed, an inverse warning box when not.
    let mut x = 3;
    match s.gps.fix {
        Fix::Fix2D | Fix::Fix3D => {
            x = text(d, "GPS", x, y, F_BAR, p.fg)?;
            let _ = write!(b, "{}", s.gps.sats);
            x = text(d, &b, x + 4, y, F_BAR, p.fg)?;
            if s.gps.fix == Fix::Fix2D {
                x = text(d, "2D", x + 3, y + 2, F_SMALL, p.dim)?;
            }
        }
        // A typed position: distances work, but it is not a live fix.
        Fix::Manual => {
            x = text(d, "POS", x, y, F_BAR, p.fg)?;
            x = text(d, "MAN", x + 4, y + 2, F_SMALL, p.dim)?;
        }
        Fix::None => {
            x = text_boxed(d, "NO FIX", x - 2, 1, F_BAR, p.accent_ink, p.accent, 2)?;
        }
        Fix::Absent => {
            x = text_boxed(d, "NO GPS", x - 2, 1, F_BAR, p.accent_ink, p.accent, 2)?;
        }
    }

    // Nodes: a filled dot when alive, a hollow crossed one when not, each
    // with its label so the meaning never rests on colour. Four or more
    // (an ADS-B receiver too) are packed closer so the right side fits.
    x += 8;
    let (to_label, gap) = if s.nodes.len() > 3 { (10, 2) } else { (11, 6) };
    for n in &s.nodes {
        let c = Point::new(x + 4, BAR_H / 2);
        if n.alive {
            circle(d, c, 4, p.fg, true, 1)?;
        } else {
            circle(d, c, 4, p.accent, false, 1)?;
            line(d, c + Point::new(-3, 3), c + Point::new(3, -3), p.accent, 1)?;
        }
        let col = if n.alive { p.fg } else { p.accent };
        x = text(d, n.kind.short(), x + to_label, y, F_BAR, col)?;
        x += gap;
    }

    // Right side, from the right edge inwards: time, power, recording.
    let mut rx = WIDTH - 3;
    b.clear();
    match s.utc_ms {
        Some(ms) => {
            let t = (ms / 1000) % 86_400;
            let _ = write!(b, "{:02}:{:02}", t / 3600, t % 3600 / 60);
        }
        None => {
            let _ = b.push_str("--:--");
        }
    }
    rx = text_right(d, "UTC", rx, y + 2, F_SMALL, p.dim)? - 3;
    rx = text_right(d, &b, rx, y, F_BAR, p.fg)? - 9;

    b.clear();
    match s.power {
        Power::Usb { .. } => {
            rx = text_right(d, "USB", rx, y, F_BAR, p.fg)? - 9;
        }
        Power::Battery { pct } => {
            let _ = write!(b, "{pct}%");
            // Battery outline with a proportional fill.
            let bx = rx - 18;
            frame(d, bx, 5, 16, 9, p.fg, 1)?;
            fill(d, bx + 16, 7, 2, 5, p.fg)?;
            let level = (i32::from(pct.min(100)) * 12 + 50) / 100;
            fill(
                d,
                bx + 2,
                7,
                level,
                5,
                if pct <= 15 { p.accent } else { p.fg },
            )?;
            rx = bx - 3;
            rx = if pct <= 15 {
                text_right(d, &b, rx, y, F_BAR, p.accent)?
            } else {
                text_right(d, &b, rx, y, F_BAR, p.fg)?
            } - 9;
        }
    }

    match s.sd {
        SdLog::Off => {}
        SdLog::Recording => {
            let l = text_right(d, "REC", rx, y, F_BAR, p.fg)?;
            circle(d, Point::new(l - 7, BAR_H / 2), 4, p.rec, true, 1)?;
        }
        // Quiet, not boxed: running without a card is allowed.
        SdLog::NoCard => {
            text_right(d, "no SD", rx, y + 2, F_SMALL, p.dim)?;
        }
        SdLog::Error => {
            let w = width("SD!", F_BAR) + 4;
            text_boxed(d, "SD!", rx - w, 1, F_BAR, p.accent_ink, p.accent, 2)?;
        }
    }
    let _ = x;
    hline(d, 0, WIDTH, BAR_H, p.grid)
}

// ---------------------------------------------------------------------------
// Radar
// ---------------------------------------------------------------------------

const RADAR_R: i32 = 104;
const RADAR_C: Point = Point::new(111, BAR_H + 4 + RADAR_R);
/// Range rings, metres. The outer one is the edge of the plot.
const RINGS: [(f32, &str); 4] = [
    (100.0, "100m"),
    (300.0, "300m"),
    (1000.0, "1km"),
    (3000.0, "3km"),
];
const LOG_K: f32 = 30.0;

/// Log-ish range scale: near drones get room, far ones still fit.
fn radius(m: f32) -> f32 {
    let max = RINGS[RINGS.len() - 1].0;
    let f = libm::logf(1.0 + m.max(0.0) / LOG_K) / libm::logf(1.0 + max / LOG_K);
    f.min(1.0) * RADAR_R as f32
}

/// The drone the radar's big readout describes: the cursor in pick mode,
/// otherwise the nearest active drone with a position.
fn focus<'a>(ui: &Ui, s: &'a UiState, order: &[u8]) -> Option<(usize, &'a DroneView, bool)> {
    if let Some(k) = ui.cursor()
        && let Some(i) = order
            .iter()
            .position(|&i| s.drones[usize::from(i)].key == k)
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

fn radar<D: DrawTarget<Color = Rgb565>>(
    ui: &Ui,
    s: &UiState,
    p: &Palette,
    d: &mut D,
) -> Res<D::Error> {
    let c = RADAR_C;
    let order = s.order();

    // Rings, labelled where they cross the south axis.
    for (m, _) in RINGS {
        circle(d, c, libm::roundf(radius(m)) as i32, p.grid, false, 1)?;
    }
    line(
        d,
        c - Point::new(0, RADAR_R),
        c + Point::new(0, RADAR_R),
        p.grid,
        1,
    )?;
    line(
        d,
        c - Point::new(RADAR_R, 0),
        c + Point::new(RADAR_R, 0),
        p.grid,
        1,
    )?;
    for (m, label) in RINGS {
        let y = c.y + libm::roundf(radius(m)) as i32 - 5;
        let w = width(label, F_SMALL);
        fill(d, c.x + 3, y - 1, w + 2, 11, p.bg)?;
        text(d, label, c.x + 4, y, F_SMALL, p.dim)?;
    }
    // Cardinal letters inside the edge. N is boxed and bold: this plot is
    // north-up, not "the way you are facing".
    let top = c.y - RADAR_R;
    text_boxed(d, "N", c.x - 6, top - 2, F_BAR, p.bg, p.fg, 2)?;
    text(d, "E", c.x + RADAR_R - 9, c.y - 14, F_LABEL, p.dim)?;
    text(d, "W", c.x - RADAR_R + 3, c.y - 14, F_LABEL, p.dim)?;
    for (i, l) in ["NORTH UP", "not the", "way you", "face"]
        .iter()
        .enumerate()
    {
        let f = if i == 0 { F_BAR } else { F_SMALL };
        let y = if i == 0 {
            BAR_H + 3
        } else {
            BAR_H + 8 + 10 * i as i32
        };
        text(d, l, 2, y, f, if i == 0 { p.fg } else { p.dim })?;
    }

    // You: a ring with a dot. Deliberately not a triangle, which would
    // suggest the unit knows which way you face.
    circle(d, c, 4, p.fg, false, 1)?;
    circle(d, c, 1, p.fg, true, 1)?;

    let foc = focus(ui, s, &order);
    let focus_key = foc.map(|f| f.1.key);

    // Operator of the focused drone, joined to it by a thin line.
    if let Some((_, f, _)) = foc
        && let (Some(dp), Some(op)) = (f.drone, f.operator)
    {
        let a = plot(c, dp);
        let b = plot(c, op);
        line(d, a, b, p.operator, 1)?;
        cross(d, b, 4, p.operator)?;
        text_boxed(d, "P", b.x + 6, b.y - 13, F_SMALL, p.operator, p.bg, 1)?;
    }

    // Stale first, then active, focused last so it is on top.
    let mut unplaced = 0u32;
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
            let want = if is_focus {
                2
            } else if dv.stale {
                0
            } else {
                1
            };
            if want != pass {
                continue;
            }
            let pt = plot(c, pos);
            let col = if dv.stale { p.stale } else { p.accent };
            if pos.distance_m > RINGS[RINGS.len() - 1].0 {
                // Beyond the outer ring: a pointer on the edge, facing out.
                let tip = draw::offset(c, pos.bearing_deg, RADAR_R as f32 + 2.0);
                let l = draw::offset(pt, pos.bearing_deg - 90.0, 6.0);
                let r = draw::offset(pt, pos.bearing_deg + 90.0, 6.0);
                let base = draw::offset(c, pos.bearing_deg, RADAR_R as f32 - 8.0);
                let l = l + (base - pt);
                let r = r + (base - pt);
                triangle(d, tip, l, r, col, !dv.stale)?;
            } else {
                let r = if is_focus { 7 } else { 5 };
                diamond(d, pt, r, col, !dv.stale)?;
            }
            if is_focus {
                circle(d, pt, 11, p.fg, false, 2)?;
            }
            if rank < 9 {
                let mut n: String<4> = String::new();
                let _ = write!(n, "{}", rank + 1);
                let lc = if dv.stale { p.stale } else { p.fg };
                let o = if is_focus { 11 } else { 7 };
                text_boxed(d, &n, pt.x + o, pt.y - o - 9, F_SMALL, lc, p.bg, 1)?;
            }
        }
    }
    if unplaced > 0 {
        let mut b: String<24> = String::new();
        let _ = write!(b, "+{unplaced} without");
        text(d, &b, 2, HEIGHT - 24, F_SMALL, p.dim)?;
        text(d, "position", 2, HEIGHT - 13, F_SMALL, p.dim)?;
    }

    readout(ui, s, p, d, foc)
}

fn plot(c: Point, pos: Polar) -> Point {
    draw::offset(c, pos.bearing_deg, radius(pos.distance_m))
}

/// Right-hand column of the radar: the focused drone in big type.
fn readout<D: DrawTarget<Color = Rgb565>>(
    _ui: &Ui,
    s: &UiState,
    p: &Palette,
    d: &mut D,
    foc: Option<(usize, &DroneView, bool)>,
) -> Res<D::Error> {
    let x0 = 226;
    let right = WIDTH - 4;
    fill(d, x0 - 3, BAR_H + 1, 1, HEIGHT - BAR_H - 1, p.grid)?;
    let mut b: String<24> = String::new();

    match foc {
        Some((rank, dv, picked)) => {
            if picked {
                let _ = write!(b, "DRONE {}", rank + 1);
                text_boxed(d, &b, x0, BAR_H + 3, F_BAR, p.bg, p.fg, 2)?;
            } else {
                let _ = write!(b, "NEAREST");
                text(d, &b, x0, BAR_H + 4, F_BAR, p.dim)?;
                diamond(d, Point::new(right - 5, BAR_H + 10), 5, p.accent, true)?;
            }
            let pos = dv.drone.unwrap_or_default();
            let (n, unit) = distance(pos.distance_m);
            let col = if dv.stale { p.stale } else { p.fg };
            let h = 44;
            let mut bh = h;
            while big_width(&n, bh) > right - x0 && bh > 24 {
                bh -= 4;
            }
            big(d, &n, x0, 40 + (h - bh), bh, col)?;
            let unit_word = if unit == "m" { "metres" } else { "km" };
            text(d, unit_word, x0, 86, F_MED, p.dim)?;

            // Bearing: an arrow in a small north-up rose, then the words.
            let rc = Point::new(x0 + 17, 131);
            circle(d, rc, 17, p.grid, false, 1)?;
            text_center(d, "N", rc.x, 103, F_SMALL, p.dim)?;
            arrow(d, rc, 14, pos.bearing_deg, col)?;
            text(d, compass(pos.bearing_deg), x0 + 40, 113, F_LARGE, col)?;
            text(d, &degrees(pos.bearing_deg), x0 + 40, 134, F_BODY, col)?;

            // Height, then the ID.
            b.clear();
            let _ = write!(b, "{} up", metres(dv.height_m));
            text(d, "HEIGHT", x0, 152, F_SMALL, p.dim)?;
            text(d, &b, x0, 163, F_BODY, col)?;
            text(d, &short_id(dv, 11), x0, 180, F_LABEL, p.dim)?;
        }
        None => {
            let msg: &[&str] = if s.drones.is_empty() {
                &["Listening", "for drones"]
            } else {
                &["No drone", "position yet"]
            };
            for (i, l) in msg.iter().enumerate() {
                text(d, l, x0, BAR_H + 40 + 18 * i as i32, F_MED, p.dim)?;
            }
        }
    }

    // Count, always in the same place.
    hline(d, x0 - 3, WIDTH, HEIGHT - 44, p.grid)?;
    count_block(s, p, d, x0, HEIGHT - 40)
}

/// "5 DRONES / 2 stale" with the number in big digits.
fn count_block<D: DrawTarget<Color = Rgb565>>(
    s: &UiState,
    p: &Palette,
    d: &mut D,
    x: i32,
    y: i32,
) -> Res<D::Error> {
    let active = s.active_count();
    let total = usize::from(s.total_drones).max(s.drones.len());
    let mut b: String<24> = String::new();
    let _ = write!(b, "{active}");
    let after = big(
        d,
        &b,
        x,
        y + 2,
        32,
        if active > 0 { p.accent } else { p.dim },
    )?;
    text(
        d,
        if active == 1 { "DRONE" } else { "DRONES" },
        after + 6,
        y + 4,
        F_LABEL,
        p.fg,
    )?;
    b.clear();
    let stale = total - active;
    if stale > 0 {
        let _ = write!(b, "+{stale} stale");
    } else {
        let _ = write!(b, "{}", if active == 0 { "none heard" } else { "active" });
    }
    text(d, &b, after + 6, y + 20, F_SMALL, p.dim)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// No GPS fix: list first (P4)
// ---------------------------------------------------------------------------

fn no_fix_banner<D: DrawTarget<Color = Rgb565>>(p: &Palette, d: &mut D, y: i32) -> Res<D::Error> {
    fill(d, 0, y, WIDTH, 18, p.accent)?;
    text(d, "!", 6, y + 2, F_BAR, p.accent_ink)?;
    text(d, "NO POSITION", 18, y + 2, F_BAR, p.accent_ink)?;
    text(
        d,
        "- distances unavailable",
        18 + width("NO POSITION ", F_BAR),
        y + 3,
        F_LABEL,
        p.accent_ink,
    )?;
    Ok(())
}

fn no_fix_home<D: DrawTarget<Color = Rgb565>>(
    ui: &Ui,
    s: &UiState,
    p: &Palette,
    d: &mut D,
) -> Res<D::Error> {
    let y = BAR_H + 1;
    no_fix_banner(p, d, y)?;
    // Count, big, with an honest note on what the list shows instead.
    count_block(s, p, d, 6, y + 22)?;
    text_right(d, "newest first", WIDTH - 6, y + 24, F_SMALL, p.dim)?;
    text_right(d, "signal is not", WIDTH - 6, y + 36, F_SMALL, p.dim)?;
    text_right(d, "distance", WIDTH - 6, y + 46, F_SMALL, p.dim)?;
    rows(ui, s, p, d, y + 60)
}

// ---------------------------------------------------------------------------
// List
// ---------------------------------------------------------------------------

const ROW_H: i32 = 18;
// List columns: left edges, or right edges for numbers.
const C_ID: i32 = 22;
const C_DIST: i32 = 136; // right
const C_DIR: i32 = 150; // arrow centre
const C_SIG: i32 = 96;
const C_HGT: i32 = 218; // right
const C_AGE: i32 = 256; // right
const C_RADIO: i32 = 268;

fn list_page<D: DrawTarget<Color = Rgb565>>(
    ui: &Ui,
    s: &UiState,
    p: &Palette,
    d: &mut D,
) -> Res<D::Error> {
    let mut y = BAR_H + 1;
    if !s.gps.has_position() {
        no_fix_banner(p, d, y)?;
        y += 19;
    }
    let mut b: String<24> = String::new();
    let _ = write!(b, "DRONES {}", s.active_count());
    text(d, &b, 4, y + 3, F_BAR, p.fg)?;
    let sort = if s.gps.has_position() {
        "nearest first"
    } else {
        "newest first"
    };
    text_right(d, sort, WIDTH - 4, y + 5, F_SMALL, p.dim)?;
    rows(ui, s, p, d, y + 19)
}

/// Column header, rows, stale divider and the hint line, from `top` down.
fn rows<D: DrawTarget<Color = Rgb565>>(
    ui: &Ui,
    s: &UiState,
    p: &Palette,
    d: &mut D,
    top: i32,
) -> Res<D::Error> {
    let fix = s.gps.has_position();
    let order = s.order();
    let hint_y = HEIGHT - 12;

    // Header.
    let hc = p.dim;
    text(d, "#", 4, top, F_SMALL, hc)?;
    text(d, "ID", C_ID, top, F_SMALL, hc)?;
    if fix {
        text_right(d, "DIST", C_DIST, top, F_SMALL, hc)?;
        text(d, "DIR", C_DIR - 7, top, F_SMALL, hc)?;
    } else {
        text(d, "SIGNAL", C_SIG, top, F_SMALL, hc)?;
    }
    text_right(d, "HGT", C_HGT, top, F_SMALL, hc)?;
    text_right(d, "AGE", C_AGE, top, F_SMALL, hc)?;
    text(d, "RADIO", C_RADIO, top, F_SMALL, hc)?;
    hline(d, 0, WIDTH, top + 11, p.grid)?;

    if order.is_empty() {
        text_center(d, "No drones heard yet", WIDTH / 2, top + 30, F_MED, p.dim)?;
        text_center(
            d,
            "Listening on every radio",
            WIDTH / 2,
            top + 50,
            F_LABEL,
            p.dim,
        )?;
        return Ok(());
    }

    // Rows that fit, scrolled so the cursor stays visible.
    let first_y = top + 14;
    let stale_at = order.iter().position(|&i| s.drones[usize::from(i)].stale);
    let divider = if stale_at.is_some_and(|i| i > 0) {
        12
    } else {
        0
    };
    let fit = ((hint_y - 2 - first_y - divider) / ROW_H).max(1) as usize;
    let cur = ui.cursor().and_then(|k| {
        order
            .iter()
            .position(|&i| s.drones[usize::from(i)].key == k)
    });
    let start = match cur {
        Some(c) if c >= fit => c + 1 - fit,
        _ => 0,
    };

    let mut y = first_y;
    for (rank, &i) in order.iter().enumerate().skip(start).take(fit) {
        let dv = &s.drones[usize::from(i)];
        if Some(rank) == stale_at && rank > 0 {
            hline(d, 4, WIDTH - 4, y + 5, p.grid)?;
            text_boxed(d, "not heard 60s+", 100, y, F_SMALL, p.stale, p.bg, 2)?;
            y += 12;
        }
        let selected = Some(rank) == cur;
        let (fg, bg) = if selected {
            fill(d, 0, y - 1, WIDTH, ROW_H, p.fg)?;
            (p.bg, p.fg)
        } else if dv.stale {
            (p.stale, p.bg)
        } else {
            (p.fg, p.bg)
        };
        row(dv, rank, fix, s.now_ms, p, fg, bg, d, y)?;
        y += ROW_H;
    }
    let more = order.len().saturating_sub(start + fit);
    if more > 0 {
        let mut b: String<16> = String::new();
        let _ = write!(b, "+{more} more");
        text_right(d, &b, WIDTH - 4, hint_y, F_SMALL, p.dim)?;
    }
    let hint = if ui.mode() == Mode::Pick {
        "UP/DN choose  SEL open  hold SEL back"
    } else {
        "SEL: choose a drone   UP/DN: pages"
    };
    text(d, hint, 4, hint_y, F_SMALL, p.dim)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn row<D: DrawTarget<Color = Rgb565>>(
    dv: &DroneView,
    rank: usize,
    fix: bool,
    now: u64,
    p: &Palette,
    fg: Rgb565,
    bg: Rgb565,
    d: &mut D,
    y: i32,
) -> Res<D::Error> {
    let mut b: String<8> = String::new();
    let _ = write!(b, "{}", rank + 1);
    text_right(d, &b, 16, y, F_BODY, fg)?;
    text(d, &short_id(dv, 8), C_ID, y, F_BODY, fg)?;
    if fix {
        match dv.drone {
            Some(pos) => {
                text_right(d, &distance_short(pos.distance_m), C_DIST, y, F_BODY, fg)?;
                arrow(d, Point::new(C_DIR, y + 6), 7, pos.bearing_deg, fg)?;
                text(d, compass(pos.bearing_deg), C_DIR + 10, y, F_BODY, fg)?;
            }
            None => {
                text_right(d, "--", C_DIST, y, F_BODY, fg)?;
                text(d, "no pos", C_DIR - 7, y + 2, F_SMALL, fg)?;
            }
        }
    } else {
        let off = if bg == p.bg { p.grid } else { fg };
        signal_bars(d, C_SIG, y + 12, dv.best_rssi(), fg, off)?;
        if let Some(r) = dv.best_rssi() {
            let mut b: String<8> = String::new();
            let _ = write!(b, "{r}");
            text(d, &b, C_SIG + 24, y, F_BODY, fg)?;
        }
    }
    text_right(d, &metres(dv.height_m), C_HGT, y, F_BODY, fg)?;
    text_right(
        d,
        &age(now.saturating_sub(dv.last_seen_ms)),
        C_AGE,
        y,
        F_BODY,
        fg,
    )?;
    radio_letters(
        d,
        dv.radios,
        C_RADIO,
        y,
        F_BODY,
        fg,
        if bg == p.bg { p.grid } else { fg },
    )
}

/// "WN.L5": each radio in a fixed column; a dot where it was not heard.
fn radio_letters<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    r: Radios,
    x: i32,
    y: i32,
    font: &'static MonoFont<'static>,
    on: Rgb565,
    off: Rgb565,
) -> Res<D::Error> {
    let cw = font.character_size.width as i32;
    for (i, radio) in Radio::ALL.iter().enumerate() {
        let mut s: String<4> = String::new();
        if r.contains(*radio) {
            let _ = s.push(radio.letter());
            text(d, &s, x + i as i32 * cw, y, font, on)?;
        } else {
            let _ = s.push('\u{b7}');
            text(d, &s, x + i as i32 * cw, y, font, off)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Detail
// ---------------------------------------------------------------------------

fn detail<D: DrawTarget<Color = Rgb565>>(
    ui: &Ui,
    s: &UiState,
    p: &Palette,
    d: &mut D,
) -> Res<D::Error> {
    let order = s.order();
    let key = ui.cursor();
    let found = key.and_then(|k| {
        order
            .iter()
            .position(|&i| s.drones[usize::from(i)].key == k)
            .map(|r| (r, &s.drones[usize::from(order[r])]))
    });
    let Some((rank, dv)) = found else {
        text_center(d, "Drone no longer tracked", WIDTH / 2, 100, F_MED, p.fg)?;
        text_center(d, "hold SEL to go back", WIDTH / 2, 124, F_LABEL, p.dim)?;
        return Ok(());
    };
    let col = if dv.stale { p.stale } else { p.fg };
    let mut b: String<48> = String::new();

    // Header: "DRONE 2 of 5 . Serial number", type on the right, then the
    // full ID large (smaller when it is a long hex ID).
    let y0 = BAR_H + 3;
    let _ = write!(
        b,
        "DRONE {} of {}  {}",
        rank + 1,
        order.len(),
        dv.id_kind.label()
    );
    text(d, &b, 4, y0, F_SMALL, p.dim)?;
    b.clear();
    if dv.stale {
        let _ = write!(b, "STALE");
        text_boxed(
            d,
            &b,
            WIDTH - 4 - width("STALE", F_SMALL) - 4,
            y0 - 1,
            F_SMALL,
            p.bg,
            p.stale,
            2,
        )?;
    } else if !dv.model.is_empty() {
        text_right(d, &dv.model, WIDTH - 4, y0, F_SMALL, p.dim)?;
    } else {
        text_right(d, dv.ua.label(), WIDTH - 4, y0, F_SMALL, p.dim)?;
    }
    let id = if dv.id.is_empty() {
        "(none)"
    } else {
        dv.id.as_str()
    };
    if width(id, F_LARGE) <= WIDTH - 8 {
        text(d, id, 4, y0 + 12, F_LARGE, col)?;
    } else {
        text(d, id, 4, y0 + 16, F_BODY, col)?;
    }
    hline(d, 0, WIDTH, y0 + 34, p.grid)?;

    // Label / value rows.
    let lx = 4;
    let vx = 64;
    let rh = 18;
    let mut y = y0 + 38;
    let label = |d: &mut D, t: &str, y: i32| text(d, t, lx, y + 2, F_SMALL, p.dim).map(drop);

    // Direction rows: distance, a north-up arrow, compass point and degrees.
    let fix = s.gps.has_position();
    for (name, pos, is_pilot) in [("DRONE", dv.drone, false), ("PILOT", dv.operator, true)] {
        label(d, name, y)?;
        match pos {
            Some(pp) => {
                let x = text(d, &distance_short(pp.distance_m), vx, y, F_BODY, col)?;
                arrow(d, Point::new(x + 12, y + 6), 7, pp.bearing_deg, col)?;
                b.clear();
                let _ = write!(b, "{} {}", compass(pp.bearing_deg), degrees(pp.bearing_deg));
                let x = text(d, &b, x + 24, y, F_BODY, col)?;
                if is_pilot {
                    cross(d, Point::new(x + 10, y + 6), 4, p.operator)?;
                }
            }
            None if is_pilot && dv.drone_to_operator_m.is_some() => {
                // No own fix, but both drone and pilot positions are known.
                b.clear();
                let m = dv.drone_to_operator_m.unwrap_or(0.0);
                let _ = write!(b, "{} from the drone", distance_short(m));
                text(d, &b, vx, y, F_BODY, col)?;
            }
            None => {
                let why = if !fix {
                    "unknown: no own position"
                } else if is_pilot {
                    "pilot position not sent"
                } else {
                    "drone sent no position"
                };
                text(d, why, vx, y + 2, F_SMALL, p.dim)?;
            }
        }
        y += rh;
    }

    label(d, "HEIGHT", y)?;
    b.clear();
    let refr = match dv.height_ref {
        HeightRef::Takeoff => "above takeoff",
        HeightRef::Ground => "above ground",
    };
    let _ = write!(b, "{} {}", metres(dv.height_m), refr);
    if let Some(a) = dv.above_you_m {
        let _ = write!(b, ", {:+}m vs you", libm::roundf(a) as i32);
    }
    text(d, &b, vx, y, F_BODY, col)?;
    y += rh;

    label(d, "SPEED", y)?;
    b.clear();
    match dv.speed_mps {
        Some(v) => {
            let _ = write!(b, "{:.1}m/s", v);
        }
        None => {
            let _ = b.push_str("--");
        }
    }
    let mut x = text(d, &b, vx, y, F_BODY, col)?;
    if let Some(t) = dv.track_deg {
        arrow(d, Point::new(x + 12, y + 6), 7, t, col)?;
        x = text(d, compass(t), x + 24, y, F_BODY, col)?;
    }
    if let Some(v) = dv.vspeed_mps {
        b.clear();
        let r = libm::roundf(v * 10.0) / 10.0;
        let _ = if r > 0.0 {
            write!(b, "climb {:.1}", r)
        } else if r < 0.0 {
            write!(b, "sink {:.1}", -r)
        } else {
            write!(b, "level")
        };
        text(d, &b, x + 12, y, F_BODY, col)?;
    }
    y += rh;

    label(d, "OP ID", y)?;
    let op = if dv.operator_id.is_empty() {
        "--"
    } else {
        dv.operator_id.as_str()
    };
    text(d, op, vx, y, F_BODY, col)?;
    y += rh;

    label(d, "SELF-ID", y)?;
    let sid = if dv.self_id.is_empty() {
        "--"
    } else {
        dv.self_id.as_str()
    };
    text(d, sid, vx, y, F_BODY, col)?;
    y += rh;

    // Signal: each radio that heard it, as letter, bars and dBm.
    label(d, "SIGNAL", y)?;
    let heard = Radio::ALL
        .iter()
        .filter(|r| dv.rssi[r.index()].is_some())
        .count();
    let mut x = vx;
    for r in Radio::ALL {
        let Some(rssi) = dv.rssi[r.index()] else {
            continue;
        };
        let mut l: String<2> = String::new();
        let _ = l.push(r.letter());
        x = text(d, &l, x, y, F_BODY, col)?;
        signal_bars(d, x + 3, y + 12, Some(rssi), col, p.grid)?;
        x += 27;
        if heard <= 4 {
            b.clear();
            let _ = write!(b, "{rssi}");
            x = text(d, &b, x, y + 2, F_SMALL, col)?;
        }
        x += 10;
    }
    if heard == 0 {
        text(d, "--", vx, y, F_BODY, col)?;
    }
    y += rh;

    label(d, "HEARD", y)?;
    b.clear();
    let _ = write!(
        b,
        "{} ago, first {} ago",
        age(s.now_ms.saturating_sub(dv.last_seen_ms)),
        age(s.now_ms.saturating_sub(dv.first_seen_ms))
    );
    let last = s.now_ms.saturating_sub(dv.last_seen_ms);
    if last < 2000 {
        b.clear();
        let _ = write!(
            b,
            "now, first {} ago",
            age(s.now_ms.saturating_sub(dv.first_seen_ms))
        );
    }
    text(d, &b, vx, y, F_BODY, col)?;

    text(
        d,
        "UP/DN next drone   hold SEL back",
        4,
        HEIGHT - 12,
        F_SMALL,
        p.dim,
    )
    .map(drop)
}

// ---------------------------------------------------------------------------
// Signals
// ---------------------------------------------------------------------------

/// RF emitters, then Wi-Fi drones: two short tables, newest data only.
/// Neither has a position, so this page has no cursor.
fn signals<D: DrawTarget<Color = Rgb565>>(s: &UiState, p: &Palette, d: &mut D) -> Res<D::Error> {
    const RH: i32 = 14;
    let mut y = BAR_H + 4;
    let hint_y = HEIGHT - 12;
    let mut b: String<40> = String::new();
    text(d, "SIGNALS", 4, y, F_BAR, p.fg)?;
    let _ = write!(
        b,
        "{} RF, {} Wi-Fi drones",
        s.total_emitters.max(s.emitters.len() as u16),
        s.total_wifi_drones.max(s.wifi_drones.len() as u16)
    );
    text_right(d, &b, WIDTH - 4, y + 2, F_SMALL, p.dim)?;
    y += 18;

    // RF: kind, frequency, level, duty or line rate, age.
    for (x, h, right) in [
        (4, "RF", false),
        (150, "MHz", true),
        (196, "LEVEL", true),
        (252, "DUTY", true),
        (314, "AGE", true),
    ] {
        if right {
            text_right(d, h, x, y, F_SMALL, p.dim)?;
        } else {
            text(d, h, x, y, F_SMALL, p.dim)?;
        }
    }
    hline(d, 0, WIDTH, y + 11, p.grid)?;
    y += 14;
    // Leave room for the Wi-Fi table: header plus at least two rows.
    let rf_rows = ((hint_y - y - 14 - 2 * RH - 4) / RH).max(1) as usize;
    if s.emitters.is_empty() {
        text(d, "none heard", 18, y, F_LABEL, p.dim)?;
        y += RH;
    }
    for e in s.emitters.iter().take(rf_rows) {
        let col = if e.stale { p.stale } else { p.fg };
        text(d, e.kind.label(), 4, y, F_LABEL, col)?;
        b.clear();
        let _ = write!(b, "{:.1}", e.freq_mhz);
        text_right(d, &b, 150, y, F_LABEL, col)?;
        b.clear();
        let _ = write!(b, "+{}dB", e.snr_db.max(0));
        text_right(d, &b, 196, y, F_LABEL, col)?;
        b.clear();
        let _ = if e.line_hz != 0 {
            write!(b, "{:.1}k", f32::from(e.line_hz) / 1000.0)
        } else {
            write!(b, "{}%", e.duty_pct)
        };
        text_right(d, &b, 252, y, F_LABEL, col)?;
        text_right(
            d,
            &age(s.now_ms.saturating_sub(e.last_seen_ms)),
            314,
            y,
            F_LABEL,
            col,
        )?;
        y += RH;
    }
    let more = usize::from(s.total_emitters).max(s.emitters.len());
    let more = more.saturating_sub(rf_rows.min(s.emitters.len()));
    if more > 0 {
        b.clear();
        let _ = write!(b, "+{more} more");
        text_right(d, &b, WIDTH - 4, y, F_SMALL, p.dim)?;
        y += 11;
    }

    y += 3;
    for (x, h, right) in [
        (4, "WI-FI DRONE", false),
        (110, "NETWORK", false),
        (252, "CH", true),
        (282, "dBm", true),
        (314, "AGE", true),
    ] {
        if right {
            text_right(d, h, x, y, F_SMALL, p.dim)?;
        } else {
            text(d, h, x, y, F_SMALL, p.dim)?;
        }
    }
    hline(d, 0, WIDTH, y + 11, p.grid)?;
    y += 14;
    if s.wifi_drones.is_empty() {
        text(d, "none heard", 18, y, F_LABEL, p.dim)?;
    }
    let mut shown = 0;
    for w in &s.wifi_drones {
        if y + RH > hint_y {
            break;
        }
        let col = if w.stale { p.stale } else { p.fg };
        text(d, &w.maker, 4, y, F_LABEL, col)?;
        // 20 characters of the network name fit before the channel.
        let ssid = w.ssid.get(..w.ssid.len().min(19)).unwrap_or("");
        text(d, ssid, 110, y, F_LABEL, col)?;
        b.clear();
        let _ = write!(b, "{}", w.channel);
        text_right(d, &b, 252, y, F_LABEL, col)?;
        b.clear();
        let _ = write!(b, "{}", w.rssi);
        text_right(d, &b, 282, y, F_LABEL, col)?;
        text_right(
            d,
            &age(s.now_ms.saturating_sub(w.last_seen_ms)),
            314,
            y,
            F_LABEL,
            col,
        )?;
        y += RH;
        shown += 1;
    }
    let more = usize::from(s.total_wifi_drones)
        .max(s.wifi_drones.len())
        .saturating_sub(shown);
    if more > 0 {
        b.clear();
        let _ = write!(b, "+{more} more");
        text_right(d, &b, WIDTH - 4, hint_y, F_SMALL, p.dim)?;
    }
    text(d, "UP/DN: pages", 4, hint_y, F_SMALL, p.dim).map(drop)
}

// ---------------------------------------------------------------------------
// Flags (PLAN.md Phase 12)
// ---------------------------------------------------------------------------

/// Level colours: Alert in the accent, Warning in full ink, Watch and Note
/// quieter. Words carry the meaning too (no colour-only signal).
fn level_label(level: u8) -> &'static str {
    match level {
        0 => "NOTE",
        1 => "WATCH",
        2 => "WARN",
        _ => "ALERT",
    }
}

fn flags_page<D: DrawTarget<Color = Rgb565>>(s: &UiState, p: &Palette, d: &mut D) -> Res<D::Error> {
    const RH: i32 = 14;
    let mut y = BAR_H + 4;
    let hint_y = HEIGHT - 12;
    text(d, "FLAGS", 4, y, F_BAR, p.fg)?;
    let Some(f) = &s.flags else {
        return text(d, "no rule engine on this hub", 18, y + 24, F_LABEL, p.dim).map(drop);
    };
    text_right(d, &f.pack, WIDTH - 4, y + 2, F_SMALL, p.dim)?;
    y += 16;
    // The wording rule (PLAN.md): possible, never "illegal".
    text(
        d,
        "possible violations - hub can't see waivers/LAANC",
        4,
        y,
        F_SMALL,
        p.dim,
    )?;
    y += 13;
    for (x, h, right) in [
        (4, "LEVEL", false),
        (52, "WHO", false),
        (146, "RULE", false),
        (314, "FOR", true),
    ] {
        if right {
            text_right(d, h, x, y, F_SMALL, p.dim)?;
        } else {
            text(d, h, x, y, F_SMALL, p.dim)?;
        }
    }
    hline(d, 0, WIDTH, y + 11, p.grid)?;
    y += 14;
    if f.rows.is_empty() {
        text(d, "none", 18, y, F_LABEL, p.dim)?;
    }
    let mut b: String<40> = String::new();
    let mut shown = 0;
    for r in &f.rows {
        if y + 2 * RH > hint_y {
            break;
        }
        let col = match r.level {
            3 => p.accent,
            2 => p.fg,
            _ => p.dim,
        };
        text(d, level_label(r.level), 4, y, F_LABEL, col)?;
        // 12 characters of the ID fit before the rule column.
        text(
            d,
            r.who.get(..r.who.len().min(12)).unwrap_or(""),
            52,
            y,
            F_LABEL,
            col,
        )?;
        text(d, r.rule, 146, y, F_LABEL, col)?;
        b.clear();
        let _ = write!(b, "{}s", r.secs);
        text_right(d, &b, 314, y, F_LABEL, col)?;
        y += RH - 2;
        text(d, &r.text, 52, y, F_SMALL, p.dim)?;
        y += RH;
        shown += 1;
    }
    let more = usize::from(f.total).max(f.rows.len()).saturating_sub(shown);
    if more > 0 {
        b.clear();
        let _ = write!(b, "+{more} more");
        text_right(d, &b, WIDTH - 4, hint_y, F_SMALL, p.dim)?;
    }
    text(d, "UP/DN: pages", 4, hint_y, F_SMALL, p.dim).map(drop)
}

// ---------------------------------------------------------------------------
// Health
// ---------------------------------------------------------------------------

fn health<D: DrawTarget<Color = Rgb565>>(s: &UiState, p: &Palette, d: &mut D) -> Res<D::Error> {
    let mut y = BAR_H + 4;
    text(d, "HEALTH", 4, y, F_BAR, p.fg)?;
    let mut b: String<40> = String::new();
    let _ = write!(b, "hub up {}", uptime(s.hub_uptime_s));
    text_right(d, &b, WIDTH - 4, y + 2, F_SMALL, p.dim)?;
    y += 20;

    let cols = [
        (4, "NODE"),
        (112, "FR/S"),
        (164, "RID"),
        (222, "DROPS"),
        (274, "UP"),
    ];
    for (x, h) in cols {
        if x == 4 {
            text(d, h, x, y, F_SMALL, p.dim)?;
        } else {
            text_right(d, h, x + 40, y, F_SMALL, p.dim)?;
        }
    }
    hline(d, 0, WIDTH, y + 11, p.grid)?;
    y += 15;
    for (i, n) in s.nodes.iter().enumerate() {
        let col = if n.alive { p.fg } else { p.accent };
        let c = Point::new(9, y + 6);
        if n.alive {
            circle(d, c, 4, p.fg, true, 1)?;
        } else {
            circle(d, c, 4, p.accent, false, 1)?;
            line(d, c + Point::new(-3, 3), c + Point::new(3, -3), p.accent, 1)?;
        }
        text(d, n.kind.long(), 18, y, F_BODY, col)?;
        if n.alive {
            for (x, v) in [
                (112, n.frames_per_s as u32),
                (164, n.rid_found),
                (222, n.drops),
            ] {
                b.clear();
                let _ = write!(b, "{v}");
                text_right(d, &b, x + 40, y, F_BODY, col)?;
            }
            text_right(d, &uptime(n.uptime_s), 314, y, F_BODY, col)?;
        } else {
            text_right(d, "NOT RESPONDING", 314, y, F_BODY, col)?;
        }
        y += ROW_H;
        if let Some(Some(a)) = s.assigned.get(i) {
            b.clear();
            let state = if a.applied { "running" } else { "sent" };
            let _ = write!(b, "-> {} plan {:04x} {state}", a.role, a.plan);
            text(
                d,
                &b,
                18,
                y - 4,
                F_SMALL,
                if a.applied { p.dim } else { p.accent },
            )?;
            y += 9;
        }
    }
    if s.nodes.is_empty() {
        text(d, "no radio nodes reported", 18, y, F_LABEL, p.accent)?;
        y += ROW_H;
    }

    hline(d, 0, WIDTH, y + 2, p.grid)?;
    y += 8;
    let lines: [(&str, String<40>); 4] = [
        ("GPS", {
            let mut b = String::new();
            let _ = match s.gps.fix {
                Fix::None => write!(b, "no fix, {} satellites", s.gps.sats),
                Fix::Fix2D => write!(b, "2D fix, {} satellites", s.gps.sats),
                Fix::Fix3D => write!(b, "3D fix, {} satellites", s.gps.sats),
                Fix::Manual => write!(b, "no fix; typed position in use"),
                Fix::Absent => write!(b, "no receiver found"),
            };
            b
        }),
        ("TRACKS", {
            let mut b = String::new();
            let total = usize::from(s.total_drones).max(s.drones.len());
            let _ = write!(b, "{} ({} active)", total, s.active_count());
            b
        }),
        ("SD LOG", {
            let mut b = String::new();
            let _ = b.push_str(match s.sd {
                SdLog::Off => "off",
                SdLog::Recording => "recording",
                SdLog::NoCard => "no card",
                SdLog::Error => "ERROR - not recording",
            });
            b
        }),
        ("POWER", {
            let mut b = String::new();
            let _ = match s.power {
                Power::Usb {
                    battery_pct: Some(p),
                } => write!(b, "USB, battery {p}%"),
                Power::Usb { battery_pct: None } => write!(b, "USB"),
                Power::Battery { pct } => write!(b, "battery {pct}%"),
            };
            b
        }),
    ];
    for (k, v) in lines {
        text(d, k, 4, y + 2, F_SMALL, p.dim)?;
        text(d, &v, 70, y, F_BODY, p.fg)?;
        y += 17;
    }
    if let Some(a) = &s.aircraft {
        // A quiet line, never an alert: crewed aircraft are not what the
        // unit hunts. "LOW" is the only emphasis, in the accent colour.
        text(d, "AIRCRAFT", 4, y + 2, F_SMALL, p.dim)?;
        let mut b: String<40> = String::new();
        if a.total == 0 {
            let _ = b.push_str("none heard");
        } else {
            let _ = write!(b, "{}", a.active);
        }
        let mut x = text(d, &b, 70, y, F_BODY, p.fg)?;
        if a.low_nearby > 0 {
            b.clear();
            let _ = write!(b, " {} LOW", a.low_nearby);
            x = text(d, &b, x, y, F_BODY, p.accent)?;
        }
        if let Some(n) = &a.nearest {
            b.clear();
            let _ = write!(
                b,
                " {} {}",
                n.name,
                draw::distance_short(n.distance_m).as_str()
            );
            if let Some(ft) = n.alt_ft {
                let _ = write!(b, " {ft}ft");
            }
            text(d, &b, x, y, F_BODY, p.fg)?;
        }
    }
    text(d, "UP/DN: pages", 4, HEIGHT - 12, F_SMALL, p.dim).map(drop)
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

fn settings<D: DrawTarget<Color = Rgb565>>(ui: &Ui, p: &Palette, d: &mut D) -> Res<D::Error> {
    let y0 = BAR_H + 4;
    text(d, "SETTINGS", 4, y0, F_BAR, p.fg)?;
    let st = ui.settings();
    let editing = ui.mode() == Mode::Edit;
    let rows: [(&str, &str); 3] = [
        (
            "Theme",
            match st.theme {
                Theme::Night => "Night (dark)",
                Theme::Day => "Day (light)",
            },
        ),
        ("Brightness", ""),
        ("Buzzer", if st.buzzer { "On" } else { "Off (muted)" }),
    ];
    let mut y = y0 + 26;
    for (i, (k, v)) in rows.iter().enumerate() {
        let sel = editing && ui.settings_row() == i;
        let (fg, bg) = if sel { (p.bg, p.fg) } else { (p.fg, p.bg) };
        if sel {
            fill(d, 0, y - 4, WIDTH, 28, p.fg)?;
            text(d, ">", 6, y, F_LARGE, fg)?;
        }
        text(d, k, 24, y, F_LARGE, fg)?;
        if i == 1 {
            for lvl in 0..5u8 {
                let x = 170 + i32::from(lvl) * 22;
                let h = 6 + i32::from(lvl) * 3;
                if lvl < st.brightness {
                    fill(d, x, y + 18 - h, 16, h, fg)?;
                } else {
                    frame(d, x, y + 18 - h, 16, h, if sel { fg } else { p.grid }, 1)?;
                }
            }
        } else {
            text(d, v, 170, y, F_LARGE, fg)?;
        }
        let _ = bg;
        y += 34;
    }
    let hint = if editing {
        "UP/DN choose  SEL change  hold SEL done"
    } else {
        "SEL: change settings   UP/DN: pages"
    };
    text(d, hint, 4, HEIGHT - 12, F_SMALL, p.dim).map(drop)
}

// ---------------------------------------------------------------------------
// Overlays
// ---------------------------------------------------------------------------

fn alert<D: DrawTarget<Color = Rgb565>>(
    key: u32,
    more: u8,
    s: &UiState,
    p: &Palette,
    d: &mut D,
) -> Res<D::Error> {
    // Tall enough to cover the radar readout's big number completely, so
    // the banner never leaves fragments of a different drone's distance.
    let y = BAR_H + 1;
    let h = 93;
    let ink = p.accent_ink;
    fill(d, 0, y, WIDTH, h, p.accent)?;
    // Heavy rules top and bottom: the banner reads as an interruption even
    // on a washed-out panel, without relying on its colour.
    fill(d, 0, y, WIDTH, 3, ink)?;
    fill(d, 0, y + h - 3, WIDTH, 3, ink)?;

    diamond(d, Point::new(16, y + 17), 9, ink, true)?;
    text(d, "NEW DRONE", 32, y + 7, F_LARGE, ink)?;
    text_right(d, "SEL: details", WIDTH - 6, y + 11, F_SMALL, ink)?;

    let mut b: String<40> = String::new();
    let Some(dv) = s.drone(key) else {
        return text(d, "(already gone)", 8, y + 40, F_MED, ink).map(drop);
    };
    text(d, &short_id(dv, 14), 8, y + 34, F_MED, ink)?;
    if more > 0 {
        let _ = write!(b, "+{more} more new");
        text(d, &b, 8, y + 54, F_BODY, ink)?;
    }
    match dv.drone {
        Some(pos) => {
            // Distance in big digits, unit and compass point beside it.
            let (n, unit) = distance(pos.distance_m);
            let dir = compass(pos.bearing_deg);
            let side = width(dir, F_LARGE).max(width(unit, F_MED));
            let bh = 40;
            let bx = WIDTH - 8 - side - 6 - big_width(&n, bh);
            big(d, &n, bx, y + 36, bh, ink)?;
            text_right(d, unit, WIDTH - 8, y + 38, F_MED, ink)?;
            text_right(d, dir, WIDTH - 8, y + 56, F_LARGE, ink)?;
        }
        None => {
            let why = if s.gps.has_position() {
                "no position yet"
            } else {
                "no own position"
            };
            text_right(d, why, WIDTH - 8, y + 58, F_BODY, ink)?;
        }
    }
    Ok(())
}

fn toast<D: DrawTarget<Color = Rgb565>>(title: &str, p: &Palette, d: &mut D) -> Res<D::Error> {
    let w = width(title, F_LARGE) + 24;
    let x = (WIDTH - w) / 2;
    let y = HEIGHT / 2 - 18;
    fill(d, x, y, w, 36, p.fg)?;
    text_center(d, title, WIDTH / 2, y + 8, F_LARGE, p.bg)
}
