//! Golden JSON for every mapping, and a loopback through a real ZeroMQ
//! SUB socket.

use super::*;
use crate::demo;
use link::{RfKind, RfSighting};
use rid::message::*;
use serde_json::json;
use tracker::{Config, Tracker};

const SERIAL: &str = "TESTSERIAL0000001";
const WIFI: [u8; 6] = [0x02, 0xde, 0x00, 0x00, 0xa1, 0x01];

fn tracker(config: Config) -> Tracker {
    Tracker::new(config)
}

/// A Remote ID Wi-Fi beacon carrying a full message set.
fn full_beacon(t: &mut Tracker, now: u64) {
    let enc = |m: Message| rid::encode::message(&m).unwrap();
    let msgs = [
        enc(Message::BasicId(BasicId {
            id_type: IdType::SerialNumber,
            ua_type: UaType::HelicopterOrMultirotor,
            uas_id: FixedStr::new(SERIAL.as_bytes()).unwrap(),
        })),
        enc(Message::Location(Location {
            status: Status::Airborne,
            height_type: HeightType::AboveTakeoff,
            direction: Some(270.0),
            speed_horizontal: Some(15.25),
            speed_vertical: Some(0.5),
            position: Some(LatLon {
                lat: 40.123_456_7,
                lon: -75.765_432_1,
            }),
            altitude_pressure: Some(98.0),
            altitude_geodetic: Some(100.5),
            height: Some(50.0),
            horizontal_accuracy: HorizontalAccuracy::M3,
            vertical_accuracy: VerticalAccuracy::M10,
            baro_accuracy: VerticalAccuracy::M10,
            speed_accuracy: SpeedAccuracy::Mps1,
            timestamp: Some(1234.5),
            timestamp_accuracy: Some(0.2),
        })),
        enc(Message::System(System {
            operator_location_type: OperatorLocationType::Takeoff,
            classification_type: ClassificationType::Undeclared,
            operator_position: Some(LatLon {
                lat: 40.2,
                lon: -75.8,
            }),
            area_count: 1,
            area_radius: 0,
            area_ceiling: None,
            area_floor: None,
            category: Category::Undeclared,
            class: UaClass::Undeclared,
            operator_altitude: Some(20.0),
            timestamp: None,
        })),
        enc(Message::SelfId(SelfId {
            desc_type: DescType::Text,
            description: FixedStr::new(b"Survey flight").unwrap(),
        })),
        enc(Message::OperatorId(OperatorId {
            operator_id_type: OperatorIdType::OperatorId,
            operator_id: FixedStr::new(b"OP123456").unwrap(),
        })),
    ];
    let det = link::Detection {
        transport: Transport::WifiBeacon,
        channel: 6,
        rssi: -60,
        mac: WIFI,
        counter: 1,
        uptime_us: now * 1000,
        messages: heapless::Vec::from_slice(&msgs).unwrap(),
    };
    t.ingest(0, &det, now);
}

#[test]
fn remote_id_drone_golden() {
    let mut t = tracker(Config::default());
    full_beacon(&mut t, 1000);
    let ctx = t.context(1500, None);
    let m = drone_message(&t.tracks()[0], &ctx).unwrap();
    assert_eq!(
        m,
        json!([
            {"Frequency Message": {"frequency": 2_437_000_000u64}},
            {"Basic ID": {
                "id_type": "Serial Number (ANSI/CTA-2063-A)",
                "id": SERIAL, "ua_type": 2, "MAC": "02:DE:00:00:A1:01",
                "RSSI": -60, "transport": "WifiBeacon", "frequency_mhz": 2437.0}},
            {"Location/Vector Message": {
                "latitude": 40.123_456_7, "longitude": -75.765_432_1,
                "geodetic_altitude": 100.5, "pressure_altitude": 98.0,
                "height_agl": 50.0, "speed": 15.25, "vert_speed": 0.5,
                "direction": 270, "op_status": "Airborne",
                "height_type": "Above Takeoff",
                "horizontal_accuracy": "< 3m", "vertical_accuracy": "< 10m",
                "baro_accuracy": "< 10m", "speed_accuracy": "< 1 m/s",
                "timestamp": 1234.5, "timestamp_accuracy": 0.2}},
            {"System Message": {
                "latitude": 40.2, "longitude": -75.8,
                "operator_lat": 40.2, "operator_lon": -75.8}},
            {"Operator ID Message": {
                "operator_id_type": "Operator ID", "operator_id": "OP123456"}},
            {"Self-ID Message": {"text": "Survey flight"}},
        ])
    );
}

/// What DragonSync's parser needs: the list is JSON an array of objects,
/// each with one block name, and the serial-typed Basic ID is the last.
#[test]
fn registration_and_serial_put_serial_last() {
    let mut t = tracker(Config::default());
    let enc = |m: Message| rid::encode::message(&m).unwrap();
    let msgs = [
        enc(Message::BasicId(BasicId {
            id_type: IdType::SerialNumber,
            ua_type: UaType::Aeroplane,
            uas_id: FixedStr::new(SERIAL.as_bytes()).unwrap(),
        })),
        enc(Message::BasicId(BasicId {
            id_type: IdType::CaaRegistration,
            ua_type: UaType::Aeroplane,
            uas_id: FixedStr::new(b"FIN87astrdge12k8").unwrap(),
        })),
    ];
    let det = link::Detection {
        transport: Transport::BleLegacy,
        channel: 37,
        rssi: -70,
        mac: WIFI,
        counter: 1,
        uptime_us: 0,
        messages: heapless::Vec::from_slice(&msgs).unwrap(),
    };
    t.ingest(0, &det, 1000);
    let ctx = t.context(1000, None);
    let m = drone_message(&t.tracks()[0], &ctx).unwrap();
    let a = m.as_array().unwrap();
    assert_eq!(a.len(), 2, "no position, no frequency (BLE): only IDs");
    assert_eq!(a[0]["Basic ID"]["id_type"], "CAA Assigned Registration ID");
    assert_eq!(
        a[1]["Basic ID"]["id_type"],
        "Serial Number (ANSI/CTA-2063-A)"
    );
    assert_eq!(a[1]["Basic ID"]["ua_type"], 1);
    assert_eq!(a[0]["Basic ID"]["ua_type"], 1, "repeated in every block");
    assert!(a[1]["Basic ID"].get("frequency_mhz").is_none());
}

/// The demo's DJI signals: OcuSync + Remote ID, Wi-Fi DroneID, O4, undecoded.
fn dji_tracker(config: Config) -> Tracker {
    let center = (39.952_583, -75.165_222);
    let mut tr = tracker(config);
    let mut t = 0;
    while t <= 10_000 {
        for (node, f) in demo::signal_step(center, t) {
            tr.ingest_frame(node, &f, t);
        }
        t += 250;
    }
    tr
}

fn by_id<'a>(tr: &'a Tracker, s: &str) -> &'a Track {
    tr.tracks()
        .iter()
        .find(|t| t.primary_id().and_then(|i| i.id.as_str()) == Some(s))
        .unwrap()
}

#[test]
fn dji_droneid_with_serial_and_model() {
    let tr = dji_tracker(Config::default());
    let ctx = tr.context(10_000, None);
    let m = drone_message(by_id(&tr, "DEMO0CHARLIE0003"), &ctx).unwrap();
    let a = m.as_array().unwrap();
    let basic = &a[1]["Basic ID"];
    assert_eq!(basic["id"], "DEMO0CHARLIE0003");
    assert_eq!(basic["id_type"], "Serial Number (ANSI/CTA-2063-A)");
    // An OcuSync burst: its centre, in MHz, one of the demo's hops.
    let mhz = basic["frequency_mhz"].as_f64().unwrap();
    assert!([2414.5, 5756.5, 5776.5].contains(&mhz), "{mhz}");
    // The Frequency Message comes first, so the MHz value above is read last.
    assert_eq!(
        a[0]["Frequency Message"]["frequency"],
        (mhz * 1e6).round() as u64
    );
    assert!(a.iter().any(|b| b.get("Location/Vector Message").is_some()));
    // The model stands in for a Self-ID the aircraft did not send.
    assert!(
        a.iter().any(|b| b["Self-ID Message"]["text"] == "Mavic 3"),
        "{m}"
    );

    // The legacy Wi-Fi element: a serial, a home point, the MAC.
    let m = drone_message(by_id(&tr, "DEMO0DELTA000004"), &ctx).unwrap();
    let a = m.as_array().unwrap();
    assert_eq!(a[0]["Basic ID"]["MAC"], "60:60:1F:DE:00:04");
    let sys = a
        .iter()
        .find_map(|b| b.get("System Message"))
        .expect("home point");
    assert!((sys["home_lat"].as_f64().unwrap() - 39.952_583).abs() < 1e-5);
    assert!(sys.get("latitude").is_none(), "no operator position");
}

#[test]
fn o4_drone_without_serial_is_drone_alert_key_id() {
    let tr = dji_tracker(Config::default());
    let ctx = tr.context(10_000, None);
    let o4 = tr
        .tracks()
        .iter()
        .find(|t| t.dji.key_id == Some([0xde, 0xc0, 0x00, 0x04]))
        .unwrap();
    let m = drone_message(o4, &ctx).unwrap();
    let basic = &m[1]["Basic ID"];
    assert_eq!(basic["id"], "drone-alert-dec00004");
    assert_eq!(basic["id_type"], "Serial Number (ANSI/CTA-2063-A)");
    // No position: DJI's O4 record is encrypted.
    assert!(
        m.as_array()
            .unwrap()
            .iter()
            .all(|b| b.get("Location/Vector Message").is_none())
    );

    // The shared "undecoded burst" track has no identity at all.
    let und = tr.tracks().iter().find(|t| t.dji.undecoded).unwrap();
    assert!(drone_message(und, &ctx).is_none());
}

#[test]
fn hide_droneid_ids_keeps_serials_and_key_ids_out() {
    let tr = dji_tracker(Config {
        hide_droneid_ids: true,
        ..Config::default()
    });
    let ctx = tr.context(10_000, None);
    let out: Vec<String> = tr
        .tracks()
        .iter()
        .filter_map(|t| drone_message(t, &ctx))
        .map(|m| m.to_string())
        .collect();
    for s in &out {
        assert!(!s.contains("dec00004") && !s.contains("DEMO0DELTA"), "{s}");
    }
    // The O4 drone is gone; DELTA's serial came only from DroneID.
    assert!(
        tr.tracks()
            .iter()
            .filter(|t| t.dji.key_id.is_some())
            .all(|t| drone_message(t, &ctx).is_none())
    );
}

fn emitter(kind: RfKind, freq: u32, width: u16, snr: i8, line: u16) -> Emitter {
    let mut tr = tracker(Config::default());
    tr.ingest_frame(
        4,
        &link::Frame::Rf(RfSighting {
            kind,
            freq,
            width_khz: width,
            snr_db: snr,
            duty_pct: 100,
            line_hz: line,
            uptime_us: 0,
        }),
        1000,
    );
    tr.emitters()[0]
}

fn ctx_with_pos() -> Context {
    let mut tr = tracker(Config::default());
    tr.set_own_position(39.952_583, -75.165_222, Some(12.0), 1000);
    tr.context(1000, None)
}

#[test]
fn analog_video_with_line_rate_is_a_confirmed_alert() {
    let e = emitter(RfKind::AnalogVideo, 59452, 8000, 20, 15_734);
    let m = emitter_message(&e, &ctx_with_pos()).unwrap();
    assert_eq!(
        m,
        json!([
            {"Basic ID": {"id_type": "Serial Number (ANSI/CTA-2063-A)",
                "id": "fpv-alert-5945.200MHz", "description": "Analog video"}},
            {"Location/Vector Message": {
                "latitude": 39.952_583, "longitude": -75.165_222,
                "geodetic_altitude": 12.0}},
            {"Self-ID Message": {"text": "Analog video (confirm)"}},
            {"Frequency Message": {"frequency": 5_945_200_000u64}},
            {"Signal Info": {"source": "confirm", "signal_type": "analog_fpv",
                "center_hz": 5_945_200_000u64, "bandwidth_hz": 8_000_000,
                "snr_db": 20, "line_hz": 15_734}},
        ])
    );
}

#[test]
fn without_own_position_there_is_no_location_block() {
    let e = emitter(RfKind::AnalogVideo, 59452, 8000, 20, 15_734);
    let ctx = tracker(Config::default()).context(1000, None);
    let m = emitter_message(&e, &ctx).unwrap();
    assert!(
        m.as_array()
            .unwrap()
            .iter()
            .all(|b| b.get("Location/Vector Message").is_none())
    );
}

#[test]
fn only_confirmed_verdicts_say_confirm() {
    let ctx = ctx_with_pos();
    let src = |k, line| {
        let e = emitter(k, 57565, 9000, 15, line);
        let m = emitter_message(&e, &ctx)?;
        let s = m.as_array().unwrap().last().unwrap()["Signal Info"].clone();
        Some((
            s["source"].as_str().unwrap().to_string(),
            s["signal_type"].as_str().unwrap().to_string(),
        ))
    };
    let pair = |a: &str, b: &str| Some((a.to_string(), b.to_string()));
    assert_eq!(src(RfKind::DjiVideo, 0), pair("confirm", "dji_video"));
    assert_eq!(src(RfKind::DjiO4Video, 0), pair("confirm", "dji_o4_video"));
    assert_eq!(
        src(RfKind::DjiFpvAirUnit, 0),
        pair("confirm", "dji_fpv_air_unit")
    );
    assert_eq!(
        src(RfKind::DjiController, 0),
        pair("confirm", "dji_controller")
    );
    assert_eq!(src(RfKind::Hdzero, 0), pair("energy", "hdzero"));
    assert_eq!(src(RfKind::LteStyle, 0), pair("energy", "lte_ofdm"));
    assert_eq!(src(RfKind::WifiNarrow, 0), pair("energy", "wifi_narrow"));
    assert_eq!(src(RfKind::FpvCarrier, 0), pair("energy", "fpv_carrier"));
    assert_eq!(src(RfKind::Energy, 0), None);
    // Analog video with no measured line rate is not confirmed.
    assert_eq!(src(RfKind::AnalogVideo, 0), pair("energy", "analog_fpv"));
}

fn open_link(channel: u8) -> LinkTrack {
    let mut tr = tracker(Config::default());
    tr.ingest_open_link(
        2,
        &link::OpenLinkSighting {
            link: openlink::OpenLink {
                system: openlink::System::WfbNg,
                id: Some(openlink::WFB_DEFAULT_LINK_ID),
                port: 0,
                direction: openlink::Direction::AirToGround,
                tx: [0x57, 0x42, 0x75, 0x05, 0xd6, 0x00],
            },
            channel,
            rssi: -64,
            uptime_us: 0,
            packets: 100,
        },
        1000,
    );
    tr.open_links()[0].clone()
}

#[test]
fn open_link_hides_its_identifier_by_default() {
    let l = open_link(161);
    let m = open_link_message(&l, &ctx_with_pos()).unwrap();
    assert_eq!(
        m,
        json!([
            {"Basic ID": {"id_type": "Serial Number (ANSI/CTA-2063-A)",
                "id": "fpv-alert-5805.000MHz", "description": "Open FPV link"}},
            {"Location/Vector Message": {
                "latitude": 39.952_583, "longitude": -75.165_222,
                "geodetic_altitude": 12.0}},
            {"Self-ID Message": {"text": "wfb-ng FPV link (confirm)"}},
            {"Frequency Message": {"frequency": 5_805_000_000u64}},
            {"Signal Info": {"source": "confirm", "signal_type": "wfb_ng",
                "center_hz": 5_805_000_000u64, "rssi": -64}},
        ])
    );
    let s = m.to_string();
    assert!(!s.contains("7505d6") && !s.contains("0x"), "{s}");
    assert!(!s.contains("link_id"));
}

#[test]
fn open_link_id_appears_only_with_show_link_ids() {
    let l = open_link(161);
    let mut tr = tracker(Config {
        show_link_ids: true,
        ..Config::default()
    });
    tr.set_own_position(39.95, -75.16, None, 0);
    let ctx = tr.context(0, None);
    let m = open_link_message(&l, &ctx).unwrap();
    assert_eq!(m[0]["Basic ID"]["id"], "fpv-alert-5805.000MHz-7505d6");
    assert_eq!(
        m[2]["Self-ID Message"]["text"],
        "wfb-ng FPV link 0x7505d6 (confirm)"
    );
    // Never as Signal Info.link_id: DragonSync would promote it to a track.
    assert!(m.to_string().matches("link_id").count() == 0);
}

#[test]
fn open_link_without_a_channel_is_dropped() {
    assert!(open_link_message(&open_link(0), &ctx_with_pos()).is_none());
}

#[test]
fn channels_map_to_frequencies() {
    assert_eq!(wifi_channel_mhz(1), Some(2412.0));
    assert_eq!(wifi_channel_mhz(6), Some(2437.0));
    assert_eq!(wifi_channel_mhz(14), Some(2484.0));
    assert_eq!(wifi_channel_mhz(36), Some(5180.0));
    assert_eq!(wifi_channel_mhz(149), Some(5745.0));
    assert_eq!(wifi_channel_mhz(0), None);
    assert_eq!(wifi_channel_mhz(20), None);
}

// ------------------------------------------------------------ loopback --

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loopback_through_a_real_zeromq_sub_socket() {
    use zeromq::{SocketRecv, SubSocket};
    let drones = Bus::bind("test", "tcp://127.0.0.1:0").unwrap();
    let signals = Bus::bind("test", "tcp://127.0.0.1:0").unwrap();
    assert_ne!(drones.endpoint, signals.endpoint);
    let (dep, sep) = (drones.endpoint.clone(), signals.endpoint.clone());
    let mut dsync = DragonSync::new(Some(drones), Some(signals));

    let mut tr = tracker(Config::default());
    full_beacon(&mut tr, 1000);
    tr.ingest_frame(
        4,
        &link::Frame::Rf(RfSighting {
            kind: RfKind::AnalogVideo,
            freq: 59452,
            width_khz: 8000,
            snr_db: 20,
            duty_pct: 100,
            line_hz: 15_734,
            uptime_us: 0,
        }),
        1000,
    );
    let ctx = tr.context(1500, None);

    // Rate limit: one message per track per tick, and nothing again until
    // the track is heard again.
    let (d, s) = dsync.due(&tr, &ctx);
    assert_eq!((d.len(), s.len()), (1, 1));
    let (d2, s2) = dsync.due(&tr, &ctx);
    assert!(d2.is_empty() && s2.is_empty());
    full_beacon(&mut tr, 2000);
    let ctx2 = tr.context(2100, None);
    assert_eq!(dsync.due(&tr, &ctx2).0.len(), 1);

    let mut sub_d = SubSocket::new();
    sub_d.connect(&dep).await.unwrap();
    sub_d.subscribe("").await.unwrap();
    let mut sub_s = SubSocket::new();
    sub_s.connect(&sep).await.unwrap();
    sub_s.subscribe("").await.unwrap();

    // A PUB socket drops what nobody is subscribed to yet, so publish until
    // the subscribers have joined.
    let mut got: [Option<String>; 2] = [None, None];
    for _ in 0..50 {
        // Fresh ticks: forget what was sent.
        dsync.sent.clear();
        dsync.sent_rf.clear();
        dsync.tick(&tr, &ctx);
        for (i, sub) in [&mut sub_d, &mut sub_s].into_iter().enumerate() {
            if got[i].is_none()
                && let Ok(Ok(m)) =
                    tokio::time::timeout(Duration::from_millis(100), sub.recv()).await
            {
                got[i] = Some(String::from_utf8(m.get(0).unwrap().to_vec()).unwrap());
            }
        }
        if got.iter().all(Option::is_some) {
            break;
        }
    }
    let drone: Value = serde_json::from_str(got[0].as_ref().expect("drone message")).unwrap();
    assert_eq!(drone[1]["Basic ID"]["id"], SERIAL);
    let sig: Value = serde_json::from_str(got[1].as_ref().expect("signal alert")).unwrap();
    assert_eq!(
        sig.as_array().unwrap().last().unwrap()["Signal Info"]["source"],
        "confirm"
    );
    // Each port carries only its own kind.
    assert!(
        sig[0]["Basic ID"]["id"]
            .as_str()
            .unwrap()
            .starts_with("fpv-alert-")
    );
}

#[test]
fn a_taken_port_is_reported_not_ignored() {
    let a = Bus::bind("--dragonsync", "tcp://127.0.0.1:0").unwrap();
    let e = Bus::bind("--dragonsync", &a.endpoint)
        .err()
        .expect("in use");
    assert!(e.starts_with("--dragonsync tcp://127.0.0.1:"), "{e}");
    assert!(Bus::bind("--dragonsync", "ipc:///tmp/x").is_err());
}
