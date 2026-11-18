//! GPS jamming and spoofing **in the area**, seen through other receivers.
//!
//! The hub checks its own GPS receiver elsewhere (`gnss::integrity`,
//! docs/GNSS-INTEGRITY.md), and each drone's own broadcast for physics it
//! breaks (`spoof`, docs/SPOOFING.md). This module treats everything else
//! that reports a satellite position as one more sensor:
//!
//! - **Aircraft** (ADS-B on 1090 MHz, UAT on 978 MHz) say how much their own
//!   position can be trusted: the NIC (integrity, a containment radius) and
//!   NACp (accuracy) figures, `adsb::nicnac`. When a jammer is on, aircraft
//!   in range lose GPS and those figures fall, several aircraft at once.
//!   This is the method of gpsjam.org (John Wiseman), which maps the share
//!   of aircraft reporting low navigation accuracy per area and day.
//! - **Drones** (Remote ID, DroneID): a spoofer drags every GPS receiver in
//!   range to a false position, so several drones' reported positions jump
//!   at the same moment by about the same amount, or to the same place. A
//!   single drone jumping is that drone's problem ([`crate::spoof`]
//!   already flags it); two or more together is the area's.
//! - **Aircraft positions** jumping together in the same way (needs three).
//! - **The hub's own receiver** ([`OwnGnss`], set by the hub): agreement
//!   raises the confidence and lets one independent receiver suffice;
//!   it is never required.
//!
//! What fires is one hub-raised [`link::ThreatKind::AreaGnss`] threat per
//! level (jamming, spoofing), with the evidence in `detail`
//! ([`detail`]). docs/DRONE-ATTACKS.md "GPS interference in the area"
//! explains the rules in plain language, with thresholds, reasons and
//! false alarms.
//!
//! **What it cannot do.** One aircraft whose ADS-B position is consistently
//! wrong (spoofed alone, or lying) is invisible without multilateration
//! (timing the same message at several receivers), which this hub cannot
//! do. Drones that lose GPS altogether (jamming) mostly stop sending a
//! position; that is not used here (a drone landing looks the same).
//!
//! **Memory.** [`AreaWatch`] is about 8 KB and lives outside the tracker
//! (the P4 hub keeps it in PSRAM); the tracker only gained one byte per
//! aircraft, inside existing padding ([`AircraftTrack::nic`],
//! [`AircraftTrack::nacp`]). Fixed
//! tables, no allocation, deterministic thresholds (the constants below).

use core::fmt::Write as _;

use heapless::{String, Vec};
use libm::{cos, sqrt};
use link::{ThreatKind, ThreatSighting};
use serde::Serialize;

use crate::aircraft::{AircraftKey, AircraftTrack};
use crate::threats::{Threat, ThreatId};
use crate::{TrackId, TrackState, Tracker, geo};

// ------------------------------------------------------------ thresholds --

/// NIC at or above this is "good": containment radius under 0.2 NM
/// (370 m). The US ADS-B Out rule requires it (14 CFR 91.227(c)(1)(iii)),
/// so a correctly equipped aircraft with working GPS always reports it.
pub const NIC_GOOD: u8 = 7;
/// NACp at or above this is "good": 95 % error under 0.05 NM (93 m), the
/// same rule's requirement (14 CFR 91.227(c)(1)(i)).
pub const NACP_GOOD: u8 = 8;
/// An aircraft counts only if heard this recently, and its NIC only if its
/// position was updated this recently.
pub const AIR_FRESH_MS: u64 = 10_000;
/// A low figure must last this long before the aircraft counts as
/// degraded (one odd message is not jamming).
pub const AIR_HOLD_MS: u64 = 5_000;
/// "Dropped": degraded now, and reported good within this long. A
/// per-aircraft drop is far stronger evidence than a low figure, because
/// some old transponders report a low figure all the time.
pub const AIR_GOOD_MEMORY_MS: u64 = 15 * 60_000;
/// Position lost: an airborne aircraft within [`AIR_LOST_RANGE_M`] of the
/// hub, still heard, whose position has not been updated for this long
/// (aircraft that lose GPS often stop sending a position at all).
pub const AIR_POS_LOST_MS: u64 = 20_000;
/// ... only this close, where its position messages are reliably heard;
/// farther out, missing positions are just weak reception.
pub const AIR_LOST_RANGE_M: f64 = 50_000.0;
/// Jamming from aircraft alone: at least this many dropped aircraft ...
pub const AIR_MIN_DROPPED: usize = 2;
/// ... and, gpsjam.org style, `(degraded - 1) / counted` at least this
/// share (gpsjam.org colours an area red above 10 % low-accuracy aircraft,
/// after subtracting one aircraft to limit false alarms).
pub const AIR_BAD_RATIO: f32 = 0.10;
/// An aircraft position more than this off where its own previous
/// position, speed and track put it is a jump.
pub const AIR_JUMP_M: f64 = 2_000.0;
/// ... judged only between positions at most this far apart in time.
pub const AIR_JUMP_MAX_DT_MS: u64 = 30_000;
/// Aircraft jumping alike within [`JUMP_WINDOW_MS`] needed for spoofing.
pub const AIR_MIN_JUMPERS: usize = 3;
/// Two aircraft jumps are alike if their displacements differ by at most
/// this much, or [`JUMP_ALIKE_FRAC`] of the larger, ...
pub const AIR_ALIKE_M: f64 = 1_000.0;
/// ... or they landed within this distance of each other.
pub const AIR_SAME_PLACE_M: f64 = 3_000.0;
/// A drone track's position move counts only if [`crate::spoof`] counted
/// a jump and it moved at least this far between two looks.
pub const DRONE_MIN_JUMP_M: f64 = 60.0;
/// Drones jumping alike within [`JUMP_WINDOW_MS`] needed for spoofing.
pub const DRONE_MIN_JUMPERS: usize = 2;
/// Two drone jumps are alike if their displacements differ by at most this
/// much, or [`JUMP_ALIKE_FRAC`] of the larger, ...
pub const DRONE_ALIKE_M: f64 = 75.0;
/// ... or they landed within this distance of each other.
pub const DRONE_SAME_PLACE_M: f64 = 150.0;
/// Displacements within this fraction of the larger one are alike.
pub const JUMP_ALIKE_FRAC: f64 = 0.25;
/// Jumps this close in time are "at the same time" (looks are once a
/// second; drones report about once a second; aircraft positions arrive
/// every half second or so but the ADSBee reports once a second).
pub const JUMP_WINDOW_MS: u64 = 10_000;
/// A verdict stays raised this long after its evidence was last seen, so
/// a short spoofing episode stays on the screen and in the log.
pub const LATCH_MS: u64 = 60_000;
/// The hub's own GPS verdict counts as "now" for this long after it was set.
pub const OWN_FRESH_MS: u64 = 5_000;
/// Aircraft and drone tracks followed.
pub const MAX_AIR: usize = 48;
pub const MAX_DRONES: usize = 64;
/// Recent jumps remembered (each kind).
pub const MAX_JUMPS: usize = 16;
/// GPS L1, 100 kHz units: the `freq` of the threat.
pub const L1_FREQ: u32 = 15_754;

/// `detail` bits of an [`ThreatKind::AreaGnss`] threat.
///
/// | bits | meaning |
/// |---|---|
/// | 0 | jamming (aircraft lost integrity) |
/// | 1 | spoofing (positions moved together) |
/// | 2 | evidence: aircraft integrity (NIC / NACp / position lost) |
/// | 3 | evidence: aircraft positions jumped together |
/// | 4 | evidence: drone positions jumped together |
/// | 5 | the hub's own GPS agrees (its own jamming / spoofing check) |
/// | 8-15 | jamming: aircraft degraded now; spoofing: aircraft that jumped together |
/// | 16-23 | jamming: aircraft counted; spoofing: drones that jumped together |
/// | 24-31 | jamming: aircraft that dropped from good; spoofing: the jump, 100 m units (255 = 25.5 km or more) |
///
/// The threat's `level` is a confidence 0-100, `count` the number of
/// receivers behind it (aircraft and drones, plus one for the hub's own),
/// `target[0]` 1 for jamming or 2 for spoofing (so the two are separate
/// threats with separate alerts), `freq` GPS L1.
pub mod detail {
    pub const JAMMING: u32 = 1 << 0;
    pub const SPOOFING: u32 = 1 << 1;
    pub const AIR_INTEGRITY: u32 = 1 << 2;
    pub const AIR_JUMPS: u32 = 1 << 3;
    pub const DRONE_JUMPS: u32 = 1 << 4;
    pub const OWN_AGREES: u32 = 1 << 5;

    pub const fn byte(d: u32, n: u32) -> u8 {
        (d >> (8 * n)) as u8
    }
}

/// The hub's own GPS integrity verdict, as `gnss::integrity::State` gives
/// it (the tracker does not depend on `gnss`; the hub maps one to the other).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnGnss {
    // Same order as `gnss::integrity::State`, so the hub's mapping is a
    // plain copy (no lookup table in scarce RAM).
    Ok,
    SuspectJamming,
    Jamming,
    SuspectSpoofing,
    Spoofing,
    /// No receiver, or no verdict yet.
    #[default]
    Unknown,
}

impl OwnGnss {
    fn jamming(self) -> bool {
        matches!(self, OwnGnss::SuspectJamming | OwnGnss::Jamming)
    }
    fn spoofing(self) -> bool {
        matches!(self, OwnGnss::SuspectSpoofing | OwnGnss::Spoofing)
    }
}

// ----------------------------------------------------------------- state --

#[derive(Debug, Clone, Copy, PartialEq)]
struct Pt {
    lat: f64,
    lon: f64,
}

/// East and north metres from `a` to `b` (flat earth: jumps are compared
/// over at most a few tens of km).
fn en(a: Pt, b: Pt) -> (f64, f64) {
    const M_PER_DEG: f64 = 111_320.0;
    let mid = ((a.lat + b.lat) / 2.0).to_radians();
    ((b.lon - a.lon) * M_PER_DEG * cos(mid), (b.lat - a.lat) * M_PER_DEG)
}

fn offset(p: Pt, east_m: f64, north_m: f64) -> Pt {
    const M_PER_DEG: f64 = 111_320.0;
    Pt { lat: p.lat + north_m / M_PER_DEG, lon: p.lon + east_m / (M_PER_DEG * cos(p.lat.to_radians())) }
}

fn norm((e, n): (f64, f64)) -> f64 {
    sqrt(e * e + n * n)
}

/// One position that moved further than it should have.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Jump {
    at_ms: u64,
    /// Which receiver (aircraft address or drone track id).
    who: u32,
    /// Where it landed.
    to: Pt,
    /// East, north metres: the unexplained part of the move.
    d: (f64, f64),
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Air {
    key: AircraftKey,
    /// Hub time of the last look at it.
    seen_ms: u64,
    /// Last look at which its figures were good.
    good_ms: Option<u64>,
    /// Since when its figures have been low (cleared when good again).
    degraded_since: Option<u64>,
    /// The position (and its hub time) at the previous look.
    pos: Option<(Pt, u64)>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Drone {
    id: TrackId,
    seen_ms: u64,
    pos: Option<Pt>,
    jumps: u8,
}

/// One verdict (jamming or spoofing) and when its evidence was last seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Verdict {
    detail: u32,
    level: u8,
    count: u32,
    last_ms: u64,
    on: bool,
}

/// What the area check sees right now, for the screen, the JSON stream and
/// the health page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AreaStatus {
    /// Jamming verdict raised (latched for [`LATCH_MS`]).
    pub jamming: bool,
    pub spoofing: bool,
    /// Confidences, 0-100 (0 when not raised).
    pub jam_level: u8,
    pub spoof_level: u8,
    /// Latest `detail` of each verdict ([`detail`]).
    pub jam_detail: u32,
    pub spoof_detail: u32,
    /// Aircraft counted right now (airborne, fresh, with integrity figures).
    pub counted: u8,
    /// Of those, degraded for [`AIR_HOLD_MS`] or more ...
    pub degraded: u8,
    /// ... and of those, reported good within [`AIR_GOOD_MEMORY_MS`].
    pub dropped: u8,
    /// Largest groups of alike jumps in the last [`JUMP_WINDOW_MS`].
    pub air_jumpers: u8,
    pub drone_jumpers: u8,
    /// The hub's own verdict as last set.
    pub own: OwnGnss,
}

/// Watches aircraft integrity and drone / aircraft position jumps, and
/// raises [`ThreatKind::AreaGnss`] threats in the tracker. Call
/// [`AreaWatch::tick`] about once a second.
#[derive(Debug, Clone)]
pub struct AreaWatch {
    air: Vec<Air, MAX_AIR>,
    drones: Vec<Drone, MAX_DRONES>,
    air_jumps: Vec<Jump, MAX_JUMPS>,
    drone_jumps: Vec<Jump, MAX_JUMPS>,
    own: OwnGnss,
    own_ms: u64,
    /// Last time the own receiver said jamming / spoofing.
    own_jam_ms: Option<u64>,
    own_spoof_ms: Option<u64>,
    jam: Verdict,
    spoof: Verdict,
    status: AreaStatus,
}

impl Default for AreaWatch {
    fn default() -> Self {
        Self::new()
    }
}

fn push_jump(v: &mut Vec<Jump, MAX_JUMPS>, j: Jump) {
    if v.is_full() {
        v.remove(0);
    }
    let _ = v.push(j);
}

/// Largest set of jumps from distinct receivers that are all alike the
/// same seed jump, and their mean displacement (metres).
fn alike_group(jumps: &[Jump], now: u64, alike_m: f64, same_place_m: f64) -> (usize, f64) {
    let recent = |j: &&Jump| now.saturating_sub(j.at_ms) <= JUMP_WINDOW_MS;
    let mut best = (0usize, 0.0f64);
    for seed in jumps.iter().filter(recent) {
        let mut who: Vec<u32, MAX_JUMPS> = Vec::new();
        let mut sum = 0.0;
        for j in jumps.iter().filter(recent) {
            if who.contains(&j.who) {
                continue;
            }
            let diff = norm((j.d.0 - seed.d.0, j.d.1 - seed.d.1));
            let tol = alike_m.max(JUMP_ALIKE_FRAC * norm(j.d).max(norm(seed.d)));
            let same_place = norm(en(j.to, seed.to)) <= same_place_m;
            if diff <= tol || same_place {
                let _ = who.push(j.who);
                sum += norm(j.d);
            }
        }
        if who.len() > best.0 {
            best = (who.len(), sum / who.len() as f64);
        }
    }
    best
}

impl AreaWatch {
    pub const fn new() -> Self {
        Self {
            air: Vec::new(),
            drones: Vec::new(),
            air_jumps: Vec::new(),
            drone_jumps: Vec::new(),
            own: OwnGnss::Unknown,
            own_ms: 0,
            own_jam_ms: None,
            own_spoof_ms: None,
            jam: Verdict { detail: 0, level: 0, count: 0, last_ms: 0, on: false },
            spoof: Verdict { detail: 0, level: 0, count: 0, last_ms: 0, on: false },
            status: AreaStatus {
                jamming: false,
                spoofing: false,
                jam_level: 0,
                spoof_level: 0,
                jam_detail: 0,
                spoof_detail: 0,
                counted: 0,
                degraded: 0,
                dropped: 0,
                air_jumpers: 0,
                drone_jumpers: 0,
                own: OwnGnss::Unknown,
            },
        }
    }

    /// The hub's own GPS verdict (once a second while known).
    pub fn set_own_gnss(&mut self, v: OwnGnss, now_ms: u64) {
        self.own = v;
        self.own_ms = now_ms;
        if v.jamming() {
            self.own_jam_ms = Some(now_ms);
        }
        if v.spoofing() {
            self.own_spoof_ms = Some(now_ms);
        }
    }

    /// What the last [`AreaWatch::tick`] saw.
    pub fn status(&self) -> &AreaStatus {
        &self.status
    }

    /// Look at the tracker's aircraft and drones, decide, and raise or
    /// refresh the threats. Returns the threats touched (jamming,
    /// spoofing), for a caller that wants them; new ones are also handed
    /// out by [`Tracker::take_new_threat`] as usual.
    pub fn tick<const N: usize, const A: usize>(
        &mut self,
        tr: &mut Tracker<N, A>,
        now_ms: u64,
    ) -> (Option<ThreatId>, Option<ThreatId>) {
        let own_pos = tr.own_position(now_ms);
        let (counted, degraded, dropped) = self.look_aircraft(tr.aircraft(), own_pos.map(|o| Pt { lat: o.lat, lon: o.lon }), now_ms);
        self.look_drones(tr, now_ms);
        let (air_n, air_d) = alike_group(&self.air_jumps, now_ms, AIR_ALIKE_M, AIR_SAME_PLACE_M);
        let (dr_n, dr_d) = alike_group(&self.drone_jumps, now_ms, DRONE_ALIKE_M, DRONE_SAME_PLACE_M);
        let own_fresh = now_ms.saturating_sub(self.own_ms) <= OWN_FRESH_MS;
        let own_jam = own_fresh && self.own.jamming();
        let own_spoof = self.own_spoof_ms.is_some_and(|t| now_ms.saturating_sub(t) <= JUMP_WINDOW_MS);

        // ---- jamming
        let ratio_ok = counted > 0 && (degraded.saturating_sub(1)) as f32 / counted as f32 >= AIR_BAD_RATIO;
        let air_jam = dropped >= AIR_MIN_DROPPED && ratio_ok;
        let corroborated_jam = own_jam && dropped >= 1;
        if air_jam || corroborated_jam {
            let mut d = detail::JAMMING | detail::AIR_INTEGRITY;
            let mut level = if air_jam { 50 + 10 * (dropped - AIR_MIN_DROPPED).min(3) as u8 } else { 45 };
            if own_jam {
                d |= detail::OWN_AGREES;
                level += 20;
            }
            d |= u32::from(clamp8(degraded)) << 8 | u32::from(clamp8(counted)) << 16 | u32::from(clamp8(dropped)) << 24;
            self.jam = Verdict { detail: d, level: level.min(95), count: (dropped + usize::from(own_jam)) as u32, last_ms: now_ms, on: true };
        }

        // ---- spoofing
        let drone_spoof = dr_n >= DRONE_MIN_JUMPERS;
        let air_spoof = air_n >= AIR_MIN_JUMPERS;
        let own_drone = own_spoof && dr_n >= 1;
        if drone_spoof || air_spoof || own_drone {
            let mut d = detail::SPOOFING;
            let mut level: u8 = 0;
            let mut count = 0;
            let mut jump_m: f64 = 0.0;
            if drone_spoof || own_drone {
                d |= detail::DRONE_JUMPS;
                level = level.max(if drone_spoof { 60 + 10 * (dr_n - DRONE_MIN_JUMPERS).min(3) as u8 } else { 50 });
                count += dr_n;
                jump_m = jump_m.max(dr_d);
            }
            if air_spoof {
                d |= detail::AIR_JUMPS;
                level = level.max(60 + 10 * (air_n - AIR_MIN_JUMPERS).min(3) as u8);
                count += air_n;
                jump_m = jump_m.max(air_d);
            }
            if air_spoof && (drone_spoof || own_drone) {
                level += 15;
            }
            if own_spoof {
                d |= detail::OWN_AGREES;
                count += 1;
                if drone_spoof || air_spoof {
                    level += 20;
                }
            }
            let jump_100m = (jump_m / 100.0).clamp(0.0, 255.0) as u32;
            d |= u32::from(clamp8(air_n)) << 8 | u32::from(clamp8(dr_n)) << 16 | jump_100m << 24;
            self.spoof = Verdict { detail: d, level: level.min(95), count: count as u32, last_ms: now_ms, on: true };
        }

        let jam_id = self.raise(tr, true, now_ms);
        let spoof_id = self.raise(tr, false, now_ms);
        self.status = AreaStatus {
            jamming: self.jam.on,
            spoofing: self.spoof.on,
            jam_level: if self.jam.on { self.jam.level } else { 0 },
            spoof_level: if self.spoof.on { self.spoof.level } else { 0 },
            jam_detail: self.jam.detail,
            spoof_detail: self.spoof.detail,
            counted: clamp8(counted),
            degraded: clamp8(degraded),
            dropped: clamp8(dropped),
            air_jumpers: clamp8(air_n),
            drone_jumpers: clamp8(dr_n),
            own: if own_fresh { self.own } else { OwnGnss::Unknown },
        };
        (jam_id, spoof_id)
    }

    /// Refresh one verdict's threat while it is latched.
    fn raise<const N: usize, const A: usize>(&mut self, tr: &mut Tracker<N, A>, jamming: bool, now_ms: u64) -> Option<ThreatId> {
        let v = if jamming { &mut self.jam } else { &mut self.spoof };
        if !v.on {
            return None;
        }
        if now_ms.saturating_sub(v.last_ms) > LATCH_MS {
            v.on = false;
            return None;
        }
        let mut target = [0u8; 6];
        target[0] = if jamming { 1 } else { 2 };
        let s = ThreatSighting {
            kind: ThreatKind::AreaGnss,
            channel: 0,
            freq: L1_FREQ,
            width_khz: 0,
            level: v.level.min(100) as i8,
            count: v.count,
            window_ms: if jamming { AIR_HOLD_MS as u32 } else { JUMP_WINDOW_MS as u32 },
            source: [0; 6],
            target,
            detail: v.detail,
            ssid: String::new(),
            uptime_us: now_ms.saturating_mul(1000),
        };
        tr.ingest_hub_threat(&s, now_ms)
    }

    /// Update the per-aircraft state; returns (counted, degraded, dropped).
    fn look_aircraft(&mut self, aircraft: &[AircraftTrack], own: Option<Pt>, now: u64) -> (usize, usize, usize) {
        // Forget aircraft the tracker dropped, and old jumps.
        self.air.retain(|s| aircraft.iter().any(|a| a.key == s.key));
        self.air_jumps.retain(|j| now.saturating_sub(j.at_ms) <= JUMP_WINDOW_MS);
        let (mut counted, mut degraded, mut dropped) = (0, 0, 0);
        for a in aircraft {
            // Only ADS-B and UAT broadcasts from the aircraft itself carry
            // its own GPS figures: not FLARM / OGN, not TIS-B (a ground
            // station relaying radar tracks), not ground vehicles.
            let from_aircraft = matches!(a.key.source, adsb::Source::ModeS | adsb::Source::Uat)
                && matches!(a.address_qualifier, 0 | 1);
            if !from_aircraft {
                continue;
            }
            let i = match self.air.iter().position(|s| s.key == a.key) {
                Some(i) => i,
                None => {
                    if self.air.is_full() {
                        // Make room: the one looked at longest ago.
                        if let Some(j) = (0..self.air.len()).min_by_key(|&j| self.air[j].seen_ms) {
                            self.air.swap_remove(j);
                        }
                    }
                    if self.air.push(Air { key: a.key, seen_ms: now, good_ms: None, degraded_since: None, pos: None }).is_err() {
                        continue;
                    }
                    self.air.len() - 1
                }
            };
            let s = &mut self.air[i];
            s.seen_ms = now;

            // Jumps: compare with where it should be by now.
            if let Some(p) = &a.position {
                let here = Pt { lat: p.value.lat, lon: p.value.lon };
                if let Some((prev, prev_ms)) = s.pos
                    && p.rx_ms > prev_ms
                {
                    let dt_ms = p.rx_ms - prev_ms;
                    if dt_ms <= AIR_JUMP_MAX_DT_MS {
                        let dt = dt_ms as f64 / 1000.0;
                        let predicted = match (a.speed_mps(), a.direction_deg) {
                            (Some(v), Some(dir)) => {
                                let r = f64::from(dir).to_radians();
                                offset(prev, v * dt * libm::sin(r), v * dt * cos(r))
                            }
                            _ => prev,
                        };
                        let d = en(predicted, here);
                        if norm(d) > AIR_JUMP_M {
                            push_jump(&mut self.air_jumps, Jump { at_ms: now, who: a.key.icao, to: here, d });
                        }
                    }
                }
                if s.pos.is_none_or(|(_, t)| p.rx_ms > t) {
                    s.pos = Some((here, p.rx_ms));
                }
            }

            // Integrity: only airborne aircraft heard recently, with figures.
            let heard = now.saturating_sub(a.last_seen_ms) <= AIR_FRESH_MS;
            if !a.airborne || !heard || a.nic().is_none() {
                s.degraded_since = None;
                continue;
            }
            let pos_age = a.position.as_ref().map_or(u64::MAX, |p| now.saturating_sub(p.rx_ms));
            let near = match (own, &a.position) {
                (Some(o), Some(p)) => geo::range_bearing(o.lat, o.lon, p.value.lat, p.value.lon).distance_m <= AIR_LOST_RANGE_M,
                _ => false,
            };
            let lost = pos_age >= AIR_POS_LOST_MS && near;
            if pos_age > AIR_FRESH_MS && !lost {
                // Neither a current figure nor a clear loss: not counted.
                s.degraded_since = None;
                continue;
            }
            counted += 1;
            let nic_low = pos_age <= AIR_FRESH_MS && a.nic().is_some_and(|n| n < NIC_GOOD);
            let nacp_low = a.nacp().is_some_and(|n| n < NACP_GOOD);
            if lost || nic_low || nacp_low {
                let since = *s.degraded_since.get_or_insert(now);
                if now.saturating_sub(since) >= AIR_HOLD_MS {
                    degraded += 1;
                    if s.good_ms.is_some_and(|g| now.saturating_sub(g) <= AIR_GOOD_MEMORY_MS) {
                        dropped += 1;
                    }
                }
            } else {
                s.degraded_since = None;
                s.good_ms = Some(now);
            }
        }
        (counted, degraded, dropped)
    }

    /// Notice drone tracks whose jump count rose since the last look.
    fn look_drones<const N: usize, const A: usize>(&mut self, tr: &Tracker<N, A>, now: u64) {
        let ctx = tr.context(now, None);
        self.drones.retain(|d| tr.get(d.id).is_some());
        self.drone_jumps.retain(|j| now.saturating_sub(j.at_ms) <= JUMP_WINDOW_MS);
        for t in tr.tracks() {
            if ctx.state(t) != TrackState::Active {
                continue;
            }
            let Some(p) = t.location.as_ref().and_then(|l| l.value.position) else { continue };
            let here = Pt { lat: p.lat, lon: p.lon };
            let jumps = t.spoof.jumps();
            match self.drones.iter_mut().find(|d| d.id == t.id) {
                Some(d) => {
                    if jumps > d.jumps
                        && let Some(prev) = d.pos
                    {
                        let v = en(prev, here);
                        if norm(v) >= DRONE_MIN_JUMP_M {
                            push_jump(&mut self.drone_jumps, Jump { at_ms: now, who: t.id.0, to: here, d: v });
                        }
                    }
                    d.jumps = jumps;
                    d.pos = Some(here);
                    d.seen_ms = now;
                }
                None => {
                    if self.drones.is_full()
                        && let Some(j) = (0..self.drones.len()).min_by_key(|&j| self.drones[j].seen_ms)
                    {
                        self.drones.swap_remove(j);
                    }
                    let _ = self.drones.push(Drone { id: t.id, seen_ms: now, pos: Some(here), jumps });
                }
            }
        }
    }
}

fn clamp8(n: usize) -> u8 {
    n.min(255) as u8
}

/// A plain-English sentence for an [`ThreatKind::AreaGnss`] threat (the
/// evidence log's text, the console), from its `detail`.
pub fn describe(t: &Threat) -> String<192> {
    let d = t.detail;
    let mut s: String<192> = String::new();
    let b = |n| detail::byte(d, n);
    if d & detail::JAMMING != 0 {
        let _ = write!(
            s,
            "GPS jamming in the area: {} of {} aircraft report degraded GPS ({} had been good)",
            b(1),
            b(2),
            b(3)
        );
    } else {
        let _ = s.push_str("GPS spoofing in the area: ");
        let mut parts = 0;
        if d & detail::DRONE_JUMPS != 0 {
            let _ = write!(s, "{} drone(s)", b(2));
            parts += 1;
        }
        if d & detail::AIR_JUMPS != 0 {
            let _ = write!(s, "{}{} aircraft", if parts > 0 { " and " } else { "" }, b(1));
        }
        let _ = write!(s, " jumped together by about {:.1} km", f32::from(b(3)) / 10.0);
    }
    if d & detail::OWN_AGREES != 0 {
        let _ = s.push_str("; the hub's own GPS agrees");
    }
    let _ = write!(s, " (confidence {}%)", t.level.max(0));
    s
}

// ------------------------------------------------------------------ json --

#[derive(Serialize)]
struct StatusLine {
    t: &'static str,
    jamming: bool,
    spoofing: bool,
    jam_level: u8,
    spoof_level: u8,
    jam_detail: u32,
    spoof_detail: u32,
    counted: u8,
    degraded: u8,
    dropped: u8,
    air_jumpers: u8,
    drone_jumpers: u8,
    own: OwnGnss,
}

/// Longest `{"t":"area_gnss"}` line.
pub const MAX_STATUS_LINE: usize = 384;

/// `{"t":"area_gnss",...}\n`: the area check's current view (counts and
/// verdicts; see [`AreaStatus`]).
pub fn status_line(s: &AreaStatus, buf: &mut [u8]) -> Result<usize, crate::text::FormatError> {
    let line = StatusLine {
        t: "area_gnss",
        jamming: s.jamming,
        spoofing: s.spoofing,
        jam_level: s.jam_level,
        spoof_level: s.spoof_level,
        jam_detail: s.jam_detail,
        spoof_detail: s.spoof_detail,
        counted: s.counted,
        degraded: s.degraded,
        dropped: s.dropped,
        air_jumpers: s.air_jumpers,
        drone_jumpers: s.drone_jumpers,
        own: s.own,
    };
    let n = serde_json_core::to_slice(&line, buf)
        .map_err(|_| crate::text::FormatError::BufferTooSmall)?;
    let nl = buf.get_mut(n).ok_or(crate::text::FormatError::BufferTooSmall)?;
    *nl = b'\n';
    Ok(n + 1)
}
