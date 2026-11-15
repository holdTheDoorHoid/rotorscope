//! Real-data checks (PLAN.md Phase 12 "done when"): drones placed by fake
//! positions at known spots around PHL and PNE get the right flags against
//! a pack built from the public data.
//!
//! Needs a Philadelphia pack (not committed; public data is downloaded, not
//! stored in git):
//!
//! ```text
//! cargo run --release -p airspace-pack -- --lat 39.95 --lon -75.16 --out /tmp/AIRSPACE.PAK
//! AIRSPACE_PAK=/tmp/AIRSPACE.PAK cargo test -p airspace-pack --test philly -- --nocapture
//! ```
//! Without `AIRSPACE_PAK` every test prints "skipped" and passes.

use airspace::{AreaKind, Pack};
use faarules::{Engine, Rule};
use rid::Transport;
use rid::message::*;
use tracker::{Observation, Tracker};

fn pack_bytes() -> Option<Vec<u8>> {
    let path = std::env::var("AIRSPACE_PAK").ok()?;
    Some(std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}")))
}

macro_rules! need_pack {
    () => {
        match pack_bytes() {
            Some(b) => b,
            None => {
                eprintln!("skipped: set AIRSPACE_PAK to a Philadelphia pack");
                return;
            }
        }
    };
}

/// 2026-07-01 17:00 UTC (daytime).
const DAY_MS: u64 = 1_782_925_200_000;

// Reference points (FAA airport data / published coordinates).
const PHL: (f64, f64) = (39.872_09, -75.240_66);
const PNE: (f64, f64) = (40.081_95, -75.010_58);
const INDEPENDENCE_MALL: (f64, f64) = (39.950_3, -75.149_6);
const LINC_FIELD: (f64, f64) = (39.900_8, -75.167_4);
/// Fairmount Park (Belmont Plateau), inside the city but outside the PHL
/// Class B surface area and the PNE Class D.
const BELMONT: (f64, f64) = (39.998_5, -75.212_5);

fn loc(p: (f64, f64), hae_m: f32) -> Location {
    Location {
        status: Status::Airborne,
        height_type: HeightType::AboveTakeoff,
        direction: None,
        speed_horizontal: Some(3.0),
        speed_vertical: None,
        position: Some(LatLon { lat: p.0, lon: p.1 }),
        altitude_pressure: None,
        altitude_geodetic: Some(hae_m),
        height: None,
        horizontal_accuracy: HorizontalAccuracy::M3,
        vertical_accuracy: VerticalAccuracy::M3,
        baro_accuracy: VerticalAccuracy::M3,
        speed_accuracy: SpeedAccuracy::Mps1,
        timestamp: None,
        timestamp_accuracy: None,
    }
}

fn system(p: (f64, f64)) -> System {
    System {
        operator_location_type: OperatorLocationType::LiveGnss,
        classification_type: ClassificationType::Undeclared,
        operator_position: Some(LatLon {
            lat: p.0 + 0.001,
            lon: p.1,
        }),
        area_count: 1,
        area_radius: 0,
        area_ceiling: None,
        area_floor: None,
        category: Category::Undeclared,
        class: UaClass::Undeclared,
        operator_altitude: None,
        timestamp: None,
    }
}

/// Fly a drone at `p`, `agl_m` above the pack's ground, for `secs`; return
/// the flags (rule, area, level) and the printed sentences.
fn fly(bytes: &[u8], p: (f64, f64), agl_m: f64, secs: u64) -> Vec<(Rule, Option<AreaKind>, u8)> {
    let pack = Pack::parse(bytes).unwrap();
    let ground = pack.ground_hae_m(p.0, p.1).expect("terrain");
    let mut tr: Tracker<4, 2> = Tracker::default();
    let mut eng: Engine<4> = Engine::default();
    let sn = Message::BasicId(BasicId {
        id_type: IdType::SerialNumber,
        ua_type: UaType::HelicopterOrMultirotor,
        uas_id: FixedStr::new(b"RSTEST-PHILLY").unwrap(),
    });
    let mut now = 1000;
    let msgs = [
        sn,
        Message::Location(loc(p, (ground + agl_m) as f32)),
        Message::System(system(p)),
    ];
    let mut evals_us = Vec::new();
    for _ in 0..secs {
        now += 1000;
        let o = Observation {
            node: 0,
            transport: Transport::WifiBeacon,
            channel: 6,
            rssi: -60,
            mac: [2, 0, 0, 0, 0, 9],
            counter: 0,
            rx_ms: now,
        };
        tr.ingest_messages(&o, &msgs, 0);
        let t = std::time::Instant::now();
        eng.update(&tr, Some(&pack), now, Some(DAY_MS + now));
        evals_us.push(t.elapsed().as_micros());
    }
    let e = &eng.entries()[0];
    println!(
        "--- {p:?} {agl_m} m AGL for {secs} s (host eval max {} us):",
        evals_us.iter().max().unwrap()
    );
    for f in &e.flags {
        let mut s = String::new();
        faarules::text::write_flag(&mut s, f, Some(&pack)).unwrap();
        println!("  {s}");
        assert!(!s.to_lowercase().contains("illegal"));
    }
    e.flags.iter().map(|f| (f.rule, f.area, f.level)).collect()
}

fn has(
    flags: &[(Rule, Option<AreaKind>, u8)],
    rule: Rule,
    area: Option<AreaKind>,
    level: u8,
) -> bool {
    flags
        .iter()
        .any(|&(r, a, l)| r == rule && (area.is_none() || a == area) && l == level)
}

#[test]
fn ground_heights_match_published_field_elevations() {
    let b = need_pack!();
    let p = Pack::parse(&b).unwrap();
    // FAA field elevations: PHL 36 ft (11 m), PNE 120 ft (36.6 m). A field
    // elevation is the highest point of the runways; the reference point's
    // ground is lower, and 15 arc-second posts average over ~400 m (PHL sits
    // on the river bank). So: within 10 m, and not above the field elevation
    // by more than 3 m.
    for (pt, elev_m) in [(PHL, 11.0), (PNE, 36.6)] {
        let g = p.ground_msl_m(pt.0, pt.1).unwrap();
        println!("{pt:?}: ground {g:.1} m, field {elev_m} m");
        assert!((g - elev_m).abs() < 10.0 && g < elev_m + 3.0, "{pt:?} {g}");
    }
    // GEOID18 at Philadelphia is about -33 m.
    assert!((p.geoid_m(39.95, -75.16).unwrap() + 32.95).abs() < 0.5);
}

#[test]
fn phl_class_b_surface_and_zero_ceiling() {
    let b = need_pack!();
    let p = Pack::parse(&b).unwrap();
    assert_eq!(
        p.uasfm_ceiling_ft(PHL.0, PHL.1),
        Some(0),
        "PHL field is a 0 ft facility-map cell"
    );
    let f = fly(&b, PHL, 60.0, 20);
    assert!(
        has(&f, Rule::Controlled, Some(AreaKind::ClassB), 0),
        "{f:?}"
    );
    assert!(has(&f, Rule::FacilityMap, None, 2), "{f:?}");
    // Over 400 ft too: an urban cell, so 525 ft is "check for structures"
    // (Watch) and 850 ft a Warning.
    let f = fly(&b, PHL, 160.0, 40);
    assert!(has(&f, Rule::Height, None, 1), "{f:?}");
    let f = fly(&b, PHL, 260.0, 40);
    assert!(has(&f, Rule::Height, None, 2), "{f:?}");
}

#[test]
fn pne_class_d() {
    let b = need_pack!();
    let f = fly(&b, PNE, 40.0, 20);
    assert!(
        has(&f, Rule::Controlled, Some(AreaKind::ClassD), 0),
        "{f:?}"
    );
    assert!(
        f.iter().any(|&(r, _, l)| r == Rule::FacilityMap && l >= 1),
        "{f:?}"
    );
}

#[test]
fn independence_mall_national_security_area() {
    let b = need_pack!();
    let f = fly(&b, INDEPENDENCE_MALL, 50.0, 10);
    assert!(
        has(&f, Rule::Area, Some(AreaKind::NationalSecurity), 3),
        "{f:?}"
    );
    // Above its 400 ft AGL ceiling the restriction no longer applies (the
    // height flag does).
    let f = fly(&b, INDEPENDENCE_MALL, 160.0, 10);
    assert!(
        !f.iter()
            .any(|&(r, a, _)| r == Rule::Area && a == Some(AreaKind::NationalSecurity)),
        "{f:?}"
    );
}

#[test]
fn stadium_is_only_a_note() {
    let b = need_pack!();
    let f = fly(&b, LINC_FIELD, 50.0, 10);
    assert!(has(&f, Rule::Area, Some(AreaKind::Stadium), 0), "{f:?}");
}

#[test]
fn city_park_low_is_clean_and_high_is_a_structure_watch() {
    let b = need_pack!();
    let f = fly(&b, BELMONT, 60.0, 40);
    assert!(f.iter().all(|&(_, _, l)| l == 0), "{f:?}");
    // 200 m (656 ft) over an urban cell: "check for structures", Watch.
    let f = fly(&b, BELMONT, 200.0, 40);
    assert!(has(&f, Rule::Height, None, 1), "{f:?}");
}

#[test]
fn lookups_are_fast_on_the_host() {
    let b = need_pack!();
    let p = Pack::parse(&b).unwrap();
    let t = std::time::Instant::now();
    let mut n = 0;
    for i in 0..1000 {
        let lat = 39.85 + f64::from(i % 40) * 0.008;
        let lon = -75.35 + f64::from(i / 40) * 0.012;
        n += p.areas_at(lat, lon).count();
        let _ = p.ground_hae_m(lat, lon);
        let _ = p.uasfm_ceiling_ft(lat, lon);
    }
    println!("1000 lookups: {:?} ({n} area hits)", t.elapsed());
}
