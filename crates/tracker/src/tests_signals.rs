//! Protocol-4 signals: DJI DroneID (OcuSync and legacy Wi-Fi) merged with
//! Remote ID, RF emitters, Wi-Fi drones and node status, and their output.
//!
//! Every serial here is made up (`TEST...`); none comes from a recording.

extern crate std;
use std::string::String;

use super::*;
use crate::tests::{CITY_HALL, WIFI_MAC, det, location, serial};
use link::{DroneIdRecord, DroneIdSighting, Frame, RfKind, RfSighting, WifiDroneSighting};
use rid::message::{FixedStr, LatLon};

const SN: &str = "TEST0SYNTH000001";
const DJI_WIFI_MAC: [u8; 6] = [0x60, 0x60, 0x1f, 0xaa, 0xbb, 0xcc];

fn rad_e7(deg: f64) -> i32 {
    (deg.to_radians() * 1e7).round() as i32
}

fn serial16(s: &str) -> [u8; 16] {
    let mut out = [0u8; 16];
    out[..s.len()].copy_from_slice(s.as_bytes());
    out
}

/// 20 m north of where [`flight`] puts the drone.
const NEAR_FLIGHT: (f64, f64) = (CITY_HALL.0 + 320.0 / 111_320.0, CITY_HALL.1);

/// A plaintext OcuSync record: drone 300 m north of City Hall, pilot at
/// City Hall, home 10 m east of it. Altitude 3281 ft = 1000 m.
fn flight(sn: &str) -> DroneIdSighting {
    let north = CITY_HALL.0 + 300.0 / 111_320.0;
    DroneIdSighting {
        freq: 57565,
        rssi: -71,
        snr_db: 14,
        uptime_us: 0,
        record: DroneIdRecord::Flight {
            serial: serial16(sn),
            lat_raw: rad_e7(north),
            lon_raw: rad_e7(CITY_HALL.1),
            altitude_raw: 3281,
            height_raw: 1640,
            v_north: 300,
            v_east: -400,
            v_up: -50,
            yaw_raw: 9000,
            pilot_lat_raw: rad_e7(CITY_HALL.0),
            pilot_lon_raw: rad_e7(CITY_HALL.1),
            home_lat_raw: rad_e7(CITY_HALL.0),
            home_lon_raw: rad_e7(CITY_HALL.1 + 10.0 / 85_000.0),
            product_type: 68,
        },
    }
}

fn encrypted(key: [u8; 4], freq: u32) -> DroneIdSighting {
    DroneIdSighting {
        freq,
        rssi: -80,
        snr_db: 6,
        uptime_us: 0,
        record: DroneIdRecord::Encrypted { key_id: key },
    }
}

fn s(buf: &[u8], n: usize) -> &str {
    core::str::from_utf8(&buf[..n]).unwrap()
}

fn json_of(t: &Track, ctx: &Context) -> serde_json::Value {
    let mut buf = [0u8; json::MAX_LINE];
    let n = json::track_line(t, ctx, &mut buf).unwrap();
    serde_json::from_str(s(&buf, n)).unwrap()
}

// ------------------------------------------------------------- OcuSync --

#[test]
fn droneid_flight_is_scaled_like_the_droneid_crate() {
    let mut tr: Tracker = Tracker::default();
    let sighting = flight(SN);
    let id = tr.ingest_droneid(4, &sighting, 1_000).unwrap();
    let t = tr.get(id).unwrap();
    assert_eq!(t.primary_id().unwrap().id.as_str(), Some(SN));
    assert_eq!(
        t.ua_type,
        Some(rid::message::UaType::HelicopterOrMultirotor)
    );
    assert_eq!(t.location_src, Source::DroneId);
    assert_eq!(t.system_src, Source::DroneId);

    // Same scaling as droneid::payload::FlightInfo's own methods.
    let DroneIdRecord::Flight {
        lat_raw,
        altitude_raw,
        height_raw,
        ..
    } = sighting.record
    else {
        unreachable!()
    };
    let reference = droneid::payload::FlightInfo {
        version: 2,
        seq: 0,
        state: 0,
        serial: serial16(SN),
        lat_raw,
        lon_raw: 0,
        altitude_raw,
        height_raw,
        v_north: 0,
        v_east: 0,
        v_up: 0,
        yaw_raw: 0,
        gps_time_ms: 0,
        app_lat_raw: 0,
        app_lon_raw: 0,
        home_lat_raw: 0,
        home_lon_raw: 0,
        product_type: 68,
        uuid_len: 0,
        uuid: [0; 20],
    };
    let l = &t.location.as_ref().unwrap().value;
    let p = l.position.unwrap();
    assert_eq!(p.lat, reference.lat());
    assert!((l.altitude_pressure.unwrap() as f64 - reference.altitude_m()).abs() < 1e-3);
    assert!((l.height.unwrap() as f64 - reference.height_m()).abs() < 1e-3);
    assert!((l.altitude_pressure.unwrap() - 1000.0).abs() < 0.01);
    // Never claimed as WGS-84.
    assert_eq!(l.altitude_geodetic, None);
    // 3 m/s north, 4 m/s west: 5 m/s on a track of 306.87 deg; 0.5 m/s down.
    assert!((l.speed_horizontal.unwrap() - 5.0).abs() < 1e-4);
    assert!((l.direction.unwrap() - 306.87).abs() < 0.01);
    assert!((l.speed_vertical.unwrap() + 0.5).abs() < 1e-4);

    let op = t.system.as_ref().unwrap().value.operator_position.unwrap();
    assert!((op.lat - CITY_HALL.0).abs() < 1e-5 && (op.lon - CITY_HALL.1).abs() < 1e-5);
    assert!(t.home.is_some());
    assert_eq!(t.dji.product_name(), Some("Mavic 3"));
    assert_eq!(t.dji.freq_mhz(), Some(5756.5));
    assert_eq!(t.latest_rssi(), Some(-71));
    assert_eq!(t.nodes, 1 << 4);
    // A drone with an ID is announced once confirmed: heard again a
    // second later (`probation`).
    assert_eq!(tr.take_new_drone(1_000), None);
    assert_eq!(tr.ingest_droneid(4, &sighting, 2_000), Some(id));
    assert_eq!(tr.take_new_drone(2_000), Some(id));

    // Geometry works from the DroneID position: ~300 m north.
    tr.set_own_position(CITY_HALL.0, CITY_HALL.1, Some(10.0), 1_000);
    let ctx = tr.context(1_000, None);
    let g = ctx.geometry(tr.get(id).unwrap());
    let d = g.drone.unwrap();
    assert!((d.distance_m - 300.0).abs() < 2.0, "{}", d.distance_m);
    assert!(d.bearing_deg < 1.0 || d.bearing_deg > 359.0);
    // No WGS-84 altitude, so no "above you".
    assert_eq!(g.height_above_m, None);
}

#[test]
fn zero_positions_mean_unknown() {
    let mut tr: Tracker = Tracker::default();
    let mut f = flight(SN);
    if let DroneIdRecord::Flight {
        lat_raw,
        lon_raw,
        pilot_lat_raw,
        pilot_lon_raw,
        ..
    } = &mut f.record
    {
        (*lat_raw, *lon_raw, *pilot_lat_raw, *pilot_lon_raw) = (0, 0, 0, 0);
    }
    let id = tr.ingest_droneid(0, &f, 10).unwrap();
    let t = tr.get(id).unwrap();
    assert_eq!(t.location.as_ref().unwrap().value.position, None);
    assert!(t.system.is_none());
}

/// DJI's Remote ID serial equals its DroneID serial: one track, whichever
/// arrives first, and Remote ID's position wins while it is fresh.
#[test]
fn droneid_merges_with_remote_id_by_serial() {
    for droneid_first in [true, false] {
        let mut tr: Tracker = Tracker::default();
        let rid = det(
            Transport::WifiBeacon,
            WIFI_MAC,
            -60,
            // 20 m from where DroneID puts it (a real DJI drone sends the
            // same position on both).
            &[serial(SN), location(NEAR_FLIGHT.0, NEAR_FLIGHT.1, 140.0)],
        );
        if droneid_first {
            tr.ingest_droneid(2, &flight(SN), 1_000);
            tr.ingest(0, &rid, 1_500);
        } else {
            tr.ingest(0, &rid, 1_000);
            tr.ingest_droneid(2, &flight(SN), 1_500);
        }
        assert_eq!(tr.len(), 1, "droneid_first={droneid_first}");
        let t = &tr.tracks()[0];
        assert_eq!(t.uas_ids.len(), 1);
        assert!(t.dji.ocusync.heard() && t.slot(SLOTS[0]).heard());
        assert_eq!(t.nodes, 0b101);
        let names: std::vec::Vec<_> = t.source_names().collect();
        assert_eq!(names, ["WifiBeacon", "DroneId"]);
        // Remote ID's position, whichever came last.
        assert_eq!(t.location_src, Source::RemoteId);
        assert!((t.location.as_ref().unwrap().value.position.unwrap().lat - NEAR_FLIGHT.0).abs() < 1e-6);
        // The pilot position is DroneID's (that RID frame had no System).
        assert_eq!(t.system_src, Source::DroneId);
        // Remote ID goes quiet: DroneID takes the position over.
        tr.ingest_droneid(2, &flight(SN), 1_500 + dji::RID_PREFERRED_MS);
        let t = &tr.tracks()[0];
        assert_eq!(t.location_src, Source::DroneId);
    }
}

/// A Remote ID track that had only a MAC (a Location-only BLE advert) is
/// folded into the DroneID track when its Basic ID arrives.
#[test]
fn mac_keyed_rid_track_joins_the_droneid_track() {
    let mut tr: Tracker = Tracker::default();
    let ble = [0xc0, 1, 2, 3, 4, 5];
    tr.ingest(
        1,
        &det(
            Transport::BleLegacy,
            ble,
            -70,
            &[location(NEAR_FLIGHT.0, NEAR_FLIGHT.1, 100.0)],
        ),
        100,
    );
    tr.ingest_droneid(2, &flight(SN), 200);
    assert_eq!(tr.len(), 2);
    tr.ingest(1, &det(Transport::BleLegacy, ble, -70, &[serial(SN)]), 300);
    assert_eq!(tr.len(), 1);
    assert!(tr.tracks()[0].dji.ocusync.heard());
}

#[test]
fn hidden_droneid_identity_still_merges() {
    let mut tr: Tracker = Tracker::new(Config {
        hide_droneid_ids: true,
        ..Config::default()
    });
    let id = tr.ingest_droneid(0, &flight(SN), 100).unwrap();
    let ctx = tr.context(100, None);
    let t = tr.get(id).unwrap();
    assert!(t.uas_ids.is_empty());
    let v = json_of(t, &ctx);
    assert_eq!(v["ids"], serde_json::json!([]));
    assert_eq!(v["dji"]["ids_shown"], false);
    let mut buf = [0u8; 4096];
    let n = csv::row(t, &ctx, &mut buf).unwrap();
    assert!(!s(&buf, n).contains(SN));
    assert_eq!(cot::uid_base(t), std::format!("dji-t{}", id.0).as_str());
    // A hidden-ID drone is announced once confirmed (heard again a second
    // later), like any other.
    assert_eq!(tr.take_new_drone(100), None);
    assert_eq!(tr.ingest_droneid(0, &flight(SN), 1_100), Some(id));
    assert_eq!(tr.take_new_drone(1_100), Some(id));

    // The drone also broadcasts it in Remote ID: same track, now shown.
    tr.ingest(
        0,
        &det(
            Transport::BleLongRange,
            [0xc4, 0, 0, 0, 0, 1],
            -70,
            &[serial(SN)],
        ),
        200,
    );
    assert_eq!(tr.len(), 1);
    assert_eq!(tr.tracks()[0].primary_id().unwrap().id.as_str(), Some(SN));

    // O4 key IDs are hidden too.
    let k = tr.ingest_droneid(0, &encrypted([0xde, 0xad, 0xbe, 0xef], 24145), 300).unwrap();
    let v = json_of(tr.get(k).unwrap(), &tr.context(300, None));
    assert_eq!(v["dji"]["key_id"], serde_json::Value::Null);
    assert_eq!(v["dji"]["encrypted"], true);
}

fn o4(kind: link::O4Kind, key: [u8; 4]) -> DroneIdSighting {
    DroneIdSighting {
        freq: 58165,
        rssi: -66,
        snr_db: 11,
        uptime_us: 1,
        record: DroneIdRecord::O4 { kind, key_id: key, object: [0x5a; 135].into_iter().collect() },
    }
}

#[test]
fn o4_cryp_and_infp_share_a_session_track_with_a_state() {
    use crate::O4State;
    let mut tr: Tracker = Tracker::default();
    let k = [0xde, 0xad, 0xbe, 0xef];
    let a = tr.ingest_droneid(0, &o4(link::O4Kind::Cryp, k), 100).unwrap();
    let t = tr.get(a).unwrap();
    assert_eq!(t.dji.o4.state(), Some(O4State::GpsLocked));
    let ctx = tr.context(100, None);
    let v = json_of(t, &ctx);
    assert_eq!(v["dji"]["o4"]["state"], "GpsLocked");
    assert_eq!(v["dji"]["o4"]["model_dependent"], true);
    assert_eq!(v["dji"]["key_id"], "deadbeef");
    // INFP of the same session joins the track and moves the state on.
    let b = tr.ingest_droneid(0, &o4(link::O4Kind::Infp10, k), 740).unwrap();
    let c = tr.ingest_droneid(0, &o4(link::O4Kind::Infp11, k), 1380).unwrap();
    assert_eq!((a, a), (b, c));
    // An older node's `Encrypted` record of the same session joins too.
    assert_eq!(tr.ingest_droneid(0, &encrypted(k, 57565), 1400), Some(a));
    let t = tr.get(a).unwrap();
    assert_eq!((t.dji.o4.cryp, t.dji.o4.infp10, t.dji.o4.infp11), (1, 1, 1));
    assert_eq!(t.dji.o4.state(), Some(O4State::ProbablyFlying));
    assert!(O4State::ProbablyFlying.label().contains("model-dependent"));
    let ctx = tr.context(1400, None);
    let v = json_of(t, &ctx);
    assert_eq!(v["dji"]["o4"]["state"], "ProbablyFlying");
    assert_eq!(v["dji"]["o4"]["infp10"], 1);
    let mut buf = [0u8; csv::MAX_ROW];
    let n = csv::row(t, &ctx, &mut buf).unwrap();
    assert!(s(&buf, n).contains(",deadbeef,O4Session,"), "{}", s(&buf, n));
    // Hidden IDs: no key in JSON or CSV, the state still shown.
    let mut tr2: Tracker = Tracker::new(crate::Config { hide_droneid_ids: true, ..crate::Config::default() });
    let h = tr2.ingest_droneid(0, &o4(link::O4Kind::Infp10, k), 5).unwrap();
    let ctx = tr2.context(5, None);
    let v = json_of(tr2.get(h).unwrap(), &ctx);
    assert_eq!(v["dji"]["key_id"], serde_json::Value::Null);
    assert_eq!(v["dji"]["o4"]["state"], "ProbablyFlying");
    let n = csv::row(tr2.get(h).unwrap(), &ctx, &mut buf).unwrap();
    assert!(!s(&buf, n).contains("deadbeef"));
}

#[test]
fn o4_records_are_presence_tracks_keyed_by_key_id() {
    let mut tr: Tracker = Tracker::default();
    let a = tr.ingest_droneid(0, &encrypted([1, 2, 3, 4], 24145), 100).unwrap();
    let b = tr.ingest_droneid(0, &encrypted([1, 2, 3, 4], 57565), 200).unwrap();
    let c = tr.ingest_droneid(0, &encrypted([9, 9, 9, 9], 57565), 300).unwrap();
    assert_eq!(a, b);
    assert_ne!(a, c);
    // (Standing is beside the point here.)
    tr.vouch_for_tests(300);
    let t = tr.get(a).unwrap();
    assert!(t.location.is_none() && t.uas_ids.is_empty() && !t.dji.undecoded);
    assert_eq!(t.dji.ocusync.count, 2);
    assert_eq!(t.dji.freq_mhz(), Some(5756.5));
    let ctx = tr.context(300, None);
    let v = json_of(t, &ctx);
    assert_eq!(v["dji"]["key_id"], "01020304");
    assert_eq!(v["dji"]["ocusync"]["mhz"], 5756.5);
    assert_eq!(v["loc"], serde_json::Value::Null);
    let names: std::vec::Vec<_> = t.source_names().collect();
    assert_eq!(names, ["DroneIdO4"]);
    // No position: no CoT, but CSV still logs it.
    assert_eq!(
        cot::drone_event(t, &tr.context(300, Some(1)), &mut [0u8; 2048]),
        Err(FormatError::NoPosition)
    );
    let mut buf = [0u8; csv::MAX_ROW];
    let n = csv::row(t, &ctx, &mut buf).unwrap();
    assert!(s(&buf, n).contains(",DroneIdO4,"), "{}", s(&buf, n));
    // Never merged with a plaintext drone.
    tr.ingest_droneid(0, &flight(SN), 400);
    assert_eq!(tr.len(), 3);
}

#[test]
fn undecoded_bursts_share_one_presence_track() {
    let mut tr: Tracker = Tracker::default();
    let burst = |f| DroneIdSighting {
        freq: f,
        rssi: -88,
        snr_db: 3,
        uptime_us: 0,
        record: DroneIdRecord::Detected,
    };
    let a = tr.ingest_droneid(0, &burst(24145), 100).unwrap();
    let b = tr.ingest_droneid(1, &burst(57765), 200).unwrap();
    assert_eq!(a, b);
    assert_eq!(tr.len(), 1);
    let t = tr.get(a).unwrap();
    assert!(t.dji.undecoded);
    assert_eq!(t.dji.freq_mhz(), Some(5776.5));
    assert_eq!(t.nodes, 0b11);
    // A real drone does not join it.
    let c = tr.ingest_droneid(0, &flight(SN), 300).unwrap();
    assert_ne!(a, c);
    // Garbage in the serial field: no identity, so it is presence too.
    let mut bad = flight(SN);
    if let DroneIdRecord::Flight { serial, .. } = &mut bad.record {
        *serial = [0xff; 16];
    }
    assert_eq!(tr.ingest_droneid(0, &bad, 400), Some(a));
    // Expires like any track.
    tr.expire(400 + Config::default().remove_after_ms);
    assert!(tr.is_empty());
}

// ------------------------------------------------------- DJI Wi-Fi --

fn dji_element(sn: &str, with_header: bool) -> heapless::Vec<u8, 255> {
    use rid::dji::{FlightInfo, FlightInfoExt};
    let info = FlightInfo {
        version: 2,
        seq: 7,
        state: FlightInfo::STATE_SERIAL_VALID | FlightInfo::STATE_PRIVACY_DISABLED,
        serial: FixedStr::new(sn.as_bytes()).unwrap(),
        position: Some(LatLon {
            lat: 39.96,
            lon: -75.17,
        }),
        ext: Some(FlightInfoExt {
            altitude: 60,
            height: 45,
            v_north: 0,
            v_east: 100,
            v_up: 0,
            pitch: 0.0,
            roll: 0.0,
            yaw: 45.0,
            home: Some(LatLon {
                lat: 39.959,
                lon: -75.169,
            }),
            product_type: 21,
            uuid: FixedStr::default(),
        }),
    };
    let mut body = [0u8; 200];
    let n = rid::encode::dji_flight_info(&info, &mut body).unwrap();
    let mut out = heapless::Vec::new();
    if with_header {
        out.extend_from_slice(&[221, n as u8]).unwrap();
    }
    out.extend_from_slice(&body[..n]).unwrap();
    out
}

#[test]
fn dji_wifi_element_is_decoded_into_a_track() {
    for header in [false, true] {
        let mut tr: Tracker = Tracker::default();
        let el = dji_element(SN, header);
        let id = tr
            .ingest_dji_wifi(0, 6, -55, DJI_WIFI_MAC, &el, 100)
            .unwrap();
        let t = tr.get(id).unwrap();
        assert_eq!(t.primary_id().unwrap().id.as_str(), Some(SN));
        assert_eq!(t.location_src, Source::DjiWifi);
        let l = &t.location.as_ref().unwrap().value;
        assert!((l.position.unwrap().lat - 39.96).abs() < 1e-5);
        assert_eq!(l.altitude_pressure, Some(60.0));
        assert_eq!(l.height, Some(45.0));
        assert!((l.speed_horizontal.unwrap() - 1.0).abs() < 1e-4);
        assert_eq!(l.direction, Some(90.0));
        assert!(t.home.is_some());
        assert!(t.system.is_none(), "the Wi-Fi record has no pilot position");
        assert_eq!(t.dji.product_name(), Some("Spark"));
        assert_eq!(t.dji.wifi.mac, DJI_WIFI_MAC);
        let v = json_of(t, &tr.context(100, None));
        assert_eq!(v["dji"]["wifi"]["ch"], 6);
        assert_eq!(v["dji"]["model"], "Spark");
        assert_eq!(v["loc_src"], "DjiWifi");
    }
}

#[test]
fn dji_wifi_merges_with_remote_id_and_droneid() {
    let mut tr: Tracker = Tracker::default();
    tr.ingest(
        0,
        &det(Transport::WifiBeacon, WIFI_MAC, -60, &[serial(SN)]),
        100,
    );
    tr.ingest_dji_wifi(0, 6, -55, DJI_WIFI_MAC, &dji_element(SN, false), 200);
    tr.ingest_droneid(3, &flight(SN), 300);
    assert_eq!(tr.len(), 1);
    let names: std::vec::Vec<_> = tr.tracks()[0].source_names().collect();
    assert_eq!(names, ["WifiBeacon", "DroneId", "DjiWifi"]);
}

#[test]
fn dji_wifi_purpose_and_unknown_records() {
    let mut tr: Tracker = Tracker::default();
    let mut buf = [0u8; 80];
    let n =
        rid::encode::dji_flight_purpose(SN.as_bytes(), b"ID1", b"roof survey", &mut buf).unwrap();
    let id = tr
        .ingest_dji_wifi(0, 1, -50, DJI_WIFI_MAC, &buf[..n], 100)
        .unwrap();
    let t = tr.get(id).unwrap();
    assert_eq!(
        t.self_id.as_ref().unwrap().value.description.as_str(),
        Some("roof survey")
    );
    // An unknown subcommand counts against the known address only.
    let mut unk = buf;
    unk[6] = 0x42;
    assert_eq!(
        tr.ingest_dji_wifi(0, 1, -50, DJI_WIFI_MAC, &unk[..n], 200),
        Some(id)
    );
    assert_eq!(tr.get(id).unwrap().dji.wifi.count, 2);
    assert_eq!(
        tr.ingest_dji_wifi(0, 1, -50, [2, 0, 0, 0, 0, 9], &unk[..n], 300),
        None
    );
    // Not DJI's element at all.
    assert_eq!(
        tr.ingest_dji_wifi(0, 1, -50, DJI_WIFI_MAC, &[0xfa, 0x0b, 0xbc, 0x0d], 300),
        None
    );
    assert_eq!(tr.ingest_dji_wifi(0, 1, -50, DJI_WIFI_MAC, &[], 300), None);
    assert_eq!(tr.len(), 1);

    // With identity hidden, the purpose text is not kept.
    let mut tr: Tracker = Tracker::new(Config {
        hide_droneid_ids: true,
        ..Config::default()
    });
    let id = tr
        .ingest_dji_wifi(0, 1, -50, DJI_WIFI_MAC, &buf[..n], 100)
        .unwrap();
    assert!(tr.get(id).unwrap().self_id.is_none());
}

// ---------------------------------------------------------- RF emitters --

fn rf(kind: RfKind, freq: u32) -> RfSighting {
    RfSighting {
        kind,
        freq,
        width_khz: 0,
        snr_db: 12,
        duty_pct: 100,
        line_hz: 0,
        uptime_us: 0,
    }
}

#[test]
fn rf_emitters_are_keyed_by_kind_and_frequency() {
    let mut tr: Tracker = Tracker::default();
    let a = tr.ingest_rf(5, &rf(RfKind::FpvCarrier, 58060), 100);
    // Same kind 1 MHz off: the same transmitter.
    let mut confirmed = rf(RfKind::FpvCarrier, 58070);
    confirmed.snr_db = 20;
    assert_eq!(tr.ingest_rf(5, &confirmed, 200), a);
    // Same frequency, other kind: another entry.
    let mut video = rf(RfKind::AnalogVideo, 58060);
    video.line_hz = 15_734;
    let b = tr.ingest_rf(5, &video, 300);
    assert_ne!(a, b);
    // A later report without a line rate keeps the measured one.
    tr.ingest_rf(5, &rf(RfKind::AnalogVideo, 58060), 400);
    let e = tr.get_emitter(b).unwrap();
    assert_eq!((e.line_hz, e.sightings, e.freq_mhz()), (15_734, 2, 5806.0));
    let e = tr.get_emitter(a).unwrap();
    assert_eq!((e.snr_db, e.peak_snr_db, e.freq), (20, 20, 58070));
    // HDZero's 27 MHz width widens the match to half of it.
    let mut hd = rf(RfKind::Hdzero, 57320);
    hd.width_khz = 27_000;
    let h = tr.ingest_rf(5, &hd, 500);
    assert_eq!(tr.ingest_rf(5, &rf(RfKind::Hdzero, 57320 + 100), 600), h);
    assert_eq!(tr.emitters().len(), 3);
}

/// Protocol 7's DJI kinds: a hopping controller stays one emitter across its
/// hop set, the video kinds key like any other, and all of them alert once.
#[test]
fn dji_kinds_key_alert_and_label() {
    let mut tr: Tracker = Tracker::default();
    let c = tr.ingest_rf(5, &rf(RfKind::DjiController, 57461), 100);
    assert_eq!(tr.ingest_rf(5, &rf(RfKind::DjiController, 57821), 200), c);
    assert_eq!(tr.ingest_rf(5, &rf(RfKind::DjiController, 58401), 300), c);
    // Another band is another controller.
    assert_ne!(tr.ingest_rf(5, &rf(RfKind::DjiController, 24291), 300), c);
    let v = tr.ingest_rf(5, &rf(RfKind::DjiVideo, 57685), 300);
    let o4 = tr.ingest_rf(5, &rf(RfKind::DjiO4Video, 52100), 300);
    let fpv = tr.ingest_rf(5, &rf(RfKind::DjiFpvAirUnit, 57700), 300);
    assert_ne!(v, fpv);
    // Video kinds do not hop: 10 MHz away is another link.
    assert_ne!(tr.ingest_rf(5, &rf(RfKind::DjiVideo, 57785), 300), v);
    let mut alerts = std::vec::Vec::new();
    while let Some(k) = tr.take_new_signal(300) {
        alerts.push(k);
    }
    assert_eq!(alerts.len(), 6, "{alerts:?}");
    assert!(alerts.contains(&SignalKey::Emitter(o4)));
    assert_eq!(signals::rf_label(RfKind::DjiO4Video), "DJI O4 video");
    assert_eq!(signals::rf_label(RfKind::DjiController), "DJI controller");
    assert_eq!(tr.get_emitter(c).unwrap().sightings, 3);
}

#[test]
fn energy_never_alerts_and_video_alerts_once() {
    let mut tr: Tracker = Tracker::default();
    tr.ingest_rf(5, &rf(RfKind::Energy, 24370), 100);
    tr.ingest_rf(5, &rf(RfKind::FpvCarrier, 57650), 100);
    assert_eq!(tr.take_new_signal(100), None);
    let v = tr.ingest_rf(5, &rf(RfKind::AnalogVideo, 58060), 200);
    let l = tr.ingest_rf(5, &rf(RfKind::LteStyle, 57700), 200);
    assert_eq!(tr.take_new_signal(200), Some(SignalKey::Emitter(v)));
    assert_eq!(tr.take_new_signal(200), Some(SignalKey::Emitter(l)));
    assert_eq!(tr.take_new_signal(200), None);
    tr.ingest_rf(5, &rf(RfKind::AnalogVideo, 58060), 300);
    assert_eq!(tr.take_new_signal(300), None);
}

#[test]
fn rf_emitters_age_like_open_links_and_make_room() {
    let cfg = Config::default();
    let mut tr: Tracker = Tracker::default();
    let id = tr.ingest_rf(5, &rf(RfKind::Hdzero, 57320), 0);
    let ctx = tr.context(cfg.stale_after_ms, None);
    assert_eq!(
        ctx.emitter_state(tr.get_emitter(id).unwrap()),
        TrackState::Stale
    );
    tr.expire(cfg.remove_after_ms);
    assert!(tr.emitters().is_empty());
    // A full table drops the quietest.
    for i in 0..signals::MAX_EMITTERS as u32 + 3 {
        tr.ingest_rf(
            5,
            &rf(RfKind::Energy, 50000 + i * 100),
            1_000 + u64::from(i),
        );
    }
    assert_eq!(tr.emitters().len(), signals::MAX_EMITTERS);
    assert!(tr.emitters().iter().all(|e| e.freq >= 50300));
}

// --------------------------------------------------------- Wi-Fi drones --

fn tello(mac: [u8; 6], rssi: i8) -> WifiDroneSighting {
    let ssid = "TELLO-5A1B2C";
    WifiDroneSighting {
        maker: wifidrone::classify(&mac, ssid.as_bytes()).unwrap(),
        mac,
        ssid: heapless::String::try_from(ssid).unwrap(),
        channel: 6,
        rssi,
        uptime_us: 0,
    }
}

#[test]
fn wifi_drones_by_maker_with_addresses_hidden() {
    let mac = [0x00, 0x1a, 0x2b, 0x3c, 0x4d, 0x5e];
    let mut tr: Tracker = Tracker::default();
    let a = tr.ingest_wifi_drone(0, &tello(mac, -60), 100);
    assert_eq!(tr.ingest_wifi_drone(1, &tello(mac, -50), 200), a);
    assert_eq!(tr.take_new_signal(200), Some(SignalKey::WifiDrone(a)));
    assert_eq!(tr.take_new_signal(200), None);
    let w = tr.get_wifi_drone(a).unwrap().clone();
    let w = &w;
    assert_eq!(
        (w.maker_name(), w.rssi, w.node, w.sightings),
        ("DJI Tello", -50, 1, 2)
    );

    let ctx = tr.context(200, None);
    let mut buf = [0u8; json::MAX_WIFI_DRONE_LINE];
    let n = json::wifi_drone_line(w, &ctx, &mut buf).unwrap();
    let v: serde_json::Value = serde_json::from_str(s(&buf, n)).unwrap();
    assert_eq!(v["t"], "wifidrone");
    assert_eq!(v["maker"], "DJI Tello");
    assert_eq!(v["ssid"], "TELLO-5A1B2C");
    assert_eq!(v["mac"], serde_json::Value::Null);
    assert_eq!(v["band"], "2.4");
    let mut ctx2 = ctx;
    ctx2.config.show_link_ids = true;
    let n = json::wifi_drone_line(w, &ctx2, &mut buf).unwrap();
    assert!(s(&buf, n).contains("\"mac\":\"00:1a:2b:3c:4d:5e\""));
    // CSV never logs the address.
    let mut row = [0u8; csv::MAX_ROW];
    let n = csv::wifi_drone_row(w, &ctx2, &mut row).unwrap();
    let line = s(&row, n);
    assert!(!line.contains("00:1a"), "{line}");
    assert_eq!(
        line.matches(',').count(),
        csv::HEADER.matches(',').count(),
        "{line}"
    );
    assert!(line.contains(",W1,") && line.contains("TELLO-5A1B2C,Ssid,DJI Tello,WifiDrone,"));

    // A maker index from a newer node still shows.
    let mut odd = tello([0, 1, 2, 3, 4, 6], -70);
    odd.maker = 200;
    let b = tr.ingest_wifi_drone(0, &odd, 300);
    assert_eq!(tr.get_wifi_drone(b).unwrap().maker_name(), "Unknown maker");
}

// ------------------------------------------------------------- nodes --

#[test]
fn status_and_hello_are_kept_per_node() {
    use link::NodeRole;
    let mut tr: Tracker = Tracker::default();
    let hello = Frame::Hello {
        role: NodeRole::Wifi24,
        protocol: link::PROTOCOL_VERSION,
        firmware: heapless::String::new(),
    };
    tr.ingest_frame(3, &hello, 10);
    assert_eq!(tr.node(3).unwrap().role, Some(NodeRole::Wifi24));
    assert_eq!(tr.node(3).unwrap().can, None);
    let can = NodeRole::Wifi24.bit() | NodeRole::DroneId.bit();
    tr.ingest_frame(
        3,
        &Frame::Status {
            role: NodeRole::DroneId,
            can,
            plan: 7,
        },
        20,
    );
    let n = tr.node(3).unwrap();
    assert_eq!(
        (n.role, n.can, n.plan, n.last_ms),
        (Some(NodeRole::DroneId), Some(can), Some(7), 20)
    );
    assert_eq!(n.can(NodeRole::DroneId), Some(true));
    assert_eq!(n.can(NodeRole::Ble), Some(false));
    tr.ingest_frame(
        9,
        &Frame::Status {
            role: NodeRole::Gateway,
            can: NodeRole::Gateway.bit(),
            plan: 0,
        },
        30,
    );
    assert_eq!(tr.nodes().len(), 2);
}

#[test]
fn ingest_frame_routes_every_kind() {
    let mut tr: Tracker = Tracker::default();
    tr.ingest_frame(
        0,
        &Frame::Detection(det(
            Transport::WifiBeacon,
            WIFI_MAC,
            -60,
            &[serial("TEST-RID-1")],
        )),
        10,
    );
    tr.ingest_frame(1, &Frame::DroneId(flight(SN)), 20);
    tr.ingest_frame(2, &Frame::Rf(rf(RfKind::AnalogVideo, 58060)), 30);
    tr.ingest_frame(0, &Frame::WifiDrone(tello([0, 1, 2, 3, 4, 5], -60)), 40);
    tr.ingest_frame(
        0,
        &Frame::DjiWifi {
            channel: 6,
            rssi: -50,
            mac: DJI_WIFI_MAC,
            uptime_us: 0,
            element: dji_element("TEST0SYNTH000002", false),
        },
        50,
    );
    tr.ingest_frame(
        0,
        &Frame::Via {
            node: [0; 6],
            rssi: 0,
        },
        60,
    );
    assert_eq!(tr.len(), 3);
    assert_eq!(tr.emitters().len(), 1);
    assert_eq!(tr.wifi_drones().len(), 1);
}

// ------------------------------------------------------------- output --

#[test]
fn rf_json_and_csv() {
    let mut tr: Tracker = Tracker::default();
    let mut v = rf(RfKind::AnalogVideo, 58060);
    v.line_hz = 15_625;
    v.width_khz = 8_000;
    let id = tr.ingest_rf(5, &v, 100);
    let e = tr.get_emitter(id).unwrap();
    let ctx = tr.context(600, None);
    let mut buf = [0u8; json::MAX_RF_LINE];
    let n = json::rf_line(e, &ctx, &mut buf).unwrap();
    let got = s(&buf, n);
    assert_eq!(
        got,
        concat!(
            r#"{"t":"rf","id":1,"state":"active","kind":"AnalogVideo","label":"Analog video","alerts":true,"#,
            r#""mhz":5806.0,"width_khz":8000,"snr":12,"peak_snr":12,"duty":100,"line_hz":15625,"n":1,"node":5,"#,
            r#""first_ms":100,"last_ms":100,"age_ms":500}"#,
            "\n"
        )
    );
    let energy = tr.ingest_rf(5, &rf(RfKind::Energy, 24370), 100);
    let n = json::rf_line(tr.get_emitter(energy).unwrap(), &ctx, &mut buf).unwrap();
    assert!(s(&buf, n).contains("\"alerts\":false"));
    let mut row = [0u8; csv::MAX_ROW];
    let n = csv::rf_row(tr.get_emitter(id).unwrap(), &ctx, &mut row).unwrap();
    let line = s(&row, n);
    assert_eq!(
        line,
        ",600,R1,,,,5806.0,FreqMHz,AnalogVideo,Rf,,,,,,,,12,,,,,\n"
    );
    assert_eq!(line.matches(',').count(), csv::HEADER.matches(',').count());
}

/// The longest lines the new data can produce fit their buffers.
#[test]
fn worst_cases_fit() {
    let mut tr: Tracker = Tracker::default();
    // A Remote ID drone with every slot and every field at its longest
    // (as tests::json_worst_case_fits), plus OcuSync and Wi-Fi DroneID.
    let sn = "\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"";
    let pack = [serial(sn), location(-89.123_456_7, -179.123_456_7, -989.5)];
    for (i, slot) in SLOTS.into_iter().enumerate() {
        let mut d = det(
            slot.transport,
            [0xff, 0xff, 0xff, 0xff, 0xff, i as u8],
            -128,
            &pack,
        );
        d.channel = if slot.band == Band::Ghz5 { 165 } else { 14 };
        tr.ingest(255, &d, u64::MAX / 4 + i as u64);
    }
    let mut f = flight(sn);
    f.freq = u32::MAX;
    (f.rssi, f.snr_db) = (-128, -128);
    let t0 = u64::MAX / 4 + 10;
    tr.ingest_droneid(255, &f, t0);
    tr.ingest_dji_wifi(255, 165, -128, [0xff; 6], &dji_element(sn, false), t0);
    tr.ingest_droneid(255, &encrypted([0xff; 4], u32::MAX), t0);
    assert_eq!(tr.len(), 2);
    tr.set_own_position(89.999_999_9, 179.999_999_9, Some(-12_345.678_9), t0);
    let ctx = tr.context(u64::MAX / 2, Some(u64::MAX / 4));
    let mut big = [0u8; 8192];
    for t in tr.tracks() {
        let n = json::track_line(t, &ctx, &mut big).unwrap();
        std::println!("worst-case JSON line with DJI: {n} bytes");
        assert!(n <= json::MAX_LINE, "{n}");
        assert!(csv::row(t, &ctx, &mut big).unwrap() <= csv::MAX_ROW);
        if let Ok(n) = cot::drone_event(t, &ctx, &mut big) {
            assert!(n <= cot::MAX_EVENT);
        }
    }

    let mut e = rf(RfKind::WifiNarrow, u32::MAX);
    (e.width_khz, e.snr_db, e.duty_pct, e.line_hz) = (u16::MAX, -128, u8::MAX, u16::MAX);
    tr.ingest_rf(u8::MAX, &e, t0);
    let n = json::rf_line(&tr.emitters()[0], &ctx, &mut big).unwrap();
    std::println!("worst-case RF line: {n} bytes");
    assert!(n <= json::MAX_RF_LINE, "{n}");
    assert!(csv::rf_row(&tr.emitters()[0], &ctx, &mut big).unwrap() <= csv::MAX_ROW);

    let mut w = tello([0xff; 6], -128);
    w.ssid = heapless::String::try_from(
        "\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"",
    )
    .unwrap();
    (w.maker, w.channel) = (0, u8::MAX);
    tr.ingest_wifi_drone(u8::MAX, &w, t0);
    let mut ctx2 = ctx;
    ctx2.config.show_link_ids = true;
    let n = json::wifi_drone_line(&tr.wifi_drones()[0], &ctx2, &mut big).unwrap();
    std::println!("worst-case Wi-Fi drone line: {n} bytes");
    assert!(n <= json::MAX_WIFI_DRONE_LINE, "{n}");
    assert!(csv::wifi_drone_row(&tr.wifi_drones()[0], &ctx2, &mut big).unwrap() <= csv::MAX_ROW);
}

#[test]
fn state_full_adds_the_new_tables_and_state_is_unchanged() {
    let mut tr: Tracker = Tracker::default();
    tr.ingest_droneid(0, &flight(SN), 100);
    tr.ingest_rf(1, &rf(RfKind::Hdzero, 57320), 100);
    tr.ingest_rf(1, &rf(RfKind::Energy, 24370), 100);
    tr.ingest_wifi_drone(2, &tello([0, 1, 2, 3, 4, 5], -60), 100);
    let ctx = tr.context(200, None);
    let hub = json::HubStatus {
        own_source: json::OwnSource::Gps,
        manual: None,
        gps: json::GpsStatus {
            state: json::GpsState::Absent,
            sats: None,
        },
        recording: false,
        nodes: &[],
        adsb: None,
        fix_held: false,
        utc_held: false,
    };
    let signals = json::Signals {
        emitters: tr.emitters(),
        wifi_drones: tr.wifi_drones(),
    };
    let mut buf = std::vec![0u8; json::state_full_max_len(1, 0, 0, 0, 2, 1)];
    let n = json::state_full(tr.tracks(), &[], &[], &signals, &ctx, &hub, &mut buf).unwrap();
    let full: serde_json::Value = serde_json::from_str(s(&buf, n)).unwrap();
    assert_eq!(full["rf"].as_array().unwrap().len(), 2);
    assert_eq!(full["wifi_drones"][0]["maker"], "DJI Tello");
    assert_eq!(full["tracks"][0]["dji"]["model"], "Mavic 3");
    assert_eq!(full["tracks"][0]["loc_src"], "DroneId");
    assert!(full["tracks"][0]["home"]["lat"].is_number());

    // `state` still ends at the aircraft, byte for byte the old layout.
    let mut old = std::vec![0u8; json::state_max_len(1, 0, 0, 0)];
    let m = json::state(tr.tracks(), &[], &[], &ctx, &hub, &mut old).unwrap();
    let old = String::from(s(&old, m));
    assert!(old.ends_with("],\"aircraft\":[]}\n"));
    assert!(!old.contains("\"rf\""));
    assert_eq!(&s(&buf, n)[..m - 3], &old[..m - 3]);
    // Too small a buffer is an error.
    assert_eq!(
        json::state_full(
            tr.tracks(),
            &[],
            &[],
            &signals,
            &ctx,
            &hub,
            &mut buf[..m + 10]
        ),
        Err(FormatError::BufferTooSmall)
    );
}

/// What the protocol-4 additions cost in the hub's static RAM (printed with
/// `--nocapture`; the C5 hub has none to spare).
#[test]
fn memory_footprint() {
    use core::mem::size_of;
    std::println!(
        "Track {} B (DjiInfo {} B), Tracker<64,0> {} B, emitters {} B, wifi drones {} B, nodes {} B",
        size_of::<Track>(),
        size_of::<DjiInfo>(),
        size_of::<Tracker<64, 0>>(),
        size_of::<signals::EmitterTable>(),
        size_of::<signals::WifiDroneTable>(),
        size_of::<signals::NodeTable>(),
    );
}

/// A decrypted INFP-10 over City Hall (what the hub's `djicrypt` layer
/// hands the tracker once the owner's key unwrapped the session).
fn infp10_plain(sn: &str) -> droneid::o4plain::Infp10 {
    use droneid::o4plain::{HOME_UNSET, Infp10};
    let mut account = [0u8; 20];
    account[..9].copy_from_slice(b"pilot-acc");
    Infp10 {
        version: 2,
        seq: 1,
        state: Infp10::STATE_SERIAL_VALID
            | Infp10::STATE_MOTORS_ON
            | Infp10::STATE_IN_AIR
            | Infp10::STATE_POSITION_VALID
            | Infp10::STATE_ALTITUDE_VALID
            | Infp10::STATE_HEIGHT_VALID
            | Infp10::STATE_HVEL_VALID
            | Infp10::STATE_APP_POSITION_VALID,
        serial: serial16(sn),
        lon_raw: rad_e7(CITY_HALL.1),
        lat_raw: rad_e7(CITY_HALL.0),
        altitude_m: 120,
        height_dm: 805,
        v_north: 300,
        v_east: 0,
        v_up: 0,
        heading_cdeg: 0,
        gps_time_ms: 1_794_232_987_250,
        app_lat_raw: rad_e7(39.9530),
        app_lon_raw: rad_e7(-75.1640),
        home_lon_raw: HOME_UNSET,
        home_lat_raw: HOME_UNSET,
        product_type: 84,
        identity_len: 0,
        identity: [0; 20],
        account_len: 9,
        account,
    }
}

#[test]
fn decrypted_o4_joins_its_serial_and_gets_positions() {
    let k = [0xde, 0xad, 0xbe, 0xef];
    let mut tr: Tracker = Tracker::default();
    // Remote ID heard first, with the same serial the O4 records carry.
    let rid = det(Transport::BleLegacy, WIFI_MAC, -60, &[serial(SN)]);
    tr.ingest(0, &rid, 500);
    tr.ingest_droneid(2, &o4(link::O4Kind::Cryp, k), 1_000);
    assert_eq!(tr.len(), 2, "session track apart until decrypted");
    let p = infp10_plain(SN);
    tr.ingest_droneid(2, &o4(link::O4Kind::Infp10, k), 1_640);
    tr.ingest_o4_decrypted(k, crate::O4Decrypted::Infp10(&p), 1_640).unwrap();
    assert_eq!(tr.len(), 2, "not on one record (`claims`)");
    tr.ingest_droneid(2, &o4(link::O4Kind::Infp10, k), 2_640);
    tr.ingest_o4_decrypted(k, crate::O4Decrypted::Infp10(&p), 2_640).unwrap();
    tr.ingest_droneid(2, &o4(link::O4Kind::Infp10, k), 3_640);
    let id = tr.ingest_o4_decrypted(k, crate::O4Decrypted::Infp10(&p), 3_640).unwrap();
    assert_eq!(tr.len(), 1, "session folded into the serial's track after three records over 2 s");
    let t = tr.get(id).unwrap();
    assert!(t.dji.o4_decrypted);
    assert_eq!(t.dji.key_id, Some(k));
    assert_eq!(t.dji.product_type, Some(84));
    let loc = &t.location.as_ref().unwrap().value;
    assert!((loc.position.unwrap().lat - CITY_HALL.0).abs() < 1e-5);
    assert_eq!(loc.altitude_pressure, Some(120.0));
    assert_eq!(loc.height, Some(80.5));
    assert!((loc.speed_horizontal.unwrap() - 3.0).abs() < 1e-3);
    let pilot = t.system.as_ref().unwrap().value.operator_position.unwrap();
    assert!((pilot.lat - 39.9530).abs() < 1e-5);
    assert!(t.home.is_none(), "home unset sentinel");
    // JSON says decrypted, not encrypted; the account ID appears nowhere.
    let ctx = tr.context(1_700, None);
    let v = json_of(t, &ctx);
    assert_eq!(v["dji"]["decrypted"], true);
    assert_eq!(v["dji"]["encrypted"], false);
    assert!(!std::format!("{v}").contains("pilot-acc"));
}

#[test]
fn decrypted_o4_without_a_known_serial_track_keeps_the_session_track() {
    let k = [1, 2, 3, 4];
    let mut tr: Tracker = Tracker::default();
    tr.ingest_droneid(2, &o4(link::O4Kind::Cryp, k), 1_000);
    let p = infp10_plain(SN);
    let id = tr.ingest_o4_decrypted(k, crate::O4Decrypted::Infp10(&p), 1_100).unwrap();
    assert_eq!(tr.len(), 1);
    let t = tr.get(id).unwrap();
    assert!(t.dji.serial.is_some() && t.dji.o4_decrypted);
    assert!(t.location.is_some());
}
