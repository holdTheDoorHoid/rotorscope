//! `--demo`: two synthetic drones circling a point, so the viewer (and the
//! tracker's merging) can be shown without hardware or a transmitter.
//!
//! The messages are built with `rid::encode` and fed to the tracker as
//! hub-format `link::Detection`s with raw bytes, i.e. the exact path the hub
//! runs. The demo exercises the interesting cases on purpose:
//! - ALPHA is a multirotor heard on Wi-Fi beacon (packs, one MAC) and on BLE 4
//!   legacy (one message per advert, another MAC). Its first BLE adverts carry
//!   a Location before any Basic ID, so they start a MAC-keyed track that
//!   merges into ALPHA when the Basic ID advert arrives half a second later.
//! - BRAVO is a fixed-wing on BLE Long Range whose address rotates every 30 s
//!   (BLE privacy); the UAS ID keeps it one track.

use std::f64::consts::PI;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use rid::Transport;
use rid::message::*;

use crate::input::Event;

/// Simulation step: the BLE 4 advert interval.
const STEP_MS: u64 = 250;

struct Orbit {
    radius_m: f64,
    period_s: f64,
    /// +1 clockwise (seen from above), -1 counter-clockwise.
    sense: f64,
    alt_m: f32,
}

impl Orbit {
    /// Position, track over ground (deg) and speed (m/s) at `t` seconds.
    fn at(&self, center: (f64, f64), t: f64) -> (LatLon, f32, f32) {
        let theta = self.sense * 2.0 * PI * t / self.period_s;
        let (north, east) = (self.radius_m * theta.cos(), self.radius_m * theta.sin());
        // Small-offset flat-earth conversion: fine for a few km.
        let lat = center.0 + north / 111_320.0;
        let lon = center.1 + east / (111_320.0 * center.0.to_radians().cos());
        let heading = (theta.to_degrees() + 90.0 * self.sense).rem_euclid(360.0);
        let speed = 2.0 * PI * self.radius_m / self.period_s;
        // Round to what F3411 can carry, so encoding never rejects it.
        (
            LatLon { lat, lon },
            heading.round() as f32 % 360.0,
            (speed * 4.0).round() as f32 / 4.0,
        )
    }
}

struct Drone {
    serial: &'static str,
    ua_type: UaType,
    self_id: &'static str,
    operator_id: &'static str,
    orbit: Orbit,
    /// Operator offset from the centre, metres north/east.
    operator_ne: (f64, f64),
}

const ALPHA: Drone = Drone {
    serial: "DEMO-ALPHA-0001",
    ua_type: UaType::HelicopterOrMultirotor,
    self_id: "rid-view demo: survey",
    operator_id: "DEMO-OP-ALPHA",
    orbit: Orbit {
        radius_m: 300.0,
        period_s: 60.0,
        sense: 1.0,
        alt_m: 120.0,
    },
    operator_ne: (-40.0, 25.0),
};

const BRAVO: Drone = Drone {
    serial: "DEMO-BRAVO-0002",
    ua_type: UaType::Aeroplane,
    self_id: "rid-view demo: mapping",
    operator_id: "DEMO-OP-BRAVO",
    orbit: Orbit {
        radius_m: 800.0,
        period_s: 120.0,
        sense: -1.0,
        alt_m: 150.0,
    },
    operator_ne: (600.0, -900.0),
};

const ALPHA_WIFI: [u8; 6] = [0x02, 0xde, 0x00, 0x00, 0xa1, 0x01];
const ALPHA_BLE: [u8; 6] = [0xc2, 0xde, 0x00, 0x00, 0xa1, 0x02];

impl Drone {
    fn messages(&self, center: (f64, f64), t_ms: u64) -> [rid::RawMessage; 5] {
        let t = t_ms as f64 / 1000.0;
        let (pos, dir, speed) = self.orbit.at(center, t);
        let op = LatLon {
            lat: center.0 + self.operator_ne.0 / 111_320.0,
            lon: center.1 + self.operator_ne.1 / (111_320.0 * center.0.to_radians().cos()),
        };
        let enc = |m: Message| rid::encode::message(&m).expect("demo values are in range");
        [
            enc(Message::BasicId(BasicId {
                id_type: IdType::SerialNumber,
                ua_type: self.ua_type,
                uas_id: FixedStr::new(self.serial.as_bytes()).unwrap(),
            })),
            enc(Message::Location(Location {
                status: Status::Airborne,
                height_type: HeightType::AboveTakeoff,
                direction: Some(dir),
                speed_horizontal: Some(speed),
                speed_vertical: Some(0.0),
                position: Some(pos),
                altitude_pressure: Some(self.orbit.alt_m - 4.0),
                altitude_geodetic: Some(self.orbit.alt_m),
                height: Some(self.orbit.alt_m - 20.0),
                horizontal_accuracy: HorizontalAccuracy::M3,
                vertical_accuracy: VerticalAccuracy::M10,
                baro_accuracy: VerticalAccuracy::M10,
                speed_accuracy: SpeedAccuracy::Mps1,
                timestamp: Some(((t_ms / 100) % 36_000) as f32 / 10.0),
                timestamp_accuracy: Some(0.2),
            })),
            enc(Message::System(System {
                operator_location_type: OperatorLocationType::Takeoff,
                classification_type: ClassificationType::Undeclared,
                operator_position: Some(op),
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
                description: FixedStr::new(self.self_id.as_bytes()).unwrap(),
            })),
            enc(Message::OperatorId(OperatorId {
                operator_id_type: OperatorIdType::OperatorId,
                operator_id: FixedStr::new(self.operator_id.as_bytes()).unwrap(),
            })),
        ]
    }
}

fn detection(
    transport: Transport,
    mac: [u8; 6],
    rssi: f64,
    counter: u8,
    t_ms: u64,
    msgs: &[rid::RawMessage],
) -> link::Detection {
    link::Detection {
        transport,
        channel: if matches!(transport, Transport::WifiBeacon) {
            6
        } else {
            37 + (counter % 3)
        },
        rssi: rssi.round().clamp(-127.0, 0.0) as i8,
        mac,
        counter,
        uptime_us: t_ms * 1000,
        messages: heapless::Vec::from_slice(msgs).expect("at most 9"),
    }
}

/// The detections due at simulated time `t_ms` (a multiple of [`STEP_MS`]).
fn step(center: (f64, f64), t_ms: u64) -> Vec<(u8, link::Detection)> {
    let n = t_ms / STEP_MS;
    let ctr = n as u8;
    // A gentle RSSI wobble so the column visibly updates.
    let wobble = 3.0 * (t_ms as f64 / 1700.0).sin();
    let mut out = Vec::new();

    let a = ALPHA.messages(center, t_ms);
    if n.is_multiple_of(4) {
        // Wi-Fi beacon: the whole set as one Message Pack, once a second.
        out.push((
            0,
            detection(
                Transport::WifiBeacon,
                ALPHA_WIFI,
                -58.0 + wobble,
                ctr,
                t_ms,
                &a,
            ),
        ));
    }
    // BLE 4: one message per advert. Location first, Basic ID third, so the
    // MAC-keyed track exists briefly before it merges into ALPHA.
    const BLE_ORDER: [usize; 5] = [1, 2, 0, 3, 4];
    let i = BLE_ORDER[(n % 5) as usize];
    out.push((
        1,
        detection(
            Transport::BleLegacy,
            ALPHA_BLE,
            -72.0 - wobble,
            ctr,
            t_ms,
            &a[i..=i],
        ),
    ));

    if n % 4 == 2 {
        let b = BRAVO.messages(center, t_ms);
        // Resolvable private address that changes every 30 s.
        let epoch = (t_ms / 30_000) as u8;
        let mac = [0x5b, 0xd0, 0x00, 0x00, 0xb2, epoch];
        out.push((
            1,
            detection(
                Transport::BleLongRange,
                mac,
                -88.0 + wobble,
                ctr,
                t_ms,
                &b[..4],
            ),
        ));
    }
    out
}

/// Run the demo on a thread. `live`: in real time, forever. Otherwise run
/// `sim_secs` of simulated time as fast as possible, then end.
/// The demo's nodes are `node_base` (Wi-Fi) and `node_base + 1` (BLE).
pub fn spawn(
    center: (f64, f64),
    live: bool,
    sim_secs: u64,
    start: Instant,
    node_base: u8,
    tx: Sender<Event>,
) {
    std::thread::spawn(move || {
        let mut t_ms = 0;
        loop {
            if !live && t_ms > sim_secs * 1000 {
                let _ = tx.send(Event::End {
                    node: node_base,
                    why: "demo finished".into(),
                });
                return;
            }
            let at_ms = if live {
                let due = start + Duration::from_millis(t_ms);
                if let Some(wait) = due.checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                }
                start.elapsed().as_millis() as u64
            } else {
                t_ms
            };
            for (node, det) in step(center, t_ms) {
                if tx
                    .send(Event::Detection {
                        node: node_base + node,
                        det: Box::new(det),
                        at_ms,
                    })
                    .is_err()
                {
                    return;
                }
            }
            t_ms += STEP_MS;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The demo must drive the tracker into exactly two drones, with
    /// ALPHA's BLE MAC track merged and BRAVO's rotating MAC unified.
    #[test]
    fn demo_yields_two_merged_drones() {
        let center = (39.952_583, -75.165_222);
        let mut tr: tracker::Tracker = tracker::Tracker::default();
        let mut alerts = 0;
        let mut t = 0;
        while t <= 70_000 {
            for (node, det) in step(center, t) {
                tr.ingest(node, &det, t);
            }
            while tr.take_new_drone(t).is_some() {
                alerts += 1;
            }
            t += STEP_MS;
        }
        assert_eq!(tr.len(), 2, "{:#?}", tr.tracks());
        assert_eq!(alerts, 2);
        let alpha = tr
            .tracks()
            .iter()
            .find(|t| t.primary_id().unwrap().id.as_str() == Some(ALPHA.serial))
            .unwrap();
        let heard: Vec<_> = alpha.transports().collect();
        assert_eq!(heard, [Transport::WifiBeacon, Transport::BleLegacy]);
        let bravo = tr
            .tracks()
            .iter()
            .find(|t| t.primary_id().unwrap().id.as_str() == Some(BRAVO.serial))
            .unwrap();
        assert_eq!(
            bravo.link(Transport::BleLongRange).mac[5],
            2,
            "third MAC epoch at 70 s"
        );
        assert_eq!(bravo.ua_type, Some(UaType::Aeroplane));
    }

    #[test]
    fn orbit_radius_is_right() {
        let center = (39.952_583, -75.165_222);
        for t in [0.0, 7.5, 15.0, 33.0] {
            let (p, _, speed) = ALPHA.orbit.at(center, t);
            let d = tracker::geo::range_bearing(center.0, center.1, p.lat, p.lon).distance_m;
            assert!((d - 300.0).abs() < 2.0, "t={t} d={d}");
            assert!((speed - 31.5).abs() < 0.2);
        }
    }
}
