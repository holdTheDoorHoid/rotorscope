//! `from_tracker`: a real tracker, fed decoded Remote ID messages, fills a
//! UiState that renders.

use rid::Transport;
use rid::message::*;
use screen::from_tracker::fill_from_tracker;
use screen::*;
use tracker::{Observation, Tracker};

// Philadelphia City Hall (the unit) and Independence Hall (the drone).
const CITY_HALL: (f64, f64) = (39.952_583, -75.165_222);
const INDEPENDENCE: (f64, f64) = (39.948_889, -75.150_000);

fn obs(transport: Transport, channel: u8, rssi: i8, mac: [u8; 6], rx_ms: u64) -> Observation {
    Observation {
        node: 0,
        transport,
        channel,
        rssi,
        mac,
        counter: 1,
        rx_ms,
    }
}

fn location(lat: f64, lon: f64) -> Message {
    Message::Location(Location {
        status: Status::Airborne,
        height_type: HeightType::AboveTakeoff,
        direction: Some(90.0),
        speed_horizontal: Some(5.25),
        speed_vertical: Some(-1.0),
        position: Some(LatLon { lat, lon }),
        altitude_pressure: Some(130.0),
        altitude_geodetic: Some(140.0),
        height: Some(50.0),
        horizontal_accuracy: HorizontalAccuracy::M3,
        vertical_accuracy: VerticalAccuracy::M10,
        baro_accuracy: VerticalAccuracy::M10,
        speed_accuracy: SpeedAccuracy::Mps1,
        timestamp: None,
        timestamp_accuracy: None,
    })
}

#[test]
fn fills_from_a_tracker() {
    let mut t: Tracker<8> = Tracker::default();
    t.set_own_position(CITY_HALL.0, CITY_HALL.1, Some(40.0), 1_000);

    let id = Message::BasicId(BasicId {
        id_type: IdType::SerialNumber,
        ua_type: UaType::HelicopterOrMultirotor,
        uas_id: FixedStr::new(b"1581F5FKD229400D").unwrap(),
    });
    let self_id = Message::SelfId(SelfId {
        desc_type: DescType::Text,
        description: FixedStr::new(b"Bench test").unwrap(),
    });
    let wifi = [0x60, 0x60, 0x1f, 0, 0, 1];
    let ble = [0xc0, 0x11, 0x22, 0x33, 0x44, 0x55];
    let loc = location(INDEPENDENCE.0, INDEPENDENCE.1);
    t.ingest_messages(
        &obs(Transport::WifiBeacon, 149, -61, wifi, 2_000),
        &[id.clone(), loc, self_id],
        0,
    );
    t.ingest_messages(&obs(Transport::BleLongRange, 37, -83, ble, 2_500), &[id], 0);
    // A second transmitter with no Basic ID yet: keyed by its MAC.
    t.ingest_messages(
        &obs(Transport::BleLegacy, 38, -90, [0xd0, 1, 2, 3, 4, 5], 2_600),
        &[location(39.95, -75.16)],
        0,
    );

    let mut st = UiState {
        gps: Gps {
            fix: Fix::Fix3D,
            sats: 9,
        },
        ..UiState::default()
    };
    fill_from_tracker(&t, &t.context(3_000, Some(1_000_000)), &mut st);

    assert_eq!(st.total_drones, 2);
    assert_eq!(st.utc_ms, Some(1_000_000));
    let d = st
        .drones
        .iter()
        .find(|d| d.id_kind == IdKind::Serial)
        .unwrap();
    assert_eq!(d.id.as_str(), "1581F5FKD229400D");
    assert_eq!(d.ua, UaKind::Multirotor);
    assert_eq!(d.self_id.as_str(), "Bench test");
    let pos = d.drone.unwrap();
    assert!((pos.distance_m - 1363.9).abs() < 1.0, "{}", pos.distance_m);
    assert!((pos.bearing_deg - 107.5).abs() < 0.5, "{}", pos.bearing_deg);
    assert_eq!(d.above_you_m, Some(100.0));
    // Channel 149 is 5 GHz: shown as "5", not "W".
    assert!(d.radios.contains(Radio::Wifi5));
    assert!(!d.radios.contains(Radio::WifiBeacon));
    assert!(d.radios.contains(Radio::BleLongRange));
    assert_eq!(d.rssi[Radio::Wifi5.index()], Some(-61));
    assert_eq!(d.rssi[Radio::BleLongRange.index()], Some(-83));

    let m = st.drones.iter().find(|d| d.id_kind == IdKind::Mac).unwrap();
    assert_eq!(m.id.as_str(), "d0:01:02:03:04:05");

    // Fix too old for the tracker: the screen switches to the P4 layout.
    fill_from_tracker(&t, &t.context(60_000, None), &mut st);
    assert_eq!(st.gps.fix, Fix::None);
    assert!(st.drones.iter().all(|d| d.drone.is_none()));
}
