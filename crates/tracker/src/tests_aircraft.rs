//! ADS-B aircraft: their own table and expiry, never a drone or an alert,
//! range/bearing from the unit, and every output.

extern crate std;
use std::string::String;

use crate::aircraft::{
    AIRCRAFT_REMOVE_MS, AIRCRAFT_STALE_MS, AircraftTable, LOW_ABOVE_M, LOW_RANGE_M, same_drone,
};
use crate::tests::{CITY_HALL, WIFI_MAC, det, location, serial};
use crate::*;
use adsb::{Aircraft, Callsign, Source, flag};
use rid::Transport;

const UTC: u64 = 1_794_232_987_250;

/// Flags for "airborne, everything valid", plus "position updated".
const ALL_VALID: u32 = 0xff;
const UPDATED_POS: u32 = 1 << flag::UPDATED_POSITION;

/// An aircraft report `north_m` metres north of City Hall.
fn report(icao: u32, north_m: f64, baro_ft: i32, category: i8) -> Aircraft {
    Aircraft {
        source: Source::ModeS,
        icao,
        flags: ALL_VALID | UPDATED_POS,
        callsign: Callsign::new("N911PH").unwrap(),
        squawk: Some(0o1200),
        category,
        lat_deg: CITY_HALL.0 + north_m / 111_000.0,
        lon_deg: CITY_HALL.1,
        baro_alt_ft: baro_ft,
        gnss_alt_ft: baro_ft + 100,
        direction_deg: 270.0,
        speed_kt: 95,
        baro_vrate_fpm: -500,
        gnss_vrate_fpm: -512,
        uat_emergency: 0,
        nicnac: 0,
        acdims: 0,
        version: 2,
        signal_dbm: -62,
        signal_quality: 9,
        frames_per_s: 3,
        es_frames_per_s: 4,
    }
}

/// The same aircraft, listed by the ADSBee but not heard this second.
fn quiet(mut a: Aircraft) -> Aircraft {
    a.flags &= 0xff;
    a.frames_per_s = 0;
    a.es_frames_per_s = 0;
    a
}

fn s(buf: &[u8], n: usize) -> String {
    String::from_utf8(buf[..n].to_vec()).unwrap()
}

fn with_own(t: &mut Tracker, at: u64) {
    t.set_own_position(CITY_HALL.0, CITY_HALL.1, Some(40.0), at);
}

/// The hub keeps these in static RAM, which it has little of without PSRAM.
#[test]
fn sizes() {
    use core::mem::size_of;
    std::println!(
        "AircraftTrack {} B, adsb::Report {} B, table of 32 {} B",
        size_of::<AircraftTrack>(),
        size_of::<adsb::Report>(),
        size_of::<AircraftTable<32>>()
    );
    assert!(size_of::<AircraftTrack>() <= 128);
}

#[test]
fn aircraft_never_become_drones_or_alerts() {
    let mut tr: Tracker = Tracker::default();
    with_own(&mut tr, 0);
    let key = tr.ingest_aircraft(&report(0xA1B2C3, 1000.0, 800, 7), 100);
    assert!(key.is_some());
    assert_eq!(tr.aircraft().len(), 1);
    assert!(tr.tracks().is_empty());
    assert_eq!(tr.take_new_drone(10_000), None);
    assert_eq!(tr.active_count(100), 0);
    assert_eq!(tr.active_aircraft_count(100), 1);
}

#[test]
fn quiet_reports_neither_create_nor_refresh() {
    let mut tr: Tracker = Tracker::default();
    let r = report(0xA1B2C3, 1000.0, 800, 7);
    // The ADSBee still lists an aircraft it stopped hearing: no new entry.
    assert_eq!(tr.ingest_aircraft(&quiet(r), 0), None);
    tr.ingest_aircraft(&r, 1_000).unwrap();
    tr.ingest_aircraft(&quiet(r), 9_000).unwrap();
    let a = &tr.aircraft()[0];
    assert_eq!(a.last_seen_ms, 1_000);
    assert_eq!(a.reports, 2);
    // The position was not updated in that report, so its age is real.
    assert_eq!(a.position.as_ref().unwrap().rx_ms, 1_000);
    let ctx = tr.context(1_000 + AIRCRAFT_STALE_MS, None);
    assert_eq!(ctx.aircraft_state(a), TrackState::Stale);
    let ctx = tr.context(1_000 + AIRCRAFT_STALE_MS - 1, None);
    assert_eq!(ctx.aircraft_state(a), TrackState::Active);
}

/// ADSBee 0.9.0 can keep the UPDATED_* bits set on lines that only repeat
/// old values (docs/ADSBEE.md). With no messages counted, such a line must
/// not refresh the aircraft or re-stamp its position.
#[test]
fn sticky_updated_bits_neither_create_nor_refresh() {
    let mut tr: Tracker = Tracker::default();
    let r = report(0xA1B2C3, 1000.0, 800, 7);
    let mut sticky = r;
    sticky.flags = ALL_VALID | 0x7f << 23;
    sticky.frames_per_s = 0;
    sticky.es_frames_per_s = 0;
    assert_eq!(tr.ingest_aircraft(&sticky, 0), None);
    tr.ingest_aircraft(&r, 1_000).unwrap();
    tr.ingest_aircraft(&sticky, 9_000).unwrap();
    let a = &tr.aircraft()[0];
    assert_eq!(a.last_seen_ms, 1_000);
    assert_eq!(a.position.as_ref().unwrap().rx_ms, 1_000);
    let ctx = tr.context(1_000 + AIRCRAFT_STALE_MS, None);
    assert_eq!(ctx.aircraft_state(a), TrackState::Stale);
}

#[test]
fn aircraft_expire_on_their_own_clock() {
    let mut tr: Tracker = Tracker::default();
    tr.ingest_aircraft(&report(1, 1000.0, 800, 7), 0);
    tr.expire(AIRCRAFT_REMOVE_MS - 1);
    assert_eq!(tr.aircraft().len(), 1);
    tr.expire(AIRCRAFT_REMOVE_MS);
    assert!(tr.aircraft().is_empty());
}

#[test]
fn fields_keep_last_known_values() {
    let mut tr: Tracker = Tracker::default();
    let mut r = report(0xABCDEF, 0.0, 1200, 7);
    tr.ingest_aircraft(&r, 0);
    // Next report lacks callsign, squawk and altitude: keep what we had.
    r.callsign = Callsign::default();
    r.squawk = None;
    r.flags &= !(1 << flag::BARO_ALT_VALID);
    r.category = -1;
    tr.ingest_aircraft(&r, 1_000);
    let a = &tr.aircraft()[0];
    assert_eq!(a.callsign.as_str(), "N911PH");
    assert_eq!(a.squawk, Some(0o1200));
    assert_eq!(a.baro_alt_ft, Some(1200));
    assert_eq!(a.category, 7);
    assert_eq!(a.last_seen_ms, 1_000);
}

#[test]
fn geometry_uses_vincenty_and_flags_low_nearby() {
    let mut tr: Tracker = Tracker::default();
    with_own(&mut tr, 0);
    tr.ingest_aircraft(&report(1, 2_000.0, 1_000, 7), 0);
    tr.ingest_aircraft(&report(2, 2_000.0, 30_000, 3), 0);
    tr.ingest_aircraft(&report(3, LOW_RANGE_M + 2_000.0, 1_000, 7), 0);
    let ctx = tr.context(100, None);
    let g: std::vec::Vec<_> = tr
        .aircraft()
        .iter()
        .map(|a| ctx.aircraft_geometry(a))
        .collect();
    let r = g[0].range.unwrap();
    let want = geo::range_bearing(
        CITY_HALL.0,
        CITY_HALL.1,
        CITY_HALL.0 + 2_000.0 / 111_000.0,
        CITY_HALL.1,
    );
    assert_eq!(r, want);
    assert!(r.bearing_deg < 0.01 || r.bearing_deg > 359.99);
    // 1100 ft geometric = 335.3 m, minus own 40 m.
    assert!((g[0].above_m.unwrap() - (1_100.0 * 0.3048 - 40.0)).abs() < 1e-9);
    assert!(g[0].low_nearby);
    assert!(!g[1].low_nearby, "too high");
    assert!(!g[2].low_nearby, "too far");
    assert!(g[0].above_m.unwrap() <= LOW_ABOVE_M);

    // On the ground (not airborne) is never "low nearby".
    let mut r = report(4, 500.0, 0, 3);
    r.flags &= !(1 << flag::AIRBORNE);
    tr.ingest_aircraft(&r, 0);
    let a = tr
        .get_aircraft(AircraftKey {
            icao: 4,
            source: Source::ModeS,
        })
        .unwrap();
    assert!(!ctx.aircraft_geometry(a).low_nearby);

    // No own fix: no geometry at all (P4).
    tr.clear_own_position();
    let ctx = tr.context(100, None);
    assert_eq!(
        ctx.aircraft_geometry(&tr.aircraft()[0]),
        AircraftGeometry::default()
    );
}

#[test]
fn full_table_keeps_the_nearest() {
    let own = OwnFix {
        lat: CITY_HALL.0,
        lon: CITY_HALL.1,
        alt_m: None,
        fix_ms: 0,
    };
    let mut t: AircraftTable<3> = AircraftTable::new();
    for (icao, d) in [(1, 10_000.0), (2, 20_000.0), (3, 30_000.0)] {
        assert!(
            t.ingest(&report(icao, d, 5_000, 3), 0, Some(&own))
                .is_some()
        );
    }
    // Nearer than the farthest: the farthest goes.
    assert!(
        t.ingest(&report(4, 5_000.0, 5_000, 3), 0, Some(&own))
            .is_some()
    );
    let mut icaos: std::vec::Vec<u32> = t.all().iter().map(|a| a.key.icao).collect();
    icaos.sort();
    assert_eq!(icaos, [1, 2, 4]);
    // Farther than everyone: not added.
    assert_eq!(
        t.ingest(&report(5, 50_000.0, 5_000, 3), 0, Some(&own)),
        None
    );
    // An update to an aircraft already held always lands.
    assert!(
        t.ingest(&report(2, 60_000.0, 5_000, 3), 1, Some(&own))
            .is_some()
    );

    // A tracker built without ADS-B (capacity 0) holds nothing, harmlessly.
    let mut none: Tracker<4, 0> = Tracker::default();
    assert_eq!(none.ingest_aircraft(&report(1, 0.0, 1, 1), 0), None);
    assert!(none.aircraft().is_empty());
}

#[test]
fn json_aircraft_line() {
    let mut tr: Tracker = Tracker::default();
    with_own(&mut tr, 900);
    let mut r = report(0xA1B2C3, 1_000.0, 800, 7);
    r.squawk = Some(0o7700);
    tr.ingest_aircraft(&r, 1_000);
    let ctx = tr.context(1_500, Some(UTC));
    let mut buf = [0u8; 1024];
    let n = json::aircraft_line(&tr.aircraft()[0], tr.tracks(), &ctx, &mut buf).unwrap();
    let text = s(&buf, n);
    assert!(text.ends_with("}\n"));
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["t"], "aircraft");
    assert_eq!(v["icao"], "A1B2C3");
    assert_eq!(v["src"], "1090");
    assert_eq!(v["call"], "N911PH");
    assert_eq!(v["squawk"], "7700");
    assert_eq!(v["emerg"], "emergency");
    assert_eq!(v["kind"], "Rotorcraft");
    assert_eq!(v["label"], "Helicopter");
    assert_eq!(v["alt_baro_ft"], 800);
    assert_eq!(v["pos_age_ms"], 500);
    assert_eq!(v["state"], "active");
    assert_eq!(v["drone"], serde_json::Value::Null);
    assert_eq!(v["own"]["low"], true);
    assert_eq!(v["own"]["brg"], 0.0);
    assert!((v["own"]["dist_m"].as_f64().unwrap() - 1_000.0).abs() < 15.0);

    // No own fix: `own` is null, as for drones.
    tr.clear_own_position();
    let ctx = tr.context(1_500, None);
    let n = json::aircraft_line(&tr.aircraft()[0], tr.tracks(), &ctx, &mut buf).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&buf[..n]).unwrap();
    assert_eq!(v["own"], serde_json::Value::Null);
}

#[test]
fn aircraft_line_worst_case_fits() {
    let mut tr: Tracker = Tracker::default();
    tr.set_own_position(-89.123_456_789, -179.123_456_789, Some(-12_345.678_9), 0);
    let mut r = report(0xFFFFFF, 0.0, i32::MIN, i8::MIN);
    // FLARM has the longest source label ("FLARM" in JSON and CSV).
    r.source = Source::Flarm;
    r.icao = 0x0FFF_FFFF;
    r.lat_deg = -89.123_456_789_012_3;
    r.lon_deg = -179.123_456_789_012_3;
    r.gnss_alt_ft = i32::MIN;
    r.speed_kt = i32::MIN;
    r.baro_vrate_fpm = i32::MIN;
    r.direction_deg = 359.123_46;
    r.signal_dbm = i16::MIN;
    r.callsign = Callsign::new("WWWWWWWW").unwrap();
    r.squawk = Some(0o7500);
    r.flags = u32::MAX;
    tr.ingest_aircraft(&r, u64::MAX / 2);
    let mut ctx = tr.context(u64::MAX, Some(u64::MAX));
    ctx.own = Some(OwnFix {
        lat: 89.123_456_789_012_3,
        lon: 179.123_456_789_012_3,
        alt_m: Some(-12_345.678_901_234),
        fix_ms: 0,
    });
    let mut buf = [0u8; 2048];
    let n = json::aircraft_line(&tr.aircraft()[0], &[], &ctx, &mut buf).unwrap();
    std::println!("worst-case aircraft line: {n} bytes");
    assert!(n <= json::MAX_AIRCRAFT_LINE, "{n}");
    let mut row = [0u8; 1024];
    let n = csv::aircraft_row(&tr.aircraft()[0], &ctx, &mut row).unwrap();
    assert!(n <= csv::MAX_ROW, "{n}");
}

#[test]
fn state_lists_aircraft_last_and_streams_exactly() {
    let mut tr: Tracker = Tracker::default();
    with_own(&mut tr, 0);
    tr.ingest(
        0,
        &det(Transport::WifiBeacon, WIFI_MAC, -60, &[serial("SN-1")]),
        0,
    );
    tr.ingest_aircraft(&report(1, 1_000.0, 800, 7), 0);
    tr.ingest_aircraft(&report(2, 9_000.0, 9_000, 3), 0);
    let ctx = tr.context(100, Some(UTC));
    let adsb_status = json::AdsbStatus {
        alive: true,
        age_ms: Some(400),
        frames_per_s: Some(118),
        aircraft: Some(17),
        errors: 2,
    };
    let hub = json::HubStatus {
        own_source: json::OwnSource::Manual,
        manual: None,
        gps: json::GpsStatus {
            state: json::GpsState::Absent,
            sats: None,
        },
        recording: false,
        nodes: &[],
        adsb: Some(adsb_status),
    };
    let mut buf = std::vec![0u8; json::state_max_len(1, 0, 2, 0)];
    let n = json::state(
        tr.tracks(),
        tr.open_links(),
        tr.aircraft(),
        &ctx,
        &hub,
        &mut buf,
    )
    .unwrap();
    let text = s(&buf, n);
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["aircraft"].as_array().unwrap().len(), 2);
    assert_eq!(v["tracks"].as_array().unwrap().len(), 1);
    assert_eq!(v["adsb"]["frames_per_s"], 118);
    // Each aircraft is exactly its aircraft_line, and they come last.
    let mut line = [0u8; 1024];
    let ln = json::aircraft_line(&tr.aircraft()[1], tr.tracks(), &ctx, &mut line).unwrap();
    let last = s(&line, ln - 1);
    assert!(text.ends_with(&std::format!(",{last}]}}\n")), "{text}");
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
            Err(FormatError::BufferTooSmall)
        );
    }
}

#[test]
fn state_head_with_adsb_fits_its_bound() {
    let tr: Tracker = Tracker::default();
    let mut ctx = tr.context(u64::MAX, Some(u64::MAX));
    ctx.own = Some(OwnFix {
        lat: -89.123_456_789_012_3,
        lon: -179.123_456_789_012_3,
        alt_m: Some(-12_345.678_901_234),
        fix_ms: 0,
    });
    let hub = json::HubStatus {
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
        nodes: &[],
        adsb: Some(json::AdsbStatus {
            alive: false,
            age_ms: Some(u64::MAX),
            frames_per_s: Some(u32::MAX),
            aircraft: Some(u16::MAX),
            errors: u32::MAX,
        }),
    };
    let mut buf = [0u8; 2048];
    let n = json::state(&[], &[], &[], &ctx, &hub, &mut buf).unwrap();
    std::println!("worst-case state head with ADS-B: {n} bytes");
    assert!(n <= json::STATE_HEAD_MAX, "{n}");
}

#[test]
fn csv_aircraft_row() {
    let mut tr: Tracker = Tracker::default();
    with_own(&mut tr, 4_900);
    tr.ingest_aircraft(&report(0xA1B2C3, 1_000.0, 800, 7), 4_500);
    let ctx = tr.context(5_000, Some(UTC));
    let mut buf = [0u8; 512];
    let n = csv::aircraft_row(&tr.aircraft()[0], &ctx, &mut buf).unwrap();
    let row = s(&buf, n);
    assert_eq!(row.matches(',').count(), csv::HEADER.matches(',').count());
    let cols: std::vec::Vec<&str> = row.trim_end().split(',').collect();
    let head: std::vec::Vec<&str> = csv::HEADER.trim_end().split(',').collect();
    let col = |name: &str| cols[head.iter().position(|h| *h == name).unwrap()];
    assert_eq!(col("utc"), "2026-11-09T14:03:07.250Z");
    assert_eq!(col("track"), "ICAO-A1B2C3");
    assert_eq!(col("uas_id"), "N911PH");
    assert_eq!(col("id_type"), "Callsign");
    assert_eq!(col("ua_type"), "Rotorcraft");
    assert_eq!(col("transports"), "Adsb1090");
    assert_eq!(col("drone_alt_m"), "274.3"); // 900 ft geometric
    assert_eq!(col("baro_alt_m"), "243.8"); // 800 ft
    assert_eq!(col("speed_mps"), "48.87"); // 95 kt
    assert_eq!(col("rssi_dbm"), "-62");
    assert_eq!(col("brg_deg"), "0.0");
    assert_eq!(col("op_lat"), "");
}

#[test]
fn cot_aircraft_event_and_types() {
    use adsb::Kind;
    assert_eq!(cot::aircraft_type(Kind::Rotorcraft), "a-n-A-C-H");
    assert_eq!(cot::aircraft_type(Kind::Light), "a-n-A-C-F");
    assert_eq!(cot::aircraft_type(Kind::Unknown), "a-n-A-C-F");
    assert_eq!(cot::aircraft_type(Kind::LighterThanAir), "a-n-A-C-L");
    let mut tr: Tracker = Tracker::default();
    with_own(&mut tr, 4_900);
    tr.ingest_aircraft(&report(0xA1B2C3, 1_000.0, 800, 7), 4_500);
    let a = &tr.aircraft()[0];

    let ctx = tr.context(5_000, None);
    let mut buf = [0u8; cot::MAX_EVENT];
    assert_eq!(
        cot::aircraft_event(a, tr.tracks(), &ctx, &mut buf),
        Err(FormatError::NoTime)
    );
    let ctx = tr.context(5_000, Some(UTC));
    let n = cot::aircraft_event(a, tr.tracks(), &ctx, &mut buf).unwrap();
    let x = s(&buf, n);
    assert!(
        x.contains(r#"uid="ICAO-A1B2C3" type="a-n-A-C-H" how="m-g""#),
        "{x}"
    );
    assert!(x.contains(r#"<contact callsign="N911PH"/>"#), "{x}");
    // Position received at 4500 ms = 500 ms before UTC; stale 30 s later.
    assert!(x.contains(r#"time="2026-11-09T14:03:06.750Z""#), "{x}");
    assert!(x.contains(r#"stale="2026-11-09T14:03:36.750Z""#), "{x}");
    assert!(x.contains(r#"hae="274.3""#), "{x}");
    assert!(x.contains("Squawk: 1200; Altitude: 800 ft"), "{x}");

    // No position: nothing to plot.
    let mut tr2: Tracker = Tracker::default();
    let mut r = report(5, 0.0, 800, 7);
    r.flags &= !(1 << flag::POSITION_VALID);
    tr2.ingest_aircraft(&r, 0);
    assert_eq!(
        cot::aircraft_event(
            &tr2.aircraft()[0],
            &[],
            &tr2.context(0, Some(UTC)),
            &mut buf
        ),
        Err(FormatError::NoPosition)
    );
}

#[test]
fn adsb_uav_that_is_a_remote_id_drone_is_not_sent_twice() {
    let mut tr: Tracker = Tracker::default();
    with_own(&mut tr, 0);
    // A Remote ID drone 1 km north at 120 m...
    let lat = CITY_HALL.0 + 1_000.0 / 111_000.0;
    tr.ingest(
        0,
        &det(
            Transport::WifiBeacon,
            WIFI_MAC,
            -60,
            &[serial("SN-UAV"), location(lat, CITY_HALL.1, 120.0)],
        ),
        0,
    );
    // ... and an ADS-B "UAV" 50 m from it, plus a helicopter at the same spot.
    tr.ingest_aircraft(&report(0xDDDDDD, 1_050.0, 400, 14), 0);
    tr.ingest_aircraft(&report(0xEEEEEE, 1_050.0, 400, 7), 0);
    let ctx = tr.context(100, Some(UTC));
    let uav = &tr.aircraft()[0];
    let heli = &tr.aircraft()[1];
    let drone = tr.tracks()[0].id;
    assert_eq!(same_drone(uav, tr.tracks(), &ctx), Some(drone));
    // Only a UAV is ever taken for a drone: the helicopter is its own thing.
    assert_eq!(same_drone(heli, tr.tracks(), &ctx), None);
    let mut buf = [0u8; cot::MAX_EVENT];
    assert_eq!(
        cot::aircraft_event(uav, tr.tracks(), &ctx, &mut buf),
        Err(FormatError::Duplicate)
    );
    assert!(cot::aircraft_event(heli, tr.tracks(), &ctx, &mut buf).is_ok());
    let n = json::aircraft_line(uav, tr.tracks(), &ctx, &mut buf).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&buf[..n]).unwrap();
    assert_eq!(v["drone"], drone.0);
    // Still exactly one drone track and no alert from the aircraft.
    assert_eq!(tr.tracks().len(), 1);
}
