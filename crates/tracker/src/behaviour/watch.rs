//! Per-drone things the rules across drones and days need (slice 16d),
//! kept with the drone's path history ([`super::PathRing`]), so only on
//! hubs that keep one (the P4):
//!
//! - **The pilot's recent positions** (rule 18, "pilot on the move"): up
//!   to [`PILOT_POINTS`] broadcast pilot positions, one every
//!   [`PILOT_STEP_MS`] or more.
//! - **The drone's serial numbers** (rule 19, "identity changes"): a
//!   fingerprint of its Remote ID serial and of its DJI serial, and when
//!   either last changed within this track.
//!
//! **Pilot positions are rough** (a phone's GPS, or the take-off point
//! repeated), so "moving" needs all of: the trail spans at least
//! [`PILOT_SPAN_MS`]; the average of its newest third and of its oldest
//! third are at least [`PILOT_FAST_MPS`] apart per second; each half of the
//! trail moves at least [`PILOT_HALF_SHARE`] of that; and most single steps
//! ([`PILOT_STEADY_SHARE`]) move at least half that speed. One jump of a
//! bad position fails the last test; a walking pilot and a jittery phone
//! fail the first.

use heapless::Deque;
use libm::sqrtf;

use super::places::Frame;
use crate::Track;

/// Pilot positions kept per drone.
pub const PILOT_POINTS: usize = 10;
/// One pilot position kept per this long, at most.
pub const PILOT_STEP_MS: u64 = 4_900;
/// A gap this long in the pilot's reports starts the trail again.
pub const PILOT_GAP_MS: u64 = 30_000;
/// "Moving" is judged over at least this long.
pub const PILOT_SPAN_MS: u64 = 30_000;
/// Faster than this is "on the move" (a brisk run is about 4 m/s).
pub const PILOT_FAST_MPS: f32 = 5.0;
/// Each half of the trail moves at least this share of [`PILOT_FAST_MPS`].
pub const PILOT_HALF_SHARE: f32 = 0.6;
/// This share of single steps moves at least half [`PILOT_FAST_MPS`].
pub const PILOT_STEADY_SHARE: f32 = 0.66;
/// A pilot position older than this is not taken; the newest kept must be
/// at most this old for the pilot to count as moving.
pub const PILOT_FRESH_MS: u64 = 15_000;

/// One pilot position: 1e-7 degrees, and hub time in ms (low 32 bits:
/// the trail never spans more than minutes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct PilotPoint {
    lat_e7: i32,
    lon_e7: i32,
    ms: u32,
}

impl PilotPoint {
    fn lat(&self) -> f64 {
        f64::from(self.lat_e7) * 1e-7
    }

    fn lon(&self) -> f64 {
        f64::from(self.lon_e7) * 1e-7
    }
}

/// What 16d keeps per drone (about 140 bytes).
#[derive(Debug, Clone)]
pub struct Watch {
    pilot: Deque<PilotPoint, PILOT_POINTS>,
    /// Hub time of the newest pilot point (full width).
    pilot_ms: u64,
    /// The pilot report last looked at (its reception time).
    pilot_rx_ms: u64,
    /// Fingerprints of the Remote ID serial and the DJI serial (0 none).
    serial: u32,
    dji: u32,
    /// When either changed within this track (0 never).
    id_changed_ms: u64,
}

impl Default for Watch {
    fn default() -> Self {
        Self::new()
    }
}

/// How the pilot moves (rule 18).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PilotMove {
    pub mps: f32,
    /// How long the trail it was judged on spans.
    pub span_ms: u64,
}

impl Watch {
    pub const fn new() -> Self {
        Self { pilot: Deque::new(), pilot_ms: 0, pilot_rx_ms: 0, serial: 0, dji: 0, id_changed_ms: 0 }
    }

    /// Once a second, for every drone with a path ring: take its newest
    /// pilot position and look at its serials.
    pub(crate) fn note(&mut self, t: &Track, now_ms: u64) {
        if let Some(s) = &t.system
            && let Some(p) = s.value.operator_position
            && s.rx_ms > self.pilot_rx_ms
            && now_ms.saturating_sub(s.rx_ms) <= PILOT_FRESH_MS
        {
            self.pilot_rx_ms = s.rx_ms;
            self.pilot_point(p.lat, p.lon, s.rx_ms);
        }
        let h = |b: &[u8]| crate::claims::hash_bytes(b);
        let serial = t
            .uas_ids
            .iter()
            .find(|u| u.id_type == rid::message::IdType::SerialNumber)
            .map_or(0, |u| h(u.id.as_bytes()));
        let dji = t.dji.serial.as_ref().map_or(0, |s| h(s.as_bytes()));
        for (old, new) in [(&mut self.serial, serial), (&mut self.dji, dji)] {
            if new != 0 {
                if *old != 0 && *old != new {
                    self.id_changed_ms = now_ms;
                }
                *old = new;
            }
        }
    }

    fn pilot_point(&mut self, lat: f64, lon: f64, ms: u64) {
        if !self.pilot.is_empty() {
            if ms < self.pilot_ms + PILOT_STEP_MS {
                return;
            }
            if ms > self.pilot_ms + PILOT_GAP_MS {
                self.pilot.clear();
            }
        }
        if self.pilot.is_full() {
            self.pilot.pop_front();
        }
        let e7 = |d: f64| libm::round(d * 1e7) as i32;
        let _ = self.pilot.push_back(PilotPoint { lat_e7: e7(lat), lon_e7: e7(lon), ms: ms as u32 });
        self.pilot_ms = ms;
    }

    /// The pilot is on the move (rule 18): see the module notes.
    pub fn pilot_moving(&self, now_ms: u64) -> Option<PilotMove> {
        let n = self.pilot.len();
        if n < 4 || now_ms.saturating_sub(self.pilot_ms) > PILOT_FRESH_MS {
            return None;
        }
        let (a, b) = self.pilot.as_slices();
        let pts = || a.iter().chain(b.iter());
        let newest = *pts().last()?;
        let frame = Frame::at(newest.lat(), newest.lon());
        // Seconds before the newest point (the stamps wrap every 49 days).
        let secs = |p: &PilotPoint| newest.ms.wrapping_sub(p.ms) as f32 / 1000.0;
        let span = secs(pts().next()?);
        if span * 1000.0 < PILOT_SPAN_MS as f32 {
            return None;
        }
        let mean = |skip: usize, take: usize| {
            let (mut x, mut y, mut t) = (0.0f32, 0.0f32, 0.0f32);
            for p in pts().skip(skip).take(take) {
                let (px, py) = frame.xy(p.lat(), p.lon());
                x += px;
                y += py;
                t += secs(p);
            }
            let k = take.max(1) as f32;
            (x / k, y / k, t / k)
        };
        let third = (n / 3).max(1);
        let old = mean(0, third);
        let mid = mean(third, n - 2 * third);
        let new = mean(n - third, third);
        let speed = |p: (f32, f32, f32), q: (f32, f32, f32)| {
            let dt = (p.2 - q.2).max(0.5);
            sqrtf((q.0 - p.0) * (q.0 - p.0) + (q.1 - p.1) * (q.1 - p.1)) / dt
        };
        let mps = speed(old, new);
        let half = PILOT_FAST_MPS * PILOT_HALF_SHARE;
        if mps < PILOT_FAST_MPS || speed(old, mid) < half || speed(mid, new) < half {
            return None;
        }
        // Steady: most single steps move, so one jump is not a drive.
        let mut steady = 0usize;
        let mut prev: Option<&PilotPoint> = None;
        for p in pts() {
            if let Some(q) = prev {
                let (x0, y0) = frame.xy(q.lat(), q.lon());
                let (x1, y1) = frame.xy(p.lat(), p.lon());
                let dt = (secs(q) - secs(p)).max(0.5);
                if sqrtf((x1 - x0) * (x1 - x0) + (y1 - y0) * (y1 - y0)) / dt >= PILOT_FAST_MPS / 2.0 {
                    steady += 1;
                }
            }
            prev = Some(p);
        }
        if (steady as f32) < PILOT_STEADY_SHARE * (n - 1) as f32 {
            return None;
        }
        Some(PilotMove { mps, span_ms: (span * 1000.0) as u64 })
    }

    /// When the drone's serial (Remote ID or DJI) last changed within this
    /// track, if it ever did.
    pub fn id_changed_ms(&self) -> Option<u64> {
        (self.id_changed_ms != 0).then_some(self.id_changed_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::behaviour::scenario::offset;

    const AT: (f64, f64) = (39.9521, -75.1638);

    fn trail(step_m: impl Fn(u32) -> (f64, f64), n: u32) -> Watch {
        let mut w = Watch::new();
        for i in 0..n {
            let (e, nn) = step_m(i);
            let (lat, lon) = offset(AT.0, AT.1, e, nn);
            w.pilot_point(lat, lon, 1_000_000 + u64::from(i) * 5_000);
        }
        w
    }

    #[test]
    fn a_driving_pilot_moves_a_walking_one_does_not() {
        let now = 1_000_000 + 9 * 5_000;
        // 15 m/s east, ten points 5 s apart: 45 s.
        let car = trail(|i| (f64::from(i) * 75.0, 0.0), 10);
        let m = car.pilot_moving(now).unwrap();
        assert!((m.mps - 15.0).abs() < 0.5, "{m:?}");
        assert_eq!(m.span_ms, 45_000);
        // Walking, 1.5 m/s, with 8 m of phone jitter.
        let walk = trail(|i| (f64::from(i) * 7.5 + if i % 2 == 0 { 8.0 } else { -8.0 }, 0.0), 10);
        assert_eq!(walk.pilot_moving(now), None);
        // One jump of 300 m (a bad position, or the reported spot changing).
        let jump = trail(|i| (if i >= 5 { 300.0 } else { 0.0 }, 0.0), 10);
        assert_eq!(jump.pilot_moving(now), None);
        // A drive too short to judge (20 s).
        let short = trail(|i| (f64::from(i) * 75.0, 0.0), 5);
        assert_eq!(short.pilot_moving(1_000_000 + 4 * 5_000), None);
        // An old trail is not "moving now".
        assert_eq!(car.pilot_moving(now + PILOT_FRESH_MS + 1), None);
    }
}
