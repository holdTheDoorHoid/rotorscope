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
use crate::threats::Mute;
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
        "Tracker<64,32> {} B, Track {} B, Claims {} B, Tracker<32,0> {} B, AircraftTrack {} B, Threat {} B, ControlTable {} B, Mute {} B, ThreatTable {} B",
        size_of::<Tracker<64, 32>>(),
        size_of::<Track>(),
        size_of::<Claims>(),
        size_of::<Tracker<32, 0>>(),
        size_of::<AircraftTrack>(),
        size_of::<threats::Threat>(),
        size_of::<ctrllinks::ControlTable>(),
        size_of::<threats::Mute>(),
        size_of::<threats::ThreatTable>(),
    );
    // The P4 keeps the tracker in a 1 MiB PSRAM window.
    assert!(size_of::<Tracker<64, 32>>() < 1 << 20);
}

fn threat(kind: link::ThreatKind, target: u8, freq: u32) -> link::ThreatSighting {
    link::ThreatSighting {
        kind,
        channel: 6,
        freq,
        width_khz: 0,
        level: 20,
        count: 50,
        window_ms: 1000,
        source: [0x02, 0, 0, 0, 0, target],
        target: [0x0a, 0, 0, 0, 1, target],
        detail: 7,
        ssid: heapless::String::new(),
        uptime_us: 1,
    }
}

/// Deauth floods against 24 different targets from two nodes, while a
/// jammer is active, a node is blinded and a control link is contested, and
/// someone also spins up a new fake "evil twin" network every 50 ms: none of
/// the jammer, the blinded node or the contested link is ever evicted,
/// `partly_blind` stays true throughout.
#[test]
fn threat_flood_never_evicts_jamming_or_contested() {
    use link::ThreatKind::*;
    let mut tr: Tracker = Tracker::default();
    let jam = tr.ingest_threat(3, &threat(JamBroadband, 0, 58_000), 0).unwrap();
    let ctl = tr.ingest_threat(4, &threat(ControlContested, 0, 9_150), 0).unwrap();
    let blind = tr.ingest_hub_threat(&threat(Blinded, 0, 0), 0).unwrap();
    let mut t = 0;
    let mut twin = 0u32;
    while t < 120_000 {
        // The jammer, the blinded node and the contested link keep being
        // reported, once a second.
        if t % 1_000 == 0 {
            assert_eq!(tr.ingest_threat(3, &threat(JamBroadband, 0, 58_000), t), Some(jam));
            assert_eq!(tr.ingest_threat(4, &threat(ControlContested, 0, 9_150), t), Some(ctl));
            assert_eq!(tr.ingest_hub_threat(&threat(Blinded, 0, 0), t), Some(blind));
        }
        // Each node reports floods against a new target every 50 ms.
        for node in [1, 2] {
            for kind in [Deauth, Disassoc, ProbeFlood, AuthFlood, BeaconFlood, BleSpam] {
                tr.ingest_threat(node, &threat(kind, (t / 50 % 24) as u8, 24_370), t);
            }
        }
        // And a new evil twin every 50 ms (each its own entry).
        let mut s = threat(EvilTwin, 0, 24_370);
        s.source = [0x02, 0xee, (twin >> 16) as u8, (twin >> 8) as u8, twin as u8, 1];
        tr.ingest_threat(1, &s, t);
        twin += 1;
        assert!(tr.partly_blind(t), "at {t}");
        assert!(tr.get_threat(jam).is_some() && tr.get_threat(ctl).is_some(), "at {t}");
        assert!(tr.get_threat(blind).is_some(), "at {t}");
        assert!(tr.threats().iter().filter(|x| threats::is_flood_kind(x.kind)).count() <= threats::MAX_FLOOD_THREATS);
        t += 50;
    }
}

/// Flood kinds are one entry per reporting node, whatever the target; a
/// muted target keeps its own (muted) entry outside the aggregate.
#[test]
fn flood_threats_aggregate_per_node() {
    use link::ThreatKind::*;
    let mut tr: Tracker = Tracker::default();
    let mut t = 0;
    while t < 60_000 {
        for node in [1, 2] {
            for kind in [Deauth, Disassoc, ProbeFlood, AuthFlood] {
                tr.ingest_threat(node, &threat(kind, (t / 50 % 24) as u8, 24_370), t);
            }
        }
        t += 50;
    }
    assert_eq!(tr.threats().len(), 8, "{:?}", tr.threats());
    let deauths: std::vec::Vec<_> = tr.threats().iter().filter(|x| x.kind == Deauth).collect();
    assert_eq!(deauths.len(), 2);
    assert_eq!(deauths[0].count, 50 * 1_200);
    let mut m = Mute::for_threat(deauths[0], t);
    assert_eq!((m.target, m.node), ([0; 6], deauths[0].node), "muting an aggregate mutes that node's");
    m.target = [0x0a, 0, 0, 0, 1, 99];
    m.node = threats::NODE_ANY;
    assert_eq!(tr.mute(m, t), Ok(true));
    tr.ingest_threat(1, &threat(Deauth, 99, 24_370), t);
    let muted: std::vec::Vec<_> = tr.threats().iter().filter(|x| x.kind == Deauth && x.muted).collect();
    assert_eq!(muted.len(), 1);
    assert_eq!(muted[0].target[5], 99);
    assert!(!muted[0].per_node);
    assert!(tr.threats().iter().filter(|x| x.kind == Deauth && !x.muted).all(|x| x.count == 50 * 1_200));
}

/// The flood threat's JSON line names the table and the rate.
#[test]
fn flood_threat_json_line() {
    let mut tr: Tracker = Tracker::default();
    let mut f = probation::FloodWatch::new();
    for ms in (0..6_000).step_by(4) {
        f.note(ms);
    }
    let s = f.check(6_000, probation::FLOOD_NEW_PER_S, probation::FloodTable::Drones).unwrap();
    let id = tr.ingest_hub_threat(&s, 6_000).unwrap();
    let ctx = tr.context(6_000, None);
    let mut buf = [0u8; threats::MAX_THREAT_LINE];
    let n = threats::threat_line(tr.get_threat(id).unwrap(), &ctx, &mut buf).unwrap();
    let text = core::str::from_utf8(&buf[..n]).unwrap();
    assert!(text.contains("\"kind\":\"id_flood\""), "{text}");
    assert!(text.contains("\"label\":\"Flood of new drone IDs\""), "{text}");
    assert!(text.contains("\"table\":\"drones\",\"per_s\":250"), "{text}");
    assert_eq!(
        probation::describe(tr.get_threat(id).unwrap()).as_str(),
        "Flood of new drone IDs: 250 new a second over 5 s (limit 20); unconfirmed ones are kept out of alerts"
    );
    // The longest per-node flood line still fits, with addresses shown.
    let mut tr: Tracker = Tracker::new(Config { show_link_ids: true, ..Config::default() });
    for kind in [link::ThreatKind::BeaconFlood, link::ThreatKind::ChannelSwitch, link::ThreatKind::BleSpam] {
        let mut big = threat(kind, 0xff, u32::MAX);
        big.ssid = heapless::String::try_from("\"".repeat(32).as_str()).unwrap();
        big.detail = u32::MAX;
        big.channel = u8::MAX;
        big.width_khz = u32::MAX;
        big.level = i8::MIN;
        big.count = u32::MAX;
        let id = tr.ingest_threat(u8::MAX, &big, u64::MAX / 4).unwrap();
        tr.ingest_threat(u8::MAX, &big, u64::MAX / 4);
        let ctx = tr.context(u64::MAX / 2, None);
        let t = tr.get_threat(id).unwrap();
        assert!(t.per_node);
        assert!(threats::threat_line(t, &ctx, &mut buf).is_ok(), "{kind:?}");
    }
}

/// An ADS-B report (as the ADSBee or the hub's own decoder gives it),
/// heard this second, `north_m` north of the hub.
fn plane(icao: u32, north_m: f64, ft: i32) -> adsb::Aircraft {
    adsb::Aircraft {
        source: adsb::Source::ModeS,
        icao,
        flags: 0xff | (1 << adsb::flag::UPDATED_POSITION),
        callsign: adsb::Callsign::new("TEST").unwrap(),
        squawk: Some(0o1200),
        category: 7,
        lat_deg: CITY_HALL.0 + north_m / 111_000.0,
        lon_deg: CITY_HALL.1,
        baro_alt_ft: ft,
        gnss_alt_ft: ft + 100,
        direction_deg: 0.0,
        speed_kt: 100,
        baro_vrate_fpm: 0,
        gnss_vrate_fpm: 0,
        uat_emergency: 0,
        nicnac: 0,
        acdims: 0,
        version: 2,
        signal_dbm: -60,
        signal_quality: 9,
        frames_per_s: 3,
        es_frames_per_s: 4,
    }
}

/// Two real aircraft with a history, then 40 forged addresses a second, each
/// "heard" once right at the hub, for 60 s: the real ones keep their
/// entries and history, no fake ever makes the drone-near-aircraft warning,
/// a new real aircraft still gets in and establishes itself, and the
/// operator is told about the flood.
#[test]
fn aircraft_survive_a_flood_of_forged_addresses() {
    let mut tr: Tracker = Tracker::default();
    tr.set_own_position(CITY_HALL.0, CITY_HALL.1, Some(10.0), 0);
    // A real, confirmed drone at the hub at 120 m: any aircraft "at the
    // hub" would be a conflict.
    tr.ingest(1, &real(9), 0);
    tr.ingest(1, &real(9), 1_000);
    let mut t = 0;
    // 100 kt north: 51 m a second.
    let fly = |t: u64, from: f64| from + 51.0 * t as f64 / 1000.0;
    while t < 5_000 {
        tr.ingest_aircraft(&plane(0xA11111, fly(t, -20_000.0), 3_000), t);
        tr.ingest_aircraft(&plane(0xA22222, fly(t, 15_000.0), 6_000), t);
        t += 1_000;
    }
    let firsts: std::vec::Vec<u64> = tr.aircraft().iter().map(|a| a.first_seen_ms).collect();
    assert!(tr.aircraft().iter().all(|a| a.established()));
    let mut fake = 0u32;
    while t < 65_000 {
        tr.ingest(1, &real(9), t);
        tr.ingest_aircraft(&plane(0xA11111, fly(t, -20_000.0), 3_000), t);
        tr.ingest_aircraft(&plane(0xA22222, fly(t, 15_000.0), 6_000), t);
        if t >= 30_000 {
            tr.ingest_aircraft(&plane(0xA33333, fly(t, 8_000.0), 4_000), t);
        }
        for _ in 0..40 {
            tr.ingest_aircraft(&plane(0xC00000 + fake, 0.0, 500), t);
            fake += 1;
        }
        tr.expire(t);
        for (i, icao) in [0xA11111u32, 0xA22222].into_iter().enumerate() {
            let a = tr.aircraft().iter().find(|a| a.key.icao == icao).expect("never displaced");
            assert_eq!(a.first_seen_ms, firsts[i]);
            assert!(a.established());
        }
        assert!(tr.conflicts(t).iter().all(|c| c.aircraft.icao < 0xC00000), "a fake raised a conflict at {t}");
        t += 1_000;
    }
    let late = tr.aircraft().iter().find(|a| a.key.icao == 0xA33333).expect("a new real aircraft gets in");
    assert!(late.established());
    let flood = tr
        .active_threats(t)
        .find(|x| probation::flood_table(x) == Some(probation::FloodTable::Aircraft))
        .expect("aircraft flood alarm");
    assert!(probation::flood_rate(flood) >= probation::AIRCRAFT_FLOOD_PER_S);
}

/// A busy real sky (50 aircraft, more than the table holds, all heard every
/// second) does not raise the aircraft flood alarm.
#[test]
fn a_busy_real_sky_is_not_an_aircraft_flood() {
    let mut tr: Tracker = Tracker::default();
    tr.set_own_position(CITY_HALL.0, CITY_HALL.1, Some(10.0), 0);
    let mut t = 0;
    while t < 60_000 {
        for k in 0..50u32 {
            tr.ingest_aircraft(&plane(0xA00000 + k, 2_000.0 * f64::from(k) + 51.0 * t as f64 / 1000.0, 5_000), t);
        }
        tr.expire(t);
        t += 1_000;
    }
    assert_eq!(tr.aircraft().len(), 32);
    assert!(tr.threats().iter().all(|x| x.kind != link::ThreatKind::IdFlood));
}

// ------------------------------------------------- one node shedding a flood --

/// A node's once-a-second health report: uptime and its drop total.
fn stats(up_s: u64, dropped: u32) -> link::Frame {
    link::Frame::Stats { uptime_us: up_s * 1_000_000 + 1, frames_seen: 500 * up_s as u32, rid_found: 0, dropped, by_transport: heapless::Vec::new() }
}

/// What one node delivers each second for `secs` seconds from `from_ms`:
/// `fakes` new one-frame IDs (spread over the second), every drone in
/// `reals` once, and a Stats frame whose drop total grows by `drops`.
/// `up` and `dropped` are the node's uptime (s) and drop total, carried
/// across calls. Returns the hub ms the node-shed alarm for `node` was first
/// active, if it was.
#[allow(clippy::too_many_arguments)]
fn node_run(
    tr: &mut Tracker,
    node: u8,
    from_ms: u64,
    secs: u64,
    fakes: u32,
    drops: u32,
    reals: &[u8],
    next_fake: &mut u32,
    up: &mut u64,
    dropped: &mut u32,
) -> Option<u64> {
    let mut raised = None;
    for k in 0..secs {
        let t0 = from_ms + k * 1000;
        for j in 0..fakes {
            tr.ingest(node, &fake(*next_fake), t0 + u64::from(j) * 1000 / u64::from(fakes));
            *next_fake += 1;
        }
        for &n in reals {
            tr.ingest(node, &real(n), t0 + 7 + u64::from(n));
        }
        *up += 1;
        *dropped = dropped.wrapping_add(drops);
        tr.ingest_frame(node, &stats(*up, *dropped), t0 + 900);
        tr.expire(t0 + 950);
        if raised.is_none() && node_shed(tr, node, t0 + 950).is_some() {
            raised = Some(t0 + 950);
        }
    }
    raised
}

fn node_shed<const N: usize, const A: usize>(tr: &Tracker<N, A>, node: u8, now: u64) -> Option<&threats::Threat> {
    tr.active_threats(now)
        .find(|t| probation::flood_table(t) == Some(probation::FloodTable::NodeShed) && t.node == node)
}

fn no_flood_alarm_at_all(tr: &Tracker, now: u64) {
    let any: std::vec::Vec<_> = tr.threats().iter().filter(|t| t.kind == link::ThreatKind::IdFlood).collect();
    assert!(any.is_empty(), "no flood alarm of any table at {now}: {any:?}");
}

/// A 200-a-second fake-drone flood heard by one Wi-Fi node: the node lets
/// its new-source budget (8 a second) through and sheds the rest, so the
/// hub sees 8 new IDs a second, under the hub-wide line, and a drop total
/// rising by 192 a second. The node-shed alarm names the node within
/// `SHED_RAISE_MS`; the hub-wide drone alarm stays quiet (it is not 20 a
/// second); a real drone on another node is unaffected; the alarm is
/// snooze-only and a snooze of it covers that node only.
#[test]
fn a_flood_shed_by_one_node_raises_the_node_shed_alarm() {
    let mut tr: Tracker = Tracker::default();
    let (mut next, mut up2, mut drop2, mut up1, mut drop1) = (0, 0, 0, 0, 0);
    // Ten quiet seconds: node 2 healthy (no drops), a real drone on node 1.
    assert_eq!(node_run(&mut tr, 2, 0, 10, 0, 0, &[], &mut next, &mut up2, &mut drop2), None);
    node_run(&mut tr, 1, 0, 10, 0, 0, &[1], &mut next, &mut up1, &mut drop1);
    // The flood starts at 10 s, as node-wifi delivers it.
    let mut at = None;
    for s in 10..40u64 {
        let r = node_run(&mut tr, 2, s * 1000, 1, 8, 192, &[], &mut next, &mut up2, &mut drop2);
        node_run(&mut tr, 1, s * 1000, 1, 0, 0, &[1], &mut next, &mut up1, &mut drop1);
        at = at.or(r);
    }
    let at = at.expect("node-shed alarm raised");
    std::println!("node-shed alarm raised {} ms after the flood started", at - 10_000);
    assert!(at <= 10_000 + probation::SHED_RAISE_MS, "raised at {at}");
    let t = node_shed(&tr, 2, 40_000).unwrap();
    assert!(t.by_hub);
    assert_eq!(t.label(), "Node shedding a flood of drone IDs");
    assert_eq!(probation::shed_rate(t), Some(192));
    assert_eq!(probation::flood_rate(t), 8);
    assert_eq!(
        probation::describe(t).as_str(),
        "Node shedding a flood of drone IDs (node 2): 192 frames a second discarded, 8 new drone IDs a second never confirmed, over 5 s"
    );
    let ctx = tr.context(40_000, None);
    let mut buf = [0u8; threats::MAX_THREAT_LINE];
    let n = threats::threat_line(t, &ctx, &mut buf).unwrap();
    let text = core::str::from_utf8(&buf[..n]).unwrap();
    assert!(text.contains("\"node\":2"), "{text}");
    assert!(text.contains("\"table\":\"node\",\"per_s\":8,\"dropped_per_s\":192"), "{text}");
    // The hub-wide line (20 new IDs a second) is not reached by one node.
    assert!(tr.flood_status(40_000).is_none());
    assert!(tr.threats().iter().all(|t| probation::flood_table(t) != Some(probation::FloodTable::Drones)));
    // The real drone on node 1 is tracked and confirmed throughout.
    assert!(track_of(&tr, 1).is_some_and(|t| t.confirmed()));
    // Node 1 is not shedding anything.
    assert!(node_shed(&tr, 1, 40_000).is_none());
    // Safety kind: a snooze (5 min), never a mute; it names node 2 only.
    let id = t.id;
    let m = tr.silence(id, 40_000).unwrap();
    assert!(m.snooze());
    assert_eq!((m.node, m.sub), (2, probation::FloodTable::NodeShed as u8));
    assert!(m.check_permanent().is_err(), "never from the site file");
    // The same flood now reaches node 3 too: its own alarm, not snoozed.
    let (mut up3, mut drop3) = (0, 0);
    let mut at3 = None;
    for s in 40..60u64 {
        node_run(&mut tr, 2, s * 1000, 1, 8, 192, &[], &mut next, &mut up2, &mut drop2);
        at3 = at3.or(node_run(&mut tr, 3, s * 1000, 1, 8, 192, &[], &mut next, &mut up3, &mut drop3));
    }
    assert!(at3.is_some(), "node 3's alarm is not covered by node 2's snooze");
    assert!(tr.threats().iter().any(|t| t.node == 2 && t.snoozed));
    // The flood stops: both alarms end within THREAT_ACTIVE_MS plus the window.
    for s in 60..80u64 {
        node_run(&mut tr, 2, s * 1000, 1, 0, 0, &[], &mut next, &mut up2, &mut drop2);
        node_run(&mut tr, 3, s * 1000, 1, 0, 0, &[], &mut next, &mut up3, &mut drop3);
    }
    assert!(node_shed(&tr, 2, 80_000).is_none() && node_shed(&tr, 3, 80_000).is_none());
}

/// The same flood a little over the node's budget (12 a second: 8 through,
/// 4 shed) is under the drop line, and so is not raised; a flood shedding at
/// least `SHED_DROP_PER_S` is. The stated limit, tested.
#[test]
fn the_node_shed_alarm_starts_at_its_drop_line() {
    for (drops, raised) in [(4, false), (probation::SHED_DROP_PER_S, true)] {
        let mut tr: Tracker = Tracker::default();
        let (mut next, mut up, mut dropped) = (0, 0, 0);
        node_run(&mut tr, 2, 0, 3, 0, 0, &[], &mut next, &mut up, &mut dropped);
        let r = node_run(&mut tr, 2, 3_000, 30, 8, drops, &[], &mut next, &mut up, &mut dropped);
        assert_eq!(r.is_some(), raised, "{drops} dropped a second");
    }
}

/// A quiet site: one real drone, a node that drops nothing. No alarm.
#[test]
fn quiet_site_raises_no_node_shed_alarm() {
    let mut tr: Tracker = Tracker::default();
    let (mut next, mut up, mut dropped) = (0, 0, 0);
    assert_eq!(node_run(&mut tr, 1, 0, 120, 0, 0, &[3], &mut next, &mut up, &mut dropped), None);
    no_flood_alarm_at_all(&tr, 120_000);
}

/// A node booting: its first Stats carries everything it lost while
/// starting (a baseline, not a rate), it delivers the drones around it all
/// at once (they confirm), and later it reboots (uptime and total go back).
/// No alarm.
#[test]
fn a_booting_node_raises_no_node_shed_alarm() {
    let mut tr: Tracker = Tracker::default();
    let mut next = 0;
    let reals: std::vec::Vec<u8> = (0..12).collect();
    // First report: 2 s up, 900 frames already lost.
    let (mut up, mut dropped) = (1, 900);
    assert_eq!(node_run(&mut tr, 4, 0, 30, 0, 3, &reals, &mut next, &mut up, &mut dropped), None);
    // Reboot: uptime from 0, a fresh total that starts high again.
    let (mut up, mut dropped) = (0, 700);
    assert_eq!(node_run(&mut tr, 4, 30_000, 30, 0, 3, &reals, &mut next, &mut up, &mut dropped), None);
    // A link gap of a minute: the next total covers the whole gap, and is
    // a new baseline rather than a minute's drops in one second.
    let (mut up2, mut dropped2) = (up + 60, dropped + 60 * 40);
    assert_eq!(node_run(&mut tr, 4, 120_000, 10, 0, 3, &reals, &mut next, &mut up2, &mut dropped2), None);
    no_flood_alarm_at_all(&tr, 130_000);
}

/// A busy venue: the node's receive pool overflows steadily (40 frames a
/// second, over the drop line) and now and then a stray Remote ID frame is
/// heard once and never again (one a second). And a modest steady drop
/// rate (5 a second) with the same strays. Neither is a flood.
#[test]
fn busy_venue_drops_raise_no_node_shed_alarm() {
    for drops in [40, 5] {
        let mut tr: Tracker = Tracker::default();
        let (mut next, mut up, mut dropped) = (0, 0, 0);
        assert_eq!(node_run(&mut tr, 1, 0, 120, 1, drops, &[2, 5], &mut next, &mut up, &mut dropped), None, "{drops}");
        no_flood_alarm_at_all(&tr, 120_000);
    }
}

/// The 40-drone race through one node, which also drops: the node's
/// budget sheds some first frames as the drones switch on (8 new sources a
/// second), and the venue's receive pool overflows (20 a second). Every
/// new ID confirms, so no alarm of any table.
#[test]
fn a_drone_race_through_a_shedding_node_raises_no_alarm() {
    let mut tr: Tracker = Tracker::default();
    let mut t = 0;
    let (mut up, mut dropped) = (0u64, 0u32);
    while t < 64_000 {
        for n in 0..40u8 {
            if t >= u64::from(n) * 100 {
                tr.ingest(0, &real(n), t + u64::from(n));
            }
        }
        if t % 1000 == 0 {
            up += 1;
            // 10 drones a second switch on for 4 s: 2 a second over budget.
            dropped += 20 + if t < 4_000 { 2 } else { 0 };
            tr.ingest_frame(0, &stats(up, dropped), t + 900);
        }
        tr.expire(t);
        t += 500;
    }
    assert_eq!(tr.tracks().iter().filter(|t| t.confirmed()).count(), 40);
    no_flood_alarm_at_all(&tr, t);
}
