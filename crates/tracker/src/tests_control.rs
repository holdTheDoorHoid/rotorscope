//! Drone control links (link protocol 12, `ctrllinks`).

extern crate std;

use crate::ctrllinks::{MAX_CONTROL_LINKS, short_name};
use crate::json;
use crate::tests::CITY_HALL;
use crate::*;
use ctrlwatch::content::{
    AltRef, Channels, Opaque, Position, PositionKind, PositionSource, Telemetry,
};
use ctrlwatch::{
    Confidence, ControlContent, ControlSighting, Direction, LinkId, Probe, Region, System,
};
use link::{ControlEnergy, ControlReport, EnergyActivity, HopperReport};

fn sighting(system: System, id: Option<LinkId>, rssi: i16) -> ControlSighting {
    ControlSighting {
        system,
        region: Region::Fcc915,
        probe: Probe::Elrs {
            rate: ctrlwatch::elrs::ElrsRate::L900_200,
            version: ctrlwatch::elrs::OtaVersion::V4,
        },
        freq_hz: 915_500_000,
        rssi_dbm: rssi,
        snr_db: 7,
        t_us: 1,
        confidence: Confidence::Confirmed,
        direction: Direction::Uplink,
        id,
        version: Some(4),
    }
}

fn report(
    system: System,
    id: Option<LinkId>,
    part: u8,
    content: &[ControlContent],
) -> ControlReport {
    ControlReport {
        sighting: sighting(system, id, -70),
        best: Confidence::Confirmed,
        packets: 12,
        confirmed: 3,
        peak_rssi: -61,
        part,
        content: content.iter().cloned().collect(),
        raw: None,
    }
}

fn pos(kind: PositionKind, north_m: f64, alt_m: i32) -> ControlContent {
    ControlContent::Position(Position {
        kind,
        lat_e7: ((CITY_HALL.0 + north_m / 111_000.0) * 1e7) as i32,
        lon_e7: (CITY_HALL.1 * 1e7) as i32,
        alt_mm: Some((alt_m * 1000, AltRef::Msl)),
        rel_alt_mm: Some(alt_m * 1000 - 12_000),
        ground_speed_cms: Some(850),
        heading_cdeg: Some(9_000),
        satellites: Some(14),
        fix_type: Some(3),
        source: PositionSource::CrsfGps,
    })
}

#[test]
fn summary_makes_one_entry_per_system_and_id() {
    let mut tr: Tracker = Tracker::default();
    let a = tr.ingest_control(2, &report(System::ExpressLrs, None, 0, &[]), 1_000);
    let b = tr.ingest_control(2, &report(System::ExpressLrs, None, 0, &[]), 2_000);
    assert_eq!(a, b, "no ID: one entry per system and region");
    let c = tr.ingest_control(
        2,
        &report(System::ExpressLrs, Some(LinkId::SikNetId(25)), 0, &[]),
        2_000,
    );
    assert_ne!(a, c, "with the ID opt-in, links are told apart");
    tr.ingest_control(3, &report(System::Sik, None, 0, &[]), 2_000);
    assert_eq!(tr.control_links().len(), 3);
    let l = &tr.control_links()[0];
    assert_eq!(
        (l.packets, l.confirmed, l.peak_rssi, l.node, l.best),
        (12, 3, -61, 2, Confidence::Confirmed)
    );
    assert!(l.heard_up && !l.heard_down);
    assert_eq!(tr.control_counts().0, 4);
    assert_eq!(tr.active_control_count(2_000), 3);
    // No positions: no drone track.
    assert!(tr.is_empty());
    // A full table drops the one heard least recently.
    for i in 0..10u16 {
        tr.ingest_control(
            1,
            &report(System::Mlrs, Some(LinkId::MlrsSyncWord(i)), 0, &[]),
            3_000 + u64::from(i),
        );
    }
    assert_eq!(tr.control_links().len(), MAX_CONTROL_LINKS);
    // Expiry with everything else.
    tr.expire(3_100 + 600_000);
    assert!(tr.control_links().is_empty());
}

#[test]
fn content_fills_sticks_telemetry_and_a_drone_track() {
    let mut tr: Tracker = Tracker::default();
    let mut ch = Channels::default();
    for (i, v) in [1500u16, 1520, 988, 1490, 2000].into_iter().enumerate() {
        ch.set(i, v);
    }
    ch.armed = Some(true);
    let content = [
        ControlContent::Channels(ch),
        ControlContent::Telemetry(Telemetry::Battery {
            voltage_mv: 15_800,
            current_ma: None,
            used_mah: None,
            remaining_pct: Some(71),
        }),
    ];
    tr.ingest_control(1, &report(System::ExpressLrs, None, 1, &content), 1_000);
    let more = [
        ControlContent::Telemetry(Telemetry::Link {
            uplink_rssi_dbm: -80,
            uplink_lq: 98,
            uplink_snr_db: 5,
            downlink_rssi_dbm: None,
            downlink_lq: None,
        }),
        ControlContent::Telemetry(Telemetry::FlightMode(
            heapless::String::try_from("ACRO\u{1}").unwrap(),
        )),
    ];
    tr.ingest_control(1, &report(System::ExpressLrs, None, 2, &more), 1_000);
    let l = &tr.control_links()[0];
    let s = l.sticks.unwrap();
    assert_eq!((s.us, s.armed), ([1500, 1520, 988, 1490], Some(true)));
    let t = l.telemetry.as_ref().unwrap();
    assert_eq!(
        (t.battery_mv, t.battery_pct, t.lq, t.mode.as_str()),
        (Some(15_800), Some(71), Some(98), "ACRO")
    );
    assert!(tr.is_empty(), "no position yet");

    // A position makes a drone track, sourced from the control link.
    tr.ingest_control(
        1,
        &report(
            System::ExpressLrs,
            None,
            1,
            &[pos(PositionKind::Aircraft, 0.0, 130)],
        ),
        2_000,
    );
    assert_eq!(tr.len(), 1);
    let id = tr.control_links()[0].track.unwrap();
    let t = tr.get(id).unwrap();
    assert_eq!(
        (t.location_src, t.control),
        (Source::ControlLink, Some(System::ExpressLrs))
    );
    let loc = &t.location.as_ref().unwrap().value;
    assert!((loc.position.unwrap().lat - CITY_HALL.0).abs() < 1e-6);
    assert_eq!(
        (
            loc.altitude_geodetic,
            loc.height,
            loc.speed_horizontal,
            loc.direction
        ),
        (Some(130.0), Some(118.0), Some(8.5), Some(90.0))
    );
    assert_eq!(t.nodes, 0b10);
    // The next position updates the same track; a home point goes to home.
    tr.ingest_control(
        1,
        &report(
            System::ExpressLrs,
            None,
            1,
            &[
                pos(PositionKind::Aircraft, 30.0, 140),
                pos(PositionKind::Home, -5.0, 0),
            ],
        ),
        3_000,
    );
    assert_eq!(tr.len(), 1);
    let t = tr.get(id).unwrap();
    assert!(t.home.is_some());
    assert_eq!(t.location.as_ref().unwrap().rx_ms, 3_000);
    // A 0,0 "no fix" position is ignored.
    let mut bad = pos(PositionKind::Aircraft, 0.0, 10);
    if let ControlContent::Position(p) = &mut bad {
        p.lat_e7 = 0;
        p.lon_e7 = 0;
    }
    tr.ingest_control(1, &report(System::ExpressLrs, None, 1, &[bad]), 4_000);
    assert_eq!(tr.get(id).unwrap().location.as_ref().unwrap().rx_ms, 3_000);
    assert_eq!(tr.control_counts().3, 2);
    // It is announced like any drone without an ID.
    assert_eq!(tr.take_new_drone(4_000), Some(id));
    // Encrypted content is flagged.
    tr.ingest_control(
        4,
        &report(
            System::Sik,
            None,
            1,
            &[ControlContent::Opaque(Opaque::Encrypted)],
        ),
        4_000,
    );
    assert!(tr.control_links()[1].encrypted);
    assert_eq!(short_name(System::Sik), "SiK");
}

/// A drone known only from its control link's telemetry, beside a
/// helicopter: the drone-near-aircraft warning fires.
#[test]
fn control_link_drone_near_aircraft_warns() {
    let mut tr: Tracker = Tracker::default();
    tr.ingest_control(
        1,
        &report(
            System::Mlrs,
            None,
            1,
            &[pos(PositionKind::Aircraft, 0.0, 120)],
        ),
        0,
    );
    tr.ingest_aircraft(
        &crate::tests_conflict::heli(0xA1B2C3, 50.0, 400, 500, 7, 270.0, 60),
        0,
    );
    let c = tr.take_new_conflict(100).expect("conflict");
    assert_eq!(c.level, crate::conflict::Level::Warning);
    let drone = tr.get(c.drone).unwrap();
    assert_eq!(drone.location_src, Source::ControlLink);
}

#[test]
fn energy_bands_keep_the_last_hopper() {
    let mut tr: Tracker = Tracker::default();
    let hop = HopperReport {
        low_khz: 903_500,
        high_khz: 926_900,
        channels: 24,
        spacing_khz: Some(600),
        occupied_khz: 400,
        rssi: -71,
        spread_db: 3,
        burst_us: Some(1_100),
        interval_us: Some(4_000),
        periodic_pct: 96,
        candidates: [(System::ExpressLrs, 82), (System::Crossfire, 55)]
            .into_iter()
            .collect(),
    };
    let e = |activity, windows| ControlEnergy {
        region: Region::Fcc915,
        windows,
        activity,
        steady: [(915_000, -50)].into_iter().collect(),
        uptime_us: 9,
    };
    tr.ingest_frame(
        2,
        &link::Frame::ControlEnergy(e(EnergyActivity::Hopper(hop.clone()), 30)),
        1_000,
    );
    tr.ingest_frame(
        2,
        &link::Frame::ControlEnergy(e(EnergyActivity::Background, 31)),
        5_000,
    );
    assert_eq!(tr.control_bands().len(), 1);
    let b = &tr.control_bands()[0];
    assert_eq!(
        (b.windows, b.steady, b.activity.clone()),
        (31, 1, EnergyActivity::Background)
    );
    assert_eq!(b.hopper.as_ref().unwrap().0, hop);
    let ctx = tr.context(5_000, None);
    let mut buf = [0u8; json::MAX_CONTROL_LINE];
    let n = json::control_energy_line(b, &ctx, &mut buf).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&buf[..n]).unwrap();
    assert_eq!(v["t"], "control_energy");
    assert_eq!(v["activity"], "background");
    assert_eq!(v["band"], "915");
    assert_eq!(v["hopper"]["cands"][0]["label"], "ExpressLRS");
    assert_eq!(v["hopper"]["cands"][0]["score"], 82);
    assert_eq!(v["hopper"]["state"], "active");
    assert_eq!(v["hopper"]["age_ms"], 4_000);
}

/// The JSON line: every field, IDs only when the node sent one, and the
/// worst case fits its buffer.
#[test]
fn control_line_json() {
    let mut tr: Tracker = Tracker::default();
    let mut ch = Channels::default();
    ch.set(0, u16::MAX);
    ch.armed = Some(false);
    let worst = [
        ControlContent::Channels(ch),
        ControlContent::Telemetry(Telemetry::FlightMode(
            heapless::String::try_from("WWWWWWWWWWWWWWWW").unwrap(),
        )),
    ];
    let id = LinkId::ElrsUid {
        uid3: Some(0xAB),
        uid4: 0xCD,
        uid5: 0xEF,
        uid5_real: 0xEE,
    };
    let mut r = report(System::ExpressLrs, Some(id), 1, &worst);
    r.sighting.rssi_dbm = i16::MIN;
    r.packets = u32::MAX;
    tr.ingest_control(7, &r, u64::MAX / 4);
    tr.ingest_control(
        7,
        &report(
            System::ExpressLrs,
            Some(id),
            2,
            &[
                ControlContent::Telemetry(Telemetry::Battery {
                    voltage_mv: u32::MAX,
                    current_ma: None,
                    used_mah: None,
                    remaining_pct: Some(u8::MAX),
                }),
                ControlContent::Telemetry(Telemetry::BaroAltitude {
                    alt_dm: i32::MIN,
                    vario_cms: None,
                }),
            ],
        ),
        u64::MAX / 4,
    );
    tr.ingest_control(
        7,
        &report(
            System::ExpressLrs,
            Some(id),
            1,
            &[pos(PositionKind::Aircraft, 0.0, 100)],
        ),
        u64::MAX / 4,
    );
    tr.ingest_control(7, &report(System::Sik, None, 0, &[]), u64::MAX / 4);
    let ctx = tr.context(u64::MAX / 4 + 10, None);
    let mut buf = [0u8; json::MAX_CONTROL_LINE];
    let n = json::control_line(&tr.control_links()[0], &ctx, &mut buf).unwrap();
    std::println!(
        "control line {n} bytes: {}",
        core::str::from_utf8(&buf[..n]).unwrap()
    );
    let v: serde_json::Value = serde_json::from_slice(&buf[..n]).unwrap();
    assert_eq!(v["t"], "control");
    assert_eq!(v["sys"], "ExpressLrs");
    assert_eq!(v["label"], "ExpressLRS");
    assert_eq!(v["band"], "915");
    assert_eq!(v["mhz"], 915.5);
    assert_eq!(v["rate"], "200Hz v4");
    assert_eq!(v["conf"], "confirmed");
    assert_eq!(v["link_id"], "uid ..abcdee");
    assert_eq!(v["armed"], false);
    assert_eq!(v["mode"], "WWWWWWWWWWWW");
    assert!(v["track"].is_number());
    let n = json::control_line(&tr.control_links()[1], &ctx, &mut buf).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&buf[..n]).unwrap();
    assert!(
        v["link_id"].is_null()
            && v["sticks"].is_null()
            && v["batt_mv"].is_null()
            && v["track"].is_null()
    );
}
