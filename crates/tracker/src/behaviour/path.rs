//! Where each drone has been: a short path history per drone track.
//!
//! Two rings per drone: one point a second for the last two minutes (the
//! "fine" ring, used for straightness and smoothing), and one point every
//! five seconds for the last twenty minutes (the "coarse" ring, for the
//! lingering and circling rules of slice 16b). A point is 12 bytes, so a
//! drone costs about 4.4 KB and 64 drones about 280 KB. The store lives in
//! the tracker, which the P4 hub keeps in PSRAM.
//!
//! The store is not inside [`crate::Track`]: tracks are moved and merged by
//! value, and a 4 KB track would make every such move a 4 KB copy on the
//! stack. Each ring is tagged with the [`TrackId`] that owns it instead.
//!
//! The unit's own trail ([`OwnTrail`], one point every five seconds) is
//! kept here too, for the "following" rule; and with each ring, what the
//! rules across drones and days (slice 16d) keep per drone
//! ([`super::watch::Watch`]: the pilot's recent positions, the drone's
//! serials).

use heapless::Deque;

use crate::TrackId;

/// Fine ring: one point a second for two minutes.
pub const FINE_POINTS: usize = 120;
/// Coarse ring: one point every five seconds for twenty minutes.
pub const COARSE_POINTS: usize = 240;
/// Spacing of the fine ring (a little under a second, so a once-a-second
/// caller whose calls wander a few milliseconds never skips one).
pub const FINE_STEP_MS: u64 = 900;
/// Spacing of the coarse ring.
pub const COARSE_STEP_MS: u64 = 4_900;

/// "Height unknown" in [`PathPoint::alt_half_m`].
pub const NO_ALT: i16 = i16::MIN;

/// One remembered position, 12 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PathPoint {
    /// Latitude and longitude, 1e-7 degree (about 1 cm; Remote ID's own unit).
    pub lat_e7: i32,
    pub lon_e7: i32,
    /// Height above the WGS-84 ellipsoid in half metres, [`NO_ALT`] unknown.
    pub alt_half_m: i16,
    /// Hub time in tenths of a second, wrapping every 109 minutes. The ring
    /// keeps the full time of its newest point, and every point it holds is
    /// younger than 20 minutes, so the wrap never matters.
    tag: u16,
}

impl PathPoint {
    pub fn new(lat: f64, lon: f64, alt_m: Option<f32>, at_ms: u64) -> Self {
        let e7 = |d: f64| libm::round(d * 1e7) as i32;
        let alt = alt_m
            .filter(|a| a.is_finite())
            .map_or(NO_ALT, |a| (libm::roundf(a * 2.0) as i32).clamp(i32::from(NO_ALT) + 1, i32::from(i16::MAX)) as i16);
        Self { lat_e7: e7(lat), lon_e7: e7(lon), alt_half_m: alt, tag: tag_of(at_ms) }
    }

    pub fn lat(&self) -> f64 {
        f64::from(self.lat_e7) * 1e-7
    }

    pub fn lon(&self) -> f64 {
        f64::from(self.lon_e7) * 1e-7
    }

    pub fn alt_m(&self) -> Option<f32> {
        (self.alt_half_m != NO_ALT).then(|| f32::from(self.alt_half_m) / 2.0)
    }
}

fn tag_of(ms: u64) -> u16 {
    (ms / 100) as u16
}

/// One drone's history.
#[derive(Debug, Clone)]
pub struct PathRing {
    /// The owning track's id; 0 when free (track ids start at 1).
    owner: u32,
    /// Hub time of the newest point (full width).
    last_ms: u64,
    last_coarse_ms: u64,
    fine: Deque<PathPoint, FINE_POINTS>,
    coarse: Deque<PathPoint, COARSE_POINTS>,
    /// The pilot's trail and the drone's serials (slice 16d).
    watch: super::watch::Watch,
}

impl PathRing {
    pub const fn new() -> Self {
        Self {
            owner: 0,
            last_ms: 0,
            last_coarse_ms: 0,
            fine: Deque::new(),
            coarse: Deque::new(),
            watch: super::watch::Watch::new(),
        }
    }

    /// The pilot's trail and the drone's serials (slice 16d).
    pub fn watch(&self) -> &super::watch::Watch {
        &self.watch
    }

    pub fn owner(&self) -> Option<TrackId> {
        (self.owner != 0).then_some(TrackId(self.owner))
    }

    /// Hub time of a point held here.
    pub fn time_of(&self, p: &PathPoint) -> u64 {
        let back = u64::from(tag_of(self.last_ms).wrapping_sub(p.tag)) * 100;
        self.last_ms.saturating_sub(back)
    }

    /// Hub time of the newest point, if any.
    pub fn last_ms(&self) -> Option<u64> {
        (!self.fine.is_empty()).then_some(self.last_ms)
    }

    /// Points in the fine ring (one a second), oldest first.
    pub fn fine(&self) -> impl DoubleEndedIterator<Item = (u64, &PathPoint)> + '_ {
        let (a, b) = self.fine.as_slices();
        a.iter().chain(b.iter()).map(move |p| (self.time_of(p), p))
    }

    /// Points in the coarse ring (one every five seconds), oldest first.
    pub fn coarse(&self) -> impl DoubleEndedIterator<Item = (u64, &PathPoint)> + '_ {
        let (a, b) = self.coarse.as_slices();
        a.iter().chain(b.iter()).map(move |p| (self.time_of(p), p))
    }

    pub fn fine_len(&self) -> usize {
        self.fine.len()
    }

    pub fn coarse_len(&self) -> usize {
        self.coarse.len()
    }

    fn reset(&mut self, owner: u32) {
        *self = Self::new();
        self.owner = owner;
    }

    /// Add a position heard at `at_ms`. Kept only when at least
    /// [`FINE_STEP_MS`] after the newest point (and in the coarse ring
    /// [`COARSE_STEP_MS`] after its newest). An older or equal time is
    /// ignored (the same report seen again).
    fn push(&mut self, p: PathPoint, at_ms: u64) {
        if !self.fine.is_empty() && at_ms < self.last_ms + FINE_STEP_MS {
            return;
        }
        if self.fine.is_full() {
            self.fine.pop_front();
        }
        let _ = self.fine.push_back(p);
        self.last_ms = at_ms;
        if self.coarse.is_empty() || at_ms >= self.last_coarse_ms + COARSE_STEP_MS {
            if self.coarse.is_full() {
                self.coarse.pop_front();
            }
            let _ = self.coarse.push_back(p);
            self.last_coarse_ms = at_ms;
        }
    }
}

impl Default for PathRing {
    fn default() -> Self {
        Self::new()
    }
}

/// One ring per track the tracker can hold.
pub struct PathStore<const N: usize> {
    rings: [PathRing; N],
}

impl<const N: usize> Default for PathStore<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> PathStore<N> {
    const EMPTY: PathRing = PathRing::new();

    pub const fn new() -> Self {
        Self { rings: [Self::EMPTY; N] }
    }

    /// The history of `id`, if it has one.
    pub fn get(&self, id: TrackId) -> Option<&PathRing> {
        self.rings.iter().find(|r| r.owner == id.0 && r.owner != 0)
    }

    /// Record a position of track `id` heard at `at_ms`. A track with no
    /// ring takes a free one, else the one whose newest point is oldest
    /// (more tracks than rings cannot happen: there is one ring per track
    /// slot, and rings of removed tracks are freed by [`PathStore::retain`]).
    pub fn record(&mut self, id: TrackId, lat: f64, lon: f64, alt_m: Option<f32>, at_ms: u64) {
        if id.0 == 0 || N == 0 {
            return;
        }
        let i = match self.rings.iter().position(|r| r.owner == id.0) {
            Some(i) => i,
            None => {
                let i = self.rings.iter().position(|r| r.owner == 0).unwrap_or_else(|| {
                    (0..N).min_by_key(|&i| self.rings[i].last_ms).unwrap_or(0)
                });
                self.rings[i].reset(id.0);
                i
            }
        };
        self.rings[i].push(PathPoint::new(lat, lon, alt_m, at_ms), at_ms);
    }

    /// What slice 16d keeps for `id`, if it has a ring.
    pub(crate) fn watch_mut(&mut self, id: TrackId) -> Option<&mut super::watch::Watch> {
        self.rings.iter_mut().find(|r| r.owner == id.0 && r.owner != 0).map(|r| &mut r.watch)
    }

    /// Free the rings of tracks that no longer exist.
    pub fn retain(&mut self, mut alive: impl FnMut(TrackId) -> bool) {
        for r in self.rings.iter_mut() {
            if r.owner != 0 && !alive(TrackId(r.owner)) {
                r.reset(0);
            }
        }
    }

    /// Rings in use.
    pub fn in_use(&self) -> usize {
        self.rings.iter().filter(|r| r.owner != 0).count()
    }
}

/// Points of the unit's own trail kept for each drone path ring (so a hub
/// that keeps no drone histories keeps no trail either).
pub const OWN_POINTS_PER_RING: usize = 4;
/// Spacing of the unit's own trail (as the coarse ring).
pub const OWN_STEP_MS: u64 = COARSE_STEP_MS;
/// A gap in the unit's fixes longer than this starts the trail again.
pub const OWN_GAP_MS: u64 = 60_000;

/// One remembered position of the unit itself, 12 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OwnPoint {
    pub lat_e7: i32,
    pub lon_e7: i32,
    /// Hub time in tenths of a second (wraps after 13 years).
    t_ds: u32,
}

impl OwnPoint {
    const EMPTY: OwnPoint = OwnPoint { lat_e7: 0, lon_e7: 0, t_ds: 0 };

    pub fn lat(&self) -> f64 {
        f64::from(self.lat_e7) * 1e-7
    }

    pub fn lon(&self) -> f64 {
        f64::from(self.lon_e7) * 1e-7
    }

    /// Hub time, milliseconds (to the tenth of a second).
    pub fn ms(&self) -> u64 {
        u64::from(self.t_ds) * 100
    }
}

/// Where the unit itself has been: one point every five seconds, for the
/// "following" rule (slice 16b). It holds [`OWN_POINTS_PER_RING`] points
/// for each drone path ring, so the P4 hub (64 rings) keeps 256 points,
/// about 21 minutes, in 3 KB; a hub with no path rings keeps none.
#[derive(Debug, Clone)]
pub struct OwnTrail<const P: usize> {
    /// Where the next point goes, and how many are held.
    head: u16,
    len: u16,
    pts: [[OwnPoint; OWN_POINTS_PER_RING]; P],
}

impl<const P: usize> Default for OwnTrail<P> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const P: usize> OwnTrail<P> {
    const CAP: usize = P * OWN_POINTS_PER_RING;

    pub const fn new() -> Self {
        Self { head: 0, len: 0, pts: [[OwnPoint::EMPTY; OWN_POINTS_PER_RING]; P] }
    }

    pub fn len(&self) -> usize {
        usize::from(self.len)
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The `i`th newest point (0 the newest).
    pub fn get(&self, i: usize) -> Option<&OwnPoint> {
        if i >= self.len() {
            return None;
        }
        let k = (usize::from(self.head) + Self::CAP - 1 - i) % Self::CAP;
        Some(&self.pts[k / OWN_POINTS_PER_RING][k % OWN_POINTS_PER_RING])
    }

    /// Add a fix taken at `at_ms`. Kept only [`OWN_STEP_MS`] after the
    /// newest point; a gap over [`OWN_GAP_MS`], or time going backwards,
    /// starts the trail again.
    pub fn record(&mut self, lat: f64, lon: f64, at_ms: u64) {
        if Self::CAP == 0 {
            return;
        }
        if let Some(last) = self.get(0).map(OwnPoint::ms) {
            if at_ms < last || at_ms > last + OWN_GAP_MS {
                self.clear();
            } else if at_ms < last + OWN_STEP_MS {
                return;
            }
        }
        let e7 = |d: f64| libm::round(d * 1e7) as i32;
        let k = usize::from(self.head);
        self.pts[k / OWN_POINTS_PER_RING][k % OWN_POINTS_PER_RING] =
            OwnPoint { lat_e7: e7(lat), lon_e7: e7(lon), t_ds: (at_ms / 100) as u32 };
        self.head = ((k + 1) % Self::CAP) as u16;
        self.len = (self.len() + 1).min(Self::CAP) as u16;
    }

    pub fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rings_keep_two_minutes_fine_and_twenty_coarse() {
        let mut s: PathStore<4> = PathStore::new();
        let id = TrackId(7);
        // 30 minutes at one report a second (and the same report twice).
        for k in 0..1800u64 {
            let t = 1_000_000 + k * 1000;
            s.record(id, 39.95 + k as f64 * 1e-6, -75.16, Some(100.25), t);
            s.record(id, 39.95 + k as f64 * 1e-6, -75.16, Some(100.25), t + 10);
        }
        let r = s.get(id).unwrap();
        assert_eq!(r.fine_len(), FINE_POINTS);
        assert_eq!(r.coarse_len(), COARSE_POINTS);
        let (t0, p0) = r.fine().next().unwrap();
        let (t1, p1) = r.fine().next_back().unwrap();
        assert_eq!(t1, 1_000_000 + 1799 * 1000, "newest time is exact");
        assert_eq!(t1 - t0, 119_000, "two minutes");
        assert!((p1.lat() - (39.95 + 1799.0 * 1e-6)).abs() < 1e-7);
        assert_eq!(p0.alt_m(), Some(100.5), "half-metre steps");
        let (c0, _) = r.coarse().next().unwrap();
        let (c1, _) = r.coarse().next_back().unwrap();
        assert!(c1 - c0 >= 19 * 60_000 && c1 - c0 <= 20 * 60_000, "{}", c1 - c0);
        // Times survive the 109-minute wrap of the stored tag.
        let mut s2: PathStore<1> = PathStore::new();
        for k in 0..10u64 {
            s2.record(id, 0.0, 0.0, None, 6_553_000 + k * 1000);
        }
        let times: std::vec::Vec<u64> = s2.get(id).unwrap().fine().map(|(t, _)| t).collect();
        assert_eq!(times, (0..10).map(|k| 6_553_000 + k * 1000).collect::<std::vec::Vec<_>>());
    }

    #[test]
    fn rings_are_freed_and_reused() {
        let mut s: PathStore<2> = PathStore::new();
        s.record(TrackId(1), 1.0, 1.0, None, 1000);
        s.record(TrackId(2), 2.0, 2.0, None, 2000);
        assert_eq!(s.in_use(), 2);
        // A third track takes the oldest ring.
        s.record(TrackId(3), 3.0, 3.0, None, 3000);
        assert!(s.get(TrackId(1)).is_none());
        assert_eq!(s.get(TrackId(3)).unwrap().fine_len(), 1);
        s.retain(|id| id == TrackId(3));
        assert_eq!(s.in_use(), 1);
        assert!(s.get(TrackId(2)).is_none());
    }

    #[test]
    fn own_trail_keeps_a_point_every_five_seconds() {
        let mut o: OwnTrail<2> = OwnTrail::new();
        assert!(o.is_empty());
        for k in 0..60u64 {
            o.record(1.0 + k as f64 * 1e-5, 2.0, 10_000 + k * 1000);
        }
        // Eight points (two rings' worth), the newest at second 59 - 59 % 5.
        assert_eq!(o.len(), 8);
        let newest = o.get(0).unwrap();
        assert_eq!(newest.ms(), 10_000 + 55_000);
        assert!((newest.lat() - (1.0 + 55.0 * 1e-5)).abs() < 1e-7);
        assert_eq!(o.get(7).unwrap().ms(), 10_000 + 20_000);
        assert!(o.get(8).is_none());
        // A long gap starts again.
        o.record(1.0, 2.0, 10_000 + 55_000 + OWN_GAP_MS + 1);
        assert_eq!(o.len(), 1);
        // No rings, no trail.
        let mut z: OwnTrail<0> = OwnTrail::new();
        z.record(1.0, 2.0, 1000);
        assert!(z.is_empty() && core::mem::size_of::<OwnTrail<0>>() <= 4);
    }

    extern crate std;
}
