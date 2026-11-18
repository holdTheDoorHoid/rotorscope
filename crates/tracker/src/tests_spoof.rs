//! Spoofing checks and track continuity, on synthetic flights.
//!
//! Two kinds of test: honest flights that must not be flagged (GPS jitter,
//! multipath RSSI swings, a drone beside the detector, an add-on module on
//! Wi-Fi and BLE, a DJI drone heard on Remote ID and DroneID, ...) and
//! spoofer scenarios that must be. Every serial and address here is made up
//! (`FAKE`, `TEST`); none comes from a recording.

extern crate std;
use std::string::String;
use std::vec::Vec as SVec;

use super::*;
use crate::spoof::{Check, REPORT_MIN, SerialFault, cta2063_check};
use crate::tests::CITY_HALL;
use link::{DroneIdRecord, DroneIdSighting};
use rid::message::*;

/// UTC at hub time 0: 2026-11-09T14:03:07.250Z (187.25 s past the hour).
const UTC0: u64 = 1_794_232_987_250;
/// Valid ANSI/CTA-2063-A, obviously fake: maker 1ZZZ, 9 characters.
const SN: &str = "1ZZZ9FAKE00001";
const SN2: &str = "1ZZZ9FAKE00002";
/// A DJI-style serial that fits DroneID's 16 bytes: 1581, B (11), 11 chars.
const DJI_SN: &str = "1581BFAKE0000001";

/// Locally administered Wi-Fi address (randomised; no maker).
const MAC_A: [u8; 6] = [0x02, 0x11, 0x22, 0x33, 0x44, 0x01];
const MAC_B: [u8; 6] = [0x02, 0x11, 0x22, 0x33, 0x44, 0x02];
/// DJI Wi-Fi block (60:60:1F).
const DJI_MAC: [u8; 6] = [0x60, 0x60, 0x1f, 0x00, 0x0f, 0xaa];
/// Espressif block (24:0A:C4), as an ESP32 uses by default.
const ESP_MAC: [u8; 6] = [0x24, 0x0a, 0xc4, 0x00, 0x0f, 0xbb];
/// BLE random static addresses (top two bits set).
const BLE_A: [u8; 6] = [0xc3, 0x01, 0x02, 0x03, 0x04, 0x05];
const BLE_B: [u8; 6] = [0xd7, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e];

fn at(east_m: f64, north_m: f64) -> LatLon {
    LatLon {
        lat: CITY_HALL.0 + north_m / 111_132.0,
        lon: CITY_HALL.1 + east_m / (111_132.0 * CITY_HALL.0.to_radians().cos()),
    }
}

/// Seconds past the hour at hub time `ms`, as a Location carries it.
fn ts(ms: u64) -> f32 {
    let s = ((UTC0 + ms) % 3_600_000) as f64 / 1000.0;
    ((s * 10.0).floor() / 10.0) as f32
}

fn basic(sn: &str) -> Message {
    Message::BasicId(BasicId {
        id_type: IdType::SerialNumber,
        ua_type: UaType::HelicopterOrMultirotor,
        uas_id: FixedStr::new(sn.as_bytes()).unwrap(),
    })
}

fn session(id: u8) -> Message {
    Message::BasicId(BasicId {
        id_type: IdType::SpecificSession,
        ua_type: UaType::HelicopterOrMultirotor,
        uas_id: FixedStr::new(&[0x01, 0xAB, id, 0x10, 0x20]).unwrap(),
    })
}

#[derive(Clone, Copy)]
struct Fix {
    p: LatLon,
    alt: f32,
    height: f32,
    speed: f32,
    dir: Option<f32>,
    ms: u64,
}

fn loc(f: Fix) -> Message {
    Message::Location(Location {
        status: if f.height > 1.0 {
            Status::Airborne
        } else {
            Status::Ground
        },
        height_type: HeightType::AboveTakeoff,
        direction: f.dir,
        speed_horizontal: Some(f.speed),
        speed_vertical: Some(0.0),
        position: Some(f.p),
        altitude_pressure: Some(f.alt - 20.0),
        altitude_geodetic: Some(f.alt),
        height: Some(f.height),
        horizontal_accuracy: HorizontalAccuracy::M3,
        vertical_accuracy: VerticalAccuracy::M10,
        baro_accuracy: VerticalAccuracy::M10,
        speed_accuracy: SpeedAccuracy::Mps1,
        timestamp: Some(ts(f.ms)),
        timestamp_accuracy: Some(0.2),
    })
}

fn system_at(op: LatLon) -> Message {
    Message::System(System {
        operator_location_type: OperatorLocationType::Takeoff,
        classification_type: ClassificationType::Undeclared,
        operator_position: Some(op),
        area_count: 1,
        area_radius: 0,
        area_ceiling: None,
        area_floor: None,
        category: Category::Undeclared,
        class: UaClass::Undeclared,
        operator_altitude: Some(15.0),
        timestamp: Some(300_000_000),
    })
}

struct Sim {
    tr: Tracker,
    /// Deterministic noise.
    seed: u64,
}

impl Sim {
    fn new() -> Self {
        let mut tr: Tracker = Tracker::default();
        tr.set_utc(UTC0, 0);
        tr.set_own_position(CITY_HALL.0, CITY_HALL.1, Some(40.0), 0);
        Self { tr, seed: 0x5eed }
    }

    /// Uniform in -1..1.
    fn noise(&mut self) -> f64 {
        self.seed = self
            .seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        ((self.seed >> 33) as f64 / (1u64 << 31) as f64) * 2.0 - 1.0
    }

    #[allow(clippy::too_many_arguments)]
    fn frame(
        &mut self,
        node: u8,
        transport: Transport,
        mac: [u8; 6],
        rssi: i8,
        counter: u8,
        msgs: &[Message],
        ms: u64,
    ) -> TrackId {
        self.tr
            .set_own_position(CITY_HALL.0, CITY_HALL.1, Some(40.0), ms);
        let obs = Observation {
            node,
            transport,
            channel: if matches!(transport, Transport::WifiBeacon | Transport::WifiNan) {
                6
            } else {
                37
            },
            rssi,
            mac,
            counter,
            rx_ms: ms,
        };
        self.tr.ingest_messages(&obs, msgs, 0).track().unwrap()
    }

    fn track(&self, id: TrackId) -> &Track {
        self.tr.get(id).unwrap()
    }

    fn drain(&mut self, ms: u64) -> SVec<SpoofEvent> {
        let mut v = SVec::new();
        while let Some(e) = self.tr.take_spoof_flag(ms) {
            v.push(e);
        }
        v
    }
}

/// The RSSI an honest 20 dBm transmitter gives at `d` m (log-distance,
/// exponent 2.7), plus multipath noise of +/- `swing` dB.
fn honest_rssi(d: f64, noise: f64, swing: f64) -> i8 {
    let fspl = 20.0 * d.max(1.0).log10() + 40.05;
    let extra = 7.0 * (d.max(1.0) / 1.0).log10();
    (20.0 - fspl - extra + noise * swing).clamp(-110.0, -10.0) as i8
}

fn assert_clean(sim: &mut Sim, id: TrackId, ms: u64) {
    let t = sim.track(id);
    assert!(
        t.spoof.score() < REPORT_MIN,
        "score {} flags {:?}",
        t.spoof.score(),
        t.spoof.flags()
    );
    let evs = sim.drain(ms);
    assert!(evs.is_empty(), "{evs:?}");
}

/// An honest Wi-Fi beacon flight: one frame per second with Basic ID,
/// Location and System, the counter advancing.
#[allow(clippy::too_many_arguments)]
fn honest_flight(
    sim: &mut Sim,
    node: u8,
    mac: [u8; 6],
    sn: &str,
    start_ms: u64,
    secs: u64,
    pos: impl Fn(u64) -> (LatLon, f32, Option<f32>),
    swing_db: f64,
) -> TrackId {
    let op = at(-30.0, -40.0);
    let mut id = None;
    for k in 0..secs * 2 {
        let ms = start_ms + k * 500;
        let (mut p, speed, dir) = pos(ms - start_ms);
        // GPS jitter: a few metres.
        p.lat += sim.noise() * 2.5 / 111_132.0;
        p.lon += sim.noise() * 2.5 / 85_000.0;
        let d = spoof_dist(p);
        let rssi = honest_rssi(d, sim.noise(), swing_db);
        let alt = 100.0 + sim.noise() as f32 * 4.0;
        let msgs = [
            basic(sn),
            loc(Fix {
                p,
                alt,
                height: 60.0 + sim.noise() as f32,
                speed,
                dir,
                ms,
            }),
            system_at(op),
        ];
        id = Some(sim.frame(node, Transport::WifiBeacon, mac, rssi, k as u8, &msgs, ms));
    }
    id.unwrap()
}

fn spoof_dist(p: LatLon) -> f64 {
    spoof::dist_m(
        LatLon {
            lat: CITY_HALL.0,
            lon: CITY_HALL.1,
        },
        p,
    )
}

// ------------------------------------------------- honest: no alarms --

#[test]
fn hover_with_gps_jitter_and_multipath_is_clean() {
    let mut sim = Sim::new();
    let id = honest_flight(
        &mut sim,
        0,
        MAC_A,
        SN,
        1_000,
        180,
        |_| (at(200.0, 250.0), 0.25, None),
        // Multipath: +/- 10 dB swings.
        10.0,
    );
    assert!(
        sim.track(id).spoof.flags().is_empty(),
        "{:?}",
        sim.track(id).spoof.flags()
    );
    assert_clean(&mut sim, id, 200_000);
}

#[test]
fn drone_beside_the_detector_is_clean() {
    let mut sim = Sim::new();
    // 5 m away, RSSI -15 dBm: far louder than at range, but too close to judge.
    let id = honest_flight(
        &mut sim,
        0,
        MAC_A,
        SN,
        1_000,
        60,
        |_| (at(3.0, 4.0), 0.0, None),
        3.0,
    );
    assert!(sim.track(id).spoof.flag(Check::RssiTooStrong).is_none());
    assert_clean(&mut sim, id, 70_000);
}

#[test]
fn fast_straight_and_turning_flight_is_clean() {
    let mut sim = Sim::new();
    // 20 m/s east for 30 s, then a 40 m/s orbit of radius 150 m.
    let id = honest_flight(
        &mut sim,
        0,
        MAC_A,
        SN,
        1_000,
        90,
        |ms| {
            let t = ms as f64 / 1000.0;
            if t < 30.0 {
                (at(-300.0 + 20.0 * t, 400.0), 20.0, Some(90.0))
            } else {
                let w = 40.0 / 150.0; // rad/s
                let a = (t - 30.0) * w;
                let p = at(300.0 + 150.0 * a.sin(), 400.0 + 150.0 * a.cos());
                // Tangent heading for a clockwise orbit from north.
                let dir = ((a.to_degrees() + 90.0) % 360.0) as f32;
                (p, 40.0, Some(dir))
            }
        },
        6.0,
    );
    assert_clean(&mut sim, id, 100_000);
}

/// An add-on module (ESP32-based, as ArduRemoteID boards are) sends Wi-Fi
/// beacons and BLE 4 adverts, one message per advert, with a rotating BLE
/// address. Two transmitters, one drone: one track, nothing reported.
#[test]
fn addon_module_wifi_and_ble_is_one_clean_track() {
    let mut sim = Sim::new();
    let op = at(0.0, -60.0);
    let mut last = None;
    for k in 0..240u64 {
        let ms = 1_000 + k * 250;
        let t = k as f64 * 0.25;
        let p = at(100.0 + 8.0 * t, 300.0);
        let f = Fix {
            p,
            alt: 110.0,
            height: 70.0,
            speed: 8.0,
            dir: Some(90.0),
            ms,
        };
        let rssi = honest_rssi(spoof_dist(p), sim.noise(), 6.0);
        let id = if k % 2 == 0 {
            sim.frame(
                0,
                Transport::WifiBeacon,
                ESP_MAC,
                rssi,
                (k / 2) as u8,
                &[basic(SN), loc(f), system_at(op)],
                ms,
            )
        } else {
            // BLE 4: one message per advert, rotating; the address rotates once.
            let ble = if k < 120 { BLE_A } else { BLE_B };
            let msg = match (k / 2) % 3 {
                0 => basic(SN),
                1 => loc(f),
                _ => system_at(op),
            };
            sim.frame(
                1,
                Transport::BleLegacy,
                ble,
                rssi - 8,
                (k / 6) as u8,
                &[msg],
                ms,
            )
        };
        last = Some(id);
    }
    let id = last.unwrap();
    assert_eq!(sim.tr.len(), 1, "{:?}", sim.tr.tracks());
    let t = sim.track(id);
    // The Espressif address is noted, weakly.
    assert_eq!(
        t.spoof.flag(Check::DevBoard).map(|f| f.confidence),
        Some(15)
    );
    assert!(t.spoof.flag(Check::AddressAlternation).is_none());
    assert_clean(&mut sim, id, 70_000);
}

/// A DJI drone heard on Remote ID and on OcuSync DroneID, both saying the
/// same thing.
#[test]
fn dji_remote_id_and_droneid_agreeing_is_clean() {
    let mut sim = Sim::new();
    let op = at(-20.0, -20.0);
    let rad = |deg: f64| (deg.to_radians() * 1e7).round() as i32;
    let mut serial = [0u8; 16];
    serial.copy_from_slice(DJI_SN.as_bytes());
    let mut id = None;
    for k in 0..120u64 {
        let ms = 1_000 + k * 500;
        let t = k as f64 * 0.5;
        let p = at(150.0 + 5.0 * t, 500.0);
        let rssi = honest_rssi(spoof_dist(p), sim.noise(), 6.0);
        let f = Fix {
            p,
            alt: 120.0,
            height: 80.0,
            speed: 5.0,
            dir: Some(90.0),
            ms,
        };
        let msgs = [basic(DJI_SN), loc(f), system_at(op)];
        id = Some(sim.frame(0, Transport::WifiBeacon, DJI_MAC, rssi, k as u8, &msgs, ms));
        if k % 3 == 0 {
            // DroneID's own GPS reading, a few metres off.
            let q = at(150.0 + 5.0 * t + 4.0, 500.0 - 3.0);
            let s = DroneIdSighting {
                freq: 24_445,
                rssi: rssi - 3,
                snr_db: 12,
                uptime_us: 0,
                record: DroneIdRecord::Flight {
                    serial,
                    lat_raw: rad(q.lat),
                    lon_raw: rad(q.lon),
                    altitude_raw: 394,
                    height_raw: 262,
                    v_north: 0,
                    v_east: 500,
                    v_up: 0,
                    yaw_raw: 9000,
                    pilot_lat_raw: rad(op.lat),
                    pilot_lon_raw: rad(op.lon),
                    home_lat_raw: rad(op.lat),
                    home_lon_raw: rad(op.lon),
                    product_type: 68,
                },
            };
            sim.tr.ingest_droneid(2, &s, ms + 100);
        }
    }
    let id = id.unwrap();
    assert_eq!(sim.tr.len(), 1);
    let t = sim.track(id);
    assert!(t.dji.ocusync.heard());
    assert!(t.spoof.flags().is_empty(), "{:?}", t.spoof.flags());
    assert_clean(&mut sim, id, 70_000);
}

/// Two nodes hear every beacon: identical content, identical counter.
#[test]
fn two_nodes_hearing_the_same_frames_is_clean() {
    let mut sim = Sim::new();
    let op = at(0.0, -30.0);
    let mut id = None;
    for k in 0..120u64 {
        let ms = 1_000 + k * 500;
        let p = at(300.0, 300.0 + k as f64);
        let f = Fix {
            p,
            alt: 100.0,
            height: 50.0,
            speed: 2.0,
            dir: Some(0.0),
            ms,
        };
        let msgs = [basic(SN), loc(f), system_at(op)];
        let rssi = honest_rssi(spoof_dist(p), 0.0, 0.0);
        sim.frame(0, Transport::WifiBeacon, MAC_A, rssi, k as u8, &msgs, ms);
        id = Some(sim.frame(
            1,
            Transport::WifiBeacon,
            MAC_A,
            rssi - 4,
            k as u8,
            &msgs,
            ms + 20,
        ));
    }
    let id = id.unwrap();
    assert!(
        sim.track(id).spoof.flags().is_empty(),
        "{:?}",
        sim.track(id).spoof.flags()
    );
    assert_clean(&mut sim, id, 70_000);
}

/// No UTC and no own fix: the clock and signal checks stay silent rather
/// than guess.
#[test]
fn without_utc_or_fix_the_checks_that_need_them_are_silent() {
    let mut tr: Tracker = Tracker::default();
    let p = at(5_000.0, 0.0);
    for k in 0..30u64 {
        let ms = 1_000 + k * 1000;
        let mut l = loc(Fix {
            p,
            alt: 100.0,
            height: 50.0,
            speed: 0.0,
            dir: None,
            ms,
        });
        if let Message::Location(l) = &mut l {
            l.timestamp = Some(1.0); // wildly wrong, but nothing to compare
        }
        let obs = Observation {
            node: 0,
            transport: Transport::WifiBeacon,
            channel: 6,
            rssi: -30,
            mac: MAC_A,
            counter: k as u8,
            rx_ms: ms,
        };
        tr.ingest_messages(&obs, &[basic(SN), l, system_at(at(0.0, 0.0))], 0);
    }
    let t = &tr.tracks()[0];
    assert!(t.spoof.flag(Check::RssiTooStrong).is_none());
    assert!(t.spoof.flag(Check::ClockSkew).is_none());
}

// ------------------------------------------------- spoofers: flagged --

/// A spoofer clones a real drone's serial 2 km away, from another address.
/// Before WP-10 the track's position bounced between the two and the real
/// drone was flagged (position jump 90, address alternation 70). Now the
/// first path holds, the other is a rival claim, and the track is disputed:
/// two transmitters claim one ID. No spoof flag on either path.
#[test]
fn cloned_serial_two_places_is_a_dispute_not_a_flag() {
    let mut sim = Sim::new();
    let op = at(0.0, -50.0);
    let mut id = None;
    for k in 0..40u64 {
        let ms = 1_000 + k * 500;
        let (mac, p) = if k % 2 == 0 {
            (MAC_A, at(200.0, 300.0))
        } else {
            (MAC_B, at(2_000.0, 1_500.0))
        };
        let f = Fix {
            p,
            alt: 100.0,
            height: 50.0,
            speed: 1.0,
            dir: None,
            ms,
        };
        let rssi = honest_rssi(spoof_dist(p), 0.0, 0.0);
        id = Some(sim.frame(
            0,
            Transport::WifiBeacon,
            mac,
            rssi,
            k as u8,
            &[basic(SN), loc(f), system_at(op)],
            ms,
        ));
    }
    let id = id.unwrap();
    let t = sim.track(id);
    assert!(t.spoof.flag(Check::PositionJump).is_none(), "{:?}", t.spoof.flags());
    assert!(t.spoof.flag(Check::AddressAlternation).is_none(), "{:?}", t.spoof.flags());
    assert!(t.spoof.score() < REPORT_MIN, "{:?}", t.spoof.flags());
    let shown = t.location.as_ref().unwrap().value.position.unwrap();
    assert!(spoof::dist_m(shown, at(200.0, 300.0)) < 10.0);
    let d = t.claims.dispute(20_500).expect("disputed");
    assert!(d.what & claims::what::POSITION != 0);
    let r = d.rival.unwrap();
    assert!(spoof::dist_m(r.position, at(2_000.0, 1_500.0)) < 10.0);
    assert_eq!(r.src, ClaimSrc::RemoteId);
    assert!(sim.drain(30_000).is_empty());
    let ev = sim.tr.take_dispute(20_500).unwrap();
    let text = claims::event_text(&ev, sim.tr.get(id));
    assert!(text.contains("two transmitters claim this ID"), "{text}");
    assert!(text.contains("another Remote ID claim 2163 m from the shown one"), "{text}");
    assert_eq!(sim.tr.take_dispute(20_500), None, "once");
}

/// A drone that says it is 5 km away, heard at -35 dBm.
#[test]
fn far_claim_with_loud_signal_is_flagged() {
    let mut sim = Sim::new();
    let id = honest_flight(
        &mut sim,
        0,
        MAC_A,
        SN,
        1_000,
        10,
        |_| (at(5_000.0, 0.0), 0.0, None),
        0.0,
    );
    // Same claim, but the frames are loud.
    let mut last = id;
    for k in 0..10u64 {
        let ms = 20_000 + k * 1000;
        let f = Fix {
            p: at(5_000.0, 0.0),
            alt: 100.0,
            height: 50.0,
            speed: 0.0,
            dir: None,
            ms,
        };
        last = sim.frame(
            0,
            Transport::WifiBeacon,
            MAC_A,
            -35,
            (50 + k) as u8,
            &[basic(SN), loc(f)],
            ms,
        );
    }
    let f = *sim.track(last).spoof.flag(Check::RssiTooStrong).unwrap();
    // 5 km less the 300 m node allowance: limit about -61.5 dBm.
    assert_eq!(f.confidence, 85, "{f:?}");
    assert_eq!(f.detail, 26, "{f:?}");
    let ev = sim.drain(31_000);
    assert!(ev.iter().any(|e| e.check == Check::RssiTooStrong));
}

/// With the node's position known, the allowance is not needed.
#[test]
fn node_position_tightens_the_signal_check() {
    let mut sim = Sim::new();
    sim.tr.set_node_position(3, CITY_HALL.0, CITY_HALL.1);
    let mut id = None;
    for k in 0..6u64 {
        let ms = 1_000 + k * 1000;
        let f = Fix {
            p: at(600.0, 0.0),
            alt: 100.0,
            height: 50.0,
            speed: 0.0,
            dir: None,
            ms,
        };
        // Limit at 600 m is about -43.6 dBm.
        id = Some(sim.frame(
            3,
            Transport::WifiBeacon,
            MAC_A,
            -38,
            k as u8,
            &[basic(SN), loc(f)],
            ms,
        ));
    }
    assert!(
        sim.track(id.unwrap())
            .spoof
            .flag(Check::RssiTooStrong)
            .is_some()
    );
    // The same frames from a node with no position: 600 - 300 m = 300 m,
    // limit about -37.6 dBm, so -38 passes.
    let mut sim = Sim::new();
    for k in 0..6u64 {
        let ms = 1_000 + k * 1000;
        let f = Fix {
            p: at(600.0, 0.0),
            alt: 100.0,
            height: 50.0,
            speed: 0.0,
            dir: None,
            ms,
        };
        id = Some(sim.frame(
            3,
            Transport::WifiBeacon,
            MAC_A,
            -38,
            k as u8,
            &[basic(SN), loc(f)],
            ms,
        ));
    }
    assert!(
        sim.track(id.unwrap())
            .spoof
            .flag(Check::RssiTooStrong)
            .is_none()
    );
}

/// One ESP8266 sketch, one address, five invented drones.
#[test]
fn one_address_many_ids_is_flagged() {
    let mut sim = Sim::new();
    let ids = [
        "1ZZZ5AAAA1",
        "1ZZZ5AAAA2",
        "1ZZZ5AAAA3",
        "1ZZZ5AAAA4",
        "1ZZZ5AAAA5",
    ];
    for k in 0..50u64 {
        let ms = 1_000 + k * 100;
        let n = (k % 5) as usize;
        let f = Fix {
            p: at(500.0 * n as f64, 800.0),
            alt: 100.0,
            height: 50.0,
            speed: 0.0,
            dir: None,
            ms,
        };
        sim.frame(
            0,
            Transport::WifiBeacon,
            ESP_MAC,
            -60,
            k as u8,
            &[basic(ids[n]), loc(f)],
            ms,
        );
    }
    assert_eq!(sim.tr.len(), 5);
    for t in sim.tr.tracks() {
        let f = t.spoof.flag(Check::SharedAddress).unwrap();
        assert_eq!(f.detail, 5, "{f:?}");
        assert!(f.confidence >= 80, "{f:?}");
    }
}

/// Two honest drones that share a fixed MAC (as one maker's do) heard at
/// different strengths: weak only.
#[test]
fn two_drones_sharing_a_fixed_mac_stay_weak() {
    let mut sim = Sim::new();
    for k in 0..60u64 {
        let ms = 1_000 + k * 500;
        let (sn, p, rssi) = if k % 2 == 0 {
            (SN, at(100.0, 200.0), -55)
        } else {
            (SN2, at(-400.0, 600.0), -75)
        };
        let f = Fix {
            p,
            alt: 100.0,
            height: 50.0,
            speed: 0.0,
            dir: None,
            ms,
        };
        sim.frame(
            0,
            Transport::WifiBeacon,
            MAC_A,
            rssi,
            (k / 2) as u8,
            &[basic(sn), loc(f), system_at(at(0.0, 0.0))],
            ms,
        );
    }
    assert_eq!(sim.tr.len(), 2);
    for t in sim.tr.tracks() {
        assert_eq!(t.spoof.flag(Check::SharedAddress).unwrap().confidence, 25);
        assert!(t.spoof.score() < REPORT_MIN);
    }
}

/// Eight "drones" appear within 2 s on one node at the same strength,
/// claiming positions from 300 m to 5 km away.
#[test]
fn spoofer_swarm_is_flagged() {
    let mut sim = Sim::new();
    for round in 0..5u64 {
        for n in 0..8u64 {
            let ms = 1_000 + round * 1_000 + n * 50;
            let sn = std::format!("1ZZZ5SWRM{n}");
            let f = Fix {
                p: at(300.0 + 650.0 * n as f64, 100.0),
                alt: 100.0,
                height: 50.0,
                speed: 0.0,
                dir: None,
                ms,
            };
            let mac = [0x02, 0, 0, 0, 0x5a, n as u8];
            sim.frame(
                0,
                Transport::WifiBeacon,
                mac,
                -62 + (n % 2) as i8,
                round as u8,
                &[basic(&sn), loc(f)],
                ms,
            );
        }
    }
    assert_eq!(sim.tr.len(), 8);
    for t in sim.tr.tracks() {
        let f = t.spoof.flag(Check::SpooferSwarm).unwrap();
        assert_eq!(f.detail, 8);
        assert_eq!(f.confidence, 65, "{f:?}");
    }
}

/// Drones powered up together close by (a light show): weak only.
#[test]
fn light_show_power_up_stays_weak() {
    let mut sim = Sim::new();
    for round in 0..5u64 {
        for n in 0..8u64 {
            let ms = 1_000 + round * 1_000 + n * 50;
            let sn = std::format!("1ZZZ5SHOW{n}");
            let f = Fix {
                p: at(400.0 + 3.0 * n as f64, 400.0),
                alt: 40.0,
                height: 0.0,
                speed: 0.0,
                dir: None,
                ms,
            };
            let mac = [0x02, 0, 0, 0, 0x5b, n as u8];
            sim.frame(
                0,
                Transport::WifiBeacon,
                mac,
                -70,
                round as u8,
                &[basic(&sn), loc(f)],
                ms,
            );
        }
    }
    for t in sim.tr.tracks() {
        assert!(t.spoof.flag(Check::SpooferSwarm).unwrap().confidence < REPORT_MIN);
        assert!(t.spoof.score() < REPORT_MIN);
    }
}

/// A recording of three Location frames played in a loop.
#[test]
fn replayed_loop_is_flagged() {
    let mut sim = Sim::new();
    let rec: SVec<Message> = (0..3u64)
        .map(|k| {
            loc(Fix {
                p: at(300.0 + 5.0 * k as f64, 300.0),
                alt: 100.0,
                height: 50.0,
                speed: 5.0,
                dir: Some(90.0),
                // Recorded 6 s apart, played back every second.
                ms: 1_000 + k * 6000,
            })
        })
        .collect();
    let mut id = None;
    for k in 0..30u64 {
        let ms = 1_000 + k * 1000;
        let msgs = [
            basic(SN),
            rec[(k % 3) as usize].clone(),
            system_at(at(0.0, 0.0)),
        ];
        id = Some(sim.frame(0, Transport::WifiBeacon, MAC_A, -70, k as u8, &msgs, ms));
    }
    let t = sim.track(id.unwrap());
    let r = t.spoof.flag(Check::Replay).unwrap();
    assert!(r.confidence >= 75, "{r:?}");
    assert!(
        t.spoof.flag(Check::TimeInconsistent).is_some(),
        "{:?}",
        t.spoof.flags()
    );
}

/// Positions that move at 30 m/s while the drone reports 0 m/s.
#[test]
fn positions_faster_than_reported_speed_are_flagged() {
    let mut sim = Sim::new();
    let id = honest_flight(
        &mut sim,
        0,
        MAC_A,
        SN,
        1_000,
        60,
        |ms| (at(-500.0 + 30.0 * ms as f64 / 1000.0, 600.0), 0.0, None),
        0.0,
    );
    let f = *sim.track(id).spoof.flag(Check::MotionMismatch).unwrap();
    assert!(f.confidence >= 60, "{f:?}");
    assert!((25..=35).contains(&f.detail), "{f:?}");
}

/// Moving north while claiming to fly south.
#[test]
fn course_against_reported_direction_is_flagged() {
    let mut sim = Sim::new();
    let id = honest_flight(
        &mut sim,
        0,
        MAC_A,
        SN,
        1_000,
        60,
        |ms| {
            (
                at(300.0, 300.0 + 10.0 * ms as f64 / 1000.0),
                10.0,
                Some(180.0),
            )
        },
        0.0,
    );
    let f = *sim.track(id).spoof.flag(Check::MotionMismatch).unwrap();
    assert!(f.detail <= -150, "{f:?}");
}

#[test]
fn clock_far_from_gps_time_is_flagged() {
    let mut sim = Sim::new();
    let mut id = None;
    for k in 0..10u64 {
        let ms = 1_000 + k * 1000;
        let mut l = loc(Fix {
            p: at(300.0, 300.0),
            alt: 100.0,
            height: 50.0,
            speed: 0.0,
            dir: None,
            ms,
        });
        if let Message::Location(l) = &mut l {
            // Ten minutes slow, advancing normally.
            l.timestamp = Some(ts(ms + 3_000_000));
        }
        id = Some(sim.frame(
            0,
            Transport::WifiBeacon,
            MAC_A,
            -70,
            k as u8,
            &[basic(SN), l],
            ms,
        ));
    }
    let f = *sim.track(id.unwrap()).spoof.flag(Check::ClockSkew).unwrap();
    assert_eq!(f.confidence, 40);
    assert!((-601..=-599).contains(&f.detail), "{f:?}");
    // And the clock advancing at the right rate is not "inconsistent".
    assert!(
        sim.track(id.unwrap())
            .spoof
            .flag(Check::TimeInconsistent)
            .is_none()
    );
}

/// DroneID puts the drone 1.2 km from where Remote ID does, both
/// continuing. A DJI drone sends both from one flight controller, so one of
/// them is forged; which one is not knowable. Before WP-10 the track got a
/// `drone_id_disagrees` flag (75); now it is a dispute between the two
/// sources, and Remote ID's position stays shown.
#[test]
fn droneid_disagreeing_with_remote_id_is_a_dispute() {
    let mut sim = Sim::new();
    let rad = |deg: f64| (deg.to_radians() * 1e7).round() as i32;
    let mut serial = [0u8; 16];
    serial.copy_from_slice(DJI_SN.as_bytes());
    let mut id = None;
    for k in 0..10u64 {
        let ms = 1_000 + k * 1000;
        let f = Fix {
            p: at(300.0, 300.0),
            alt: 100.0,
            height: 50.0,
            speed: 0.0,
            dir: None,
            ms,
        };
        id = Some(sim.frame(
            0,
            Transport::WifiBeacon,
            DJI_MAC,
            -70,
            k as u8,
            &[basic(DJI_SN), loc(f)],
            ms,
        ));
        // DroneID (the real drone) says 1.2 km elsewhere.
        let q = at(1_500.0, 300.0);
        let s = DroneIdSighting {
            freq: 24_445,
            rssi: -70,
            snr_db: 12,
            uptime_us: 0,
            record: DroneIdRecord::Flight {
                serial,
                lat_raw: rad(q.lat),
                lon_raw: rad(q.lon),
                altitude_raw: 328,
                height_raw: 164,
                v_north: 0,
                v_east: 0,
                v_up: 0,
                yaw_raw: 0,
                pilot_lat_raw: 0,
                pilot_lon_raw: 0,
                home_lat_raw: 0,
                home_lon_raw: 0,
                product_type: 68,
            },
        };
        sim.tr.ingest_droneid(2, &s, ms + 200);
    }
    let t = sim.track(id.unwrap());
    assert!(t.spoof.flag(Check::DroneIdDisagrees).is_none(), "{:?}", t.spoof.flags());
    assert_eq!(t.location_src, Source::RemoteId);
    let d = t.claims.dispute(10_300).expect("disputed");
    let r = d.rival.unwrap();
    assert_eq!(r.src, ClaimSrc::DroneId);
    let apart = claims::apart_m(t, 10_300).unwrap();
    assert!((1_100.0..=1_300.0).contains(&apart), "{apart}");
}

/// DroneID 150 m from Remote ID: inside the path limit, so not a rival
/// claim, but further than one drone's two reports differ. The
/// `drone_id_disagrees` check still covers that band.
#[test]
fn droneid_a_little_off_remote_id_is_still_flagged() {
    let mut sim = Sim::new();
    let rad = |deg: f64| (deg.to_radians() * 1e7).round() as i32;
    let mut serial = [0u8; 16];
    serial.copy_from_slice(DJI_SN.as_bytes());
    let mut id = None;
    for k in 0..10u64 {
        let ms = 1_000 + k * 1000;
        let f = Fix { p: at(300.0, 300.0), alt: 100.0, height: 50.0, speed: 0.0, dir: None, ms };
        id = Some(sim.frame(0, Transport::WifiBeacon, DJI_MAC, -70, k as u8, &[basic(DJI_SN), loc(f)], ms));
        let q = at(450.0, 300.0);
        let s = DroneIdSighting {
            freq: 24_445,
            rssi: -70,
            snr_db: 12,
            uptime_us: 0,
            record: DroneIdRecord::Flight {
                serial,
                lat_raw: rad(q.lat),
                lon_raw: rad(q.lon),
                altitude_raw: 328,
                height_raw: 164,
                v_north: 0,
                v_east: 0,
                v_up: 0,
                yaw_raw: 0,
                pilot_lat_raw: 0,
                pilot_lon_raw: 0,
                home_lat_raw: 0,
                home_lon_raw: 0,
                product_type: 68,
            },
        };
        sim.tr.ingest_droneid(2, &s, ms + 200);
    }
    let t = sim.track(id.unwrap());
    let f = *t.spoof.flag(Check::DroneIdDisagrees).unwrap();
    assert!(f.confidence >= 75, "{f:?}");
    assert!((130..=170).contains(&f.detail), "{f:?}");
    assert!(!t.claims.disputed(10_300));
}

#[test]
fn dji_serial_from_an_esp32_is_flagged() {
    let mut sim = Sim::new();
    let f = Fix {
        p: at(300.0, 300.0),
        alt: 100.0,
        height: 50.0,
        speed: 0.0,
        dir: None,
        ms: 1_000,
    };
    let id = sim.frame(
        0,
        Transport::WifiBeacon,
        ESP_MAC,
        -70,
        0,
        &[basic("1581F0FAKE00000000AA"), loc(f)],
        1_000,
    );
    let t = sim.track(id);
    assert_eq!(
        t.spoof
            .flag(Check::VendorMismatch)
            .map(|f| (f.confidence, f.detail)),
        Some((45, 2))
    );
    // A non-DJI serial from a DJI address.
    let id = sim.frame(
        1,
        Transport::WifiBeacon,
        DJI_MAC,
        -70,
        0,
        &[basic(SN2), loc(f)],
        1_100,
    );
    assert_eq!(
        sim.track(id)
            .spoof
            .flag(Check::VendorMismatch)
            .map(|f| f.detail),
        Some(3)
    );
    // A DJI Wi-Fi DroneID element from an ESP address.
    let t = sim.tr.tracks.iter_mut().find(|t| t.id == id).unwrap();
    spoof::dji_wifi_frame(t, &ESP_MAC);
    assert_eq!(
        t.spoof
            .flag(Check::VendorMismatch)
            .map(|f| (f.confidence, f.detail)),
        Some((45, 4))
    );
    // From a DJI address: nothing.
    let mut clean = Track::new(TrackId(99), 0);
    spoof::dji_wifi_frame(&mut clean, &DJI_MAC);
    assert!(clean.spoof.flags().is_empty());
}

#[test]
fn operator_on_the_drone_and_far_away_are_flagged() {
    let mut sim = Sim::new();
    let mut id = None;
    for k in 0..10u64 {
        let ms = 1_000 + k * 1000;
        let p = at(300.0 + 10.0 * k as f64, 300.0);
        let f = Fix {
            p,
            alt: 100.0,
            height: 50.0,
            speed: 10.0,
            dir: Some(90.0),
            ms,
        };
        id = Some(sim.frame(
            0,
            Transport::WifiBeacon,
            MAC_A,
            -70,
            k as u8,
            &[basic(SN), loc(f), system_at(p)],
            ms,
        ));
    }
    assert!(
        sim.track(id.unwrap())
            .spoof
            .flag(Check::OperatorOnDrone)
            .is_some()
    );

    let mut sim = Sim::new();
    for k in 0..10u64 {
        let ms = 1_000 + k * 1000;
        let f = Fix {
            p: at(300.0, 300.0),
            alt: 100.0,
            height: 50.0,
            speed: 0.0,
            dir: None,
            ms,
        };
        id = Some(sim.frame(
            0,
            Transport::WifiBeacon,
            MAC_A,
            -70,
            k as u8,
            &[basic(SN), loc(f), system_at(at(40_000.0, 0.0))],
            ms,
        ));
    }
    let f = *sim
        .track(id.unwrap())
        .spoof
        .flag(Check::OperatorFar)
        .unwrap();
    assert_eq!(f.detail, 39);
}

#[test]
fn operator_teleporting_is_flagged() {
    let mut sim = Sim::new();
    let mut id = None;
    for k in 0..10u64 {
        let ms = 1_000 + k * 1000;
        let f = Fix {
            p: at(300.0, 300.0),
            alt: 100.0,
            height: 50.0,
            speed: 0.0,
            dir: None,
            ms,
        };
        let op = if k % 2 == 0 {
            at(0.0, 0.0)
        } else {
            at(3_000.0, 0.0)
        };
        id = Some(sim.frame(
            0,
            Transport::WifiBeacon,
            MAC_A,
            -70,
            k as u8,
            &[basic(SN), loc(f), system_at(op)],
            ms,
        ));
    }
    assert!(
        sim.track(id.unwrap())
            .spoof
            .flag(Check::OperatorJump)
            .unwrap()
            .confidence
            >= 55
    );
}

#[test]
fn absurd_values_are_flagged() {
    let mut sim = Sim::new();
    let mut id = None;
    for k in 0..5u64 {
        let ms = 1_000 + k * 1000;
        let f = Fix {
            p: at(300.0, 300.0),
            alt: 15_000.0,
            height: 50.0,
            speed: 0.0,
            dir: None,
            ms,
        };
        id = Some(sim.frame(
            0,
            Transport::WifiBeacon,
            MAC_A,
            -70,
            k as u8,
            &[basic(SN), loc(f)],
            ms,
        ));
    }
    let f = *sim
        .track(id.unwrap())
        .spoof
        .flag(Check::ImplausibleValue)
        .unwrap();
    assert_eq!((f.confidence, f.detail), (35, 2));
}

/// Height above takeoff climbs 20 m/s while the GPS altitude stays put.
#[test]
fn altitude_and_height_disagreeing_is_flagged() {
    let mut sim = Sim::new();
    let mut id = None;
    for k in 0..40u64 {
        let ms = 1_000 + k * 1000;
        let f = Fix {
            p: at(300.0, 300.0),
            alt: 100.0,
            height: 20.0 + 20.0 * k as f32,
            speed: 0.0,
            dir: None,
            ms,
        };
        id = Some(sim.frame(
            0,
            Transport::WifiBeacon,
            MAC_A,
            -70,
            k as u8,
            &[basic(SN), loc(f)],
            ms,
        ));
    }
    assert!(
        sim.track(id.unwrap())
            .spoof
            .flag(Check::AltitudeMismatch)
            .unwrap()
            .confidence
            >= 50
    );
}

#[test]
fn frozen_counter_is_weak() {
    let mut sim = Sim::new();
    let mut id = None;
    for k in 0..40u64 {
        let ms = 1_000 + k * 500;
        let f = Fix {
            p: at(300.0, 300.0 + k as f64),
            alt: 100.0,
            height: 50.0,
            speed: 2.0,
            dir: Some(0.0),
            ms,
        };
        id = Some(sim.frame(
            0,
            Transport::WifiBeacon,
            MAC_A,
            -70,
            0,
            &[basic(SN), loc(f), system_at(at(0.0, 0.0))],
            ms,
        ));
    }
    let t = sim.track(id.unwrap());
    assert_eq!(t.spoof.flag(Check::FrozenCounter).unwrap().detail, 100);
    assert!(t.spoof.score() < REPORT_MIN);
}

#[test]
fn malformed_auth_pages_are_flagged() {
    let mut tr: Tracker = Tracker::default();
    let raw = |m: Message| rid::encode::message(&m).unwrap();
    let page0 = raw(Message::Auth(Auth {
        auth_type: AuthType::UasIdSignature,
        page: 0,
        page_zero: Some(AuthPageZero {
            last_page_index: 1,
            length: 30,
            timestamp: 1,
        }),
        data: AuthData::new(&[7; 17]).unwrap(),
    }));
    // Page 0 whose length exceeds what its pages hold: does not decode.
    let mut broken = page0;
    broken[3] = 200;
    // Page 5 when page 0 said the last is 1.
    let page5 = raw(Message::Auth(Auth {
        auth_type: AuthType::UasIdSignature,
        page: 5,
        page_zero: None,
        data: AuthData::new(&[7; 23]).unwrap(),
    }));
    for (k, m) in [page0, page5, broken].into_iter().enumerate() {
        let d = crate::tests::det(
            Transport::WifiBeacon,
            MAC_A,
            -70,
            &[crate::tests::serial(SN), m],
        );
        tr.ingest(0, &d, 1_000 + k as u64 * 100);
    }
    let t = &tr.tracks()[0];
    assert_eq!(t.spoof.flag(Check::AuthMalformed).unwrap().detail, 2);
    assert_eq!(t.spoof.auth_pages, 0b10_0001);
}

#[test]
fn serial_numbers_against_cta_2063_a() {
    assert_eq!(cta2063_check(b"1581F0FAKE00000000AA"), Ok(()));
    assert_eq!(cta2063_check(SN.as_bytes()), Ok(()));
    assert_eq!(cta2063_check(b"MFR0ADR1P1SC00L"), Ok(()));
    assert_eq!(
        cta2063_check(b"1581F5FKD229400D"),
        Err(SerialFault::LengthMismatch)
    );
    assert_eq!(cta2063_check(b"1581"), Err(SerialFault::TooShort));
    assert_eq!(
        cta2063_check(b"15O1312A"),
        Err(SerialFault::ManufacturerCode)
    );
    assert_eq!(cta2063_check(b"1581G12345"), Err(SerialFault::LengthCode));
    assert_eq!(cta2063_check(b"15813AOB"), Err(SerialFault::SerialChar));
    assert_eq!(cta2063_check(b"1581312a"), Err(SerialFault::SerialChar));
    // A bad serial alone is weak: noted, never reported.
    let mut sim = Sim::new();
    let f = Fix {
        p: at(300.0, 300.0),
        alt: 100.0,
        height: 50.0,
        speed: 0.0,
        dir: None,
        ms: 1_000,
    };
    let id = sim.frame(
        0,
        Transport::WifiBeacon,
        MAC_A,
        -70,
        0,
        &[basic("FAKE-DRONE-42"), loc(f)],
        1_000,
    );
    assert_eq!(
        sim.track(id)
            .spoof
            .flag(Check::SerialFormat)
            .unwrap()
            .confidence,
        20
    );
    assert_clean(&mut sim, id, 2_000);
}

#[test]
fn weak_flags_together_stay_below_report_level() {
    let mut s = SpoofState::default();
    s.raise(Check::DevBoard, 15, 1);
    s.raise(Check::SerialFormat, 20, 4);
    s.raise(Check::FrozenCounter, 20, 100);
    s.raise(Check::IncompleteMessageSet, 20, 1);
    assert_eq!(s.score(), REPORT_MIN - 1);
    // One moderate flag lifts the combination.
    s.raise(Check::RssiTooStrong, 45, 5);
    assert!(s.score() > 55, "{}", s.score());
    // Five flags in four slots: the weakest went.
    assert_eq!(s.flags().len(), spoof::MAX_FLAGS);
    assert!(s.flag(Check::DevBoard).is_none());
    // Confidence only rises.
    s.raise(Check::RssiTooStrong, 30, 1);
    assert_eq!(s.flag(Check::RssiTooStrong).unwrap().confidence, 45);
}

#[test]
fn flags_are_reported_once_and_again_when_stronger() {
    let mut sim = Sim::new();
    let id = sim.frame(0, Transport::WifiBeacon, MAC_A, -70, 0, &[basic(SN)], 1_000);
    sim.tr
        .tracks
        .iter_mut()
        .for_each(|t| t.spoof.raise(Check::PositionJump, 50, 900));
    let ev = sim.tr.take_spoof_flag(1_000).unwrap();
    assert_eq!(ev.track, id);
    assert_eq!(ev.data(), [1, 50]);
    let text = spoof::event_text(&ev, sim.tr.get(id));
    assert_eq!(
        text.as_str(),
        "track 1 SerialNumber 1ZZZ9FAKE00001: position jumped 900 m, faster than any drone flies (confidence 50, score 50)"
    );
    assert_eq!(sim.tr.take_spoof_flag(1_000), None);
    sim.tr
        .tracks
        .iter_mut()
        .for_each(|t| t.spoof.raise(Check::PositionJump, 60, 900));
    assert_eq!(sim.tr.take_spoof_flag(1_000), None);
    sim.tr
        .tracks
        .iter_mut()
        .for_each(|t| t.spoof.raise(Check::PositionJump, 75, 2000));
    assert_eq!(
        sim.tr.take_spoof_flag(1_000).map(|e| e.confidence),
        Some(75)
    );
}

#[test]
fn check_names_and_codes() {
    for c in Check::ALL {
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, std::format!("\"{}\"", c.name()));
        assert_eq!(Check::from_code(c.code()), Some(c));
        // Every reason renders something sensible.
        let r: heapless::String<128> = crate::text::render(spoof::Reason {
            check: c,
            detail: 3,
        });
        assert!(r.len() > 10, "{c:?}: {r}");
    }
    assert_eq!(Check::ALL.len(), 23);
    assert_eq!(Check::from_code(0), None);
}

#[test]
fn json_carries_flags_and_link() {
    let mut sim = Sim::new();
    let id = sim.frame(0, Transport::WifiBeacon, MAC_A, -70, 0, &[basic(SN)], 1_000);
    sim.tr.tracks.iter_mut().for_each(|t| {
        t.spoof.raise(Check::RssiTooStrong, 65, 14);
        t.continuity.previous = Some(TrackId(9));
        t.continuity.link_confidence = 70;
        t.continuity.identity_changed = true;
    });
    let ctx = sim.tr.context(1_500, None);
    let mut buf = [0u8; json::MAX_LINE];
    let n = json::track_line(sim.track(id), &ctx, &mut buf).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&buf[..n]).unwrap();
    assert_eq!(v["spoof"]["score"], 65);
    assert_eq!(v["spoof"]["flags"][0]["check"], "rssi_too_strong");
    assert_eq!(v["spoof"]["flags"][0]["detail"], 14);
    assert_eq!(v["cont"]["prev"], 9);
    assert_eq!(v["cont"]["id_changed"], true);
    let s: heapless::String<192> = crate::text::render(spoof::Summary(sim.track(id)));
    assert_eq!(
        s.as_str(),
        "possible spoof 65%: signal 14 dB stronger than possible at its claimed distance; \
         probably the same drone as track 9, new identity mid-flight"
    );
    // CoT remarks carry the same note.
    let ctx = sim.tr.context(1_500, Some(UTC0 + 1_500));
    let mut big = [0u8; 4096];
    sim.tr.vouch_for_tests(1_500);
    let n = cot::drone_event(sim.track(id), &ctx, &mut big);
    // No position: no CoT event; the summary is still well-formed text.
    assert_eq!(n, Err(FormatError::NoPosition));
}

// -------------------------------------------------------- continuity --

/// A flight segment: `secs` of 1 Hz frames from `mac` with `id`.
#[allow(clippy::too_many_arguments)]
fn segment(
    sim: &mut Sim,
    mac: [u8; 6],
    id: Message,
    start_ms: u64,
    secs: u64,
    pos: impl Fn(f64) -> LatLon,
    speed: f32,
    dir: Option<f32>,
    height: f32,
    op: LatLon,
) -> TrackId {
    let mut last = None;
    for k in 0..secs {
        let ms = start_ms + k * 1000;
        let p = pos(k as f64);
        let f = Fix {
            p,
            alt: 40.0 + height,
            height,
            speed,
            dir,
            ms,
        };
        let rssi = honest_rssi(spoof_dist(p), sim.noise(), 3.0);
        last = Some(sim.frame(
            0,
            Transport::WifiBeacon,
            mac,
            rssi,
            k as u8,
            &[id.clone(), loc(f), system_at(op)],
            ms,
        ));
    }
    last.unwrap()
}

/// Mid-flight, the drone swaps serial and MAC: the new track continues the
/// old one and the change is flagged.
#[test]
fn mid_flight_identity_change_is_linked_and_flagged() {
    let mut sim = Sim::new();
    let op = at(0.0, -100.0);
    let a = segment(
        &mut sim,
        MAC_A,
        basic(SN),
        1_000,
        30,
        |t| at(100.0 + 10.0 * t, 400.0),
        10.0,
        Some(90.0),
        60.0,
        op,
    );
    // 3 s of silence, then a new identity where the old one would be.
    let b = segment(
        &mut sim,
        MAC_B,
        basic(SN2),
        34_000,
        20,
        |t| at(100.0 + 10.0 * (33.0 + t), 400.0),
        10.0,
        Some(90.0),
        60.0,
        op,
    );
    assert_ne!(a, b);
    let t = sim.track(b);
    assert_eq!(t.continuity.previous, Some(a));
    assert!(t.continuity.identity_changed);
    assert!(t.continuity.link_confidence >= 75, "{:?}", t.continuity);
    assert_eq!(sim.track(a).continuity.next, Some(b));
    let f = t.spoof.flag(Check::IdentityChanged).unwrap();
    assert_eq!((f.confidence, f.detail), (55, a.0 as i32));
    let evs = sim.drain(60_000);
    assert!(
        evs.iter()
            .any(|e| e.check == Check::IdentityChanged && e.track == b)
    );
}

/// Between flights: the drone lands, and two minutes later takes off from
/// the same spot with a new session ID and MAC. Linked, not flagged.
#[test]
fn new_session_id_between_flights_is_linked_quietly() {
    let mut sim = Sim::new();
    let op = at(0.0, -100.0);
    let home = at(200.0, 200.0);
    let a = segment(
        &mut sim,
        MAC_A,
        session(1),
        1_000,
        30,
        |_| home,
        0.0,
        None,
        0.0,
        op,
    );
    let b = segment(
        &mut sim,
        MAC_B,
        session(2),
        151_000,
        20,
        |t| at(200.0, 200.0 + 2.0 * t),
        2.0,
        Some(0.0),
        10.0,
        op,
    );
    let t = sim.track(b);
    assert_eq!(t.continuity.previous, Some(a));
    assert!(!t.continuity.identity_changed);
    assert!(t.spoof.flag(Check::IdentityChanged).is_none());
    assert_clean(&mut sim, b, 172_000);
}

/// Two drones flying at once, near each other: never linked.
#[test]
fn simultaneous_drones_are_not_linked() {
    let mut sim = Sim::new();
    let op = at(0.0, -100.0);
    for k in 0..30u64 {
        let ms = 1_000 + k * 1000;
        for (mac, sn, east) in [(MAC_A, SN, 0.0), (MAC_B, SN2, 40.0)] {
            let f = Fix {
                p: at(300.0 + east, 300.0 + k as f64),
                alt: 100.0,
                height: 60.0,
                speed: 1.0,
                dir: Some(0.0),
                ms: ms + east as u64,
            };
            sim.frame(
                0,
                Transport::WifiBeacon,
                mac,
                -70,
                k as u8,
                &[basic(sn), loc(f), system_at(op)],
                ms + east as u64,
            );
        }
    }
    for t in sim.tr.tracks() {
        assert_eq!(t.continuity.previous, None);
    }
}

/// A link is undone when the "old" drone turns out to still be flying.
#[test]
fn link_is_undone_when_the_old_track_returns() {
    let mut sim = Sim::new();
    let op = at(0.0, -100.0);
    let a = segment(
        &mut sim,
        MAC_A,
        basic(SN),
        1_000,
        20,
        |t| at(100.0 + 5.0 * t, 400.0),
        5.0,
        Some(90.0),
        60.0,
        op,
    );
    let b = segment(
        &mut sim,
        MAC_B,
        basic(SN2),
        22_000,
        5,
        |t| at(100.0 + 5.0 * (21.0 + t), 400.0),
        5.0,
        Some(90.0),
        60.0,
        op,
    );
    assert_eq!(sim.track(b).continuity.previous, Some(a));
    // The old identity is heard again.
    segment(
        &mut sim,
        MAC_A,
        basic(SN),
        28_000,
        3,
        |t| at(200.0 + 5.0 * t, 420.0),
        5.0,
        Some(90.0),
        60.0,
        op,
    );
    let t = sim.track(b);
    assert_eq!(t.continuity.previous, None);
    assert!(!t.continuity.identity_changed);
    assert!(t.spoof.flag(Check::IdentityChanged).is_none());
    assert_eq!(sim.track(a).continuity.next, None);
}

/// A new track far from where the old one was heading is not linked.
#[test]
fn far_away_new_track_is_not_linked() {
    let mut sim = Sim::new();
    let op = at(0.0, -100.0);
    segment(
        &mut sim,
        MAC_A,
        basic(SN),
        1_000,
        20,
        |t| at(100.0 + 5.0 * t, 400.0),
        5.0,
        Some(90.0),
        60.0,
        op,
    );
    let b = segment(
        &mut sim,
        MAC_B,
        basic(SN2),
        23_000,
        5,
        |t| at(-1_500.0, 1_500.0 + t),
        1.0,
        Some(0.0),
        60.0,
        at(-1_500.0, 1_400.0),
    );
    assert_eq!(sim.track(b).continuity.previous, None);
}

/// What the checks and links cost in RAM (printed with `--nocapture`).
#[test]
fn spoof_memory_footprint() {
    use core::mem::size_of;
    std::println!(
        "SpoofState {} B, Continuity {} B, Track {} B, Tracker<64,0> {} B",
        size_of::<SpoofState>(),
        size_of::<Continuity>(),
        size_of::<Track>(),
        size_of::<Tracker<64, 0>>(),
    );
    assert!(size_of::<SpoofState>() <= 128);
    let _ = String::new();
}
