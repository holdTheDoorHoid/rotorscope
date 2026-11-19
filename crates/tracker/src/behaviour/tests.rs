//! Scripted flights through the threat-level rules.
//!
//! Each test flies made-up Remote ID drones ([`super::scenario`]) past a
//! protected site or a moving handheld unit, one report a second, through
//! the whole tracker (decode, merge, confirm, path history, rules, level
//! policy), and checks the level a person would see each second. The
//! simulation helper ([`Sim`]) is meant to be reused by the later slices
//! (orbit, loiter, drop, follow).

extern crate std;
use std::vec::Vec;

use rid::{RawMessage, Transport};
use rid::encode;
use rid::message::*;

use super::scenario::{Flight, Sample, offset};
use super::*;
use crate::tests::det;
use crate::{Track, site};

/// The P4 hub's tracker: a path history for every track.
pub(crate) type Tracker = crate::Tracker<64, { crate::aircraft::MAX_AIRCRAFT }, 64>;

/// The protected house (and the fixed unit standing at it).
pub(crate) const SITE: (f64, f64) = (39.95210, -75.16380);
/// The unit's own height above the ellipsoid.
const OWN_ALT: f64 = 10.0;

/// One made-up drone: its serial, how it flies (a plain function of time,
/// seconds from the start), where its pilot stands, how rough its reported
/// positions are (metres, each axis, deterministic), and when it is heard.
pub(crate) struct Drone<'a> {
    pub serial: &'a str,
    pub path: &'a dyn Fn(f64) -> Sample,
    pub pilot: Option<(f64, f64)>,
    pub jitter_m: f64,
    /// Heard from `from_s` until (not including) `until_s`.
    pub from_s: u64,
    pub until_s: u64,
}

impl<'a> Drone<'a> {
    pub fn new(serial: &'a str, path: &'a dyn Fn(f64) -> Sample) -> Self {
        Self { serial, path, pilot: None, jitter_m: 0.0, from_s: 0, until_s: u64::MAX }
    }
}

/// The tracker and a clock, stepped one second at a time.
pub(crate) struct Sim {
    /// Boxed: with the path history the tracker is about 350 KB.
    pub tr: std::boxed::Box<Tracker>,
    /// Seconds since the start.
    pub s: u64,
    /// The unit's own path (`None`: standing at [`SITE`]).
    pub own: Option<std::boxed::Box<dyn Fn(f64) -> Sample>>,
    seed: u32,
}

/// Hub time of second 0 (not zero, so "never" and "at start" differ).
const T0: u64 = 100_000;

impl Sim {
    pub fn new(site_lines: &[&str]) -> Self {
        let mut tr: std::boxed::Box<Tracker> = std::boxed::Box::default();
        let mut st = site::SiteState::default();
        for l in site_lines {
            st.apply(site::parse_line(l).unwrap_or_else(|e| panic!("{l}: {e}")));
        }
        tr.set_places(&st.places);
        Self { tr, s: 0, own: None, seed: 12345 }
    }

    pub fn now(&self) -> u64 {
        T0 + self.s * 1000
    }

    fn noise(&mut self) -> f64 {
        // A fixed pseudo-random sequence in -1..1.
        self.seed = self.seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        f64::from((self.seed >> 8) & 0xFFFF) / 32_768.0 - 1.0
    }

    /// One second: the unit's fix, one report from each drone heard now,
    /// then the per-second tick.
    pub fn step(&mut self, drones: &[Drone<'_>]) {
        let now = self.now();
        let t = self.s as f64;
        match &self.own {
            None => self.tr.set_own_fix(SITE.0, SITE.1, Some(OWN_ALT), now, Some((0.0, None))),
            Some(f) => {
                let o = f(t);
                self.tr.set_own_fix(o.lat, o.lon, Some(OWN_ALT), now, Some((o.speed_mps, Some(o.course_deg))));
            }
        }
        for (i, d) in drones.iter().enumerate() {
            if self.s < d.from_s || self.s >= d.until_s {
                continue;
            }
            let mut p = (d.path)(t);
            if d.jitter_m > 0.0 {
                let (e, n) = (self.noise() * d.jitter_m, self.noise() * d.jitter_m);
                (p.lat, p.lon) = offset(p.lat, p.lon, e, n);
            }
            let mac = [0x60, 0x60, 0x1f, 0, 0, i as u8 + 1];
            let det = det(Transport::WifiBeacon, mac, -60, &frame(d.serial, &p, d.pilot));
            self.tr.ingest(0, &det, now);
        }
        self.tr.expire(now);
        self.tr.tick_behaviour(now);
        self.s += 1;
    }

    /// Run until second `to` (exclusive), calling `each` after every second.
    pub fn run(&mut self, drones: &[Drone<'_>], to: u64, mut each: impl FnMut(&Sim)) {
        while self.s < to {
            self.step(drones);
            each(self);
        }
    }

    pub fn track(&self, serial: &str) -> Option<&Track> {
        self.tr.tracks().iter().find(|t| t.primary_id().and_then(|u| u.id.as_str()) == Some(serial))
    }

    pub fn level(&self, serial: &str) -> Option<Level> {
        self.track(serial).and_then(|t| t.threat.level)
    }

    pub fn rules(&self, serial: &str) -> Vec<Rule> {
        self.track(serial).map_or(Vec::new(), |t| t.threat.reasons.iter().map(|r| r.rule).collect())
    }
}

/// One Remote ID report: serial, location (with speed and direction) and,
/// when given, the pilot's position.
pub(crate) fn frame(serial: &str, p: &Sample, pilot: Option<(f64, f64)>) -> std::vec::Vec<RawMessage> {
    let mut v = std::vec![
        encode::message(&Message::BasicId(BasicId {
            id_type: IdType::SerialNumber,
            ua_type: UaType::HelicopterOrMultirotor,
            uas_id: FixedStr::new(serial.as_bytes()).unwrap(),
        }))
        .unwrap(),
        encode::message(&Message::Location(Location {
            status: Status::Airborne,
            height_type: HeightType::AboveTakeoff,
            direction: Some(p.course_deg),
            speed_horizontal: Some(p.speed_mps),
            speed_vertical: Some(p.vspeed_mps),
            position: Some(LatLon { lat: p.lat, lon: p.lon }),
            altitude_pressure: None,
            altitude_geodetic: Some(p.alt_m),
            height: Some(p.alt_m - OWN_ALT as f32),
            horizontal_accuracy: HorizontalAccuracy::M10,
            vertical_accuracy: VerticalAccuracy::M10,
            baro_accuracy: VerticalAccuracy::M10,
            speed_accuracy: SpeedAccuracy::Mps1,
            timestamp: None,
            timestamp_accuracy: None,
        }))
        .unwrap(),
    ];
    if let Some((lat, lon)) = pilot {
        v.push(
            encode::message(&Message::System(System {
                operator_location_type: OperatorLocationType::Takeoff,
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
            }))
            .unwrap(),
        );
    }
    v
}

const HOUSE: &str = "area house circle 39.95210 -75.16380 150";

/// The highest level seen while running.
fn worst(sim: &mut Sim, drones: &[Drone<'_>], to: u64, serial: &str) -> Option<Level> {
    let mut w = None;
    sim.run(drones, to, |s| w = w.max(s.level(serial)));
    w
}

// ------------------------------------------------------------------ 16a --

#[test]
fn beeline_at_a_fixed_site_warns_then_goes_critical() {
    let from = offset(SITE.0, SITE.1, 0.0, -1200.0);
    let path = move |t: f64| Flight::Toward { from, to: SITE, speed_mps: 12.0, alt_from: 60.0, alt_to: 40.0 }.at(t);
    let drones = [Drone::new("SN-BEE", &path)];
    let mut sim = Sim::new(&[HOUSE]);
    let mut first_warning = None;
    let mut first_critical = None;
    sim.run(&drones, 90, |s| {
        let l = s.level("SN-BEE");
        if l == Some(Level::Warning) && first_warning.is_none() {
            first_warning = Some(s.s);
            assert_eq!(s.rules("SN-BEE")[0], Rule::Beeline);
        }
        if l == Some(Level::Critical) && first_critical.is_none() {
            first_critical = Some(s.s);
        }
    });
    let w = first_warning.expect("never warned");
    let c = first_critical.expect("never critical");
    // Warning once it is predicted to reach the fence within a minute
    // (1050 m to go at 12 m/s: from second ~28) and its path has been
    // straight for 10 s; Critical once under 30 s from the fence
    // (360 m out: second ~58).
    assert!((26..=34).contains(&w), "warning at {w}");
    assert!((56..=62).contains(&c), "critical at {c}");
    // The rises were announced in order, once each.
    let mut rises = Vec::new();
    while let Some(r) = sim.tr.take_level_rise(sim.now()) {
        rises.push(r.level);
    }
    assert_eq!(rises, [Level::Critical], "only the latest rise is pending");
}

#[test]
fn every_rise_is_announced_once() {
    let from = offset(SITE.0, SITE.1, 0.0, -1200.0);
    let path = move |t: f64| Flight::Toward { from, to: SITE, speed_mps: 12.0, alt_from: 60.0, alt_to: 40.0 }.at(t);
    let drones = [Drone::new("SN-BEE", &path)];
    let mut sim = Sim::new(&[HOUSE]);
    let mut rises = Vec::new();
    while sim.s < 90 {
        sim.step(&drones);
        while let Some(r) = sim.tr.take_level_rise(sim.now()) {
            let t = sim.tr.get(r.track).unwrap();
            let text = describe(t, sim.now());
            let data = rise_data(t, &r);
            assert_eq!(data[0], r.level as u8);
            assert_eq!(u32::from_le_bytes(data[1..5].try_into().unwrap()), r.track.0);
            rises.push((r.level, text));
        }
    }
    let levels: Vec<Level> = rises.iter().map(|r| r.0).collect();
    assert_eq!(levels, [Level::Warning, Level::Critical], "{rises:?}");
    assert!(rises[0].1.starts_with("Warning: drone SN-BEE heading for house, "), "{}", rises[0].1);
    assert!(rises[1].1.starts_with("Critical: drone SN-BEE FAST, heading for house"), "{}", rises[1].1);
    assert!(rises[1].1.contains("there in about"), "{}", rises[1].1);
}

#[test]
fn fast_fpv_dive_is_critical_well_before_it_arrives() {
    let from = offset(SITE.0, SITE.1, 2000.0, 0.0);
    let path = move |t: f64| Flight::Toward { from, to: SITE, speed_mps: 40.0, alt_from: 300.0, alt_to: 20.0 }.at(t);
    let drones = [Drone::new("FPV-1", &path)];
    let mut sim = Sim::new(&[HOUSE]);
    let mut critical_at = None;
    sim.run(&drones, 40, |s| {
        if critical_at.is_none() && s.level("FPV-1") == Some(Level::Critical) {
            critical_at = Some(s.s);
            let r = &s.track("FPV-1").unwrap().threat.reasons[0];
            assert_eq!(r.rule, Rule::FastInbound);
            assert!(r.speed_mps().unwrap() > FAST_MPS, "{r:?}");
            assert!(r.range_m() > 1000.0, "{r:?}");
        }
    });
    // Fast drones need only 5 s of straight path.
    let c = critical_at.expect("never critical");
    assert!(c <= 10, "critical at {c}");
}

#[test]
fn delivery_drone_flying_straight_past_stays_notice() {
    let from = offset(SITE.0, SITE.1, -1500.0, 400.0);
    let path = move |t: f64| Flight::Line { from, course_deg: 90.0, speed_mps: 15.0, alt_m: 100.0 }.at(t);
    let drones = [Drone { jitter_m: 8.0, ..Drone::new("DLV-1", &path) }];
    let mut sim = Sim::new(&[HOUSE]);
    assert_eq!(worst(&mut sim, &drones, 200, "DLV-1"), Some(Level::Notice));
}

#[test]
fn estate_photographer_orbiting_just_outside_does_not_warn() {
    // A neighbour's house 260 m east; the drone circles it at 60 m,
    // coming within 50 m of our fence; its pilot stands at that house.
    let next_door = offset(SITE.0, SITE.1, 260.0, 0.0);
    let path = move |t: f64| Flight::Orbit { centre: next_door, radius_m: 60.0, speed_mps: 6.0, alt_m: 50.0, clockwise: true }.at(t);
    let drones = [Drone { pilot: Some(next_door), jitter_m: 5.0, ..Drone::new("PHOTO", &path) }];
    let mut sim = Sim::new(&[HOUSE]);
    let w = worst(&mut sim, &drones, 300, "PHOTO");
    assert!(w < Some(Level::Warning), "{w:?}");
    assert_eq!(w, Some(Level::Notice));
}

#[test]
fn hovering_inside_with_the_pilot_outside_is_two_signs() {
    let over = offset(SITE.0, SITE.1, 40.0, 30.0);
    let path = move |_t: f64| Flight::Hover { lat: over.0, lon: over.1, alt_m: 50.0 }.at(0.0);
    let pilot = offset(SITE.0, SITE.1, -400.0, 0.0);
    // No pilot position: rule 1 alone, Caution.
    let mut sim = Sim::new(&[HOUSE]);
    sim.run(&[Drone::new("HOV-1", &path)], 30, |_| {});
    assert_eq!(sim.level("HOV-1"), Some(Level::Caution));
    assert_eq!(sim.rules("HOV-1"), [Rule::InArea]);
    // Pilot 250 m beyond the fence: rule 3 adds to rule 1, Warning.
    let mut sim = Sim::new(&[HOUSE]);
    sim.run(&[Drone { pilot: Some(pilot), ..Drone::new("HOV-1", &path) }], 30, |_| {});
    let t = sim.track("HOV-1").unwrap();
    assert_eq!(t.threat.level, Some(Level::Warning));
    assert!(t.threat.combined);
    assert_eq!(sim.rules("HOV-1"), [Rule::PilotOutside, Rule::InArea]);
    let r = &t.threat.reasons[0];
    assert!((r.range_m() - 250.0).abs() < 5.0, "{r:?}");
    let text = describe(t, sim.now());
    assert!(text.contains("pilot outside, 250 m beyond the edge of house; over house; two signs at once"), "{text}");
}

#[test]
fn above_the_ceiling_counts_inside_only() {
    let over = offset(SITE.0, SITE.1, 0.0, 60.0);
    let high = move |_t: f64| Flight::Hover { lat: over.0, lon: over.1, alt_m: 200.0 }.at(0.0);
    let mut sim = Sim::new(&[HOUSE, "area house ceiling 150"]);
    sim.run(&[Drone::new("HIGH", &high)], 20, |_| {});
    // 190 m above the unit: over the 150 m ceiling, and in the area.
    assert_eq!(sim.rules("HIGH"), [Rule::AboveCeiling, Rule::InArea]);
    assert_eq!(sim.level("HIGH"), Some(Level::Warning));
    assert!((sim.track("HIGH").unwrap().threat.reasons[0].height_m().unwrap() - 190.0).abs() < 1.0);
    let mut sim = Sim::new(&[HOUSE, "area house ceiling 250"]);
    sim.run(&[Drone::new("HIGH", &high)], 20, |_| {});
    assert_eq!(sim.rules("HIGH"), [Rule::InArea]);
}

#[test]
fn pilot_standing_at_the_fence_is_caution() {
    let far = offset(SITE.0, SITE.1, 0.0, 800.0);
    let path = move |_t: f64| Flight::Hover { lat: far.0, lon: far.1, alt_m: 60.0 }.at(0.0);
    let fence = offset(SITE.0, SITE.1, 165.0, 0.0);
    let mut sim = Sim::new(&[HOUSE]);
    sim.run(&[Drone { pilot: Some(fence), ..Drone::new("P-1", &path) }], 20, |_| {});
    assert_eq!(sim.rules("P-1"), [Rule::PilotNear]);
    assert_eq!(sim.level("P-1"), Some(Level::Caution));
}

#[test]
fn bubble_uses_relative_motion() {
    // A walker heading east at 1.5 m/s with a 300 m bubble.
    let start = offset(SITE.0, SITE.1, -600.0, 0.0);
    let walk = move |t: f64| Flight::Line { from: start, course_deg: 90.0, speed_mps: 1.5, alt_m: OWN_ALT as f32 }.at(t);
    // (a) A drone hovering 150 m north of the path: the walker passes it.
    let hover_at = offset(SITE.0, SITE.1, 0.0, 150.0);
    let hover = move |_t: f64| Flight::Hover { lat: hover_at.0, lon: hover_at.1, alt_m: 40.0 }.at(0.0);
    let mut sim = Sim::new(&["bubble 300"]);
    sim.own = Some(std::boxed::Box::new(walk));
    let mut seen_in = false;
    let mut worst_l = None;
    sim.run(&[Drone::new("HOVER", &hover)], 800, |s| {
        worst_l = worst_l.max(s.level("HOVER"));
        seen_in |= s.rules("HOVER").contains(&Rule::InArea);
    });
    assert!(seen_in, "the hovering drone came inside the bubble");
    assert_eq!(worst_l, Some(Level::Caution), "walking past a hovering drone is not an approach");
    // (b) A drone flying at the walker: from 1 km north of where the
    // walker will be at 60 s, south at 10 m/s.
    let meet = offset(SITE.0, SITE.1, -600.0 + 1.5 * 100.0, 0.0);
    let from = offset(meet.0, meet.1, 0.0, 1000.0);
    let attack = move |t: f64| Flight::Line { from, course_deg: 180.0, speed_mps: 10.0, alt_m: 40.0 }.at(t);
    let mut sim = Sim::new(&["bubble 300"]);
    sim.own = Some(std::boxed::Box::new(walk));
    let mut first_warning = None;
    sim.run(&[Drone::new("ATK", &attack)], 90, |s| {
        if first_warning.is_none() && s.level("ATK") >= Some(Level::Warning) {
            first_warning = Some(s.s);
            let r = &s.track("ATK").unwrap().threat.reasons[0];
            assert!(r.is_bubble(), "{r:?}");
            assert!(matches!(r.rule, Rule::Beeline | Rule::FastInbound));
        }
    });
    // 700 m to the bubble's edge at 10 m/s: within a minute from about
    // second 10, once 10 s of path are in.
    let w = first_warning.expect("never warned");
    assert!((10..=16).contains(&w), "warning at {w}");
}

#[test]
fn a_bubble_on_a_moving_unit_sees_a_drone_it_drives_at_as_no_beeline() {
    // Driving at 15 m/s straight at a hovering drone: closing fast, but the
    // drone is not flying anywhere; it gets Caution when inside the bubble.
    let start = offset(SITE.0, SITE.1, -1500.0, 0.0);
    let drive = move |t: f64| Flight::Line { from: start, course_deg: 90.0, speed_mps: 15.0, alt_m: OWN_ALT as f32 }.at(t);
    let hover = move |_t: f64| Flight::Hover { lat: SITE.0, lon: SITE.1, alt_m: 50.0 }.at(0.0);
    let mut sim = Sim::new(&["bubble 300"]);
    sim.own = Some(std::boxed::Box::new(drive));
    assert_eq!(worst(&mut sim, &[Drone::new("H2", &hover)], 150, "H2"), Some(Level::Caution));
}

#[test]
fn friends_are_lowered_never_hidden() {
    let over = offset(SITE.0, SITE.1, 30.0, 0.0);
    let path = move |_t: f64| Flight::Hover { lat: over.0, lon: over.1, alt_m: 50.0 }.at(0.0);
    let pilot = offset(SITE.0, SITE.1, -400.0, 0.0);
    let friend = "friend OUR-MINI our own Mini";
    let mut sim = Sim::new(&[HOUSE, friend]);
    sim.run(&[Drone { pilot: Some(pilot), ..Drone::new("OUR-MINI", &path) }], 30, |_| {});
    let t = sim.track("OUR-MINI").unwrap();
    assert_eq!(t.threat.level, Some(Level::Notice), "{:?}", t.threat);
    assert!(t.threat.friend);
    // The reasons are still there, lowered.
    assert!(t.threat.reasons.iter().all(|r| r.level == Level::Notice && r.rule.is_place_rule()));
    assert!(describe(t, sim.now()).starts_with("Notice: drone OUR-MINI (friend) "));
    // The same serial diving in fast is still Critical.
    let from = offset(SITE.0, SITE.1, 2000.0, 0.0);
    let dive = move |t: f64| Flight::Toward { from, to: SITE, speed_mps: 40.0, alt_from: 200.0, alt_to: 20.0 }.at(t);
    let mut sim = Sim::new(&[HOUSE, friend]);
    assert_eq!(worst(&mut sim, &[Drone::new("OUR-MINI", &dive)], 30, "OUR-MINI"), Some(Level::Critical));
    assert!(sim.track("OUR-MINI").unwrap().threat.friend);
}

#[test]
fn position_jitter_does_not_flicker_levels() {
    // Hovering just outside the fence (160 m out, 150 m fence) with
    // positions off by up to 15 m each way, independently every second (a
    // harsh model: real receivers' errors drift slowly). Now and then the
    // average lands inside and it is Caution, but the level never goes up
    // and down quicker than the 30 s hold, and never above Caution.
    let edge = offset(SITE.0, SITE.1, 160.0, 0.0);
    let path = move |_t: f64| Flight::Hover { lat: edge.0, lon: edge.1, alt_m: 50.0 }.at(0.0);
    let drones = [Drone { jitter_m: 15.0, ..Drone::new("JIT", &path) }];
    let mut sim = Sim::new(&[HOUSE]);
    let mut changes: Vec<(u64, Option<Level>)> = Vec::new();
    sim.run(&drones, 300, |s| {
        let l = s.level("JIT");
        if changes.last().map(|c| c.1) != Some(l) {
            changes.push((s.s, l));
        }
    });
    assert!(changes.iter().all(|c| c.1 <= Some(Level::Caution)), "jitter never makes an approach: {changes:?}");
    for w in changes.windows(2).skip(1) {
        assert!(w[1].0 - w[0].0 >= DECAY_MS / 1000, "{changes:?}");
    }
    assert!(changes.len() <= 6, "{changes:?}");
    // Just inside the fence (145 m): Caution from the start, and it stays.
    let edge = offset(SITE.0, SITE.1, 0.0, 145.0);
    let path = move |_t: f64| Flight::Hover { lat: edge.0, lon: edge.1, alt_m: 50.0 }.at(0.0);
    let drones = [Drone { jitter_m: 15.0, ..Drone::new("JIT3", &path) }];
    let mut sim = Sim::new(&[HOUSE]);
    let mut levels = Vec::new();
    sim.run(&drones, 300, |s| {
        let l = s.level("JIT3");
        if levels.last() != Some(&l) {
            levels.push(l);
        }
    });
    assert_eq!(levels, [None, Some(Level::Caution)], "only up, once (None before it is confirmed)");
    // Further out, the same roughness never even reaches Caution.
    let far = offset(SITE.0, SITE.1, 260.0, 0.0);
    let path = move |_t: f64| Flight::Hover { lat: far.0, lon: far.1, alt_m: 50.0 }.at(0.0);
    let mut sim = Sim::new(&[HOUSE]);
    let drones = [Drone { jitter_m: 30.0, ..Drone::new("JIT2", &path) }];
    assert_eq!(worst(&mut sim, &drones, 300, "JIT2"), Some(Level::Notice));
}

#[test]
fn a_single_wild_position_does_not_raise_the_level() {
    // One report 120 m off (into the area), the rest steady 60 m outside.
    let out = offset(SITE.0, SITE.1, 210.0, 0.0);
    let wild = offset(SITE.0, SITE.1, 90.0, 0.0);
    let path = move |t: f64| {
        let p = if (40.0..41.0).contains(&t) { wild } else { out };
        Flight::Hover { lat: p.0, lon: p.1, alt_m: 50.0 }.at(0.0)
    };
    let mut sim = Sim::new(&[HOUSE]);
    assert_eq!(worst(&mut sim, &[Drone::new("ONE", &path)], 80, "ONE"), Some(Level::Notice));
}

#[test]
fn levels_come_down_after_thirty_seconds_clear() {
    // Hovers inside for a minute, then flies away north at 8 m/s: out of
    // the 150 m circle at about second 75.
    let over = offset(SITE.0, SITE.1, 0.0, 50.0);
    let away = offset(SITE.0, SITE.1, 0.0, 2000.0);
    let path = move |t: f64| {
        if t < 60.0 {
            Flight::Hover { lat: over.0, lon: over.1, alt_m: 50.0 }.at(t)
        } else {
            Flight::Toward { from: over, to: away, speed_mps: 8.0, alt_from: 50.0, alt_to: 50.0 }.at(t - 60.0)
        }
    };
    let mut sim = Sim::new(&[HOUSE]);
    let mut out_at = None;
    let mut notice_at = None;
    sim.run(&[Drone::new("LEAVE", &path)], 130, |s| {
        if out_at.is_none() && s.s > 60 && !s.rules("LEAVE").contains(&Rule::InArea) && s.level("LEAVE") == Some(Level::Caution) {
            // Held at Caution although the rule no longer holds.
        }
        let t = s.track("LEAVE").unwrap();
        if out_at.is_none() && s.s > 60 && t.threat.level == Some(Level::Caution) {
            let here = t.location.as_ref().unwrap().value.position.unwrap();
            if crate::geo::range_bearing(SITE.0, SITE.1, here.lat, here.lon).distance_m > 160.0 {
                out_at = Some(s.s);
            }
        }
        if notice_at.is_none() && s.s > 60 && t.threat.level == Some(Level::Notice) {
            notice_at = Some(s.s);
        }
    });
    let out = out_at.expect("held at Caution after leaving");
    let n = notice_at.expect("came down");
    assert!(n >= 100 && n <= 110, "out {out}, notice {n}");
}

#[test]
fn a_drone_lost_while_inbound_keeps_its_level() {
    let from = offset(SITE.0, SITE.1, 0.0, -1500.0);
    let path = move |t: f64| Flight::Toward { from, to: SITE, speed_mps: 30.0, alt_from: 80.0, alt_to: 20.0 }.at(t);
    // Heard for 20 s (Critical by then), then lost behind trees.
    let drones = [Drone { until_s: 20, ..Drone::new("LOST", &path) }];
    let mut sim = Sim::new(&[HOUSE]);
    sim.run(&drones, 20, |_| {});
    assert_eq!(sim.level("LOST"), Some(Level::Critical));
    sim.run(&drones, 30, |_| {});
    let t = sim.track("LOST").unwrap();
    assert_eq!(t.threat.level, Some(Level::Critical));
    assert!(t.threat.held);
    let r = &t.threat.reasons[0];
    assert_eq!(r.rule, Rule::LastSeenInbound);
    assert_eq!(r.ago_s(), Some(10), "last heard at second 19, judged at 29: {r:?}");
    assert!(describe(t, sim.now()).contains("last seen heading for house"), "{}", describe(t, sim.now()));
    sim.run(&drones, 52, |_| {});
    assert_eq!(sim.level("LOST"), Some(Level::Notice), "held 30 s, then down");
    assert!(sim.rules("LOST").is_empty());
    // A drone lost while not heading anywhere is not held as inbound.
    let over = offset(SITE.0, SITE.1, 0.0, 50.0);
    let hover = move |_t: f64| Flight::Hover { lat: over.0, lon: over.1, alt_m: 50.0 }.at(0.0);
    let mut sim = Sim::new(&[HOUSE]);
    let drones = [Drone { until_s: 20, ..Drone::new("GONE", &hover) }];
    sim.run(&drones, 30, |_| {});
    assert!(!sim.track("GONE").unwrap().threat.held);
}

#[test]
fn snooze_is_per_drone_timed_and_still_logged() {
    let from = offset(SITE.0, SITE.1, 0.0, -1200.0);
    let path = move |t: f64| Flight::Toward { from, to: SITE, speed_mps: 12.0, alt_from: 60.0, alt_to: 40.0 }.at(t);
    let drones = [Drone::new("SNZ", &path)];
    let mut sim = Sim::new(&[HOUSE]);
    sim.run(&drones, 40, |_| {});
    assert_eq!(sim.level("SNZ"), Some(Level::Warning));
    while sim.tr.take_level_rise(sim.now()).is_some() {}
    assert_eq!(sim.tr.snoozable_levels(sim.now()), 1);
    assert!(sim.tr.top_threat(sim.now()).is_some());
    let ids = sim.tr.snooze_levels(sim.now());
    assert_eq!(ids.len(), 1);
    assert_eq!(sim.tr.snoozable_levels(sim.now()), 0);
    assert!(sim.tr.top_threat(sim.now()).is_none(), "the strip leaves a snoozed drone out");
    sim.run(&drones, 70, |_| {});
    // Still judged and logged: the Critical rise is there, marked snoozed.
    assert_eq!(sim.level("SNZ"), Some(Level::Critical));
    let r = sim.tr.take_level_rise(sim.now()).unwrap();
    assert!(r.snoozed);
    let t = sim.tr.get(r.track).unwrap();
    assert_eq!(rise_data(t, &r)[9] & 1, 1);
    assert!(t.threat.snooze_left_s(sim.now()).unwrap() > 200);
    // Five minutes later the snooze is over by itself.
    assert!(!t.threat.snoozed(sim.now() + SNOOZE_MS));
}

#[test]
fn drones_without_a_position_or_unconfirmed_get_no_level() {
    let mut tr: std::boxed::Box<Tracker> = std::boxed::Box::default();
    let mut st = site::SiteState::default();
    st.apply(site::parse_line(HOUSE).unwrap());
    tr.set_places(&st.places);
    tr.set_own_fix(SITE.0, SITE.1, None, T0, Some((0.0, None)));
    // Only a Basic ID: no position.
    let id = encode::message(&Message::BasicId(BasicId {
        id_type: IdType::SerialNumber,
        ua_type: UaType::HelicopterOrMultirotor,
        uas_id: FixedStr::new(b"NOPOS").unwrap(),
    }))
    .unwrap();
    for k in 0..5 {
        tr.ingest(0, &det(Transport::BleLegacy, [0xc0, 1, 2, 3, 4, 5], -70, &[id]), T0 + k * 1000);
        tr.tick_behaviour(T0 + k * 1000);
    }
    assert_eq!(tr.tracks()[0].threat.level, None);
    // One report inside the area: not confirmed yet, not judged.
    let p = Flight::Hover { lat: SITE.0, lon: SITE.1, alt_m: 30.0 }.at(0.0);
    let mut tr2: std::boxed::Box<Tracker> = std::boxed::Box::default();
    let places = tr.places().clone();
    tr2.set_places(&places);
    tr2.ingest(0, &det(Transport::WifiBeacon, [0x60, 0, 0, 0, 0, 9], -60, &frame("ONCE", &p, None)), T0);
    tr2.tick_behaviour(T0);
    assert_eq!(tr2.tracks()[0].threat.level, None);
    assert!(tr2.take_level_rise(T0).is_none());
}

#[test]
fn own_speed_comes_from_the_receiver_or_from_fixes() {
    let mut tr: std::boxed::Box<Tracker> = std::boxed::Box::default();
    // Walking east at 2 m/s, fixes once a second, no speed from the receiver.
    for k in 0..10u64 {
        let (lat, lon) = offset(SITE.0, SITE.1, 2.0 * k as f64, 0.0);
        tr.set_own_position(lat, lon, None, T0 + k * 1000);
    }
    let o = tr.own_position(T0 + 9000).unwrap();
    assert!((o.speed_mps.unwrap() - 2.0).abs() < 0.1, "{o:?}");
    assert!((o.course_deg.unwrap() - 90.0).abs() < 2.0, "{o:?}");
    let (e, n) = o.velocity();
    assert!((e - 2.0).abs() < 0.1 && n.abs() < 0.1);
    // Standing still: speed 0, no course.
    for k in 10..20u64 {
        tr.set_own_position(SITE.0, SITE.1 + 0.0002, None, T0 + k * 1000);
    }
    let o = tr.own_position(T0 + 19_000).unwrap();
    assert_eq!((o.speed_mps, o.course_deg), (Some(0.0), None));
    assert_eq!(o.velocity(), (0.0, 0.0));
    // The receiver's own figures win.
    tr.set_own_fix(SITE.0, SITE.1, None, T0 + 20_000, Some((12.5, Some(271.0))));
    let o = tr.own_position(T0 + 20_000).unwrap();
    assert_eq!((o.speed_mps, o.course_deg), (Some(12.5), Some(271.0)));
    // A crawl below 0.5 m/s is standing still.
    tr.set_own_fix(SITE.0, SITE.1, None, T0 + 21_000, Some((0.3, Some(10.0))));
    assert_eq!(tr.own_position(T0 + 21_000).unwrap().course_deg, None);
}

#[test]
fn path_history_is_kept_per_track() {
    let from = offset(SITE.0, SITE.1, 0.0, -1200.0);
    let path = move |t: f64| Flight::Toward { from, to: SITE, speed_mps: 12.0, alt_from: 60.0, alt_to: 40.0 }.at(t);
    let mut sim = Sim::new(&[]);
    sim.run(&[Drone::new("PATH", &path)], 150, |_| {});
    let id = sim.track("PATH").unwrap().id;
    let r = sim.tr.path(id).unwrap();
    assert_eq!(r.fine_len(), path::FINE_POINTS);
    assert_eq!(r.coarse_len(), 30);
    // No places: every drone with a position is Notice.
    assert_eq!(sim.level("PATH"), Some(Level::Notice));
}

#[test]
fn the_tracker_with_path_history_stays_under_a_mebibyte() {
    use core::mem::size_of;
    let hub = size_of::<Tracker>();
    std::println!(
        "Tracker<64,{},64> {} B (path store {} B, PathRing {} B, TrackThreat {} B, Places {} B, Track {} B)",
        crate::aircraft::MAX_AIRCRAFT,
        hub,
        size_of::<PathStore<64>>(),
        size_of::<PathRing>(),
        size_of::<TrackThreat>(),
        size_of::<Places>(),
        size_of::<Track>(),
    );
    assert!(hub < 1 << 20, "{hub}");
    assert!(size_of::<PathRing>() <= 4_500);
    assert_eq!(size_of::<PathPoint>(), 12);
}

#[test]
fn level_and_reasons_reach_json_cot_and_the_strip() {
    let from = offset(SITE.0, SITE.1, 0.0, -1200.0);
    let path = move |t: f64| Flight::Toward { from, to: SITE, speed_mps: 12.0, alt_from: 60.0, alt_to: 40.0 }.at(t);
    let mut sim = Sim::new(&[HOUSE]);
    sim.run(&[Drone::new("OUT-1", &path)], 65, |_| {});
    let now = sim.now() - 1000;
    let t = sim.track("OUT-1").unwrap();
    assert_eq!(t.threat.level, Some(Level::Critical));
    let ctx = sim.tr.context(now, Some(1_700_000_000_000));
    let mut buf = [0u8; crate::json::MAX_LINE];
    let n = crate::json::track_line(t, &ctx, &mut buf).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&buf[..n]).unwrap();
    let th = &v["threat"];
    assert_eq!(th["level"], "critical");
    assert_eq!(th["held"], false);
    assert_eq!(th["snoozed_s"], serde_json::Value::Null);
    assert_eq!(th["reasons"][0]["rule"], "fast_inbound");
    assert_eq!(th["reasons"][0]["place"], "house");
    assert!(th["reasons"][0]["eta_s"].as_f64().unwrap() < 30.0);
    assert!(th["reasons"][0].get("h_m").is_none(), "only the numbers the rule has");
    assert_eq!(th["reasons"][1]["rule"], "beeline");
    let mut cot = [0u8; crate::cot::MAX_EVENT];
    let n = crate::cot::drone_event(t, &ctx, &mut cot).unwrap();
    let x = core::str::from_utf8(&cot[..n]).unwrap();
    assert!(x.contains("; Threat: Critical: FAST, heading for house, "), "{x}");
    assert!(x.contains("<color argb=\"-65536\"/>"), "{x}");
    let strip = strip_text(t).unwrap();
    assert!(strip.starts_with("CRITICAL OUT-1 FAST to house "), "{strip}");
    assert_eq!(sim.tr.top_threat(now).map(|t| t.id), Some(t.id));
}
