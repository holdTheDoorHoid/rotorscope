//! The tracker under a fake-drone flood (WP-09, docs/SPOOFING.md "Floods").
//!
//! The attacker is anyone in radio range sending well-formed Remote ID with
//! any field values at any rate: one ESP32 makes hundreds of new drone IDs a
//! second with random serials and addresses, every one claiming to sit on top
//! of the hub. These tests check that real drones keep their tracks, their
//! ids, their history and their single alert through that, that a new real
//! drone is still announced, and that the operator is told about the flood.

extern crate std;

use crate::tests::{CITY_HALL, det, location, serial};
use crate::*;
use rid::Transport;

/// 200 fake IDs a second: one every 5 ms.
const FAKE_EVERY_MS: u64 = 5;

/// Fake number `i`: its own serial and its own random-looking address, at
/// the hub.
fn fake(i: u32) -> link::Detection {
    let h = i.wrapping_mul(2_654_435_761);
    let mac = [0x02, (h >> 24) as u8, (h >> 16) as u8, (h >> 8) as u8, h as u8, (i & 0xff) as u8];
    det(
        Transport::WifiBeacon,
        mac,
        -40,
        &[serial(&std::format!("FAKE{i:012}")), location(CITY_HALL.0, CITY_HALL.1, 60.0)],
    )
}

/// A real drone 800 m north of the hub, its own serial and address.
fn real(n: u8) -> link::Detection {
    det(
        Transport::WifiBeacon,
        [0x60, 0x60, 0x1f, 0x00, 0x00, n],
        -70,
        &[
            serial(&std::format!("REAL-DRONE-{n}")),
            location(CITY_HALL.0 + 0.0072 + f64::from(n) * 0.001, CITY_HALL.1, 120.0),
        ],
    )
}

fn track_of<const N: usize, const A: usize>(tr: &Tracker<N, A>, n: u8) -> Option<&Track> {
    let want = std::format!("REAL-DRONE-{n}");
    tr.tracks()
        .iter()
        .find(|t| t.uas_ids.iter().any(|u| u.id.as_str() == Some(want.as_str())))
}

/// Run `ms` of a flood at 200 new IDs/s while real drones `reals` send once
/// a second (at `phase` ms into each second). Calls `each` after every real
/// frame. Returns the alerts handed out, by track id.
fn run_flood(
    tr: &mut Tracker,
    from_ms: u64,
    ms: u64,
    next_fake: &mut u32,
    reals: &[u8],
    alerts: &mut std::vec::Vec<TrackId>,
    mut each: impl FnMut(&Tracker, u64),
) {
    let mut t = from_ms;
    while t < from_ms + ms {
        tr.ingest(0, &fake(*next_fake), t);
        *next_fake += 1;
        if t % 1000 == 0 {
            for &n in reals {
                tr.ingest(1, &real(n), t + u64::from(n));
                each(tr, t + u64::from(n));
            }
        }
        while let Some(id) = tr.take_new_drone(t) {
            alerts.push(id);
        }
        if t % 1000 == 0 {
            tr.expire(t);
        }
        t += FAKE_EVERY_MS;
    }
}

/// A real drone already tracked when a 60 s flood starts keeps its track
/// id, its history and its one alert; no fake ever gets an alert; the
/// operator is told about the flood.
#[test]
fn real_drone_survives_a_60_s_flood_of_200_ids_a_second() {
    let mut tr: Tracker = Tracker::default();
    tr.set_own_position(CITY_HALL.0, CITY_HALL.1, Some(10.0), 0);
    let mut alerts = std::vec::Vec::new();
    // Five quiet seconds first: the drone is confirmed and announced.
    let mut t = 0;
    while t < 5_000 {
        tr.ingest(1, &real(1), t);
        while let Some(id) = tr.take_new_drone(t) {
            alerts.push(id);
        }
        t += 1_000;
    }
    let id = track_of(&tr, 1).unwrap().id;
    let first = track_of(&tr, 1).unwrap().first_seen_ms;
    assert_eq!(alerts, [id], "announced once before the flood");

    let mut next = 0;
    let mut frames_before = track_of(&tr, 1).unwrap().link(Transport::WifiBeacon).count;
    run_flood(&mut tr, 5_000, 60_000, &mut next, &[1], &mut alerts, |tr, now| {
        tr_own(tr, now);
        let r = track_of(tr, 1).expect("the real drone is never evicted");
        assert_eq!(r.id, id, "same track id at {now}");
        assert_eq!(r.first_seen_ms, first, "history kept at {now}");
        let n = r.link(Transport::WifiBeacon).count;
        assert_eq!(n, frames_before + 1, "every real frame lands on the same track");
        frames_before = n;
    });
    assert_eq!(next, 12_000);
    assert_eq!(alerts, [id], "no second alert for the real drone and none for a fake");
    let r = track_of(&tr, 1).unwrap();
    assert!(r.confirmed());
    // The operator is told: a flood threat, active, with the rate seen.
    let flood = tr
        .active_threats(65_000)
        .find(|t| t.kind == link::ThreatKind::IdFlood)
        .expect("flood alarm raised");
    assert_eq!(flood.label(), "Flood of new drone IDs");
    let rate = crate::probation::flood_rate(flood);
    assert!((150..=210).contains(&rate), "about 200 new IDs a second: {rate}");
    // No fake ever displaced a confirmed track: every confirmed track is
    // the real one.
    assert_eq!(tr.tracks().iter().filter(|t| t.confirmed()).count(), 1);
}

fn tr_own(_tr: &Tracker, _now: u64) {}

/// A drone that appears while the flood is already running is still
/// confirmed and announced, within the delay docs/SPOOFING.md states.
#[test]
fn new_real_drone_is_announced_during_a_flood() {
    let mut tr: Tracker = Tracker::default();
    let mut alerts = std::vec::Vec::new();
    let mut next = 0;
    // Ten seconds of flood: the table is full of fakes.
    run_flood(&mut tr, 0, 10_000, &mut next, &[], &mut alerts, |_, _| {});
    assert!(alerts.is_empty(), "no fake is ever announced");
    assert_eq!(tr.len(), 64);
    // Then a real drone at 1 Hz, flood still running.
    let mut first_alert = None;
    run_flood(&mut tr, 10_000, 30_000, &mut next, &[2], &mut alerts, |_, _| {});
    for id in &alerts {
        if track_of(&tr, 2).is_some_and(|t| t.id == *id) && first_alert.is_none() {
            first_alert = Some(*id);
        }
    }
    let r = track_of(&tr, 2).expect("tracked");
    assert_eq!(alerts, [r.id], "announced once, and nothing else is");
    // First heard at 10 s; confirmed by its frame at 12 s at the latest
    // (one frame to be remembered, one to be admitted, one a second later).
    let at = r.confirmed_ms().unwrap();
    assert!(at <= 10_000 + crate::probation::WORST_CASE_FLOOD_ANNOUNCE_MS, "confirmed at {at}");
    assert_eq!(r.first_seen_ms, 10_002, "history counts from its first frame");
}

/// A quiet site: one drone alone is confirmed and announced within the
/// stated delay (2 s at one frame a second), exactly once.
#[test]
fn quiet_site_drone_is_announced_promptly_and_once() {
    let mut tr: Tracker = Tracker::default();
    let mut alerts = std::vec::Vec::new();
    let mut t = 0;
    let mut announced_at = None;
    while t <= 10_000 {
        tr.ingest(0, &real(3), t);
        while let Some(id) = tr.take_new_drone(t) {
            alerts.push(id);
            announced_at.get_or_insert(t);
        }
        t += 1_000;
    }
    assert_eq!(alerts.len(), 1);
    let at = announced_at.unwrap();
    assert!(at <= crate::probation::WORST_CASE_ANNOUNCE_MS, "announced at {at}");
    assert_eq!(at, 1_000, "the second frame, a second after the first");
    // Faster senders are confirmed as soon as a second has passed.
    let mut tr: Tracker = Tracker::default();
    let mut t = 0;
    let mut at = None;
    while t <= 3_000 {
        tr.ingest(0, &real(4), t);
        if tr.take_new_drone(t).is_some() {
            at.get_or_insert(t);
        }
        t += 200;
    }
    assert_eq!(at, Some(1_000));
}

/// A single frame never announces anything, and goes away on its own.
#[test]
fn one_frame_is_never_announced() {
    let mut tr: Tracker = Tracker::default();
    let id = tr.ingest(0, &fake(1), 0).track().unwrap();
    for t in [0, 1_000, 5_000, 20_000] {
        assert_eq!(tr.take_new_drone(t), None);
    }
    assert!(!tr.get(id).unwrap().confirmed());
    tr.expire(crate::probation::UNCONFIRMED_REMOVE_MS);
    assert!(tr.get(id).is_none(), "an unconfirmed track is dropped sooner");
}

/// Fakes that repeat (every ID twice, so each looks like it is "heard
/// again") still never push out a confirmed drone, and new tracks are made
/// at a bounded rate.
#[test]
fn repeating_fakes_never_displace_a_confirmed_drone() {
    let mut tr: Tracker = Tracker::default();
    let mut t = 0;
    while t < 3_000 {
        tr.ingest(1, &real(5), t);
        t += 1_000;
    }
    let id = track_of(&tr, 5).unwrap().id;
    let mut created = 0u32;
    let mut last_next_id = tr.tracks().iter().map(|t| t.id.0).max().unwrap();
    let mut i = 0u32;
    while t < 33_000 {
        // Each fake ID twice, 600 ms apart, 200 new IDs a second.
        tr.ingest(0, &fake(i), t);
        if i >= 120 {
            tr.ingest(0, &fake(i - 120), t);
        }
        i += 1;
        if t % 1000 == 0 {
            tr.ingest(1, &real(5), t);
            assert_eq!(track_of(&tr, 5).map(|t| t.id), Some(id), "at {t}");
        }
        let top = tr.tracks().iter().map(|t| t.id.0).max().unwrap();
        if top > last_next_id {
            created += top - last_next_id;
            last_next_id = top;
        }
        t += FAKE_EVERY_MS;
    }
    // 30 s: free slots (63, refilled as unconfirmed ones expire) plus at
    // most PROMOTE_PER_S promotions a second.
    let bound = 63 * (1 + 30_000 / crate::probation::UNCONFIRMED_REMOVE_MS as u32)
        + 30 * crate::probation::PROMOTE_PER_S
        + crate::probation::PROMOTE_BURST;
    assert!(created <= bound, "{created} tracks made, bound {bound}");
}

/// The flood alarm has a threshold: a busy but real sky does not raise it.
#[test]
fn a_drone_race_does_not_raise_the_flood_alarm() {
    let mut tr: Tracker = Tracker::default();
    // 40 drones switched on within 4 s, then all flying at 2 Hz for 60 s.
    let mut t = 0;
    while t < 64_000 {
        for n in 0..40u8 {
            if t >= u64::from(n) * 100 {
                tr.ingest(0, &real(n), t + u64::from(n));
            }
        }
        tr.expire(t);
        t += 500;
    }
    assert_eq!(tr.tracks().iter().filter(|t| t.confirmed()).count(), 40);
    assert!(tr.threats().iter().all(|t| t.kind != link::ThreatKind::IdFlood));
    assert!(tr.flood_status(t).is_none());
}

/// Antipodal fakes (drone and operator on opposite sides of the Earth)
/// cost a bounded number of solver iterations, not 200 a call.
#[test]
fn antipodal_positions_cost_a_bounded_number_of_iterations() {
    let (r, iters) = crate::geo::range_bearing_counted(0.0, 0.0, 0.5, 179.7);
    assert!(r.distance_m.is_finite() && r.distance_m > 19_000_000.0);
    assert!(iters <= crate::geo::MAX_VINCENTY_ITERATIONS, "{iters}");
    assert_eq!(iters, 0, "far apart: the sphere, no iterations");
    let (r, iters) = crate::geo::range_bearing_counted(39.9, -75.2, -39.9, 104.8);
    assert!(r.distance_m > 19_000_000.0);
    assert_eq!(iters, 0);
    // Radio range: Vincenty, converging in a handful.
    let (r, iters) = crate::geo::range_bearing_counted(39.952_583, -75.165_222, 39.948_889, -75.15);
    assert!((r.distance_m - 1_363.931).abs() < 0.001, "{r:?}");
    assert!((1..=8).contains(&iters), "{iters}");
    // Every pair of points is bounded.
    for lat in [-89.0, -45.0, 0.0, 10.0, 60.0, 89.9] {
        for dlon in [0.0, 1.0, 9.0, 90.0, 179.0, 179.99] {
            let (_, n) = crate::geo::range_bearing_counted(lat, 0.0, -lat + 0.01, dlon);
            assert!(n <= crate::geo::MAX_VINCENTY_ITERATIONS);
        }
    }
}

/// `size_of` of what the hubs keep, printed for the report.
#[test]
fn sizes() {
    use core::mem::size_of;
    std::println!(
        "Tracker<64,32> {} B, Track {} B, Tracker<32,0> {} B, AircraftTrack {} B, Threat {} B, ControlTable {} B",
        size_of::<Tracker<64, 32>>(),
        size_of::<Track>(),
        size_of::<Tracker<32, 0>>(),
        size_of::<AircraftTrack>(),
        size_of::<threats::Threat>(),
        size_of::<ctrllinks::ControlTable>(),
    );
    // The P4 keeps the tracker in a 1 MiB PSRAM window.
    assert!(size_of::<Tracker<64, 32>>() < 1 << 20);
}
