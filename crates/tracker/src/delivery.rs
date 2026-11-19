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
//! take over: the comparison stops raising [`LOSS_STALE_MS`] after the last
//! report.
//!
//! **Reports swallowed, the rest let through.** The comparison runs when a
//! report arrives, so whoever drops the packets carrying `NodeHealth` (a
//! captured gateway can tell them apart: frames are authenticated, not
//! hidden) would blind it while dropping whatever else it liked. So a node
//! that has reported before, is still heard ([`HEARD_RECENT_MS`], at least
//! [`OVERDUE_MIN_FRAMES`] frames since its last report) and whose last
//! report is [`REPORTS_OVERDUE_MS`] old is raised as the same `LinkLoss`,
//! with detail [`LOSS_DETAIL_NO_REPORTS`] (bench, 2026-11-19).
//!
//! **A quiet node.** A node that sends a hundred frames a minute could lose
//! almost half of them and stay under [`LOSS_MIN_FRAMES`] in every minute.
//! So the same two thresholds are also applied over [`SLOW_WINDOW_MS`]: a
//! loss that lasts shows there even when no single minute holds enough
//! frames (bench follow-up, 2026-11-19). The long window's finding counts
//! only while the last minute still shows loss, so it ends with the loss.
//!
//! **Holding.** Once raised, the finding holds until the loss is under half
//! of both thresholds ([`LOSS_HOLD_FRAMES`], [`LOSS_HOLD_PCT`]). When reports
//! are themselves being lost the window shrinks to a few reports and the
//! count in it dips under the raise threshold while the loss goes on; on the
//! bench that ended the alarm and raised it again. One loss is one alarm.
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
/// Once raised, the finding holds while at least this many are missing...
pub const LOSS_HOLD_FRAMES: u32 = LOSS_MIN_FRAMES / 2;
/// ...and more than this percent of the frames claimed.
pub const LOSS_HOLD_PCT: u32 = LOSS_MIN_PCT / 2;
/// The long window the same thresholds are applied over, for a node that
/// sends too little for the short one to hold [`LOSS_MIN_FRAMES`].
pub const SLOW_WINDOW_MS: u64 = 300_000;
/// One report in this long is kept for the long window.
pub const SLOW_STEP_MS: u64 = 30_000;
/// A verdict counts for this long after the report that made it (four
/// report periods): one or two lost reports do not end the alarm.
pub const LOSS_STALE_MS: u64 = 20_000;
/// A node that has reported before, is still being heard, and whose last
/// report is this old (six report periods) has had its reports swallowed:
/// raised as [`ThreatKind::LinkLoss`] with detail [`LOSS_DETAIL_NO_REPORTS`].
pub const REPORTS_OVERDUE_MS: u64 = 30_000;
/// "Still being heard": frames verified from the node within this long...
pub const HEARD_RECENT_MS: u64 = 10_000;
/// ...and at least this many since its last report (a stray frame or two
/// after a node stops is not "heard").
pub const OVERDUE_MIN_FRAMES: u32 = 10;
/// `detail` of a `LinkLoss` raised because the reports themselves stopped
/// arriving. The ordinary finding's detail is a percentage of at least 20.
pub const LOSS_DETAIL_NO_REPORTS: u32 = 0;
/// Added to the percentage in `detail` when the finding is the long
/// window's ([`SLOW_WINDOW_MS`]), so the text can say which time it covers.
pub const LOSS_DETAIL_SLOW: u32 = 1_000;
/// More frames a second than any node sends (the radio's ceiling with
/// small frames): bounds what a counter wrap can claim.
pub const MAX_FRAMES_PER_S: u64 = 4_000;
/// Reports kept per node: more than the window at one report every 5 s.
const SAMPLES: usize = 16;
/// Reports kept per node for the long window: one per step, and one more.
const SLOW_SAMPLES: usize = (SLOW_WINDOW_MS / SLOW_STEP_MS) as usize + 2;

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
    /// One report per [`SLOW_STEP_MS`], over [`SLOW_WINDOW_MS`].
    slow: Vec<Sample, SLOW_SAMPLES>,
    /// The latest report's finding: (frames missing, frames claimed,
    /// window ms), or `None` when the window was clean.
    loss: Option<(u32, u32, u32)>,
    /// That finding is the long window's, not the short one's.
    slow_found: bool,
    /// When that report arrived.
    verdict_ms: u64,
    /// Hub second of the last raise (at most one a second).
    raised_s: Option<u64>,
    /// `received` as [`DeliveryWatch::check`] last saw it, and when it last
    /// saw it grow: when the node was last heard.
    heard_total: u32,
    heard_ms: Option<u64>,
}

impl NodeDelivery {
    const fn new(node: u8) -> Self {
        Self { node, received: 0, claimed: 0, last: None, samples: Vec::new(), slow: Vec::new(), loss: None, slow_found: false, verdict_ms: 0, raised_s: None, heard_total: 0, heard_ms: None }
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
        // The long window: one report per step, the oldest dropped.
        let cut = now_ms.saturating_sub(SLOW_WINDOW_MS);
        while e.slow.first().is_some_and(|s| s.at_ms < cut) || e.slow.is_full() {
            e.slow.remove(0);
        }
        if e.slow.last().is_none_or(|s| now_ms.saturating_sub(s.at_ms) >= SLOW_STEP_MS) {
            let _ = e.slow.push(now);
        }
        // A finding already raised holds down to half the thresholds.
        let (min_frames, min_pct) = if e.loss.is_some() { (LOSS_HOLD_FRAMES, LOSS_HOLD_PCT) } else { (LOSS_MIN_FRAMES, LOSS_MIN_PCT) };
        let over = |base: Sample| {
            let claimed = now.claimed.wrapping_sub(base.claimed);
            let received = now.received.wrapping_sub(base.received);
            let missing = claimed.saturating_sub(received);
            let lossy = missing >= min_frames && u64::from(missing) * 100 > u64::from(claimed) * u64::from(min_pct);
            let window = now_ms.saturating_sub(base.at_ms).min(u64::from(u32::MAX)) as u32;
            lossy.then_some((missing, claimed, window))
        };
        // The short window's finding first: it is the sharper statement.
        // The long window's counts only while the loss is still going on
        // (the last minute is over the hold percentage, whatever its
        // count), so the alarm ends within a minute of the loss ending and
        // not five minutes later.
        let short = over(e.samples[0]);
        let base = e.samples[0];
        let (claimed, received) = (now.claimed.wrapping_sub(base.claimed), now.received.wrapping_sub(base.received));
        let going_on = u64::from(claimed.saturating_sub(received)) * 100 > u64::from(claimed) * u64::from(LOSS_HOLD_PCT);
        e.slow_found = short.is_none();
        e.loss = short.or_else(|| e.slow.first().and_then(|&b| over(b)).filter(|_| going_on));
        e.verdict_ms = now_ms;
    }

    fn current(e: &NodeDelivery, now_ms: u64) -> Option<(u32, u32, u32)> {
        e.loss.filter(|_| now_ms.saturating_sub(e.verdict_ms) < LOSS_STALE_MS)
    }

    /// The reports-overdue finding: `(frames heard since the last report, its
    /// age in ms)` for a node that has reported, is still heard, and whose
    /// last report is [`REPORTS_OVERDUE_MS`] old.
    fn overdue(e: &NodeDelivery, now_ms: u64) -> Option<(u32, u32)> {
        let last = e.samples.last()?;
        let age = now_ms.saturating_sub(last.at_ms);
        let since = e.received.wrapping_sub(last.received);
        let heard = e.heard_ms.is_some_and(|h| now_ms.saturating_sub(h) < HEARD_RECENT_MS);
        (age >= REPORTS_OVERDUE_MS && heard && since >= OVERDUE_MIN_FRAMES).then_some((since, age.min(u64::from(u32::MAX)) as u32))
    }

    /// The latest finding for `node`: frames missing over the window and
    /// frames claimed in it, while it holds.
    pub fn loss(&self, node: u8, now_ms: u64) -> Option<(u32, u32)> {
        let e = self.v.iter().find(|e| e.node == node)?;
        Self::current(e, now_ms).map(|(m, c, _)| (m, c))
    }

    /// For the console, per node watched: `(node, claimed, received,
    /// window_ms, reports, age_ms, lossy)`: the frames the node claimed and
    /// the hub verified between the oldest report in the window and the
    /// latest, how many reports the window holds, how old the latest is,
    /// and whether that report found the window lossy. Totals, not secrets.
    pub fn snapshot(&self, now_ms: u64, mut f: impl FnMut(u8, u32, u32, u32, usize, u64, bool)) {
        for e in self.v.iter() {
            let (Some(base), Some(last)) = (e.samples.first(), e.samples.last()) else { continue };
            f(
                e.node,
                last.claimed.wrapping_sub(base.claimed),
                last.received.wrapping_sub(base.received),
                last.at_ms.saturating_sub(base.at_ms).min(u64::from(u32::MAX)) as u32,
                e.samples.len(),
                now_ms.saturating_sub(last.at_ms),
                e.loss.is_some(),
            );
        }
    }

    /// Called often: for each node at most once a second, `(node, sighting)`
    /// to raise while it is losing frames.
    pub fn check(&mut self, now_ms: u64, mut raise: impl FnMut(u8, ThreatSighting)) {
        let s = now_ms / 1000;
        for e in self.v.iter_mut() {
            if e.received != e.heard_total {
                e.heard_total = e.received;
                e.heard_ms = Some(now_ms);
            }
            // The comparison's verdict while it holds; failing that, a node
            // still heard whose reports have stopped arriving.
            let (count, window, detail) = match (Self::current(e, now_ms), Self::overdue(e, now_ms)) {
                // Percent of what it sent (claimed >= missing >= 50).
                (Some((missing, claimed, window)), _) => {
                    let pct = (u64::from(missing) * 100 / u64::from(claimed.max(1))) as u32;
                    (missing, window, if e.slow_found { LOSS_DETAIL_SLOW + pct } else { pct })
                }
                (None, Some((since, age))) => (since, age, LOSS_DETAIL_NO_REPORTS),
                (None, None) => continue,
            };
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
                    count,
                    window_ms: window,
                    source: [0; 6],
                    target: [0; 6],
                    detail,
                    ssid: heapless::String::new(),
                    uptime_us: 0,
                },
            );
        }
    }
}

/// Plain text for the evidence log and console: which node (`mac` is its
/// address), and what is missing over which time.
pub fn describe(t: &crate::threats::Threat, mac: [u8; 6]) -> heapless::String<320> {
    use core::fmt::Write;
    let mut s = heapless::String::new();
    let m = mac;
    let _ = write!(s, "LINK LOSS: node {} ({:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}) ", t.node, m[0], m[1], m[2], m[3], m[4], m[5]);
    let why = "(forged acknowledgements, a gateway dropping its packets, or heavy loss)";
    let _ = if t.detail == LOSS_DETAIL_NO_REPORTS {
        write!(
            s,
            "is still heard, but its delivery reports stopped arriving {} s ago or more, so what it sends cannot be checked {why}",
            REPORTS_OVERDUE_MS / 1000
        )
    } else {
        let (pct, over) = if t.detail >= LOSS_DETAIL_SLOW { (t.detail - LOSS_DETAIL_SLOW, "five minutes") } else { (t.detail, "minute") };
        write!(s, "says it delivered frames the hub never verified: {pct} % of what it sent over the last {over} is missing {why}")
    };
    s
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

    /// The delivery check's numbers per node, for the console
    /// ([`DeliveryWatch::snapshot`]).
    pub fn delivery_snapshot(&self, now_ms: u64, f: impl FnMut(u8, u32, u32, u32, usize, u64, bool)) {
        self.delivery.snapshot(now_ms, f);
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
        assert!(describe(l, [0; 6]).contains("50 % of what it sent over the last minute is missing"));
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

    /// A quiet node (20 frames a report, 240 a minute at most in a window)
    /// losing 30 % of them: 6 a report, at most 72 in a minute's window...
    /// over the raise threshold there. So take a quieter one: 10 frames a
    /// report, 30 % lost is 3 a report, 36 in any minute: never 50. The long
    /// window sees 180 missing of 600.
    #[test]
    fn a_quiet_node_losing_steadily_is_found_by_the_long_window() {
        let mut tr: Tracker = Tracker::default();
        let mut st = (100, 5_000);
        let t = run(&mut tr, 2, &mut st, 0, 13, 10, 100);
        assert!(link_loss(&tr, 2, t).is_none());
        // One minute of it: under 50 frames, nothing yet.
        let t = run(&mut tr, 2, &mut st, t, 12, 10, 70);
        assert!(link_loss(&tr, 2, t).is_none(), "36 missing in a minute is under the threshold");
        // It goes on: found within the long window.
        let mut t = t;
        let mut found = None;
        for i in 0..48 {
            t = run(&mut tr, 2, &mut st, t, 1, 10, 70);
            if let (None, Some(l)) = (found, link_loss(&tr, 2, t)) {
                found = Some(i);
                assert_eq!(l.detail, LOSS_DETAIL_SLOW + 20, "just over 20 %, marked as the long window's");
                let text = describe(l, [2, 0, 0, 0, 0, 0x1a]);
                assert!(text.starts_with("LINK LOSS: node 2 (02:00:00:00:00:1a) says it delivered"), "{text}");
                assert!(text.contains("20 % of what it sent over the last five minutes is missing"), "{text}");
            }
        }
        let l = link_loss(&tr, 2, t).expect("a lasting loss on a quiet node is raised");
        // 120 frames arrived whole after the long window's first report and
        // before the loss began, so "more than 20 % of what it sent" takes
        // 25 lossy reports (3k * 100 > 20 * (120 + 10k)): about two minutes.
        assert_eq!(found, Some(25 - 12 - 1), "raised 25 reports (125 s) after the loss began");
        // Once raised it holds on the last minute alone (36 missing is over
        // the hold threshold), so the figure is the minute's again.
        assert_eq!(l.detail, 30, "percent missing over the last minute");
        // A quiet node that loses nothing, or a frame now and then, never is.
        let mut ok: Tracker = Tracker::default();
        let t = run(&mut ok, 3, &mut (1, 0), 0, 150, 10, 90);
        assert!(link_loss(&ok, 3, t).is_none(), "10 % of a trickle for 12 minutes");
        // The loss stops: the alarm ends once the long window has moved on.
        let t = run(&mut tr, 2, &mut st, t, 70, 10, 100);
        tr.expire(t + crate::threats::THREAT_ACTIVE_MS);
        assert!(link_loss(&tr, 2, t + crate::threats::THREAT_ACTIVE_MS).is_none());
    }

    /// Bench, 2026-11-19: with the loss scattered and reports themselves
    /// lost, the window shrank to a few reports, the count in it dipped
    /// under 50 while 60 % was still being lost, and the alarm ended and
    /// was raised again. Once raised it now holds down to half the
    /// thresholds: one loss, one alarm.
    #[test]
    fn a_raised_loss_holds_through_a_dip_under_the_raise_threshold() {
        let mut w = DeliveryWatch::new();
        let (mut up, mut sent, mut t) = (100u32, 0u32, 0u64);
        let mut report = |w: &mut DeliveryWatch, t: &mut u64, gap_s: u32, frames: u32, seen: u32| {
            *t += u64::from(gap_s) * 1_000;
            up += gap_s;
            sent += frames;
            w.received(2, seen);
            w.health(2, up, sent, *t);
        };
        for _ in 0..4 {
            report(&mut w, &mut t, 5, 40, 40);
        }
        // Heavy loss: raised.
        for _ in 0..4 {
            report(&mut w, &mut t, 5, 40, 16);
        }
        assert!(w.loss(2, t).is_some());
        // Reports go missing too: after a long gap the short window holds
        // two reports and 30 frames missing of 50. Under the raise
        // threshold of 50 frames, over the hold threshold of 25.
        report(&mut w, &mut t, 70, 50, 20);
        report(&mut w, &mut t, 5, 50, 20);
        let (missing, claimed) = w.loss(2, t).expect("still held");
        assert!(missing >= LOSS_HOLD_FRAMES && missing * 100 > claimed * LOSS_HOLD_PCT, "{missing} of {claimed}");
        // The same two reports on a node with nothing raised raise nothing
        // (the long window is kept out of it: these are its first reports).
        let mut fresh = DeliveryWatch::new();
        fresh.health(3, 10, 0, 1_000);
        fresh.received(3, 20);
        fresh.health(3, 15, 49, 6_000);
        assert!(fresh.loss(3, 6_000).is_none(), "29 missing does not raise");
        // And the hold ends when the loss does.
        for _ in 0..70 {
            report(&mut w, &mut t, 5, 40, 40);
        }
        assert!(w.loss(2, t).is_none());
    }

    /// A gateway that drops the packets carrying `NodeHealth` (it can tell
    /// them by size or content: frames are authenticated, not hidden) while
    /// passing some of the rest. Found on the bench, 2026-11-19: for 110 s
    /// no report arrived, the node was still heard, and nothing was raised,
    /// because the comparison only runs when a report arrives. A node that
    /// is heard but whose reports stopped is itself a finding.
    #[test]
    fn swallowed_health_reports_while_the_node_is_heard_raise_link_loss() {
        let mut tr: Tracker = Tracker::default();
        let mut st = (100, 5_000);
        let t0 = run(&mut tr, 2, &mut st, 0, 13, 12, 100);
        assert!(link_loss(&tr, 2, t0).is_none());
        // From now on the reports are swallowed; a quarter of the node's
        // other frames still arrive.
        let mut t = t0;
        let mut raised_at = None;
        for s in 1..=45u64 {
            t = t0 + s * 1_000;
            if s % 5 == 0 {
                tr.link_received(2, 3);
            }
            tr.expire(t);
            if raised_at.is_none() && link_loss(&tr, 2, t).is_some() {
                raised_at = Some(s * 1_000);
            }
        }
        let at = raised_at.expect("a heard node whose reports stopped is reported");
        assert!((REPORTS_OVERDUE_MS..REPORTS_OVERDUE_MS + 2_000).contains(&at), "raised after {at} ms");
        let l = link_loss(&tr, 2, t).unwrap();
        assert!(l.by_hub);
        assert_eq!(l.detail, LOSS_DETAIL_NO_REPORTS, "not a percentage: the reports themselves are missing");
        let text = describe(l, [0; 6]);
        assert!(text.contains("its delivery reports stopped arriving 30 s ago or more") && !text.contains('%'), "{text}");
        assert!(text.len() < 300, "{}", text.len());
        // A report gets through at last. It is cumulative, so the ordinary
        // comparison now sees what went missing: still raised, as a percent.
        let t = t + 1_000;
        st.0 += 46;
        st.1 += 9 * 12;
        tr.ingest_node_health(2, &link::NodeHealth { uptime_s: st.0, frames_sent: st.1, ..Default::default() }, t);
        let l = link_loss(&tr, 2, t).expect("still raised");
        assert!(l.detail >= 20, "{}", l.detail);
        // Reports flow again and everything arrives: it ends by itself.
        let t = run(&mut tr, 2, &mut st, t, 14, 12, 100);
        tr.expire(t + crate::threats::THREAT_ACTIVE_MS);
        assert!(link_loss(&tr, 2, t + crate::threats::THREAT_ACTIVE_MS).is_none());
    }

    /// What the reports-overdue rule must not catch: a node that fell
    /// silent altogether (the silent-node alarms have it), a node that never
    /// reported (old firmware, or not heard long enough), and a stray frame
    /// or two after the last report.
    #[test]
    fn overdue_reports_are_not_raised_for_silent_unreported_or_barely_heard_nodes() {
        // Silent altogether.
        let mut tr: Tracker = Tracker::default();
        let t0 = run(&mut tr, 2, &mut (100, 5_000), 0, 13, 12, 100);
        for s in 1..=120u64 {
            tr.expire(t0 + s * 1_000);
            assert!(link_loss(&tr, 2, t0 + s * 1_000).is_none(), "silent at {s} s");
        }
        // Never reported: frames only.
        let mut tr: Tracker = Tracker::default();
        for s in 1..=120u64 {
            tr.link_received(3, 5);
            tr.expire(s * 1_000);
            assert!(link_loss(&tr, 3, s * 1_000).is_none());
        }
        // A few stray frames after the last report, then silence.
        let mut tr: Tracker = Tracker::default();
        let t0 = run(&mut tr, 4, &mut (100, 5_000), 0, 13, 12, 100);
        tr.link_received(4, OVERDUE_MIN_FRAMES - 1);
        for s in 1..=120u64 {
            tr.expire(t0 + s * 1_000);
            assert!(link_loss(&tr, 4, t0 + s * 1_000).is_none(), "stray frames at {s} s");
        }
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
