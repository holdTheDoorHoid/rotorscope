//! `demo` feature: two synthetic drones, so every output can be exercised
//! with no radio nodes attached.
//!
//! Conduct: the identities are deliberately and obviously fake (`DEMO-...`,
//! Self-ID "DEMO - NOT AN AIRCRAFT"). Nothing is transmitted: the detections
//! are built with `rid::encode` and pushed into the same queue the node UARTs
//! feed, exactly as if Node A or Node B had heard them.
//!
//! - DEMO-ALPHA-0001: 120 m circle, 90 s lap, heard as a Wi-Fi beacon (pack).
//! - DEMO-BRAVO-0002: 250 m circle the other way, 150 s lap, heard over BLE
//!   Long Range (pack) and, every third second, as BT4 legacy single messages,
//!   so the cross-transport merge is exercised.
//! - A wfb-ng FPV link on channel 161 (video down, MAVLink up), sent as the
//!   5 GHz node would: one `OpenLink` sighting per stream per second with a
//!   running count. It exercises the open-link table and outputs.
//! - With `adsbee`: a helicopter, callsign DEMOHELI, address DE0001, circling
//!   1.5 km out at 900 ft, as the ADSBee would report it once a second. Its
//!   CSBee line is built with `adsb::encode` and parsed back by the same
//!   `adsb::LineSplitter` the UART uses, then queued like a UART report.
//!   It is low and near, so it exercises the quiet "low aircraft" line, and
//!   it must never alert.

use core::f64::consts::PI;

use embassy_time::{Duration, Ticker};
use link::{Detection, Frame, OpenLinkSighting};
use rid::message::{
    BasicId, Category, ClassificationType, DescType, FixedStr, HeightType, HorizontalAccuracy, IdType, LatLon,
    Location, Message, OperatorId, OperatorIdType, OperatorLocationType, SelfId, SpeedAccuracy, Status, System,
    UaClass, UaType, VerticalAccuracy,
};
use rid::{RawMessage, Transport, encode};

use crate::hub::now_ms;
use crate::links::FRAMES;

/// Detections carry this node number; no real link uses it.
pub const NODE: u8 = 9;

const GROUND_M: f32 = 15.0;
const EARTH_R: f64 = 6_371_000.0;

struct Drone {
    uas_id: &'static [u8],
    operator_id: &'static [u8],
    radius_m: f64,
    period_s: f64,
    /// +1 clockwise, -1 counter-clockwise.
    dir: f64,
    height_m: f32,
    mac: [u8; 6],
    transport: Transport,
    channel: u8,
    rssi: i8,
}

const ALPHA: Drone = Drone {
    uas_id: b"DEMO-ALPHA-0001",
    operator_id: b"DEMO-OPERATOR-A",
    radius_m: 120.0,
    period_s: 90.0,
    dir: 1.0,
    height_m: 60.0,
    // Locally administered, obviously synthetic.
    mac: [0x02, 0xde, 0x40, 0x00, 0x00, 0x01],
    transport: Transport::WifiBeacon,
    channel: 6,
    rssi: -58,
};

const BRAVO: Drone = Drone {
    uas_id: b"DEMO-BRAVO-0002",
    operator_id: b"DEMO-OPERATOR-B",
    radius_m: 250.0,
    period_s: 150.0,
    dir: -1.0,
    height_m: 90.0,
    mac: [0x02, 0xde, 0x40, 0x00, 0x00, 0x02],
    transport: Transport::BleLongRange,
    channel: 37,
    rssi: -77,
};

const SELF_ID: &[u8] = b"DEMO - NOT AN AIRCRAFT";

/// Position, heading and speed on the circle at `t_s`.
fn at(d: &Drone, (clat, clon): (f64, f64), t_s: f64) -> (f64, f64, f32, f32) {
    let theta = d.dir * 2.0 * PI * (t_s % d.period_s) / d.period_s;
    let north = d.radius_m * libm::cos(theta);
    let east = d.radius_m * libm::sin(theta);
    let lat = clat + (north / EARTH_R).to_degrees();
    let lon = clon + (east / (EARTH_R * libm::cos(clat.to_radians()))).to_degrees();
    // Velocity is d/dt (cos, sin) * R * omega * dir = (-sin, cos) * ...
    let heading = (libm::atan2(d.dir * libm::cos(theta), -d.dir * libm::sin(theta)).to_degrees() + 360.0) % 360.0;
    let speed = 2.0 * PI * d.radius_m / d.period_s;
    (lat, lon, heading as f32, speed as f32)
}

fn messages(d: &Drone, centre: (f64, f64), t_s: f64) -> Result<[RawMessage; 5], rid::Error> {
    let (lat, lon, heading, speed) = at(d, centre, t_s);
    let basic = Message::BasicId(BasicId {
        id_type: IdType::SerialNumber,
        ua_type: UaType::HelicopterOrMultirotor,
        uas_id: FixedStr::new(d.uas_id)?,
    });
    let location = Message::Location(Location {
        status: Status::Airborne,
        height_type: HeightType::AboveTakeoff,
        direction: Some(heading),
        speed_horizontal: Some(speed),
        speed_vertical: Some(0.0),
        position: Some(LatLon { lat, lon }),
        altitude_pressure: None,
        altitude_geodetic: Some(GROUND_M + d.height_m),
        height: Some(d.height_m),
        horizontal_accuracy: HorizontalAccuracy::M10,
        vertical_accuracy: VerticalAccuracy::M10,
        baro_accuracy: VerticalAccuracy::Unknown,
        speed_accuracy: SpeedAccuracy::Mps1,
        timestamp: Some(((t_s % 3600.0) * 10.0) as u32 as f32 / 10.0),
        timestamp_accuracy: Some(0.1),
    });
    let self_id = Message::SelfId(SelfId { desc_type: DescType::Text, description: FixedStr::new(SELF_ID)? });
    let system = Message::System(System {
        operator_location_type: OperatorLocationType::Takeoff,
        classification_type: ClassificationType::Undeclared,
        operator_position: Some(LatLon { lat: centre.0, lon: centre.1 }),
        area_count: 1,
        area_radius: 0,
        area_ceiling: None,
        area_floor: None,
        category: Category::Undeclared,
        class: UaClass::Undeclared,
        operator_altitude: Some(GROUND_M),
        timestamp: None,
    });
    let operator = Message::OperatorId(OperatorId {
        operator_id_type: OperatorIdType::OperatorId,
        operator_id: FixedStr::new(d.operator_id)?,
    });
    Ok([
        encode::message(&basic)?,
        encode::message(&location)?,
        encode::message(&self_id)?,
        encode::message(&system)?,
        encode::message(&operator)?,
    ])
}

fn detection(d: &Drone, transport: Transport, counter: u8, msgs: &[RawMessage]) -> Frame {
    Frame::Detection(Detection {
        transport,
        channel: d.channel,
        rssi: d.rssi,
        mac: d.mac,
        counter,
        uptime_us: now_ms() * 1000,
        messages: msgs.iter().copied().collect(),
    })
}

fn inject(frame: Frame) {
    // Same queue as the UARTs; a full queue drops, as it would for a node.
    let _ = FRAMES.try_send((NODE, frame));
}

/// The demo FPV link's streams: (radio port, frames per second).
const FPV: [(u8, u32); 2] = [(0, 900), (144, 20)];

fn fpv_sighting(port: u8, packets: u32) -> Frame {
    let ch = ((openlink::WFB_DEFAULT_LINK_ID << 8) | u32::from(port)).to_be_bytes();
    Frame::OpenLink(OpenLinkSighting {
        link: openlink::OpenLink {
            system: openlink::System::WfbNg,
            id: Some(openlink::WFB_DEFAULT_LINK_ID),
            port,
            direction: if port < 128 {
                openlink::Direction::AirToGround
            } else {
                openlink::Direction::GroundToAir
            },
            tx: [0x57, 0x42, ch[0], ch[1], ch[2], ch[3]],
        },
        channel: 161,
        rssi: if port < 128 { -66 } else { -79 },
        uptime_us: now_ms() * 1000,
        packets,
    })
}

#[embassy_executor::task]
pub async fn task(centre: (f64, f64)) {
    let mut ticker = Ticker::every(Duration::from_secs(1));
    let mut counter: u8 = 0;
    let mut seconds: u32 = 0;
    loop {
        ticker.next().await;
        counter = counter.wrapping_add(1);
        seconds = seconds.wrapping_add(1);
        for (port, fps) in FPV {
            inject(fpv_sighting(port, seconds.saturating_mul(fps)));
        }
        let t_s = now_ms() as f64 / 1000.0;
        #[cfg(feature = "adsbee")]
        demo_aircraft(centre, t_s, seconds);
        for d in [&ALPHA, &BRAVO] {
            match messages(d, centre, t_s) {
                Ok(msgs) => {
                    inject(detection(d, d.transport, counter, &msgs));
                    // Bravo also "heard" as BT4 legacy: one message per advert,
                    // Basic ID then Location, as a real BT4 transmitter rotates.
                    if d.transport == Transport::BleLongRange && counter % 3 == 0 {
                        inject(detection(d, Transport::BleLegacy, counter, &msgs[..1]));
                        inject(detection(d, Transport::BleLegacy, counter, &msgs[1..2]));
                    }
                }
                Err(_) => crate::usb::msg("err", "demo encode"),
            }
        }
    }
}

/// The demo helicopter's report for this second (feature `adsbee`).
#[cfg(feature = "adsbee")]
fn demo_aircraft(centre: (f64, f64), t_s: f64, seconds: u32) {
    use adsb::flag;
    const R_M: f64 = 1_500.0;
    const PERIOD_S: f64 = 160.0;
    let th = 2.0 * PI * t_s / PERIOD_S;
    let lat = centre.0 + (R_M * libm::cos(th) / EARTH_R).to_degrees();
    let lon = centre.1 + (R_M * libm::sin(th) / (EARTH_R * libm::cos(centre.0.to_radians()))).to_degrees();
    let dir = ((th.to_degrees() + 90.0) % 360.0) as f32;
    let kt = (2.0 * PI * R_M / PERIOD_S / 0.514_444) as i32;
    let flags = [
        flag::AIRBORNE,
        flag::BARO_ALT_VALID,
        flag::POSITION_VALID,
        flag::DIRECTION_VALID,
        flag::SPEED_VALID,
        flag::UPDATED_POSITION,
    ]
    .iter()
    .fold(0u32, |f, b| f | 1 << b);
    let a = adsb::Aircraft {
        source: adsb::Source::ModeS,
        icao: 0xDE0001,
        flags,
        callsign: adsb::Callsign::new("DEMOHELI").unwrap_or_default(),
        squawk: Some(0o1200),
        category: 7,
        lat_deg: lat,
        lon_deg: lon,
        baro_alt_ft: 900,
        gnss_alt_ft: 0,
        direction_deg: dir,
        speed_kt: kt,
        baro_vrate_fpm: 0,
        gnss_vrate_fpm: 0,
        uat_emergency: 0,
        nicnac: 0,
        acdims: 0,
        version: 2,
        signal_dbm: -58,
        signal_quality: 6,
        frames_per_s: 2,
        es_frames_per_s: 4,
    };
    let stats = adsb::Stats {
        version: adsb::PROTOCOL_VERSION,
        demods_per_s: 40,
        raw_squitters_per_s: 3,
        squitters_per_s: 2,
        raw_extended_squitters_per_s: 5,
        extended_squitters_per_s: 4,
        raw_uat_per_s: 0,
        uat_per_s: 0,
        aircraft: 1,
        tscal: 0,
        uptime_s: seconds,
    };
    let mut line = [0u8; adsb::MAX_LINE];
    let mut split = adsb::LineSplitter::new();
    let mut queue = |r: Result<adsb::Report, adsb::Error>| match r {
        Ok(r) => {
            let _ = crate::adsbee::REPORTS.try_send(r);
        }
        Err(_) => crate::usb::msg("err", "demo csbee"),
    };
    if let Some(n) = adsb::encode::aircraft(&a, &mut line) {
        split.feed(&line[..n], &mut queue);
    }
    if let Some(n) = adsb::encode::stats(&stats, &mut line) {
        split.feed(&line[..n], &mut queue);
    }
}
