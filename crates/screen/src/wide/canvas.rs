//! A horizontal band of the 800x480 panel in RAM, and the drawing the wide
//! layout needs: solid fills, anti-aliased text, rings, discs, lines and
//! convex shapes. Every primitive first clips to the band's rows, so the
//! cost of drawing a frame band by band is mostly the layout logic, not
//! pixels outside the band.
//!
//! Anti-aliasing blends into the pixel already in the band (painter's
//! order), so it is correct on any background the page drew first.

use embedded_graphics::pixelcolor::{Rgb565, raw::RawU16};
use embedded_graphics::prelude::RawData;

use super::font::Font;
use super::{H, W};

/// Rounding for `f32` in `no_std` (through `libm`).
pub trait Rounding {
    fn flr(self) -> f32;
    fn cel(self) -> f32;
    fn rnd(self) -> f32;
}

impl Rounding for f32 {
    #[inline]
    fn flr(self) -> f32 {
        libm::floorf(self)
    }
    #[inline]
    fn cel(self) -> f32 {
        libm::ceilf(self)
    }
    #[inline]
    fn rnd(self) -> f32 {
        libm::roundf(self)
    }
}

/// Raw RGB565 as the panel takes it.
#[inline]
pub fn raw(c: Rgb565) -> u16 {
    RawU16::from(c).into_inner()
}

/// `a` (0..=256) of `fg` over `bg`, in RGB565.
#[inline]
pub fn mix(bg: u16, fg: u16, a: u32) -> u16 {
    if a >= 256 {
        return fg;
    }
    if a == 0 {
        return bg;
    }
    let (br, bgg, bb) = (u32::from(bg >> 11), u32::from((bg >> 5) & 63), u32::from(bg & 31));
    let (fr, fgg, fb) = (u32::from(fg >> 11), u32::from((fg >> 5) & 63), u32::from(fg & 31));
    let r = (br * (256 - a) + fr * a + 128) >> 8;
    let g = (bgg * (256 - a) + fgg * a + 128) >> 8;
    let b = (bb * (256 - a) + fb * a + 128) >> 8;
    ((r << 11) | (g << 5) | b) as u16
}

/// A band of `rows` panel rows starting at `top`, 800 pixels wide.
pub struct Canvas<'a> {
    px: &'a mut [u16],
    top: i32,
    rows: i32,
    /// Pixels a primitive tried to put outside the panel (the snapshot
    /// tests assert there are none).
    pub off_panel: u32,
}

impl<'a> Canvas<'a> {
    /// A band over `px` (whole rows of 800) at panel row `top`.
    pub fn new(px: &'a mut [u16], top: i32) -> Self {
        let cap = (px.len() / W as usize) as i32;
        let rows = cap.min(H - top).max(0);
        Self { px, top, rows, off_panel: 0 }
    }

    pub fn top(&self) -> i32 {
        self.top
    }
    pub fn rows(&self) -> i32 {
        self.rows
    }
    /// The band's pixels, `rows() * 800` of them, raw RGB565.
    pub fn pixels(&self) -> &[u16] {
        &self.px[..(self.rows * W) as usize]
    }

    /// Whether panel rows `y0..y1` touch this band.
    #[inline]
    pub fn touches(&self, y0: i32, y1: i32) -> bool {
        y1 > self.top && y0 < self.top + self.rows
    }

    /// Rows of `y0..y1` inside the band.
    #[inline]
    fn clip_rows(&self, y0: i32, y1: i32) -> (i32, i32) {
        (y0.max(self.top), y1.min(self.top + self.rows))
    }

    fn check(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) {
        // Counted once, by the band holding the shape's top row (or the
        // first band), so a full-frame and a banded render agree.
        if (x0 < 0 || y0 < 0 || x1 > W || y1 > H) && (self.touches(y0, y0 + 1) || (y0 < 0 && self.top == 0)) {
            self.off_panel += 1;
        }
    }

    #[inline]
    fn at(&self, x: i32, y: i32) -> usize {
        ((y - self.top) * W + x) as usize
    }

    pub fn clear(&mut self, c: Rgb565) {
        let n = (self.rows * W) as usize;
        self.px[..n].fill(raw(c));
    }

    /// A solid rectangle.
    pub fn fill(&mut self, x: i32, y: i32, w: i32, h: i32, c: Rgb565) {
        if w <= 0 || h <= 0 {
            return;
        }
        self.check(x, y, x + w, y + h);
        let (r0, r1) = self.clip_rows(y, y + h);
        let (x0, x1) = (x.max(0), (x + w).min(W));
        if x0 >= x1 {
            return;
        }
        let v = raw(c);
        for yy in r0..r1 {
            let i = self.at(0, yy);
            self.px[i + x0 as usize..i + x1 as usize].fill(v);
        }
    }

    /// A rectangle outline, `t` pixels thick, inside the rectangle.
    pub fn frame(&mut self, x: i32, y: i32, w: i32, h: i32, t: i32, c: Rgb565) {
        self.fill(x, y, w, t, c);
        self.fill(x, y + h - t, w, t, c);
        self.fill(x, y + t, t, h - 2 * t, c);
        self.fill(x + w - t, y + t, t, h - 2 * t, c);
    }

    /// Blend `c` at `a` (0..=256) over the pixel at (x, y).
    #[inline]
    pub fn blend(&mut self, x: i32, y: i32, c: u16, a: u32) {
        if a == 0 || !(0..W).contains(&x) || !(self.top..self.top + self.rows).contains(&y) {
            return;
        }
        let i = self.at(x, y);
        self.px[i] = mix(self.px[i], c, a);
    }

    /// Pull a region towards `c` by `a` (0..=256): the page behind an
    /// alert, faded.
    pub fn veil(&mut self, x: i32, y: i32, w: i32, h: i32, c: Rgb565, a: u32) {
        let (r0, r1) = self.clip_rows(y, y + h);
        let (x0, x1) = (x.max(0), (x + w).min(W));
        let v = raw(c);
        for yy in r0..r1 {
            for xx in x0..x1 {
                let i = self.at(xx, yy);
                self.px[i] = mix(self.px[i], v, a);
            }
        }
    }

    // ------------------------------------------------------------- text --

    /// Draw `s` with the line's top at `y` (baseline at `y + ascent`),
    /// `track` extra pixels between letters. Returns the x just past it.
    #[allow(clippy::too_many_arguments)]
    pub fn text(&mut self, s: &str, x: i32, y: i32, f: &Font, c: Rgb565, track: i32) -> i32 {
        let end = x + f.width(s, track);
        let base = y + i32::from(f.ascent);
        // Glyphs may reach a little past the line box (accents, descenders).
        if !self.touches(y - 4, y + i32::from(f.height) + 4) {
            return end;
        }
        let blob = super::glyphs::blob();
        let v = raw(c);
        let mut pen16 = x * 16;
        for ch in s.chars() {
            let g = f.glyph(ch);
            let gx = (pen16 + 8) / 16 + i32::from(g.x);
            let gy = base + i32::from(g.y);
            let (w, h) = (i32::from(g.w), i32::from(g.h));
            if w > 0 && h > 0 {
                self.check(gx, gy, gx + w, gy + h);
                let (r0, r1) = self.clip_rows(gy, gy + h);
                for yy in r0..r1 {
                    let row = (yy - gy) * w;
                    for dx in 0..w {
                        let n = (row + dx) as usize;
                        let byte = blob[g.off as usize + n / 2];
                        let a = if n & 1 == 0 { byte >> 4 } else { byte & 15 };
                        if a != 0 {
                            self.blend(gx + dx, yy, v, u32::from(a) * 256 / 15);
                        }
                    }
                }
            }
            pen16 += i32::from(g.adv) + track * 16;
        }
        end
    }

    // ----------------------------------------------------------- shapes --

    /// A filled disc with a soft edge.
    pub fn disc(&mut self, cx: f32, cy: f32, r: f32, c: Rgb565) {
        self.ring_span(cx, cy, 0.0, r, c, None);
    }

    /// A ring of width `w` centred on radius `r`; `dash` = (on, off) in
    /// pixels along the circumference.
    pub fn ring(&mut self, cx: f32, cy: f32, r: f32, w: f32, c: Rgb565, dash: Option<(f32, f32)>) {
        self.ring_span(cx, cy, (r - w / 2.0).max(0.0), r + w / 2.0, c, dash.map(|d| (d, r)));
    }

    /// Everything between radii `r0` and `r1`, anti-aliased on both edges
    /// (`r0` = 0: a disc).
    fn ring_span(&mut self, cx: f32, cy: f32, r0: f32, r1: f32, c: Rgb565, dash: Option<((f32, f32), f32)>) {
        let v = raw(c);
        let ext = r1 + 1.0;
        self.check((cx - r1).rnd() as i32, (cy - r1).rnd() as i32, (cx + r1).rnd() as i32, (cy + r1).rnd() as i32);
        let (y0, y1) = self.clip_rows((cy - ext).flr() as i32, (cy + ext).cel() as i32 + 1);
        let outer2 = (r1 + 0.5) * (r1 + 0.5);
        let hole_r = r0 - 0.5;
        let solid_r = r1 - 0.5;
        for yy in y0..y1 {
            let dy = yy as f32 + 0.5 - cy;
            let dy2 = dy * dy;
            if dy2 >= outer2 {
                continue;
            }
            let half = libm::sqrtf(outer2 - dy2);
            let xa = ((cx - half).flr() as i32).max(0);
            let xb = ((cx + half).cel() as i32).min(W - 1);
            // Columns certainly inside the hole (ring) or certainly solid
            // (disc): skipped, or filled without any maths.
            let inner = |r: f32| if r > 0.0 && r * r > dy2 { libm::sqrtf(r * r - dy2) - 1.0 } else { -1.0 };
            let (skip, solid) = if r0 > 0.0 { (inner(hole_r), -1.0) } else { (-1.0, inner(solid_r)) };
            let mut xx = xa;
            while xx <= xb {
                let dx = xx as f32 + 0.5 - cx;
                if skip > 0.0 && dx > -skip && dx < skip {
                    xx = (cx + skip).flr() as i32 + 1;
                    continue;
                }
                if solid > 0.0 && dx > -solid && dx < solid && dash.is_none() {
                    let end = ((cx + solid).flr() as i32).min(xb);
                    if end > xx && yy >= self.top && yy < self.top + self.rows {
                        let i = self.at(xx, yy);
                        self.px[i..i + (end - xx) as usize].fill(v);
                        xx = end;
                        continue;
                    }
                }
                let d = libm::sqrtf(dx * dx + dy2);
                let mut cov = (r1 + 0.5 - d).clamp(0.0, 1.0);
                if r0 > 0.0 {
                    cov = cov.min((d - r0 + 0.5).clamp(0.0, 1.0));
                }
                if let Some(((on, off), r)) = dash
                    && cov > 0.0
                {
                    let s = (libm::atan2f(dx, -dy) + core::f32::consts::PI) * r;
                    if s % (on + off) >= on {
                        cov = 0.0;
                    }
                }
                self.blend(xx, yy, v, (cov * 256.0) as u32);
                xx += 1;
            }
        }
    }

    /// A line `w` pixels wide with round ends.
    pub fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, w: f32, c: Rgb565) {
        self.line_dashed(x0, y0, x1, y1, w, c, None)
    }

    /// As [`Canvas::line`], dashed (`on`, `off` pixels).
    #[allow(clippy::too_many_arguments)]
    pub fn line_dashed(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, w: f32, c: Rgb565, dash: Option<(f32, f32)>) {
        let v = raw(c);
        let hw = w / 2.0;
        let (dx, dy) = (x1 - x0, y1 - y0);
        let len2 = dx * dx + dy * dy;
        let len = libm::sqrtf(len2);
        let pad = hw + 1.0;
        let bx0 = (x0.min(x1) - pad).flr() as i32;
        let bx1 = (x0.max(x1) + pad).cel() as i32;
        let by0 = (y0.min(y1) - pad).flr() as i32;
        let by1 = (y0.max(y1) + pad).cel() as i32;
        self.check((x0.min(x1) - hw).rnd() as i32, (y0.min(y1) - hw).rnd() as i32, (x0.max(x1) + hw).rnd() as i32, (y0.max(y1) + hw).rnd() as i32);
        let (r0, r1) = self.clip_rows(by0, by1);
        for yy in r0..r1 {
            let py = yy as f32 + 0.5;
            // The columns this row can touch.
            let (mut xa, mut xb) = (bx0, bx1);
            if dy.abs() > 0.5 {
                let t = (py - y0) / dy;
                let xc = x0 + t * dx;
                let spread = pad * len / dy.abs();
                xa = xa.max((xc - spread).flr() as i32);
                xb = xb.min((xc + spread).cel() as i32);
            }
            for xx in xa.max(0)..=xb.min(W - 1) {
                let px = xx as f32 + 0.5;
                let t = if len2 > 0.0 { (((px - x0) * dx + (py - y0) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
                if let Some((on, off)) = dash
                    && (t * len) % (on + off) >= on
                {
                    continue;
                }
                let (qx, qy) = (x0 + t * dx - px, y0 + t * dy - py);
                let d = libm::sqrtf(qx * qx + qy * qy);
                let cov = (hw + 0.5 - d).clamp(0.0, 1.0);
                self.blend(xx, yy, v, (cov * 256.0) as u32);
            }
        }
    }

    /// A filled convex polygon (points in either winding), anti-aliased.
    pub fn poly(&mut self, pts: &[(f32, f32)], c: Rgb565) {
        let n = pts.len();
        if n < 3 {
            return;
        }
        let v = raw(c);
        // Orientation, so every edge normal points outwards.
        let mut area = 0.0;
        for i in 0..n {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            area += a.0 * b.1 - b.0 * a.1;
        }
        let sign = if area < 0.0 { -1.0 } else { 1.0 };
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for p in pts {
            x0 = x0.min(p.0);
            y0 = y0.min(p.1);
            x1 = x1.max(p.0);
            y1 = y1.max(p.1);
        }
        self.check(x0.rnd() as i32, y0.rnd() as i32, x1.rnd() as i32, y1.rnd() as i32);
        let (r0, r1) = self.clip_rows(y0.flr() as i32 - 1, y1.cel() as i32 + 1);
        let mut edges = [(0.0f32, 0.0f32, 0.0f32); 8];
        for (i, e) in edges.iter_mut().enumerate().take(n.min(8)) {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            let (ex, ey) = (b.0 - a.0, b.1 - a.1);
            let l = libm::sqrtf(ex * ex + ey * ey).max(1e-6);
            // Outward normal for counter-clockwise (screen) winding.
            let (nx, ny) = (sign * ey / l, -sign * ex / l);
            *e = (nx, ny, -(nx * a.0 + ny * a.1));
        }
        for yy in r0..r1 {
            for xx in (x0.flr() as i32 - 1).max(0)..=(x1.cel() as i32 + 1).min(W - 1) {
                let (px, py) = (xx as f32 + 0.5, yy as f32 + 0.5);
                let mut d = f32::MIN;
                for e in &edges[..n.min(8)] {
                    d = d.max(e.0 * px + e.1 * py + e.2);
                }
                let cov = (0.5 - d).clamp(0.0, 1.0);
                self.blend(xx, yy, v, (cov * 256.0) as u32);
            }
        }
    }

    /// A rounded rectangle, filled.
    pub fn rrect(&mut self, x: i32, y: i32, w: i32, h: i32, r: i32, c: Rgb565) {
        self.rrect_impl(x, y, w, h, r, c, None);
    }

    /// A rounded rectangle's outline, `t` pixels thick.
    #[allow(clippy::too_many_arguments)]
    pub fn rrect_stroke(&mut self, x: i32, y: i32, w: i32, h: i32, r: i32, t: f32, c: Rgb565) {
        self.rrect_impl(x, y, w, h, r, c, Some(t));
    }

    #[allow(clippy::too_many_arguments)]
    fn rrect_impl(&mut self, x: i32, y: i32, w: i32, h: i32, r: i32, c: Rgb565, stroke: Option<f32>) {
        if w <= 0 || h <= 0 {
            return;
        }
        let r = r.min(w / 2).min(h / 2).max(0);
        if r == 0 {
            match stroke {
                None => self.fill(x, y, w, h, c),
                Some(t) => self.frame(x, y, w, h, (t.rnd() as i32).max(1), c),
            }
            return;
        }
        self.check(x, y, x + w, y + h);
        let v = raw(c);
        let (r0, r1) = self.clip_rows(y, y + h);
        let rf = r as f32;
        for yy in r0..r1 {
            // Rows clear of the corners: spans, no maths.
            if yy >= y + r && yy < y + h - r {
                match stroke {
                    None => self.fill(x, yy, w, 1, c),
                    Some(t) => {
                        let t = (t.rnd() as i32).max(1);
                        self.fill(x, yy, t, 1, c);
                        self.fill(x + w - t, yy, t, 1, c);
                    }
                }
                continue;
            }
            let py = yy as f32 + 0.5;
            for xx in x.max(0)..(x + w).min(W) {
                let px = xx as f32 + 0.5;
                // Signed distance to the rounded box.
                let qx = (px - (x as f32 + w as f32 / 2.0)).abs() - (w as f32 / 2.0 - rf);
                let qy = (py - (y as f32 + h as f32 / 2.0)).abs() - (h as f32 / 2.0 - rf);
                let ox = qx.max(0.0);
                let oy = qy.max(0.0);
                let d = libm::sqrtf(ox * ox + oy * oy) + qx.max(qy).min(0.0) - rf;
                let cov = match stroke {
                    None => (0.5 - d).clamp(0.0, 1.0),
                    Some(t) => (0.5 - d).min(d + t + 0.5).clamp(0.0, 1.0),
                };
                self.blend(xx, yy, v, (cov * 256.0) as u32);
            }
        }
    }
}
