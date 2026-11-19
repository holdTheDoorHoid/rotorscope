//! Rules across drones and days (slice 16d, docs/THREAT-PROFILES.md rules
//! 14-19), for drones that report their position, confirmed tracks only:
//!
//! - **14 Spotter then striker** (Critical): a drone lingered (loitering or
//!   circling, rules 7 and 8) at a place, and within [`STRIKE_WINDOW_MS`]
//!   of its last lingering a *different* drone heads for the same place
//!   (beeline or fast inbound, rules 5 and 6). The striker's reason names
//!   the spotter; the spotter, while still heard, is marked too (Warning).
//! - **15 Several at once** (Warning): the site's `crowd` number of drones
//!   (3 unless the site file says) at or near the places at once, or any
//!   drone at or near a place while the hub hears jamming, a node is
//!   blinded, or GPS interference is seen in the area.
//! - **16 Probing** (Caution): back in an area it left between
//!   [`PROBE_MIN_OUT_MS`] and [`PROBE_WINDOW_MS`] before, having gone at
//!   least [`PROBE_AWAY_M`] beyond the edge meanwhile (a drone hovering at
//!   the fence, whose rough positions put it in and out, is not probing).
//!   Probing and "in
//!   the area" are **two signs**, so a drone back over the area is a
//!   Warning: coming back after leaving is the "test the response" pattern
//!   CISA lists.
//! - **17 Repeat visitor**: the same drone ID, or the same pilot spot, at
//!   the places on [`super::visits::REPEAT_DAYS`] or more days within two
//!   weeks ([`super::visits`]). It raises the drone's other reasons by one
//!   level, never above Warning (a Caution becomes a Warning); on its own it
//!   is a reason at Notice.
//! - **18 Pilot on the move** (Caution): the broadcast pilot position moving
//!   faster than 5 m/s for at least 30 s ([`super::watch`]) while the drone
//!   is at or near a place.
//! - **19 Identity changes** (Warning): near a place, within
//!   [`IDENTITY_RECENT_MS`] of (a) the track starting as the continuation
//!   of an earlier one under a different ID mid-flight
//!   ([`crate::continuity`]), or (b) its serial changing within the track.
//!
//! "At or near a place" ([`NEAR_MIN_M`]): inside it, or outside its edge by
//! at most its own size (the radius for a circle or the bubble; the widest
//! distance from the middle to a corner for a polygon), at least 50 m.
//!
//! **Friends.** A friend is never a spotter, is not counted towards
//! several at once, and is never recorded as a visitor. Several at once,
//! probing and pilot on the move give a friend only Notice (as the place
//! rules do). The spotter-then-striker reason on a striker is never
//! lowered (as rules 5 and 6), nor are identity changes: switching to a
//! friend's serial mid-flight is exactly how a faked friend would look.
//!
//! **Hook for 16c** (not built): a striker need not report its position.
//! An FPV signal getting louder fast (rule 12) can call
//! [`Across::striker`] with the place it is judged to close on (the bubble,
//! or the only area, for a signal heard at the hub) and its own key; it then
//! gets the same answer a positional striker gets.
//!
//! **Memory.** Per drone path ring one spotter slot and one exit slot (48
//! and 24 bytes), plus the visit table; nothing on hubs without path
//! history (the S3 hub), which do not run the threat levels anyway.

use heapless::Vec;

use super::places::{NAME_LEN, Name, Places};
use super::visits::{self, Visits};
use super::{Level, PathRing, Reason, Rule};
use crate::{Track, TrackId};

/// A striker counts this long after the spotter last lingered.
pub const STRIKE_WINDOW_MS: u64 = 600_000;
/// A spotter is marked while a striker it led was seen this recently.
pub const SPOTTER_MARK_MS: u64 = 5_000;
/// "Near a place": outside its edge by at most its own size, and never
/// less than this.
pub const NEAR_MIN_M: f32 = 50.0;
/// Probing: out of the area for at least this long (a drone at the fence
/// going in and out is not probing)...
pub const PROBE_MIN_OUT_MS: u64 = 30_000;
/// ...and back within this long...
pub const PROBE_WINDOW_MS: u64 = 1_800_000;
/// ...having gone at least this far beyond the edge (positions are good to
/// about 30 m).
pub const PROBE_AWAY_M: f32 = 50.0;
/// An identity change counts this long.
pub const IDENTITY_RECENT_MS: u64 = 600_000;

/// What the alarm codes in a rule-15 reason mean ([`Reason::alarm`]).
pub const ALARM_JAMMING: u8 = 1;
pub const ALARM_GNSS: u8 = 2;

/// A drone that lingered at a place (rule 14).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Spotter {
    /// Its track (0: free slot).
    track: u32,
    key: u64,
    place: [u8; NAME_LEN],
    last_ms: u64,
    /// When a striker it led was last seen, and which.
    struck_ms: u64,
    striker: u32,
}

/// A drone that left an area, and then (`back`) came back probing (rule
/// 16). Keyed by the drone's ID, so it holds across a new track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Exit {
    key: u64,
    /// The area's bit (as [`super::Eval::inside`]); 0: free slot.
    bit: u16,
    /// It went [`PROBE_AWAY_M`] beyond the edge since.
    far: bool,
    /// It came back: probing while it stays in.
    back: bool,
    /// How long it was away (seconds), once back.
    away_s: u16,
    /// When it left; once back, when it was last seen in (the entry is
    /// freed [`PROBE_WINDOW_MS`] after either).
    left_ms: u64,
}

/// What the rules across drones keep between seconds, in the tracker.
#[derive(Debug, Clone)]
pub struct Across<const P: usize> {
    spotters: [Spotter; P],
    exits: [Exit; P],
    /// Drones at or near the places counted this second and the last.
    near_now: u16,
    near_prev: u16,
    pruned_day: Option<u16>,
    pub(crate) visits: Visits<P>,
}

impl<const P: usize> Default for Across<P> {
    fn default() -> Self {
        Self::new()
    }
}

/// One second's view for [`super::evaluate`].
pub struct Cross<'a, const P: usize> {
    pub across: &'a mut Across<P>,
    /// An alarm that may hide or herald an attack is sounding (rule 15):
    /// [`ALARM_JAMMING`] (jamming, or a node blinded), [`ALARM_GNSS`] (GPS
    /// interference in the area), 0 none.
    pub alarm: u8,
    /// Today's UTC day number, when UTC is known (rule 17).
    pub today: Option<u16>,
}

/// The nearest place a drone is at or near.
#[derive(Debug, Clone, PartialEq)]
pub struct Near {
    pub name: Name,
    pub from_middle: f32,
    pub out_m: f32,
}

/// What [`super::evaluate`] found that these rules use.
pub(crate) struct Facts<'a> {
    pub key: u64,
    /// The ID kept for repeat visitors (serial, registration, DJI serial).
    pub id_key: Option<u64>,
    pub friend: bool,
    pub near: Option<&'a Near>,
    pub was_inside: u16,
    pub inside_now: u16,
    /// How far beyond each area's edge it is (0 inside; `f32::MAX` for an
    /// area with no outline).
    pub area_out: [f32; super::places::MAX_AREAS],
    pub pilot: Option<rid::message::LatLon>,
}

/// The drone's key for these rules: its ID's fingerprint (primary ID, else
/// its DJI serial), and whether that ID is one kept for repeat visitors
/// (a serial number or a registration: session and UTM IDs are new every
/// flight). A drone with no ID gets a key made from its track number.
pub fn keys(t: &Track) -> (u64, Option<u64>) {
    use rid::message::IdType;
    if let Some(u) = t.primary_id().filter(|u| !u.id.as_bytes().is_empty()) {
        let k = visits::fingerprint(u.id_type.code(), u.id.as_bytes());
        let kept = matches!(u.id_type, IdType::SerialNumber | IdType::CaaRegistration);
        return (k, kept.then_some(k));
    }
    if let Some(s) = t.dji.serial.as_ref().filter(|s| !s.as_bytes().is_empty()) {
        let k = visits::fingerprint(IdType::SerialNumber.code(), s.as_bytes());
        return (k, Some(k));
    }
    // Even, so never equal to a fingerprint (always odd).
    (u64::from(t.id.0) << 1, None)
}

fn place_bytes(n: &str) -> [u8; NAME_LEN] {
    let mut p = [0u8; NAME_LEN];
    p[..n.len().min(NAME_LEN)].copy_from_slice(&n.as_bytes()[..n.len().min(NAME_LEN)]);
    p
}

impl<const P: usize> Across<P> {
    pub const fn new() -> Self {
        const S: Spotter = Spotter { track: 0, key: 0, place: [0; NAME_LEN], last_ms: 0, struck_ms: 0, striker: 0 };
        const E: Exit = Exit { key: 0, bit: 0, far: false, back: false, away_s: 0, left_ms: 0 };
        Self { spotters: [S; P], exits: [E; P], near_now: 0, near_prev: 0, pruned_day: None, visits: Visits::new() }
    }

    /// The start of each second's judging: last second's count, old
    /// entries freed, the visit history pruned once a day (and cleared when
    /// the site file says `visits off`).
    pub fn begin_tick(&mut self, places: &Places, today: Option<u16>, now_ms: u64) {
        self.near_prev = core::mem::take(&mut self.near_now);
        for s in self.spotters.iter_mut().filter(|s| s.track != 0) {
            if now_ms.saturating_sub(s.last_ms.max(s.struck_ms)) > STRIKE_WINDOW_MS {
                *s = Spotter::default();
            }
        }
        for e in self.exits.iter_mut().filter(|e| e.bit != 0) {
            if now_ms.saturating_sub(e.left_ms) > PROBE_WINDOW_MS {
                *e = Exit::default();
            }
        }
        if !places.visits {
            if self.visits.counts() != (0, 0) {
                self.visits.clear();
            }
        } else if let Some(d) = today
            && self.pruned_day != Some(d)
        {
            self.pruned_day = Some(d);
            self.visits.prune(d);
        }
    }

    /// Drones at or near the places last second (rule 15).
    pub fn near_count(&self) -> u16 {
        self.near_prev
    }

    /// Drone `track` (key `key`) lingered at `place` now (rule 14).
    fn spotted(&mut self, track: TrackId, key: u64, place: &[u8; NAME_LEN], now_ms: u64) {
        let i = match self.spotters.iter().position(|s| s.track == track.0) {
            Some(i) => i,
            None => {
                let Some(i) = (0..P).min_by_key(|&i| (self.spotters[i].track != 0, self.spotters[i].last_ms)) else { return };
                self.spotters[i] = Spotter { track: track.0, key, ..Spotter::default() };
                i
            }
        };
        let s = &mut self.spotters[i];
        s.place = *place;
        s.last_ms = now_ms;
    }

    /// A drone (`track`, `key`; `previous`: the track it continues, if
    /// any) heads for `place` now: the spotter that led it, if one lingered
    /// there in the last [`STRIKE_WINDOW_MS`] (the most recent). The spotter
    /// is marked. This is also the hook for drones without a position
    /// (slice 16c, see the module notes).
    pub fn striker(&mut self, track: TrackId, key: u64, previous: Option<TrackId>, place: &str, now_ms: u64) -> Option<(TrackId, u64)> {
        let place = place_bytes(place);
        let s = self
            .spotters
            .iter_mut()
            .filter(|s| {
                s.track != 0
                    && s.track != track.0
                    && s.key != key
                    && previous != Some(TrackId(s.track))
                    && s.place == place
                    && now_ms.saturating_sub(s.last_ms) <= STRIKE_WINDOW_MS
            })
            .max_by_key(|s| s.last_ms)?;
        s.struck_ms = now_ms;
        s.striker = track.0;
        Some((TrackId(s.track), now_ms.saturating_sub(s.last_ms)))
    }

    /// Drone `track` is a spotter a striker followed in the last
    /// [`SPOTTER_MARK_MS`]: the striker, and the place.
    fn marked(&self, track: TrackId, now_ms: u64) -> Option<(TrackId, [u8; NAME_LEN])> {
        self.spotters
            .iter()
            .find(|s| s.track == track.0 && s.struck_ms != 0 && now_ms.saturating_sub(s.struck_ms) <= SPOTTER_MARK_MS)
            .map(|s| (TrackId(s.striker), s.place))
    }

    /// The drone left area `bit` (rule 16).
    fn left(&mut self, key: u64, bit: u16, now_ms: u64) {
        let Some(i) = self
            .exits
            .iter()
            .position(|e| e.bit == bit && e.key == key)
            .or_else(|| (0..P).min_by_key(|&i| (self.exits[i].bit != 0, self.exits[i].left_ms)))
        else {
            return;
        };
        self.exits[i] = Exit { key, bit, left_ms: now_ms, ..Exit::default() };
    }

    /// The drone is `out_m` beyond the edge of area `bit` it left.
    fn away(&mut self, key: u64, bit: u16, out_m: f32) {
        if out_m >= PROBE_AWAY_M
            && let Some(e) = self.exits.iter_mut().find(|e| e.bit == bit && e.key == key)
        {
            e.far = true;
        }
    }

    /// The drone came into area `bit`: probing when it was away long
    /// enough and far enough, and not too long.
    fn came_back(&mut self, key: u64, bit: u16, now_ms: u64) {
        let Some(e) = self.exits.iter_mut().find(|e| e.bit == bit && e.key == key) else { return };
        let away = now_ms.saturating_sub(e.left_ms);
        if e.far && (PROBE_MIN_OUT_MS..=PROBE_WINDOW_MS).contains(&away) {
            e.back = true;
            e.away_s = (away / 1000).min(u64::from(u16::MAX - 1)) as u16;
        } else {
            *e = Exit::default();
        }
    }

    /// The drone, in area `bit` now, is probing it: how long it was away
    /// (seconds). The entry is kept while it stays in.
    fn probing(&mut self, key: u64, bit: u16, now_ms: u64) -> Option<u16> {
        let e = self.exits.iter_mut().find(|e| e.back && e.bit == bit && e.key == key)?;
        e.left_ms = now_ms;
        Some(e.away_s)
    }

    /// The visit table (rule 17).
    pub fn visits(&self) -> &Visits<P> {
        &self.visits
    }
}

impl Reason {
    /// Another drone's track kept in place of the numbers these rules do
    /// not use (`eta_ds`, `speed_dms`).
    fn with_other(mut self, id: TrackId) -> Self {
        self.eta_ds = id.0 as u16;
        self.speed_dms = (id.0 >> 16) as u16;
        self
    }

    /// The other drone a reason is about (rules 14 and 19): the spotter on
    /// a striker, the striker on a spotter, the earlier track of a drone
    /// that changed identity.
    pub fn other(&self) -> Option<TrackId> {
        if !matches!(self.rule, Rule::SpotterStriker | Rule::Spotter | Rule::IdentityChanged) {
            return None;
        }
        let id = u32::from(self.eta_ds) | u32::from(self.speed_dms) << 16;
        (id != 0 && id != u32::MAX).then_some(TrackId(id))
    }

    fn with_code(mut self, code: u8) -> Self {
        self.eta_ds = u16::from(code);
        self
    }

    /// Several at once: why ([`ALARM_JAMMING`], [`ALARM_GNSS`]); `None` when
    /// it is the count of drones ([`Reason::count`]).
    pub fn alarm(&self) -> Option<u8> {
        (self.rule == Rule::Several && matches!(self.eta_ds, 1 | 2)).then_some(self.eta_ds as u8)
    }

    /// Several at once: how many drones; repeat visitor: on how many days.
    pub fn count(&self) -> Option<u16> {
        (matches!(self.rule, Rule::Several | Rule::RepeatVisitor) && self.alarm().is_none()).then(|| self.ago_s().unwrap_or(0))
    }

    /// Repeat visitor: matched by the pilot's spot rather than the drone's ID.
    pub fn by_pilot_spot(&self) -> bool {
        self.rule == Rule::RepeatVisitor && self.eta_ds == 1
    }
}

/// Rules 14-19 for one drone, adding to `hits`.
#[inline(never)]
pub(crate) fn judge<const H: usize, const P: usize>(
    hits: &mut Vec<Reason, H>,
    t: &Track,
    ring: Option<&PathRing>,
    places: &Places,
    f: &Facts<'_>,
    c: &mut Cross<'_, P>,
    now_ms: u64,
) {
    let a = &mut *c.across;
    let place_level = if f.friend { Level::Notice } else { Level::Caution };
    let at = |n: &Near, rule: Rule, level: Level| Reason::new(rule, level, &n.name, n.from_middle);
    let range_at = |hits: &Vec<Reason, H>, place: &[u8; NAME_LEN]| {
        hits.iter().find(|h| h.rule == Rule::InArea && &h.place == place).map_or(0.0, |h| h.range_m())
    };

    // 16: probing (fixed areas; the bubble moves with the unit, so coming
    // back into it is often the unit's own doing).
    let mut probing: Option<(usize, u16)> = None;
    for i in 0..places.areas.len() {
        let bit = 2u16 << i;
        let (was, is) = (f.was_inside & bit != 0, f.inside_now & bit != 0);
        if was && !is {
            a.left(f.key, bit, now_ms);
        } else if !is {
            a.away(f.key, bit, f.area_out[i]);
        } else if !was {
            a.came_back(f.key, bit, now_ms);
        }
        if is && probing.is_none() {
            probing = a.probing(f.key, bit, now_ms).map(|s| (i, s));
        }
    }
    if let Some((i, away_s)) = probing {
        let name = &places.areas[i].name;
        let r = Reason::new(Rule::Probing, place_level, name, range_at(hits, &place_bytes(name)));
        let _ = hits.push(r.with_secs(u64::from(away_s) * 1000));
    }

    // 14: spotter then striker.
    let lingering = hits
        .iter()
        .filter(|h| matches!(h.rule, Rule::Loitering | Rule::Circling) && h.level >= Level::Caution)
        .max_by_key(|h| h.level)
        .map(|h| h.place);
    if let Some(place) = lingering
        && !f.friend
    {
        a.spotted(t.id, f.key, &place, now_ms);
    }
    let inbound = hits.iter().find(|h| h.rule == Rule::FastInbound).or_else(|| hits.iter().find(|h| h.rule == Rule::Beeline)).copied();
    if let Some(r) = inbound
        && let Some((spotter, ago)) = a.striker(t.id, f.key, t.continuity.previous, r.place(), now_ms)
    {
        let s = Reason::new(Rule::SpotterStriker, Level::Critical, &Name::new(), r.range_m()).with_secs(ago).with_other(spotter);
        let _ = hits.push(Reason { place: r.place, ..s });
    }
    if let Some((striker, place)) = a.marked(t.id, now_ms) {
        let range = f.near.filter(|n| place_bytes(&n.name) == place).map_or(range_at(hits, &place), |n| n.from_middle);
        let s = Reason::new(Rule::Spotter, Level::Warning, &Name::new(), range).with_other(striker);
        let _ = hits.push(Reason { place, ..s });
    }

    // 15: several at once, or during jamming or GPS interference.
    if let Some(n) = f.near {
        if !f.friend {
            a.near_now = a.near_now.saturating_add(1);
        }
        let level = if f.friend { Level::Notice } else { Level::Warning };
        if c.alarm != 0 {
            let _ = hits.push(at(n, Rule::Several, level).with_code(c.alarm));
        } else if let Some(min) = places.crowd_min
            && a.near_prev >= u16::from(min)
        {
            let _ = hits.push(at(n, Rule::Several, level).with_secs(u64::from(a.near_prev) * 1000));
        }
    }

    if let Some(n) = f.near {
        // 18: pilot on the move.
        if let Some(m) = ring.and_then(|r| r.watch().pilot_moving(now_ms)) {
            let _ = hits.push(at(n, Rule::PilotMoving, place_level).with_speed(m.mps).with_secs(m.span_ms));
        }
        // 19: identity changes (never lowered for a friend).
        let linked = (t.continuity.identity_changed && now_ms.saturating_sub(t.first_seen_ms) <= IDENTITY_RECENT_MS)
            .then_some(t.continuity.previous)
            .flatten()
            .map(|p| (t.first_seen_ms, Some(p)));
        let within = ring
            .and_then(|r| r.watch().id_changed_ms())
            .filter(|&ms| now_ms.saturating_sub(ms) <= IDENTITY_RECENT_MS)
            .map(|ms| (ms, None));
        if let Some((ms, prev)) = [linked, within].into_iter().flatten().max_by_key(|x| x.0) {
            let r = at(n, Rule::IdentityChanged, Level::Warning).with_secs(now_ms.saturating_sub(ms));
            let _ = hits.push(match prev {
                Some(p) => r.with_other(p),
                None => r,
            });
        }
    }

    // 17: repeat visitor (last: it depends on the others).
    if let (Some(n), Some(day), false, true) = (f.near, c.today, f.friend, places.visits) {
        if let Some(k) = f.id_key {
            a.visits.see_id(k, day);
        }
        let cell = f.pilot.map(|p| visits::spot_cell(p.lat, p.lon));
        if let Some(cell) = cell {
            a.visits.see_spot(cell, day);
        }
        let by_id = f.id_key.map_or(0, |k| a.visits.id_days(k, day));
        let by_spot = cell.map_or(0, |c| a.visits.spot_days(c, day));
        let days = by_id.max(by_spot);
        if days >= visits::REPEAT_DAYS {
            let others = hits.iter().map(|h| h.level).max().unwrap_or(Level::Notice);
            let level = if others >= Level::Caution { Level::Warning } else { Level::Notice };
            let r = at(n, Rule::RepeatVisitor, level).with_secs(u64::from(days) * 1000).with_code(u8::from(by_spot > by_id));
            let _ = hits.push(r);
        }
    }
}
