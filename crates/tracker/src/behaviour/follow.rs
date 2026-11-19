//! Rule 11, **following** (slice 16b): a drone that keeps near a handheld
//! unit while it travels. For the bubble only (a fixed area does not move).
//!
//! The drone's path (one point every five seconds, [`super::path`]) is laid
//! beside the unit's own trail ([`OwnTrail`]) and walked back from now.
//! It counts as following once, for some moment in that walk:
//! - the unit has since moved at least [`FOLLOW_TRAVEL_M`] (straight-line,
//!   so pacing up and down a street does not add up);
//! - the drone stayed within the range (the bubble's radius, at least
//!   [`FOLLOW_MIN_RANGE_M`]) of the unit at every point since (one wild
//!   position forgiven);
//! - and the drone itself moved the same way, at least [`FOLLOW_KEEP_UP`]
//!   as far, within [`FOLLOW_MAX_ANGLE_DEG`] of the unit's direction.
//!
//! The last condition is what keeps out a drone the user simply walks past,
//! or one hovering where the user walks: that drone does not move with
//! them.

use libm::sqrtf;

use super::linger::DWELL_GAP_MS;
use super::path::{OwnTrail, PathRing};
use super::places::Frame;
use crate::OwnFix;

/// The unit must have travelled at least this far with the drone near.
pub const FOLLOW_TRAVEL_M: f32 = 200.0;
/// The range the drone must stay within is the bubble's radius, but never
/// less than this (positions are only good to about 30 m, and a follow-me
/// drone flies tens of metres behind).
pub const FOLLOW_MIN_RANGE_M: f32 = 100.0;
/// The drone must itself have moved at least this share of the unit's
/// travel ...
pub const FOLLOW_KEEP_UP: f32 = 0.5;
/// ... in about the same direction.
pub const FOLLOW_MAX_ANGLE_DEG: f32 = 45.0;
/// A drone position is matched with the unit's own position closest in
/// time, at most this far apart.
pub const FOLLOW_MATCH_MS: u64 = 5_000;

/// Following: how far the unit has travelled with the drone near, and for
/// how long.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Follow {
    pub travelled_m: f32,
    pub ms: u64,
}

/// Whether the drone (its path `ring`, now at `drone_now`) has been
/// following the unit (now at `own`, earlier along `trail`), staying within
/// `range_m`.
#[inline(never)]
pub fn following<const P: usize>(
    ring: &PathRing,
    trail: &OwnTrail<P>,
    drone_now: (f64, f64),
    own: &OwnFix,
    range_m: f32,
    now_ms: u64,
) -> Option<Follow> {
    let frame = Frame::at(own.lat, own.lon);
    let len = |v: (f32, f32)| sqrtf(v.0 * v.0 + v.1 * v.1);
    let d_now = frame.xy(drone_now.0, drone_now.1);
    if len(d_now) > range_m || trail.is_empty() {
        return None;
    }
    let mut j = 0usize;
    let mut prev = ring.last_ms()?;
    let mut misses = 0;
    let mut best: Option<Follow> = None;
    for (t, p) in ring.coarse().rev() {
        if prev.saturating_sub(t) > DWELL_GAP_MS {
            break;
        }
        prev = t;
        // The unit's own point closest in time (both run newest first).
        while let (Some(a), Some(b)) = (trail.get(j), trail.get(j + 1)) {
            if b.ms().abs_diff(t) <= a.ms().abs_diff(t) {
                j += 1;
            } else {
                break;
            }
        }
        let Some(o) = trail.get(j) else { break };
        if o.ms().abs_diff(t) > FOLLOW_MATCH_MS {
            // The unit had no fix then.
            break;
        }
        let u = frame.xy(o.lat(), o.lon());
        let q = frame.xy(p.lat(), p.lon());
        if len((q.0 - u.0, q.1 - u.1)) > range_m {
            misses += 1;
            if misses >= 2 {
                break;
            }
            continue;
        }
        misses = 0;
        // The unit went from `u` to here (the middle of the frame); the
        // drone from `q` to `d_now`.
        let unit = (-u.0, -u.1);
        let drone = (d_now.0 - q.0, d_now.1 - q.1);
        let travelled = len(unit);
        if travelled >= FOLLOW_TRAVEL_M
            && len(drone) >= FOLLOW_KEEP_UP * travelled
            && super::angle_deg(unit, drone) <= FOLLOW_MAX_ANGLE_DEG
            && best.is_none_or(|b| travelled > b.travelled_m)
        {
            best = Some(Follow { travelled_m: travelled, ms: now_ms.saturating_sub(t) });
        }
    }
    best
}

/// How long the unit has stayed within `radius_m` of where it is now (its
/// own trail walked back from now). The bubble moves with the unit, so a
/// drone counts as loitering at the bubble only for as long as the unit
/// itself has been standing about: a drone hovering where the user walks
/// is not loitering at them (and one that keeps up is rule 11).
pub fn own_still_ms<const P: usize>(trail: &OwnTrail<P>, own: &OwnFix, radius_m: f32) -> u64 {
    let frame = Frame::at(own.lat, own.lon);
    let mut earliest = own.fix_ms;
    let mut i = 0;
    while let Some(o) = trail.get(i) {
        let (x, y) = frame.xy(o.lat(), o.lon());
        if x * x + y * y > radius_m * radius_m {
            break;
        }
        earliest = earliest.min(o.ms());
        i += 1;
    }
    own.fix_ms.saturating_sub(earliest)
}
