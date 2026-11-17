//! Track-table behaviour, fed with detections built by `rid::encode` (the
//! same encoder the bench test transmitter uses), and formatter golden output.

extern crate std;
use std::string::String;

use super::*;
use rid::RawMessage;
use rid::encode;
use rid::message::*;

pub(crate) const WIFI_MAC: [u8; 6] = [0x60, 0x60, 0x1f, 0x00, 0x00, 0x01];
const BLE_MAC: [u8; 6] = [0xc0, 0x11, 0x22, 0x33, 0x44, 0x55];
const BLE_MAC2: [u8; 6] = [0xd0, 0x99, 0x88, 0x77, 0x66, 0x55];

// Philadelphia City Hall and Independence Hall.
pub(crate) const CITY_HALL: (f64, f64) = (39.952_583, -75.165_222);
const INDEPENDENCE: (f64, f64) = (39.948_889, -75.150_000);

fn enc(m: Message) -> RawMessage {
    encode::message(&m).unwrap()
}

fn basic(id_type: IdType, id: &[u8]) -> RawMessage {
    enc(Message::BasicId(BasicId {
        id_type,
        ua_type: UaType::HelicopterOrMultirotor,
        uas_id: FixedStr::new(id).unwrap(),
    }))
}

pub(crate) fn serial(id: &str) -> RawMessage {
    basic(IdType::SerialNumber, id.as_bytes())
}

fn location_msg(lat: f64, lon: f64, alt: f32) -> Location {
    Location {
        status: Status::Airborne,
        height_type: HeightType::AboveTakeoff,
        direction: Some(90.0),
        speed_horizontal: Some(5.25),
        speed_vertical: Some(0.5),
        position: Some(LatLon { lat, lon }),
        altitude_pressure: Some(alt - 10.0),
        altitude_geodetic: Some(alt),
        height: Some(50.0),
        horizontal_accuracy: HorizontalAccuracy::M3,
        vertical_accuracy: VerticalAccuracy::M10,
        baro_accuracy: VerticalAccuracy::M10,
        speed_accuracy: SpeedAccuracy::Mps1,
        timestamp: Some(1234.5),
        timestamp_accuracy: Some(0.2),
    }
}

pub(crate) fn location(lat: f64, lon: f64, alt: f32) -> RawMessage {
    enc(Message::Location(location_msg(lat, lon, alt)))
}

fn system(lat: f64, lon: f64) -> RawMessage {
    enc(Message::System(System {
        operator_location_type: OperatorLocationType::Takeoff,
        classification_type: ClassificationType::Undeclared,
        operator_position: Some(LatLon { lat, lon }),
        area_count: 1,
        area_radius: 0,
        area_ceiling: None,
        area_floor: None,
        category: Category::Undeclared,
        class: UaClass::Undeclared,
        operator_altitude: Some(15.0),
        timestamp: Some(300_000_000),
    }))
}

fn self_id(text: &str) -> RawMessage {
    enc(Message::SelfId(SelfId {
        desc_type: DescType::Text,
        description: FixedStr::new(text.as_bytes()).unwrap(),
    }))
}

fn operator_id(text: &str) -> RawMessage {
    enc(Message::OperatorId(OperatorId {
        operator_id_type: OperatorIdType::OperatorId,
        operator_id: FixedStr::new(text.as_bytes()).unwrap(),
    }))
}

pub(crate) fn det(
    transport: Transport,
    mac: [u8; 6],
    rssi: i8,
    msgs: &[RawMessage],
) -> link::Detection {
    link::Detection {
        transport,
        channel: if matches!(transport, Transport::WifiBeacon | Transport::WifiNan) {
            6
        } else {
            37
        },
        rssi,
        mac,
        counter: 7,
        uptime_us: 0,
        messages: heapless::Vec::from_slice(msgs).unwrap(),
    }
}

fn the_id(t: &Track) -> &str {
    t.primary_id().unwrap().id.as_str().unwrap()
}

#[test]
fn beacon_and_ble_with_same_id_are_one_track() {
    let mut tr: Tracker = Tracker::default();
    let pack = [serial("SN-ALPHA"), location(39.95, -75.16, 100.0)];
    let a = tr.ingest(0, &det(Transport::WifiBeacon, WIFI_MAC, -60, &pack), 1_000);
    let b = tr.ingest(
        1,
        &det(Transport::BleLongRange, BLE_MAC, -80, &[serial("SN-ALPHA")]),
        1_100,
    );
    assert_eq!(a, b);
    assert_eq!(tr.len(), 1);
    let t = &tr.tracks()[0];
    let heard: std::vec::Vec<_> = t.transports().collect();
    assert_eq!(heard, [Transport::WifiBeacon, Transport::BleLongRange]);
    assert_eq!(t.link(Transport::WifiBeacon).rssi, -60);
    assert_eq!(t.link(Transport::BleLongRange).mac, BLE_MAC);
    assert_eq!(t.nodes, 0b11);
    assert_eq!(t.latest_link().unwrap().0, Transport::BleLongRange);
    assert!(t.location.is_some());
}

/// BLE 4 sends one message per advert: a Location may arrive before any
/// Basic ID. Those adverts form a MAC-keyed track that gains the ID later.
#[test]
fn mac_track_gains_id_on_same_mac() {
    let mut tr: Tracker = Tracker::default();
    let l = det(
        Transport::BleLegacy,
        BLE_MAC,
        -70,
        &[location(39.95, -75.16, 100.0)],
    );
    let first = tr.ingest(0, &l, 0);
    assert!(tr.tracks()[0].uas_ids.is_empty());
    let second = tr.ingest(
        0,
        &det(Transport::BleLegacy, BLE_MAC, -70, &[serial("SN-B")]),
        300,
    );
    assert_eq!(first, second);
    assert_eq!(tr.len(), 1);
    assert_eq!(the_id(&tr.tracks()[0]), "SN-B");
    assert_eq!(tr.tracks()[0].link(Transport::BleLegacy).count, 2);
}

/// The drone is already known by ID over Wi-Fi; its BLE adverts start on a
/// fresh MAC with a Location only, then a Basic ID arrives on that MAC. The
/// MAC-keyed track must fold into the ID-keyed one, keeping its data.
#[test]
fn mac_track_merges_into_id_track() {
    let mut tr: Tracker = Tracker::default();
    let wifi = tr
        .ingest(
            0,
            &det(Transport::WifiNan, WIFI_MAC, -55, &[serial("SN-C")]),
            0,
        )
        .track()
        .unwrap();
    tr.ingest(
        1,
        &det(
            Transport::BleLegacy,
            BLE_MAC,
            -75,
            &[location(39.9, -75.1, 80.0)],
        ),
        500,
    );
    tr.ingest(
        1,
        &det(Transport::BleLegacy, BLE_MAC, -75, &[self_id("survey")]),
        600,
    );
    assert_eq!(tr.len(), 2);
    let merged = tr.ingest(
        1,
        &det(Transport::BleLegacy, BLE_MAC, -74, &[serial("SN-C")]),
        700,
    );
    assert_eq!(
        merged.track(),
        Some(wifi),
        "the older, ID-keyed track survives"
    );
    assert_eq!(tr.len(), 1);
    let t = tr.get(wifi).unwrap();
    assert_eq!(t.location.as_ref().unwrap().rx_ms, 500);
    assert_eq!(
        t.self_id.as_ref().unwrap().value.description.as_str(),
        Some("survey")
    );
    assert_eq!(t.link(Transport::BleLegacy).count, 3);
    assert_eq!(t.link(Transport::WifiNan).count, 1);
    assert_eq!(t.first_seen_ms, 0);
    // Later location-only adverts on the BLE MAC land on the merged track.
    let again = tr.ingest(
        1,
        &det(
            Transport::BleLegacy,
            BLE_MAC,
            -74,
            &[location(39.9, -75.1, 81.0)],
        ),
        900,
    );
    assert_eq!(again.track(), Some(wifi));
}

/// BLE privacy: the drone's advertising address rotates. The new MAC is a
/// new MAC-keyed track until its Basic ID ties it back.
#[test]
fn rotating_ble_mac_is_reunited_by_id() {
    let mut tr: Tracker = Tracker::default();
    let a = tr.ingest(
        0,
        &det(Transport::BleLegacy, BLE_MAC, -70, &[serial("SN-D")]),
        0,
    );
    let b = tr.ingest(
        0,
        &det(
            Transport::BleLegacy,
            BLE_MAC2,
            -70,
            &[location(1.0, 1.0, 10.0)],
        ),
        1000,
    );
    assert_ne!(a, b);
    let c = tr.ingest(
        0,
        &det(Transport::BleLegacy, BLE_MAC2, -70, &[serial("SN-D")]),
        1200,
    );
    assert_eq!(c, a);
    assert_eq!(tr.len(), 1);
    assert_eq!(tr.tracks()[0].link(Transport::BleLegacy).mac, BLE_MAC2);
}

/// Serial number and session ID are both kept; a frame carrying only the
/// session ID finds the track; the serial is the primary (stable) ID.
#[test]
fn two_basic_ids_are_both_kept() {
    let mut tr: Tracker = Tracker::default();
    let session = basic(
        IdType::SpecificSession,
        &[0x01, 0xAB, 0xCD, 0xEF, 0x00, 0x7F],
    );
    let pack = [session, serial("SN-E")];
    let a = tr.ingest(0, &det(Transport::WifiBeacon, WIFI_MAC, -50, &pack), 0);
    // Session-only on another transport and MAC: same drone.
    let b = tr.ingest(
        1,
        &det(Transport::BleExtended, BLE_MAC, -70, &[session]),
        100,
    );
    assert_eq!(a, b);
    let t = tr.get(a.track().unwrap()).unwrap();
    assert_eq!(t.uas_ids.len(), 2);
    assert_eq!(t.primary_id().unwrap().id_type, IdType::SerialNumber);

    // And when the two IDs first arrive on separate tracks, a frame that
    // links them (session on the serial track's MAC) merges the two.
    let mut tr: Tracker = Tracker::default();
    let s = tr.ingest(
        0,
        &det(Transport::WifiBeacon, WIFI_MAC, -50, &[serial("SN-F")]),
        0,
    );
    let x = tr.ingest(1, &det(Transport::BleLegacy, BLE_MAC, -70, &[session]), 10);
    assert_ne!(s, x);
    let m = tr.ingest(
        1,
        &det(Transport::BleLegacy, BLE_MAC, -70, &[serial("SN-F")]),
        20,
    );
    assert_eq!(m, s);
    assert_eq!(tr.len(), 1);
    assert_eq!(tr.tracks()[0].uas_ids.len(), 2);
}

/// A MAC reused by a different drone (two test transmitters with the same
/// default address) must not merge two different serials.
#[test]
fn conflicting_ids_on_one_mac_stay_apart() {
    let mut tr: Tracker = Tracker::default();
    let a = tr.ingest(
        0,
        &det(Transport::WifiBeacon, WIFI_MAC, -50, &[serial("SN-1")]),
        0,
    );
    let b = tr.ingest(
        0,
        &det(Transport::WifiBeacon, WIFI_MAC, -50, &[serial("SN-2")]),
        10,
    );
    assert_ne!(a, b);
    assert_eq!(tr.len(), 2);
    // A Wi-Fi MAC does not match a BLE address with the same bytes.
    let c = tr.ingest(
        0,
        &det(
            Transport::BleLegacy,
            WIFI_MAC,
            -50,
            &[location(1.0, 1.0, 1.0)],
        ),
        20,
    );
    assert_ne!(c, a);
    assert_ne!(c, b);
}

#[test]
fn undecodable_frames_never_start_a_track() {
    let mut tr: Tracker = Tracker::default();
    let mut junk = [0u8; 25];
    junk[0] = 0x92; // message type 9: undefined
    assert_eq!(
        tr.ingest(0, &det(Transport::WifiBeacon, WIFI_MAC, -50, &[junk]), 0),
        Ingested::Ignored
    );
    assert!(tr.is_empty());
    // But one from a known transmitter counts against it.
    let id = tr
        .ingest(
            0,
            &det(Transport::WifiBeacon, WIFI_MAC, -50, &[serial("SN-G")]),
            1,
        )
        .track()
        .unwrap();
    assert_eq!(
        tr.ingest(0, &det(Transport::WifiBeacon, WIFI_MAC, -50, &[junk]), 2),
        Ingested::Track(id)
    );
    assert_eq!(tr.tracks()[0].decode_errors, 1);
}

#[test]
fn stale_then_removed() {
    let mut tr: Tracker = Tracker::default();
    tr.ingest(
        0,
        &det(Transport::WifiBeacon, WIFI_MAC, -50, &[serial("SN-H")]),
        0,
    );
    let state = |tr: &Tracker, now| tr.context(now, None).state(&tr.tracks()[0]);
    assert_eq!(state(&tr, 59_999), TrackState::Active);
    assert_eq!(state(&tr, 60_000), TrackState::Stale);
    assert_eq!(tr.active_count(60_000), 0);
    assert_eq!(tr.expire(599_999), 0);
    assert_eq!(tr.expire(600_000), 1);
    assert!(tr.is_empty());
}

#[test]
fn eviction_at_capacity() {
    let mut tr: Tracker<4> = Tracker::default();
    let ids: std::vec::Vec<_> = (0..4u8)
        .map(|i| {
            let mac = [2, 0, 0, 0, 0, i];
            let sn = std::format!("SN-{i}");
            tr.ingest(
                0,
                &det(Transport::WifiBeacon, mac, -50, &[serial(&sn)]),
                u64::from(i) * 1_000,
            )
            .track()
            .unwrap()
        })
        .collect();
    // All active: the least-recently-heard (SN-0) goes.
    let mut t = 10_000;
    tr.ingest(
        0,
        &det(Transport::WifiBeacon, [3; 6], -50, &[serial("SN-X")]),
        t,
    );
    assert_eq!(tr.len(), 4);
    assert!(tr.get(ids[0]).is_none());

    // Refresh SN-1 and SN-X; SN-2 and SN-3 are now stale. A new drone
    // evicts the oldest stale one (SN-2), not an active one.
    t = 70_000;
    tr.ingest(
        0,
        &det(
            Transport::WifiBeacon,
            [2, 0, 0, 0, 0, 1],
            -50,
            &[serial("SN-1")],
        ),
        t,
    );
    tr.ingest(
        0,
        &det(Transport::WifiBeacon, [3; 6], -50, &[serial("SN-X")]),
        t,
    );
    tr.ingest(
        0,
        &det(Transport::WifiBeacon, [4; 6], -50, &[serial("SN-Y")]),
        t + 1,
    );
    assert_eq!(tr.len(), 4);
    assert!(tr.get(ids[1]).is_some(), "fresh track kept");
    assert!(tr.get(ids[2]).is_none(), "oldest stale track evicted");
    assert!(tr.get(ids[3]).is_some());
}

#[test]
fn new_drone_alert_exactly_once() {
    let mut tr: Tracker = Tracker::default();
    // With an ID: due at once, and only once.
    let a = tr
        .ingest(
            0,
            &det(Transport::WifiBeacon, WIFI_MAC, -50, &[serial("SN-I")]),
            0,
        )
        .track()
        .unwrap();
    assert_eq!(tr.take_new_drone(0), Some(a));
    assert_eq!(tr.take_new_drone(0), None);
    tr.ingest(
        0,
        &det(Transport::WifiBeacon, WIFI_MAC, -50, &[serial("SN-I")]),
        100,
    );
    assert_eq!(tr.take_new_drone(100), None);

    // Without an ID: held back for the grace period...
    let m = tr
        .ingest(
            1,
            &det(
                Transport::BleLegacy,
                BLE_MAC,
                -70,
                &[location(1.0, 1.0, 1.0)],
            ),
            200,
        )
        .track()
        .unwrap();
    assert_eq!(tr.take_new_drone(500), None);
    // ...and if its ID arrives in time and merges it, no second alert.
    tr.ingest(
        1,
        &det(Transport::BleLegacy, BLE_MAC, -70, &[serial("SN-I")]),
        600,
    );
    assert_eq!(tr.take_new_drone(5_000), None);
    assert!(tr.get(m).is_none());

    // A drone that never sends an ID is still announced after the grace.
    let n = tr
        .ingest(
            1,
            &det(
                Transport::BleLegacy,
                BLE_MAC2,
                -70,
                &[location(1.0, 1.0, 1.0)],
            ),
            10_000,
        )
        .track()
        .unwrap();
    assert_eq!(tr.take_new_drone(10_999), None);
    assert_eq!(tr.take_new_drone(11_000), Some(n));
    // Its later ID does not re-announce it.
    tr.ingest(
        1,
        &det(Transport::BleLegacy, BLE_MAC2, -70, &[serial("SN-J")]),
        11_500,
    );
    assert_eq!(tr.take_new_drone(12_000), None);
}

#[test]
fn geometry_needs_a_recent_fix() {
    let mut tr: Tracker = Tracker::default();
    let pack = [
        serial("SN-K"),
        location(INDEPENDENCE.0, INDEPENDENCE.1, 140.0),
        system(CITY_HALL.0, CITY_HALL.1),
    ];
    tr.ingest(0, &det(Transport::WifiBeacon, WIFI_MAC, -50, &pack), 0);
    assert_eq!(
        tr.context(0, None).geometry(&tr.tracks()[0]),
        Geometry::default()
    );
    assert!(tr.nearest(0).is_none());

    tr.set_own_position(CITY_HALL.0, CITY_HALL.1, Some(40.0), 0);
    let g = tr.context(100, None).geometry(&tr.tracks()[0]);
    let d = g.drone.unwrap();
    assert!((d.distance_m - 1_363.931).abs() < 0.01, "{d:?}");
    assert!((d.bearing_deg - 107.4958).abs() < 0.001);
    assert!(
        g.operator.unwrap().distance_m < 0.01,
        "operator stands at City Hall"
    );
    assert_eq!(g.height_above_m, Some(100.0));
    assert_eq!(tr.nearest(100).unwrap().0.id, tr.tracks()[0].id);

    // A fix older than 10 s counts as no fix (P4).
    assert_eq!(
        tr.context(10_001, None).geometry(&tr.tracks()[0]),
        Geometry::default()
    );
    tr.set_own_position(CITY_HALL.0, CITY_HALL.1, None, 20_000);
    let g = tr.context(20_000, None).geometry(&tr.tracks()[0]);
    assert!(
        g.drone.is_some() && g.height_above_m.is_none(),
        "2D fix: no relative height"
    );
    tr.clear_own_position();
    assert!(tr.context(20_000, None).own.is_none());
}

// ----------------------------------------------------------- formatters --

/// A fully populated track at hub time 5 s, with UTC 2026-11-09T14:03:07.250Z.
fn golden() -> (Tracker, Context) {
    let mut tr: Tracker = Tracker::default();
    let pack = [
        serial("1581F5FKD229400D"),
        location(INDEPENDENCE.0, INDEPENDENCE.1, 140.0),
        system(CITY_HALL.0, CITY_HALL.1),
        self_id("Bench <test> & co"),
        operator_id("FIN87astrdge12k8"),
    ];
    tr.ingest(0, &det(Transport::WifiBeacon, WIFI_MAC, -61, &pack), 4_000);
    tr.ingest(
        1,
        &det(
            Transport::BleLongRange,
            BLE_MAC,
            -83,
            &[serial("1581F5FKD229400D")],
        ),
        4_500,
    );
    tr.set_own_position(CITY_HALL.0, CITY_HALL.1, Some(40.0), 4_900);
    let ctx = tr.context(5_000, Some(1_794_232_987_250));
    (tr, ctx)
}

fn s(buf: &[u8], n: usize) -> String {
    String::from_utf8(buf[..n].to_vec()).unwrap()
}

#[test]
fn json_golden() {
    let (tr, ctx) = golden();
    let mut buf = [0u8; 1024];
    let n = json::track_line(&tr.tracks()[0], &ctx, &mut buf).unwrap();
    let got = s(&buf, n);
    let want = concat!(
        r#"{"t":"track","id":1,"state":"active","ids":[{"type":"SerialNumber","id":"1581F5FKD229400D"}],"#,
        r#""ua":"HelicopterOrMultirotor","first_ms":4000,"last_ms":4500,"age_ms":500,"rssi":-83,"#,
        r#""links":[{"tr":"WifiBeacon","n":1,"rssi":-61,"ch":6,"band":"2.4","mac":"60:60:1f:00:00:01","ctr":7,"age_ms":1000,"node":0},"#,
        r#"{"tr":"BleLongRange","n":1,"rssi":-83,"ch":37,"band":"2.4","mac":"c0:11:22:33:44:55","ctr":7,"age_ms":500,"node":1}],"#,
        r#""loc":{"lat":39.948889,"lon":-75.15,"alt_geo":140.0,"alt_baro":130.0,"height":50.0,"height_ref":"AboveTakeoff","#,
        r#""speed":5.25,"vspeed":0.5,"dir":90.0,"status":"Airborne","hacc_m":3.0,"vacc_m":10.0,"age_ms":1000},"#,
        r#""op":{"lat":39.952583,"lon":-75.165222,"alt":15.0,"src":"Takeoff","age_ms":1000},"#,
        r#""self_id":"Bench <test> & co","op_id":"FIN87astrdge12k8","#,
        r#""own":{"dist_m":1363.9,"brg":107.5,"op_dist_m":0.0,"op_brg":0.0,"above_m":100.0},"#,
        r#""loc_src":"RemoteId","op_src":"RemoteId","home":null,"dji":null}"#,
        "\n"
    );
    assert_eq!(got, want);
    // Valid JSON per a full parser.
    let v: serde_json::Value = serde_json::from_str(&got).unwrap();
    assert_eq!(v["own"]["dist_m"], 1363.9);

    // No fix: `own` is null.
    let mut ctx2 = ctx;
    ctx2.own = None;
    let n = json::track_line(&tr.tracks()[0], &ctx2, &mut buf).unwrap();
    assert!(s(&buf, n).contains(",\"own\":null,"), "{}", s(&buf, n));
    // Too small a buffer is an error, not a truncated line.
    assert_eq!(
        json::track_line(&tr.tracks()[0], &ctx, &mut buf[..100]),
        Err(FormatError::BufferTooSmall)
    );
}

#[test]
fn cot_golden() {
    let (tr, ctx) = golden();
    let t = &tr.tracks()[0];
    let mut buf = [0u8; 2048];
    let n = cot::drone_event(t, &ctx, &mut buf).unwrap();
    let want = concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
        r#"<event version="2.0" uid="drone-1581F5FKD229400D" type="a-u-A-M-H-R" how="m-g" "#,
        r#"time="2026-11-09T14:03:06.250Z" start="2026-11-09T14:03:06.250Z" stale="2026-11-09T14:05:06.250Z">"#,
        r#"<point lat="39.9488890" lon="-75.1500000" hae="140.0" ce="3.0" le="10.0"/>"#,
        r#"<detail><contact callsign="drone-1581F5FKD229400D"/><precisionlocation geopointsrc="gps" altsrc="gps"/>"#,
        r#"<track course="90.0" speed="5.25"/>"#,
        r#"<remarks>MAC: c0:11:22:33:44:55, RSSI: -83dBm; ID Type: SerialNumber; UA Type: HelicopterOrMultirotor (2); "#,
        r#"Operator ID: FIN87astrdge12k8; Speed: 5.25 m/s; Vert Speed: 0.5 m/s; Altitude: 140 m; Height: 50 m AboveTakeoff; "#,
        r#"Course: 90°; Status: Airborne; Description: Bench &lt;test&gt; &amp; co; Transport: WifiBeacon+BleLongRange; "#,
        r#"Range: 1364 m; Bearing: 107°</remarks><color argb="-256"/></detail></event>"#,
    );
    assert_eq!(s(&buf, n), want);

    let n = cot::operator_event(t, &ctx, &mut buf).unwrap();
    let want = concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
        r#"<event version="2.0" uid="pilot-1581F5FKD229400D" type="b-m-p-s-m" how="m-g" "#,
        r#"time="2026-11-09T14:03:06.250Z" start="2026-11-09T14:03:06.250Z" stale="2026-11-09T14:05:06.250Z">"#,
        r#"<point lat="39.9525830" lon="-75.1652220" hae="15.0" ce="9999999.0" le="9999999.0"/>"#,
        r#"<detail><contact callsign="pilot-1581F5FKD229400D"/><precisionlocation geopointsrc="gps" altsrc="gps"/>"#,
        r#"<usericon iconsetpath="com.atakmap.android.maps.public/Civilian/Person.png"/>"#,
        r#"<remarks>Pilot location for drone drone-1581F5FKD229400D; Source: Takeoff</remarks></detail></event>"#,
    );
    assert_eq!(s(&buf, n), want);

    // Missing inputs are reported, not guessed.
    let mut no_time = ctx;
    no_time.utc_ms = None;
    assert_eq!(
        cot::drone_event(t, &no_time, &mut buf),
        Err(FormatError::NoTime)
    );
    assert_eq!(
        cot::drone_event(t, &ctx, &mut buf[..200]),
        Err(FormatError::BufferTooSmall)
    );
    let mut bare: Tracker = Tracker::default();
    bare.ingest(
        0,
        &det(Transport::BleLegacy, BLE_MAC, -50, &[self_id("x")]),
        0,
    );
    let c = bare.context(0, Some(0));
    assert_eq!(
        cot::drone_event(&bare.tracks()[0], &c, &mut buf),
        Err(FormatError::NoPosition)
    );
    assert_eq!(
        cot::operator_event(&bare.tracks()[0], &c, &mut buf),
        Err(FormatError::NoPosition)
    );
    assert_eq!(cot::uid_base(&bare.tracks()[0]), "mac-c01122334455");
}

#[test]
fn cot_types() {
    assert_eq!(cot::drone_type(None), "a-u-A-M-H-R");
    assert_eq!(
        cot::drone_type(Some(UaType::HelicopterOrMultirotor)),
        "a-u-A-M-H-R"
    );
    assert_eq!(cot::drone_type(Some(UaType::Aeroplane)), "a-u-A-M-F-Q");
    assert_eq!(cot::drone_type(Some(UaType::FreeBalloon)), "b-m-p-s-m");
}

#[test]
fn csv_golden() {
    let (tr, ctx) = golden();
    let mut buf = [0u8; 512];
    let n = csv::header(&mut buf).unwrap();
    assert_eq!(s(&buf, n), csv::HEADER);
    assert_eq!(csv::HEADER.matches(',').count(), 22);
    let n = csv::row(&tr.tracks()[0], &ctx, &mut buf).unwrap();
    let want = "2026-11-09T14:03:07.250Z,5000,1,39.9525830,-75.1652220,40.0,1581F5FKD229400D,SerialNumber,\
HelicopterOrMultirotor,WifiBeacon|BleLongRange,39.9488890,-75.1500000,140.0,50.0,5.25,39.9525830,\
-75.1652220,-83,1363.9,107.5,0.0,0.0,130.0\n";
    assert_eq!(s(&buf, n), want);
    assert_eq!(want.matches(',').count(), 22);

    // Unknowns are empty fields; awkward IDs are quoted.
    let mut bare: Tracker = Tracker::default();
    bare.ingest(
        0,
        &det(Transport::BleLegacy, BLE_MAC, -50, &[serial("A,\"B\"")]),
        0,
    );
    let n = csv::row(&bare.tracks()[0], &bare.context(0, None), &mut buf).unwrap();
    assert_eq!(
        s(&buf, n),
        ",0,1,,,,\"A,\"\"B\"\"\",SerialNumber,HelicopterOrMultirotor,BleLegacy,,,,,,,,-50,,,,,\n"
    );
}

/// The decoded path (laptop JSON) merges exactly like the raw path.
#[test]
fn decoded_messages_merge_like_raw() {
    let mut tr: Tracker = Tracker::default();
    let raw = tr.ingest(
        0,
        &det(Transport::WifiBeacon, WIFI_MAC, -50, &[serial("SN-Z")]),
        0,
    );
    let obs = Observation {
        node: 1,
        transport: Transport::BleLegacy,
        channel: 37,
        rssi: -70,
        mac: BLE_MAC,
        counter: 1,
        rx_ms: 10,
    };
    let msgs = [
        decode(&serial("SN-Z")).unwrap(),
        decode(&location(1.0, 2.0, 3.0)).unwrap(),
    ];
    assert_eq!(tr.ingest_messages(&obs, &msgs, 2), raw);
    assert_eq!(tr.len(), 1);
    assert_eq!(tr.tracks()[0].decode_errors, 2);
    assert!(tr.tracks()[0].location.is_some());
}

/// The longest line a track can produce fits [`json::MAX_LINE`].
#[test]
fn json_worst_case_fits() {
    let mut tr: Tracker = Tracker::default();
    // Binary session ID (hex doubles it), quote-heavy text (escaped), all
    // five transports with long MACs and negative everything.
    let session = basic(IdType::SpecificSession, &[0xFE; 20]);
    let pack = [
        serial("\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"\""),
        session,
        location(-89.123_456_7, -179.123_456_7, -989.5),
        system(-89.765_432_1, -179.765_432_1),
        self_id("\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\"),
        operator_id("\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"\""),
    ];
    // Every slot: each Wi-Fi transport on a 2.4 GHz and a 5 GHz channel.
    for (i, slot) in SLOTS.into_iter().enumerate() {
        let mut d = det(
            slot.transport,
            [0xff, 0xff, 0xff, 0xff, 0xff, i as u8],
            -128,
            &pack,
        );
        d.channel = if slot.band == Band::Ghz5 { 165 } else { 14 };
        d.counter = 255;
        tr.ingest(255, &d, u64::MAX / 4 + i as u64);
    }
    assert_eq!(tr.len(), 1);
    assert_eq!(tr.tracks()[0].slots().count(), SLOTS.len());
    // And DJI's own broadcasts, every field at its widest.
    let wide = dji::DjiLink {
        count: u32::MAX,
        last_seen_ms: 0,
        rssi: -128,
        snr_db: -128,
        freq: u32::MAX,
        channel: u8::MAX,
        mac: [0xff; 6],
        node: u8::MAX,
    };
    let t = &mut tr.tracks[0];
    t.dji = dji::DjiInfo {
        serial: None,
        product_type: Some(69),
        key_id: Some([0xff; 4]),
        undecoded: false,
        o4_decrypted: false,
        o4: dji::O4Seen { cryp: u32::MAX, infp10: u32::MAX, infp11: u32::MAX, last_cryp_ms: u64::MAX, last_infp_ms: u64::MAX },
        ocusync: wide,
        wifi: wide,
    };
    t.home = Some(Stamped {
        value: LatLon {
            lat: -89.123_456_789_012_3,
            lon: -179.123_456_789_012_3,
        },
        rx_ms: 0,
    });
    t.location_src = Source::DjiWifi;
    t.system_src = Source::DjiWifi;
    tr.set_own_position(
        89.999_999_9,
        179.999_999_9,
        Some(-12_345.678_9),
        u64::MAX / 4,
    );
    let ctx = tr.context(u64::MAX / 2, None);
    let mut big = [0u8; 4096];
    let n = json::track_line(&tr.tracks()[0], &ctx, &mut big).unwrap();
    std::println!("worst-case JSON line: {n} bytes");
    assert!(n <= json::MAX_LINE, "{n}");
    // CoT and CSV for the same track fit the buffers rid-view uses.
    let ctx = tr.context(u64::MAX / 2, Some(u64::MAX / 4));
    assert!(cot::drone_event(&tr.tracks()[0], &ctx, &mut big).unwrap() <= cot::MAX_EVENT);
    assert!(csv::row(&tr.tracks()[0], &ctx, &mut big).unwrap() <= csv::MAX_ROW);
}

// ------------------------------------------------------------ /api/state --

fn nodes() -> [json::NodeStatus; 2] {
    [
        json::NodeStatus {
            role: link::NodeRole::Wifi24,
            alive: true,
            age_ms: Some(300),
            seen_per_s: Some(412),
            rid_per_s: 1,
            drops: Some(0),
            can: None,
            plan: None,
        },
        json::NodeStatus {
            role: link::NodeRole::Ble,
            alive: false,
            age_ms: None,
            seen_per_s: None,
            rid_per_s: 0,
            drops: None,
            can: None,
            plan: None,
        },
    ]
}

#[test]
fn state_golden() {
    let (tr, ctx) = golden();
    let nodes = nodes();
    let hub = json::HubStatus {
        own_source: json::OwnSource::Gps,
        manual: None,
        gps: json::GpsStatus {
            state: json::GpsState::Fix3d,
            sats: Some(9),
        },
        recording: true,
        nodes: &nodes,
        adsb: None,
    };
    let mut buf = [0u8; 4096];
    let n = json::state(
        tr.tracks(),
        tr.open_links(),
        tr.aircraft(),
        &ctx,
        &hub,
        &mut buf,
    )
    .unwrap();
    let got = s(&buf, n);
    let mut line = [0u8; 1024];
    let ln = json::track_line(&tr.tracks()[0], &ctx, &mut line).unwrap();
    let track = s(&line, ln - 1); // without its newline
    let want = std::format!(
        concat!(
            r#"{{"t":"state","v":1,"now_ms":5000,"utc_ms":1794232987250,"#,
            r#""own":{{"lat":39.952583,"lon":-75.165222,"alt":40.0,"source":"gps","age_ms":100}},"#,
            r#""manual":null,"gps":{{"state":"3d","sats":9}},"rec":true,"#,
            r#""nodes":[{{"role":"Wifi24","alive":true,"age_ms":300,"seen_per_s":412,"rid_per_s":1,"drops":0,"can":null,"plan":null}},"#,
            r#"{{"role":"Ble","alive":false,"age_ms":null,"seen_per_s":null,"rid_per_s":0,"drops":null,"can":null,"plan":null}}],"#,
            r#""adsb":null,"open_links":[],"tracks":[{}],"aircraft":[]}}"#,
            "\n"
        ),
        track
    );
    assert_eq!(got, want);
    let v: serde_json::Value = serde_json::from_str(&got).unwrap();
    assert_eq!(v["tracks"][0]["own"]["dist_m"], 1363.9);
    assert_eq!(v["nodes"][1]["role"], "Ble");
}

#[test]
fn state_without_fix_or_tracks() {
    let tr: Tracker = Tracker::default();
    let ctx = tr.context(1_000, None);
    let hub = json::HubStatus {
        own_source: json::OwnSource::Gps,
        manual: Some(json::ManualPos {
            lat: 1.5,
            lon: -2.25,
            alt: None,
        }),
        gps: json::GpsStatus {
            state: json::GpsState::Searching,
            sats: None,
        },
        recording: false,
        nodes: &[],
        adsb: None,
    };
    let mut buf = [0u8; 512];
    let n = json::state(
        tr.tracks(),
        tr.open_links(),
        tr.aircraft(),
        &ctx,
        &hub,
        &mut buf,
    )
    .unwrap();
    assert_eq!(
        s(&buf, n),
        concat!(
            r#"{"t":"state","v":1,"now_ms":1000,"utc_ms":null,"own":null,"#,
            r#""manual":{"lat":1.5,"lon":-2.25,"alt":null},"gps":{"state":"searching","sats":null},"#,
            r#""rec":false,"nodes":[],"adsb":null,"open_links":[],"tracks":[],"aircraft":[]}"#,
            "\n"
        )
    );
}

#[test]
fn state_several_tracks_and_small_buffers() {
    let mut tr: Tracker = Tracker::default();
    for (i, id) in ["SN-A", "SN-B", "SN-C"].into_iter().enumerate() {
        let mac = [0x60, 0, 0, 0, 0, i as u8];
        tr.ingest(
            0,
            &det(
                Transport::WifiBeacon,
                mac,
                -60,
                &[serial(id), location(1.0, 2.0, 3.0)],
            ),
            100,
        );
    }
    tr.set_own_position(1.0, 2.001, None, 100);
    let ctx = tr.context(200, None);
    let nodes = nodes();
    let hub = json::HubStatus {
        own_source: json::OwnSource::Manual,
        manual: Some(json::ManualPos {
            lat: 1.0,
            lon: 2.001,
            alt: None,
        }),
        gps: json::GpsStatus {
            state: json::GpsState::Absent,
            sats: None,
        },
        recording: false,
        nodes: &nodes,
        adsb: None,
    };
    let mut buf = std::vec![0u8; json::state_max_len(3, 0, 0, 2)];
    let n = json::state(
        tr.tracks(),
        tr.open_links(),
        tr.aircraft(),
        &ctx,
        &hub,
        &mut buf,
    )
    .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&buf[..n]).unwrap();
    assert_eq!(v["tracks"].as_array().unwrap().len(), 3);
    assert_eq!(v["tracks"][2]["ids"][0]["id"], "SN-C");
    assert_eq!(v["own"]["source"], "manual");
    assert_eq!(v["own"]["alt"], serde_json::Value::Null);
    assert_eq!(v["gps"]["state"], "absent");
    // Every shorter buffer is an error, never a truncated object.
    for len in 0..n {
        assert_eq!(
            json::state(
                tr.tracks(),
                tr.open_links(),
                tr.aircraft(),
                &ctx,
                &hub,
                &mut buf[..len]
            ),
            Err(FormatError::BufferTooSmall),
            "len {len}"
        );
    }
}

/// The documented bound holds for the worst-case head and nodes.
#[test]
fn state_head_and_nodes_fit_their_bounds() {
    let mut tr: Tracker = Tracker::default();
    tr.set_own_position(
        -89.123_456_789_012_3,
        -179.123_456_789_012_3,
        Some(-12_345.678_901_234),
        0,
    );
    let ctx = tr.context(9_000, Some(u64::MAX));
    let mut ctx = ctx;
    ctx.now_ms = u64::MAX;
    if let Some(o) = &mut ctx.own {
        o.fix_ms = 0;
    }
    let node = json::NodeStatus {
        role: link::NodeRole::Wifi24,
        alive: false,
        age_ms: Some(u64::MAX),
        seen_per_s: Some(u32::MAX),
        rid_per_s: u32::MAX,
        drops: Some(u32::MAX),
        can: Some(u8::MAX),
        plan: Some(u16::MAX),
    };
    let hub = |nodes| json::HubStatus {
        own_source: json::OwnSource::Manual,
        manual: Some(json::ManualPos {
            lat: -89.123_456_789_012_3,
            lon: -179.123_456_789_012_3,
            alt: Some(-12_345.678_901_234),
        }),
        gps: json::GpsStatus {
            state: json::GpsState::Searching,
            sats: Some(255),
        },
        recording: false,
        nodes,
        adsb: None,
    };
    let mut buf = [0u8; 2048];
    let head = json::state(&[], &[], &[], &ctx, &hub(&[]), &mut buf).unwrap();
    std::println!("worst-case state head: {head} bytes");
    assert!(head <= json::STATE_HEAD_MAX, "{head}");
    let with = json::state(&[], &[], &[], &ctx, &hub(&[node; 4]), &mut buf).unwrap();
    assert!(with - head <= 4 * json::STATE_NODE_MAX, "{}", with - head);
}

#[test]
fn own_pos_requests() {
    use json::{OwnPosError as E, OwnPosRequest as R, parse_own_pos as p};
    assert_eq!(
        p(br#"{"lat":39.9526,"lon":-75.1652,"alt":12.5}"#),
        Ok(R::Set(json::ManualPos {
            lat: 39.9526,
            lon: -75.1652,
            alt: Some(12.5)
        }))
    );
    assert_eq!(
        p(br#" {"lon":-75,"lat":40,"alt":null,"note":"x"} "#),
        Ok(R::Set(json::ManualPos {
            lat: 40.0,
            lon: -75.0,
            alt: None
        }))
    );
    assert_eq!(p(br#"{"clear":true}"#), Ok(R::Clear));
    assert_eq!(p(br#"{"clear":false}"#), Err(E::Missing));
    assert_eq!(p(br#"{"lat":40}"#), Err(E::Missing));
    assert_eq!(p(br#"{"lat":91,"lon":0}"#), Err(E::OutOfRange));
    assert_eq!(p(br#"{"lat":0,"lon":-180.5}"#), Err(E::OutOfRange));
    assert_eq!(p(br#"{"lat":0,"lon":0,"alt":1e9}"#), Err(E::OutOfRange));
    assert_eq!(p(b"lat=1&lon=2"), Err(E::Malformed));
    assert_eq!(p(br#"{"lat":"40","lon":0}"#), Err(E::Malformed));
    assert_eq!(p(b""), Err(E::Malformed));
}

// ------------------------------------------------------------ band split --

/// A Wi-Fi detection on a given channel (the shared `det` uses channel 6).
fn wifi_on(transport: Transport, channel: u8, rssi: i8, msgs: &[RawMessage]) -> link::Detection {
    let mut d = det(transport, WIFI_MAC, rssi, msgs);
    d.channel = channel;
    d
}

#[test]
fn band_of_channel() {
    assert_eq!(Band::of_wifi_channel(0), Band::Ghz24);
    assert_eq!(Band::of_wifi_channel(1), Band::Ghz24);
    assert_eq!(Band::of_wifi_channel(14), Band::Ghz24);
    assert_eq!(Band::of_wifi_channel(36), Band::Ghz5);
    assert_eq!(Band::of_wifi_channel(149), Band::Ghz5);
    assert_eq!(Band::of_wifi_channel(165), Band::Ghz5);
    // Bluetooth advertising channels are never 5 GHz.
    assert_eq!(Slot::of(Transport::BleLongRange, 37).band, Band::Ghz24);
    assert_eq!(Slot::of(Transport::BleLegacy, 39).band, Band::Ghz24);
    assert_eq!(Slot::of(Transport::WifiNan, 149).name(), "WifiNan5G");
    // SLOTS and Slot::index agree.
    for (i, s) in SLOTS.into_iter().enumerate() {
        assert_eq!(s.index(), i, "{s:?}");
        assert_eq!(
            Slot::of(s.transport, if s.band == Band::Ghz5 { 149 } else { 6 }),
            s
        );
    }
}

/// The same drone heard on 2.4 GHz (S3 node) and 5 GHz (C5 node) keeps
/// both: neither band's RSSI and channel overwrite the other's.
#[test]
fn wifi_on_both_bands_keeps_both_slots() {
    let mut tr: Tracker = Tracker::default();
    let pack = [serial("SN-DUAL"), location(39.95, -75.16, 100.0)];
    tr.ingest(0, &wifi_on(Transport::WifiBeacon, 6, -60, &pack), 1_000);
    tr.ingest(2, &wifi_on(Transport::WifiBeacon, 149, -75, &pack), 1_200);
    tr.ingest(0, &wifi_on(Transport::WifiBeacon, 6, -61, &pack), 2_000);
    assert_eq!(tr.len(), 1);
    let t = &tr.tracks()[0];

    let b24 = t.slot(Slot::of(Transport::WifiBeacon, 6));
    let b5 = t.slot(Slot::of(Transport::WifiBeacon, 149));
    assert_eq!((b24.count, b24.rssi, b24.channel, b24.node), (2, -61, 6, 0));
    assert_eq!((b5.count, b5.rssi, b5.channel, b5.node), (1, -75, 149, 2));
    // One transport, two slots.
    let heard: std::vec::Vec<_> = t.transports().collect();
    assert_eq!(heard, [Transport::WifiBeacon]);
    assert_eq!(t.slots().count(), 2);
    // The band-agnostic view adds the counts and takes the newest band.
    let any = t.link(Transport::WifiBeacon);
    assert_eq!((any.count, any.rssi, any.channel), (3, -61, 6));
    assert_eq!(t.latest_slot().unwrap().0.band, Band::Ghz24);
    assert_eq!(t.nodes, 0b101);

    // JSON: one entry per band, each labelled.
    let ctx = tr.context(2_000, None);
    let mut buf = [0u8; json::MAX_LINE];
    let n = json::track_line(t, &ctx, &mut buf).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&buf[..n]).unwrap();
    let links = v["links"].as_array().unwrap();
    assert_eq!(links.len(), 2);
    assert_eq!(
        (
            links[0]["tr"].as_str(),
            links[0]["band"].as_str(),
            links[0]["ch"].as_u64()
        ),
        (Some("WifiBeacon"), Some("2.4"), Some(6))
    );
    assert_eq!(
        (
            links[1]["tr"].as_str(),
            links[1]["band"].as_str(),
            links[1]["ch"].as_u64()
        ),
        (Some("WifiBeacon"), Some("5"), Some(149))
    );
    assert_eq!(links[1]["n"], 1);

    // CSV and CoT name the 5 GHz slot.
    let n = csv::row(t, &ctx, &mut buf).unwrap();
    assert!(
        s(&buf, n).contains(",WifiBeacon|WifiBeacon5G,"),
        "{}",
        s(&buf, n)
    );
    let ctx = tr.context(2_000, Some(1_794_232_987_250));
    let n = cot::drone_event(t, &ctx, &mut buf).unwrap();
    assert!(
        s(&buf, n).contains("Transport: WifiBeacon+WifiBeacon5G"),
        "{}",
        s(&buf, n)
    );
}

/// A MAC first heard on 5 GHz is matched by the same MAC on 2.4 GHz: Wi-Fi
/// is one address namespace whatever the band.
#[test]
fn mac_matches_across_bands() {
    let mut tr: Tracker = Tracker::default();
    tr.ingest(
        2,
        &wifi_on(Transport::WifiNan, 149, -70, &[location(1.0, 2.0, 3.0)]),
        100,
    );
    tr.ingest(
        0,
        &wifi_on(Transport::WifiBeacon, 6, -50, &[serial("SN-X")]),
        200,
    );
    assert_eq!(tr.len(), 1);
    let t = &tr.tracks()[0];
    assert_eq!(the_id(t), "SN-X");
    assert!(t.slot(Slot::of(Transport::WifiNan, 149)).heard());
    assert!(!t.slot(Slot::of(Transport::WifiNan, 6)).heard());
}

/// Merging two tracks keeps each band's slot separately.
#[test]
fn merge_keeps_bands_apart() {
    let mut tr: Tracker = Tracker::default();
    // A 5 GHz MAC-only track, then the same MAC with its ID on 5 GHz, while
    // a 2.4 GHz track already holds the ID: the two fold together.
    tr.ingest(
        0,
        &wifi_on(Transport::WifiBeacon, 1, -40, &[serial("SN-M")]),
        100,
    );
    let mut d = wifi_on(Transport::WifiBeacon, 36, -80, &[location(1.0, 2.0, 3.0)]);
    d.mac = [0x02, 0, 0, 0, 0, 9];
    tr.ingest(2, &d, 150);
    assert_eq!(tr.len(), 2);
    let mut d = wifi_on(Transport::WifiBeacon, 36, -79, &[serial("SN-M")]);
    d.mac = [0x02, 0, 0, 0, 0, 9];
    tr.ingest(2, &d, 2_000);
    assert_eq!(tr.len(), 1);
    let t = &tr.tracks()[0];
    assert_eq!(t.slot(Slot::of(Transport::WifiBeacon, 1)).rssi, -40);
    let s5 = t.slot(Slot::of(Transport::WifiBeacon, 36));
    assert_eq!((s5.count, s5.rssi), (2, -79));
}
