//! GPS interference in the area (`area_gnss`): aircraft integrity, drones
//! and aircraft jumping together, the hub's own receiver.

extern crate std;
use std::string::String;

use crate::area_gnss::{self, AreaWatch, OwnGnss, detail};
use crate::tests::{CITY_HALL, det, location, serial};
use crate::*;
use adsb::{Aircraft, Callsign, Source, flag, nicnac};
use link::ThreatKind;
use rid::Transport;

const M_PER_DEG: f64 = 111_320.0;

/// An airborne ADS-B aircraft `north_km` north and `east_km` east of City
/// Hall, heard this second with its position updated, reporting `nic` and
/// `nacp`.
fn plane(icao: u32, north_km: f64, east_km: f64, nic: u8, nacp: u8) -> Aircraft {
    Aircraft {
        source: Source::ModeS,
        icao,
        flags: 0xff | (1 << flag::UPDATED_POSITION),
        callsign: Callsign::new("TEST1").unwrap(),
        squawk: Some(0o1200),
        category: 3,
        lat_deg: CITY_HALL.0 + north_km * 1000.0 / M_PER_DEG,
        lon_deg: CITY_HALL.1 + east_km * 1000.0 / (M_PER_DEG * libm::cos(CITY_HALL.0.to_radians())),
        baro_alt_ft: 20_000,
        gnss_alt_ft: 20_100,
        direction_deg: 0.0,
        // Stationary for the prediction (speed 0): the tests move nothing.
        speed_kt: 0,
        baro_vrate_fpm: 0,
        gnss_vrate_fpm: 0,
        uat_emergency: 0,
        nicnac: nicnac::pack(3, 2, nacp, 2, true, nic),
        acdims: 0,
        version: 2,
        signal_dbm: -70,
        signal_quality: 9,
        frames_per_s: 2,
        es_frames_per_s: 4,
    }
}

fn on_ground(mut a: Aircraft) -> Aircraft {
    a.flags &= !(1 << flag::AIRBORNE);
    a
}

/// Listed by the ADSBee, not heard this second.
fn quiet(mut a: Aircraft) -> Aircraft {
    a.frames_per_s = 0;
    a.es_frames_per_s = 0;
    a
}

fn hub() -> Tracker {
    let mut tr: Tracker = Tracker::default();
    tr.set_own_position(CITY_HALL.0, CITY_HALL.1, Some(40.0), 0);
    tr
}

/// One second: every aircraft report, then the area check.
fn second(tr: &mut Tracker, w: &mut AreaWatch, t: u64, planes: &[Aircraft]) {
    tr.set_own_position(CITY_HALL.0, CITY_HALL.1, Some(40.0), t);
    for a in planes {
        tr.ingest_aircraft(a, t);
    }
    w.tick(tr, t);
}

fn area_threats(tr: &Tracker, now: u64) -> std::vec::Vec<&threats::Threat> {
    tr.active_threats(now).filter(|t| t.kind == ThreatKind::AreaGnss).collect()
}

fn fleet(nic: impl Fn(u32) -> u8, nacp: impl Fn(u32) -> u8, n: u32) -> std::vec::Vec<Aircraft> {
    (0..n).map(|i| plane(0xA0_0000 + i, 5.0 * f64::from(i) + 3.0, -2.0, nic(i), nacp(i))).collect()
}

// ------------------------------------------------------------- aircraft --

#[test]
fn many_aircraft_losing_integrity_together_is_jamming() {
    let (mut tr, mut w) = (hub(), AreaWatch::new());
    let mut t = 0;
    for _ in 0..60 {
        t += 1_000;
        second(&mut tr, &mut w, t, &fleet(|_| 8, |_| 10, 6));
    }
    assert!(area_threats(&tr, t).is_empty());
    assert_eq!(w.status().counted, 6);
    // Four of six fall to NIC 0 / NACp 0 (ADSBee prints 0; NACp 0 is
    // ignored, NIC 0 from a position report is believed).
    let mut fired_at = None;
    for s in 0..10 {
        t += 1_000;
        second(&mut tr, &mut w, t, &fleet(|i| if i < 4 { 0 } else { 8 }, |i| if i < 4 { 0 } else { 10 }, 6));
        if fired_at.is_none() && !area_threats(&tr, t).is_empty() {
            fired_at = Some(s);
        }
    }
    // Held for AIR_HOLD_MS (5 s) before counting.
    assert_eq!(fired_at, Some(5));
    let th = area_threats(&tr, t);
    assert_eq!(th.len(), 1);
    let th = th[0].clone();
    let th = &th;
    assert!(th.by_hub);
    assert_eq!(th.target[0], 1, "jamming");
    assert_eq!(th.freq, area_gnss::L1_FREQ);
    let d = th.detail;
    assert!(d & detail::JAMMING != 0 && d & detail::AIR_INTEGRITY != 0 && d & detail::OWN_AGREES == 0);
    assert_eq!((detail::byte(d, 1), detail::byte(d, 2), detail::byte(d, 3)), (4, 6, 4));
    assert_eq!(th.level, 70);
    let text = area_gnss::describe(th);
    assert!(text.contains("4 of 6 aircraft"), "{text}");
    // Alerted once.
    assert_eq!(tr.take_new_threat(t), Some(th.id));
    assert_eq!(tr.take_new_threat(t), None);
    let st = *w.status();
    assert!(st.jamming && !st.spoofing && st.dropped == 4);
}

#[test]
fn nacp_drop_counts_like_nic() {
    let (mut tr, mut w) = (hub(), AreaWatch::new());
    let mut t = 0;
    for _ in 0..30 {
        t += 1_000;
        second(&mut tr, &mut w, t, &fleet(|_| 8, |_| 10, 5));
    }
    // NIC stays 8 but NACp falls to 4 (< 1 NM) on three.
    for _ in 0..8 {
        t += 1_000;
        second(&mut tr, &mut w, t, &fleet(|_| 8, |i| if i < 3 { 4 } else { 10 }, 5));
    }
    assert_eq!(area_threats(&tr, t).len(), 1);
}

#[test]
fn old_transponders_that_always_report_low_do_not_fire() {
    let (mut tr, mut w) = (hub(), AreaWatch::new());
    let mut t = 0;
    // One of six always NIC 5, no NACp: never good, so never "dropped".
    for _ in 0..300 {
        t += 1_000;
        second(&mut tr, &mut w, t, &fleet(|i| if i == 0 { 5 } else { 8 }, |i| if i == 0 { 0 } else { 10 }, 6));
    }
    assert!(area_threats(&tr, t).is_empty());
    assert_eq!((w.status().degraded, w.status().dropped), (1, 0));
    // Even three of six (a busy general-aviation field): low share, no drop.
    let (mut tr, mut w) = (hub(), AreaWatch::new());
    for _ in 0..120 {
        t += 1_000;
        second(&mut tr, &mut w, t, &fleet(|i| if i < 3 { 6 } else { 8 }, |_| 0, 6));
    }
    assert!(area_threats(&tr, t).is_empty());
    assert_eq!(w.status().degraded, 3);
}

#[test]
fn one_aircraft_dropping_alone_is_not_jamming_unless_the_hub_agrees() {
    let (mut tr, mut w) = (hub(), AreaWatch::new());
    let mut t = 0;
    for _ in 0..30 {
        t += 1_000;
        second(&mut tr, &mut w, t, &fleet(|_| 8, |_| 10, 6));
    }
    for _ in 0..10 {
        t += 1_000;
        second(&mut tr, &mut w, t, &fleet(|i| if i == 2 { 0 } else { 8 }, |_| 10, 6));
    }
    assert!(area_threats(&tr, t).is_empty());
    assert_eq!(w.status().dropped, 1);
    // The hub's own receiver says jamming too: two independent receivers.
    for _ in 0..2 {
        t += 1_000;
        w.set_own_gnss(OwnGnss::Jamming, t);
        second(&mut tr, &mut w, t, &fleet(|i| if i == 2 { 0 } else { 8 }, |_| 10, 6));
    }
    let th = area_threats(&tr, t);
    assert_eq!(th.len(), 1);
    assert!(th[0].detail & detail::OWN_AGREES != 0);
    assert_eq!(th[0].level, 65);
    assert!(area_gnss::describe(th[0]).contains("hub's own GPS agrees"));
}

#[test]
fn aircraft_on_the_ground_or_not_heard_are_ignored() {
    // On the ground: taxiing aircraft under hangars and buildings report
    // poor figures all the time.
    let (mut tr, mut w) = (hub(), AreaWatch::new());
    let mut t = 0;
    for _ in 0..30 {
        t += 1_000;
        let f: std::vec::Vec<_> = fleet(|_| 8, |_| 10, 6).into_iter().map(on_ground).collect();
        second(&mut tr, &mut w, t, &f);
    }
    for _ in 0..20 {
        t += 1_000;
        let f: std::vec::Vec<_> = fleet(|_| 0, |_| 0, 6).into_iter().map(on_ground).collect();
        second(&mut tr, &mut w, t, &f);
    }
    assert!(area_threats(&tr, t).is_empty());
    assert_eq!(w.status().counted, 0);

    // Stale: the ADSBee still lists them with low figures but has not
    // heard them for a while.
    let (mut tr, mut w) = (hub(), AreaWatch::new());
    for _ in 0..30 {
        t += 1_000;
        second(&mut tr, &mut w, t, &fleet(|_| 8, |_| 10, 6));
    }
    for _ in 0..30 {
        t += 1_000;
        let f: std::vec::Vec<_> = fleet(|_| 0, |_| 3, 6).into_iter().map(quiet).collect();
        second(&mut tr, &mut w, t, &f);
    }
    assert!(area_threats(&tr, t).is_empty());
    assert_eq!(w.status().counted, 0);
}

#[test]
fn flarm_and_tis_b_carry_no_gps_figures() {
    let (mut tr, mut w) = (hub(), AreaWatch::new());
    let mut t = 0;
    let mk = |nic| {
        let mut f = fleet(|_| nic, |_| 10, 6);
        for (i, a) in f.iter_mut().enumerate() {
            if i % 2 == 0 {
                a.source = Source::Flarm;
            } else {
                a.source = Source::Uat;
                a.icao |= 2 << 24; // TIS-B
            }
        }
        f
    };
    for _ in 0..30 {
        t += 1_000;
        second(&mut tr, &mut w, t, &mk(8));
    }
    for _ in 0..20 {
        t += 1_000;
        second(&mut tr, &mut w, t, &mk(0));
    }
    assert!(area_threats(&tr, t).is_empty());
    assert_eq!(w.status().counted, 0);
}

#[test]
fn aircraft_losing_position_near_the_hub_count_as_degraded() {
    let (mut tr, mut w) = (hub(), AreaWatch::new());
    let mut t = 0;
    for _ in 0..30 {
        t += 1_000;
        second(&mut tr, &mut w, t, &fleet(|_| 8, |_| 10, 5));
    }
    // Three stop sending positions (still heard, still listed with their
    // old figures): after 20 s they count as lost, then held 5 s.
    for _ in 0..30 {
        t += 1_000;
        let mut f = fleet(|_| 8, |_| 10, 5);
        for a in f.iter_mut().take(3) {
            a.flags &= !(1 << flag::UPDATED_POSITION);
        }
        second(&mut tr, &mut w, t, &f);
    }
    assert_eq!(w.status().dropped, 3);
    assert_eq!(area_threats(&tr, t).len(), 1);
}

#[test]
fn several_aircraft_jumping_alike_is_spoofing() {
    let (mut tr, mut w) = (hub(), AreaWatch::new());
    let mut t = 0;
    for _ in 0..10 {
        t += 1_000;
        second(&mut tr, &mut w, t, &fleet(|_| 8, |_| 10, 4));
    }
    // Two aircraft jump 15 km north: not enough.
    t += 1_000;
    let mut f = fleet(|_| 8, |_| 10, 4);
    for a in f.iter_mut().take(2) {
        a.lat_deg += 15_000.0 / M_PER_DEG;
    }
    second(&mut tr, &mut w, t, &f);
    assert!(area_threats(&tr, t).is_empty());
    assert_eq!(w.status().air_jumpers, 2);
    // A third the next second, by about the same: spoofing.
    t += 1_000;
    f[2].lat_deg += 14_500.0 / M_PER_DEG;
    second(&mut tr, &mut w, t, &f);
    let th = area_threats(&tr, t);
    assert_eq!(th.len(), 1);
    assert_eq!(th[0].target[0], 2);
    let d = th[0].detail;
    assert!(d & detail::SPOOFING != 0 && d & detail::AIR_JUMPS != 0 && d & detail::DRONE_JUMPS == 0);
    assert_eq!(detail::byte(d, 1), 3);
    assert!((145..=150).contains(&detail::byte(d, 3)), "{}", detail::byte(d, 3));
}

// ---------------------------------------------------------------- drones --

/// A Remote ID drone (its own serial and Wi-Fi address) `north_m` north of
/// City Hall, heard by `node` at `rssi`.
fn drone_at(tr: &mut Tracker, n: u8, north_m: f64, east_m: f64, node: u8, rssi: i8, t: u64) {
    let mac = [0x60, 0x60, 0x1f, 0, 0, n];
    let lat = CITY_HALL.0 + north_m / M_PER_DEG;
    let lon = CITY_HALL.1 + east_m / (M_PER_DEG * libm::cos(CITY_HALL.0.to_radians()));
    let id = std::format!("SN-AREA-{n}");
    tr.ingest(node, &det(Transport::WifiBeacon, mac, rssi, &[serial(&id), location(lat, lon, 80.0)]), t);
}

/// A real drone in the field: its number, the node that hears it, how
/// strongly, when it was switched on (ms), and where it flies (metres N, E).
#[derive(Clone, Copy)]
struct Real {
    n: u8,
    node: u8,
    rssi: i8,
    on_ms: u64,
    at: (f64, f64),
}

/// Three independent drones: different addresses, nodes and strengths,
/// switched on a few seconds apart.
const THREE: [Real; 3] = [
    Real { n: 1, node: 0, rssi: -50, on_ms: 1_000, at: (0.0, 0.0) },
    Real { n: 2, node: 1, rssi: -62, on_ms: 4_000, at: (300.0, 400.0) },
    Real { n: 3, node: 2, rssi: -74, on_ms: 7_000, at: (-200.0, 250.0) },
];

/// Every drone reports once a second from `t0` to `t1` (exclusive), each
/// at its place moved by `shift` (metres N, E); the area check looks once
/// a second.
fn fly(tr: &mut Tracker, w: &mut AreaWatch, drones: &[Real], t0: u64, t1: u64, shift: (f64, f64)) {
    let mut t = t0;
    while t < t1 {
        for d in drones.iter().filter(|d| t >= d.on_ms) {
            drone_at(tr, d.n, d.at.0 + shift.0, d.at.1 + shift.1, d.node, d.rssi, t);
        }
        w.tick(tr, t);
        t += 1_000;
    }
}

#[test]
fn three_independent_drones_jumping_together_is_spoofing() {
    // A spoofer drags every receiver around it 1.2 km north-east: three
    // real drones, heard by different nodes, keep reporting from there.
    let (mut tr, mut w) = (hub(), AreaWatch::new());
    fly(&mut tr, &mut w, &THREE, 1_000, 15_000, (0.0, 0.0));
    assert!(area_threats(&tr, 15_000).is_empty());
    let mut fired = None;
    let mut t = 15_000;
    while t < 20_000 {
        fly(&mut tr, &mut w, &THREE, t, t + 1_000, (800.0, 900.0));
        if fired.is_none() && !area_threats(&tr, t).is_empty() {
            fired = Some(t);
        }
        t += 1_000;
    }
    // Each drone's moved claim has its own path after 2 s: a path jump.
    assert_eq!(fired, Some(17_000), "{:?}", w.status());
    let th = area_threats(&tr, t);
    assert_eq!(th.len(), 1);
    let d = th[0].detail;
    assert!(d & detail::SPOOFING != 0 && d & detail::DRONE_JUMPS != 0 && d & detail::OWN_AGREES == 0);
    assert_eq!(detail::byte(d, 2), 3);
    assert_eq!(detail::byte(d, 3), 12, "about 1.2 km");
    assert_eq!(th[0].level, 60);
    let text = area_gnss::describe(th[0]);
    assert!(text.contains("3 drone(s) jumped together by about 1.2 km"), "{text}");
    for tk in tr.tracks() {
        assert_eq!(tk.claims.path_jumps(), 1);
        assert!(!tk.claims.disputed(t));
    }
    // Latched for LATCH_MS, then ends.
    let t = t + area_gnss::LATCH_MS;
    w.tick(&mut tr, t);
    assert!(!w.status().spoofing);
    assert!(area_threats(&tr, t + crate::threats::THREAT_ACTIVE_MS).is_empty());
}

#[test]
fn two_independent_drones_need_the_hub_or_aircraft_to_agree() {
    let two = &THREE[..2];
    let (mut tr, mut w) = (hub(), AreaWatch::new());
    fly(&mut tr, &mut w, two, 1_000, 15_000, (0.0, 0.0));
    fly(&mut tr, &mut w, two, 15_000, 20_000, (800.0, 900.0));
    assert!(area_threats(&tr, 20_000).is_empty(), "two alone are not enough any more");
    assert_eq!(w.status().drone_jumpers, 2);
    // The same with the hub's own receiver saying spoofing.
    let (mut tr, mut w) = (hub(), AreaWatch::new());
    fly(&mut tr, &mut w, two, 1_000, 15_000, (0.0, 0.0));
    w.set_own_gnss(OwnGnss::Spoofing, 15_000);
    fly(&mut tr, &mut w, two, 15_000, 20_000, (800.0, 900.0));
    let th = area_threats(&tr, 20_000);
    assert_eq!(th.len(), 1);
    assert!(th[0].detail & detail::OWN_AGREES != 0);
}

#[test]
fn two_unconfirmed_fakes_jumping_together_raise_nothing() {
    // One cheap transmitter: two new identities, each heard twice within
    // a second, the second time 1.2 km away by the same vector.
    let (mut tr, mut w) = (hub(), AreaWatch::new());
    drone_at(&mut tr, 1, 0.0, 0.0, 0, -50, 10_000);
    drone_at(&mut tr, 2, 300.0, 400.0, 1, -70, 10_000);
    w.tick(&mut tr, 10_000);
    drone_at(&mut tr, 1, 800.0, 900.0, 0, -50, 10_500);
    drone_at(&mut tr, 2, 1_100.0, 1_300.0, 1, -70, 10_500);
    w.tick(&mut tr, 10_600);
    assert!(tr.tracks().iter().all(|t| !t.confirmed() && t.spoof.jumps() == 1));
    assert!(area_threats(&tr, 10_600).is_empty());
    assert_eq!(w.status().drone_jumpers, 0);
}

#[test]
fn two_disputed_tracks_raise_nothing() {
    // Two real drones keep reporting where they are; an impersonator sends
    // both IDs (and their addresses) from 1.2 km away, by the same vector,
    // between their frames. That is a dispute on each, not two jumps.
    let two = &THREE[..2];
    let (mut tr, mut w) = (hub(), AreaWatch::new());
    fly(&mut tr, &mut w, two, 1_000, 15_000, (0.0, 0.0));
    let mut t = 15_000;
    while t < 30_000 {
        for d in two {
            drone_at(&mut tr, d.n, d.at.0, d.at.1, d.node, d.rssi, t);
            drone_at(&mut tr, d.n, d.at.0 + 800.0, d.at.1 + 900.0, d.node, d.rssi, t + 500);
        }
        w.tick(&mut tr, t + 600);
        t += 1_000;
    }
    assert!(tr.tracks().iter().all(|k| k.claims.disputed(t)));
    assert!(tr.tracks().iter().all(|k| k.claims.path_jumps() == 0));
    assert!(area_threats(&tr, t).is_empty());
    // Not with the hub's GPS saying spoofing either: disputes never count.
    w.set_own_gnss(OwnGnss::Spoofing, t);
    w.tick(&mut tr, t);
    assert!(area_threats(&tr, t).is_empty());
}

#[test]
fn a_jump_that_turns_into_a_dispute_stops_counting() {
    // While drone 1 is briefly silent an impersonator sends its ID and
    // address from 1.2 km away (a path jump once it has its own path),
    // then drone 1 reports again from where it is, between the
    // impersonator's frames: a dispute, not a jump. Then drones 2 and 3
    // jump by the same vector. Two jumpers, not three: no alarm.
    let (mut tr, mut w) = (hub(), AreaWatch::new());
    fly(&mut tr, &mut w, &THREE, 1_000, 15_000, (0.0, 0.0));
    let (d, rest) = (THREE[0], &THREE[1..]);
    let shift = (800.0, 900.0);
    let mut t = 15_000;
    while t < 19_000 {
        fly(&mut tr, &mut w, rest, t, t + 1_000, (0.0, 0.0));
        drone_at(&mut tr, d.n, d.at.0 + shift.0, d.at.1 + shift.1, d.node, d.rssi, t + 500);
        t += 1_000;
    }
    assert_eq!(tr.tracks()[0].claims.path_jumps(), 1);
    assert_eq!(w.status().drone_jumpers, 1);
    while t < 26_000 {
        drone_at(&mut tr, d.n, d.at.0, d.at.1, d.node, d.rssi, t);
        // Drones 2 and 3 jump from 22 s.
        fly(&mut tr, &mut w, rest, t, t + 1_000, if t >= 22_000 { shift } else { (0.0, 0.0) });
        drone_at(&mut tr, d.n, d.at.0 + shift.0, d.at.1 + shift.1, d.node, d.rssi, t + 500);
        assert!(area_threats(&tr, t).is_empty(), "at {t}: {:?}", w.status());
        t += 1_000;
    }
    assert!(tr.tracks()[0].claims.disputed(t));
    assert!(tr.tracks()[1..].iter().all(|k| k.claims.path_jumps() == 1));
    assert_eq!(w.status().drone_jumpers, 2);
}

#[test]
fn three_drones_landing_on_the_same_spot_is_spoofing() {
    // A spoofer sending one fixed position: different vectors, same place.
    let (mut tr, mut w) = (hub(), AreaWatch::new());
    fly(&mut tr, &mut w, &THREE, 1_000, 12_000, (0.0, 0.0));
    let mut t = 12_000;
    while t < 16_000 {
        for (i, d) in THREE.iter().enumerate() {
            drone_at(&mut tr, d.n, 3_000.0 + 20.0 * i as f64, 3_000.0, d.node, d.rssi, t);
        }
        w.tick(&mut tr, t);
        t += 1_000;
    }
    assert_eq!(area_threats(&tr, t).len(), 1);
}

#[test]
fn one_transmitter_sending_three_identities_is_not_three_receivers() {
    // Three identities from one radio: addresses differ, but they were
    // born together and are heard by the same node at the same strength.
    let fakes = [
        Real { n: 1, node: 0, rssi: -55, on_ms: 1_000, at: (0.0, 0.0) },
        Real { n: 2, node: 0, rssi: -56, on_ms: 1_000, at: (300.0, 400.0) },
        Real { n: 3, node: 0, rssi: -55, on_ms: 1_000, at: (-200.0, 250.0) },
    ];
    let (mut tr, mut w) = (hub(), AreaWatch::new());
    fly(&mut tr, &mut w, &fakes, 1_000, 15_000, (0.0, 0.0));
    fly(&mut tr, &mut w, &fakes, 15_000, 20_000, (800.0, 900.0));
    assert!(tr.tracks().iter().all(|k| k.claims.path_jumps() == 1));
    assert!(area_threats(&tr, 20_000).is_empty());
    assert_eq!(w.status().drone_jumpers, 1);
}

#[test]
fn one_drone_jumping_alone_is_not_area_spoofing() {
    let (mut tr, mut w) = (hub(), AreaWatch::new());
    fly(&mut tr, &mut w, &THREE, 1_000, 15_000, (0.0, 0.0));
    let mut t = 15_000;
    while t < 20_000 {
        drone_at(&mut tr, 1, 2_000.0, 0.0, 0, -50, t);
        for d in &THREE[1..] {
            drone_at(&mut tr, d.n, d.at.0, d.at.1, d.node, d.rssi, t);
        }
        w.tick(&mut tr, t);
        t += 1_000;
    }
    assert!(area_threats(&tr, t).is_empty());
    assert_eq!(w.status().drone_jumpers, 1);
    // Three drones jumping by different amounts at once: not alike either.
    let (mut tr, mut w) = (hub(), AreaWatch::new());
    fly(&mut tr, &mut w, &THREE, 1_000, 15_000, (0.0, 0.0));
    let mut t = 15_000;
    while t < 20_000 {
        drone_at(&mut tr, 1, 2_000.0, 0.0, 0, -50, t);
        drone_at(&mut tr, 2, 300.0, -2_500.0, 1, -62, t);
        drone_at(&mut tr, 3, -3_000.0, 250.0, 2, -74, t);
        w.tick(&mut tr, t);
        t += 1_000;
    }
    assert!(area_threats(&tr, t).is_empty());
}

#[test]
fn one_drone_jumping_while_the_hub_gps_says_spoofing_fires() {
    let one = &THREE[..1];
    let (mut tr, mut w) = (hub(), AreaWatch::new());
    fly(&mut tr, &mut w, one, 1_000, 6_000, (0.0, 0.0));
    w.set_own_gnss(OwnGnss::Spoofing, 6_000);
    let mut t = 6_000;
    while area_threats(&tr, t).is_empty() && t < 12_000 {
        fly(&mut tr, &mut w, one, t, t + 1_000, (2_000.0, 0.0));
        t += 1_000;
    }
    // Its moved claim has its own path 2 s after the jump.
    assert_eq!(t, 9_000);
    let th = area_threats(&tr, t);
    assert_eq!(th.len(), 1);
    let d = th[0].detail;
    assert!(d & detail::DRONE_JUMPS != 0 && d & detail::OWN_AGREES != 0);
    assert_eq!(th[0].level, 50);
    assert_eq!(th[0].count, 2, "the drone and the hub");
}

#[test]
fn jamming_and_spoofing_are_separate_threats() {
    let (mut tr, mut w) = (hub(), AreaWatch::new());
    let mut t = 0;
    for _ in 0..20 {
        t += 1_000;
        fly(&mut tr, &mut w, &THREE, t, t + 1, (0.0, 0.0));
        second(&mut tr, &mut w, t, &fleet(|_| 8, |_| 10, 4));
    }
    for _ in 0..7 {
        t += 1_000;
        fly(&mut tr, &mut w, &THREE, t, t + 1, (0.0, 0.0));
        second(&mut tr, &mut w, t, &fleet(|_| 0, |_| 10, 4));
    }
    for _ in 0..4 {
        t += 1_000;
        fly(&mut tr, &mut w, &THREE, t, t + 1, (1_000.0, 0.0));
        second(&mut tr, &mut w, t, &fleet(|_| 0, |_| 10, 4));
    }
    let th = area_threats(&tr, t);
    assert_eq!(th.len(), 2);
    let mut alerts = 0;
    while tr.take_new_threat(t).is_some() {
        alerts += 1;
    }
    assert_eq!(alerts, 2);
    let st = w.status();
    assert!(st.jamming && st.spoofing);
}

// ------------------------------------------------------------------ json --

#[test]
fn status_line_is_json_and_fits() {
    let mut w = AreaWatch::new();
    let mut tr = hub();
    w.set_own_gnss(OwnGnss::SuspectSpoofing, 1_000);
    w.tick(&mut tr, 1_000);
    let mut s = *w.status();
    s.jam_detail = u32::MAX;
    s.spoof_detail = u32::MAX;
    s.counted = 255;
    let mut buf = [0u8; area_gnss::MAX_STATUS_LINE];
    let n = area_gnss::status_line(&s, &mut buf).unwrap();
    let text = String::from_utf8(buf[..n].to_vec()).unwrap();
    assert!(text.ends_with('\n'));
    let v: serde_json::Value = serde_json::from_str(text.trim_end()).unwrap();
    assert_eq!(v["t"], "area_gnss");
    assert_eq!(v["own"], "suspect_spoofing");
    assert_eq!(v["jam_detail"], u64::from(u32::MAX));
    // The threat line of an AreaGnss threat fits too.
    let mut tr = hub();
    let mut w = AreaWatch::new();
    let mut t = 0;
    for _ in 0..30 {
        t += 1_000;
        second(&mut tr, &mut w, t, &fleet(|_| 8, |_| 10, 4));
    }
    for _ in 0..7 {
        t += 1_000;
        second(&mut tr, &mut w, t, &fleet(|_| 0, |_| 10, 4));
    }
    let th = area_threats(&tr, t)[0].clone();
    let mut line = [0u8; threats::MAX_THREAT_LINE];
    let ctx = tr.context(t, None);
    let n = threats::threat_line(&th, &ctx, &mut line).unwrap();
    let text = String::from_utf8(line[..n].to_vec()).unwrap();
    assert!(text.contains("\"kind\":\"area_gnss\"") && text.contains("\"by_hub\":true"), "{text}");
}

#[test]
fn sizes() {
    use core::mem::size_of;
    std::println!("AreaWatch {} B", size_of::<AreaWatch>());
    assert!(size_of::<AreaWatch>() <= 12 * 1024);
}
