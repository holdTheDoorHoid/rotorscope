//! The bench test transmitter's scripted flights (`firmware/rid-beacon`,
//! feature `scripted`, src/script.rs) and its default circle (src/flight.rs),
//! flown through the whole tracker on the host.
//!
//! Both files are compiled here with `#[path]`, so these tests fly the very
//! code the beacon runs, and say what the P4 hub should show when the beacon
//! flies them over the air (docs/BENCH.md "Bench results, 2026-11-19 (threat
//! profiles)"). Run with `--nocapture` to print the seconds each level is
//! first reached.
//!
//! The beacon's heights are above the ground under it; here the hub stands
//! at that ground ([`OWN_ALT`]), so "above the unit" is the beacon's height.
//! The scripted paths send no pilot position; the circle's pilot stands at
//! its centre, as the beacon says.

extern crate std;
use std::vec::Vec;

use super::scenario::Sample;
use super::tests::{Drone, HOUSE, OWN_ALT, SITE, Sim};
use super::*;

#[allow(dead_code)]
#[path = "../../../../firmware/rid-beacon/src/script.rs"]
mod beacon_script;
#[allow(dead_code)]
#[path = "../../../../firmware/rid-beacon/src/flight.rs"]
mod beacon_flight;

use beacon_script::Path;

const ID: &str = "TESTDRONE0000000001";

/// A scripted path round [`SITE`] as the tracker's flight sample.
fn scripted(p: Path) -> impl Fn(f64) -> Sample {
    move |t: f64| {
        let s = beacon_script::at(p, SITE.0, SITE.1, t).expect("a scripted path");
        Sample {
            lat: s.lat,
            lon: s.lon,
            alt_m: OWN_ALT as f32 + s.height_m,
            speed_mps: s.speed_mps,
            course_deg: s.heading_deg,
            vspeed_mps: s.vspeed_mps,
        }
    }
}

/// The default circle round [`SITE`] (50 m, 60 s a lap, 50 m up).
fn circle(t: f64) -> Sample {
    let c = beacon_flight::Centre { lat: SITE.0, lon: SITE.1, from_env_ok: true };
    let f = beacon_flight::at(&c, t);
    Sample {
        lat: f.lat,
        lon: f.lon,
        alt_m: OWN_ALT as f32 + beacon_flight::HEIGHT_AGL_M,
        speed_mps: f.speed_mps,
        course_deg: f.heading_deg,
        vspeed_mps: 0.0,
    }
}

/// What a run showed: the worst level, the first second each level was
/// reached, and every rule seen.
#[derive(Debug, Default)]
struct Seen {
    worst: Option<Level>,
    first: [Option<u64>; 5],
    rules: Vec<Rule>,
}

impl Seen {
    fn first(&self, l: Level) -> Option<u64> {
        self.first[l as usize]
    }
}

fn fly(lines: &[&str], drone: Drone<'_>, secs: u64) -> Seen {
    let mut sim = Sim::new(lines);
    let mut seen = Seen::default();
    sim.run(&[drone], secs, |s| {
        let l = s.level(ID);
        if let Some(l) = l {
            for k in 1..=(l as usize) {
                seen.first[k].get_or_insert(s.s);
            }
        }
        seen.worst = seen.worst.max(l);
        for r in s.rules(ID) {
            if !seen.rules.contains(&r) {
                seen.rules.push(r);
            }
        }
    });
    seen
}

fn path(lines: &[&str], p: Path, secs: u64) -> Seen {
    let f = scripted(p);
    let s = fly(lines, Drone::new(ID, &f), secs);
    std::println!("bench path {:<14} {:?}: worst {:?}, first {:?}, rules {:?}", p.name(), lines, s.worst, &s.first[1..], s.rules);
    s
}

#[test]
fn the_beacon_paths_go_where_they_say() {
    use crate::geo::range_bearing;
    let at = |p: Path, t: f64| beacon_script::at(p, SITE.0, SITE.1, t).unwrap();
    let rb = |s: &beacon_script::Sample| range_bearing(SITE.0, SITE.1, s.lat, s.lon);
    // The beacon's flat map is on a sphere, `range_bearing` on the
    // ellipsoid: they differ by up to about 0.3 % here.
    let near = |d: f64, want: f64| (d - want).abs() <= 1.0 + want * 0.004;
    assert!(beacon_script::at(Path::Circle, SITE.0, SITE.1, 3.0).is_none(), "the circle is flight.rs");
    // (a) Beeline: 800 m south, 12 m/s north, 50 m up coming down to 30.
    let b = at(Path::Beeline, 0.0);
    assert!(near(rb(&b).distance_m, 800.0) && (rb(&b).bearing_deg - 180.0).abs() < 0.5, "{:?}", rb(&b));
    assert!(b.heading_deg < 0.5 && (b.speed_mps - 12.0).abs() < 1e-3 && b.height_m == 50.0 && b.vspeed_mps < 0.0);
    let b = at(Path::Beeline, 40.0);
    assert!(near(rb(&b).distance_m, 320.0) && (b.height_m - 38.0).abs() < 0.1, "{}", b.height_m);
    let b = at(Path::Beeline, 80.0);
    assert!(rb(&b).distance_m < 0.5 && b.speed_mps == 0.0 && b.height_m == 30.0, "arrived: hovering over the centre");
    // (b) Fast inbound: 1500 m east, 30 m/s west.
    let f = at(Path::Fast, 10.0);
    assert!(near(rb(&f).distance_m, 1200.0) && (rb(&f).bearing_deg - 90.0).abs() < 0.5, "{:?}", rb(&f));
    assert!((f.heading_deg - 270.0).abs() < 0.5 && (f.speed_mps - 30.0).abs() < 1e-3);
    assert!(rb(&at(Path::Fast, 60.0)).distance_m < 0.5);
    // (c) The pass: never nearer than 300 m, due north of the centre at 66.7 s.
    let mut nearest = f64::MAX;
    for t in 0..200 {
        nearest = nearest.min(rb(&at(Path::Pass, f64::from(t))).distance_m);
    }
    assert!(near(nearest, 300.0), "{nearest}");
    let p = at(Path::Pass, 1000.0 / 15.0);
    assert!(rb(&p).bearing_deg.abs() < 0.5 || (rb(&p).bearing_deg - 360.0).abs() < 0.5);
    assert!((p.heading_deg - 90.0).abs() < 0.5 && p.height_m == 100.0);
    // (d) Low hover: 40 m, down at 2 m/s, then 10 m from second 15.
    assert_eq!(at(Path::Hover, 0.0).height_m, 40.0);
    assert_eq!(at(Path::Hover, 5.0).height_m, 30.0);
    assert!(at(Path::Hover, 20.0).height_m == 10.0 && rb(&at(Path::Hover, 20.0)).distance_m < 0.5);
    // (e) Drop: 50 m; down 30 m at 3 m/s from second 40; 5 s; back up.
    let d = |t: f64| at(Path::Drop, t);
    assert!(d(10.0).height_m == 50.0 && near(rb(&d(10.0)).distance_m, 36.06));
    assert!((d(45.0).height_m - 35.0).abs() < 1e-3 && d(45.0).vspeed_mps < 0.0);
    assert_eq!(d(52.0).height_m, 20.0);
    assert!((d(60.0).height_m - 35.0).abs() < 1e-3 && d(60.0).vspeed_mps > 0.0);
    assert_eq!(d(80.0).height_m, 50.0);
    // The console keys.
    for p in beacon_script::PATHS {
        assert_eq!(Path::from_key(p.key()), Some(p));
    }
    assert_eq!(Path::from_key(b'6'), None, "6-9 stay the channel keys");
    assert_eq!(beacon_script::command(b'2', 1_000), Some(Path::Fast));
    assert_eq!(beacon_script::current(4_500), (Path::Fast, 3.5));
    assert_eq!(beacon_script::command(b'x', 9_000), None);
    assert_eq!(beacon_script::current(9_000).0, Path::Fast, "other keys leave the path alone");
}

/// What the bench should see with `area house circle ... 150` round the
/// beacon's centre, default profile.
#[test]
fn the_beacon_paths_reach_the_levels_the_bench_expects() {
    // (a) Beeline at 12 m/s from 800 m: Warning, then Critical (closing
    // at 12 m/s, under 30 s from the edge).
    let s = path(&[HOUSE], Path::Beeline, 120);
    assert_eq!(s.worst, Some(Level::Critical));
    assert!(s.rules.contains(&Rule::Beeline) && s.rules.contains(&Rule::FastInbound), "{:?}", s.rules);
    assert!(s.first(Level::Warning).unwrap() < s.first(Level::Critical).unwrap());
    assert!(s.first(Level::Critical).unwrap() <= 40, "{:?}", s.first);
    // (b) Fast inbound at 30 m/s: Critical well before it arrives (50 s).
    let s = path(&[HOUSE], Path::Fast, 80);
    assert_eq!(s.worst, Some(Level::Critical));
    assert!(s.rules.contains(&Rule::FastInbound));
    assert!(s.first(Level::Critical).unwrap() <= 25, "{:?}", s.first);
    // (c) The pass 300 m off (150 m beyond the fence): Notice at most.
    let s = path(&[HOUSE], Path::Pass, 200);
    assert!(s.worst <= Some(Level::Notice), "{:?} {:?}", s.worst, s.rules);
    // (d) Low hover at 10 m over the house: Warning (rule 9).
    let s = path(&[HOUSE], Path::Hover, 60);
    assert_eq!(s.worst, Some(Level::Warning));
    assert!(s.rules.contains(&Rule::LowHover), "{:?}", s.rules);
    // (e) The drop: Warning (rule 10) once it climbs again.
    let s = path(&[HOUSE], Path::Drop, 100);
    assert_eq!(s.worst, Some(Level::Warning));
    assert!(s.rules.contains(&Rule::Drop), "{:?}", s.rules);
    assert!(s.first(Level::Warning).unwrap() >= 50, "not before it climbs: {:?}", s.first);
}

/// The default circle (its pilot at the centre): in the area with the
/// pilot in it is two signs, a Warning at once; circling too after a lap.
/// As a friend: Notice.
#[test]
fn the_beacon_circle_with_its_pilot_and_as_a_friend() {
    let pilot = Some(SITE);
    let s = fly(&[HOUSE], Drone { pilot, ..Drone::new(ID, &circle) }, 200);
    std::println!("bench circle: worst {:?}, first {:?}, rules {:?}", s.worst, &s.first[1..], s.rules);
    assert_eq!(s.worst, Some(Level::Warning));
    for r in [Rule::InArea, Rule::PilotNear, Rule::Circling] {
        assert!(s.rules.contains(&r), "{r:?} {:?}", s.rules);
    }
    let friend = std::format!("friend {ID} bench beacon");
    let s = fly(&[HOUSE, &friend], Drone { pilot, ..Drone::new(ID, &circle) }, 200);
    std::println!("bench circle, friend: worst {:?}, rules {:?}", s.worst, s.rules);
    assert_eq!(s.worst, Some(Level::Notice));
    // Under `profile event` a pilot nearby counts one level lower: the
    // circle is only "in the area" (Caution) until the circling makes it
    // two signs (a Warning, later than under the default profile).
    let def = fly(&[HOUSE], Drone { pilot, ..Drone::new(ID, &circle) }, 200);
    let ev = fly(&[HOUSE, "profile event"], Drone { pilot, ..Drone::new(ID, &circle) }, 200);
    std::println!("bench circle, event: worst {:?}, first {:?}, rules {:?}", ev.worst, &ev.first[1..], ev.rules);
    assert_eq!(ev.worst, Some(Level::Warning));
    assert!(ev.first(Level::Warning).unwrap() > def.first(Level::Warning).unwrap() + 30, "{:?} {:?}", ev.first, def.first);
    // A friend on the beeline is still Critical (never lowered).
    let s = path(&[HOUSE, &friend], Path::Beeline, 120);
    assert_eq!(s.worst, Some(Level::Critical));
}

/// Every profile keeps the beeline and the fast inbound Critical, and the
/// pass at Notice; home sees the low hover sooner (20 m for 6 s).
#[test]
fn the_beacon_paths_under_each_profile() {
    for prof in ["profile default", "profile home", "profile facility", "profile event", "profile patrol"] {
        assert_eq!(path(&[HOUSE, prof], Path::Beeline, 120).worst, Some(Level::Critical), "{prof}");
        assert_eq!(path(&[HOUSE, prof], Path::Fast, 80).worst, Some(Level::Critical), "{prof}");
        assert!(path(&[HOUSE, prof], Path::Pass, 200).worst <= Some(Level::Notice), "{prof}");
    }
    let def = path(&[HOUSE], Path::Hover, 60).first(Level::Warning).unwrap();
    let home = path(&[HOUSE, "profile home"], Path::Hover, 60).first(Level::Warning).unwrap();
    assert!(home < def, "home {home} default {def}");
    let def = path(&[HOUSE], Path::Beeline, 120).first(Level::Critical).unwrap();
    let patrol = path(&[HOUSE, "profile patrol"], Path::Beeline, 120).first(Level::Critical).unwrap();
    assert!(patrol <= def, "patrol {patrol} default {def}");
}

/// The beacon silenced mid-beeline (`x` on its console): the level is held
/// about 30 s from when it was last heard, then comes down.
#[test]
fn the_beacon_silenced_mid_beeline_is_held_then_decays() {
    let f = scripted(Path::Beeline);
    let mut sim = Sim::new(&[HOUSE]);
    let stop = 45;
    let drone = Drone { until_s: stop, ..Drone::new(ID, &f) };
    let mut at_stop = None;
    let mut fell = None;
    sim.run(&[drone], 150, |s| {
        if s.s == stop - 1 {
            at_stop = s.level(ID);
        }
        if s.s >= stop && fell.is_none() && s.level(ID) < at_stop {
            fell = Some(s.s - stop);
        }
    });
    assert_eq!(at_stop, Some(Level::Critical));
    let fell = fell.expect("comes down");
    std::println!("bench beeline silenced at {stop} s: held {fell} s");
    assert!((29..=32).contains(&fell), "{fell}");
}

/// The bubble round a hub standing still (on the bench: the `gpsloop`
/// emulated fix, at the beacon's centre), no fixed area: the beeline and
/// the fast inbound are Critical at the bubble too; the pass 300 m off
/// stays Notice outside a 150 m bubble and along a 300 m one's edge.
#[test]
fn the_beacon_paths_at_a_standing_bubble() {
    let s = path(&["bubble 300"], Path::Beeline, 120);
    assert_eq!(s.worst, Some(Level::Critical));
    assert!(s.rules.contains(&Rule::Beeline), "{:?}", s.rules);
    assert_eq!(path(&["bubble 300"], Path::Fast, 80).worst, Some(Level::Critical));
    assert!(path(&["bubble 150"], Path::Pass, 200).worst <= Some(Level::Notice));
    // The same pass grazes a 300 m bubble (its nearest point is on the
    // edge, never inside it): still Notice. The bench flew it that way.
    assert!(path(&["bubble 300"], Path::Pass, 200).worst <= Some(Level::Notice));
}

/// As the hub hears the beacon: reports at uneven times (Wi-Fi beacon and
/// NAN through one node), judged once a second. The path keeps one point a
/// second, so its newest point can be up to a second older than the last
/// report, and the approach rules lapse one judgement before the position
/// goes stale. Bench 2026-11-19: the level was still kept 30 s, but not as
/// "last seen inbound" (`held` false, the stale "there in about 5 s" reason
/// shown for 30 s). It must be held as the lost-track rule says.
#[test]
fn a_beeline_lost_between_reports_is_held_as_last_seen_inbound() {
    use super::tests::{T0, frame};
    use rid::Transport;
    let f = scripted(Path::Beeline);
    let mut sim = Sim::new(&[HOUSE]);
    let stop_ms = 45_000;
    let mut seen_held = None;
    let mut fell = None;
    for ms in (0..130_000u64).step_by(100) {
        let now = T0 + ms;
        // Reports at x.9 s while the beacon is on, and its last one 0.4 s
        // after the one before (44.3 s): the path keeps 43.9 s as its newest
        // point (one a second), the position is 44.3 s old.
        if (ms < stop_ms - 1000 && ms % 1000 == 900) || ms == stop_ms - 700 {
            let p = f(ms as f64 / 1000.0);
            let d = crate::tests::det(Transport::WifiNan, [0x02, 0x52, 0x49, 0x44, 0, 0x43], -52, &frame(ID, &p, None));
            sim.tr.ingest(1, &d, now);
        }
        if ms % 1000 == 0 {
            sim.tr.set_own_fix(SITE.0, SITE.1, Some(OWN_ALT), now, Some((0.0, None)));
            sim.tr.expire(now);
            sim.tr.tick_behaviour(now);
            let t = sim.tr.tracks().iter().find(|t| t.primary_id().and_then(|u| u.id.as_str()) == Some(ID));
            let (level, held, rules) = t.map_or((None, false, Vec::new()), |t| {
                (t.threat.level, t.threat.held, t.threat.reasons.iter().map(|r| r.rule).collect::<Vec<_>>())
            });
            if ms == stop_ms - 1000 {
                assert_eq!(level, Some(Level::Critical));
            }
            if ms >= stop_ms + 10_000 && seen_held.is_none() {
                seen_held = Some((held, rules));
            }
            if ms >= stop_ms && fell.is_none() && level < Some(Level::Critical) {
                fell = Some((ms - stop_ms) / 1000);
            }
        }
    }
    let (held, rules) = seen_held.unwrap();
    assert!(held, "held after it was lost: {rules:?}");
    assert_eq!(rules, [Rule::LastSeenInbound]);
    let fell = fell.expect("comes down");
    assert!((29..=32).contains(&fell), "{fell}");
}
