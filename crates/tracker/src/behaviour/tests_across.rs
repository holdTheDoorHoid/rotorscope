//! Scripted flights for the rules across drones and days (slice 16d, rules
//! 14-19): spotter then striker, several at once, probing, repeat
//! visitors (with days stepped on the clock), pilot on the move and
//! identity changes; each with the false alarm it must not raise, and the
//! level coming down afterwards. Uses the 16a simulation ([`Sim`]).

extern crate std;
use std::vec::Vec;

use super::scenario::{Flight, Sample, offset};
use super::tests::{Drone, HOUSE, OWN_ALT, SITE, Sim};
use super::*;

/// 2026-11-18 00:00 UTC, plus ten hours (a morning).
const UTC0: u64 = 1_763_424_000_000 + 10 * 3_600_000;
const DAY_S: u64 = 86_400;

fn up(m: f32) -> f32 {
    m + OWN_ALT as f32
}

fn at_site(e: f64, n: f64) -> (f64, f64) {
    offset(SITE.0, SITE.1, e, n)
}

/// Each second's level and rules for each named drone.
struct Seen(Vec<(u64, std::string::String, Option<Level>, Vec<Rule>)>);

impl Seen {
    fn run(sim: &mut Sim, drones: &[Drone<'_>], to: u64, watch: &[&str]) -> Self {
        let mut v = Vec::new();
        sim.run(drones, to, |s| {
            for w in watch {
                v.push((s.s, std::string::String::from(*w), s.level(w), s.rules(w)));
            }
        });
        Self(v)
    }

    fn of<'a>(&'a self, who: &'a str) -> impl Iterator<Item = &'a (u64, std::string::String, Option<Level>, Vec<Rule>)> + 'a {
        self.0.iter().filter(move |e| e.1 == who)
    }

    fn first(&self, who: &str, rule: Rule) -> Option<u64> {
        self.of(who).find(|e| e.3.contains(&rule)).map(|e| e.0)
    }

    fn last(&self, who: &str, rule: Rule) -> Option<u64> {
        self.of(who).filter(|e| e.3.contains(&rule)).last().map(|e| e.0)
    }

    fn worst(&self, who: &str) -> Option<Level> {
        self.of(who).map(|e| e.2).max().flatten()
    }

    fn at(&self, who: &str, s: u64) -> Option<Level> {
        self.of(who).find(|e| e.0 == s).and_then(|e| e.2)
    }
}

fn reason<'a>(sim: &'a Sim, serial: &str, rule: Rule) -> Option<&'a Reason> {
    sim.track(serial)?.threat.reasons.iter().find(|r| r.rule == rule)
}

// ------------------------------------------------- 14 spotter, striker --

/// Circles round the house for 4 min (80 m out, 6 m/s, 50 m up), then flies
/// off north at 10 m/s.
fn spotter(t: f64) -> Sample {
    let orbit = Flight::Orbit { centre: at_site(20.0, 0.0), radius_m: 80.0, speed_mps: 6.0, alt_m: up(50.0), clockwise: true };
    if t < 240.0 {
        return orbit.at(t);
    }
    let left = orbit.at(240.0);
    Flight::Toward { from: (left.lat, left.lon), to: at_site(20.0, 6000.0), speed_mps: 10.0, alt_from: up(50.0), alt_to: up(50.0) }.at(t - 240.0)
}

/// From 1500 m south, straight at the house at 8 m/s (a beeline, slower
/// than the "fast inbound" speeds: on its own a Warning), starting at
/// second `start`.
fn striker(start: f64) -> impl Fn(f64) -> Sample {
    move |t: f64| Flight::Toward { from: at_site(0.0, -1500.0), to: SITE, speed_mps: 8.0, alt_from: up(60.0), alt_to: up(40.0) }.at(t - start)
}

#[test]
fn a_striker_three_minutes_after_a_spotter_is_critical() {
    let strike = striker(240.0);
    let drones = [Drone::new("SPOT-1", &spotter), Drone { from_s: 240, ..Drone::new("STRK-1", &strike) }];
    let mut sim = Sim::new(&[HOUSE]);
    let seen = Seen::run(&mut sim, &drones, 520, &["SPOT-1", "STRK-1"]);
    assert!(seen.first("SPOT-1", Rule::Circling).is_some(), "the spotter circled");
    let hit = seen.first("STRK-1", Rule::SpotterStriker).expect("spotter then striker");
    // It arrives about 187 s after the spotter left: well inside 10 min.
    assert!((330..=400).contains(&hit), "{hit}");
    assert_eq!(seen.at("STRK-1", hit), Some(Level::Critical));
    // On its own (8 m/s) it would only be a beeline: never fast inbound.
    assert_eq!(seen.first("STRK-1", Rule::FastInbound), None);
    // The spotter, still heard as it flies off, is marked.
    let m = seen.first("SPOT-1", Rule::Spotter).expect("spotter marked");
    assert!(m >= hit && m <= hit + 1);
    assert_eq!(seen.at("SPOT-1", m), Some(Level::Warning));
    // Decay: the striker arrives and hovers over the house; the spotter's
    // mark ends 30 s after the last strike.
    let last = seen.last("SPOT-1", Rule::Spotter).unwrap();
    assert!(last < 480, "{last}");
    assert!(seen.at("SPOT-1", last + DECAY_MS / 1000 + 2) <= Some(Level::Notice), "{:?}", seen.at("SPOT-1", last + 32));
    // The same flights again, stopped just after the hit: the reasons.
    let mut sim = Sim::new(&[HOUSE]);
    sim.run(&drones, m, |_| {});
    let spot_id = sim.track("SPOT-1").unwrap().id;
    let r = reason(&sim, "STRK-1", Rule::SpotterStriker).unwrap();
    assert_eq!(r.other(), Some(spot_id));
    assert_eq!(r.place(), "house");
    assert!(r.ago_s().unwrap() > 90, "{:?}", r.ago_s());
    let strike_id = sim.track("STRK-1").unwrap().id;
    assert_eq!(reason(&sim, "SPOT-1", Rule::Spotter).unwrap().other(), Some(strike_id));
    // The words name the other drone.
    let text = sim.tr.describe_level(strike_id, sim.now()).unwrap();
    assert!(text.starts_with("Critical: drone STRK-1 "), "{text}");
    assert!(text.contains("follows spotter drone SPOT-1 (lingered at house "), "{text}");
    let text = sim.tr.describe_level(spot_id, sim.now()).unwrap();
    assert!(text.contains("spotter: drone STRK-1 is rushing to house"), "{text}");
    assert!(strip_text(sim.track("STRK-1").unwrap()).unwrap().starts_with("CRITICAL STRK-1 after spotter to house"));
    // Evidence: rule 14 on the striker, 21 on the spotter.
    let rise = Rise { track: strike_id, level: Level::Critical, snoozed: false };
    assert!(rise_data(sim.track("STRK-1").unwrap(), &rise)[5..9].contains(&14));
}

#[test]
fn a_slow_striker_at_the_bubble_is_a_warning_not_critical() {
    // The same two flights in a park: no fixed area, only a 300 m bubble
    // round a unit standing at the site. Two hobbyists is the likely story,
    // so the 8 m/s beeline after a spotter stays a Warning; rule 14 is
    // still given as a reason.
    let strike = striker(240.0);
    let drones = [Drone::new("SPOT-B", &spotter), Drone { from_s: 240, ..Drone::new("STRK-B", &strike) }];
    let mut sim = Sim::new(&["bubble 300"]);
    let seen = Seen::run(&mut sim, &drones, 520, &["SPOT-B", "STRK-B"]);
    assert!(seen.first("SPOT-B", Rule::Circling).is_some(), "the spotter circled");
    assert!(seen.first("STRK-B", Rule::SpotterStriker).is_some(), "rule 14 still named");
    assert_eq!(seen.first("STRK-B", Rule::FastInbound), None);
    assert_eq!(seen.worst("STRK-B"), Some(Level::Warning));
}

#[test]
fn a_striker_fifteen_minutes_later_is_only_a_beeline() {
    let strike = striker(1140.0);
    let drones = [Drone::new("SPOT-2", &spotter), Drone { from_s: 1140, ..Drone::new("STRK-2", &strike) }];
    let mut sim = Sim::new(&[HOUSE]);
    let seen = Seen::run(&mut sim, &drones, 1400, &["STRK-2"]);
    assert!(seen.first("STRK-2", Rule::Beeline).is_some());
    assert_eq!(seen.first("STRK-2", Rule::SpotterStriker), None);
    assert_eq!(seen.worst("STRK-2"), Some(Level::Warning));
}

#[test]
fn a_hobbyist_passing_far_away_or_the_spotter_itself_is_no_striker() {
    // A second drone 1 km east, fast, going north: never heads for the house.
    let pass = |t: f64| Flight::Line { from: at_site(1000.0, -1500.0), course_deg: 0.0, speed_mps: 20.0, alt_m: up(80.0) }.at(t - 250.0);
    // The spotter itself coming back after its circle is not its own striker.
    let back = |t: f64| {
        if t < 300.0 {
            spotter(t)
        } else {
            let s = spotter(300.0);
            Flight::Toward { from: (s.lat, s.lon), to: SITE, speed_mps: 8.0, alt_from: up(50.0), alt_to: up(40.0) }.at(t - 300.0)
        }
    };
    let drones = [Drone::new("SPOT-3", &back), Drone { from_s: 250, ..Drone::new("PASS-1", &pass) }];
    let mut sim = Sim::new(&[HOUSE]);
    let seen = Seen::run(&mut sim, &drones, 420, &["SPOT-3", "PASS-1"]);
    assert_eq!(seen.first("PASS-1", Rule::SpotterStriker), None);
    assert_eq!(seen.worst("PASS-1"), Some(Level::Notice));
    assert!(seen.first("SPOT-3", Rule::Beeline).is_some(), "it does head back in");
    assert_eq!(seen.first("SPOT-3", Rule::SpotterStriker), None);
}

// -------------------------------------------------- 15 several at once --

fn hover_at(e: f64, n: f64) -> impl Fn(f64) -> Sample {
    let p = at_site(e, n);
    move |_t: f64| Flight::Hover { lat: p.0, lon: p.1, alt_m: up(60.0) }.at(0.0)
}

#[test]
fn three_drones_at_once_warn_unless_crowd_is_off() {
    // Three drones hovering just outside the fence (150 m), 250-280 m from
    // the middle: each alone is nothing (Notice).
    let (a, b, c) = (hover_at(250.0, 0.0), hover_at(0.0, 280.0), hover_at(-260.0, 0.0));
    let drones = [
        Drone::new("CRWD-A", &a),
        Drone { until_s: 120, ..Drone::new("CRWD-B", &b) },
        Drone { from_s: 20, until_s: 120, ..Drone::new("CRWD-C", &c) },
    ];
    let mut sim = Sim::new(&[HOUSE]);
    let seen = Seen::run(&mut sim, &drones, 200, &["CRWD-A", "CRWD-C"]);
    assert_eq!(seen.at("CRWD-A", 15), Some(Level::Notice), "two is not a crowd");
    let w = seen.first("CRWD-A", Rule::Several).unwrap();
    assert!((20..=26).contains(&w), "{w}");
    assert_eq!(seen.at("CRWD-A", w), Some(Level::Warning));
    assert!(seen.first("CRWD-C", Rule::Several).is_some());
    let r = reason(&sim, "CRWD-A", Rule::Several);
    assert!(r.is_none(), "gone by second 200");
    // Two leave at 120: A comes down 30 s later.
    assert_eq!(seen.at("CRWD-A", 135), Some(Level::Warning), "held");
    assert_eq!(seen.at("CRWD-A", 160), Some(Level::Notice));
    // The words and the count.
    let mut sim = Sim::new(&[HOUSE]);
    sim.run(&drones, 60, |_| {});
    let r = *reason(&sim, "CRWD-A", Rule::Several).unwrap();
    assert_eq!((r.count(), r.alarm()), (Some(3), None));
    let mut t = std::string::String::new();
    write_reason(&mut t, &r).unwrap();
    assert_eq!(t, "one of 3 drones at house at once");
    // crowd off (an event), and crowd 4: nothing.
    for line in ["crowd off", "crowd 4"] {
        let mut sim = Sim::new(&[HOUSE, line]);
        let seen = Seen::run(&mut sim, &drones, 120, &["CRWD-A", "CRWD-C"]);
        assert_eq!(seen.worst("CRWD-A"), Some(Level::Notice), "{line}");
        assert_eq!(seen.first("CRWD-C", Rule::Several), None, "{line}");
    }
}

#[test]
fn a_friend_does_not_make_a_crowd() {
    let (a, b, c) = (hover_at(250.0, 0.0), hover_at(0.0, 280.0), hover_at(-260.0, 0.0));
    let drones = [Drone::new("CRWD-A", &a), Drone::new("CRWD-B", &b), Drone::new("OUR-MINI", &c)];
    let mut sim = Sim::new(&[HOUSE, "friend OUR-MINI"]);
    let seen = Seen::run(&mut sim, &drones, 60, &["CRWD-A"]);
    assert_eq!(seen.worst("CRWD-A"), Some(Level::Notice));
}

fn jamming() -> link::ThreatSighting {
    link::ThreatSighting {
        kind: link::ThreatKind::JamBroadband,
        channel: 0,
        freq: 24_370,
        width_khz: 20_000,
        level: 20,
        count: 5,
        window_ms: 1_000,
        source: [0; 6],
        target: [0; 6],
        detail: 80,
        ssid: heapless::String::new(),
        uptime_us: 0,
    }
}

#[test]
fn a_drone_at_the_house_during_jamming_warns() {
    let a = hover_at(250.0, 0.0);
    let drones = [Drone::new("JAM-1", &a)];
    let mut sim = Sim::new(&[HOUSE]);
    sim.run(&drones, 30, |_| {});
    assert_eq!(sim.level("JAM-1"), Some(Level::Notice));
    // Jamming reported every few seconds from 30 s to 90 s (a report keeps
    // the alarm active for 10 s).
    let mut seen = Vec::new();
    while sim.s < 200 {
        if (30..90).contains(&sim.s) && sim.s % 5 == 0 {
            let now = sim.now();
            sim.tr.ingest_threat(3, &jamming(), now);
        }
        sim.step(&drones);
        seen.push((sim.s, sim.level("JAM-1"), sim.rules("JAM-1")));
    }
    let first = seen.iter().find(|e| e.2.contains(&Rule::Several)).unwrap();
    assert!(first.0 <= 33 && first.1 == Some(Level::Warning), "{first:?}");
    let r = reason(&sim, "JAM-1", Rule::Several);
    assert!(r.is_none());
    // The alarm lasts 10 s after the last report (85 s), then 30 s hold.
    let last = seen.iter().filter(|e| e.2.contains(&Rule::Several)).last().unwrap().0;
    assert!((94..=130).contains(&last), "{last}");
    assert_eq!(seen.last().unwrap().1, Some(Level::Notice));
    // The words.
    let mut sim = Sim::new(&[HOUSE]);
    sim.run(&drones, 20, |_| {});
    let now = sim.now();
    sim.tr.ingest_threat(3, &jamming(), now);
    sim.run(&drones, 22, |_| {});
    let r = *reason(&sim, "JAM-1", Rule::Several).unwrap();
    assert_eq!(r.alarm(), Some(across::ALARM_JAMMING));
    let mut t = std::string::String::new();
    write_reason(&mut t, &r).unwrap();
    assert_eq!(t, "at house while jamming is heard");
}

#[test]
fn a_drone_far_from_every_place_is_not_judged_by_jamming() {
    let far = hover_at(2000.0, 0.0);
    let drones = [Drone::new("FAR-1", &far)];
    let mut sim = Sim::new(&[HOUSE]);
    sim.run(&drones, 10, |_| {});
    let now = sim.now();
    sim.tr.ingest_threat(3, &jamming(), now);
    sim.run(&drones, 15, |_| {});
    assert_eq!(sim.level("FAR-1"), Some(Level::Notice));
}

// ------------------------------------------------------------ 16 probe --

/// Over the house (50 m from the middle) for a minute, out to 600 m south
/// at 10 m/s, hover there until `back_s`, then back in at 10 m/s.
fn probe(back_s: f64) -> impl Fn(f64) -> Sample {
    let inside = at_site(50.0, 0.0);
    let out = at_site(50.0, -600.0);
    move |t: f64| {
        if t < 60.0 {
            Flight::Hover { lat: inside.0, lon: inside.1, alt_m: up(60.0) }.at(0.0)
        } else if t < back_s {
            Flight::Toward { from: inside, to: out, speed_mps: 10.0, alt_from: up(60.0), alt_to: up(60.0) }.at(t - 60.0)
        } else {
            Flight::Toward { from: out, to: inside, speed_mps: 10.0, alt_from: up(60.0), alt_to: up(60.0) }.at(t - back_s)
        }
    }
}

#[test]
fn out_and_back_within_half_an_hour_is_probing() {
    // Away 5 min (heard all the while).
    let p = probe(360.0);
    let drones = [Drone::new("PROBE-1", &p)];
    let mut sim = Sim::new(&[HOUSE]);
    let seen = Seen::run(&mut sim, &drones, 600, &["PROBE-1"]);
    assert_eq!(seen.first("PROBE-1", Rule::Probing).map(|s| s < 300), Some(false), "not on the first visit");
    let hit = seen.first("PROBE-1", Rule::Probing).expect("probing");
    // Back across the fence about 45 s after turning back.
    assert!((400..=420).contains(&hit), "{hit}");
    // Probing and "in the area" are two signs: a Warning.
    assert_eq!(seen.at("PROBE-1", hit), Some(Level::Warning));
    let r = *reason(&sim, "PROBE-1", Rule::Probing).unwrap();
    assert!(r.for_s().unwrap() > 300, "away {:?}", r.for_s());
    let mut t = std::string::String::new();
    write_reason(&mut t, &r).unwrap();
    assert!(t.starts_with("back over house after 5 min away (probing)") || t.starts_with("back over house after 6 min"), "{t}");
    // It stays while inside, and comes down 30 s after it leaves again.
    let leave = |t: f64| {
        if t < 600.0 {
            probe(360.0)(t)
        } else {
            Flight::Toward { from: at_site(50.0, 0.0), to: at_site(50.0, -900.0), speed_mps: 10.0, alt_from: up(60.0), alt_to: up(60.0) }.at(t - 600.0)
        }
    };
    let drones = [Drone::new("PROBE-1", &leave)];
    let seen = Seen::run(&mut sim, &drones, 700, &["PROBE-1"]);
    let last = seen.last("PROBE-1", Rule::Probing).unwrap();
    assert!((614..=650).contains(&last), "{last}");
    assert!(seen.at("PROBE-1", 699) <= Some(Level::Notice));
}

#[test]
fn out_and_back_after_two_hours_is_not_probing_but_after_twenty_minutes_is() {
    // Leaves (heard to 600 m out), is not heard again until it comes back.
    let p = probe(f64::MAX);
    let back = |t0: f64| {
        move |t: f64| Flight::Toward { from: at_site(50.0, -600.0), to: at_site(50.0, 0.0), speed_mps: 10.0, alt_from: up(60.0), alt_to: up(60.0) }.at(t - t0)
    };
    for (gap_s, probing) in [(7_200u64, false), (1_200, true)] {
        let mut sim = Sim::new(&[HOUSE]);
        sim.run(&[Drone::new("PROBE-2", &p)], 130, |_| {});
        assert!(sim.track("PROBE-2").is_some());
        sim.skip(gap_s);
        let start = sim.s;
        let b = back(start as f64);
        let drones = [Drone { from_s: start, ..Drone::new("PROBE-2", &b) }];
        let seen = Seen::run(&mut sim, &drones, start + 120, &["PROBE-2"]);
        assert_eq!(seen.first("PROBE-2", Rule::Probing).is_some(), probing, "gap {gap_s}");
        assert!(seen.first("PROBE-2", Rule::InArea).is_some());
    }
}

#[test]
fn hovering_at_the_fence_with_rough_positions_is_not_probing() {
    // 16a's jitter case: in and out of the area by the noise alone.
    let edge = at_site(160.0, 0.0);
    let path = move |_t: f64| Flight::Hover { lat: edge.0, lon: edge.1, alt_m: up(50.0) }.at(0.0);
    let drones = [Drone { jitter_m: 15.0, ..Drone::new("FENCE", &path) }];
    let mut sim = Sim::new(&[HOUSE]);
    let seen = Seen::run(&mut sim, &drones, 400, &["FENCE"]);
    assert_eq!(seen.first("FENCE", Rule::Probing), None);
}

// ---------------------------------------------------- 17 repeat visitor --

/// A short visit: hovering just outside the fence (near the house) for a
/// minute, its pilot 500 m west.
fn visit(sim: &mut Sim, serial: &str, pilot: (f64, f64), over: bool) -> Option<Level> {
    let a = if over { hover_at(40.0, 0.0) } else { hover_at(250.0, 0.0) };
    let start = sim.s;
    let drones = [Drone { from_s: start, until_s: start + 60, pilot: Some(pilot), ..Drone::new(serial, &a) }];
    let mut worst = None;
    let mut rep = None;
    sim.run(&drones, start + 60, |s| {
        worst = worst.max(s.level(serial));
        if let Some(r) = reason(s, serial, Rule::RepeatVisitor) {
            rep = Some(*r);
        }
    });
    // The next day, same time.
    let next = (sim.s / DAY_S + 1) * DAY_S;
    sim.skip(next - sim.s);
    let _ = rep;
    worst
}

fn repeat_of(sim: &Sim, serial: &str) -> Option<Reason> {
    reason(sim, serial, Rule::RepeatVisitor).copied()
}

#[test]
fn the_same_drone_on_three_days_is_a_repeat_visitor() {
    let pilot = at_site(-500.0, 0.0);
    let mut sim = Sim::new(&[HOUSE]);
    sim.utc0 = Some(UTC0);
    assert_eq!(visit(&mut sim, "REP-1", pilot, false), Some(Level::Notice));
    assert_eq!(visit(&mut sim, "REP-1", pilot, false), Some(Level::Notice));
    assert_eq!(sim.tr.visits().counts(), (1, 1), "one ID and one pilot spot");
    // Day 3: a reason at Notice, "3rd day".
    let a = hover_at(250.0, 0.0);
    let start = sim.s;
    let drones = [Drone { from_s: start, pilot: Some(pilot), ..Drone::new("REP-1", &a) }];
    sim.run(&drones, start + 20, |_| {});
    let r = repeat_of(&sim, "REP-1").expect("repeat visitor");
    assert_eq!((r.level, r.count(), r.by_pilot_spot()), (Level::Notice, Some(3), false));
    assert_eq!(sim.level("REP-1"), Some(Level::Notice));
    let mut t = std::string::String::new();
    write_reason(&mut t, &r).unwrap();
    assert_eq!(t, "3rd day in 2 weeks at house (same drone)");
    // Day 4, over the house: in the area (Caution) raised to Warning.
    let next = (sim.s / DAY_S + 1) * DAY_S;
    sim.skip(next - sim.s);
    assert_eq!(visit(&mut sim, "REP-1", pilot, true), Some(Level::Warning));
    // ...and the level comes down 30 s after it leaves (and nothing stays
    // raised the next day until it is back near the house).
    let o = hover_at(250.0, 0.0);
    let start = sim.s;
    let far = hover_at(3000.0, 0.0);
    let path = move |t: f64| if t < start as f64 + 30.0 { o(t) } else { far(t) };
    let drones = [Drone { from_s: start, pilot: Some(pilot), ..Drone::new("REP-1", &path) }];
    let seen = Seen::run(&mut sim, &drones, start + 80, &["REP-1"]);
    assert!(seen.first("REP-1", Rule::RepeatVisitor).is_some());
    assert_eq!(seen.at("REP-1", start + 79), Some(Level::Notice));
    assert!(repeat_of(&sim, "REP-1").is_none());
    // 31 days later the history is gone: no repeat on one more visit.
    sim.skip(31 * DAY_S);
    let drones = [Drone { from_s: sim.s, pilot: Some(pilot), ..Drone::new("REP-1", &a) }];
    let to = sim.s + 20;
    sim.run(&drones, to, |_| {});
    assert!(repeat_of(&sim, "REP-1").is_none());
    assert_eq!(sim.tr.visits().counts(), (1, 1), "only today's visit");
}

#[test]
fn the_same_pilot_spot_with_new_drones_is_a_repeat_visitor() {
    let mut sim = Sim::new(&[HOUSE]);
    sim.utc0 = Some(UTC0);
    // A different drone each day, the pilot within 20 m of the same spot.
    visit(&mut sim, "SPOT-D1", at_site(-500.0, 0.0), false);
    visit(&mut sim, "SPOT-D2", at_site(-485.0, 10.0), false);
    let a = hover_at(250.0, 0.0);
    let start = sim.s;
    let drones = [Drone { from_s: start, pilot: Some(at_site(-510.0, -8.0)), ..Drone::new("SPOT-D3", &a) }];
    sim.run(&drones, start + 20, |_| {});
    let r = repeat_of(&sim, "SPOT-D3").expect("same pilot spot");
    assert!(r.by_pilot_spot());
    assert_eq!(r.count(), Some(3));
    // A pilot 300 m from there is someone else.
    let mut sim = Sim::new(&[HOUSE]);
    sim.utc0 = Some(UTC0);
    visit(&mut sim, "SPOT-E1", at_site(-500.0, 0.0), false);
    visit(&mut sim, "SPOT-E2", at_site(-500.0, 300.0), false);
    visit(&mut sim, "SPOT-E3", at_site(-200.0, 0.0), false);
    assert!(repeat_of(&sim, "SPOT-E3").is_none());
}

#[test]
fn friends_off_wipe_and_no_utc_keep_no_history() {
    let pilot = at_site(-500.0, 0.0);
    // A friend is never recorded.
    let mut sim = Sim::new(&[HOUSE, "friend OUR-MINI"]);
    sim.utc0 = Some(UTC0);
    for _ in 0..4 {
        visit(&mut sim, "OUR-MINI", pilot, false);
    }
    assert_eq!(sim.tr.visits().counts(), (0, 0));
    // visits off: nothing kept, and switching it off wipes what was.
    let mut sim = Sim::new(&[HOUSE, "visits off"]);
    sim.utc0 = Some(UTC0);
    for _ in 0..4 {
        visit(&mut sim, "REP-2", pilot, false);
    }
    assert_eq!(sim.tr.visits().counts(), (0, 0));
    let mut sim = Sim::new(&[HOUSE]);
    sim.utc0 = Some(UTC0);
    visit(&mut sim, "REP-3", pilot, false);
    assert_eq!(sim.tr.visits().counts(), (1, 1));
    sim.tr.places_mut().visits = false;
    sim.step(&[]);
    assert_eq!(sim.tr.visits().counts(), (0, 0));
    assert!(sim.tr.visits_mut().take_dirty(), "the hub then rewrites the file empty");
    // No UTC: nothing recorded (a day cannot be told).
    let mut sim = Sim::new(&[HOUSE]);
    for _ in 0..4 {
        visit(&mut sim, "REP-4", pilot, false);
    }
    assert_eq!(sim.tr.visits().counts(), (0, 0));
    // The operator's wipe.
    let mut sim = Sim::new(&[HOUSE]);
    sim.utc0 = Some(UTC0);
    visit(&mut sim, "REP-5", pilot, false);
    visit(&mut sim, "REP-5", pilot, false);
    assert_eq!(sim.tr.visits_mut().clear(), (1, 1));
    visit(&mut sim, "REP-5", pilot, false);
    let a = hover_at(250.0, 0.0);
    let drones = [Drone { from_s: sim.s, pilot: Some(pilot), ..Drone::new("REP-5", &a) }];
    let to = sim.s + 20;
    sim.run(&drones, to, |_| {});
    assert!(repeat_of(&sim, "REP-5").is_none(), "only two days since the wipe");
}

#[test]
fn the_history_survives_a_restart_through_the_file() {
    let pilot = at_site(-500.0, 0.0);
    let mut sim = Sim::new(&[HOUSE]);
    sim.utc0 = Some(UTC0);
    visit(&mut sim, "REP-6", pilot, false);
    visit(&mut sim, "REP-6", pilot, false);
    let mut file = Vec::new();
    assert!(sim.tr.visits().write(|c| {
        file.extend_from_slice(c);
        true
    }));
    // A new hub (a reboot) reads the card, then the drone is back.
    let mut fresh = Sim::new(&[HOUSE]);
    fresh.utc0 = Some(UTC0);
    fresh.skip(sim.s);
    let mut l = visits::Loader::new();
    for c in file.chunks(512) {
        l.feed(fresh.tr.visits_mut(), c);
    }
    assert_eq!(l.finish(fresh.tr.visits_mut(), None), Ok(visits::Loaded { ids: 1, spots: 1 }));
    let a = hover_at(250.0, 0.0);
    let drones = [Drone { from_s: fresh.s, pilot: Some(pilot), ..Drone::new("REP-6", &a) }];
    let to = fresh.s + 20;
    fresh.run(&drones, to, |_| {});
    assert_eq!(repeat_of(&fresh, "REP-6").and_then(|r| r.count()), Some(3));
    // A damaged file is discarded whole.
    let mut bad = file.clone();
    bad[20] ^= 1;
    let mut other = Sim::new(&[HOUSE]);
    let mut l = visits::Loader::new();
    l.feed(other.tr.visits_mut(), &bad);
    assert_eq!(l.finish(other.tr.visits_mut(), None), Err(visits::LoadError::Checksum));
    assert_eq!(other.tr.visits().counts(), (0, 0));
}

// ------------------------------------------------- 18 pilot on the move --

#[test]
fn a_pilot_driving_at_15_mps_is_on_the_move_a_walking_one_is_not() {
    // The drone hovers just outside the fence; its pilot drives east along
    // a road 400 m south for two minutes, then parks.
    let a = hover_at(250.0, 0.0);
    let road = |t: f64| {
        let d = t.min(120.0) * 15.0;
        at_site(-900.0 + d, -400.0)
    };
    let drones = [Drone { pilot_at: Some(&road), ..Drone::new("DRIVE-1", &a) }];
    let mut sim = Sim::new(&[HOUSE]);
    let seen = Seen::run(&mut sim, &drones, 220, &["DRIVE-1"]);
    let hit = seen.first("DRIVE-1", Rule::PilotMoving).expect("pilot on the move");
    assert!((30..=50).contains(&hit), "needs 30 s of trail: {hit}");
    assert_eq!(seen.at("DRIVE-1", hit), Some(Level::Caution));
    let last = seen.last("DRIVE-1", Rule::PilotMoving).unwrap();
    assert!((120..=175).contains(&last), "parked at 120 s, then the 30 s hold: {last}");
    assert_eq!(seen.at("DRIVE-1", 219), Some(Level::Notice));
    // Over the house instead: two signs, a Warning.
    let o = hover_at(40.0, 0.0);
    let drones = [Drone { pilot_at: Some(&road), ..Drone::new("DRIVE-2", &o) }];
    let mut sim = Sim::new(&[HOUSE]);
    let seen = Seen::run(&mut sim, &drones, 80, &["DRIVE-2"]);
    assert_eq!(seen.at("DRIVE-2", 79), Some(Level::Warning));
    let mut t = std::string::String::new();
    write_reason(&mut t, reason(&sim, "DRIVE-2", Rule::PilotMoving).unwrap()).unwrap();
    assert_eq!(t, "pilot on the move at 15 m/s while it flies at house");
    // A pilot walking (1.5 m/s) with a jittery phone: nothing.
    let walk = |t: f64| at_site(-500.0 + t * 1.5 + if (t as u64) % 2 == 0 { 6.0 } else { -6.0 }, -100.0);
    let drones = [Drone { pilot_at: Some(&walk), ..Drone::new("WALK-1", &a) }];
    let mut sim = Sim::new(&[HOUSE]);
    let seen = Seen::run(&mut sim, &drones, 200, &["WALK-1"]);
    assert_eq!(seen.first("WALK-1", Rule::PilotMoving), None);
    assert_eq!(seen.worst("WALK-1"), Some(Level::Notice));
    // Driving, but the drone is nowhere near a place: nothing.
    let f = hover_at(3000.0, 0.0);
    let drones = [Drone { pilot_at: Some(&road), ..Drone::new("DRIVE-3", &f) }];
    let mut sim = Sim::new(&[HOUSE]);
    let seen = Seen::run(&mut sim, &drones, 100, &["DRIVE-3"]);
    assert_eq!(seen.first("DRIVE-3", Rule::PilotMoving), None);
}

// ---------------------------------------------------- 19 identity changes --

#[test]
fn a_new_identity_mid_flight_near_the_house_warns_two_drones_do_not() {
    // East at 6 m/s along a line 200 m north of the middle (just outside
    // the fence: near the house). At second 100 the drone falls silent and
    // the same path carries on under another serial and address.
    let line = |t: f64| Flight::Line { from: at_site(-800.0, 200.0), course_deg: 90.0, speed_mps: 6.0, alt_m: up(60.0) }.at(t);
    let drones = [Drone { until_s: 100, ..Drone::new("IDA-1", &line) }, Drone { from_s: 103, ..Drone::new("IDB-1", &line) }];
    let mut sim = Sim::new(&[HOUSE]);
    let seen = Seen::run(&mut sim, &drones, 260, &["IDB-1"]);
    let b = sim.track("IDB-1").unwrap();
    assert!(b.continuity.identity_changed, "{:?}", b.continuity);
    let hit = seen.first("IDB-1", Rule::IdentityChanged).expect("identity change");
    assert!(hit <= 112, "{hit}");
    assert_eq!(seen.at("IDB-1", hit), Some(Level::Warning));
    // Once past the house (more than 300 m east of the middle) it stops,
    // and the level comes down 30 s later.
    let last = seen.last("IDB-1", Rule::IdentityChanged).unwrap();
    assert!(seen.at("IDB-1", last + DECAY_MS / 1000 + 1) <= Some(Level::Notice));
    let mut sim = Sim::new(&[HOUSE]);
    sim.run(&drones, 115, |_| {});
    let a_id = sim.track("IDA-1").unwrap().id;
    let r = *reason(&sim, "IDB-1", Rule::IdentityChanged).unwrap();
    assert_eq!(r.other(), Some(a_id));
    let text = sim.tr.describe_level(sim.track("IDB-1").unwrap().id, sim.now()).unwrap();
    assert!(text.contains("new identity mid-flight at house (was drone IDA-1, "), "{text}");
    // Two different drones: both heard at once, the second elsewhere.
    let other = |t: f64| Flight::Line { from: at_site(-800.0, -200.0), course_deg: 90.0, speed_mps: 6.0, alt_m: up(60.0) }.at(t);
    let drones = [Drone::new("IDA-2", &line), Drone { from_s: 103, ..Drone::new("IDB-2", &other) }];
    let mut sim = Sim::new(&[HOUSE]);
    let seen = Seen::run(&mut sim, &drones, 200, &["IDB-2"]);
    assert_eq!(seen.first("IDB-2", Rule::IdentityChanged), None);
    // One stops and a different one starts 2 km away: no link.
    let far = |t: f64| Flight::Line { from: at_site(-800.0, 2200.0), course_deg: 180.0, speed_mps: 6.0, alt_m: up(60.0) }.at(t);
    let drones = [Drone { until_s: 100, ..Drone::new("IDA-3", &line) }, Drone { from_s: 103, ..Drone::new("IDB-3", &far) }];
    let mut sim = Sim::new(&[HOUSE]);
    let seen = Seen::run(&mut sim, &drones, 200, &["IDB-3"]);
    assert_eq!(seen.first("IDB-3", Rule::IdentityChanged), None);
}

#[test]
fn a_serial_changing_within_one_track_warns() {
    // Same transmitter, same registration; the serial changes at 60 s. The
    // registration keeps it one track, which takes the new serial once it
    // persists (5 s, `claims`).
    let a = hover_at(250.0, 0.0);
    let drones = [Drone { rename: Some((60, "SER-NEW")), reg: Some("FA-REG-1"), ..Drone::new("SER-OLD", &a) }];
    let mut sim = Sim::new(&[HOUSE]);
    let mut first = None;
    sim.run(&drones, 120, |s| {
        if first.is_none() && reason(s, "SER-NEW", Rule::IdentityChanged).is_some() {
            first = Some(s.s);
        }
    });
    assert_eq!(sim.tr.len(), 1, "one track");
    let hit = first.expect("identity change");
    assert!((60..=75).contains(&hit), "{hit}");
    assert_eq!(sim.level("SER-NEW"), Some(Level::Warning));
    let r = *reason(&sim, "SER-NEW", Rule::IdentityChanged).unwrap();
    assert_eq!(r.other(), None);
    let mut t = std::string::String::new();
    write_reason(&mut t, &r).unwrap();
    assert!(t.starts_with("serial changed mid-flight at house, "), "{t}");
    // Without the registration the new serial is a new track, which
    // continuity links to the old one: rule 19 as well, naming it.
    let drones = [Drone { rename: Some((60, "SER-NEW")), ..Drone::new("SER-OLD", &a) }];
    let mut sim = Sim::new(&[HOUSE]);
    sim.run(&drones, 80, |_| {});
    let old = sim.track("SER-OLD").unwrap().id;
    assert_eq!(reason(&sim, "SER-NEW", Rule::IdentityChanged).and_then(|r| r.other()), Some(old));
    // A drone that keeps its serial: nothing.
    let drones = [Drone { reg: Some("FA-REG-2"), ..Drone::new("SER-SAME", &a) }];
    let mut sim = Sim::new(&[HOUSE]);
    let seen = Seen::run(&mut sim, &drones, 120, &["SER-SAME"]);
    assert_eq!(seen.first("SER-SAME", Rule::IdentityChanged), None);
}

// --------------------------------------------------------------- output --

#[test]
fn new_reasons_reach_json_and_the_tracker_stays_small() {
    let strike = striker(240.0);
    let drones = [Drone::new("SPOT-J", &spotter), Drone { from_s: 240, ..Drone::new("STRK-J", &strike) }];
    let mut sim = Sim::new(&[HOUSE]);
    let mut hit = None;
    sim.run(&drones, 480, |s| {
        if hit.is_none() && reason(s, "STRK-J", Rule::SpotterStriker).is_some() {
            hit = Some(s.s);
        }
    });
    let mut sim = Sim::new(&[HOUSE]);
    sim.run(&drones, hit.unwrap() + 1, |_| {});
    let now = sim.now() - 1000;
    let ctx = sim.tr.context(now, Some(UTC0));
    let mut buf = [0u8; crate::json::MAX_LINE];
    let t = sim.track("STRK-J").unwrap();
    let n = crate::json::track_line(t, &ctx, &mut buf).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&buf[..n]).unwrap();
    let r = v["threat"]["reasons"].as_array().unwrap().iter().find(|r| r["rule"] == "spotter_striker").unwrap().clone();
    assert_eq!(r["place"], "house");
    assert_eq!(r["other"], sim.track("SPOT-J").unwrap().id.0);
    assert!(r["ago_s"].as_u64().unwrap() > 60);
    assert!(r.get("eta_s").is_none() && r.get("mps").is_none(), "{r}");
    let mut cot = [0u8; crate::cot::MAX_EVENT];
    let n = crate::cot::drone_event(t, &ctx, &mut cot).unwrap();
    let x = core::str::from_utf8(&cot[..n]).unwrap();
    assert!(x.contains("follows spotter drone #"), "{x}");
    // Sizes: the P4 tracker stays well under 1 MiB.
    let hub = core::mem::size_of::<super::tests::Tracker>();
    std::println!(
        "Tracker<64,..,64> with 16d: {hub} B (across {} B, visits {} B, watch {} B)",
        core::mem::size_of::<Across<64>>(),
        core::mem::size_of::<Visits<64>>(),
        core::mem::size_of::<watch::Watch>()
    );
    assert!(hub < 1 << 20, "{hub}");
    assert!(core::mem::size_of::<Across<0>>() <= 32, "{}", core::mem::size_of::<Across<0>>());
}

/// The worst-case JSON line (`crate::tests::json_worst_case_fits`) uses the
/// four widest reasons of all rules, the new ones included.
#[test]
fn the_worst_case_json_uses_the_widest_reasons() {
    let a = hover_at(250.0, 0.0);
    let mut sim = Sim::new(&[HOUSE]);
    sim.run(&[Drone::new("WIDE", &a)], 5, |_| {});
    let ctx = sim.tr.context(sim.now(), None);
    let all = [
        Rule::InArea,
        Rule::PilotNear,
        Rule::PilotOutside,
        Rule::AboveCeiling,
        Rule::Beeline,
        Rule::FastInbound,
        Rule::Loitering,
        Rule::Circling,
        Rule::LowHover,
        Rule::Drop,
        Rule::Following,
        Rule::SpotterStriker,
        Rule::Several,
        Rule::Probing,
        Rule::RepeatVisitor,
        Rule::PilotMoving,
        Rule::IdentityChanged,
        Rule::LastSeenInbound,
        Rule::Spotter,
    ];
    let mut widths: Vec<(usize, Rule)> = all
        .iter()
        .map(|&rule| {
            let mut t = sim.track("WIDE").unwrap().clone();
            t.threat.reasons.clear();
            t.threat.reasons.push(TrackThreat::wide_for_tests(rule)).unwrap();
            let mut buf = [0u8; crate::json::MAX_LINE];
            (crate::json::track_line(&t, &ctx, &mut buf).unwrap(), rule)
        })
        .collect();
    // "Last seen inbound" is wider still, but a held drone has only that
    // one reason.
    widths.retain(|w| w.1 != Rule::LastSeenInbound);
    widths.sort_by(|a, b| b.0.cmp(&a.0));
    let top: Vec<Rule> = widths.iter().take(4).map(|w| w.1).collect();
    for r in TrackThreat::WORST_RULES {
        assert!(top.contains(&r), "{widths:?}");
    }
    // And every reason's words fit the CoT remark (96) and the strip.
    for &rule in &all {
        let r = TrackThreat::wide_for_tests(rule);
        let mut t: heapless::String<96> = heapless::String::new();
        assert!(write_reason_named(&mut t, &r, Some("12345678901234567890")).is_ok(), "{rule:?}: {t}");
    }
}
