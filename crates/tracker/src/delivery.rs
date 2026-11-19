//! Delivery accounting (docs/LINK-KEYS-DESIGN.md 7.3): does what a node says
//! it delivered actually reach the hub?
//!
//! A radio acknowledgement proves only that *something* on the air answered.
//! A forger who acknowledges a node's packets for it (or a gateway that
//! acknowledges and then drops them) makes the node believe it delivered
//! frames the hub never saw, and the hub silently misses what that node
//! hears. So the node counts what it believes it delivered
//! ([`link::NodeHealth::frames_sent`], cumulative, every 5 s) and the hub
//! counts what it verified from that node. Over a sliding
//! [`LOSS_WINDOW_MS`], if the node claims at least [`LOSS_MIN_FRAMES`] more
//! than arrived, and more than [`LOSS_MIN_PCT`] % of what it sent, the hub
//! raises [`ThreatKind::LinkLoss`] naming the node.
//!
//! Two calls feed it (the hub firmware, which has the verified count):
//!
//! - [`Tracker::link_received`]: every verified batch from a node, with the
//!   number of frames in it;
//! - [`Tracker::ingest_node_health`]: every `NodeHealth` from that node.
//!
//! What it is not fooled by:
//!
//! - **A lost `NodeHealth`.** The claims are running totals, so the next one
//!   carries everything; the window simply spans the gap.
//! - **A node reboot.** Its counters restart. A report whose uptime went
//!   back is a reboot: what it claims since then is its whole count, never a
//!   wrapped difference.
//! - **Counter wrap.** A total that went back while the uptime went on is a
//!   wrap (difference taken modulo 2^32), but only if that difference is one
//!   the node could have sent in the time ([`MAX_FRAMES_PER_S`]); otherwise
//!   it is a reboot the hub did not see, and only the new total is claimed.
//! - **The first report** of a node (or after [`DeliveryWatch::forget`]) is
//!   a baseline, never a claim: the hub cannot know what of its earlier
//!   frames it saw.
//!
//! Wherever it has to guess, it guesses "delivered" (an alarm missed, never
//! a false one). If forged acknowledgements swallow *everything*, the
//! `NodeHealth` reports stop too, and the existing alarms for a silent node
//! take over: this check stops raising [`LOSS_STALE_MS`] after the last
//! report.
//!
//! **Ending.** The verdict is taken at each report and raised once a second
//! from [`Tracker::expire`] while it holds, so one lost report does not end
//! the alarm. It ends by itself: when a report finds the window clean, or
//! [`LOSS_STALE_MS`] after the last report.

use heapless::Vec;
use link::{ThreatKind, ThreatSighting};

use crate::Tracker;

/// Nodes watched.
pub const DELIVERY_NODES: usize = 16;
/// The sliding window the claims and arrivals are compared over.
pub const LOSS_WINDOW_MS: u64 = 60_000;
/// At least this many frames claimed and missing over the window...
pub const LOSS_MIN_FRAMES: u32 = 50;
/// ...and more than this percent of the frames claimed.
pub const LOSS_MIN_PCT: u32 = 20;
/// A verdict counts for this long after the report that made it (four
/// report periods): one or two lost reports do not end the alarm.
pub const LOSS_STALE_MS: u64 = 20_000;
/// More frames a second than any node sends (the radio's ceiling with
/// small frames): bounds what a counter wrap can claim.
pub const MAX_FRAMES_PER_S: u64 = 4_000;
/// Reports kept per node: more than the window at one report every 5 s.
const SAMPLES: usize = 16;

/// The running totals at one report, in the hub's own accounting (both
/// wrap at 2^32; differences are taken modulo, and a window never holds
/// anywhere near that many).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Sample {
    at_ms: u64,
    claimed: u32,
    received: u32,
}

#[derive(Debug, Clone)]
struct NodeDelivery {
    node: u8,
    /// Frames verified from the node since the hub started watching it.
    received: u32,
    /// Frames the node has claimed, summed from its reports' differences.
    claimed: u32,
    /// The node's latest report: uptime (s) and `frames_sent`.
    last: Option<(u32, u32)>,
    samples: Vec<Sample, SAMPLES>,
    /// The latest report's finding: (frames missing, frames claimed,
    /// window ms), or `None` when the window was clean.
    loss: Option<(u32, u32, u32)>,
    /// When that report arrived.
    verdict_ms: u64,
    /// Hub second of the last raise (at most one a second).
    raised_s: Option<u64>,
}

impl NodeDelivery {
    const fn new(node: u8) -> Self {
        Self { node, received: 0, claimed: 0, last: None, samples: Vec::new(), loss: None, verdict_ms: 0, raised_s: None }
    }
}

/// See the module docs.
#[derive(Debug, Clone, Default)]
pub struct DeliveryWatch {
    v: Vec<NodeDelivery, DELIVERY_NODES>,
}

impl DeliveryWatch {
    pub const fn new() -> Self {
        Self { v: Vec::new() }
    }

    fn entry(&mut self, node: u8) -> &mut NodeDelivery {
        let i = match self.v.iter().position(|e| e.node == node) {
            Some(i) => i,
            None => {
                if self.v.is_full() {
                    // The quietest node makes room: the one reported longest ago.
                    let old = (0..self.v.len()).min_by_key(|&i| self.v[i].samples.last().map_or(0, |s| s.at_ms)).unwrap_or(0);
                    self.v.swap_remove(old);
                }
                let _ = self.v.push(NodeDelivery::new(node));
                self.v.len() - 1
            }
        };
        &mut self.v[i]
    }

    /// Forget `node` (its number changed hands, docs/LINK-KEYS-DESIGN.md 7.5).
    pub fn forget(&mut self, node: u8) {
        self.v.retain(|e| e.node != node);
    }

    /// The hub verified a batch of `frames` frames from `node`.
    pub fn received(&mut self, node: u8, frames: u32) {
        let e = self.entry(node);
        e.received = e.received.wrapping_add(frames);
    }

    /// One `NodeHealth` from `node`: its uptime and `frames_sent` total.
    pub fn health(&mut self, node: u8, uptime_s: u32, frames_sent: u32, now_ms: u64) {
        let e = self.entry(node);
        if let Some((up0, sent0)) = e.last {
            // Most frames the node could have sent in the time (its uptime
            // since the last report, or since a reboot).
            let could = |secs: u32| (u64::from(secs) + 1) * MAX_FRAMES_PER_S;
            let claim = if uptime_s < up0 {
                // Rebooted: everything since is new.
                frames_sent
            } else {
                // On, or wrapped: modulo 2^32.
                frames_sent.wrapping_sub(sent0)
            };
            let secs = if uptime_s < up0 { uptime_s } else { uptime_s - up0 };
            // An impossible claim (a reboot the hub did not see, whose
            // total went back): a new baseline, nothing claimed.
            if u64::from(claim) <= could(secs) {
                e.claimed = e.claimed.wrapping_add(claim);
            }
        }
        e.last = Some((uptime_s, frames_sent));
        // The window: reports within LOSS_WINDOW_MS, this one included.
        let cut = now_ms.saturating_sub(LOSS_WINDOW_MS);
        while e.samples.first().is_some_and(|s| s.at_ms < cut) || e.samples.is_full() {
            e.samples.remove(0);
        }
        let now = Sample { at_ms: now_ms, claimed: e.claimed, received: e.received };
        let _ = e.samples.push(now);
        let base = e.samples[0];
        let claimed = now.claimed.wrapping_sub(base.claimed);
        let received = now.received.wrapping_sub(base.received);
        let missing = claimed.saturating_sub(received);
        let lossy = missing >= LOSS_MIN_FRAMES && u64::from(missing) * 100 > u64::from(claimed) * u64::from(LOSS_MIN_PCT);
        let window = now_ms.saturating_sub(base.at_ms).min(u64::from(u32::MAX)) as u32;
        e.loss = lossy.then_some((missing, claimed, window));
        e.verdict_ms = now_ms;
    }

    fn current(e: &NodeDelivery, now_ms: u64) -> Option<(u32, u32, u32)> {
        e.loss.filter(|_| now_ms.saturating_sub(e.verdict_ms) < LOSS_STALE_MS)
    }

    /// The latest finding for `node`: frames missing over the window and
    /// frames claimed in it, while it holds.
    pub fn loss(&self, node: u8, now_ms: u64) -> Option<(u32, u32)> {
        let e = self.v.iter().find(|e| e.node == node)?;
        Self::current(e, now_ms).map(|(m, c, _)| (m, c))
    }

    /// Called often: for each node at most once a second, `(node, sighting)`
    /// to raise while it is losing frames.
    pub fn check(&mut self, now_ms: u64, mut raise: impl FnMut(u8, ThreatSighting)) {
        let s = now_ms / 1000;
        for e in self.v.iter_mut() {
            let Some((missing, claimed, window)) = Self::current(e, now_ms) else { continue };
            if e.raised_s == Some(s) {
                continue;
            }
            e.raised_s = Some(s);
            raise(
                e.node,
                ThreatSighting {
                    kind: ThreatKind::LinkLoss,
                    channel: 0,
                    freq: 0,
                    width_khz: 0,
                    level: 0,
                    count: missing,
                    window_ms: window,
                    source: [0; 6],
                    target: [0; 6],
                    // Percent of what it sent (claimed >= missing >= 50).
                    detail: (u64::from(missing) * 100 / u64::from(claimed.max(1))) as u32,
                    ssid: heapless::String::new(),
                    uptime_us: 0,
                },
            );
        }
    }
}

impl<const N: usize, const A: usize> Tracker<N, A> {
    /// Delivery accounting, call 1 of 2: the hub verified a batch of
    /// `frames` frames from `node` (count what it took from verified
    /// packets only, before any other check drops frames).
    pub fn link_received(&mut self, node: u8, frames: u32) {
        self.delivery.received(node, frames);
    }

    /// Delivery accounting, call 2 of 2: a `NodeHealth` from `node`. Raises
    /// (and keeps raising, from [`Tracker::expire`]) a hub
    /// [`ThreatKind::LinkLoss`] for the node while it claims far more than
    /// arrived (see [`crate::delivery`]).
    pub fn ingest_node_health(&mut self, node: u8, h: &link::NodeHealth, now_ms: u64) {
        self.delivery.health(node, h.uptime_s, h.frames_sent, now_ms);
        self.expire(now_ms);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    /// A node sending `per_report` frames every 5 s, of which the hub sees
    /// `seen_pct` %; `n` reports from `t0`. Returns the time after the last.
    fn run(tr: &mut Tracker, node: u8, state: &mut (u32, u32), t0: u64, n: u32, per_report: u32, seen_pct: u32) -> u64 {
        let mut t = t0;
        for _ in 0..n {
            t += 5_000;
            state.0 += 5;
            state.1 = state.1.wrapping_add(per_report);
            tr.link_received(node, per_report * seen_pct / 100);
            let h = link::NodeHealth { uptime_s: state.0, frames_sent: state.1, ..Default::default() };
            tr.ingest_node_health(node, &h, t);
        }
        t
    }

    fn link_loss(tr: &Tracker, node: u8, now: u64) -> Option<&crate::threats::Threat> {
        tr.threats().iter().find(|t| t.kind == ThreatKind::LinkLoss && t.node == node && t.active(now))
    }

    #[test]
    fn claimed_minus_received_raises_link_loss() {
        let mut tr: Tracker = Tracker::default();
        let mut st = (100, 5_000);
        // A healthy minute: everything arrives. Nothing raised.
        let t = run(&mut tr, 2, &mut st, 0, 13, 40, 100);
        assert!(link_loss(&tr, 2, t).is_none());
        // Half of what node 2 sends goes missing for a minute: raised,
        // naming node 2, as a hub threat, with the missing count and percent.
        let t = run(&mut tr, 2, &mut st, t, 13, 40, 50);
        let l = link_loss(&tr, 2, t).expect("link loss raised");
        assert!(l.by_hub);
        assert_eq!(l.detail, 50, "percent missing");
        assert!(l.count >= 50, "{}", l.count);
        assert_eq!(l.label(), "Node reports going missing");
        assert_eq!(tr.take_new_threat(t).map(|id| tr.get_threat(id).unwrap().kind), Some(ThreatKind::LinkLoss));
        // It is held between reports (raised from expire once a second).
        tr.expire(t + 9_000);
        assert!(link_loss(&tr, 2, t + 9_000).is_some());
        // Below the thresholds it is not: 10 % missing is not "more than
        // 20 %", and 30 % of a trickle is fewer than 50 frames.
        let mut quiet: Tracker = Tracker::default();
        let mut s = (1, 0);
        let t = run(&mut quiet, 3, &mut s, 0, 14, 400, 90);
        assert!(link_loss(&quiet, 3, t).is_none(), "10 % lost");
        let t = run(&mut quiet, 4, &mut (1, 0), 0, 14, 10, 70);
        assert!(link_loss(&quiet, 4, t).is_none(), "fewer than 50 frames");
        // Another node's loss is its own.
        assert!(link_loss(&tr, 3, t).is_none());
    }

    #[test]
    fn link_loss_ends_by_itself() {
        let mut tr: Tracker = Tracker::default();
        let mut st = (10, 0);
        let t = run(&mut tr, 2, &mut st, 0, 2, 40, 100);
        let t = run(&mut tr, 2, &mut st, t, 6, 40, 30);
        assert!(link_loss(&tr, 2, t).is_some());
        // The loss stops: once the window is clean the alarm ends.
        let t = run(&mut tr, 2, &mut st, t, 14, 40, 100);
        tr.expire(t + crate::threats::THREAT_ACTIVE_MS);
        assert!(link_loss(&tr, 2, t + crate::threats::THREAT_ACTIVE_MS).is_none());
        // And when the reports stop altogether, it stops raising too (the
        // silent-node alarms take over).
        let mut tr: Tracker = Tracker::default();
        let t = run(&mut tr, 2, &mut (10, 0), 0, 6, 40, 30);
        assert!(link_loss(&tr, 2, t).is_some());
        let later = t + LOSS_STALE_MS + crate::threats::THREAT_ACTIVE_MS;
        tr.expire(later);
        assert!(link_loss(&tr, 2, later).is_none());
    }

    #[test]
    fn lost_nodehealth_is_harmless() {
        let mut tr: Tracker = Tracker::default();
        let (mut up, mut sent) = (100u32, 7_000u32);
        let mut t = 0;
        // Everything the node sends arrives, but two of every three
        // NodeHealth reports are lost (they ride in batches too).
        for i in 0..40 {
            t += 5_000;
            up += 5;
            sent += 40;
            tr.link_received(2, 40);
            if i % 3 == 0 {
                tr.ingest_node_health(2, &link::NodeHealth { uptime_s: up, frames_sent: sent, ..Default::default() }, t);
            }
            assert!(link_loss(&tr, 2, t).is_none(), "report {i}");
        }
        // A long gap (a minute of reports lost) is harmless too.
        t += 60_000;
        up += 60;
        sent += 480;
        tr.link_received(2, 480);
        tr.ingest_node_health(2, &link::NodeHealth { uptime_s: up, frames_sent: sent, ..Default::default() }, t);
        assert!(link_loss(&tr, 2, t).is_none());
        // ...and so is the first report, whatever total it carries.
        let mut tr: Tracker = Tracker::default();
        tr.ingest_node_health(5, &link::NodeHealth { uptime_s: 9_000, frames_sent: 4_000_000, ..Default::default() }, 1_000);
        tr.expire(2_000);
        assert!(link_loss(&tr, 5, 2_000).is_none());
    }

    #[test]
    fn reboot_and_wrap_are_not_losses() {
        // A reboot: the counters restart (uptime back, total back).
        let mut tr: Tracker = Tracker::default();
        let mut st = (5_000, 900_000);
        let t = run(&mut tr, 2, &mut st, 0, 6, 40, 100);
        let mut st = (0, 0);
        let t = run(&mut tr, 2, &mut st, t, 14, 40, 100);
        assert!(link_loss(&tr, 2, t).is_none(), "reboot");
        // A wrap: the total passes u32::MAX while the uptime goes on.
        let mut tr: Tracker = Tracker::default();
        let mut st = (5_000, u32::MAX - 100);
        let t = run(&mut tr, 2, &mut st, 0, 14, 40, 100);
        assert!(st.1 < 1_000, "wrapped");
        assert!(link_loss(&tr, 2, t).is_none(), "wrap");
        // A reboot the hub did not see (reports lost meanwhile, and the new
        // uptime is already past the old one): the total went back, but by
        // more than the node could send. Not a wrap, so not a huge claim.
        let mut tr: Tracker = Tracker::default();
        let mut t = 0;
        for (up, sent) in [(10, 900_000u32), (15, 900_040)] {
            t += 5_000;
            tr.link_received(2, 40);
            tr.ingest_node_health(2, &link::NodeHealth { uptime_s: up, frames_sent: sent, ..Default::default() }, t);
        }
        t += 60_000;
        tr.link_received(2, 30);
        tr.ingest_node_health(2, &link::NodeHealth { uptime_s: 40, frames_sent: 30, ..Default::default() }, t);
        assert!(link_loss(&tr, 2, t).is_none(), "unseen reboot");
        // And a wrap that hides real loss is still caught.
        let mut tr: Tracker = Tracker::default();
        let mut st = (5_000, u32::MAX - 100);
        let t = run(&mut tr, 2, &mut st, 0, 8, 40, 40);
        assert!(link_loss(&tr, 2, t).is_some(), "loss across a wrap");
    }

    #[test]
    fn forget_drops_the_old_boards_counts() {
        let mut tr: Tracker = Tracker::default();
        let t = run(&mut tr, 2, &mut (10, 0), 0, 8, 40, 30);
        assert!(tr.delivery.loss(2, t).is_some());
        tr.forget_node(2, t);
        assert!(tr.delivery.loss(2, t).is_none());
        // The new board's first report is a baseline.
        tr.ingest_node_health(2, &link::NodeHealth { uptime_s: 1, frames_sent: 1_000, ..Default::default() }, t + 1_000);
        assert!(tr.delivery.loss(2, t + 1_000).is_none());
    }

    /// Missing reports mean the hub may be missing drones: like Blinded,
    /// it can only be snoozed, for that node, and never from the site file.
    /// Its JSON line names the kind.
    #[test]
    fn link_loss_is_snoozed_per_node_never_muted() {
        use crate::threats::{Mute, SNOOZE_MS, is_safety_kind};
        assert!(is_safety_kind(ThreatKind::LinkLoss));
        let mut tr: Tracker = Tracker::default();
        let t = run(&mut tr, 2, &mut (10, 0), 0, 8, 40, 30);
        let id = link_loss(&tr, 2, t).unwrap().id;
        let m = tr.silence(id, t).unwrap();
        assert!(m.snooze());
        assert_eq!((m.node, m.until_s), (2, ((t + SNOOZE_MS) / 1000) as u32));
        assert!(Mute { node: crate::threats::NODE_ANY, until_s: 0, ..Mute::empty(ThreatKind::LinkLoss) }.check_permanent().is_err());
        assert_eq!(crate::site::kind_from_name("link_loss"), Some(ThreatKind::LinkLoss));
        let ctx = crate::Context { now_ms: t, utc_ms: None, own: None, config: crate::Config::default(), flood: None };
        let mut buf = [0u8; crate::threats::MAX_THREAT_LINE];
        let n = crate::threats::threat_line(link_loss(&tr, 2, t).unwrap(), &ctx, &mut buf).unwrap();
        let line = core::str::from_utf8(&buf[..n]).unwrap();
        assert!(line.contains("\"kind\":\"link_loss\",\"label\":\"Node reports going missing\""), "{line}");
        assert!(line.contains("\"by_hub\":true,\"node\":2"), "{line}");
    }

    /// More nodes than the table holds: no panic, and the newest are kept.
    #[test]
    fn table_full_keeps_working() {
        let mut w = DeliveryWatch::new();
        for n in 0..40u8 {
            w.received(n, 5);
            w.health(n, 5, 5, u64::from(n) * 1_000);
        }
        assert_eq!(w.v.len(), DELIVERY_NODES);
        assert!(w.v.iter().any(|e| e.node == 39));
    }
}
