//! The lingering rules (slice 16b; rules 7-10 of docs/THREAT-PROFILES.md):
//! what a drone has been doing over the last minutes, worked out once a
//! second from its path history ([`super::path`]). [`super::evaluate`] then
//! checks the result against each place.
//!
//! 7. **Loitering**: staying within [`LOITER_RADIUS_M`] of one spot for
//!    minutes.
//! 8. **Circling**: turning through a full circle round one point.
//! 9. **Low hover**: under [`LOW_HOVER_M`] and nearly still for
//!    [`LOW_HOVER_MS`].
//! 10. **Drop pattern**: down more than [`DROP_DESCENT_M`], a pause, then
//!     up again, all within [`DROP_WINDOW_MS`].
//!
//! Remote ID positions are rough (30 m at 95 %) and heights wobble by a
//! few metres, so every check here works on averages and ignores small
//! moves. The details are with each number below.

use libm::{atan2f, fabsf, sqrtf};

use super::path::{PathPoint, PathRing};
use super::places::Frame;

// ------------------------------------------------------------- numbers --

/// Loitering: every position within this distance of where the drone is
/// now. A Remote ID position is good to about 30 m, so a drone holding
/// still shows up as a cloud about that wide; 50 m leaves room for that
/// and still means "one spot" (a house and its garden).
pub const LOITER_RADIUS_M: f32 = 50.0;
/// Loitering this long gives Caution. A drone passing over, or a pilot
/// lining up one photo, is gone within a minute or two.
pub const LOITER_CAUTION_MS: u64 = 180_000;
/// Loitering this long, and lower than [`LOITER_LOW_M`], gives Warning (the
/// design's example: "hovering low over the yard for 5 min").
pub const LOITER_WARNING_MS: u64 = 300_000;
/// "Low" for loitering: under 40 m above the unit, about the height from
/// which a camera picks out faces and windows. Lower still (under
/// [`LOW_HOVER_M`]) is rule 9, Warning after only 10 s.
pub const LOITER_LOW_M: f32 = 40.0;
/// A drone loitering this close outside an area counts too (watching from
/// just beyond the fence).
pub const LOITER_NEAR_M: f32 = 50.0;
/// A hole in the history longer than this ends a stay (the drone was not
/// heard: it may have gone and come back).
pub const DWELL_GAP_MS: u64 = 30_000;

/// Circling: the circle's radius must be under this (wider is not "round
/// one place" any more).
pub const CIRCLE_MAX_RADIUS_M: f32 = 150.0;
/// The path is followed in steps of at least this much ground; shorter
/// moves are ignored, so the wobble of a hovering drone's reported position
/// makes no turns.
pub const CIRCLE_STEP_M: f32 = 15.0;
/// A step turning more than this from the one before is not part of a
/// smooth circle (jumps of the reported position turn every which way).
pub const CIRCLE_MAX_TURN_DEG: f32 = 100.0;
/// At least this many turns make the full circle (so the smallest circle
/// counted is about 20 m in radius: tighter than that is within
/// the error of the positions, and is hovering).
pub const CIRCLE_MIN_TURNS: usize = 8;
/// All the turning (left and right added up) may be at most this many
/// times the net turn: a real circle turns one way.
pub const CIRCLE_MAX_WIGGLE: f32 = 1.5;
/// Slower than this along the path is not circling (hovering).
pub const CIRCLE_MIN_MPS: f32 = 1.0;
/// The full circle must lie within the last five minutes.
pub const CIRCLE_WINDOW_MS: u64 = 300_000;

/// Low hover: under this height above the unit (window height; the design
/// and the research, THREAT-TACTICS.md §1.2).
pub const LOW_HOVER_M: f32 = 15.0;
/// ... and nearly still for this long.
pub const LOW_HOVER_MS: u64 = 10_000;
/// "Nearly still": every position within this distance of where it is now.
pub const HOVER_RADIUS_M: f32 = 15.0;
/// In the bubble, a drone hovering low this close to its own pilot is
/// taking off or landing, not watching you (see [`super::evaluate`]).
pub const TAKEOFF_PILOT_M: f32 = 30.0;

/// Drop pattern: all of it within this long.
pub const DROP_WINDOW_MS: u64 = 60_000;
/// Down at least this far ...
pub const DROP_DESCENT_M: f32 = 10.0;
/// ... a pause near the bottom (within [`DROP_PAUSE_BAND_M`]) of at least
/// [`DROP_PAUSE_MS`] ...
pub const DROP_PAUSE_MS: u64 = 3_000;
pub const DROP_PAUSE_BAND_M: f32 = 3.0;
/// ... then up at least this far (so a drone that lands and stays down is
/// not a drop).
pub const DROP_CLIMB_M: f32 = 8.0;
/// Heights are averaged over this many reports (one a second) first, so
/// the few metres a GPS height wobbles make no dip.
pub const DROP_SMOOTH: usize = 5;
/// At least this many heights in the window.
const DROP_MIN_POINTS: usize = 15;

// --------------------------------------------------------------- result --

/// What the path history shows about one drone right now.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Linger {
    /// How long it has stayed within [`LOITER_RADIUS_M`] of where it is now.
    pub stay_ms: u64,
    /// How long it has been under [`LOW_HOVER_M`] and within
    /// [`HOVER_RADIUS_M`] of where it is now; 0 when it is not low now or
    /// its height is not known.
    pub low_ms: u64,
    /// The circle it has just flown, if any.
    pub orbit: Option<Orbit>,
    /// The drop it has just made, if any.
    pub dip: Option<Dip>,
}

/// A full circle (rule 8).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Orbit {
    /// Its middle.
    pub lat: f64,
    pub lon: f64,
    pub radius_m: f32,
    /// How long the last full turn took.
    pub turn_ms: u64,
}

/// A drop pattern (rule 10).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Dip {
    /// Where the bottom was (the average of the positions there).
    pub lat: f64,
    pub lon: f64,
    /// How far it came down.
    pub depth_m: f32,
    /// The height of the bottom above the ellipsoid (as the path stores it).
    pub bottom_alt_m: f32,
    /// How long ago it left the bottom.
    pub ago_ms: u64,
}

impl Linger {
    /// Work it out. `at` is where the drone is now (the average of its last
    /// few positions), `height_now` its height above the unit now, `ground`
    /// the height above the ellipsoid that counts as the ground (so a path
    /// point's height is its altitude minus `ground`), when known.
    #[inline(never)]
    pub fn of(ring: &PathRing, at: (f64, f64), height_now: Option<f32>, ground: Option<f32>, now_ms: u64) -> Self {
        let Some(newest) = ring.last_ms() else { return Self::default() };
        let frame = Frame::at(at.0, at.1);
        let stay_ms = stay(ring.coarse().rev(), &frame, LOITER_RADIUS_M, newest, |_| true);
        let low = |p: &PathPoint| match (ground, p.alt_m()) {
            (Some(g), Some(a)) => a - g < LOW_HOVER_M,
            _ => height_now.is_some_and(|h| h < LOW_HOVER_M),
        };
        let low_ms = if height_now.is_some_and(|h| h < LOW_HOVER_M) {
            stay(ring.fine().rev(), &frame, HOVER_RADIUS_M, newest, low)
        } else {
            0
        };
        Self { stay_ms, low_ms, orbit: orbit(ring, &frame, now_ms), dip: dip(ring, &frame) }
    }
}

/// How long the drone has stayed within `radius_m` of the frame's middle
/// (where it is now), walking back through `pts` (newest first) while each
/// point also `fits`. One point out is forgiven (a single wild position);
/// two in a row end the stay, as does a hole over [`DWELL_GAP_MS`].
fn stay<'a>(
    pts: impl Iterator<Item = (u64, &'a PathPoint)>,
    frame: &Frame,
    radius_m: f32,
    newest_ms: u64,
    mut fits: impl FnMut(&PathPoint) -> bool,
) -> u64 {
    let mut earliest = None;
    let mut misses = 0;
    let mut prev = newest_ms;
    for (t, p) in pts {
        if prev.saturating_sub(t) > DWELL_GAP_MS {
            break;
        }
        prev = t;
        let (x, y) = frame.xy(p.lat(), p.lon());
        if x * x + y * y <= radius_m * radius_m && fits(p) {
            earliest = Some(t);
            misses = 0;
        } else {
            misses += 1;
            if misses >= 2 {
                break;
            }
        }
    }
    earliest.map_or(0, |e| newest_ms.saturating_sub(e))
}

// ------------------------------------------------------------ circling --

/// The last full circle, looked for first in the last two minutes (one
/// position a second, each averaged with its four neighbours), then in the
/// last five (one every five seconds, averaged in threes) for slow wide
/// circles.
fn orbit(ring: &PathRing, frame: &Frame, now_ms: u64) -> Option<Orbit> {
    let found = walk::<5>(ring.fine().rev(), frame, now_ms).or_else(|| walk::<3>(ring.coarse().rev(), frame, now_ms))?;
    let (lat, lon) = frame.latlon(found.0, found.1);
    (found.2 <= CIRCLE_MAX_RADIUS_M).then_some(Orbit { lat, lon, radius_m: found.2, turn_ms: found.3 })
}

/// Walk the path back from the newest point, averaged over `N` points, in
/// steps of at least [`CIRCLE_STEP_M`], adding up the turns between steps
/// until they make a full circle. Gives the circle's middle (east, north in
/// `frame`), radius and how long it took, or `None` when the path does not
/// turn a smooth full circle: a step turns too sharply, the drone was too
/// slow or not heard, the turning goes both ways, or the path does not come
/// back near where the circle began (a drone that circled and then flew
/// off stops counting once it is about one radius away).
fn walk<'a, const N: usize>(
    pts: impl Iterator<Item = (u64, &'a PathPoint)>,
    frame: &Frame,
    now_ms: u64,
) -> Option<(f32, f32, f32, u64)> {
    let mut buf = [(0u64, 0f32, 0f32); N];
    let mut n_raw = 0usize;
    let mut prev_t: Option<u64> = None;
    // The last point kept, the direction of the last step, and the sums.
    let mut kept: Option<(u64, f32, f32)> = None;
    let mut dir: Option<(f32, f32)> = None;
    let (mut net, mut all, mut turns) = (0f32, 0f32, 0usize);
    let (mut sx, mut sy, mut sr, mut n) = (0f32, 0f32, 0f32, 0f32);
    let (mut start, mut len) = (0u64, 0f32);
    let mut first = (0f32, 0f32);
    for (t, p) in pts {
        if now_ms.saturating_sub(t) > CIRCLE_WINDOW_MS || prev_t.is_some_and(|pt| pt.saturating_sub(t) > DWELL_GAP_MS) {
            return None;
        }
        prev_t = Some(t);
        let (x, y) = frame.xy(p.lat(), p.lon());
        buf[n_raw % N] = (t, x, y);
        n_raw += 1;
        if n_raw < N {
            continue;
        }
        // The average of the last N points.
        let (mut at, mut ax, mut ay) = (0u64, 0f32, 0f32);
        for &(bt, bx, by) in &buf {
            at += bt;
            ax += bx;
            ay += by;
        }
        let (t, x, y) = (at / N as u64, ax / N as f32, ay / N as f32);
        let Some(k) = kept else {
            kept = Some((t, x, y));
            start = t;
            first = (x, y);
            (sx, sy, sr, n) = (x, y, x * x + y * y, 1.0);
            continue;
        };
        let (dx, dy) = (x - k.1, y - k.2);
        let d = sqrtf(dx * dx + dy * dy);
        if d < CIRCLE_STEP_M {
            // Too slow: not even one step in the time it takes at
            // CIRCLE_MIN_MPS.
            if k.0.saturating_sub(t) as f32 > CIRCLE_STEP_M / CIRCLE_MIN_MPS * 1000.0 {
                return None;
            }
            continue;
        }
        let u = (dx / d, dy / d);
        if let Some(v) = dir {
            let turn = atan2f(v.0 * u.1 - v.1 * u.0, v.0 * u.0 + v.1 * u.1).to_degrees();
            if fabsf(turn) > CIRCLE_MAX_TURN_DEG {
                return None;
            }
            net += turn;
            all += fabsf(turn);
            turns += 1;
        }
        dir = Some(u);
        kept = Some((t, x, y));
        len += d;
        sx += x;
        sy += y;
        sr += x * x + y * y;
        n += 1.0;
        if fabsf(net) >= 360.0 {
            let ms = start.saturating_sub(t);
            if turns < CIRCLE_MIN_TURNS || all > CIRCLE_MAX_WIGGLE * fabsf(net) || len / (ms as f32 / 1000.0).max(1.0) < CIRCLE_MIN_MPS {
                return None;
            }
            let (cx, cy) = (sx / n, sy / n);
            let r = sqrtf((sr / n - (cx * cx + cy * cy)).max(0.0));
            let (gx, gy) = (x - first.0, y - first.1);
            if sqrtf(gx * gx + gy * gy) > r.max(2.0 * CIRCLE_STEP_M) {
                return None;
            }
            return Some((cx, cy, r, ms));
        }
    }
    None
}

// ---------------------------------------------------------------- drop --

/// A drop pattern in the last [`DROP_WINDOW_MS`] of heights (the drone's
/// reported altitude above the ellipsoid, averaged over [`DROP_SMOOTH`]
/// reports): the lowest point, at least [`DROP_DESCENT_M`] below an
/// earlier height and [`DROP_CLIMB_M`] below a later one, with the drone
/// staying near it for [`DROP_PAUSE_MS`]. A drone that sends no altitude
/// above the ellipsoid makes no drop.
fn dip(ring: &PathRing, frame: &Frame) -> Option<Dip> {
    const CAP: usize = 64;
    let newest = ring.last_ms()?;
    // Age (ms before the newest) and height, newest first.
    let mut a = [(0u32, 0f32); CAP];
    let mut n = 0;
    for (t, p) in ring.fine().rev() {
        if newest.saturating_sub(t) > DROP_WINDOW_MS || n == CAP {
            break;
        }
        if let Some(h) = p.alt_m() {
            a[n] = (newest.saturating_sub(t) as u32, h);
            n += 1;
        }
    }
    if n < DROP_MIN_POINTS {
        return None;
    }
    let a = &mut a[..n];
    a.reverse();
    let half = DROP_SMOOTH / 2;
    let sm = |k: usize| {
        let (lo, hi) = (k.saturating_sub(half), (k + half).min(n - 1));
        a[lo..=hi].iter().map(|p| p.1).sum::<f32>() / (hi - lo + 1) as f32
    };
    let mut m = 0;
    let mut bottom = f32::MAX;
    for k in 0..n {
        let h = sm(k);
        if h < bottom {
            bottom = h;
            m = k;
        }
    }
    if m == 0 || m == n - 1 {
        return None;
    }
    let before = (0..m).map(sm).fold(f32::MIN, f32::max);
    let after = (m + 1..n).map(sm).fold(f32::MIN, f32::max);
    if before - bottom < DROP_DESCENT_M || after - bottom < DROP_CLIMB_M {
        return None;
    }
    // The pause: the stretch round the bottom that stays near it.
    let (mut lo, mut hi) = (m, m);
    while lo > 0 && sm(lo - 1) <= bottom + DROP_PAUSE_BAND_M {
        lo -= 1;
    }
    while hi + 1 < n && sm(hi + 1) <= bottom + DROP_PAUSE_BAND_M {
        hi += 1;
    }
    let (first, last) = (a[lo].0, a[hi].0);
    if u64::from(first - last) < DROP_PAUSE_MS {
        return None;
    }
    // Where it was during the pause.
    let (mut x, mut y, mut k) = (0f32, 0f32, 0f32);
    for (t, p) in ring.fine().rev() {
        let age = newest.saturating_sub(t);
        if age > u64::from(first) {
            break;
        }
        if age >= u64::from(last) {
            let q = frame.xy(p.lat(), p.lon());
            x += q.0;
            y += q.1;
            k += 1.0;
        }
    }
    let (lat, lon) = frame.latlon(x / k.max(1.0), y / k.max(1.0));
    Some(Dip { lat, lon, depth_m: before - bottom, bottom_alt_m: bottom, ago_ms: u64::from(last) })
}
