//! Passive O4 channel follower: predicts which DroneID frequency an O4
//! drone publishes on next, so a single-tuner receiver can be there in time.
//!
//! # The schedule (EdwardBlair/dji-droneid-spec RF_PHY.md, "RF channel sequence")
//! Publications are nominally 640 ms apart. An 8-bit publication counter `c`
//! (schedule state, not a packet field) picks the channel:
//! `r = (c + 255) mod 256`, `i = (r + 6) mod 7`, frequency =
//! `[2429.5, 2444.5, 5756.5, 5776.5, 5796.5, 5816.5, 2414.5][i]` MHz. The
//! mod-256 step comes first, so at counter wrap the walk through the seven
//! channels jumps (256 is not a multiple of 7). A full counter cycle is
//! 256 x 0.64 s = 163.84 s.
//!
//! # How following works
//! The first integrity-valid packet of a session (an O4 key ID) starts 256
//! hypotheses, one per possible counter value, and keeps those that put
//! that packet on the channel it was heard on (about 37). Every later valid
//! packet `k` publications on keeps only the hypotheses `c + k` that predict
//! its channel. Because the counter steps by one, all surviving hypotheses
//! almost always agree on the next channel, so prediction works from the
//! first packet; they disagree only around the counter wrap, where a
//! missed window on a predicted channel (with the receiver settled there
//! for the whole window) removes the hypotheses that predicted it, as the
//! spec allows.
//!
//! **Fallback.** The spec's schedule is not the only report: King-Of-Knights
//! lists eight frequencies including 2459.5 MHz and calls the hopping random
//! per drone. So the follower is an optimisation, never a dependency: a
//! packet no hypothesis explains restarts the session from that packet,
//! [`MAX_MISSES`] silent windows in a row or [`LOST_US`] without a packet
//! drop it, and the receiver goes back to scanning its plan (which keeps
//! 2459.5 MHz). Channel phase never identifies or authenticates a drone; the
//! key ID is what groups packets.
//!
//! `no_std`, integer time in microseconds, no heap, ~200 bytes per session.

/// Seven O4 channel centres in spec table order (index `i`), 100 kHz units.
pub const CHANNELS: [u32; 7] = [24295, 24445, 57565, 57765, 57965, 58165, 24145];
/// Nominal time between publications.
pub const PERIOD_US: u64 = 640_000;
/// Sessions followed at once.
pub const MAX_SESSIONS: usize = 4;
/// Silent predicted windows in a row before a session is dropped.
pub const MAX_MISSES: u8 = 4;
/// A session with no valid packet for this long is dropped (about 9 periods).
pub const LOST_US: u64 = 6_000_000;
/// Window half-width at one period out, and its growth per further period.
pub const GUARD_US: u64 = 25_000;
pub const GUARD_PER_PERIOD_US: u64 = 4_000;
/// The longest gap (in publications) a packet may be matched across.
pub const MAX_GAP: u64 = 64;

/// Spec table index for counter `c`.
pub const fn channel_index(c: u8) -> usize {
    let r = c.wrapping_add(255);
    (r as usize + 6) % 7
}

/// Channel centre (100 kHz units) for counter `c`.
pub const fn channel(c: u8) -> u32 {
    CHANNELS[channel_index(c)]
}

/// 256-bit set of counter values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Set([u32; 8]);

impl Set {
    pub fn contains(&self, c: u8) -> bool {
        self.0[c as usize / 32] & (1 << (c % 32)) != 0
    }
    fn insert(&mut self, c: u8) {
        self.0[c as usize / 32] |= 1 << (c % 32);
    }
    pub fn len(&self) -> u32 {
        self.0.iter().map(|w| w.count_ones()).sum()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn iter(&self) -> impl Iterator<Item = u8> + '_ {
        (0..=255u8).filter(|&c| self.contains(c))
    }
    /// The counters whose channel is `freq`.
    fn on(freq: u32) -> Self {
        let mut s = Set::default();
        for c in 0..=255u8 {
            if channel(c) == freq {
                s.insert(c);
            }
        }
        s
    }
    /// `{c + k : c in self, channel(c + k) == freq}`.
    fn advance(&self, k: u64, freq: u32) -> Self {
        let mut s = Set::default();
        for c in self.iter() {
            let n = c.wrapping_add(k as u8);
            if channel(n) == freq {
                s.insert(n);
            }
        }
        s
    }
}

/// One followed session (one O4 key ID).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Session {
    pub id: u32,
    /// Counter hypotheses for the anchor packet.
    pub cand: Set,
    /// Time of the last valid packet.
    pub anchor_us: u64,
    /// Measured publication period (starts at [`PERIOD_US`]).
    pub period_us: u64,
    pub misses: u8,
    /// Valid packets matched (1 = just acquired).
    pub hits: u32,
    /// Times a packet contradicted every hypothesis and restarted the session.
    pub restarts: u32,
}

impl Session {
    fn new(id: u32, t: u64, freq: u32) -> Self {
        Session { id, cand: Set::on(freq), anchor_us: t, period_us: PERIOD_US, misses: 0, hits: 1, restarts: 0 }
    }

    /// Publications between the anchor and `t`, rounded.
    fn ordinal(&self, t: u64) -> u64 {
        (t.saturating_sub(self.anchor_us) + self.period_us / 2) / self.period_us
    }

    /// Channel votes for publication `k` after the anchor: (channel, votes)
    /// of the best-supported channel and the total.
    fn vote(&self, k: u64) -> (u32, u32, u32) {
        let mut n = [0u32; 7];
        for c in self.cand.iter() {
            n[channel_index(c.wrapping_add(k as u8))] += 1;
        }
        let (i, &best) = n.iter().enumerate().max_by_key(|&(i, &v)| (v, 7 - i)).unwrap_or((0, &0));
        (CHANNELS[i], best, n.iter().sum())
    }
}

/// Where and when to listen for a predicted publication.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Appointment {
    pub id: u32,
    /// Channel centre, 100 kHz units.
    pub freq: u32,
    /// Publications after the session's anchor.
    pub k: u64,
    /// Predicted publication time and the window around it.
    pub at_us: u64,
    pub start_us: u64,
    pub end_us: u64,
    /// Every hypothesis agrees on `freq`.
    pub unanimous: bool,
}

/// What an observation did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Observed {
    /// First packet of a session: hypotheses started.
    Acquired,
    /// Matched; the hypotheses left.
    Matched { hypotheses: u32 },
    /// Same publication heard again (e.g. two bursts in one capture).
    Duplicate,
    /// No hypothesis explained it, or the gap was too long: restarted.
    Restarted,
}

/// Followed sessions.
#[derive(Clone, Debug, Default)]
pub struct Follower {
    pub sessions: [Option<Session>; MAX_SESSIONS],
}

impl Follower {
    pub const fn new() -> Self {
        Follower { sessions: [None; MAX_SESSIONS] }
    }

    pub fn session(&self, id: u32) -> Option<&Session> {
        self.sessions.iter().flatten().find(|s| s.id == id)
    }

    pub fn active(&self) -> usize {
        self.sessions.iter().flatten().count()
    }

    /// A valid packet of session `id` heard at `t_us` on `freq`. Packets
    /// on frequencies outside the seven-channel table are ignored (None).
    pub fn observe(&mut self, id: u32, t_us: u64, freq: u32) -> Option<Observed> {
        if !CHANNELS.contains(&freq) {
            return None;
        }
        let slot = match self.sessions.iter().position(|s| s.is_some_and(|s| s.id == id)) {
            Some(i) => i,
            None => {
                // A free slot, else the stalest session.
                let i = self
                    .sessions
                    .iter()
                    .position(|s| s.is_none())
                    .unwrap_or_else(|| (0..MAX_SESSIONS).min_by_key(|&i| self.sessions[i].map_or(0, |s| s.anchor_us)).unwrap_or(0));
                self.sessions[i] = Some(Session::new(id, t_us, freq));
                return Some(Observed::Acquired);
            }
        };
        let s = self.sessions[slot].as_mut()?;
        if t_us < s.anchor_us {
            return Some(Observed::Duplicate);
        }
        let k = s.ordinal(t_us);
        if k == 0 {
            return Some(Observed::Duplicate);
        }
        let next = s.cand.advance(k, freq);
        if next.is_empty() || k > MAX_GAP {
            let r = s.restarts + 1;
            *s = Session::new(id, t_us, freq);
            s.restarts = r;
            return Some(Observed::Restarted);
        }
        // Refine the period from this gap, within +-1% of nominal.
        let measured = (t_us - s.anchor_us) / k;
        let lo = PERIOD_US - PERIOD_US / 100;
        let hi = PERIOD_US + PERIOD_US / 100;
        if (lo..=hi).contains(&measured) {
            s.period_us = (3 * s.period_us + measured) / 4;
        }
        s.cand = next;
        s.anchor_us = t_us;
        s.misses = 0;
        s.hits += 1;
        Some(Observed::Matched { hypotheses: next.len() })
    }

    /// Drop sessions silent for [`LOST_US`].
    pub fn expire(&mut self, now_us: u64) {
        for s in self.sessions.iter_mut() {
            if s.is_some_and(|x| now_us.saturating_sub(x.anchor_us) > LOST_US) {
                *s = None;
            }
        }
    }

    /// The prediction for session `id` whose window has not yet closed at
    /// `now_us`.
    pub fn predict(&self, id: u32, now_us: u64) -> Option<Appointment> {
        let s = self.session(id)?;
        let mut k = s.ordinal(now_us).max(1);
        loop {
            let at = s.anchor_us + k * s.period_us;
            let guard = (GUARD_US + k * GUARD_PER_PERIOD_US).min(3 * GUARD_US);
            if at + guard > now_us {
                let (freq, votes, total) = s.vote(k);
                return Some(Appointment {
                    id,
                    freq,
                    k,
                    at_us: at,
                    start_us: at.saturating_sub(guard),
                    end_us: at + guard,
                    unanimous: votes == total,
                });
            }
            k += 1;
        }
    }

    /// The earliest open appointment over all sessions (after expiring the
    /// lost ones). With two drones whose windows overlap, the earlier wins;
    /// the other is skipped this time (not counted as a miss).
    pub fn next(&mut self, now_us: u64) -> Option<Appointment> {
        self.expire(now_us);
        self.sessions.iter().flatten().filter_map(|s| self.predict(s.id, now_us)).min_by_key(|a| a.start_us)
    }

    /// The receiver sat on `a.freq` for the whole of appointment `a`, settled
    /// and capturing, and heard nothing valid from that session. Hypotheses
    /// that predicted `a.freq` are removed when others predicted another
    /// channel. Every miss counts towards [`MAX_MISSES`] (a lost packet and
    /// a wrong hypothesis look the same); a valid packet resets the count.
    /// Returns false when the session was dropped.
    pub fn missed(&mut self, a: &Appointment) -> bool {
        let Some(slot) = self.sessions.iter().position(|s| s.is_some_and(|s| s.id == a.id)) else { return false };
        let Some(s) = self.sessions[slot].as_mut() else { return false };
        // A miss is either a lost packet or a wrong hypothesis; the receiver
        // cannot tell, so it always counts, and a hit resets the count.
        s.misses += 1;
        if s.misses >= MAX_MISSES {
            self.sessions[slot] = None;
            return false;
        }
        if !a.unanimous {
            let mut keep = Set::default();
            for c in s.cand.iter() {
                if channel(c.wrapping_add(a.k as u8)) != a.freq {
                    keep.insert(c);
                }
            }
            s.cand = keep;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_matches_the_published_formula() {
        // Direct transcription of the spec's three lines.
        let table = [24295, 24445, 57565, 57765, 57965, 58165, 24145];
        for c in 0..=255u32 {
            let r = (c + 255) % 256;
            let i = (r + 6) % 7;
            assert_eq!(channel(c as u8), table[i as usize]);
        }
        // c = 0: r = 255, i = 2 (5756.5); c = 1: r = 0, i = 6 (2414.5).
        assert_eq!((channel(0), channel(1), channel(2)), (57565, 24145, 24295));
        // r wraps between counters 0 and 1 (r = 255 -> 0): the only step
        // that does not advance the channel index by one.
        for c in 0..=255u8 {
            let step = (channel_index(c) + 1) % 7 == channel_index(c.wrapping_add(1));
            assert_eq!(step, c != 0, "counter {c}");
        }
        // Every channel is used, 36 or 37 times per cycle.
        for f in CHANNELS {
            let n = (0..=255u8).filter(|&c| channel(c) == f).count();
            assert!((36..=37).contains(&n), "{f}: {n}");
        }
    }

    /// A drone's publications: (time, frequency) for counters from `c0`.
    fn drone(c0: u8, t0: u64, n: usize, period: u64) -> Vec<(u64, u32)> {
        (0..n).map(|p| (t0 + p as u64 * period, channel(c0.wrapping_add(p as u8)))).collect()
    }

    #[test]
    fn locks_after_one_packet_and_predicts_every_next_channel() {
        for c0 in [0u8, 7, 100, 200, 250] {
            let pubs = drone(c0, 1_000_000, 300, PERIOD_US);
            let mut f = Follower::new();
            assert_eq!(f.observe(9, pubs[0].0, pubs[0].1), Some(Observed::Acquired));
            // Hear every 3rd publication (a scanner busy elsewhere), predict
            // all the ones between.
            let mut correct = 0;
            let mut total = 0;
            for p in 1..pubs.len() {
                let (t, freq) = pubs[p];
                let a = f.predict(9, t - 100_000).unwrap();
                assert_eq!(a.at_us, t, "c0 {c0} p {p}");
                assert!(a.start_us < t && a.end_us > t);
                total += 1;
                correct += (a.freq == freq) as u32;
                if p % 3 == 0 {
                    assert!(matches!(f.observe(9, t, freq), Some(Observed::Matched { .. })), "c0 {c0} p {p}");
                }
            }
            // Wrong only near a counter wrap, where hypotheses still disagree.
            assert!(correct * 100 >= total * 97, "c0 {c0}: {correct}/{total}");
        }
    }

    #[test]
    fn counter_wrap_is_resolved_by_observation_and_by_settled_misses() {
        // Start 3 publications before the wrap (counter 253).
        let pubs = drone(253, 0, 12, PERIOD_US);
        let mut f = Follower::new();
        f.observe(1, pubs[0].0, pubs[0].1);
        // Hypotheses disagree about the publication after the wrap. Each
        // settled miss on a wrong prediction prunes; a hit confirms.
        for (p, &(t, freq)) in pubs.iter().enumerate().skip(1) {
            let a = f.predict(1, t - 50_000).unwrap();
            if a.freq == freq {
                assert!(matches!(f.observe(1, t, freq), Some(Observed::Matched { .. })));
            } else {
                assert!(!a.unanimous, "p {p}: wrong although unanimous");
                assert!(f.missed(&a));
                // Not heard this time; next prediction must still be fine.
            }
        }
        // By now the counter is pinned down.
        assert_eq!(f.session(1).unwrap().cand.len(), 1);
        let s = f.session(1).unwrap();
        let c_last = s.cand.iter().next().unwrap();
        assert_eq!(c_last, 253u8.wrapping_add(11));
    }

    #[test]
    fn missed_packets_are_bridged_and_period_tracked() {
        // A slightly fast clock: 639.2 ms.
        let period = 639_200;
        let pubs = drone(40, 5_000, 120, period);
        let mut f = Follower::new();
        f.observe(2, pubs[0].0, pubs[0].1);
        for p in [4usize, 9, 10, 25, 26, 40, 41, 60, 61, 62, 80] {
            assert!(matches!(f.observe(2, pubs[p].0, pubs[p].1), Some(Observed::Matched { .. })), "p {p}");
        }
        let s = f.session(2).unwrap();
        assert!(s.period_us.abs_diff(period) < 300, "period {}", s.period_us);
        // Ten publications out, the window still covers the true time.
        let a = f.predict(2, pubs[90].0 - 200_000).unwrap();
        assert!(a.start_us <= pubs[90].0 && pubs[90].0 <= a.end_us);
        assert_eq!(a.freq, pubs[90].1);
    }

    #[test]
    fn two_drones_at_once() {
        let a = drone(3, 0, 60, PERIOD_US);
        let b = drone(181, 233_000, 60, PERIOD_US);
        let mut f = Follower::new();
        f.observe(0xA, a[0].0, a[0].1);
        f.observe(0xB, b[0].0, b[0].1);
        for p in 1..60 {
            if p % 2 == 1 {
                f.observe(0xA, a[p].0, a[p].1);
            } else {
                f.observe(0xB, b[p].0, b[p].1);
            }
        }
        assert_eq!(f.active(), 2);
        // The next appointment is the earlier of the two, with its channel.
        let now = a[59].0 + 10_000;
        let next = f.next(now).unwrap();
        let (pa, pb) = (f.predict(0xA, now).unwrap(), f.predict(0xB, now).unwrap());
        assert_eq!(next, if pa.start_us <= pb.start_us { pa } else { pb });
        assert_eq!(pb.freq, channel(181u8.wrapping_add((pb.at_us - 233_000).div_ceil(PERIOD_US) as u8)));
        assert_eq!(pa.freq, channel(3u8.wrapping_add((pa.at_us / PERIOD_US) as u8)));
    }

    #[test]
    fn falls_back_on_loss_and_on_a_different_schedule() {
        let pubs = drone(10, 0, 40, PERIOD_US);
        let mut f = Follower::new();
        f.observe(5, pubs[0].0, pubs[0].1);
        f.observe(5, pubs[1].0, pubs[1].1);
        // Drone lands: four settled misses drop the session.
        let mut now = pubs[1].0;
        let mut dropped = false;
        for _ in 0..MAX_MISSES {
            let a = f.next(now + 1).unwrap();
            if !f.missed(&a) {
                dropped = true;
            }
            now = a.end_us;
        }
        assert!(dropped && f.active() == 0);
        // Silence alone also drops it.
        f.observe(6, 0, pubs[0].1);
        assert!(f.next(LOST_US + 1).is_none());
        // A drone hopping some other way (random channels): every
        // contradiction restarts, never panics, never predicts beyond one
        // period without new evidence.
        let mut x = 0x1234_5678u32;
        let mut restarts = 0;
        for p in 0..200u64 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let freq = CHANNELS[(x % 7) as usize];
            if f.observe(7, p * PERIOD_US, freq) == Some(Observed::Restarted) {
                restarts += 1;
            }
        }
        assert!(restarts > 100, "{restarts}");
        // 2459.5 MHz (not in the published table) is ignored, not followed.
        assert_eq!(f.observe(8, 0, 24595), None);
    }

    #[test]
    fn duplicates_and_long_gaps() {
        let pubs = drone(77, 1_000_000, 200, PERIOD_US);
        let mut f = Follower::new();
        f.observe(3, pubs[0].0, pubs[0].1);
        assert_eq!(f.observe(3, pubs[0].0 + 300, pubs[0].1), Some(Observed::Duplicate));
        // 100 publications later (64 s) is beyond MAX_GAP: restart.
        assert_eq!(f.observe(3, pubs[100].0, pubs[100].1), Some(Observed::Restarted));
        assert_eq!(f.session(3).unwrap().restarts, 1);
    }
}
