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
//! - A wfb-ng (OpenIPC) FPV link on 5 GHz channel 161, as the 5 GHz node
//!   would report it: video and MAVLink down, MAVLink up, one sighting per
//!   stream per second with running packet counts. It has no position and
//!   must never become a drone track.
//! - Three crewed aircraft, as an ADSBee would report them once a second:
//!   CSBee text built with `adsb::encode` and parsed back with the same
//!   `adsb::LineSplitter` the `--adsbee` input uses. A medevac helicopter
//!   circling low 1.5 km out (the "low aircraft nearby" case), an airliner
//!   on a wide circle at 11,000 ft, and a light aircraft heard only by its
//!   transponder replies (altitude, no position). None of them may become
//!   a drone or raise an alert. Their callsigns (DEMOHELI, DEMOJET) and
//!   addresses (DE0001-DE0003) are obviously synthetic.

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

/// The demo FPV link: (port, frames per second, RSSI).
const FPV_STREAMS: [(u8, u64, f64); 3] = [(0, 900, -64.0), (16, 50, -64.0), (144, 20, -77.0)];
/// Its channel: OpenIPC's usual one.
const FPV_CHANNEL: u8 = 161;

/// The open-link sightings due at simulated time `t_ms`: one per stream each
/// second, as `node-wifi` rate-limits them.
fn link_step(t_ms: u64) -> Vec<link::OpenLinkSighting> {
    if !(t_ms / STEP_MS).is_multiple_of(4) {
        return Vec::new();
    }
    let wobble = 2.0 * (t_ms as f64 / 2300.0).sin();
    FPV_STREAMS
        .iter()
        .map(|&(port, fps, rssi)| {
            let ch = ((openlink::WFB_DEFAULT_LINK_ID << 8) | u32::from(port)).to_be_bytes();
            link::OpenLinkSighting {
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
                channel: FPV_CHANNEL,
                rssi: (rssi + wobble).round() as i8,
                uptime_us: t_ms * 1000,
                packets: ((t_ms + 1000) * fps / 1000) as u32,
            }
        })
        .collect()
}

/// A simulated aircraft for the ADS-B part of the demo.
struct Plane {
    icao: u32,
    call: &'static str,
    category: i8,
    squawk: u16,
    /// `None`: no position (Mode S replies only).
    orbit: Option<Orbit>,
    alt_ft: i32,
    rssi: i16,
}

const PLANES: [Plane; 3] = [
    Plane {
        icao: 0xDE0001,
        call: "DEMOHELI",
        category: 7,
        squawk: 0o1200,
        orbit: Some(Orbit {
            radius_m: 1500.0,
            period_s: 160.0,
            sense: 1.0,
            alt_m: 0.0,
        }),
        alt_ft: 900,
        rssi: -58,
    },
    Plane {
        icao: 0xDE0002,
        call: "DEMOJET",
        category: 3,
        squawk: 0o4721,
        orbit: Some(Orbit {
            radius_m: 12_000.0,
            period_s: 520.0,
            sense: -1.0,
            alt_m: 0.0,
        }),
        alt_ft: 11_000,
        rssi: -81,
    },
    Plane {
        icao: 0xDE0003,
        call: "",
        category: 1,
        squawk: 0o1200,
        orbit: None,
        alt_ft: 4_500,
        rssi: -88,
    },
];

/// The CSBee text an ADSBee would send at simulated time `t_ms`: once a
/// second, one `#A:` line per aircraft and a `#S:` line.
pub fn adsb_text(center: (f64, f64), t_ms: u64) -> Vec<u8> {
    use adsb::flag;
    if !t_ms.is_multiple_of(1000) {
        return Vec::new();
    }
    let t = t_ms as f64 / 1000.0;
    let mut out = Vec::new();
    let mut buf = [0u8; adsb::MAX_LINE];
    for p in &PLANES {
        let mut flags =
            1 << flag::AIRBORNE | 1 << flag::BARO_ALT_VALID | 1 << flag::UPDATED_BARO_ALT;
        let (mut lat, mut lon, mut dir, mut kt) = (0.0, 0.0, 0.0, 0);
        if let Some(o) = &p.orbit {
            let (pos, d, mps) = o.at(center, t);
            (lat, lon, dir) = (pos.lat, pos.lon, d);
            kt = (f64::from(mps) / 0.514_444).round() as i32;
            flags |= 1 << flag::POSITION_VALID
                | 1 << flag::DIRECTION_VALID
                | 1 << flag::SPEED_VALID
                | 1 << flag::UPDATED_POSITION
                | 1 << flag::UPDATED_DIRECTION
                | 1 << flag::UPDATED_SPEED;
        }
        let a = adsb::Aircraft {
            source: adsb::Source::ModeS,
            icao: p.icao,
            flags,
            callsign: adsb::Callsign::new(p.call).unwrap_or_default(),
            squawk: Some(p.squawk),
            category: p.category,
            lat_deg: lat,
            lon_deg: lon,
            baro_alt_ft: p.alt_ft,
            gnss_alt_ft: 0,
            direction_deg: dir,
            speed_kt: kt,
            baro_vrate_fpm: 0,
            gnss_vrate_fpm: 0,
            uat_emergency: 0,
            nicnac: 0,
            acdims: 0,
            version: 2,
            signal_dbm: p.rssi,
            signal_quality: 6,
            frames_per_s: 2,
            es_frames_per_s: if p.orbit.is_some() { 4 } else { 0 },
        };
        if let Some(n) = adsb::encode::aircraft(&a, &mut buf) {
            out.extend_from_slice(&buf[..n]);
        }
    }
    let s = adsb::Stats {
        version: adsb::PROTOCOL_VERSION,
        demods_per_s: 140,
        raw_squitters_per_s: 12,
        squitters_per_s: 6,
        raw_extended_squitters_per_s: 11,
        extended_squitters_per_s: 8,
        raw_uat_per_s: 0,
        uat_per_s: 0,
        aircraft: PLANES.len() as u16,
        tscal: 0,
        uptime_s: (t_ms / 1000) as u32,
    };
    if let Some(n) = adsb::encode::stats(&s, &mut buf) {
        out.extend_from_slice(&buf[..n]);
    }
    out
}

/// Run the demo on a thread. `live`: in real time, forever. Otherwise run
/// `sim_secs` of simulated time as fast as possible, then end.
/// The demo's nodes are `node_base` (Wi-Fi), `node_base + 1` (BLE) and
/// `node_base + 2` (5 GHz Wi-Fi, the FPV link).
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
        let mut adsb_split = adsb::LineSplitter::new();
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
            let mut gone = false;
            adsb_split.feed(&adsb_text(center, t_ms), |result| {
                gone |= tx.send(Event::Adsb { result, at_ms }).is_err();
            });
            if gone {
                return;
            }
            for sighting in link_step(t_ms) {
                if tx
                    .send(Event::OpenLink {
                        node: node_base + 2,
                        sighting,
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
            for s in link_step(t) {
                tr.ingest_open_link(2, &s, t);
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
        // The FPV link: one entry, three streams, no drone and no alert.
        assert_eq!(tr.open_links().len(), 1);
        let fpv = &tr.open_links()[0];
        assert_eq!(fpv.streams.len(), 3);
        assert_eq!(fpv.packets(), (71_000 * 970 / 1000) as u32);
    }

    /// The demo aircraft go through the real CSBee parser and into their
    /// own table: never a drone, never an alert.
    #[test]
    fn demo_aircraft_are_aircraft() {
        let center = (39.952_583, -75.165_222);
        let mut tr: tracker::Tracker = tracker::Tracker::default();
        tr.set_own_position(center.0, center.1, None, 0);
        let mut split = adsb::LineSplitter::new();
        let mut errors = 0;
        let mut t = 0;
        while t <= 5_000 {
            split.feed(&adsb_text(center, t), |r| match r {
                Ok(adsb::Report::Aircraft(a)) => {
                    tr.ingest_aircraft(&a, t);
                }
                Ok(adsb::Report::Stats(_)) => {}
                Err(_) => errors += 1,
            });
            t += STEP_MS;
        }
        assert_eq!(errors, 0);
        assert!(tr.tracks().is_empty());
        assert_eq!(tr.take_new_drone(t), None);
        assert_eq!(tr.aircraft().len(), 3);
        let ctx = tr.context(t, None);
        let heli = &tr.aircraft()[0];
        assert_eq!(heli.callsign.as_str(), "DEMOHELI");
        let g = ctx.aircraft_geometry(heli);
        assert!((g.range.unwrap().distance_m - 1500.0).abs() < 10.0);
        assert!(g.low_nearby);
        assert!(!ctx.aircraft_geometry(&tr.aircraft()[1]).low_nearby);
        assert!(tr.aircraft()[2].position.is_none());
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
