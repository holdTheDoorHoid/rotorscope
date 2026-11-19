//! Drawing helpers: text placement, the big readout digits, arrows, markers
//! and number formatting. Nothing here knows about pages.

use core::fmt::Write;

use embedded_graphics::{
    mono_font::{MonoFont, MonoTextStyle, MonoTextStyleBuilder},
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{Circle, Line, PrimitiveStyle, Rectangle, Triangle},
    text::{Baseline, Text},
};
use heapless::String;

use crate::model::{DroneView, IdKind};

pub type Res<E> = Result<(), E>;

pub fn width(s: &str, font: &MonoFont) -> i32 {
    let n = s.chars().count() as u32;
    (n * font.character_size.width + n.saturating_sub(1) * font.character_spacing) as i32
}

fn style(font: &'static MonoFont<'static>, c: Rgb565) -> MonoTextStyle<'static, Rgb565> {
    MonoTextStyle::new(font, c)
}

/// Text with its top-left at (x, y). Returns the x just past it.
pub fn text<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    s: &str,
    x: i32,
    y: i32,
    font: &'static MonoFont<'static>,
    c: Rgb565,
) -> Result<i32, D::Error> {
    let p = Text::with_baseline(s, Point::new(x, y), style(font, c), Baseline::Top).draw(d)?;
    Ok(p.x)
}

/// Text on a solid background box padded by `pad` pixels.
#[allow(clippy::too_many_arguments)]
pub fn text_boxed<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    s: &str,
    x: i32,
    y: i32,
    font: &'static MonoFont<'static>,
    c: Rgb565,
    bg: Rgb565,
    pad: i32,
) -> Result<i32, D::Error> {
    let w = width(s, font);
    let h = font.character_size.height as i32;
    fill(d, x, y, w + 2 * pad, h + pad, bg)?;
    let st = MonoTextStyleBuilder::new()
        .font(font)
        .text_color(c)
        .background_color(bg)
        .build();
    Text::with_baseline(s, Point::new(x + pad, y + pad / 2), st, Baseline::Top).draw(d)?;
    Ok(x + w + 2 * pad)
}

/// Text whose right edge is at `right`. Returns its left x.
pub fn text_right<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    s: &str,
    right: i32,
    y: i32,
    font: &'static MonoFont<'static>,
    c: Rgb565,
) -> Result<i32, D::Error> {
    let x = right - width(s, font);
    text(d, s, x, y, font, c)?;
    Ok(x)
}

pub fn text_center<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    s: &str,
    cx: i32,
    y: i32,
    font: &'static MonoFont<'static>,
    c: Rgb565,
) -> Res<D::Error> {
    text(d, s, cx - width(s, font) / 2, y, font, c).map(drop)
}

pub fn fill<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    c: Rgb565,
) -> Res<D::Error> {
    if w <= 0 || h <= 0 {
        return Ok(());
    }
    Rectangle::new(Point::new(x, y), Size::new(w as u32, h as u32))
        .into_styled(PrimitiveStyle::with_fill(c))
        .draw(d)
}

#[allow(clippy::too_many_arguments)]
pub fn frame<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    c: Rgb565,
    stroke: u32,
) -> Res<D::Error> {
    Rectangle::new(Point::new(x, y), Size::new(w as u32, h as u32))
        .into_styled(PrimitiveStyle::with_stroke(c, stroke))
        .draw(d)
}

pub fn line<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    a: Point,
    b: Point,
    c: Rgb565,
    stroke: u32,
) -> Res<D::Error> {
    Line::new(a, b)
        .into_styled(PrimitiveStyle::with_stroke(c, stroke))
        .draw(d)
}

pub fn hline<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    x0: i32,
    x1: i32,
    y: i32,
    c: Rgb565,
) -> Res<D::Error> {
    fill(d, x0, y, x1 - x0, 1, c)
}

pub fn circle<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    center: Point,
    r: i32,
    c: Rgb565,
    filled: bool,
    stroke: u32,
) -> Res<D::Error> {
    let style = if filled {
        PrimitiveStyle::with_fill(c)
    } else {
        PrimitiveStyle::with_stroke(c, stroke)
    };
    Circle::with_center(center, (2 * r + 1) as u32)
        .into_styled(style)
        .draw(d)
}

pub fn triangle<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    a: Point,
    b: Point,
    c: Point,
    col: Rgb565,
    filled: bool,
) -> Res<D::Error> {
    let style = if filled {
        PrimitiveStyle::with_fill(col)
    } else {
        PrimitiveStyle::with_stroke(col, 1)
    };
    Triangle::new(a, b, c).into_styled(style).draw(d)
}

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

/// Unit vector for a compass bearing on screen (north up, y down).
pub fn dir(deg: f32) -> (f32, f32) {
    let r = deg.to_radians();
    (libm::sinf(r), -libm::cosf(r))
}

pub fn offset(p: Point, deg: f32, len: f32) -> Point {
    let (x, y) = dir(deg);
    Point::new(
        p.x + libm::roundf(x * len) as i32,
        p.y + libm::roundf(y * len) as i32,
    )
}

/// A solid arrow centred on `c`, `r` pixels from centre to tip, pointing at
/// compass bearing `deg`.
pub fn arrow<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    c: Point,
    r: i32,
    deg: f32,
    col: Rgb565,
) -> Res<D::Error> {
    let r = r as f32;
    let tip = offset(c, deg, r);
    let base = offset(c, deg, r * 0.1);
    let tail = offset(c, deg + 180.0, r * 0.9);
    let left = offset(base, deg - 90.0, r * 0.55);
    let right = offset(base, deg + 90.0, r * 0.55);
    let stroke = ((r / 4.0) as u32).max(2);
    line(d, tail, base, col, stroke)?;
    triangle(d, tip, left, right, col, true)
}

/// A drone marker: a diamond. Filled for active, hollow for stale.
pub fn diamond<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    c: Point,
    r: i32,
    col: Rgb565,
    filled: bool,
) -> Res<D::Error> {
    let n = Point::new(c.x, c.y - r);
    let s = Point::new(c.x, c.y + r);
    let e = Point::new(c.x + r, c.y);
    let w = Point::new(c.x - r, c.y);
    if filled {
        triangle(d, n, e, w, col, true)?;
        triangle(d, s, e, w, col, true)
    } else {
        for (a, b) in [(n, e), (e, s), (s, w), (w, n)] {
            line(d, a, b, col, 2)?;
        }
        Ok(())
    }
}

/// Operator (pilot) marker: an X.
pub fn cross<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    c: Point,
    r: i32,
    col: Rgb565,
) -> Res<D::Error> {
    line(d, c + Point::new(-r, -r), c + Point::new(r, r), col, 2)?;
    line(d, c + Point::new(-r, r), c + Point::new(r, -r), col, 2)
}

/// Signal strength as 4 bars of rising height, filled up to the level.
/// Shape carries the value; colour does not.
pub fn signal_bars<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    x: i32,
    bottom: i32,
    rssi: Option<i8>,
    on: Rgb565,
    off: Rgb565,
) -> Res<D::Error> {
    let level = match rssi {
        None => 0,
        Some(r) if r >= -60 => 4,
        Some(r) if r >= -70 => 3,
        Some(r) if r >= -80 => 2,
        Some(_) => 1,
    };
    for i in 0..4 {
        let h = 3 + i * 3;
        let bx = x + i * 5;
        if i < level {
            fill(d, bx, bottom - h, 4, h, on)?;
        } else {
            frame(d, bx, bottom - h, 4, h, off, 1)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Big digits
// ---------------------------------------------------------------------------

/// Digit cell width for a given height.
fn big_w(h: i32) -> i32 {
    h * 11 / 20
}
fn big_t(h: i32) -> i32 {
    (h / 7).max(3)
}

/// Width of `s` drawn with [`big`] at height `h`.
pub fn big_width(s: &str, h: i32) -> i32 {
    let (w, t) = (big_w(h), big_t(h));
    let mut x = 0;
    for (i, ch) in s.chars().enumerate() {
        if i > 0 {
            x += t;
        }
        x += match ch {
            '.' => t,
            '1' => 3 * t,
            _ => w,
        };
    }
    x
}

/// Large block digits for the readout. Built from rectangles (a seven-
/// segment layout with solid corners), so they stay crisp at any size and
/// need no font data. Handles `0-9`, `.`, `-` and space.
pub fn big<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    s: &str,
    x: i32,
    y: i32,
    h: i32,
    c: Rgb565,
) -> Result<i32, D::Error> {
    let (w, t) = (big_w(h), big_t(h));
    let mid = y + (h - t) / 2;
    let half = h / 2;
    let mut cx = x;
    for (i, ch) in s.chars().enumerate() {
        if i > 0 {
            cx += t;
        }
        if ch == '.' {
            fill(d, cx, y + h - t, t, t, c)?;
            cx += t;
            continue;
        }
        if ch == '1' {
            // A narrow cell so "1.4" does not gape, with a flag and a
            // foot so a lone "1" cannot be mistaken for a divider line.
            fill(d, cx + t, y, t, h, c)?;
            fill(d, cx, y + t / 2, t, t, c)?;
            fill(d, cx, y + h - t, 3 * t, t, c)?;
            cx += 3 * t;
            continue;
        }
        // Segments a b c d e f g, as on a seven-segment display.
        let segs: u8 = match ch {
            '0' => 0b0111111,
            '1' => 0b0000110,
            '2' => 0b1011011,
            '3' => 0b1001111,
            '4' => 0b1100110,
            '5' => 0b1101101,
            '6' => 0b1111101,
            '7' => 0b0000111,
            '8' => 0b1111111,
            '9' => 0b1101111,
            '-' => 0b1000000,
            _ => 0,
        };
        let rects = [
            (cx, y, w, t),                       // a: top
            (cx + w - t, y, t, half + t / 2),    // b: upper right
            (cx + w - t, mid, t, h - (mid - y)), // c: lower right
            (cx, y + h - t, w, t),               // d: bottom
            (cx, mid, t, h - (mid - y)),         // e: lower left
            (cx, y, t, half + t / 2),            // f: upper left
            (cx, mid, w, t),                     // g: middle
        ];
        for (bit, r) in rects.iter().enumerate() {
            if segs & (1 << bit) != 0 {
                fill(d, r.0, r.1, r.2, r.3, c)?;
            }
        }
        cx += w;
    }
    Ok(cx)
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

pub type Buf = String<32>;

/// A distance as (number, unit). Rounded to what GPS on both ends can
/// honestly support, which also stops the last digit flickering.
pub fn distance(m: f32) -> (Buf, &'static str) {
    let mut s = Buf::new();
    let unit = if m < 100.0 {
        let _ = write!(s, "{}", libm::roundf(m.max(0.0)) as u32);
        "m"
    } else if m < 995.0 {
        let _ = write!(s, "{}", (libm::roundf(m / 10.0) as u32) * 10);
        "m"
    } else if m < 9_950.0 {
        let tenths = libm::roundf(m / 100.0) as u32;
        let _ = write!(s, "{}.{}", tenths / 10, tenths % 10);
        "km"
    } else {
        let _ = write!(s, "{}", (libm::roundf(m / 1000.0) as u32).min(999));
        "km"
    };
    (s, unit)
}

/// "320 m", "1.4 km": number and unit with one space (the one spacing
/// rule for every number and its unit).
pub fn distance_short(m: f32) -> Buf {
    let (n, u) = distance(m);
    let mut s = n;
    let _ = s.push(' ');
    let _ = s.push_str(u);
    s
}

/// Degrees into 0..360.
pub fn norm(deg: f32) -> f32 {
    deg - 360.0 * libm::floorf(deg / 360.0)
}

/// Eight-point compass name for a bearing.
pub fn compass(deg: f32) -> &'static str {
    const N: [&str; 8] = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];
    let i = libm::roundf(norm(deg) / 45.0) as usize % 8;
    N[i]
}

/// "045°".
pub fn degrees(deg: f32) -> Buf {
    let mut s = Buf::new();
    let v = (libm::roundf(norm(deg)) as u32) % 360;
    let _ = write!(s, "{:03}\u{b0}", v);
    s
}

/// Time since something: "now", "12 s", "4 min", "2 h" (never "m" for
/// minutes).
pub fn age(ms: u64) -> Buf {
    let s = ms / 1000;
    let mut b = Buf::new();
    let _ = if s < 2 {
        b.push_str("now").map_err(|_| core::fmt::Error)
    } else if s < 60 {
        write!(b, "{s} s")
    } else if s < 3600 {
        write!(b, "{} min", s / 60)
    } else {
        write!(b, "{} h", s / 3600)
    };
    b
}

/// Duration for uptimes: "45 s", "12 min", "3 h 05 min", "2 d 04 h".
pub fn uptime(s: u32) -> Buf {
    let mut b = Buf::new();
    let _ = if s < 60 {
        write!(b, "{s} s")
    } else if s < 3600 {
        write!(b, "{} min", s / 60)
    } else if s < 86_400 {
        write!(b, "{} h {:02} min", s / 3600, s % 3600 / 60)
    } else {
        write!(b, "{} d {:02} h", s / 86_400, s % 86_400 / 3600)
    };
    b
}

/// A shorter uptime for narrow columns: "45 s", "12 min", "3 h 05",
/// "2 d 04" (minutes and hours implied by the unit before them).
pub fn uptime_short(s: u32) -> Buf {
    let mut b = Buf::new();
    let _ = if s < 60 {
        write!(b, "{s} s")
    } else if s < 3600 {
        write!(b, "{} min", s / 60)
    } else if s < 86_400 {
        write!(b, "{} h {:02}", s / 3600, s % 3600 / 60)
    } else {
        write!(b, "{} d {:02}", s / 86_400, s % 86_400 / 3600)
    };
    b
}

/// Metres as a whole number with a unit: "120 m". `None` gives "--".
pub fn metres(v: Option<f32>) -> Buf {
    let mut b = Buf::new();
    let _ = match v {
        Some(v) => write!(b, "{} m", libm::roundf(v) as i32),
        None => write!(b, "--"),
    };
    b
}

/// The drone's ID shortened to `n` characters for lists: the tail of a
/// serial number is the part that differs between drones (the head is the
/// manufacturer code), so the head is dropped and marked with "..".
pub fn short_id(dv: &DroneView, n: usize) -> Buf {
    let mut b = Buf::new();
    let id = dv.id.as_str();
    if dv.id_kind == IdKind::Mac {
        // "MAC" + the last two bytes: "MAC4455".
        let hex: String<12> = id.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        let keep = (n.saturating_sub(3) / 2 * 2).min(6);
        let tail = &hex[hex.len().saturating_sub(keep)..];
        let _ = write!(b, "MAC{tail}");
        return b;
    }
    if id.is_empty() {
        let _ = b.push_str("?");
    } else if dv.id_kind == IdKind::Presence {
        // "DJI O4 01020304 5756.5", "DroneID 2414.5": the start says what
        // it is. Short lists drop the "DJI " so the session ID survives.
        let id = if id.chars().count() > n { id.strip_prefix("DJI ").unwrap_or(id) } else { id };
        for c in id.chars().take(n) {
            let _ = b.push(c);
        }
    } else if id.chars().count() <= n {
        let _ = b.push_str(id);
    } else {
        let keep = n.saturating_sub(2);
        let skip = id.chars().count() - keep;
        let _ = b.push_str("..");
        for c in id.chars().skip(skip) {
            let _ = b.push(c);
        }
    }
    b
}
