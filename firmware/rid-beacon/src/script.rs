//! Scripted flights (bench builds only, feature `scripted`): the threat
//! level bench paths of docs/THREAT-PROFILES.md, flown as Remote ID so a
//! real hub hears them over the air.
//!
//! Nothing here is a real aircraft. Every path is a plain function of the
//! seconds since its console key was pressed, around the same build-time
//! centre as the circle (`flight::centre`), and only changes what the
//! Location and System messages say: position, speed, direction, height,
//! climb rate and the pilot position. The identity, the transports, their
//! rates and the transmit power are exactly those of the circle.
//!
//! The shapes match the host flights of `crates/tracker` (`behaviour::
//! scenario`, the false-alarm matrix in `behaviour::tests_profiles`), and
//! `behaviour::tests_bench` flies this very file through the tracker on the
//! host (it is compiled there with `#[path]`), so the bench and the host
//! tests expect the same levels.
//!
//! The scripted paths broadcast **no pilot position** (latitude and
//! longitude zero, which F3411 defines as unknown), as the matrix's hostile
//! flights do: with a pilot inside or outside the area, "in the area" plus
//! "pilot in" or "pilot outside" is already a Warning on its own, which
//! would hide the rule each path is there to show. The circle keeps its
//! pilot at the centre, exactly as before.
//!
//! Pure `core` + `libm`: no hardware, no allocation, so the host can run it.

use core::sync::atomic::{AtomicU8, AtomicU32, Ordering::Relaxed};

/// Mean Earth radius (m), as `flight.rs`: a flat map round the centre,
/// exact enough for paths of a few kilometres.
const EARTH_R: f64 = 6_371_000.0;

/// A scripted path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Path {
    /// The circle of `flight.rs` (the default, and key `0`).
    Circle = 0,
    /// (a) Straight at the centre from 800 m south at 12 m/s, 50 m up
    /// coming down to 30 m; then hovering over the centre at 30 m.
    Beeline = 1,
    /// (b) Fast inbound: from 1500 m east at 30 m/s, 100 m up coming down
    /// to 20 m; then hovering over the centre at 20 m.
    Fast = 2,
    /// (c) A straight pass 300 m north of the centre, west to east at
    /// 15 m/s, 100 m up, from 1000 m west to 1000 m east; then hovering
    /// there. It never comes nearer than 300 m.
    Pass = 3,
    /// (d) Low hover: over the centre, down from 40 m at 2 m/s to 10 m,
    /// then still at 10 m.
    Hover = 4,
    /// (e) A payload drop 30 m east and 20 m north of the centre: 50 m up,
    /// from second 40 down 30 m at 3 m/s, 5 s at the bottom, back up at
    /// 3 m/s, then hovering at 50 m again.
    Drop = 5,
}

pub const PATHS: [Path; 6] = [Path::Circle, Path::Beeline, Path::Fast, Path::Pass, Path::Hover, Path::Drop];

impl Path {
    /// Its console key.
    pub const fn key(self) -> u8 {
        b'0' + self as u8
    }

    /// Its name in the console lines.
    pub const fn name(self) -> &'static str {
        match self {
            Path::Circle => "circle",
            Path::Beeline => "beeline",
            Path::Fast => "fast_inbound",
            Path::Pass => "straight_pass",
            Path::Hover => "low_hover",
            Path::Drop => "drop",
        }
    }

    pub fn from_key(b: u8) -> Option<Path> {
        PATHS.iter().copied().find(|p| p.key() == b)
    }
}

/// Where a scripted path is at one instant.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub lat: f64,
    pub lon: f64,
    /// Height above the ground, metres (the Location "height", type AGL;
    /// the geodetic height is the beacon's ground plus this).
    pub height_m: f32,
    /// Ground speed, m/s.
    pub speed_mps: f32,
    /// Track over ground, degrees true, 0..360 (0 when hovering).
    pub heading_deg: f32,
    /// Climb rate, m/s (negative: descending).
    pub vspeed_mps: f32,
}

/// The point `east_m`, `north_m` metres from (`lat`, `lon`).
pub fn offset(lat: f64, lon: f64, east_m: f64, north_m: f64) -> (f64, f64) {
    let dlat = (north_m / EARTH_R).to_degrees();
    let dlon = (east_m / (EARTH_R * libm::cos(lat.to_radians()))).to_degrees();
    (lat + dlat, lon + dlon)
}

fn norm(d: f64) -> f64 {
    let r = d % 360.0;
    if r < 0.0 { r + 360.0 } else { r }
}

/// Straight from (`e0`, `n0`) to (`e1`, `n1`) (metres from the centre) at
/// `v` m/s, height changing evenly from `h0` to `h1`; then hovering at the
/// end at `h1`.
#[allow(clippy::too_many_arguments)]
fn toward(lat: f64, lon: f64, t: f64, (e0, n0): (f64, f64), (e1, n1): (f64, f64), v: f64, h0: f32, h1: f32) -> Sample {
    let (de, dn) = (e1 - e0, n1 - n0);
    let dist = libm::sqrt(de * de + dn * dn);
    let total = if v > 0.0 { dist / v } else { 0.0 };
    let course = norm(libm::atan2(de, dn).to_degrees()) as f32;
    if t >= total || dist < 1e-6 {
        let (la, lo) = offset(lat, lon, e1, n1);
        return Sample { lat: la, lon: lo, height_m: h1, speed_mps: 0.0, heading_deg: 0.0, vspeed_mps: 0.0 };
    }
    let k = t / total;
    let (la, lo) = offset(lat, lon, e0 + de * k, n0 + dn * k);
    Sample {
        lat: la,
        lon: lo,
        height_m: h0 + ((h1 - h0) as f64 * k) as f32,
        speed_mps: v as f32,
        heading_deg: course,
        vspeed_mps: ((h1 - h0) as f64 / total) as f32,
    }
}

/// Where `path` is `t_s` seconds after its key, round the centre (`lat`,
/// `lon`). `None` for [`Path::Circle`], which is `flight::at`.
pub fn at(path: Path, lat: f64, lon: f64, t_s: f64) -> Option<Sample> {
    let t = t_s.max(0.0);
    Some(match path {
        Path::Circle => return None,
        Path::Beeline => toward(lat, lon, t, (0.0, -800.0), (0.0, 0.0), 12.0, 50.0, 30.0),
        Path::Fast => toward(lat, lon, t, (1500.0, 0.0), (0.0, 0.0), 30.0, 100.0, 20.0),
        Path::Pass => toward(lat, lon, t, (-1000.0, 300.0), (1000.0, 300.0), 15.0, 100.0, 100.0),
        Path::Hover => {
            let (h, vs) = if t < 15.0 { (40.0 - 2.0 * t as f32, -2.0) } else { (10.0, 0.0) };
            Sample { lat, lon, height_m: h, speed_mps: 0.0, heading_deg: 0.0, vspeed_mps: vs }
        }
        Path::Drop => {
            const START: f64 = 40.0;
            const DEPTH: f64 = 30.0;
            const RATE: f64 = 3.0;
            const PAUSE: f64 = 5.0;
            let move_s = DEPTH / RATE;
            let u = t - START;
            let (down, vs) = if u < 0.0 {
                (0.0, 0.0)
            } else if u < move_s {
                (u * RATE, -RATE)
            } else if u < move_s + PAUSE {
                (DEPTH, 0.0)
            } else if u < 2.0 * move_s + PAUSE {
                (DEPTH - (u - move_s - PAUSE) * RATE, RATE)
            } else {
                (0.0, 0.0)
            };
            let (la, lo) = offset(lat, lon, 30.0, 20.0);
            Sample { lat: la, lon: lo, height_m: (50.0 - down) as f32, speed_mps: 0.0, heading_deg: 0.0, vspeed_mps: vs as f32 }
        }
    })
}

// ---- the path in use (set from the console) ---------------------------------

/// The path in use (`Path as u8`). The circle at boot, always.
static PATH: AtomicU8 = AtomicU8::new(Path::Circle as u8);
/// Uptime (ms) when its key was pressed.
static START_MS: AtomicU32 = AtomicU32::new(0);

/// A console byte: a path key starts that path from its beginning (again,
/// if it is the one in use). `None`: not a path key.
pub fn command(byte: u8, now_ms: u32) -> Option<Path> {
    let p = Path::from_key(byte)?;
    START_MS.store(now_ms, Relaxed);
    PATH.store(p as u8, Relaxed);
    Some(p)
}

/// The path in use and the seconds since its key.
pub fn current(now_ms: u32) -> (Path, f64) {
    let p = PATHS.get(usize::from(PATH.load(Relaxed))).copied().unwrap_or(Path::Circle);
    (p, f64::from(now_ms.wrapping_sub(START_MS.load(Relaxed))) / 1000.0)
}
