//! Track continuity: keep following a drone that changes identity.
//!
//! The tracker already merges everything that shares a Basic ID, so a
//! drone that rotates its MAC (BLE does every few minutes; many drones use
//! a new Wi-Fi MAC per flight) but keeps its serial stays one track. What
//! it cannot merge is a drone that also changes its *ID*: a new session ID
//! each flight (legal and normal), or a transmitter that swaps serials
//! mid-flight to shake off a tracker (not normal).
//!
//! When a new track gets its first position, this module looks for a track
//! that went quiet just before it and whose last position, speed and
//! heading (or, between flights, last position, home or operator position)
//! fit the new one. If exactly one fits clearly, the new track records it
//! in [`Continuity::previous`]: "probably the same drone, new identity".
//! If the old one was flying and the gap is short, [`Continuity::identity_changed`]
//! is set and the track gets a [`crate::spoof::Check::IdentityChanged`] flag.
//!
//! A link is undone if the old track is heard again after the new one
//! began: two drones, not one.
//!
//! Memory: a few bytes per track; candidates are the tracks already held,
//! so nothing grows with history (a removed track simply cannot be linked
//! to any more).

use rid::message::{IdType, Location, Status, UaType};

use crate::spoof::{Check, dist_m};
use crate::{Track, TrackId};

/// The old track may overlap the new one by this much (frames from two
/// nodes arrive out of order) and still have "ended" first.
pub const OVERLAP_MS: u64 = 1_500;
/// A mid-flight identity change must happen within this gap.
pub const MID_FLIGHT_GAP_MS: u64 = 60_000;
/// Between flights (battery swap), the longest gap linked. Tracks are
/// removed after `Config::remove_after_ms` (10 min) anyway.
pub const MAX_GAP_MS: u64 = 600_000;
/// Only a track younger than this is linked (at its birth).
pub const LINK_WINDOW_MS: u64 = 30_000;
/// Least score that links.
pub const LINK_MIN: u8 = 50;
/// A runner-up this close to the best makes the link ambiguous: no link.
pub const AMBIGUOUS_MARGIN: u8 = 10;

/// Continuity state of one track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Continuity {
    /// The track this one probably continues (same drone, new identity).
    pub previous: Option<TrackId>,
    /// How sure the link is, 0-100.
    pub link_confidence: u8,
    /// The link is mid-flight and the ID changed: suspicious, unlike a new
    /// session ID or MAC between flights.
    pub identity_changed: bool,
    /// The track that continues this one.
    pub next: Option<TrackId>,
    attempts: u8,
}

impl Continuity {
    pub(crate) fn absorb(&mut self, o: &Continuity) {
        if self.previous.is_none() {
            self.previous = o.previous;
            self.link_confidence = o.link_confidence;
            self.identity_changed = o.identity_changed;
        }
        self.next = self.next.or(o.next);
    }
}

fn moving(l: &Location) -> bool {
    l.status == Status::Airborne
        || l.speed_horizontal.is_some_and(|v| v > 1.0)
        || l.height.is_some_and(|h| h > 5.0)
}

/// `tracks[i]` is about to take a frame: if it was linked to a successor
/// that has since begun, they are two drones after all.
pub(crate) fn before_frame(tracks: &mut [Track], i: usize, now: u64) {
    let Some(next) = tracks[i].continuity.next else {
        return;
    };
    let me = tracks[i].id;
    match tracks.iter().position(|t| t.id == next) {
        Some(j) if now > tracks[j].first_seen_ms + OVERLAP_MS => {
            let c = &mut tracks[j].continuity;
            if c.previous == Some(me) {
                *c = Continuity {
                    attempts: u8::MAX,
                    ..Continuity::default()
                };
                tracks[j].spoof.clear(Check::IdentityChanged);
            }
            tracks[i].continuity.next = None;
        }
        Some(_) => {}
        None => tracks[i].continuity.next = None,
    }
}

/// `tracks[i]` just took a frame: link it if it is new and positioned.
pub(crate) fn after_frame(tracks: &mut [Track], i: usize, now: u64) {
    let t = &tracks[i];
    if t.continuity.previous.is_some()
        || t.continuity.attempts >= 5
        || now.saturating_sub(t.first_seen_ms) > LINK_WINDOW_MS
    {
        return;
    }
    let Some(loc) = t.location.as_ref() else {
        return;
    };
    if loc.value.position.is_none() {
        return;
    }
    let mut best: Option<(usize, u8, bool)> = None;
    let mut second = 0u8;
    for (j, c) in tracks.iter().enumerate() {
        if j == i {
            continue;
        }
        if let Some((score, mid)) = fit(c, t) {
            match best {
                Some((_, b, _)) if score <= b => second = second.max(score),
                _ => {
                    second = second.max(best.map_or(0, |b| b.1));
                    best = Some((j, score, mid));
                }
            }
        }
    }
    tracks[i].continuity.attempts = tracks[i].continuity.attempts.saturating_add(1);
    let Some((j, score, mid)) = best else { return };
    if score < LINK_MIN || second.saturating_add(AMBIGUOUS_MARGIN) > score {
        return;
    }
    let (old_id, old_has_id, old_session) = {
        let c = &tracks[j];
        (
            c.id,
            !c.uas_ids.is_empty(),
            c.primary_id()
                .is_some_and(|u| u.id_type == IdType::SpecificSession),
        )
    };
    tracks[j].continuity.next = Some(tracks[i].id);
    let t = &mut tracks[i];
    let changed = mid && old_has_id && !t.uas_ids.is_empty();
    t.continuity.previous = Some(old_id);
    t.continuity.link_confidence = score.min(95);
    t.continuity.identity_changed = changed;
    if changed {
        let session = old_session
            && t.primary_id()
                .is_some_and(|u| u.id_type == IdType::SpecificSession);
        t.spoof.raise(
            Check::IdentityChanged,
            if session { 40 } else { 55 },
            old_id.0 as i32,
        );
    }
}

/// How well `new` continues `old`: a score and whether it is mid-flight.
fn fit(old: &Track, new: &Track) -> Option<(u8, bool)> {
    if old.continuity.next.is_some()
        || old.last_seen_ms > new.first_seen_ms + OVERLAP_MS
        || old.first_seen_ms >= new.first_seen_ms
    {
        return None;
    }
    let gap = new.first_seen_ms.saturating_sub(old.last_seen_ms);
    if gap > MAX_GAP_MS {
        return None;
    }
    // Shared IDs would have merged them; a DJI serial match likewise.
    if old.uas_ids.iter().any(|u| new.uas_ids.contains(u)) {
        return None;
    }
    let known = |u: Option<UaType>| u.filter(|t| *t != UaType::None);
    if let (Some(a), Some(b)) = (known(old.ua_type), known(new.ua_type))
        && a != b
    {
        return None;
    }
    if let (Some(a), Some(b)) = (old.dji.product_type, new.dji.product_type)
        && a != b
    {
        return None;
    }
    let ol = old.location.as_ref()?;
    let nl = new.location.as_ref()?;
    let (p0, p1) = (ol.value.position?, nl.value.position?);
    let mid = moving(&ol.value)
        && gap <= MID_FLIGHT_GAP_MS
        && old.last_seen_ms.saturating_sub(ol.rx_ms) < 10_000;

    let mut score: i32 = if mid {
        // Dead-reckon the old track to the new one's first position.
        let el = (nl.rx_ms.saturating_sub(ol.rx_ms) as f64 / 1000.0).min(60.0);
        let v = f64::from(ol.value.speed_horizontal.unwrap_or(0.0)).min(30.0);
        let pred = match ol.value.direction {
            Some(dir) if v > 0.5 => {
                let r = f64::from(dir).to_radians();
                let (e, n) = (libm::sin(r) * v * el, libm::cos(r) * v * el);
                rid::message::LatLon {
                    lat: p0.lat + n / 111_132.0,
                    lon: p0.lon + e / (111_132.0 * libm::cos(p0.lat.to_radians())),
                }
            }
            _ => p0,
        };
        let hacc = ol
            .value
            .horizontal_accuracy
            .meters()
            .map_or(30.0, f64::from)
            .min(150.0);
        let allowed = (50.0 + 2.0 * hacc + 20.0 * el).min(2_000.0);
        let d = dist_m(pred, p1);
        if d > allowed {
            return None;
        }
        if let (Some(a), Some(b)) = (ol.value.altitude_geodetic, nl.value.altitude_geodetic)
            && libm::fabs(f64::from(a - b)) > 30.0 + 10.0 * el
        {
            return None;
        }
        50 + (20.0 * (1.0 - d / allowed)) as i32
    } else {
        // Between flights: it takes off near where the old one was last,
        // its home point, or its operator.
        let near = |q: Option<rid::message::LatLon>| q.is_some_and(|q| dist_m(q, p1) <= 150.0);
        let op = old.system.as_ref().and_then(|s| s.value.operator_position);
        if !(near(Some(p0)) || near(old.home.as_ref().map(|h| h.value)) || near(op)) {
            return None;
        }
        45
    };
    let op = |t: &Track| t.system.as_ref().and_then(|s| s.value.operator_position);
    if let (Some(a), Some(b)) = (op(old), op(new)) {
        let d = dist_m(a, b);
        if d > 1_000.0 {
            return None;
        }
        if d <= 50.0 {
            score += 15;
        }
    }
    if old.nodes & new.nodes != 0
        && let (Some(a), Some(b)) = (old.latest_rssi(), new.latest_rssi())
        && (i16::from(a) - i16::from(b)).abs() <= 10
    {
        score += 10;
    }
    if old.transports().eq(new.transports()) {
        score += 5;
    }
    if old.dji.product_type.is_some() && old.dji.product_type == new.dji.product_type {
        score += 10;
    }
    Some((score.clamp(0, 100) as u8, mid))
}
