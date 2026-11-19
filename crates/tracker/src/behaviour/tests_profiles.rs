//! Slice 16e: the false-alarm matrix. Every scripted flight, ordinary and
//! hostile, flown under each of the five profiles ([`super::profile`]),
//! with the worst level a person would have seen asserted for each pair
//! (and, for the hostile ones, the second it was first reached). Ordinary
//! flights must stay at or below the level written here, hostile ones must
//! reach it. The same table is in docs/THREAT-PROFILES.md ("Slice 16e as
//! built"); run with `--nocapture` to print it.
//!
//! Ordinary flights are the false alarms of THREAT-TACTICS.md §6 and the
//! "expected false alarms" of slices 16a-16d. Uses the 16a simulation
//! ([`Sim`]): one report a second through the whole tracker.

extern crate std;
use std::string::String;
use std::vec::Vec;

use super::profile::PROFILES;
use super::scenario::{Flight, Sample, offset};
use super::tests::{Drone, HOUSE, OWN_ALT, SITE, Sim};
use super::*;

/// 2026-11-18 10:00 UTC (as tests_across).
const UTC0: u64 = 1_763_424_000_000 + 10 * 3_600_000;
const DAY_S: u64 = 86_400;
const BUBBLE: &str = "bubble 300";

fn up(m: f32) -> f32 {
    m + OWN_ALT as f32
}

fn at_site(e: f64, n: f64) -> (f64, f64) {
    offset(SITE.0, SITE.1, e, n)
}

fn hover(e: f64, n: f64, h: f32) -> impl Fn(f64) -> Sample {
    let p = at_site(e, n);
    move |_t: f64| Flight::Hover { lat: p.0, lon: p.1, alt_m: up(h) }.at(0.0)
}

/// The worst level any of `who` reached, and the first second it was seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Worst {
    level: Option<Level>,
    at_s: u64,
}

impl Worst {
    fn see(&mut self, s: &Sim, who: &[&str]) {
        for w in who {
            let l = s.level(w);
            if l > self.level {
                *self = Worst { level: l, at_s: s.s };
            }
        }
    }
}

fn fly(sim: &mut Sim, drones: &[Drone<'_>], to: u64, who: &[&str]) -> Worst {
    let mut w = Worst::default();
    sim.run(drones, to, |s| w.see(s, who));
    w
}

fn sim(place: &str, profile: &str) -> Sim {
    Sim::new(&[place, profile])
}

// ------------------------------------------------------ ordinary flights --

/// A real-estate photographer circling the house next door (230 m east, a
/// 60 m circle, 50 m up: it comes within 20 m of our fence), its pilot at
/// that house, 80 m beyond our fence.
fn photographer(profile: &str) -> Worst {
    let next_door = at_site(230.0, 0.0);
    let path = move |t: f64| Flight::Orbit { centre: next_door, radius_m: 60.0, speed_mps: 6.0, alt_m: up(50.0), clockwise: true }.at(t);
    let drones = [Drone { pilot: Some(next_door), jitter_m: 5.0, ..Drone::new("PHOTO", &path) }];
    fly(&mut sim(HOUSE, profile), &drones, 300, &["PHOTO"])
}

/// A roof or solar inspector: comes in slowly (2 m/s) at 40 m, settles
/// 8 m up over the roof for a minute, climbs away (as tests_linger).
fn inspector(profile: &str) -> Worst {
    let (from, roof, away) = (at_site(130.0, 0.0), at_site(20.0, 10.0), at_site(20.0, 2000.0));
    let path = move |t: f64| {
        if t < 120.0 {
            Flight::Toward { from, to: roof, speed_mps: 2.0, alt_from: up(40.0), alt_to: up(8.0) }.at(t)
        } else {
            Flight::Toward { from: roof, to: away, speed_mps: 8.0, alt_from: up(60.0), alt_to: up(60.0) }.at(t - 120.0)
        }
    };
    let drones = [Drone { jitter_m: 3.0, alt_jitter_m: 1.0, ..Drone::new("ROOF", &path) }];
    fly(&mut sim(HOUSE, profile), &drones, 200, &["ROOF"])
}

/// A delivery drone flying straight past, 400 m north, at 15 m/s.
fn delivery(profile: &str) -> Worst {
    let from = at_site(-1500.0, 400.0);
    let path = move |t: f64| Flight::Line { from, course_deg: 90.0, speed_mps: 15.0, alt_m: 100.0 }.at(t);
    let drones = [Drone { jitter_m: 8.0, ..Drone::new("DLV-1", &path) }];
    fly(&mut sim(HOUSE, profile), &drones, 200, &["DLV-1"])
}

/// A hobbyist in a park hovering 120 m from a handheld user (bubble 300),
/// 50 m up, for 7 minutes; its pilot 20 m from the user.
fn park_hobbyist(profile: &str) -> Worst {
    let path = hover(0.0, 120.0, 50.0);
    let drones = [Drone { pilot: Some(at_site(10.0, 15.0)), jitter_m: 5.0, ..Drone::new("PARK", &path) }];
    fly(&mut sim(BUBBLE, profile), &drones, 420, &["PARK"])
}

/// A lawnmower survey over the house: seven 200 m passes 30 m apart at
/// 5 m/s, 60 m up; the surveyor stands 50 m beyond the fence.
fn survey(profile: &str) -> Worst {
    let corner = at_site(-100.0, -100.0);
    let mow = move |t: f64| Flight::Mow { corner, width_m: 200.0, rows: 7, spacing_m: 30.0, speed_mps: 5.0, alt_m: up(60.0) }.at(t);
    let drones = [Drone { pilot: Some(at_site(-150.0, -130.0)), jitter_m: 5.0, ..Drone::new("MOW", &mow) }];
    fly(&mut sim(HOUSE, profile), &drones, 400, &["MOW"])
}

/// A drone that lands in the area and stays there (the pilot walks over
/// to it later; no pilot position broadcast).
fn landed(profile: &str) -> Worst {
    let spot = at_site(30.0, 20.0);
    let path = move |t: f64| Flight::Dip { at: spot, alt_m: up(50.0), depth_m: 50.0, start_s: 20.0, rate_mps: 2.0, pause_s: f64::INFINITY }.at(t);
    let drones = [Drone { jitter_m: 3.0, alt_jitter_m: 1.0, ..Drone::new("LAND", &path) }];
    fly(&mut sim(HOUSE, profile), &drones, 200, &["LAND"])
}

/// Two hobbyists in a park, near a handheld user (bubble 300): one hovers
/// 120 m north at 45 m; after 200 s the other starts short back-and-forth
/// passes 120-200 m east at 5 m/s (some of them towards the user). Their
/// pilots stand beside the user.
fn two_hobbyists(profile: &str) -> Worst {
    let a = hover(0.0, 120.0, 45.0);
    let corner = at_site(120.0, -60.0);
    let b = move |t: f64| Flight::Mow { corner, width_m: 80.0, rows: 6, spacing_m: 25.0, speed_mps: 5.0, alt_m: up(40.0) }.at(t - 200.0);
    let drones = [
        Drone { pilot: Some(at_site(5.0, 15.0)), jitter_m: 5.0, ..Drone::new("HOB-A", &a) },
        Drone { pilot: Some(at_site(-10.0, -20.0)), jitter_m: 5.0, from_s: 200, ..Drone::new("HOB-B", &b) },
    ];
    fly(&mut sim(BUBBLE, profile), &drones, 420, &["HOB-A", "HOB-B"])
}

/// A neighbour's drone over the neighbour's own garden (220 m east, 70 m
/// beyond our fence, 40 m up) for 4 minutes on four days running; the
/// neighbour stands in the garden, 80 m beyond our fence.
fn neighbour(profile: &str) -> Worst {
    let mut s = sim(HOUSE, profile);
    s.utc0 = Some(UTC0);
    let path = hover(220.0, 0.0, 40.0);
    let mut w = Worst::default();
    for _ in 0..4 {
        let start = s.s;
        let drones = [Drone { from_s: start, until_s: start + 240, pilot: Some(at_site(230.0, 10.0)), jitter_m: 5.0, ..Drone::new("NEXT-1", &path) }];
        s.run(&drones, start + 240, |x| w.see(x, &["NEXT-1"]));
        let next = (s.s / DAY_S + 1) * DAY_S;
        s.skip(next - s.s);
    }
    w
}

/// A drone-racing or club meet: four drones hovering over the area at
/// once, their pilots standing inside it.
fn meet(profile: &str) -> Worst {
    let spots = [(40.0, 30.0), (-50.0, 20.0), (10.0, -60.0), (-20.0, -10.0)];
    let paths: Vec<_> = spots.iter().map(|&(e, n)| hover(e, n, 30.0)).collect();
    let names = ["MEET-1", "MEET-2", "MEET-3", "MEET-4"];
    let drones: Vec<Drone<'_>> = paths
        .iter()
        .zip(names)
        .zip(spots)
        .map(|((p, n), (e, nn))| Drone { pilot: Some(at_site(e * 0.5, nn * 0.5 - 20.0)), jitter_m: 5.0, ..Drone::new(n, p) })
        .collect();
    fly(&mut sim(HOUSE, profile), &drones, 120, &names)
}

// ------------------------------------------------------- hostile flights --

/// From 1200 m south straight at the house at 12 m/s.
fn beeline(profile: &str) -> Worst {
    let from = at_site(0.0, -1200.0);
    let path = move |t: f64| Flight::Toward { from, to: SITE, speed_mps: 12.0, alt_from: up(50.0), alt_to: up(30.0) }.at(t);
    fly(&mut sim(HOUSE, profile), &[Drone::new("SN-BEE", &path)], 110, &["SN-BEE"])
}

/// An FPV dive from 2 km at 40 m/s.
fn fpv_dive(profile: &str) -> Worst {
    let from = at_site(2000.0, 0.0);
    let path = move |t: f64| Flight::Toward { from, to: SITE, speed_mps: 40.0, alt_from: 300.0, alt_to: 20.0 }.at(t);
    fly(&mut sim(HOUSE, profile), &[Drone::new("FPV-1", &path)], 60, &["FPV-1"])
}

/// A spotter circles the house for 4 min and leaves; a second drone flies
/// straight at it at 8 m/s (as tests_across).
fn spotter_striker(profile: &str) -> Worst {
    let spot = |t: f64| {
        let orbit = Flight::Orbit { centre: at_site(20.0, 0.0), radius_m: 80.0, speed_mps: 6.0, alt_m: up(50.0), clockwise: true };
        if t < 240.0 {
            return orbit.at(t);
        }
        let left = orbit.at(240.0);
        Flight::Toward { from: (left.lat, left.lon), to: at_site(20.0, 6000.0), speed_mps: 10.0, alt_from: up(50.0), alt_to: up(50.0) }.at(t - 240.0)
    };
    let strike = |t: f64| Flight::Toward { from: at_site(0.0, -1500.0), to: SITE, speed_mps: 8.0, alt_from: up(60.0), alt_to: up(40.0) }.at(t - 240.0);
    let drones = [Drone::new("SPOT-1", &spot), Drone { from_s: 240, ..Drone::new("STRK-1", &strike) }];
    fly(&mut sim(HOUSE, profile), &drones, 450, &["STRK-1"])
}

/// Recon: circling over the house (an 80 m circle, 6 m/s, 50 m up) for 5
/// minutes.
fn recon(profile: &str) -> Worst {
    let path = move |t: f64| Flight::Orbit { centre: at_site(20.0, 0.0), radius_m: 80.0, speed_mps: 6.0, alt_m: up(50.0), clockwise: true }.at(t);
    fly(&mut sim(HOUSE, profile), &[Drone { jitter_m: 5.0, ..Drone::new("RECON", &path) }], 300, &["RECON"])
}

/// A payload drop over the yard: 50 m up, down 30 m, 5 s, back up.
fn drop_(profile: &str) -> Worst {
    let yard = at_site(30.0, 20.0);
    let path = move |t: f64| Flight::Dip { at: yard, alt_m: up(50.0), depth_m: 30.0, start_s: 40.0, rate_mps: 3.0, pause_s: 5.0 }.at(t);
    let drones = [Drone { jitter_m: 5.0, alt_jitter_m: 2.0, ..Drone::new("DROP", &path) }];
    fly(&mut sim(HOUSE, profile), &drones, 160, &["DROP"])
}

/// A drone keeping pace 60 m beside a walker (bubble 300) for 400 m.
fn follow(profile: &str) -> Worst {
    let walk = |t: f64| Flight::Toward { from: at_site(-200.0, 0.0), to: at_site(200.0, 0.0), speed_mps: 1.4, alt_from: OWN_ALT as f32, alt_to: OWN_ALT as f32 }.at(t);
    let lane = |t: f64| Flight::Toward { from: at_site(-230.0, 60.0), to: at_site(170.0, 60.0), speed_mps: 1.4, alt_from: up(50.0), alt_to: up(50.0) }.at(t);
    let mut s = sim(BUBBLE, profile);
    s.own = Some(std::boxed::Box::new(walk));
    fly(&mut s, &[Drone { jitter_m: 5.0, ..Drone::new("SHADOW", &lane) }], 320, &["SHADOW"])
}

/// A camera drone at full speed (20 m/s) straight at a walker (bubble 300),
/// from 1.5 km.
fn rush_walker(profile: &str) -> Worst {
    let walk = |t: f64| Flight::Line { from: at_site(-100.0, 0.0), course_deg: 90.0, speed_mps: 1.5, alt_m: OWN_ALT as f32 }.at(t);
    let meet = at_site(-100.0 + 1.5 * 75.0, 0.0);
    let from = offset(meet.0, meet.1, 0.0, 1500.0);
    let path = move |t: f64| Flight::Line { from, course_deg: 180.0, speed_mps: 20.0, alt_m: up(40.0) }.at(t);
    let mut s = sim(BUBBLE, profile);
    s.own = Some(std::boxed::Box::new(walk));
    fly(&mut s, &[Drone::new("RUSH", &path)], 80, &["RUSH"])
}

/// A quick look in a window: in slowly at 2 m/s, down to 18 m over the
/// house, 10 s still, then up and away. Every profile sees it as a drop
/// once it climbs (down 22 m, a pause, up); `home` sees it about 9 s
/// sooner, as low hover (under 20 m for 6 s), while it is still at the
/// window.
fn window_peek(profile: &str) -> Worst {
    let (from, spot, away) = (at_site(130.0, 0.0), at_site(20.0, 10.0), at_site(20.0, 2000.0));
    let path = move |t: f64| {
        if t < 65.0 {
            Flight::Toward { from, to: spot, speed_mps: 2.0, alt_from: up(40.0), alt_to: up(18.0) }.at(t)
        } else {
            Flight::Toward { from: spot, to: away, speed_mps: 8.0, alt_from: up(60.0), alt_to: up(60.0) }.at(t - 65.0)
        }
    };
    let drones = [Drone { jitter_m: 3.0, alt_jitter_m: 1.0, ..Drone::new("PEEK", &path) }];
    fly(&mut sim(HOUSE, profile), &drones, 120, &["PEEK"])
}

// ----------------------------------------------------------- the matrix --

use Level::{Caution as C, Critical as X, Notice as N, Warning as W};

/// One flight: its name, whether it is hostile, how to fly it, and the
/// worst level expected under default, home, facility, event, patrol.
struct Case {
    name: &'static str,
    place: &'static str,
    hostile: bool,
    fly: fn(&str) -> Worst,
    want: [Level; 5],
}

const CASES: &[Case] = &[
    Case { name: "estate photographer next door", place: "house", hostile: false, fly: photographer, want: [N, N, C, N, N] },
    Case { name: "roof or solar inspector, low", place: "house", hostile: false, fly: inspector, want: [W, W, W, W, W] },
    Case { name: "delivery drone passing straight", place: "house", hostile: false, fly: delivery, want: [N, N, N, N, N] },
    Case { name: "park hobbyist near a handheld user", place: "bubble", hostile: false, fly: park_hobbyist, want: [C, C, C, C, C] },
    Case { name: "lawnmower survey over the house", place: "house", hostile: false, fly: survey, want: [W, W, W, W, W] },
    Case { name: "landed drone in the area", place: "house", hostile: false, fly: landed, want: [W, W, W, W, W] },
    Case { name: "two hobbyists in a park", place: "bubble", hostile: false, fly: two_hobbyists, want: [C, C, C, C, C] },
    Case { name: "neighbour's drone, 4 days", place: "house", hostile: false, fly: neighbour, want: [N, N, W, N, N] },
    Case { name: "club meet: 4 drones, pilots inside", place: "house", hostile: false, fly: meet, want: [W, W, W, C, W] },
    Case { name: "beeline at 12 m/s", place: "house", hostile: true, fly: beeline, want: [X, X, X, X, X] },
    Case { name: "FPV dive at 40 m/s", place: "house", hostile: true, fly: fpv_dive, want: [X, X, X, X, X] },
    Case { name: "spotter then striker", place: "house", hostile: true, fly: spotter_striker, want: [X, X, X, X, X] },
    Case { name: "recon circling over the house", place: "house", hostile: true, fly: recon, want: [W, W, W, W, W] },
    Case { name: "payload drop", place: "house", hostile: true, fly: drop_, want: [W, W, W, W, W] },
    Case { name: "following a walker", place: "bubble", hostile: true, fly: follow, want: [W, W, W, W, W] },
    Case { name: "camera drone at 20 m/s at a walker", place: "bubble", hostile: true, fly: rush_walker, want: [X, X, X, X, X] },
    Case { name: "window peek, 18 m for 10 s", place: "house", hostile: true, fly: window_peek, want: [W, W, W, W, W] },
];

fn cell(w: &Worst, hostile: bool) -> String {
    match w.level {
        None => String::from("none"),
        Some(l) if hostile => std::format!("{} @{} s", l.label(), w.at_s),
        Some(l) => String::from(l.label()),
    }
}

#[test]
fn false_alarm_matrix() {
    let mut rows = Vec::new();
    let mut wrong = Vec::new();
    for c in CASES {
        let mut cells = Vec::new();
        for (i, p) in PROFILES.into_iter().enumerate() {
            let line = std::format!("profile {}", p.name());
            let w = (c.fly)(&line);
            if w.level != Some(c.want[i]) {
                wrong.push(std::format!("{} under {}: {:?}, expected {:?}", c.name, p.name(), w.level, c.want[i]));
            }
            cells.push(cell(&w, c.hostile));
        }
        rows.push(std::format!("| {} | {} | {} | {} |", if c.hostile { "hostile" } else { "ordinary" }, c.name, c.place, cells.join(" | ")));
    }
    std::println!("| | flight | place | default | home | facility | event | patrol |\n|---|---|---|---|---|---|---|---|");
    for r in &rows {
        std::println!("{r}");
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// Ordinary flights never reach Critical under any profile, and hostile
/// ones never fall below Warning.
#[test]
fn ceilings_and_floors_of_the_matrix() {
    for c in CASES {
        for (i, l) in c.want.iter().enumerate() {
            if c.hostile {
                assert!(*l >= W, "{} {i}", c.name);
            } else {
                assert!(*l <= W, "{} {i}", c.name);
            }
        }
        // The safety floors: the approach rules and the striker are the
        // same under every profile.
        if c.hostile && c.want[0] == X {
            assert!(c.want.iter().all(|l| *l == X), "{}", c.name);
        }
    }
}

/// Patrol makes fast inbound sooner, never later; and the event profile's
/// "several at once" is off only for the count: jamming still counts.
#[test]
fn patrol_is_critical_sooner_and_event_keeps_the_jamming_half() {
    // The window peek: home sees it sooner (low hover) than the drop the
    // others wait for.
    assert!(window_peek("profile home").at_s + 5 <= window_peek("profile default").at_s);
    let d = rush_walker("profile default");
    let p = rush_walker("profile patrol");
    assert_eq!((d.level, p.level), (Some(X), Some(X)));
    assert!(p.at_s < d.at_s, "patrol {} s, default {} s", p.at_s, d.at_s);
    // Jamming while one drone is near the house: Warning under event too.
    let path = hover(250.0, 0.0, 60.0);
    for prof in ["profile event", "profile default"] {
        let mut s = sim(HOUSE, prof);
        let drones = [Drone::new("JAM-1", &path)];
        s.run(&drones, 10, |_| {});
        let now = s.now();
        s.tr.ingest_threat(3, &super::tests_across::jamming(), now);
        let w = fly(&mut s, &drones, 20, &["JAM-1"]);
        assert_eq!(w.level, Some(W), "{prof}");
        assert!(s.rules("JAM-1").contains(&Rule::Several), "{prof}: {:?}", s.rules("JAM-1"));
    }
}

/// The precedence rule on a flight: a `crowd` line wins over the event
/// profile whatever the order.
#[test]
fn a_crowd_line_wins_over_the_event_profile_in_the_matrix_flight() {
    // The meet with `crowd 3` written beside `profile event`: several at
    // once is counted again (Warning).
    let spots = [(40.0, 30.0), (-50.0, 20.0), (10.0, -60.0)];
    let paths: Vec<_> = spots.iter().map(|&(e, n)| hover(e, n, 30.0)).collect();
    let names = ["M-1", "M-2", "M-3"];
    let drones: Vec<Drone<'_>> = paths.iter().zip(names).map(|(p, n)| Drone::new(n, p)).collect();
    for (lines, want) in [(&["crowd 3", "profile event"][..], W), (&["profile event"][..], C), (&["profile event", "crowd 3"][..], W)] {
        let mut all = std::vec![HOUSE];
        all.extend_from_slice(lines);
        let mut s = Sim::new(&all);
        assert_eq!(fly(&mut s, &drones, 60, &names).level, Some(want), "{lines:?}");
    }
}

// ------------------------------------------------------- places in JSON --

/// `/api/state` (`state_full`) carries the places when the hub gives them:
/// the profile, the count in force, the bubble, each area's outline and
/// ceiling; friends counted, never listed.
#[test]
fn the_places_reach_the_state_json_and_the_worst_case_fits() {
    let mut s = Sim::new(&[
        HOUSE,
        "area house ceiling 90",
        "area yard poly 39.9520 -75.1640 39.9523 -75.1636 39.9519 -75.1633",
        "area shed ceiling 30",
        "bubble 300",
        "friend OUR-MINI our own Mini",
        "profile event",
    ]);
    s.run(&[], 2, |_| {});
    let ctx = s.tr.context(s.now(), None);
    let hub = crate::json::HubStatus {
        own_source: crate::json::OwnSource::Gps,
        manual: None,
        gps: crate::json::GpsStatus { state: crate::json::GpsState::Fix3d, sats: Some(9) },
        recording: false,
        nodes: &[],
        adsb: None,
        fix_held: false,
        utc_held: false,
    };
    let signals = crate::json::Signals { places: Some(s.tr.places()), ..Default::default() };
    let mut buf = std::vec![0u8; crate::json::state_full_max_len(0, 0, 0, 0, 0, 0)];
    let n = crate::json::state_full(&[], &[], &[], &signals, &ctx, &hub, &mut buf).unwrap();
    let text = core::str::from_utf8(&buf[..n]).unwrap();
    assert!(text.ends_with("}}\n"), "{text}");
    assert!(!text.contains("OUR-MINI"), "friends are counted, not listed: {text}");
    let v: serde_json::Value = serde_json::from_str(text).unwrap();
    let p = &v["places"];
    assert_eq!(p["profile"], "event");
    assert!(p["crowd"].is_null(), "event: not counted by number");
    assert_eq!((p["friends"].as_u64(), p["bubble_m"].as_f64(), p["visits"].as_bool()), (Some(1), Some(300.0), Some(true)));
    let areas = p["areas"].as_array().unwrap();
    assert_eq!(areas.len(), 2, "the shed has no outline: left out");
    assert_eq!(areas[0]["name"], "house");
    assert_eq!(areas[0]["ceiling_m"], 90.0);
    assert_eq!(areas[0]["circle"]["lat"], 39.9521);
    assert_eq!(areas[0]["circle"]["lon"], -75.1638);
    assert_eq!(areas[0]["circle"]["r_m"], 150.0);
    assert!(areas[0].get("poly").is_none());
    assert_eq!(areas[1]["poly"][2][1], -75.1633);
    assert_eq!(areas[1]["poly"].as_array().unwrap().len(), 3);
    // Without places (a hub that judges no levels) the field is left out,
    // and the rest is byte for byte as before.
    let none = crate::json::Signals::default();
    let m = crate::json::state_full(&[], &[], &[], &none, &ctx, &hub, &mut buf).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&buf[..m]).unwrap();
    assert!(v.get("places").is_none() && v["signal_threats"].as_array().unwrap().is_empty());
    // The widest: 8 polygons of 16 corners, the longest names, coordinates
    // with every digit, a bubble.
    let mut w = Places::new();
    for i in 0..places::MAX_AREAS {
        let mut v: heapless::Vec<Vertex, { places::MAX_VERTICES }> = heapless::Vec::new();
        for k in 0..places::MAX_VERTICES {
            let _ = v.push(Vertex { lat_e7: -899_999_999 + k as i32, lon_e7: -1_799_999_999 + k as i32 });
        }
        let name = Name::try_from(std::format!("abcdefghijk{i}").as_str()).unwrap();
        assert!(w.set_shape(&name, Shape::Poly(v)));
        assert!(w.set_ceiling(&name, -12_345.678));
    }
    w.bubble_m = Some(-10_000.123);
    w.crowd_min = Some(20);
    w.crowd_set = true;
    let mut out = std::vec![0u8; crate::json::MAX_PLACES_JSON];
    let n = crate::json::places_json(&w, &mut out).expect("the widest fits");
    std::println!("widest places object: {n} B (bound {})", crate::json::MAX_PLACES_JSON);
    assert!(n + 256 < crate::json::MAX_PLACES_JSON, "{n}");
}
