//! Rule-engine behaviour on a small synthetic pack (real-data tests against
//! a downloaded Philadelphia pack live in `tools/airspace-pack/tests`).

extern crate std;
use std::string::{String, ToString};
use std::vec;
use std::vec::Vec;

use airspace::write::*;
use airspace::{AltRef, AreaKind, Pack};
use rid::Transport;
use rid::message::*;
use tracker::{Observation, Tracker};

use super::*;

// A made-up flat place, terrain 10 m, geoid -33 m: ground is -23 m HAE.
const C: (f64, f64) = (39.90, -75.20);
const GROUND_HAE: f32 = -23.0;
/// 2026-07-01 03:00 UTC (23:00 EDT): night in Philadelphia.
pub(crate) const NIGHT_MS: u64 = 1_782_874_800_000;
/// 2026-07-01 17:00 UTC: midday.
const DAY_MS: u64 = 1_782_925_200_000;

fn square(lat: f64, lon: f64, half: f64) -> Vec<(f64, f64)> {
    vec![
        (lat - half, lon - half),
        (lat - half, lon + half),
        (lat + half, lon + half),
        (lat + half, lon - half),
    ]
}

fn area(kind: AreaKind, name: &str, ring: Vec<(f64, f64)>, ceil_ft: i32) -> AreaDef {
    AreaDef {
        kind,
        source: airspace::format::src::SPECIAL_USE,
        floor_ref: AltRef::Agl,
        floor_ft: 0,
        ceil_ref: AltRef::Msl,
        ceil_ft,
        start: 0,
        end: 0,
        flags: 0,
        name: name.to_string(),
        rings: vec![ring],
    }
}

pub(crate) fn pack_bytes() -> Vec<u8> {
    let mut b = PackBuilder {
        created: 1_794_700_000,
        center: C,
        radius_m: 30_000,
        ..Default::default()
    };
    b.sources.push(SourceDef {
        id: 7,
        date: 1_794_700_000,
        fetched: 1_794_700_000,
        name: "TEST terrain".to_string(),
    });
    b.sources.push(SourceDef {
        id: 3,
        date: 1_793_275_366,
        fetched: 1_794_700_000,
        name: "TEST SUA".to_string(),
    });
    // Class D, 0.02 deg square north-east of C, with a 0 ft facility-map cell.
    b.areas.push(area(
        AreaKind::ClassD,
        "TEST CLASS D",
        square(40.0, -75.1, 0.02),
        2500,
    ));
    b.areas.push(area(
        AreaKind::Prohibited,
        "P-TEST",
        square(39.8, -75.3, 0.01),
        18000,
    ));
    b.areas.push(area(
        AreaKind::Fria,
        "FRIA FIELD",
        square(39.75, -75.2, 0.01),
        18000,
    ));
    let n = 81u32;
    b.terrain = Some(GridDef {
        lat0: 39.5,
        lon0: -75.6,
        dlat: 0.01,
        dlon: 0.01,
        rows: n,
        cols: n,
        values: vec![10; (n * n) as usize],
    });
    b.geoid_cm = Some(GridDef {
        lat0: 39.5,
        lon0: -75.6,
        dlat: 0.4,
        dlon: 0.4,
        rows: 3,
        cols: 3,
        values: vec![-3300; 9],
    });
    // Facility-map grid covering the Class D: 0 ft everywhere in it.
    b.uasfm = Some(GridDef {
        lat0: 39.98,
        lon0: -75.12,
        dlat: 0.04,
        dlon: 0.04,
        rows: 1,
        cols: 1,
        values: vec![0],
    });
    // Urban everywhere south of 39.85.
    b.urban = Some(GridDef {
        lat0: 39.5,
        lon0: -75.6,
        dlat: 0.35,
        dlon: 0.8,
        rows: 2,
        cols: 1,
        values: vec![true, false],
    });
    b.airports.push(AirportDef {
        lat: 40.0,
        lon: -75.1,
        elev_ft: 30,
        kind: 0,
        flags: 3,
        ident: "TST".to_string(),
    });
    b.to_bytes()
}

fn obs(t: Transport, ms: u64) -> Observation {
    Observation {
        node: 0,
        transport: t,
        channel: 6,
        rssi: -60,
        mac: [0x60, 0x60, 0x1f, 0, 0, 1],
        counter: 0,
        rx_ms: ms,
    }
}

fn loc(lat: f64, lon: f64, agl_m: f32) -> Location {
    Location {
        status: Status::Airborne,
        height_type: HeightType::AboveTakeoff,
        direction: None,
        speed_horizontal: Some(5.0),
        speed_vertical: None,
        position: Some(LatLon { lat, lon }),
        altitude_pressure: None,
        altitude_geodetic: Some(GROUND_HAE + agl_m),
        height: Some(agl_m),
        horizontal_accuracy: HorizontalAccuracy::M3,
        vertical_accuracy: VerticalAccuracy::M3,
        baro_accuracy: VerticalAccuracy::M3,
        speed_accuracy: SpeedAccuracy::Mps1,
        timestamp: None,
        timestamp_accuracy: None,
    }
}

fn sys(lat: f64, lon: f64, kind: OperatorLocationType) -> System {
    System {
        operator_location_type: kind,
        classification_type: ClassificationType::Undeclared,
        operator_position: Some(LatLon { lat, lon }),
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

fn basic(sn: &str) -> Message {
    Message::BasicId(BasicId {
        id_type: IdType::SerialNumber,
        ua_type: UaType::HelicopterOrMultirotor,
        uas_id: FixedStr::new(sn.as_bytes()).unwrap(),
    })
}

struct Sim {
    tr: Tracker<8, 4>,
    eng: Engine<8>,
    bytes: Vec<u8>,
    now: u64,
    utc0: u64,
}

impl Sim {
    fn new(utc0: u64) -> Self {
        Self {
            tr: Tracker::default(),
            eng: Engine::default(),
            bytes: pack_bytes(),
            now: 1_000,
            utc0,
        }
    }

    /// One second: the drone at `l` with operator `s`, then an evaluation.
    fn step(&mut self, msgs: &[Message]) {
        self.now += 1000;
        self.tr
            .ingest_messages(&obs(Transport::WifiBeacon, self.now), msgs, 0);
        self.eval();
    }

    fn eval(&mut self) {
        let pack = Pack::parse(&self.bytes).unwrap();
        self.eng
            .update(&self.tr, Some(&pack), self.now, Some(self.utc0 + self.now));
    }

    fn entry(&self) -> &Entry {
        &self.eng.entries()[0]
    }

    fn level(&self, r: Rule) -> Option<u8> {
        self.entry()
            .flags
            .iter()
            .filter(|f| f.rule == r)
            .map(|f| f.level)
            .max()
    }

    fn texts(&self) -> String {
        let pack = Pack::parse(&self.bytes).unwrap();
        let mut s = String::new();
        for f in &self.entry().flags {
            let mut line = heapless::String::<400>::new();
            text::write_flag(&mut line, f, Some(&pack)).unwrap();
            s.push_str(&line);
            s.push('\n');
        }
        s
    }
}

fn flight(agl: f32, op_dist_deg: f64, kind: OperatorLocationType) -> [Message; 3] {
    [
        basic("SN-TEST-1"),
        Message::Location(loc(C.0, C.1, agl)),
        Message::System(sys(C.0 + op_dist_deg, C.1, kind)),
    ]
}

#[test]
fn height_levels_rise_with_time_and_margin() {
    let mut s = Sim::new(DAY_MS);
    // 160 m (525 ft), +/-3 m GNSS + 10 m terrain: well past 450 ft.
    let m = flight(160.0, 0.001, OperatorLocationType::LiveGnss);
    s.step(&m);
    assert_eq!(s.level(Rule::Height), None, "one report never flags");
    for _ in 0..3 {
        s.step(&m);
    }
    assert_eq!(s.level(Rule::Height), Some(1));
    for _ in 0..30 {
        s.step(&m);
    }
    assert_eq!(s.level(Rule::Height), Some(2), "{}", s.texts());
    let t = s.texts();
    assert!(
        t.contains("possible violation") && t.contains("107.51(b)") && t.contains("TEST terrain"),
        "{t}"
    );
    assert!(!t.to_lowercase().contains("illegal"));
    assert!(s.entry().summary.max_agl_m > 159.0);
}

#[test]
fn height_within_accuracy_stays_watch() {
    let mut s = Sim::new(DAY_MS);
    // 127 m (417 ft): over the limit, but not by more than the error + 50 ft.
    let m = flight(127.0, 0.001, OperatorLocationType::LiveGnss);
    for _ in 0..40 {
        s.step(&m);
    }
    assert_eq!(s.level(Rule::Height), Some(1));
    // Below the limit: no height flag at all.
    let mut s = Sim::new(DAY_MS);
    let m = flight(100.0, 0.001, OperatorLocationType::LiveGnss);
    for _ in 0..40 {
        s.step(&m);
    }
    assert_eq!(s.level(Rule::Height), None);
}

#[test]
fn distance_at_night_is_raised_and_takeoff_point_is_softer() {
    // 0.0144 deg north = 1.6 km, live operator, night: Warning + 1 = Alert.
    let mut s = Sim::new(NIGHT_MS);
    let m = flight(60.0, 0.0144, OperatorLocationType::LiveGnss);
    for _ in 0..35 {
        s.step(&m);
    }
    assert_eq!(s.level(Rule::Night), Some(0));
    assert_eq!(s.level(Rule::Vlos), Some(3), "{}", s.texts());
    // Same by day: Warning.
    let mut s = Sim::new(DAY_MS);
    for _ in 0..35 {
        s.step(&m);
    }
    assert_eq!(s.level(Rule::Vlos), Some(2));
    assert_eq!(s.level(Rule::Night), None);
    // 700 m from a takeoff point by day: Watch only, however long.
    let mut s = Sim::new(DAY_MS);
    let m = flight(60.0, 0.0063, OperatorLocationType::Takeoff);
    for _ in 0..40 {
        s.step(&m);
    }
    assert_eq!(s.level(Rule::Vlos), Some(1));
}

#[test]
fn facility_map_and_controlled_airspace() {
    let mut s = Sim::new(DAY_MS);
    let m = [
        basic("SN-D"),
        Message::Location(loc(40.0, -75.1, 60.0)),
        Message::System(sys(40.0005, -75.1, OperatorLocationType::LiveGnss)),
    ];
    for _ in 0..15 {
        s.step(&m);
    }
    assert_eq!(s.level(Rule::Controlled), Some(0), "{}", s.texts());
    assert_eq!(s.level(Rule::FacilityMap), Some(2), "{}", s.texts());
    assert!(s.entry().summary.area_s[AreaKind::ClassD as usize] >= 10);
    let mut j = heapless::String::<2048>::new();
    text::write_json(&mut j, s.entry(), None).unwrap();
    assert!(
        j.contains("\"area_s\":{\"Class D\":") && j.contains("\"above_uasfm_s\":"),
        "{j}"
    );
    // The track goes away: its flight summary is kept for the log.
    let id = s.tr.tracks()[0].id;
    s.now += 700_000;
    s.tr.expire(s.now);
    assert!(s.tr.get(id).is_none());
    s.eval();
    let (k, sm) = s.eng.take_finished().expect("summary");
    assert_eq!(k, Key::Drone(id));
    assert_eq!(sm.worst_level, 2);
}

#[test]
fn prohibited_area_becomes_alert_after_a_few_seconds() {
    let mut s = Sim::new(DAY_MS);
    let m = [
        basic("SN-P"),
        Message::Location(loc(39.8, -75.3, 30.0)),
        Message::System(sys(39.8005, -75.3, OperatorLocationType::LiveGnss)),
    ];
    s.step(&m);
    assert_eq!(s.level(Rule::Area), Some(2));
    for _ in 0..6 {
        s.step(&m);
    }
    assert_eq!(s.level(Rule::Area), Some(3), "{}", s.texts());
    assert!(s.texts().contains("P-TEST") && s.texts().contains("TEST SUA 2026-"));
}

#[test]
fn serial_change_and_missing_operator() {
    let mut s = Sim::new(DAY_MS);
    let a = [basic("SN-A"), Message::Location(loc(C.0, C.1, 50.0))];
    for _ in 0..20 {
        s.step(&a);
    }
    assert_eq!(s.level(Rule::NoOperator), Some(1));
    // Same transmitter (MAC) now claims another serial.
    s.step(&[basic("SN-B"), Message::Location(loc(C.0, C.1, 50.0))]);
    let e = s
        .eng
        .entries()
        .iter()
        .find(|e| e.flags.iter().any(|f| f.rule == Rule::SerialChanged));
    assert!(e.is_some(), "{:?}", s.eng.entries());
}

#[test]
fn droneid_without_remote_id() {
    let mut s = Sim::new(DAY_MS);
    let sight = link::DroneIdSighting {
        freq: 57_565,
        rssi: -70,
        snr_db: 12,
        uptime_us: 0,
        record: link::DroneIdRecord::Encrypted {
            key_id: [1, 2, 3, 4],
        },
    };
    for _ in 0..15 {
        s.now += 1000;
        s.tr.ingest_droneid(0, &sight, s.now);
        s.eval();
    }
    assert_eq!(s.level(Rule::NoRemoteId), Some(1), "{}", s.texts());
    for _ in 0..60 {
        s.now += 1000;
        s.tr.ingest_droneid(0, &sight, s.now);
        s.eval();
    }
    assert_eq!(s.level(Rule::NoRemoteId), Some(2));
}

#[test]
fn csv_and_json_have_no_stray_commas() {
    let mut s = Sim::new(NIGHT_MS);
    let m = flight(200.0, 0.02, OperatorLocationType::LiveGnss);
    for _ in 0..35 {
        s.step(&m);
    }
    let pack = Pack::parse(&s.bytes).unwrap();
    let mut csv = heapless::String::<1024>::new();
    text::write_csv_columns(&mut csv, Some(s.entry()), Some(&pack)).unwrap();
    assert_eq!(csv.matches(',').count(), 2, "{csv}");
    assert!(csv.starts_with(",3,"), "{csv}");
    let mut json = heapless::String::<2048>::new();
    text::write_json(&mut json, s.entry(), Some(&pack)).unwrap();
    assert!(
        json.contains("\"level\":3") && json.contains("\"rule\":\"HEIGHT\""),
        "{json}"
    );
    // The height is an Alert: over 600 ft (lower bound) and far at night.
    assert_eq!(s.level(Rule::Height), Some(3));
}

#[test]
fn settings_table_has_a_rule_for_every_row() {
    for r in settings::ROWS {
        assert!(!r.rule.is_empty() && !r.why.is_empty(), "{}", r.name);
    }
    assert_eq!(Settings::default().agl_limit_ft, 400.0);
}

#[test]
fn low_aircraft_over_town_away_from_airports() {
    let mut tr: Tracker<4, 4> = Tracker::default();
    let bytes = pack_bytes();
    let pack = Pack::parse(&bytes).unwrap();
    let mut eng: Engine<8> = Engine::default();
    // Urban (south of 39.85), 600 ft MSL baro: ~170 m above 10 m ground.
    let mut now = 1000;
    for _ in 0..40 {
        now += 1000;
        use adsb::flag::*;
        let r = adsb::Aircraft {
            source: adsb::Source::ModeS,
            icao: 0xA1B2C3,
            flags: (1 << AIRBORNE)
                | (1 << BARO_ALT_VALID)
                | (1 << POSITION_VALID)
                | (1 << UPDATED_POSITION)
                | (1 << UPDATED_BARO_ALT),
            callsign: adsb::Callsign::new("N12345").unwrap(),
            squawk: Some(0o1200),
            category: 1,
            lat_deg: 39.7,
            lon_deg: -75.4,
            baro_alt_ft: 600,
            gnss_alt_ft: 0,
            direction_deg: 90.0,
            speed_kt: 100,
            baro_vrate_fpm: 0,
            gnss_vrate_fpm: 0,
            uat_emergency: 0,
            nicnac: 0,
            acdims: 0,
            version: 2,
            signal_dbm: -60,
            signal_quality: 9,
            frames_per_s: 3,
            es_frames_per_s: 3,
        };
        tr.ingest_aircraft(&r, now);
        eng.update(&tr, Some(&pack), now, Some(DAY_MS + now));
    }
    let e = eng.aircraft(0xA1B2C3).expect("flagged");
    assert_eq!(e.flags[0].rule, Rule::LowAircraft);
    // ~173 m up, +/-90 m baro error, still under 1000 ft (305 m): Warning.
    assert_eq!(e.flags[0].level, 2, "{:?}", e.flags[0]);
}

#[test]
fn compact_item_text() {
    let t = text::item_text(Rule::Height as u8, 0, 2, 280.4);
    assert_eq!(
        t.as_str(),
        "Warning HEIGHT 920 ft AGL (14 CFR 107.51(b) / 49 USC 44809(a)(6))"
    );
    let t = text::item_text(Rule::Area as u8, AreaKind::NationalSecurity as u8, 3, 0.0);
    assert!(t.starts_with("Alert AREA inside Natl security UAS"), "{t}");
}

/// One frame from drone `mac`/`sn` with a fresh location at `agl` m.
pub(crate) fn heard<const T: usize, const A: usize>(tr: &mut Tracker<T, A>, mac: u32, sn: &str, agl: f32, now: u64) {
    let mut o = obs(Transport::WifiBeacon, now);
    o.mac = [0x02, 0, (mac >> 24) as u8, (mac >> 16) as u8, (mac >> 8) as u8, mac as u8];
    let msgs = [basic(sn), Message::Location(loc(C.0 + 0.001, C.1, agl))];
    tr.ingest_messages(&o, &msgs, 0);
}

/// 64 drones with fresh positions against the P4's old 32-entry engine:
/// 63 forged ones (a new identity every second, so never confirmed) and one
/// real drone flying too high. Its sustained Height flag still reaches
/// Warning on time: the churn of forged tracks never restarts its timer.
#[test]
fn sustained_flag_survives_64_drones_in_a_small_engine() {
    let bytes = pack_bytes();
    let pack = Pack::parse(&bytes).unwrap();
    let mut tr: Tracker<64, 32> = Tracker::default();
    let mut eng: Engine<32> = Engine::default();
    let real = flight(160.0, 0.001, OperatorLocationType::LiveGnss);
    let mut now = 1_000;
    let mut fake = 0u32;
    let mut warned_at = None;
    for step in 0..45 {
        now += 1000;
        tr.ingest_messages(&obs(Transport::WifiBeacon, now), &real, 0);
        if step >= 3 {
            for _ in 0..63 {
                heard(&mut tr, 0x100 + fake, &std::format!("FAKE{fake:08}"), 50.0, now);
                fake += 1;
            }
        }
        eng.update(&tr, Some(&pack), now, Some(DAY_MS + now));
        let id = tr.tracks().iter().find(|t| t.uas_ids.iter().any(|u| u.id.as_str() == Some("SN-TEST-1"))).unwrap().id;
        let level = eng.drone(id).and_then(|e| e.flags.iter().filter(|f| f.rule == Rule::Height).map(|f| f.level).max());
        if level == Some(2) {
            warned_at.get_or_insert(step);
        }
    }
    // Same as alone (`height_levels_rise_with_time_and_margin`): Warning
    // after about 30 s of the condition.
    let at = warned_at.expect("the sustained level was reached");
    assert!(at <= 36, "warning at step {at}");
    assert!(tr.len() > 32, "the engine really was outnumbered");
}

/// The P4's engine is sized for every drone and aircraft the tracker can
/// hold (64 + 32): with 63 confirmed drones already flagged-or-not, a real
/// drone arriving last still gets its entry and its Warning on time.
#[test]
fn engine_sized_for_every_track_never_refuses() {
    let bytes = pack_bytes();
    let pack = Pack::parse(&bytes).unwrap();
    let mut tr: Tracker<64, 32> = Tracker::default();
    let mut eng: Engine<96> = Engine::default();
    let real = flight(160.0, 0.001, OperatorLocationType::LiveGnss);
    let mut now = 1_000;
    let mut warned_at = None;
    for step in 0..45 {
        now += 1000;
        for k in 0..63u32 {
            heard(&mut tr, 0x10 + k, &std::format!("OTHER{k:03}"), 50.0, now);
        }
        if step >= 3 {
            tr.ingest_messages(&obs(Transport::WifiBeacon, now), &real, 0);
        }
        eng.update(&tr, Some(&pack), now, Some(DAY_MS + now));
        if let Some(t) = tr.tracks().iter().find(|t| t.uas_ids.iter().any(|u| u.id.as_str() == Some("SN-TEST-1"))) {
            let level = eng.drone(t.id).and_then(|e| e.flags.iter().filter(|f| f.rule == Rule::Height).map(|f| f.level).max());
            if level == Some(2) {
                warned_at.get_or_insert(step);
            }
        }
    }
    assert_eq!(tr.len(), 64);
    assert_eq!(eng.refused, 0);
    assert!(warned_at.expect("reached") <= 3 + 36);
}

/// `size_of` of the engine, for the report (the P4 keeps it in PSRAM).
#[test]
fn engine_sizes() {
    std::println!(
        "Entry {} B, Engine<32> {} B, Engine<96> {} B",
        core::mem::size_of::<Entry>(),
        core::mem::size_of::<Engine<32>>(),
        core::mem::size_of::<Engine<96>>()
    );
}

/// The wide screen's words: metres first, feet where the rule is in feet,
/// and every rule fits its 48 characters.
#[test]
fn screen_words_lead_with_metres() {
    let f = |rule, ev| Flag { rule, area: None, level: 2, ev, src: 0, name: heapless::String::new(), secs: 1.0, no_clock: false };
    let h = crate::text::screen_words(&f(Rule::Height, [260.0, 5.0, 121.92]));
    assert_eq!(h.as_str(), "260 m (853 ft) above ground, limit 400 ft");
    let v = crate::text::screen_words(&f(Rule::Vlos, [640.0, 1.0, 10.0]));
    assert_eq!(v.as_str(), "640 m from its pilot");
    for rule in (0..=20).filter_map(Rule::from_u8) {
        let t = crate::text::screen_words(&f(rule, [9_999.0, 9_999.0, 9_999.0]));
        assert!(!t.is_empty(), "{rule:?}");
    }
}
