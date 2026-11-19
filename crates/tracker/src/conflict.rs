//! A drone close to a crewed aircraft.
//!
//! The tracker holds drones (Remote ID, DroneID...) and crewed aircraft
//! (ADS-B, UAT, FLARM...) side by side; this module compares every pair.
//! The idea comes from SoftRF's `TrafficHelper.cpp` (GPLv3, Linar Yusupov):
//! subtract the two velocity vectors and ask how soon the pair would meet.
//! We use the closest point of approach (CPA) over the next minute instead of
//! SoftRF's "heading within 10 degrees" test, because a hovering drone has no
//! heading and a helicopter rarely flies straight at it.
//!
//! **Heights.** Drones report geodetic (GPS, WGS-84) and/or pressure altitude;
//! aircraft report pressure (baro) altitude and often geometric (GPS) too.
//! Like is compared with like. When the aircraft reports both, its own
//! difference converts the drone's pressure altitude to GPS height ("Same").
//! Comparing a GPS height with a pressure height ("Mixed") widens the
//! vertical margin by [`MIXED_EXTRA_M`]. With no height on one side
//! ("Unknown") the pair is judged on distance alone and never reaches
//! [`Level::Warning`].
//!
//! **Wording.** These are warnings to look up, not findings of fault: the
//! drone pilot must give way (14 CFR 107.37), but the hub cannot see what
//! either pilot sees.

use core::fmt::Write as _;

use adsb::Kind;
use heapless::{String, Vec};
use libm::{cos, fabs, roundf, sin, sqrt};

use crate::aircraft::{AircraftKey, AircraftTrack};
use crate::geo;
use crate::{Context, Track, TrackId, TrackState};

/// Vertical separation below which a pair counts as "at the same height"
/// (about 500 ft).
pub const VERT_MARGIN_M: f64 = 150.0;
/// Added to the margin when comparing a GPS height with a pressure height
/// (the two differ with the weather by up to a few hundred feet).
pub const MIXED_EXTRA_M: f64 = 90.0;
/// Closer than this now (and at the same height): warning.
pub const WARN_RANGE_M: f64 = 300.0;
/// Closer than this now: caution.
pub const CAUTION_RANGE_M: f64 = 1000.0;
/// Closer than this now, within twice the vertical margin: advisory.
pub const ADVISORY_RANGE_M: f64 = 3000.0;
/// Predicted to pass within [`WARN_RANGE_M`] within this many seconds: warning.
pub const WARN_CPA_S: f64 = 20.0;
/// Predicted to pass within [`CAUTION_CPA_M`] within this many seconds: caution.
pub const CAUTION_CPA_S: f64 = 45.0;
pub const CAUTION_CPA_M: f64 = 600.0;
/// How far ahead the prediction looks.
pub const LOOKAHEAD_S: f64 = 60.0;
/// A position older than this is extrapolated no further (it is stale data,
/// not a prediction).
pub const MAX_EXTRAPOLATE_S: f64 = 10.0;
/// The same pair alerts again after this long, even at the same level.
pub const REALERT_MS: u64 = 5 * 60 * 1000;
/// Pairs reported at once.
pub const MAX_CONFLICTS: usize = 16;

/// How serious a pair is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Crewed aircraft in the area, roughly at drone heights.
    Advisory = 1,
    /// Close, or predicted to pass close within [`CAUTION_CPA_S`].
    Caution = 2,
    /// Very close, or predicted to pass within [`WARN_RANGE_M`] in
    /// [`WARN_CPA_S`].
    Warning = 3,
}

impl Level {
    pub const fn name(self) -> &'static str {
        match self {
            Level::Advisory => "advisory",
            Level::Caution => "caution",
            Level::Warning => "warning",
        }
    }
}

/// What the height comparison was based on (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AltBasis {
    Same,
    Mixed,
    Unknown,
}

impl AltBasis {
    pub const fn name(self) -> &'static str {
        match self {
            AltBasis::Same => "same",
            AltBasis::Mixed => "mixed",
            AltBasis::Unknown => "unknown",
        }
    }
}

/// One drone / aircraft pair worth a look.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Conflict {
    pub drone: TrackId,
    pub aircraft: AircraftKey,
    pub level: Level,
    /// Horizontal distance now (extrapolated), metres.
    pub range_m: f32,
    /// Aircraft height minus drone height now, metres (positive: aircraft
    /// above). `None` when either height is unknown.
    pub dz_m: Option<f32>,
    /// Seconds until the closest point of approach, when the pair is
    /// closing within [`LOOKAHEAD_S`].
    pub cpa_s: Option<f32>,
    /// Horizontal distance at that point, metres.
    pub cpa_m: Option<f32>,
    pub basis: AltBasis,
}

/// One side of a pair, reduced to what the geometry needs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Body {
    pub lat: f64,
    pub lon: f64,
    /// GPS (WGS-84) height, metres.
    pub alt_geo: Option<f64>,
    /// Pressure altitude, metres.
    pub alt_baro: Option<f64>,
    /// Velocity east, north, up, m/s (`None` when unknown).
    pub vel: Option<(f64, f64)>,
    pub vz: Option<f64>,
    /// Seconds since this position was received.
    pub age_s: f64,
}

const FT: f64 = 0.3048;
const KT: f64 = 0.514_444;

fn age_s(rx_ms: u64, now_ms: u64) -> f64 {
    now_ms.saturating_sub(rx_ms) as f64 / 1000.0
}

/// East and north parts of a speed along a direction (degrees true).
pub fn east_north(speed: f64, dir_deg: f64) -> (f64, f64) {
    let r = dir_deg.to_radians();
    (speed * sin(r), speed * cos(r))
}

/// A drone track as a [`Body`], when it has sent a position.
pub fn drone_body(t: &Track, now_ms: u64) -> Option<Body> {
    let loc = t.location.as_ref()?;
    let p = loc.value.position?;
    let vel = loc
        .value
        .speed_horizontal
        .map(|s| east_north(f64::from(s), loc.value.direction.map_or(0.0, f64::from)));
    Some(Body {
        lat: p.lat,
        lon: p.lon,
        alt_geo: loc.value.altitude_geodetic.map(f64::from),
        alt_baro: loc.value.altitude_pressure.map(f64::from),
        vel,
        vz: loc.value.speed_vertical.map(f64::from),
        age_s: age_s(loc.rx_ms, now_ms),
    })
}

/// A crewed, airborne aircraft as a [`Body`], when it has a position.
/// Aircraft that are themselves drones (emitter category UAV) and ground
/// vehicles are left out.
pub fn aircraft_body(a: &AircraftTrack, now_ms: u64) -> Option<Body> {
    if !a.airborne || matches!(a.kind(), Kind::Uav | Kind::Space) || is_surface(a.kind()) {
        return None;
    }
    let p = a.position.as_ref()?;
    let vel = match (a.speed_kt, a.direction_deg) {
        (Some(s), Some(d)) => Some(east_north(f64::from(s) * KT, f64::from(d))),
        _ => None,
    };
    Some(Body {
        lat: p.value.lat,
        lon: p.value.lon,
        alt_geo: a.gnss_alt_ft.map(|f| f64::from(f) * FT),
        alt_baro: a.baro_alt_ft.map(|f| f64::from(f) * FT),
        vel,
        vz: a.vrate_fpm.map(|f| f64::from(f) * FT / 60.0),
        age_s: age_s(p.rx_ms, now_ms),
    })
}

fn is_surface(k: Kind) -> bool {
    matches!(k, Kind::SurfaceVehicle | Kind::Obstacle)
}

/// Short label for an aircraft's source.
pub fn source_name(s: adsb::Source) -> &'static str {
    match s {
        adsb::Source::ModeS => "ADS-B",
        adsb::Source::Uat => "UAT",
        adsb::Source::Ogn => "OGN",
        adsb::Source::Flarm => "FLARM",
    }
}

/// Aircraft height minus drone height, and what it was based on.
pub fn height_difference(d: &Body, a: &Body) -> (Option<f64>, AltBasis) {
    if let (Some(x), Some(y)) = (a.alt_geo, d.alt_geo) {
        return (Some(x - y), AltBasis::Same);
    }
    if let (Some(x), Some(y)) = (a.alt_baro, d.alt_baro) {
        return (Some(x - y), AltBasis::Same);
    }
    // The aircraft knows the local pressure-to-GPS offset: use it to put the
    // drone's pressure altitude on the GPS scale.
    if let (Some(ag), Some(ab), Some(db)) = (a.alt_geo, a.alt_baro, d.alt_baro) {
        return (Some(ag - (db + (ag - ab))), AltBasis::Same);
    }
    let ah = a.alt_geo.or(a.alt_baro);
    let dh = d.alt_geo.or(d.alt_baro);
    match (ah, dh) {
        (Some(x), Some(y)) => (Some(x - y), AltBasis::Mixed),
        _ => (None, AltBasis::Unknown),
    }
}

/// Where `b` is relative to `a` now, metres east and north: the distance
/// between their reported positions, each moved on along its own velocity
/// for the time since it was received (at most [`MAX_EXTRAPOLATE_S`]).
/// Shared with the threat-level rules ([`crate::behaviour`]).
pub fn relative_now(a: &Body, b: &Body) -> (f64, f64) {
    let rb = geo::range_bearing(a.lat, a.lon, b.lat, b.lon);
    let (mut px, mut py) = east_north(rb.distance_m, rb.bearing_deg);
    let step = |x: &Body| {
        let t = x.age_s.min(MAX_EXTRAPOLATE_S);
        x.vel.map_or((0.0, 0.0), |(e, n)| (e * t, n * t))
    };
    let (be, bn) = step(b);
    let (ae, an) = step(a);
    px += be - ae;
    py += bn - an;
    (px, py)
}

/// The closest point of approach of two things moving in straight lines:
/// `rel_pos` is where the second is from the first now (metres east,
/// north), `rel_vel` how fast that changes (m/s). `Some((seconds, metres))`
/// when they are getting closer and will be closest within `lookahead_s`;
/// `None` when they are not closing (or hardly moving relative to each
/// other). Shared with the threat-level rules ([`crate::behaviour`]).
pub fn closest_approach(rel_pos: (f64, f64), rel_vel: (f64, f64), lookahead_s: f64) -> Option<(f64, f64)> {
    let ((px, py), (vx, vy)) = (rel_pos, rel_vel);
    let v2 = vx * vx + vy * vy;
    if v2 <= 0.01 {
        return None;
    }
    let t = -(px * vx + py * vy) / v2;
    (t > 0.0 && t <= lookahead_s).then(|| {
        let (cx, cy) = (px + vx * t, py + vy * t);
        (t, sqrt(cx * cx + cy * cy))
    })
}

/// What [`assess`] found: level, horizontal range now (m), height
/// difference now (m), closest approach (seconds, metres), height basis.
pub type Assessment = (Level, f64, Option<f64>, Option<(f64, f64)>, AltBasis);

/// Judge one pair. `None`: nothing worth reporting.
pub fn assess(d: &Body, a: &Body) -> Option<Assessment> {
    // Relative position (aircraft from drone), each extrapolated to now.
    let (px, py) = relative_now(d, a);
    let range = sqrt(px * px + py * py);

    let (dz, basis) = height_difference(d, a);
    let margin = VERT_MARGIN_M
        + if basis == AltBasis::Mixed {
            MIXED_EXTRA_M
        } else {
            0.0
        };
    let within = |z: Option<f64>, m: f64| z.is_none_or(|z| fabs(z) < m);

    // Closest point of approach. An unknown velocity counts as standing
    // still: a hovering drone often sends no speed.
    let (ve, vn) = a.vel.unwrap_or((0.0, 0.0));
    let (we, wn) = d.vel.unwrap_or((0.0, 0.0));
    let cpa = closest_approach((px, py), (ve - we, vn - wn), LOOKAHEAD_S);
    let dz_at = |t: f64| dz.map(|z| z + (a.vz.unwrap_or(0.0) - d.vz.unwrap_or(0.0)) * t);

    let mut level = None;
    let mut raise = |l: Level| {
        if level.is_none_or(|c| l > c) {
            level = Some(l);
        }
    };
    if range < WARN_RANGE_M && within(dz, margin) {
        raise(Level::Warning);
    }
    if let Some((t, m)) = cpa {
        if t <= WARN_CPA_S && m < WARN_RANGE_M && within(dz_at(t), margin) {
            raise(Level::Warning);
        } else if t <= CAUTION_CPA_S && m < CAUTION_CPA_M && within(dz_at(t), margin) {
            raise(Level::Caution);
        }
    }
    if range < CAUTION_RANGE_M && within(dz, margin) {
        raise(Level::Caution);
    }
    if range < ADVISORY_RANGE_M && within(dz, 2.0 * margin) {
        raise(Level::Advisory);
    }
    let mut level = level?;
    if basis == AltBasis::Unknown && level == Level::Warning {
        level = Level::Caution;
    }
    Some((level, range, dz, cpa, basis))
}

/// Every pair worth reporting, most serious first, then nearest.
pub fn find<'a>(
    drones: impl Iterator<Item = &'a Track>,
    aircraft: &[AircraftTrack],
    ctx: &Context,
) -> Vec<Conflict, MAX_CONFLICTS> {
    let now = ctx.now_ms;
    let planes: Vec<(AircraftKey, Body), { crate::aircraft::MAX_AIRCRAFT }> = aircraft
        .iter()
        .filter(|a| ctx.aircraft_state(a) == TrackState::Active && a.established())
        .filter_map(|a| Some((a.key, aircraft_body(a, now)?)))
        .collect();
    let mut out: Vec<Conflict, MAX_CONFLICTS> = Vec::new();
    // Only standing on both sides (WP-09): a confirmed drone track and an
    // established aircraft, so neither a forged drone nor a forged
    // aircraft raises (or, by filling the tables, hides) the warning.
    for t in drones.filter(|t| ctx.state(t) == TrackState::Active && t.confirmed()) {
        let Some(d) = drone_body(t, now) else {
            continue;
        };
        for (key, a) in &planes {
            let Some((level, range, dz, cpa, basis)) = assess(&d, a) else {
                continue;
            };
            let c = Conflict {
                drone: t.id,
                aircraft: *key,
                level,
                range_m: range as f32,
                dz_m: dz.map(|z| z as f32),
                cpa_s: cpa.map(|c| c.0 as f32),
                cpa_m: cpa.map(|c| c.1 as f32),
                basis,
            };
            if out.push(c).is_err() {
                // Full: replace the least serious if this one is worse.
                if let Some(w) = out.iter_mut().min_by(|x, y| rank(x).total_cmp(&rank(y)))
                    && rank(&c) > rank(w)
                {
                    *w = c;
                }
            }
        }
    }
    out.sort_unstable_by(|x, y| rank(y).total_cmp(&rank(x)));
    out
}

/// Higher is more serious.
fn rank(c: &Conflict) -> f64 {
    f64::from(c.level as u8) * 1.0e6 - f64::from(c.range_m)
}

/// Which pairs have been announced, so each alerts once per level (and again
/// after [`REALERT_MS`]).
#[derive(Debug, Default)]
pub struct ConflictWatch {
    seen: Vec<(TrackId, AircraftKey, Level, u64), MAX_CONFLICTS>,
}

impl ConflictWatch {
    pub const fn new() -> Self {
        Self { seen: Vec::new() }
    }

    /// The first pair in `now` that is new, more serious than when last
    /// announced, or last announced over [`REALERT_MS`] ago.
    pub fn take_new(&mut self, current: &[Conflict], now_ms: u64) -> Option<Conflict> {
        self.seen.retain(|s| {
            now_ms.saturating_sub(s.3) < REALERT_MS
                || current.iter().any(|c| (c.drone, c.aircraft) == (s.0, s.1))
        });
        for c in current {
            match self
                .seen
                .iter_mut()
                .find(|s| (s.0, s.1) == (c.drone, c.aircraft))
            {
                Some(s) if c.level > s.2 || now_ms.saturating_sub(s.3) >= REALERT_MS => {
                    *s = (c.drone, c.aircraft, c.level, now_ms);
                    return Some(*c);
                }
                Some(s) => {
                    // Lower now: remember the lower level, so climbing back
                    // up alerts again.
                    if c.level < s.2 {
                        s.2 = c.level;
                    }
                }
                None => {
                    let entry = (c.drone, c.aircraft, c.level, now_ms);
                    if self.seen.push(entry).is_err() {
                        let oldest = (0..self.seen.len()).min_by_key(|&i| self.seen[i].3)?;
                        self.seen[oldest] = entry;
                    }
                    return Some(*c);
                }
            }
        }
        None
    }
}

/// One line of text for the console, the evidence log and the screen:
/// "WARNING: drone 1581F5... 240 m from N123AB (helicopter), 30 m below it,
/// closest in 12 s at 80 m".
pub fn describe(c: &Conflict, drone: &Track, ac: Option<&AircraftTrack>) -> String<160> {
    let mut s: String<160> = String::new();
    let id = drone.primary_id().map(|u| u.id).and_then(|f| {
        let mut t: String<20> = String::new();
        t.push_str(f.as_str()?).ok()?;
        Some(t)
    });
    let _ = write!(s, "{}: drone ", c.level.name());
    match &id {
        Some(t) if !t.is_empty() => {
            let _ = write!(s, "{}", t.as_str());
        }
        _ => {
            let _ = write!(s, "#{}", drone.id.0);
        }
    }
    let _ = write!(s, " {:.0} m from ", c.range_m);
    match ac {
        Some(a) if !a.callsign.as_str().is_empty() => {
            let _ = write!(s, "{}", a.callsign.as_str());
        }
        _ => {
            let _ = write!(s, "{:06X}", c.aircraft.icao);
        }
    }
    if let Some(a) = ac {
        let _ = write!(s, " ({})", a.kind().label());
    }
    match c.dz_m {
        Some(z) if z >= 0.0 => {
            let _ = write!(s, ", {:.0} m below it", z);
        }
        Some(z) => {
            let _ = write!(s, ", {:.0} m above it", -z);
        }
        None => {
            let _ = write!(s, ", height unknown");
        }
    }
    if let (Some(t), Some(m)) = (c.cpa_s, c.cpa_m) {
        let _ = write!(s, ", closest in {:.0} s at {:.0} m", t, m);
    }
    s
}

#[derive(serde::Serialize)]
struct ConflictLine<'a> {
    t: &'static str,
    level: &'static str,
    drone: u32,
    icao: u32,
    src: &'static str,
    callsign: &'a str,
    range_m: f32,
    dz_m: Option<f32>,
    cpa_s: Option<f32>,
    cpa_m: Option<f32>,
    alt_basis: &'static str,
}

/// Longest `{"t":"conflict"}` line.
pub const MAX_CONFLICT_LINE: usize = 256;

/// `{"t":"conflict",...}\n` into `buf`.
pub fn conflict_line(
    c: &Conflict,
    ac: Option<&AircraftTrack>,
    buf: &mut [u8],
) -> Result<usize, crate::text::FormatError> {
    let line = ConflictLine {
        t: "conflict",
        level: c.level.name(),
        drone: c.drone.0,
        icao: c.aircraft.icao,
        src: source_name(c.aircraft.source),
        callsign: ac.map_or("", |a| a.callsign.as_str()),
        range_m: roundf(c.range_m * 10.0) / 10.0,
        dz_m: c.dz_m.map(|z| roundf(z * 10.0) / 10.0),
        cpa_s: c.cpa_s.map(|t| roundf(t * 10.0) / 10.0),
        cpa_m: c.cpa_m.map(|m| roundf(m * 10.0) / 10.0),
        alt_basis: c.basis.name(),
    };
    let n = serde_json_core::to_slice(&line, buf)
        .map_err(|_| crate::text::FormatError::BufferTooSmall)?;
    let nl = buf
        .get_mut(n)
        .ok_or(crate::text::FormatError::BufferTooSmall)?;
    *nl = b'\n';
    Ok(n + 1)
}
