//! Scripted flights for the lingering rules (slice 16b, rules 7-11):
//! loitering, circling, low hover, the drop pattern and following, each
//! with the false alarms of THREAT-TACTICS.md §6 it must not raise, and
//! the level coming down afterwards. Uses the 16a simulation ([`Sim`]).

extern crate std;
use std::vec::Vec;

use super::scenario::{Flight, Sample, offset};
use super::tests::{Drone, HOUSE, OWN_ALT, SITE, Sim};
use super::*;

/// Each second's level and rules for one drone.
struct Seen(Vec<(u64, Option<Level>, Vec<Rule>)>);

impl Seen {
    fn run(sim: &mut Sim, drones: &[Drone<'_>], to: u64, serial: &str) -> Self {
        let mut v = Vec::new();
        sim.run(drones, to, |s| v.push((s.s, s.level(serial), s.rules(serial))));
        Self(v)
    }

    fn first(&self, rule: Rule) -> Option<u64> {
        self.0.iter().find(|e| e.2.contains(&rule)).map(|e| e.0)
    }

    fn last(&self, rule: Rule) -> Option<u64> {
        self.0.iter().rev().find(|e| e.2.contains(&rule)).map(|e| e.0)
    }

    fn ever(&self, rule: Rule) -> bool {
        self.first(rule).is_some()
    }

    /// The first second at `level` or above.
    fn reached(&self, level: Level) -> Option<u64> {
        self.0.iter().find(|e| e.1 >= Some(level)).map(|e| e.0)
    }

    fn worst(&self) -> Option<Level> {
        self.0.iter().map(|e| e.1).max().flatten()
    }

    /// What was seen at second `s` (the first entry is second 1).
    fn entry(&self, s: u64) -> &(u64, Option<Level>, Vec<Rule>) {
        &self.0[s as usize - 1]
    }

    /// The level at second `s`.
    fn at(&self, s: u64) -> Option<Level> {
        self.0.iter().find(|e| e.0 == s).and_then(|e| e.1)
    }
}

/// The height above the unit of a flight altitude (the unit stands at
/// [`OWN_ALT`]).
fn up(m: f32) -> f32 {
    m + OWN_ALT as f32
}

fn at_site(e: f64, n: f64) -> (f64, f64) {
    offset(SITE.0, SITE.1, e, n)
}

// ---------------------------------------------------------- 8 circling --

#[test]
fn orbit_over_the_area_is_circling_and_with_in_area_a_warning() {
    // Round the house, 80 m out, at 6 m/s (a turn in 84 s), 50 m up; then
    // off north at 10 m/s from second 240.
    let centre = at_site(20.0, 0.0);
    let orbit = Flight::Orbit { centre, radius_m: 80.0, speed_mps: 6.0, alt_m: up(50.0), clockwise: true };
    let left = orbit.at(240.0);
    let away = at_site(20.0, 3000.0);
    let path = move |t: f64| {
        if t < 240.0 {
            orbit.at(t)
        } else {
            Flight::Toward { from: (left.lat, left.lon), to: away, speed_mps: 10.0, alt_from: up(50.0), alt_to: up(50.0) }.at(t - 240.0)
        }
    };
    let mut sim = Sim::new(&[HOUSE]);
    let seen = Seen::run(&mut sim, &[Drone { jitter_m: 5.0, ..Drone::new("ORB", &path) }], 340, "ORB");
    let c = seen.first(Rule::Circling).expect("never circling");
    assert!((84..=105).contains(&c), "circling from {c}");
    // In the area too: two signs, Warning.
    let (_, level, rules) = seen.entry(c);
    assert_eq!(*level, Some(Level::Warning), "{rules:?}");
    assert!(rules.contains(&Rule::InArea), "{rules:?}");
    assert_eq!(seen.reached(Level::Warning), Some(c), "Caution (in the area) until the circle is complete");
    let t = sim.track("ORB").unwrap();
    // Gone now; while circling the reason said how wide and how long.
    assert!(!t.threat.reasons.iter().any(|r| r.rule == Rule::Circling));
    // It stops counting soon after it flies off (about one radius); the
    // reason is still shown for the 30 s the level is held, then it comes
    // down.
    let last = seen.last(Rule::Circling).unwrap();
    assert!((265..=300).contains(&last), "circling shown until {last}");
    assert!(seen.at(last + 1) <= Some(Level::Caution), "{:?}", &seen.0[last as usize - 1..]);
    assert_eq!(seen.at(339), Some(Level::Notice));
}

#[test]
fn circling_numbers_and_circling_round_a_small_area_from_outside() {
    // A shed 30 m across the radius; the drone circles 100 m out round a
    // point 40 m from it, so it never flies over the shed but its circle
    // takes the shed in: Caution, one sign.
    let shed = "area shed circle 39.95210 -75.16380 30";
    let centre = at_site(40.0, 0.0);
    let path = move |t: f64| Flight::Orbit { centre, radius_m: 100.0, speed_mps: 8.0, alt_m: up(60.0), clockwise: false }.at(t);
    let mut sim = Sim::new(&[shed]);
    let seen = Seen::run(&mut sim, &[Drone { jitter_m: 5.0, ..Drone::new("RING", &path) }], 150, "RING");
    assert!(seen.ever(Rule::Circling));
    assert!(!seen.ever(Rule::InArea));
    assert_eq!(seen.worst(), Some(Level::Caution));
    let t = sim.track("RING").unwrap();
    assert_eq!(sim.rules("RING"), [Rule::Circling]);
    let r = &t.threat.reasons[0];
    assert!((r.range_m() - 100.0).abs() < 10.0, "radius {r:?}");
    // A turn of 628 m at 8 m/s: about 79 s.
    assert!((70..=90).contains(&r.for_s().unwrap()), "{r:?}");
    assert!((r.height_m().unwrap() - 60.0).abs() < 1.5);
    let text = describe(t, sim.now());
    assert!(text.starts_with("Caution: drone RING circling shed (") && text.contains(" m radius), a full turn in "), "{text}");
}

#[test]
fn photographer_circling_the_house_next_door_is_not_circling_ours() {
    // Closer than the 16a test: the circle passes 10 m outside our fence,
    // but it is round the neighbour's house, so it does not take ours in.
    let next_door = at_site(230.0, 0.0);
    let path = move |t: f64| Flight::Orbit { centre: next_door, radius_m: 70.0, speed_mps: 5.0, alt_m: up(40.0), clockwise: true }.at(t);
    let drones = [Drone { pilot: Some(next_door), jitter_m: 5.0, ..Drone::new("ESTATE", &path) }];
    let mut sim = Sim::new(&[HOUSE]);
    let seen = Seen::run(&mut sim, &drones, 400, "ESTATE");
    assert!(!seen.ever(Rule::Circling));
    assert!(seen.worst() <= Some(Level::Caution), "{:?}", seen.worst());
    // With our house's area 150 m wider it reaches us: now it counts.
    let mut sim = Sim::new(&["area house circle 39.95210 -75.16380 200"]);
    let seen = Seen::run(&mut sim, &drones, 200, "ESTATE");
    assert!(seen.ever(Rule::Circling));
}

#[test]
fn lawnmower_survey_over_the_area_is_not_circling() {
    // Seven 200 m passes 30 m apart over the house at 5 m/s, then a hover.
    // (The passes that go straight over the middle are "heading for" the
    // house under 16a's approach rules, Critical: worth knowing, not the
    // point here.)
    let corner = at_site(-100.0, -100.0);
    let mow = move |t: f64| Flight::Mow { corner, width_m: 200.0, rows: 7, spacing_m: 30.0, speed_mps: 5.0, alt_m: up(60.0) }.at(t);
    let mut sim = Sim::new(&[HOUSE]);
    let seen = Seen::run(&mut sim, &[Drone { jitter_m: 5.0, ..Drone::new("MOW", &mow) }], 400, "MOW");
    assert!(seen.ever(Rule::InArea));
    assert!(!seen.ever(Rule::Circling));
    assert!(!seen.ever(Rule::Loitering) && !seen.ever(Rule::LowHover) && !seen.ever(Rule::Drop));
}

#[test]
fn a_hovering_drone_with_rough_positions_is_not_circling() {
    // Positions off by up to 15 m and 30 m each way, heights by 4 m, every
    // second independently (harsher than real receivers): no circle and no
    // drop out of the wobble, over ten minutes.
    for (jitter, serial) in [(15.0, "WOB15"), (30.0, "WOB30")] {
        let over = at_site(30.0, 20.0);
        let path = move |_t: f64| Flight::Hover { lat: over.0, lon: over.1, alt_m: up(50.0) }.at(0.0);
        let mut sim = Sim::new(&[HOUSE]);
        let drones = [Drone { jitter_m: jitter, alt_jitter_m: 4.0, ..Drone::new(serial, &path) }];
        let seen = Seen::run(&mut sim, &drones, 600, serial);
        assert!(!seen.ever(Rule::Circling), "{serial}");
        assert!(!seen.ever(Rule::Drop), "{serial}");
        assert!(!seen.ever(Rule::LowHover), "{serial}");
        // It does loiter, and that is the same sign as being there.
        assert!(seen.ever(Rule::Loitering), "{serial}");
        assert_eq!(seen.worst(), Some(Level::Caution), "{serial}");
    }
}

// ------------------------------------------------------- 7 loitering --

#[test]
fn loitering_long_and_low_over_the_area_warns() {
    // 30 m up over the garden for seven minutes, then off.
    let over = at_site(40.0, 30.0);
    let away = at_site(40.0, 2000.0);
    let path = move |t: f64| {
        if t < 420.0 {
            Flight::Hover { lat: over.0, lon: over.1, alt_m: up(30.0) }.at(t)
        } else {
            Flight::Toward { from: over, to: away, speed_mps: 10.0, alt_from: up(30.0), alt_to: up(30.0) }.at(t - 420.0)
        }
    };
    let mut sim = Sim::new(&[HOUSE]);
    let seen = Seen::run(&mut sim, &[Drone { jitter_m: 10.0, alt_jitter_m: 1.0, ..Drone::new("LOIT", &path) }], 480, "LOIT");
    let l = seen.first(Rule::Loitering).expect("never loitering");
    assert!((178..=195).contains(&l), "loitering from {l}");
    // Loitering and being there are one sign: still Caution.
    assert_eq!(seen.at(l + 10), Some(Level::Caution));
    let w = seen.reached(Level::Warning).expect("never warned");
    assert!((298..=315).contains(&w), "warning from {w}");
    let (_, _, rules) = seen.entry(w);
    assert_eq!(rules[0], Rule::Loitering, "{rules:?}");
    assert!(!sim.track("LOIT").unwrap().threat.combined);
    // Out of the area at about 432 s; Notice 30 s after the last rule.
    assert_eq!(seen.at(470), Some(Level::Notice), "{:?}", &seen.0[420..]);
    // The words.
    let mut sim = Sim::new(&[HOUSE]);
    sim.run(&[Drone { jitter_m: 10.0, ..Drone::new("LOIT", &path) }], 320, |_| {});
    let t = sim.track("LOIT").unwrap();
    let text = describe(t, sim.now());
    assert!(text.starts_with("Warning: drone LOIT lingering at house for 5 min, 30 m up; over house"), "{text}");
    assert_eq!(strip_text(t).unwrap().as_str(), "WARNING LOIT lingering house 5min");
}

#[test]
fn loitering_high_or_short_stays_caution() {
    // 100 m up for two and a half minutes, then off: not long enough.
    let over = at_site(-30.0, 40.0);
    let away = at_site(-2000.0, 40.0);
    let short = move |t: f64| {
        if t < 150.0 {
            Flight::Hover { lat: over.0, lon: over.1, alt_m: up(100.0) }.at(t)
        } else {
            Flight::Toward { from: over, to: away, speed_mps: 10.0, alt_from: up(100.0), alt_to: up(100.0) }.at(t - 150.0)
        }
    };
    let mut sim = Sim::new(&[HOUSE]);
    let seen = Seen::run(&mut sim, &[Drone { jitter_m: 10.0, ..Drone::new("HI-S", &short) }], 250, "HI-S");
    assert!(!seen.ever(Rule::Loitering));
    assert_eq!(seen.worst(), Some(Level::Caution));
    // 100 m up for seven minutes: loitering, but high, so Caution.
    let long = move |_t: f64| Flight::Hover { lat: over.0, lon: over.1, alt_m: up(100.0) }.at(0.0);
    let mut sim = Sim::new(&[HOUSE]);
    let seen = Seen::run(&mut sim, &[Drone { jitter_m: 10.0, ..Drone::new("HI-L", &long) }], 420, "HI-L");
    assert!(seen.ever(Rule::Loitering));
    assert_eq!(seen.worst(), Some(Level::Caution));
}

#[test]
fn loitering_just_outside_the_fence_is_caution() {
    // 30 m beyond the fence, 50 m up, for four minutes.
    let out = at_site(0.0, 180.0);
    let path = move |_t: f64| Flight::Hover { lat: out.0, lon: out.1, alt_m: up(50.0) }.at(0.0);
    let mut sim = Sim::new(&[HOUSE]);
    let seen = Seen::run(&mut sim, &[Drone { jitter_m: 5.0, ..Drone::new("FENCE", &path) }], 240, "FENCE");
    let l = seen.first(Rule::Loitering).expect("never loitering");
    assert!((178..=195).contains(&l), "{l}");
    assert_eq!(seen.at(l - 1), Some(Level::Notice));
    assert_eq!(sim.rules("FENCE"), [Rule::Loitering]);
    assert_eq!(sim.level("FENCE"), Some(Level::Caution));
}

// ------------------------------------------------------- 9 low hover --

#[test]
fn roof_inspector_hovering_low_warns_as_expected() {
    // A known false alarm (THREAT-TACTICS.md §6): an inspection drone
    // comes in slowly at 40 m (2 m/s: under the approach rules' 3 m/s),
    // settles 8 m up over the roof, then climbs away.
    // Inside the area, under 15 m and still for over 10 s: Warning.
    let from = at_site(130.0, 0.0);
    let roof = at_site(20.0, 10.0);
    let away = at_site(20.0, 2000.0);
    let path = move |t: f64| {
        if t < 120.0 {
            Flight::Toward { from, to: roof, speed_mps: 2.0, alt_from: up(40.0), alt_to: up(8.0) }.at(t)
        } else {
            Flight::Toward { from: roof, to: away, speed_mps: 8.0, alt_from: up(60.0), alt_to: up(60.0) }.at(t - 120.0)
        }
    };
    let mut sim = Sim::new(&[HOUSE]);
    let drones = [Drone { jitter_m: 3.0, alt_jitter_m: 1.0, ..Drone::new("ROOF", &path) }];
    let seen = Seen::run(&mut sim, &drones, 200, "ROOF");
    // Under 15 m from about 43 s, arrives at 55 s; low and still for 10 s
    // by about 60 s.
    let h = seen.first(Rule::LowHover).expect("never low hover");
    assert!((55..=70).contains(&h), "low hover from {h}");
    assert_eq!(seen.reached(Level::Warning), Some(h));
    let (_, _, rules) = seen.entry(h);
    assert_eq!(rules[0], Rule::LowHover);
    // Climbs away at 120; shown while the Warning is held 30 s, then down.
    let last = seen.last(Rule::LowHover).unwrap();
    assert!((145..=155).contains(&last), "{last}");
    assert!(seen.at(last + 1) <= Some(Level::Caution));
    // The words, while hovering.
    let mut sim = Sim::new(&[HOUSE]);
    sim.run(&drones, 90, |_| {});
    let t = sim.track("ROOF").unwrap();
    let text = describe(t, sim.now());
    assert!(text.starts_with("Warning: drone ROOF hovering low over house, 8 m up, for "), "{text}");
    assert!(strip_text(t).unwrap().starts_with("WARNING ROOF low over house 8m"));
}

#[test]
fn landing_and_staying_down_is_low_but_no_drop() {
    // Down from 50 m to the ground at 2 m/s inside the area, and stays.
    let spot = at_site(30.0, 20.0);
    let path = move |t: f64| Flight::Dip { at: spot, alt_m: up(50.0), depth_m: 50.0, start_s: 20.0, rate_mps: 2.0, pause_s: f64::INFINITY }.at(t);
    let mut sim = Sim::new(&[HOUSE]);
    let seen = Seen::run(&mut sim, &[Drone { jitter_m: 3.0, alt_jitter_m: 1.0, ..Drone::new("LAND", &path) }], 200, "LAND");
    assert!(!seen.ever(Rule::Drop));
    // A drone sitting in the yard is low and still: Warning (expected).
    assert!(seen.ever(Rule::LowHover));
}

#[test]
fn no_height_no_low_hover() {
    // A drone that sends no height at all cannot be judged low.
    let over = at_site(20.0, 0.0);
    let p = Flight::Hover { lat: over.0, lon: over.1, alt_m: up(5.0) }.at(0.0);
    let mut tr: std::boxed::Box<super::tests::Tracker> = std::boxed::Box::default();
    let mut st = crate::site::SiteState::default();
    st.apply(crate::site::parse_line(HOUSE).unwrap());
    tr.set_places(&st.places);
    let mut msgs = super::tests::frame("NOHT", &p, None);
    // Rewrite the location without any height.
    msgs[1] = rid::encode::message(&rid::message::Message::Location(rid::message::Location {
        status: rid::message::Status::Airborne,
        height_type: rid::message::HeightType::AboveTakeoff,
        direction: None,
        speed_horizontal: Some(0.0),
        speed_vertical: None,
        position: Some(rid::message::LatLon { lat: p.lat, lon: p.lon }),
        altitude_pressure: None,
        altitude_geodetic: None,
        height: None,
        horizontal_accuracy: rid::message::HorizontalAccuracy::M10,
        vertical_accuracy: rid::message::VerticalAccuracy::M10,
        baro_accuracy: rid::message::VerticalAccuracy::M10,
        speed_accuracy: rid::message::SpeedAccuracy::Mps1,
        timestamp: None,
        timestamp_accuracy: None,
    }))
    .unwrap();
    for k in 0..60u64 {
        let now = super::tests::T0 + k * 1000;
        tr.set_own_fix(SITE.0, SITE.1, Some(OWN_ALT), now, Some((0.0, None)));
        tr.ingest(0, &crate::tests::det(rid::Transport::WifiBeacon, [0x60, 0, 0, 0, 0, 7], -60, &msgs), now);
        tr.tick_behaviour(now);
    }
    let t = &tr.tracks()[0];
    assert_eq!(t.threat.level, Some(Level::Caution));
    assert!(t.threat.reasons.iter().all(|r| r.rule == Rule::InArea), "{:?}", t.threat.reasons);
}

// ------------------------------------------------------------ 10 drop --

#[test]
fn drop_over_the_yard_warns_then_comes_down() {
    // 50 m up over the yard; from second 40 down 30 m at 3 m/s, 5 s at the
    // bottom, back up. Heights wobble by up to 2 m.
    let yard = at_site(30.0, 20.0);
    let path = move |t: f64| Flight::Dip { at: yard, alt_m: up(50.0), depth_m: 30.0, start_s: 40.0, rate_mps: 3.0, pause_s: 5.0 }.at(t);
    let drones = [Drone { jitter_m: 5.0, alt_jitter_m: 2.0, ..Drone::new("DROP", &path) }];
    let mut sim = Sim::new(&[HOUSE]);
    let seen = Seen::run(&mut sim, &drones, 160, "DROP");
    // Down at 50, bottom until 55, 8 m back up by about 58.
    let d = seen.first(Rule::Drop).expect("never a drop");
    assert!((57..=63).contains(&d), "drop from {d}");
    assert_eq!(seen.reached(Level::Warning), Some(d));
    assert!(seen.0[..d as usize - 1].iter().all(|e| e.1 <= Some(Level::Caution)));
    // It counts while the whole dip is within the last minute (the
    // descent began at 40, so until about 103), and is shown while the
    // level is held 30 s more.
    let last = seen.last(Rule::Drop).unwrap();
    assert!((125..=140).contains(&last), "drop shown until {last}");
    assert_eq!(seen.at(last + 1), Some(Level::Caution), "back to just being over the yard");
    // The numbers and words, just after.
    let mut sim = Sim::new(&[HOUSE]);
    sim.run(&drones, 66, |_| {});
    let t = sim.track("DROP").unwrap();
    let r = &t.threat.reasons[0];
    assert_eq!(r.rule, Rule::Drop);
    assert!((r.range_m() - 30.0).abs() <= 4.0, "depth {r:?}");
    assert!((r.height_m().unwrap() - 20.0).abs() <= 3.0, "bottom {r:?}");
    assert!(r.ago_s().unwrap() <= 12, "{r:?}");
    let text = describe(t, sim.now());
    assert!(text.starts_with("Warning: drone DROP dropped ") && text.contains(" m over house (down to "), "{text}");
    assert!(text.contains(" m up) and climbed again, "), "{text}");
}

#[test]
fn a_dip_outside_the_area_is_no_drop() {
    let out = at_site(0.0, 300.0);
    let path = move |t: f64| Flight::Dip { at: out, alt_m: up(50.0), depth_m: 30.0, start_s: 20.0, rate_mps: 3.0, pause_s: 5.0 }.at(t);
    let mut sim = Sim::new(&[HOUSE]);
    let seen = Seen::run(&mut sim, &[Drone::new("FAR", &path)], 100, "FAR");
    assert!(!seen.ever(Rule::Drop));
    assert_eq!(seen.worst(), Some(Level::Notice));
}

// ------------------------------------------------------- 11 following --

/// A walker 400 m east along a street at 1.4 m/s (from second 0 to about
/// 286), with a 300 m bubble.
fn walk_east(t: f64) -> Sample {
    Flight::Toward { from: at_site(-200.0, 0.0), to: at_site(200.0, 0.0), speed_mps: 1.4, alt_from: OWN_ALT as f32, alt_to: OWN_ALT as f32 }.at(t)
}

#[test]
fn following_a_walker_for_400_m_warns() {
    // A drone 60 m north and 30 m behind, 50 m up, keeping pace; at second
    // 320 it flies off north at 12 m/s.
    let lane = move |t: f64| {
        Flight::Toward { from: at_site(-230.0, 60.0), to: at_site(170.0, 60.0), speed_mps: 1.4, alt_from: up(50.0), alt_to: up(50.0) }.at(t)
    };
    let off = lane(320.0);
    let path = move |t: f64| {
        if t < 320.0 {
            lane(t)
        } else {
            let far = offset(off.lat, off.lon, 0.0, 3000.0);
            Flight::Toward { from: (off.lat, off.lon), to: far, speed_mps: 12.0, alt_from: up(50.0), alt_to: up(50.0) }.at(t - 320.0)
        }
    };
    let mut sim = Sim::new(&["bubble 300"]);
    sim.own = Some(std::boxed::Box::new(walk_east));
    let drones = [Drone { jitter_m: 5.0, ..Drone::new("SHADOW", &path) }];
    let mut at_end = None;
    let mut v = Vec::new();
    sim.run(&drones, 420, |s| {
        v.push((s.s, s.level("SHADOW"), s.rules("SHADOW")));
        if s.s == 300 {
            at_end = s.track("SHADOW").unwrap().threat.reasons.iter().find(|r| r.rule == Rule::Following).copied();
        }
    });
    let seen = Seen(v);
    // 200 m of walking takes 143 s (the paths are kept every 5 s).
    let f = seen.first(Rule::Following).expect("never following");
    assert!((143..=160).contains(&f), "following from {f}");
    assert_eq!(seen.reached(Level::Warning), Some(f), "Caution (in the bubble) until then");
    let r = at_end.expect("following at the end of the walk");
    assert!(r.is_bubble());
    assert!(r.range_m() >= 380.0 && r.range_m() <= 410.0, "{r:?}");
    assert!(r.for_s().unwrap() >= 270, "{r:?}");
    // Off at 320: out of reach (300 m) about 20 s later, shown while the
    // Warning is held 30 s more.
    let last = seen.last(Rule::Following).unwrap();
    assert!((360..=385).contains(&last), "following shown until {last}");
    assert!(seen.at(last + 1) <= Some(Level::Caution));
    assert_eq!(seen.at(419), Some(Level::Notice));
}

#[test]
fn walking_past_a_hovering_drone_is_not_following() {
    // The walker passes 50 m under a drone hovering over the street: it is
    // near for the whole 400 m, but it does not move.
    let over = at_site(0.0, 50.0);
    let hover = move |_t: f64| Flight::Hover { lat: over.0, lon: over.1, alt_m: up(50.0) }.at(0.0);
    let mut sim = Sim::new(&["bubble 300"]);
    sim.own = Some(std::boxed::Box::new(walk_east));
    let seen = Seen::run(&mut sim, &[Drone { jitter_m: 10.0, ..Drone::new("STILL", &hover) }], 300, "STILL");
    assert!(seen.ever(Rule::InArea));
    assert!(!seen.ever(Rule::Following));
    assert!(!seen.ever(Rule::Loitering), "the bubble moved: not loitering at the walker");
    assert_eq!(seen.worst(), Some(Level::Caution));
    // A drone flying the other way past the walker is near only briefly.
    let other = move |t: f64| {
        Flight::Toward { from: at_site(300.0, 40.0), to: at_site(-500.0, 40.0), speed_mps: 4.0, alt_from: up(50.0), alt_to: up(50.0) }.at(t)
    };
    let mut sim = Sim::new(&["bubble 300"]);
    sim.own = Some(std::boxed::Box::new(walk_east));
    let seen = Seen::run(&mut sim, &[Drone { jitter_m: 5.0, ..Drone::new("PASS", &other) }], 250, "PASS");
    assert!(!seen.ever(Rule::Following));
}

#[test]
fn a_friends_follow_me_drone_is_notice() {
    // The user's own drone in follow-me mode, 15 m behind, 20 m up.
    let selfie = move |t: f64| {
        Flight::Toward { from: at_site(-215.0, 5.0), to: at_site(185.0, 5.0), speed_mps: 1.4, alt_from: up(20.0), alt_to: up(20.0) }.at(t)
    };
    let mut sim = Sim::new(&["bubble 300", "friend MY-SELFIE my own drone"]);
    sim.own = Some(std::boxed::Box::new(walk_east));
    let pilot_walks = None;
    let seen = Seen::run(&mut sim, &[Drone { pilot: pilot_walks, jitter_m: 3.0, ..Drone::new("MY-SELFIE", &selfie) }], 300, "MY-SELFIE");
    assert!(seen.ever(Rule::Following), "still worked out and shown");
    assert_eq!(seen.worst(), Some(Level::Notice));
    let t = sim.track("MY-SELFIE").unwrap();
    assert!(t.threat.friend);
    assert!(t.threat.reasons.iter().all(|r| r.level == Level::Notice));
}

// --------------------------------------------- the bubble and its pilot --

#[test]
fn in_the_bubble_a_pilot_nearby_is_no_second_sign() {
    // A hobbyist 100 m from the unit, 40 m up, the pilot 20 m from the
    // unit. Fixed area: two signs, Warning (as 16a). Bubble: Caution.
    let spot = at_site(100.0, 0.0);
    let pilot = at_site(20.0, 0.0);
    let path = move |_t: f64| Flight::Hover { lat: spot.0, lon: spot.1, alt_m: up(40.0) }.at(0.0);
    let drones = [Drone { pilot: Some(pilot), ..Drone::new("PARK", &path) }];
    let mut sim = Sim::new(&[HOUSE]);
    sim.run(&drones, 30, |_| {});
    assert_eq!(sim.level("PARK"), Some(Level::Warning));
    assert!(sim.track("PARK").unwrap().threat.combined);
    let mut sim = Sim::new(&["bubble 300"]);
    sim.run(&drones, 30, |_| {});
    let t = sim.track("PARK").unwrap();
    assert_eq!(t.threat.level, Some(Level::Caution));
    assert!(!t.threat.combined);
    assert_eq!(sim.rules("PARK"), [Rule::PilotNear, Rule::InArea], "both still shown");
    // Taking off beside the pilot (5 m up for 30 s), then climbing: not a
    // low hover in the bubble.
    let pad = at_site(60.0, 0.0);
    let pilot = at_site(50.0, 0.0);
    let takeoff = move |t: f64| {
        let alt = if t < 30.0 { up(5.0) } else { up(40.0) };
        Flight::Hover { lat: pad.0, lon: pad.1, alt_m: alt }.at(t)
    };
    let mut sim = Sim::new(&["bubble 300"]);
    let seen = Seen::run(&mut sim, &[Drone { pilot: Some(pilot), jitter_m: 2.0, ..Drone::new("UP", &takeoff) }], 60, "UP");
    assert!(!seen.ever(Rule::LowHover));
    assert_eq!(seen.worst(), Some(Level::Caution));
    // The same in a fixed area is a low hover (the roof-inspector case).
    let mut sim = Sim::new(&[HOUSE]);
    let seen = Seen::run(&mut sim, &[Drone { pilot: Some(pilot), jitter_m: 2.0, ..Drone::new("UP", &takeoff) }], 60, "UP");
    assert!(seen.ever(Rule::LowHover));
}

// ------------------------------------------------------------- outputs --

#[test]
fn new_rules_reach_json_cot_the_strip_and_the_evidence() {
    let yard = at_site(30.0, 20.0);
    let path = move |t: f64| Flight::Dip { at: yard, alt_m: up(50.0), depth_m: 30.0, start_s: 40.0, rate_mps: 3.0, pause_s: 5.0 }.at(t);
    let mut sim = Sim::new(&[HOUSE]);
    let mut rise = None;
    while sim.s < 66 {
        sim.step(&[Drone::new("OUT-D", &path)]);
        while let Some(r) = sim.tr.take_level_rise(sim.now()) {
            let t = sim.tr.get(r.track).unwrap();
            rise = Some((r.level, rise_data(t, &r)));
        }
    }
    let (level, data) = rise.unwrap();
    assert_eq!(level, Level::Warning);
    assert_eq!(data[5..9], [Rule::Drop as u8, Rule::InArea as u8, 0, 0]);
    assert_eq!(Rule::Drop as u8, 10);
    let now = sim.now() - 1000;
    let t = sim.track("OUT-D").unwrap();
    let ctx = sim.tr.context(now, Some(1_700_000_000_000));
    let mut buf = [0u8; crate::json::MAX_LINE];
    let n = crate::json::track_line(t, &ctx, &mut buf).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&buf[..n]).unwrap();
    let r0 = &v["threat"]["reasons"][0];
    assert_eq!(r0["rule"], "drop");
    assert_eq!(r0["level"], "warning");
    assert!(r0["m"].as_f64().unwrap() > 25.0);
    assert!(r0["h_m"].as_f64().is_some() && r0["ago_s"].as_u64().is_some());
    assert!(r0.get("for_s").is_none() && r0.get("eta_s").is_none());
    let mut cot = [0u8; crate::cot::MAX_EVENT];
    let n = crate::cot::drone_event(t, &ctx, &mut cot).unwrap();
    let x = core::str::from_utf8(&cot[..n]).unwrap();
    assert!(x.contains("; Threat: Warning: dropped "), "{x}");
    assert!(x.contains(&std::format!("<color argb=\"{}\"/>", 0xFFFF_8000_u32 as i32)), "orange: {x}");
    assert!(strip_text(t).unwrap().starts_with("WARNING OUT-D drop at house "));
    // Loitering and following carry how long.
    let mut sim = Sim::new(&["bubble 300"]);
    sim.own = Some(std::boxed::Box::new(walk_east));
    let lane = move |t: f64| {
        Flight::Toward { from: at_site(-230.0, 60.0), to: at_site(170.0, 60.0), speed_mps: 1.4, alt_from: up(50.0), alt_to: up(50.0) }.at(t)
    };
    sim.run(&[Drone::new("OUT-F", &lane)], 200, |_| {});
    let t = sim.track("OUT-F").unwrap();
    let ctx = sim.tr.context(sim.now() - 1000, None);
    let n = crate::json::track_line(t, &ctx, &mut buf).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&buf[..n]).unwrap();
    let r0 = &v["threat"]["reasons"][0];
    assert_eq!(r0["rule"], "following");
    assert_eq!(r0["place"], "");
    assert!(r0["for_s"].as_u64().unwrap() > 140);
    assert!(r0.get("h_m").is_none());
    let text = describe(t, sim.now());
    assert!(text.starts_with("Warning: drone OUT-F following you for 2"), "{text}");
    assert!(strip_text(t).unwrap().starts_with("WARNING OUT-F following you 2"));
}

#[test]
fn the_unit_keeps_its_own_trail_and_the_tracker_stays_small() {
    use core::mem::size_of;
    let hub = size_of::<super::tests::Tracker>();
    std::println!("Tracker<64,..,64> with the own trail: {hub} B (trail {} B)", size_of::<OwnTrail<64>>());
    assert!(hub < 1 << 20, "{hub}");
    assert!(size_of::<OwnTrail<64>>() <= 3_100);
    // The S3 hub and the viewer (no path rings) keep no trail.
    assert!(size_of::<OwnTrail<0>>() <= 4);
}

