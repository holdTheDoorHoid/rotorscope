//! Scripted signals and sounds for the rules without a position (slice
//! 16c, rules 12 and 13, and 14 for a striker without a position): a video
//! signal getting louder on one node and on two, the false alarms it must
//! not raise (banking, a null, switching on, a still pilot, one spike, a
//! pit-mode step, a crossing), the dark drone and what explains it away,
//! low hums, the microphone and the radio agreeing, and the outputs. Uses
//! the 16a simulation ([`Sim`]); signals are fed to the tracker each second
//! before the drones.

extern crate std;
use std::vec::Vec;

use link::{RfKind, RfSighting};

use super::scenario::{Flight, Sample, offset};
use super::tests::{Drone, HOUSE, OWN_ALT, SITE, Sim};
use super::trend::*;
use super::*;

/// One video emitter heard by one node: its level each second (`None`:
/// not heard that second).
struct Sig<'a> {
    node: u8,
    freq: u32,
    kind: RfKind,
    level: &'a dyn Fn(u64) -> Option<f32>,
}

impl<'a> Sig<'a> {
    fn video(node: u8, level: &'a dyn Fn(u64) -> Option<f32>) -> Self {
        Self { node, freq: 57_400, kind: RfKind::AnalogVideo, level }
    }
}

/// One rotor sound: heard from `from` to `until` (seconds), its level each
/// second.
struct Snd<'a> {
    id: u32,
    from: u64,
    until: u64,
    low_hum: bool,
    aircraft: bool,
    level: &'a dyn Fn(u64) -> f32,
}

impl<'a> Snd<'a> {
    fn rotor(from: u64, until: u64, level: &'a dyn Fn(u64) -> f32) -> Self {
        Self { id: 7, from, until, low_hum: false, aircraft: false, level }
    }
}

/// One second: the signals and sounds heard now, then the drones and the
/// tick.
fn step(sim: &mut Sim, drones: &[Drone<'_>], sigs: &[Sig<'_>], snds: &[Snd<'_>]) {
    let now = sim.now();
    for g in sigs {
        if let Some(db) = (g.level)(sim.s) {
            let s = RfSighting {
                kind: g.kind,
                freq: g.freq,
                width_khz: 0,
                snr_db: libm::roundf(db) as i8,
                duty_pct: 90,
                line_hz: 15_734,
                uptime_us: 0,
            };
            sim.tr.ingest_rf(g.node, &s, now);
        }
    }
    for d in snds.iter().filter(|d| (d.from..d.until).contains(&sim.s)) {
        let c = RotorSound {
            id: d.id,
            first_ms: super::tests::T0 + d.from * 1000,
            last_ms: now,
            level_db: libm::roundf((d.level)(sim.s)) as i8,
            low_hum: d.low_hum,
            aircraft: d.aircraft,
        };
        sim.tr.hear_sound(&c, now);
    }
    sim.step(drones);
}

/// Each second's level and rules of every signal threat, by its source.
#[derive(Default)]
struct Seen(Vec<(u64, std::string::String, Option<Level>, Vec<Rule>)>);

impl Seen {
    fn run(sim: &mut Sim, drones: &[Drone<'_>], sigs: &[Sig<'_>], snds: &[Snd<'_>], to: u64) -> Self {
        let mut v = Vec::new();
        while sim.s < to {
            step(sim, drones, sigs, snds);
            for s in sim.tr.signal_threats() {
                let who = std::format!("{}", s.src);
                v.push((sim.s - 1, who, s.threat.level, s.threat.reasons.iter().map(|r| r.rule).collect()));
            }
        }
        Self(v)
    }

    fn worst(&self) -> Option<Level> {
        self.0.iter().map(|e| e.2).max().flatten()
    }

    fn worst_of(&self, who: &str) -> Option<Level> {
        self.0.iter().filter(|e| e.1 == who).map(|e| e.2).max().flatten()
    }

    fn first(&self, rule: Rule) -> Option<u64> {
        self.0.iter().find(|e| e.3.contains(&rule)).map(|e| e.0)
    }

    fn first_at(&self, level: Level) -> Option<u64> {
        self.0.iter().find(|e| e.2 >= Some(level)).map(|e| e.0)
    }
}

const VIDEO: &str = "Analog video 5740.0 MHz";

fn video(sim: &Sim) -> Option<&SignalThreat> {
    sim.tr.signal_threats().iter().find(|s| matches!(s.src, SignalSrc::Emitter { .. }))
}

fn sound(sim: &Sim) -> Option<&SignalThreat> {
    sim.tr.signal_threats().iter().find(|s| matches!(s.src, SignalSrc::Sound { .. }))
}

fn reason(s: &SignalThreat, rule: Rule) -> Option<&Reason> {
    s.threat.reasons.iter().find(|r| r.rule == rule)
}

/// Quiet at `base` dB until `start`, then louder by `rate` dB a second for
/// `secs` seconds, then steady (an FPV drone flying in and stopping).
fn ramp(base: f32, start: u64, rate: f32, secs: u64) -> impl Fn(u64) -> Option<f32> {
    move |t| Some(base + rate * t.saturating_sub(start).min(secs) as f32)
}

/// A fixed pseudo-random wobble in -1..1 for second `t` and stream `k`.
fn wobble(t: u64, k: u64) -> f32 {
    let x = (t.wrapping_mul(2_654_435_761).wrapping_add(k.wrapping_mul(40_503))) % 1000;
    x as f32 / 500.0 - 1.0
}

// ------------------------------------------------------ 12 getting louder --

#[test]
fn a_steady_rise_on_one_node_is_a_caution_that_comes_down_later() {
    // From 8 dB above the noise to 20 dB in 12 s (a drone flying in from
    // about 400 m to 100 m), on the hub's own receiver; then it stays.
    let lv = ramp(8.0, 30, 1.0, 12);
    let sigs = [Sig::video(0, &lv)];
    // A friend line names a serial: it cannot cover a signal.
    let mut sim = Sim::new(&[HOUSE, "friend SN-OURS our own Mini"]);
    let seen = Seen::run(&mut sim, &[], &sigs, &[], 40);
    // Six seconds of rise at 1 dB a second, from second 31.
    let hit = seen.first(Rule::GettingLouder).expect("getting louder");
    assert_eq!(hit, 37);
    assert_eq!(seen.worst(), Some(Level::Caution));
    let s = video(&sim).unwrap();
    let r = reason(s, Rule::GettingLouder).unwrap();
    assert_eq!(r.nodes(), Some(1));
    assert!(r.db().unwrap() >= 6, "{:?}", r.db());
    assert!((5..=12).contains(&r.for_s().unwrap()), "{:?}", r.for_s());
    assert_eq!(r.signal_kind(), Some(SignalKind::Video));
    // The hub stands in the house's area, and node 0 is the hub.
    assert_eq!(r.place(), "house");
    assert!(!r.has_range() && !r.agreed() && !r.no_place());
    // Words: a trend, not a position.
    let text = sim.tr.describe_signal_level(s.id, sim.now()).unwrap();
    assert!(text.starts_with("Caution: Analog video 5740.0 MHz (no position): FPV video signal getting louder fast on 1 node (+"), "{text}");
    assert!(text.ends_with(", nearing house"), "{text}");
    assert_eq!(strip_text_signal(s).unwrap().as_str(), "CAUTION FPV video louder fast, house");
    // Evidence: rule 12, flag bit 4 (no position).
    let id = s.id;
    let rise = sim.tr.take_signal_rise(sim.now()).expect("a rise to log");
    assert_eq!((rise.id, rise.level), (id, Level::Caution));
    let d = signal_rise_data(sim.tr.get_signal_threat(rise.id).unwrap(), &rise);
    assert_eq!((d[0], d[5], d[9]), (2, 12, 0x10));
    assert_eq!(sim.tr.take_signal_rise(sim.now()), None);
    // It goes on being heard at its new level: the level comes down 30 s
    // after the rise stopped being seen, and the entry stays.
    let seen = Seen::run(&mut sim, &[], &sigs, &[], 90);
    // (The rise is last seen near second 45; the level and its reason are
    // held for 30 s after.)
    let last = seen.0.iter().filter(|e| e.2.is_some()).map(|e| e.0).max().unwrap();
    assert!((70..=80).contains(&last), "{last}");
    assert_eq!(seen.0.last().unwrap().2, None);
    assert_eq!(sim.tr.signal_threats().len(), 1);
}

#[test]
fn a_rise_on_two_nodes_at_once_is_a_warning() {
    let a = ramp(8.0, 30, 1.0, 12);
    let b = ramp(4.0, 31, 0.9, 12);
    let sigs = [Sig::video(1, &a), Sig::video(2, &b)];
    let mut sim = Sim::new(&[HOUSE]);
    let seen = Seen::run(&mut sim, &[], &sigs, &[], 45);
    assert_eq!(seen.worst(), Some(Level::Warning));
    let w = seen.first_at(Level::Warning).unwrap();
    assert!(w <= 40, "{w}");
    // Seen at the moment both were rising.
    let mut sim = Sim::new(&[HOUSE]);
    Seen::run(&mut sim, &[], &sigs, &[], w + 1);
    let s = video(&sim).unwrap();
    assert_eq!(reason(s, Rule::GettingLouder).unwrap().nodes(), Some(2));
    let mut nodes: Vec<u8> = s.rising_nodes().collect();
    nodes.sort();
    assert_eq!(nodes, [1, 2]);
    let text = sim.tr.describe_signal_level(s.id, sim.now()).unwrap();
    assert!(text.contains("getting louder fast on 2 nodes"), "{text}");
    assert_eq!(strip_text_signal(s).unwrap().as_str(), "WARNING FPV video louder fast, 2 nodes, house");
}

#[test]
fn banking_swings_and_a_null_that_recovers_are_not_getting_louder() {
    // Banking: +-8 dB every 6 s round a steady 15 dB, with a little noise.
    let bank = |t: u64| Some(15.0 + 8.0 * libm::sinf(t as f32 * core::f32::consts::TAU / 6.0) + wobble(t, 1));
    // A null: steady 15 dB, down to 3 dB at second 40, back up over 7 s
    // (a steady 12 dB rise, but only back to where it was).
    let null = |t: u64| {
        Some(match t {
            0..38 => 15.0 + wobble(t, 2),
            38..41 => 3.0,
            41..48 => 3.0 + 12.0 * (t - 40) as f32 / 7.0,
            _ => 15.0 + wobble(t, 3),
        })
    };
    for (name, lv) in [("bank", &bank as &dyn Fn(u64) -> Option<f32>), ("null", &null)] {
        let sigs = [Sig::video(0, lv)];
        let mut sim = Sim::new(&[HOUSE]);
        let seen = Seen::run(&mut sim, &[], &sigs, &[], 120);
        assert_eq!(seen.worst(), None, "{name}");
    }
}

#[test]
fn switching_on_beside_a_node_or_leaving_pit_mode_is_not_getting_louder() {
    // Appears at full strength (one weak first sample, half a dwell).
    let on = |t: u64| match t {
        0..20 => None,
        20 => Some(9.0),
        _ => Some(30.0 + wobble(t, 4)),
    };
    // A video transmitter in pit mode (25 mW) switching to race power: one
    // step of 14 dB, then steady.
    let pit = |t: u64| Some(if t < 30 { 6.0 + wobble(t, 5) } else { 20.0 + wobble(t, 6) });
    for (name, lv) in [("on", &on as &dyn Fn(u64) -> Option<f32>), ("pit", &pit)] {
        let sigs = [Sig::video(0, lv)];
        let mut sim = Sim::new(&[HOUSE]);
        let seen = Seen::run(&mut sim, &[], &sigs, &[], 90);
        assert_eq!(seen.worst(), None, "{name}");
    }
}

#[test]
fn still_goggles_and_one_noisy_sample_are_not_getting_louder() {
    // A pilot's own goggles or controller standing still: 18 dB +-3.
    let still = |t: u64| Some(18.0 + 3.0 * wobble(t, 7));
    // Steady, with one sample 18 dB too loud.
    let spike = |t: u64| Some(if t == 40 { 30.0 } else { 12.0 + wobble(t, 8) * 0.5 });
    for (name, lv) in [("still", &still as &dyn Fn(u64) -> Option<f32>), ("spike", &spike)] {
        let sigs = [Sig::video(0, lv)];
        let mut sim = Sim::new(&[HOUSE]);
        let seen = Seen::run(&mut sim, &[], &sigs, &[], 150);
        assert_eq!(seen.worst(), None, "{name}");
    }
}

#[test]
fn louder_on_one_node_and_quieter_on_another_is_a_crossing() {
    let up = ramp(6.0, 30, 1.0, 12);
    let down = |t: u64| Some(20.0 - t.saturating_sub(30).min(12) as f32);
    let sigs = [Sig::video(1, &up), Sig::video(2, &down)];
    let mut sim = Sim::new(&[HOUSE]);
    assert_eq!(Seen::run(&mut sim, &[], &sigs, &[], 60).worst(), None);
}

#[test]
fn sparse_samples_still_show_a_steady_rise() {
    // A band scan that hears the channel only every other second.
    let lv = |t: u64| (t % 2 == 0).then(|| 6.0 + 1.0 * t.saturating_sub(30).min(14) as f32);
    let sigs = [Sig::video(0, &lv)];
    let mut sim = Sim::new(&[HOUSE]);
    assert_eq!(Seen::run(&mut sim, &[], &sigs, &[], 50).worst(), Some(Level::Caution));
}

#[test]
fn the_place_follows_the_node() {
    // A node placed in the yard (far from the hub, outside the house's
    // area) hears it rise: it closes on the yard.
    let yard = "area yard circle 39.95600 -75.16380 80";
    let lv = ramp(8.0, 30, 1.0, 12);
    let sigs = [Sig::video(3, &lv)];
    let mut sim = Sim::new(&[HOUSE, yard]);
    sim.tr.set_node_position(3, 39.95600, -75.16370);
    Seen::run(&mut sim, &[], &sigs, &[], 40);
    let r = *reason(video(&sim).unwrap(), Rule::GettingLouder).unwrap();
    assert_eq!(r.place(), "yard");
    // A node placed away from every place, with no bubble: no place, and
    // the words say none.
    let mut sim = Sim::new(&[HOUSE]);
    sim.tr.set_node_position(3, 39.97000, -75.16380);
    Seen::run(&mut sim, &[], &sigs, &[], 40);
    let s = video(&sim).unwrap();
    let r = reason(s, Rule::GettingLouder).unwrap();
    assert!(r.no_place());
    let text = sim.tr.describe_signal_level(s.id, sim.now()).unwrap();
    assert!(!text.contains("nearing"), "{text}");
    let ctx = sim.tr.context(sim.now(), None);
    let mut buf = [0u8; crate::json::MAX_SIGNAL_THREAT_LINE];
    let n = crate::json::signal_threat_line(s, &ctx, &mut buf).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&buf[..n]).unwrap();
    assert!(v["threat"]["reasons"][0]["place"].is_null(), "{v}");
    // At the hub with the bubble on: the bubble ("you").
    let mut sim = Sim::new(&[HOUSE, "bubble 300"]);
    Seen::run(&mut sim, &[], &sigs, &[], 40);
    let s = video(&sim).unwrap();
    assert!(reason(s, Rule::GettingLouder).unwrap().is_bubble());
    assert!(sim.tr.describe_signal_level(s.id, sim.now()).unwrap().ends_with("nearing you"));
}

#[test]
fn a_control_link_counts_by_its_downlink_only() {
    use crate::ctrllinks::ControlTable;
    use ctrlwatch::{Confidence, ControlSighting, Direction, Probe, Region, System};
    let run = |direction: Direction| {
        let mut sim = Sim::new(&[HOUSE]);
        let mut ct = std::boxed::Box::new(ControlTable::new());
        let mut worst = None;
        while sim.s < 50 {
            let rssi = -95 + sim.s.saturating_sub(30).min(14) as i16;
            let r = link::ControlReport {
                sighting: ControlSighting {
                    system: System::ExpressLrs,
                    region: Region::Fcc915,
                    probe: Probe::Elrs { rate: ctrlwatch::elrs::ElrsRate::L900_200, version: ctrlwatch::elrs::OtaVersion::V4 },
                    freq_hz: 915_500_000,
                    rssi_dbm: rssi,
                    snr_db: 7,
                    t_us: 1,
                    confidence: Confidence::Confirmed,
                    direction,
                    id: None,
                    version: Some(4),
                },
                best: Confidence::Confirmed,
                packets: 12,
                confirmed: 3,
                peak_rssi: rssi,
                part: 0,
                content: Default::default(),
                raw: None,
            };
            let now = sim.now();
            ct.ingest(&mut *sim.tr, 1, &r, now);
            sim.step(&[]);
            worst = worst.max(sim.tr.signal_threats().iter().filter_map(|s| s.threat.level).max());
        }
        (worst, sim.tr.signal_threats().first().map(|s| std::format!("{}", s.src)))
    };
    let (down, what) = run(Direction::Downlink);
    assert_eq!(down, Some(Level::Caution));
    assert_eq!(what.as_deref(), Some("ExpressLRS 915 link"));
    // The pilot's uplink: the drone moving changes nothing in it.
    assert_eq!(run(Direction::Uplink).0, None);
}

// --------------------------------------------- 14 a striker, no position --

/// Circles round the house for 4 min (80 m out, 6 m/s, 50 m up), then
/// keeps circling (still heard).
fn spotter(t: f64) -> Sample {
    let centre = offset(SITE.0, SITE.1, 20.0, 0.0);
    Flight::Orbit { centre, radius_m: 80.0, speed_mps: 6.0, alt_m: 50.0 + OWN_ALT as f32, clockwise: true }.at(t)
}

#[test]
fn a_rising_fpv_signal_after_a_spotter_is_critical() {
    let lv = ramp(8.0, 250, 1.0, 12);
    let sigs = [Sig::video(0, &lv)];
    let drones = [Drone::new("SPOT-S", &spotter)];
    let mut sim = Sim::new(&[HOUSE]);
    let seen = Seen::run(&mut sim, &drones, &sigs, &[], 262);
    let hit = seen.first(Rule::SpotterStriker).expect("spotter then striker");
    assert_eq!(seen.first(Rule::GettingLouder), Some(hit));
    assert_eq!(seen.worst(), Some(Level::Critical));
    let s = video(&sim).unwrap();
    let r = reason(s, Rule::SpotterStriker).unwrap();
    let spot = sim.track("SPOT-S").unwrap();
    assert_eq!(r.other(), Some(spot.id));
    assert_eq!(r.place(), "house");
    assert!(!r.has_range());
    let text = sim.tr.describe_signal_level(s.id, sim.now()).unwrap();
    assert!(text.starts_with("Critical: Analog video 5740.0 MHz (no position): follows spotter drone SPOT-S (lingered at house "), "{text}");
    assert!(text.contains("FPV video signal getting louder fast on 1 node"), "{text}");
    assert_eq!(strip_text_signal(s).unwrap().as_str(), "CRITICAL FPV video after spotter to house");
    // The spotter is marked, and its reason names the signal.
    let m = spot.threat.reasons.iter().find(|r| r.rule == Rule::Spotter).expect("spotter marked");
    assert_eq!(m.other_signal(), Some(s.id));
    assert_eq!(m.other(), None);
    let text = sim.tr.describe_level(spot.id, sim.now()).unwrap();
    assert!(text.contains(&std::format!("spotter: a signal getting louder (S{}) is closing on house", s.id.0)), "{text}");
    assert!(strip_text(spot).unwrap().contains("spotter for signal S"), "{:?}", strip_text(spot));
    // JSON: the spotter's reason carries `sig`, not `other`.
    let ctx = sim.tr.context(sim.now(), None);
    let mut buf = [0u8; crate::json::MAX_LINE];
    let n = crate::json::track_line(spot, &ctx, &mut buf).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&buf[..n]).unwrap();
    let r = v["threat"]["reasons"].as_array().unwrap().iter().find(|r| r["rule"] == "spotter").unwrap().clone();
    assert_eq!(r["sig"], s.id.0);
    assert!(r.get("other").is_none(), "{r}");
    // The CoT remarks of the spotter say so too.
    let ctx = sim.tr.context(sim.now(), Some(1_763_424_000_000));
    let mut cot = [0u8; crate::cot::MAX_EVENT];
    let n = crate::cot::drone_event(spot, &ctx, &mut cot).unwrap();
    let x = core::str::from_utf8(&cot[..n]).unwrap();
    assert!(x.contains("spotter: a signal getting louder (S"), "{x}");
}

#[test]
fn a_rising_fpv_signal_after_a_spotter_at_the_bubble_is_a_warning() {
    let lv = ramp(8.0, 250, 1.0, 12);
    let sigs = [Sig::video(0, &lv)];
    let drones = [Drone::new("SPOT-B", &spotter)];
    let mut sim = Sim::new(&["bubble 300"]);
    let seen = Seen::run(&mut sim, &drones, &sigs, &[], 262);
    assert!(seen.first(Rule::SpotterStriker).is_some());
    assert_eq!(seen.worst(), Some(Level::Warning));
}

#[test]
fn a_spotter_flying_in_itself_is_not_its_own_striker() {
    // A drone circles the house, then flies straight in from 600 m out: its
    // own video gets louder as it comes. That is its own beeline, not a
    // second drone.
    let path = |t: f64| {
        if t < 240.0 {
            return spotter(t);
        }
        let from = offset(SITE.0, SITE.1, 0.0, 600.0);
        Flight::Toward { from, to: SITE, speed_mps: 12.0, alt_from: 50.0, alt_to: 30.0 }.at(t - 240.0)
    };
    let lv = ramp(8.0, 262, 1.0, 12);
    let sigs = [Sig::video(0, &lv)];
    let drones = [Drone::new("SELF-1", &path)];
    let mut sim = Sim::new(&[HOUSE]);
    let seen = Seen::run(&mut sim, &drones, &sigs, &[], 280);
    assert!(sim.track("SELF-1").unwrap().threat.reasons.iter().any(|r| matches!(r.rule, Rule::Beeline | Rule::FastInbound)));
    assert!(seen.first(Rule::GettingLouder).is_some());
    assert_eq!(seen.first(Rule::SpotterStriker), None);
}

// ------------------------------------------------------- 13 dark drone --

#[test]
fn rotor_sound_with_no_radio_is_a_dark_drone() {
    let lv = |t: u64| -40.0 + wobble(t, 9);
    let snd = [Snd::rotor(10, 40, &lv)];
    let mut sim = Sim::new(&[HOUSE]);
    let seen = Seen::run(&mut sim, &[], &[], &snd, 40);
    let hit = seen.first(Rule::DarkDrone).expect("dark drone");
    assert!((15..=16).contains(&hit), "{hit}");
    assert_eq!(seen.worst(), Some(Level::Warning));
    let s = sound(&sim).unwrap();
    let r = reason(s, Rule::DarkDrone).unwrap();
    assert_eq!(r.place(), "house");
    assert_eq!(r.signal_kind(), Some(SignalKind::Sound));
    assert!(r.for_s().unwrap() >= 25);
    let text = sim.tr.describe_signal_level(s.id, sim.now()).unwrap();
    assert_eq!(text.as_str(), "Warning: rotor sound (no position): rotor sound, no radio heard, for 29 s, near house");
    assert_eq!(strip_text_signal(s).unwrap().as_str(), "WARNING rotor sound, no radio heard");
    // JSON: basis "sound", no distance.
    let ctx = sim.tr.context(sim.now(), None);
    let mut buf = [0u8; crate::json::MAX_SIGNAL_THREAT_LINE];
    let n = crate::json::signal_threat_line(s, &ctx, &mut buf).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&buf[..n]).unwrap();
    assert_eq!(v["basis"], "sound");
    assert_eq!(v["src"], "sound");
    let r = &v["threat"]["reasons"][0];
    assert_eq!((r["rule"].as_str(), r["kind"].as_str(), r["place"].as_str()), (Some("dark_drone"), Some("sound"), Some("house")));
    assert!(r.get("m").is_none(), "{r}");
    // Silence: the level comes down 30 s after, and the entry goes.
    let seen = Seen::run(&mut sim, &[], &[], &snd, 140);
    assert!(seen.0.iter().filter(|e| e.2.is_some()).all(|e| e.0 <= 72), "{:?}", seen.0.last());
    assert!(sim.tr.signal_threats().is_empty());
}

fn hover(e: f64, n: f64) -> impl Fn(f64) -> Sample {
    let p = offset(SITE.0, SITE.1, e, n);
    move |_t| Flight::Hover { lat: p.0, lon: p.1, alt_m: 40.0 + OWN_ALT as f32 }.at(0.0)
}

#[test]
fn a_remote_id_drone_nearby_explains_the_sound_one_far_away_does_not() {
    let lv = |t: u64| -40.0 + wobble(t, 10);
    let snd = [Snd::rotor(10, 60, &lv)];
    let near = hover(80.0, 40.0);
    let mut sim = Sim::new(&[HOUSE]);
    let seen = Seen::run(&mut sim, &[Drone::new("RID-NEAR", &near)], &[], &snd, 60);
    assert_eq!(seen.first(Rule::DarkDrone), None);
    assert_eq!(seen.worst(), None);
    // 2 km away: it cannot be what the microphone hears.
    let far = hover(0.0, 2000.0);
    let mut sim = Sim::new(&[HOUSE]);
    let seen = Seen::run(&mut sim, &[Drone::new("RID-FAR", &far)], &[], &snd, 60);
    assert!(seen.first(Rule::DarkDrone).is_some());
    // A drone last heard by radio more than 15 s before the sound began
    // does not explain it either; one heard 13 s before does.
    for (until, dark) in [(0, true), (15, true), (5, true), (8, false)] {
        let d = Drone { until_s: until, ..Drone::new("RID-GONE", &near) };
        let snd = [Snd::rotor(if until == 15 { 75 } else { 20 }, 100, &lv)];
        let mut sim = Sim::new(&[HOUSE]);
        let seen = Seen::run(&mut sim, &[d], &[], &snd, 100);
        assert_eq!(seen.first(Rule::DarkDrone).is_some(), dark, "until {until}");
    }
}

#[test]
fn a_low_hum_or_an_aircraft_is_never_a_dark_drone() {
    let up = |t: u64| -50.0 + t.saturating_sub(20).min(15) as f32;
    let hum = [Snd { low_hum: true, ..Snd::rotor(10, 60, &up) }];
    let mut sim = Sim::new(&[HOUSE]);
    assert_eq!(Seen::run(&mut sim, &[], &[], &hum, 60).worst(), None);
    let plane = [Snd { aircraft: true, ..Snd::rotor(10, 60, &up) }];
    let mut sim = Sim::new(&[HOUSE]);
    assert_eq!(Seen::run(&mut sim, &[], &[], &plane, 60).worst(), None);
}

#[test]
fn rotor_sound_getting_louder_with_radio_around_is_a_caution() {
    // A Remote ID drone hovers nearby (so the sound is not dark) while the
    // sound of another gets louder.
    let near = hover(80.0, 40.0);
    let up = |t: u64| -50.0 + t.saturating_sub(30).min(12) as f32;
    let snd = [Snd::rotor(10, 60, &up)];
    let mut sim = Sim::new(&[HOUSE]);
    let seen = Seen::run(&mut sim, &[Drone::new("RID-N2", &near)], &[], &snd, 60);
    assert_eq!(seen.worst_of("rotor sound"), Some(Level::Caution));
    assert_eq!(seen.first(Rule::DarkDrone), None);
    // Dark and getting louder: still a Warning (rule 13 with 12).
    let mut sim = Sim::new(&[HOUSE]);
    let seen = Seen::run(&mut sim, &[], &[], &snd, 60);
    assert_eq!(seen.worst(), Some(Level::Warning));
    assert!(seen.first(Rule::GettingLouder).is_some() && seen.first(Rule::DarkDrone).is_some());
}

#[test]
fn rotor_sound_and_a_rising_fpv_signal_together_are_a_warning() {
    // On its own this rise is a Caution (one node); with the microphone
    // hearing rotor sound get louder at the same time, a Warning.
    let lv = ramp(8.0, 30, 1.0, 12);
    let sigs = [Sig::video(0, &lv)];
    let up = |t: u64| -50.0 + t.saturating_sub(29).min(12) as f32;
    let snd = [Snd::rotor(5, 60, &up)];
    let mut sim = Sim::new(&[HOUSE]);
    let seen = Seen::run(&mut sim, &[], &sigs, &snd, 40);
    assert_eq!(seen.worst_of(VIDEO), Some(Level::Warning));
    let s = video(&sim).unwrap();
    assert!(reason(s, Rule::GettingLouder).unwrap().agreed());
    let text = sim.tr.describe_signal_level(s.id, sim.now()).unwrap();
    assert!(text.ends_with(", rotor sound too"), "{text}");
    assert_eq!(seen.worst_of("rotor sound"), Some(Level::Warning));
}

#[test]
fn a_dark_drone_then_a_rising_fpv_signal_is_a_warning() {
    // Rotor sound with no radio for 30 s (a dark drone), then an FPV video
    // signal appears and gets louder: the radio ends rule 13, but the
    // sound's held level still makes the rise a Warning.
    let flat = |t: u64| -40.0 + wobble(t, 11);
    let snd = [Snd::rotor(0, 80, &flat)];
    let lv = |t: u64| (t >= 30).then(|| 8.0 + t.saturating_sub(40).min(12) as f32);
    let sigs = [Sig::video(0, &lv)];
    let mut sim = Sim::new(&[HOUSE]);
    let seen = Seen::run(&mut sim, &[], &sigs, &snd, 55);
    assert!(seen.first(Rule::DarkDrone).unwrap() < 30);
    assert_eq!(seen.worst_of(VIDEO), Some(Level::Warning));
}

// ------------------------------------------------------------- output --

#[test]
fn signal_threats_reach_json_and_the_tracker_stays_small() {
    let a = ramp(8.0, 30, 1.0, 12);
    let b = ramp(4.0, 31, 0.9, 12);
    let sigs = [Sig::video(1, &a), Sig::video(2, &b)];
    let mut sim = Sim::new(&[HOUSE]);
    Seen::run(&mut sim, &[], &sigs, &[], 40);
    let s = video(&sim).unwrap();
    let ctx = sim.tr.context(sim.now(), None);
    let mut buf = [0u8; crate::json::MAX_SIGNAL_THREAT_LINE];
    let n = crate::json::signal_threat_line(s, &ctx, &mut buf).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&buf[..n]).unwrap();
    assert_eq!(v["t"], "signal_threat");
    assert_eq!(v["basis"], "signal_trend");
    assert_eq!((v["src"].as_str(), v["kind"].as_str(), v["what"].as_str()), (Some("rf"), Some("video"), Some(VIDEO)));
    assert_eq!(v["threat"]["level"], "warning");
    let r = &v["threat"]["reasons"][0];
    assert_eq!(r["rule"], "getting_louder");
    assert_eq!(r["nodes"], 2);
    assert!(r["db"].as_u64().unwrap() >= 6 && r["for_s"].as_u64().is_some());
    assert!(r.get("m").is_none() && r.get("eta_s").is_none() && r.get("agreed").is_none(), "{r}");
    // /api/state (`state_full`): under "signal_threats".
    let hub = crate::json::HubStatus {
        own_source: crate::json::OwnSource::Gps,
        manual: None,
        gps: crate::json::GpsStatus { state: crate::json::GpsState::Absent, sats: None },
        recording: false,
        nodes: &[],
        adsb: None,
        fix_held: false,
        utc_held: false,
    };
    let signals = crate::json::Signals { emitters: sim.tr.emitters(), wifi_drones: &[], threats: sim.tr.signal_threats(), places: Some(sim.tr.places()) };
    let len = crate::json::state_full_max_len(0, 0, 0, 0, sim.tr.emitters().len(), 0) + crate::json::MAX_SIGNAL_THREAT_LINE + 1;
    let mut big = std::vec![0u8; len];
    let n = crate::json::state_full(&[], &[], &[], &signals, &ctx, &hub, &mut big).unwrap();
    let full: serde_json::Value = serde_json::from_slice(&big[..n]).unwrap();
    assert_eq!(full["signal_threats"][0]["threat"]["reasons"][0]["rule"], "getting_louder");
    // The widest line still fits: every reason at its widest.
    let mut wide = s.clone();
    wide.threat = TrackThreat::worst_for_tests(sim.now());
    for r in wide.threat.reasons.iter_mut() {
        *r = TrackThreat::wide_for_tests(Rule::GettingLouder);
    }
    let n = crate::json::signal_threat_line(&wide, &ctx, &mut buf).expect("fits");
    std::println!("widest signal_threat line: {n} B");
    // Sizes: the P4 tracker stays well under 1 MiB.
    let hub = core::mem::size_of::<super::tests::Tracker>();
    std::println!(
        "Tracker<64,..,64> with 16c: {hub} B (trends {} B, one signal {} B)",
        core::mem::size_of::<Trends<64>>(),
        core::mem::size_of::<SignalThreat>()
    );
    assert!(hub < 1 << 20, "{hub}");
    std::println!("S3 Tracker<64, 32>: {} B", core::mem::size_of::<crate::Tracker<64, 32>>());
    assert!(core::mem::size_of::<Trends<0>>() <= 32, "{}", core::mem::size_of::<Trends<0>>());
}

#[test]
fn hubs_without_path_history_keep_no_signal_history() {
    // The S3 hub's tracker: no slots, nothing judged, nothing kept.
    let mut tr: crate::Tracker<64, { crate::aircraft::MAX_AIRCRAFT }, 0> = crate::Tracker::default();
    tr.set_own_fix(SITE.0, SITE.1, Some(OWN_ALT), 1000, Some((0.0, None)));
    for s in 0..60u64 {
        let snr = 8 + s.saturating_sub(30).min(12) as i8;
        let r = RfSighting { kind: RfKind::AnalogVideo, freq: 57_400, width_khz: 0, snr_db: snr, duty_pct: 90, line_hz: 0, uptime_us: 0 };
        tr.ingest_rf(0, &r, 1000 + s * 1000);
        tr.tick_behaviour(1000 + s * 1000);
    }
    assert!(tr.signal_threats().is_empty());
}


