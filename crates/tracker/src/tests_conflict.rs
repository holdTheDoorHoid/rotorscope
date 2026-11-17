//! Drone near a crewed aircraft (`conflict`).

extern crate std;
use std::string::String;

use crate::conflict::{self, AltBasis, Body, Level, REALERT_MS};
use crate::tests::{CITY_HALL, WIFI_MAC, det, location, serial};
use crate::*;
use adsb::{Aircraft, Callsign, Source, flag};
use rid::Transport;

const M_PER_DEG: f64 = 111_000.0;

/// A helicopter (category 7) `north_m` north of City Hall, heading `dir`.
pub(crate) fn heli(
    icao: u32,
    north_m: f64,
    baro_ft: i32,
    gnss_ft: i32,
    category: i8,
    dir: f32,
    kt: i32,
) -> Aircraft {
    Aircraft {
        source: Source::ModeS,
        icao,
        flags: 0xff | (1 << flag::UPDATED_POSITION),
        callsign: Callsign::new("N911PH").unwrap(),
        squawk: Some(0o1200),
        category,
        lat_deg: CITY_HALL.0 + north_m / M_PER_DEG,
        lon_deg: CITY_HALL.1,
        baro_alt_ft: baro_ft,
        gnss_alt_ft: gnss_ft,
        direction_deg: dir,
        speed_kt: kt,
        baro_vrate_fpm: 0,
        gnss_vrate_fpm: 0,
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

/// A Remote ID drone at City Hall, 120 m GPS height.
fn tracker_with_drone() -> Tracker {
    let mut tr: Tracker = Tracker::default();
    tr.ingest(
        0,
        &det(
            Transport::WifiBeacon,
            WIFI_MAC,
            -60,
            &[serial("SN-NEAR"), location(CITY_HALL.0, CITY_HALL.1, 120.0)],
        ),
        0,
    );
    tr
}

fn body(north_m: f64, geo: Option<f64>, baro: Option<f64>, vel: Option<(f64, f64)>) -> Body {
    Body {
        lat: CITY_HALL.0 + north_m / M_PER_DEG,
        lon: CITY_HALL.1,
        alt_geo: geo,
        alt_baro: baro,
        vel,
        vz: None,
        age_s: 0.0,
    }
}

#[test]
fn helicopter_beside_a_drone_warns_once() {
    let mut tr = tracker_with_drone();
    // 50 m north, GPS 500 ft (152 m): 32 m above the drone.
    tr.ingest_aircraft(&heli(0xA1B2C3, 50.0, 400, 500, 7, 270.0, 60), 0);
    let c = tr.conflicts(100);
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].level, Level::Warning);
    assert_eq!(c[0].basis, AltBasis::Same);
    let dz = c[0].dz_m.unwrap();
    assert!((dz - 32.4).abs() < 1.0, "{dz}");

    let first = tr.take_new_conflict(100).expect("announced");
    assert_eq!(first.level, Level::Warning);
    assert_eq!(tr.take_new_conflict(1_100), None, "once per pair and level");

    let drone = tr.get(first.drone).unwrap();
    let text = conflict::describe(&first, drone, tr.get_aircraft(first.aircraft));
    assert!(text.starts_with("warning: drone SN-NEAR"), "{text}");
    assert!(text.contains("N911PH (Helicopter)"), "{text}");
    assert!(text.contains("m below it"), "{text}");

    let mut buf = [0u8; conflict::MAX_CONFLICT_LINE];
    let n = conflict::conflict_line(&first, tr.get_aircraft(first.aircraft), &mut buf).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&buf[..n]).unwrap();
    assert_eq!(v["t"], "conflict");
    assert_eq!(v["level"], "warning");
    assert_eq!(v["callsign"], "N911PH");
    assert_eq!(v["icao"], 0xA1B2C3);
    assert_eq!(v["src"], "ADS-B");
    assert_eq!(v["alt_basis"], "same");
    let _ = String::new();
}

#[test]
fn aircraft_well_above_or_unmanned_or_far_is_ignored() {
    let mut tr = tracker_with_drone();
    // Overhead but 5,000 ft up.
    tr.ingest_aircraft(&heli(0x000001, 50.0, 5_000, 5_100, 7, 270.0, 60), 0);
    // An ADS-B drone (category 14) right beside ours.
    tr.ingest_aircraft(&heli(0x000002, 30.0, 400, 500, 14, 270.0, 20), 0);
    // A helicopter 10 km away at the same height.
    tr.ingest_aircraft(&heli(0x000003, 10_000.0, 400, 500, 7, 270.0, 60), 0);
    assert!(tr.conflicts(100).is_empty(), "{:?}", tr.conflicts(100));
}

#[test]
fn stale_tracks_do_not_conflict() {
    let mut tr = tracker_with_drone();
    tr.ingest_aircraft(&heli(0xA1B2C3, 50.0, 400, 500, 7, 270.0, 60), 0);
    // Long after both were last heard.
    assert!(tr.conflicts(10 * 60 * 1000).is_empty());
}

#[test]
fn closing_aircraft_is_predicted() {
    let drone = body(0.0, Some(100.0), None, None);
    // 100 kt (51 m/s) heading north, starting south of the drone.
    let v = Some((0.0, 51.4));
    // 1.5 km out: closest in ~29 s, right over the drone -> caution.
    let (level, range, _, cpa, _) =
        conflict::assess(&drone, &body(-1_500.0, Some(120.0), None, v)).unwrap();
    assert_eq!(level, Level::Caution);
    assert!((range - 1_500.0).abs() < 5.0);
    let (t, m) = cpa.unwrap();
    assert!((t - 29.2).abs() < 0.5 && m < 1.0, "{t} {m}");
    // 900 m out: ~17.5 s -> warning, although it is not yet within 300 m.
    let (level, ..) = conflict::assess(&drone, &body(-900.0, Some(120.0), None, v)).unwrap();
    assert_eq!(level, Level::Warning);
    // Flying away from the drone, 900 m north: no prediction, caution by distance.
    let (level, _, _, cpa, _) =
        conflict::assess(&drone, &body(900.0, Some(120.0), None, v)).unwrap();
    assert_eq!((level, cpa), (Level::Caution, None));
    // Passing 800 m to the side never gets closer than the advisory ring.
    let side = Body {
        lon: CITY_HALL.1 + 800.0 / 85_000.0,
        ..body(-900.0, Some(120.0), None, v)
    };
    let (level, ..) = conflict::assess(&drone, &side).unwrap();
    assert_eq!(level, Level::Advisory);
}

#[test]
fn heights_compare_like_with_like() {
    // Drone has pressure altitude only; the aircraft's own GPS-minus-baro
    // offset (+30 m) puts it on the GPS scale.
    let d = body(0.0, None, Some(100.0), None);
    let a = body(100.0, Some(160.0), Some(130.0), None);
    let (dz, basis) = conflict::height_difference(&d, &a);
    assert_eq!(basis, AltBasis::Same);
    assert!((dz.unwrap() - 30.0).abs() < 1e-9);

    // GPS against pressure: mixed, and the margin widens (200 m apart is
    // still "same height" at 150 + 90 m).
    let d = body(0.0, Some(100.0), None, None);
    let a = body(200.0, None, Some(300.0), None);
    let (level, _, dz, _, basis) = conflict::assess(&d, &a).unwrap();
    assert_eq!(basis, AltBasis::Mixed);
    assert_eq!(dz, Some(200.0));
    assert_eq!(level, Level::Warning);

    // No height on the drone: judged on distance, never a warning.
    let d = body(0.0, None, None, None);
    let a = body(50.0, Some(3_000.0), None, None);
    let (level, _, dz, _, basis) = conflict::assess(&d, &a).unwrap();
    assert_eq!(
        (level, dz, basis),
        (Level::Caution, None, AltBasis::Unknown)
    );
}

#[test]
fn old_positions_are_extrapolated_a_little() {
    // The aircraft's position is 8 s old and it flies north at 50 m/s:
    // it is now 400 m nearer than its last report.
    let d = body(0.0, Some(100.0), None, None);
    let mut a = body(-1_000.0, Some(100.0), None, Some((0.0, 50.0)));
    a.age_s = 8.0;
    let (_, range, ..) = conflict::assess(&d, &a).unwrap();
    assert!((range - 600.0).abs() < 5.0, "{range}");
    // ...but never more than MAX_EXTRAPOLATE_S worth.
    a.age_s = 30.0;
    let (_, range, ..) = conflict::assess(&d, &a).unwrap();
    assert!((range - 500.0).abs() < 5.0, "{range}");
}

#[test]
fn escalation_and_realert() {
    let mut tr = tracker_with_drone();
    // 2 km out at the same height: advisory.
    tr.ingest_aircraft(&heli(0xA1B2C3, 2_000.0, 400, 500, 7, 270.0, 0), 0);
    assert_eq!(tr.take_new_conflict(100).unwrap().level, Level::Advisory);
    assert_eq!(tr.take_new_conflict(200), None);
    // Now 200 m away: warning, announced again.
    tr.ingest_aircraft(&heli(0xA1B2C3, 200.0, 400, 500, 7, 270.0, 0), 1_000);
    tr.ingest(
        0,
        &det(
            Transport::WifiBeacon,
            WIFI_MAC,
            -60,
            &[serial("SN-NEAR"), location(CITY_HALL.0, CITY_HALL.1, 120.0)],
        ),
        1_000,
    );
    assert_eq!(tr.take_new_conflict(1_100).unwrap().level, Level::Warning);
    assert_eq!(tr.take_new_conflict(1_200), None);

    // The watch alone: same pair, same level, after the re-alert time.
    let mut w = conflict::ConflictWatch::new();
    let c = tr.conflicts(1_100);
    assert!(w.take_new(&c, 0).is_some());
    assert!(w.take_new(&c, REALERT_MS - 1).is_none());
    assert!(w.take_new(&c, REALERT_MS).is_some());
}
