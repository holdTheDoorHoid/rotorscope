//! The hub's deaf-node check (`threats::DeafWatch`, `ThreatKind::Blinded`)
//! against what ends a busy spell rather than a jammer (Phase 2 bench,
//! 2026-11-18: after a 1000 frames/s injected flood on c5a stopped, the P4
//! raised "Node blinded" for node 2 and held it for good while the node
//! heard its usual ~80 frames/s).

extern crate std;

use link::ThreatKind;

use crate::threats::{DEAF_HOLD_MS, DEAF_MIN_NORMAL};
use crate::*;

/// One node sending a Stats report every second.
struct Node {
    node: u8,
    t_ms: u64,
    up_us: u64,
    seen: u32,
    dropped: u32,
}

impl Node {
    fn new(node: u8) -> Self {
        Self { node, t_ms: 0, up_us: 0, seen: 0, dropped: 0 }
    }

    /// `secs` reports, each after a second hearing `seen` management frames
    /// of which `dropped` were shed or lost to a full receive pool.
    fn run(&mut self, tr: &mut Tracker, secs: u32, seen: u32, dropped: u32) {
        for _ in 0..secs {
            self.t_ms += 1_000;
            self.up_us += 1_000_000;
            self.seen = self.seen.wrapping_add(seen);
            self.dropped = self.dropped.wrapping_add(dropped);
            tr.ingest_node_stats(self.node, self.up_us, self.seen, self.dropped, self.t_ms);
        }
    }

    /// Seconds (of the next `secs`) during which a Blinded alarm for this
    /// node was active, and the last such second (0: none).
    fn blinded_secs(&mut self, tr: &mut Tracker, secs: u32, seen: u32, dropped: u32) -> (u32, u32) {
        let (mut n, mut last) = (0, 0);
        for s in 1..=secs {
            self.run(tr, 1, seen, dropped);
            if blinded(tr, self.node, self.t_ms) {
                n += 1;
                last = s;
            }
        }
        (n, last)
    }
}

fn blinded(tr: &Tracker, node: u8, now_ms: u64) -> bool {
    tr.threats().iter().any(|t| t.kind == ThreatKind::Blinded && t.node == node && t.active(now_ms))
}

/// The bench case: settled at 80 frames/s, then minutes of flood at 300 and
/// 1000 a second, then quiet again. Nothing raises, whether the node counts
/// the flood as shed (`dropped`, as node-wifi does for a Remote ID flood)
/// or not at all (a header-only flood: deauths are counted, never queued).
#[test]
fn flood_then_quiet_raises_nothing() {
    for shed in [true, false] {
        let mut tr: Tracker = Tracker::default();
        let mut n = Node::new(2);
        n.run(&mut tr, 180, 80, 0);
        let (normal, _) = tr.node_rates(2, n.t_ms).unwrap();
        assert!((normal - 80.0).abs() < 2.0, "{normal}");
        for (rate, secs) in [(300, 60), (1000, 120), (300, 60), (1000, 60)] {
            let d = if shed { rate - 8 } else { 0 };
            assert_eq!(n.blinded_secs(&mut tr, secs, 80 + rate, d), (0, 0));
        }
        // After the flood: ten minutes at the usual rate, never blinded.
        assert_eq!(n.blinded_secs(&mut tr, 600, 80, 0), (0, 0), "shed {shed}");
        assert_eq!(tr.take_deaf_relearn(), None);
        // The flood barely moved what is normal.
        let (normal, rate) = tr.node_rates(2, n.t_ms).unwrap();
        std::println!("after the flood (shed {shed}): normal {normal:.1}/s");
        assert!(normal < 80.0 * 1.6, "shed {shed}: normal {normal}");
        assert!((rate - 80.0).abs() < 1.0);
    }
}

/// A real busy spell (12x the usual for 30 minutes, nothing shed) that ends:
/// at most a short alarm, which ends by itself within a bounded time.
#[test]
fn long_busy_spell_ends_with_at_most_a_short_alarm() {
    let mut tr: Tracker = Tracker::default();
    let mut n = Node::new(3);
    n.run(&mut tr, 300, 80, 0);
    assert_eq!(n.blinded_secs(&mut tr, 1800, 1000, 0), (0, 0));
    let (secs, last) = n.blinded_secs(&mut tr, 3600, 80, 0);
    // Raised, if at all, for under two minutes, and over within three.
    assert!(secs < 120 && last < 180, "{secs} s, last at {last} s");
    assert!(!blinded(&tr, 3, n.t_ms));
    // If it was raised, the hub was told why it ended.
    let notice = tr.take_deaf_relearn();
    assert_eq!(notice.is_some(), secs > 0, "{notice:?}");
    std::println!("busy spell: blinded {secs} s, notice {notice:?}");
}

/// A node that stops hearing anything: raised within the hold time and
/// still raised an hour later (silence is never learned as normal).
#[test]
fn silence_raises_and_stays_raised() {
    let mut tr: Tracker = Tracker::default();
    let mut n = Node::new(4);
    n.run(&mut tr, 300, 200, 0);
    let mut first = None;
    for s in 1..=3600u32 {
        n.run(&mut tr, 1, 0, 0);
        let b = blinded(&tr, 4, n.t_ms);
        if b {
            first.get_or_insert(s);
        }
        if s > 20 {
            assert!(b, "alarm ended at {s} s of silence");
        }
    }
    let first = first.unwrap();
    assert!(u64::from(first) * 1000 <= DEAF_HOLD_MS + 1_000, "raised after {first} s");
    // What is normal sank, but never under the floor.
    let (normal, _) = tr.node_rates(4, n.t_ms).unwrap();
    assert!(normal >= DEAF_MIN_NORMAL, "{normal}");
    assert_eq!(tr.take_deaf_relearn(), None);
}

/// A node that hears far less from now on (5 % of before, a quiet place
/// rather than a jammer): raised, then the lower level becomes the new
/// normal and the alarm ends by itself.
#[test]
fn lasting_drop_is_relearned_and_the_alarm_ends() {
    let mut tr: Tracker = Tracker::default();
    let mut n = Node::new(5);
    n.run(&mut tr, 300, 200, 0);
    let mut first = None;
    let mut last = 0;
    let mut notice = None;
    for s in 1..=1200u32 {
        n.run(&mut tr, 1, 10, 0);
        if blinded(&tr, 5, n.t_ms) {
            first.get_or_insert(s);
            last = s;
        }
        if let Some(r) = tr.take_deaf_relearn() {
            assert!(notice.replace((s, r)).is_none(), "a second notice at {s} s");
        }
    }
    // The re-learn is announced once, as the node's new normal.
    let (at, r) = notice.unwrap();
    assert_eq!(r.node, 5);
    assert!((r.rate - 10.0).abs() < 0.5 && r.was > 190.0, "{r:?}");
    assert!((130..=160).contains(&at), "notice at {at} s");
    assert!(first.unwrap() <= 11, "raised at {:?}", first);
    // Bound: hold (10 s) + re-learning to under 10x the new rate (~135 s
    // at a 180 s time constant) + the alarm's own 10 s; see DeafWatch.
    assert!(last <= 170, "still raised at {last} s");
    assert!(!blinded(&tr, 5, n.t_ms));
}

/// While raised, a reboot or a new plan still starts the learning again:
/// the alarm ends and nothing raises until relearned.
#[test]
fn reboot_or_new_plan_while_raised_starts_again() {
    for reboot in [true, false] {
        let mut tr: Tracker = Tracker::default();
        let mut n = Node::new(6);
        tr.ingest_status(6, link::NodeRole::Wifi24, 0, 1, 0);
        n.run(&mut tr, 300, 200, 0);
        n.run(&mut tr, 30, 0, 0);
        assert!(blinded(&tr, 6, n.t_ms));
        if reboot {
            n.up_us = 0;
            n.seen = 0;
            n.dropped = 0;
        } else {
            tr.ingest_status(6, link::NodeRole::Wifi24, 0, 2, n.t_ms);
        }
        // The node now hears 5/s: not judged until learned again.
        let (_, last) = n.blinded_secs(&mut tr, 120, 5, 0);
        assert!(last <= 10, "reboot {reboot}: last {last}");
        assert!(!blinded(&tr, 6, n.t_ms));
        // Ended by starting again: not a re-learn.
        assert_eq!(tr.take_deaf_relearn(), None);
    }
}
