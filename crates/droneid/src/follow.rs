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
//! candidate that is not yet followed restarts from a packet no hypothesis
//! explains, [`MAX_MISSES`] silent windows in a row or [`LOST_US`] without a
//! packet drop a followed session, and the receiver goes back to scanning
//! its plan (which keeps 2459.5 MHz). Channel phase never identifies or
//! authenticates a drone; the key ID is what groups packets.
//!
//! # Hostile packets (docs/DRONEID-O4.md "The follower under forged packets")
//! The key ID travels in clear and the CRCs are public, so anyone with a
//! transmitter can send structurally valid O4 bursts with any key ID on any
//! channel at any time. Nothing here can tell them from a drone. What the
//! rules below do is bound what a forger gets:
//!
//! - **Confirmation.** A key ID that is not followed is a *candidate*, a
//!   16-byte entry in a table of [`CANDIDATES`] (its hypotheses are rebuilt
//!   from its last two packets, never stored). It is never visited: it only
//!   grows from packets the plan's scanning happens to hear. It is followed
//!   once it holds [`CONFIRM_HITS`] packets that are mutually consistent in
//!   channel *and* time (each inside the window the earlier ones predict,
//!   [`guard_us`]) and one of the [`MAX_SESSIONS`] followed slots is free.
//! - **A flood of new key IDs cannot push a real drone out.** When the
//!   table is full a new key ID takes the entry with the oldest last packet
//!   (counted in [`Follower::evicted`]). Every new key ID costs the node one
//!   full decode of a valid burst, and the decode budget
//!   (`rawcap::budget`) lets through at most 87 of those in
//!   [`TENTATIVE_LOST_US`] (30 s at the fastest measured decode, 144 ms;
//!   `crates/rawcap/tests/follow_forge.rs` checks it), fewer than
//!   [`CANDIDATES`]. So a flood only ever displaces candidates older than
//!   that, which would have expired anyway, and the forger has nothing to
//!   steer: no rate, timing or choice of key IDs gets a younger entry
//!   evicted. Before 2026-11-18 there were four such slots and a forged key
//!   ID every 0.7 s replaced a real drone's first packet within ~3 s, long
//!   before the plan heard it again (~8 s): the drone was never followed.
//! - **No eviction or restart of a followed session.** A packet that
//!   contradicts a followed session (wrong channel, or off its predicted
//!   time) is ignored and counted, never obeyed: the session keeps its
//!   hypotheses and timing and ends only by its own misses or silence. A
//!   confirmed candidate never displaces a followed session; with every
//!   slot held it waits (still counting packets) until one ends.
//! - **Time share.** Every visit is paid from a credit ([`Share`]) that
//!   earns [`SHARE_PERMILLE`] of the time spent on the plan and holds at most
//!   [`SHARE_CREDIT_US`], so however many sessions there are, the follower
//!   takes at most about half of the receiver's time from the plan.
//!
//! What a forger can still do: follow the published schedule exactly and
//! create followed sessions; with all [`MAX_SESSIONS`] slots held before a
//! real drone is confirmed, the drone is not followed (it is still heard by
//! the plan, which is then cut into the gaps between the forged sessions'
//! visits: see docs/DRONEID-O4.md for what that costs). Copy a real drone's
//! key ID and feed its candidate inconsistent packets (it keeps restarting),
//! or its followed session consistent ones. And the plan's own hearing is
//! not uniform in time: while sessions are followed it is away from the
//! plan around their publication times, so a drone that publishes at the
//! same moments as a followed (forged) session is heard by the plan only
//! when that session's visit is declined.
//!
//! `no_std`, integer time in microseconds, no heap: [`MAX_SESSIONS`] x ~64
//! bytes of sessions and [`CANDIDATES`] x 16 bytes of candidates (~2.3 KiB
//! with the share, `size_of::<Follower>()`).

/// Seven O4 channel centres in spec table order (index `i`), 100 kHz units.
pub const CHANNELS: [u32; 7] = [24295, 24445, 57565, 57765, 57965, 58165, 24145];
/// Nominal time between publications.
pub const PERIOD_US: u64 = 640_000;
/// Sessions followed (confirmed) at once.
pub const MAX_SESSIONS: usize = 4;
/// Key IDs remembered while not followed (see the module docs: more than
/// the decode budget can let new key IDs through in [`TENTATIVE_LOST_US`]).
pub const CANDIDATES: usize = 128;
/// Mutually consistent packets (the first one included) before a session
/// is followed. Until then the plan's scanning is the only way it grows: a
/// parked receiver hears about one publication in seven, the DroneID plan
/// about one every 8 s, so a real drone is followed after ~15 s instead of
/// after its first packet. A forger must place three packets on the right
/// channels within the predicted windows instead of one anywhere.
pub const CONFIRM_HITS: u32 = 3;
/// A candidate is dropped this long after its last consistent packet
/// (longer than [`LOST_US`]: it is never visited, so its packets come only
/// from scanning).
pub const TENTATIVE_LOST_US: u64 = 30_000_000;
/// Share of the receiver's time the follower may take from the plan, per
/// mille: following one drone costs about a fifth (a ~50 ms window, the
/// 50 ms lead, up to 120 ms early), two about two fifths.
pub const SHARE_PERMILLE: u64 = 500;
/// Follower time the share may save up (µs of credit; a visit costs its
/// length times `1 - share`). Over any stretch `T` the follower gets at most
/// `T / 2 + 2 s`.
pub const SHARE_CREDIT_US: u64 = 1_000_000;
/// Silent predicted windows in a row before a session is dropped.
pub const MAX_MISSES: u8 = 4;
/// A session with no valid packet for this long is dropped (about 9 periods).
pub const LOST_US: u64 = 6_000_000;
/// A followed session this long without a packet goes before the others
/// ([`Follower::next`]), so the share cannot starve it into [`LOST_US`].
pub const STARVE_US: u64 = 2_000_000;
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

/// Window half-width for publication `k` after an anchor (the same window
/// [`Follower::predict`] visits; a later packet must fall inside it to count
/// as consistent).
pub const fn guard_us(k: u64) -> u64 {
    let g = GUARD_US + k * GUARD_PER_PERIOD_US;
    if g < 3 * GUARD_US { g } else { 3 * GUARD_US }
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
    /// Mutually consistent packets.
    pub hits: u32,
    /// Packets that contradicted the session and were ignored.
    pub contradictions: u32,
    /// Hypotheses disagree and the last visit proved nothing (see
    /// [`Follower::inconclusive`]): the next one goes to the second-best
    /// channel.
    pub alt: bool,
}

impl Session {
    /// Publications between the anchor and `t`, rounded.
    fn ordinal(&self, t: u64) -> u64 {
        (t.saturating_sub(self.anchor_us) + self.period_us / 2) / self.period_us
    }

    /// The hypotheses left if a packet on `freq` at `t`, `k` publications
    /// after the anchor, belongs to this session: `None` if it cannot (a
    /// channel no hypothesis predicts, outside the predicted window, or
    /// across too long a gap).
    fn fits(&self, k: u64, t: u64, freq: u32) -> Option<Set> {
        if k > MAX_GAP {
            return None;
        }
        let at = self.anchor_us + k * self.period_us;
        if t.abs_diff(at) > guard_us(k) {
            return None;
        }
        let next = self.cand.advance(k, freq);
        (!next.is_empty()).then_some(next)
    }

    /// Channel votes for publication `k` after the anchor: (channel, votes)
    /// of the best-supported channel (the second-best with [`Session::alt`]
    /// when there is one) and the total.
    fn vote(&self, k: u64) -> (u32, u32, u32) {
        let mut n = [0u32; 7];
        for c in self.cand.iter() {
            n[channel_index(c.wrapping_add(k as u8))] += 1;
        }
        let rank = |&(i, &v): &(usize, &u32)| (v, 7 - i);
        let (i, &best) = n.iter().enumerate().max_by_key(rank).unwrap_or((0, &0));
        if self.alt
            && let Some((j, &second)) = n.iter().enumerate().filter(|&(j, &v)| j != i && v > 0).max_by_key(rank)
        {
            return (CHANNELS[j], second, n.iter().sum());
        }
        (CHANNELS[i], best, n.iter().sum())
    }
}

/// A key ID heard but not followed: its last packet, and the one before
/// (enough to rebuild its hypotheses). 16 bytes; `hits` 0 = an empty entry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Candidate {
    pub id: u32,
    /// Time of the last consistent packet.
    pub t_us: u64,
    /// Channel index ([`CHANNELS`]) of the last packet and of the one
    /// before, and the publications between them.
    pub ch: u8,
    pub prev_ch: u8,
    pub gap: u8,
    /// Mutually consistent packets (1 = just heard; saturates).
    pub hits: u8,
}

impl Candidate {
    fn new(id: u32, t_us: u64, ch: usize) -> Self {
        Candidate { id, t_us, ch: ch as u8, prev_ch: 0, gap: 0, hits: 1 }
    }

    /// Counter hypotheses for the last packet, from the last two (a
    /// superset of what every packet would leave, which only makes
    /// matching a little more lenient).
    pub fn hypotheses(&self) -> Set {
        let last = CHANNELS[self.ch as usize];
        if self.hits <= 1 { Set::on(last) } else { Set::on(CHANNELS[self.prev_ch as usize]).advance(self.gap as u64, last) }
    }

    /// As a session anchored on its last packet (for predictions).
    fn as_session(&self) -> Session {
        Session {
            id: self.id,
            cand: self.hypotheses(),
            anchor_us: self.t_us,
            period_us: PERIOD_US,
            misses: 0,
            hits: self.hits as u32,
            contradictions: 0,
            alt: false,
        }
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
    /// First packet of a key ID: a candidate started.
    Acquired,
    /// Consistent with the session or candidate; the hypotheses left.
    Matched { hypotheses: u32 },
    /// Consistent, and the session is now followed.
    Confirmed { hypotheses: u32 },
    /// Same publication heard again (e.g. two bursts in one capture).
    Duplicate,
    /// A candidate that no hypothesis explained it for (or the gap was too
    /// long): restarted from this packet.
    Restarted,
    /// It contradicts a followed session: ignored (counted).
    Contradicted,
}

/// The follower's share of the receiver's time ([`SHARE_PERMILLE`]): a
/// credit that time on the plan earns and visits spend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Share {
    credit_us: i64,
    /// End of the last visit paid for (time before it earns nothing).
    last_us: u64,
    /// Visits paid for and refused, and the follower time granted.
    pub granted: u32,
    pub declined: u32,
    pub used_us: u64,
}

impl Default for Share {
    fn default() -> Self {
        Self::new()
    }
}

impl Share {
    pub const fn new() -> Self {
        Share { credit_us: SHARE_CREDIT_US as i64, last_us: 0, granted: 0, declined: 0, used_us: 0 }
    }

    /// The credit at `t` if nothing is spent before.
    fn credit_at(&self, t: u64) -> i64 {
        let earned = (t.saturating_sub(self.last_us) * SHARE_PERMILLE / 1000) as i64;
        (self.credit_us + earned).min(SHARE_CREDIT_US as i64)
    }

    fn cost(us: u64) -> i64 {
        (us * (1000 - SHARE_PERMILLE) / 1000) as i64
    }

    /// Whether a visit from `from_us` to `until_us` could be paid then.
    pub fn affordable(&self, from_us: u64, until_us: u64) -> bool {
        self.credit_at(from_us) >= Self::cost(until_us.saturating_sub(from_us))
    }

    /// Pay for a visit from `now_us` to `until_us`; false (counted) if the
    /// credit does not cover it.
    pub fn take(&mut self, now_us: u64, until_us: u64) -> bool {
        if !self.affordable(now_us, until_us) {
            self.declined += 1;
            return false;
        }
        let len = until_us.saturating_sub(now_us);
        self.credit_us = self.credit_at(now_us) - Self::cost(len);
        self.last_us = until_us.max(now_us);
        self.granted += 1;
        self.used_us += len;
        true
    }
}

/// Followed sessions, candidates, and the time share.
#[derive(Clone, Debug)]
pub struct Follower {
    pub sessions: [Option<Session>; MAX_SESSIONS],
    pub candidates: [Candidate; CANDIDATES],
    pub share: Share,
    /// Packets that contradicted a followed session (ignored), and
    /// candidates displaced by a new key ID before they expired (with the
    /// decode budget in front, a flood cannot make this grow: see the
    /// module docs).
    pub contradictions: u32,
    pub evicted: u32,
}

impl Default for Follower {
    fn default() -> Self {
        Self::new()
    }
}

impl Follower {
    pub const fn new() -> Self {
        Follower {
            sessions: [None; MAX_SESSIONS],
            candidates: [Candidate { id: 0, t_us: 0, ch: 0, prev_ch: 0, gap: 0, hits: 0 }; CANDIDATES],
            share: Share::new(),
            contradictions: 0,
            evicted: 0,
        }
    }

    /// Forget every session and candidate (counters and share kept): the
    /// bench's `hil forge` starts from nothing. In place, so no copy of the
    /// table passes through the stack.
    pub fn clear(&mut self) {
        self.sessions = [None; MAX_SESSIONS];
        for c in self.candidates.iter_mut() {
            *c = Candidate::default();
        }
    }

    /// The followed session of key ID `id`.
    pub fn session(&self, id: u32) -> Option<&Session> {
        self.sessions.iter().flatten().find(|s| s.id == id)
    }

    /// The candidate (not yet followed) of key ID `id`.
    pub fn candidate(&self, id: u32) -> Option<&Candidate> {
        self.candidates.iter().find(|c| c.hits > 0 && c.id == id)
    }

    fn slot(&self, id: u32) -> Option<usize> {
        self.sessions.iter().position(|s| s.is_some_and(|s| s.id == id))
    }

    /// Followed sessions.
    pub fn active(&self) -> usize {
        self.sessions.iter().flatten().count()
    }

    /// Candidates (key IDs heard, not followed).
    pub fn tentative(&self) -> usize {
        self.candidates.iter().filter(|c| c.hits > 0).count()
    }

    /// A valid packet of session `id` heard at `t_us` on `freq`. Packets
    /// on frequencies outside the seven-channel table are ignored (None).
    pub fn observe(&mut self, id: u32, t_us: u64, freq: u32) -> Option<Observed> {
        let ch = CHANNELS.iter().position(|&c| c == freq)?;
        self.expire(t_us);
        let Some(slot) = self.slot(id) else {
            return Some(self.observe_candidate(id, t_us, ch));
        };
        let s = self.sessions[slot].as_mut()?;
        if t_us < s.anchor_us {
            return Some(Observed::Duplicate);
        }
        let k = s.ordinal(t_us);
        if k == 0 {
            return Some(Observed::Duplicate);
        }
        let Some(next) = s.fits(k, t_us, freq) else {
            // Never obeyed: a forged packet with this key ID would
            // otherwise wipe the real drone's hypotheses.
            s.contradictions += 1;
            self.contradictions += 1;
            return Some(Observed::Contradicted);
        };
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
        s.alt = false;
        s.hits += 1;
        Some(Observed::Matched { hypotheses: next.len() })
    }

    /// A packet of key ID `id`, not followed.
    fn observe_candidate(&mut self, id: u32, t_us: u64, ch: usize) -> Observed {
        let Some(i) = self.candidates.iter().position(|c| c.hits > 0 && c.id == id) else {
            self.acquire(id, t_us, ch);
            return Observed::Acquired;
        };
        let c = self.candidates[i];
        if t_us < c.t_us {
            return Observed::Duplicate;
        }
        let s = c.as_session();
        let k = s.ordinal(t_us);
        if k == 0 {
            return Observed::Duplicate;
        }
        let Some(next) = s.fits(k, t_us, CHANNELS[ch]) else {
            self.candidates[i] = Candidate::new(id, t_us, ch);
            return Observed::Restarted;
        };
        let hits = c.hits.saturating_add(1);
        if hits as u32 >= CONFIRM_HITS
            && let Some(free) = self.sessions.iter().position(|s| s.is_none())
        {
            self.sessions[free] = Some(Session {
                id,
                cand: next,
                anchor_us: t_us,
                period_us: PERIOD_US,
                misses: 0,
                hits: hits as u32,
                contradictions: 0,
                alt: false,
            });
            self.candidates[i] = Candidate::default();
            return Observed::Confirmed { hypotheses: next.len() };
        }
        self.candidates[i] = Candidate { id, t_us, ch: ch as u8, prev_ch: c.ch, gap: k as u8, hits };
        Observed::Matched { hypotheses: next.len() }
    }

    /// A new key ID: an empty entry, else the one whose last packet is
    /// oldest (counted: see the module docs for why a flood cannot steer
    /// this onto a live candidate).
    fn acquire(&mut self, id: u32, t_us: u64, ch: usize) {
        let i = match self.candidates.iter().position(|c| c.hits == 0) {
            Some(i) => i,
            None => {
                self.evicted += 1;
                (0..CANDIDATES).min_by_key(|&i| self.candidates[i].t_us).unwrap_or(0)
            }
        };
        self.candidates[i] = Candidate::new(id, t_us, ch);
    }

    /// Drop followed sessions silent for [`LOST_US`] and candidates for
    /// [`TENTATIVE_LOST_US`].
    pub fn expire(&mut self, now_us: u64) {
        for s in self.sessions.iter_mut() {
            if s.is_some_and(|x| now_us.saturating_sub(x.anchor_us) > LOST_US) {
                *s = None;
            }
        }
        for c in self.candidates.iter_mut() {
            if c.hits > 0 && now_us.saturating_sub(c.t_us) > TENTATIVE_LOST_US {
                *c = Candidate::default();
            }
        }
    }

    /// The prediction for key ID `id` (followed or candidate) whose window
    /// has not yet closed at `now_us`. Only followed sessions get
    /// appointments ([`Follower::next`]).
    pub fn predict(&self, id: u32, now_us: u64) -> Option<Appointment> {
        let s = match self.session(id) {
            Some(s) => *s,
            None => self.candidate(id)?.as_session(),
        };
        let mut k = s.ordinal(now_us).max(1);
        loop {
            let at = s.anchor_us + k * s.period_us;
            let guard = guard_us(k);
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

    /// The next appointment over the followed sessions (after expiring the
    /// lost ones): the earliest open window, unless a session has gone
    /// [`STARVE_US`] without a packet, in which case the window of the one
    /// silent longest. With two drones whose windows overlap, the earlier
    /// wins; the other is skipped this time (not counted as a miss).
    /// Whether the receiver may go is [`Follower::grant`]'s decision.
    ///
    /// Without the starvation rule the share went first come: with four
    /// forged sessions taking every window, a real drone followed before
    /// them had its visits declined until it expired ([`LOST_US`]), and
    /// with every slot taken it was never followed again.
    pub fn next(&mut self, now_us: u64) -> Option<Appointment> {
        self.expire(now_us);
        let starving = self.sessions.iter().flatten().filter(|s| now_us.saturating_sub(s.anchor_us) > STARVE_US).min_by_key(|s| s.anchor_us);
        if let Some(s) = starving {
            return self.predict(s.id, now_us);
        }
        self.sessions.iter().flatten().filter_map(|s| self.predict(s.id, now_us)).min_by_key(|a| a.start_us)
    }

    /// Whether leaving the plan at `depart_us` for `a` would be within the
    /// share (to decide whether to cut a plan dwell short for it).
    pub fn affordable(&self, a: &Appointment, depart_us: u64) -> bool {
        self.share.affordable(depart_us, a.end_us)
    }

    /// Pay for a visit from `now_us` until `until_us` (normally `a.end_us`).
    /// False when the share does not cover it: stay on the plan.
    pub fn grant(&mut self, now_us: u64, until_us: u64) -> bool {
        self.share.take(now_us, until_us)
    }

    /// The receiver visited `a` and heard nothing from that session, but
    /// the visit proves nothing: it arrived after the window opened, or a
    /// decode of something else held the loop during it. Not a miss (the
    /// drone may well have been there). Where the hypotheses disagree (at a
    /// counter wrap), the next visit tries the second-best channel instead:
    /// without that, on a busy channel where every visit is interrupted,
    /// nothing ever pruned the wrong majority and the session ran out at
    /// [`LOST_US`] (bench 2026-11-18, `hil forge 180 flood`: 21 of 26 silent
    /// visits interrupted, the drone lost at its wrap).
    pub fn inconclusive(&mut self, a: &Appointment) {
        if let Some(slot) = self.slot(a.id)
            && let Some(s) = self.sessions[slot].as_mut()
            && !a.unanimous
        {
            s.alt = !s.alt;
        }
    }

    /// The receiver sat on `a.freq` for the whole of appointment `a`, settled
    /// and capturing, and heard nothing valid from that session. Hypotheses
    /// that predicted `a.freq` are removed when others predicted another
    /// channel. Every miss counts towards [`MAX_MISSES`] (a lost packet and
    /// a wrong hypothesis look the same); a valid packet resets the count.
    /// Returns false when the session was dropped.
    pub fn missed(&mut self, a: &Appointment) -> bool {
        let Some(slot) = self.slot(a.id) else { return false };
        let Some(s) = self.sessions[slot].as_mut() else { return false };
        // A miss is either a lost packet or a wrong hypothesis; the receiver
        // cannot tell, so it always counts, and a hit resets the count.
        s.misses += 1;
        if s.misses >= MAX_MISSES {
            self.sessions[slot] = None;
            return false;
        }
        s.alt = false;
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
    fn memory() {
        // The module docs' figures (static: the follower lives in node-sdr's
        // boxed Radio, no heap of its own).
        assert_eq!(core::mem::size_of::<Candidate>(), 16);
        assert!(core::mem::size_of::<Option<Session>>() <= 72, "{}", core::mem::size_of::<Option<Session>>());
        assert!(core::mem::size_of::<Follower>() <= 2_400, "{}", core::mem::size_of::<Follower>());
    }

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
                    assert!(matches!(f.observe(9, t, freq), Some(Observed::Matched { .. } | Observed::Confirmed { .. })), "c0 {c0} p {p}");
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
                assert!(matches!(f.observe(1, t, freq), Some(Observed::Matched { .. } | Observed::Confirmed { .. })));
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
    fn a_wrap_on_a_busy_channel_is_resolved_without_counted_misses() {
        // Every visit that hears nothing is inconclusive (a decode of
        // something else held the loop, as on a busy 2.4 GHz channel), so
        // nothing is ever counted as a miss. Before Follower::inconclusive
        // the wrong majority at the counter wrap was never pruned, every
        // visit went to the wrong channel and the session ran out.
        for c0 in [250u8, 251, 252, 253, 254] {
            let pubs = drone(c0, 0, 40, PERIOD_US);
            let mut f = Follower::new();
            for p in &pubs[..CONFIRM_HITS as usize] {
                f.observe(1, p.0, p.1);
            }
            let mut heard = 0;
            for &(t, freq) in &pubs[CONFIRM_HITS as usize..] {
                let Some(a) = f.next(t - 60_000) else { break };
                assert_eq!(a.at_us, t);
                if a.freq == freq {
                    f.observe(1, t, freq);
                    heard += 1;
                } else {
                    f.inconclusive(&a);
                }
            }
            assert!(f.session(1).is_some(), "c0 {c0}: lost after {heard}");
            assert!(heard >= 30, "c0 {c0}: {heard}");
        }
    }

    #[test]
    fn missed_packets_are_bridged_and_period_tracked() {
        // A slightly fast clock: 639.2 ms.
        let period = 639_200;
        let pubs = drone(40, 5_000, 120, period);
        let mut f = Follower::new();
        f.observe(2, pubs[0].0, pubs[0].1);
        // Gaps up to nine publications: once followed (from the third
        // packet) a session silent for LOST_US (~9.4 periods) is dropped.
        for p in [4usize, 9, 10, 18, 19, 27, 28, 37, 38, 39, 48] {
            assert!(matches!(f.observe(2, pubs[p].0, pubs[p].1), Some(Observed::Matched { .. } | Observed::Confirmed { .. })), "p {p}");
        }
        let s = f.session(2).unwrap();
        assert!(s.period_us.abs_diff(period) < 300, "period {}", s.period_us);
        // Ten publications out, the window still covers the true time.
        let a = f.predict(2, pubs[58].0 - 200_000).unwrap();
        assert!(a.start_us <= pubs[58].0 && pubs[58].0 <= a.end_us);
        assert_eq!(a.freq, pubs[58].1);
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
        for p in &pubs[..CONFIRM_HITS as usize] {
            f.observe(5, p.0, p.1);
        }
        // Drone lands: four settled misses drop the session.
        let mut now = pubs[CONFIRM_HITS as usize - 1].0;
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
        // 100 publications later (64 s): the candidate expired
        // (TENTATIVE_LOST_US) and starts again.
        assert_eq!(f.observe(3, pubs[100].0, pubs[100].1), Some(Observed::Acquired));
        assert_eq!(f.candidate(3).unwrap().hits, 1);
    }

    /// Confirm session `id` from publications `pubs` (heard back to back).
    fn confirm(f: &mut Follower, id: u32, pubs: &[(u64, u32)]) {
        for (n, p) in pubs[..CONFIRM_HITS as usize].iter().enumerate() {
            let r = f.observe(id, p.0, p.1);
            if n + 1 == CONFIRM_HITS as usize {
                assert!(matches!(r, Some(Observed::Confirmed { .. })), "{r:?}");
            }
        }
        assert!(f.session(id).is_some());
    }

    #[test]
    fn a_new_session_is_not_visited_until_confirmed() {
        // One forged packet used to send the receiver to its predicted
        // channel from the next publication on.
        let pubs = drone(30, 1_000_000, 20, PERIOD_US);
        let mut f = Follower::new();
        assert_eq!(f.observe(1, pubs[0].0, pubs[0].1), Some(Observed::Acquired));
        assert!(f.next(pubs[0].0 + 10_000).is_none());
        assert!(matches!(f.observe(1, pubs[3].0, pubs[3].1), Some(Observed::Matched { .. })));
        assert!(f.next(pubs[3].0 + 10_000).is_none());
        assert_eq!((f.active(), f.tentative()), (0, 1));
        // The third consistent packet confirms; from then on it is visited.
        assert!(matches!(f.observe(1, pubs[9].0, pubs[9].1), Some(Observed::Confirmed { .. })));
        let a = f.next(pubs[9].0 + 10_000).unwrap();
        assert_eq!((a.at_us, a.freq), pubs[10]);
        assert_eq!((f.active(), f.tentative()), (1, 0));
    }

    #[test]
    fn inconsistent_packets_never_confirm() {
        // Three packets of one key ID on the wrong channels, or at the
        // wrong times, are not three consistent hits.
        let pubs = drone(50, 0, 30, PERIOD_US);
        let mut f = Follower::new();
        f.observe(2, pubs[0].0, pubs[0].1);
        // Right channel, 200 ms off its publication time.
        assert_eq!(f.observe(2, pubs[1].0 + 200_000, pubs[1].1), Some(Observed::Restarted));
        // Wrong channel at the right time.
        let wrong = CHANNELS.iter().copied().find(|&c| c != pubs[3].1 && f.candidate(2).unwrap().hypotheses().advance(2, c).is_empty()).unwrap();
        assert_eq!(f.observe(2, pubs[1].0 + 200_000 + 2 * PERIOD_US, wrong), Some(Observed::Restarted));
        assert!(f.session(2).is_none());
        // Random channels at publication times confirm only by luck: a
        // channel matches about one time in seven, so three in a row about
        // one in fifty; random times almost never.
        let mut x = 0x2545_f491u32;
        let mut confirmed = 0;
        let mut g = Follower::new();
        for p in 0..2000u64 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let t = p * 2 * PERIOD_US + (x >> 8) as u64 % PERIOD_US;
            if g.observe(9, t, CHANNELS[(x % 7) as usize]) == Some(Observed::Confirmed { hypotheses: 0 }) || g.session(9).is_some() {
                confirmed += 1;
                g = Follower::new();
            }
        }
        assert!(confirmed < 10, "{confirmed} random sessions confirmed");
    }

    #[test]
    fn a_fifth_key_id_cannot_evict_a_followed_session() {
        // The real drone, followed; then forged key IDs, one packet each.
        // Before: the fourth new ID evicted the session with the oldest
        // anchor, which was the real drone's.
        let real = drone(12, 0, 40, PERIOD_US);
        let mut f = Follower::new();
        confirm(&mut f, 0xD0, &real);
        let t = real[2].0;
        for k in 1..=40u32 {
            let r = f.observe(0x100 + k, t + k as u64 * 1000, CHANNELS[k as usize % 7]);
            assert_eq!(r, Some(Observed::Acquired));
        }
        assert!(f.session(0xD0).is_some());
        assert_eq!((f.active(), f.tentative()), (1, 40));
        assert_eq!(f.evicted, 0);
        // None of them is visited.
        let a = f.next(t + 50_000).unwrap();
        assert_eq!((a.id, a.at_us, a.freq), (0xD0, real[3].0, real[3].1));
        // A full candidate table gives up the entry whose last packet is
        // oldest, whatever its hits: never a followed session.
        let mut g = Follower::new();
        confirm(&mut g, 0xD0, &real);
        g.observe(0xC0, t + 1_000, real[3].1);
        for k in 1..CANDIDATES as u32 {
            g.observe(0x100 + k, t + 2_000 + k as u64 * 1000, CHANNELS[k as usize % 7]);
        }
        assert_eq!((g.tentative(), g.evicted), (CANDIDATES, 0));
        g.observe(0x999, t + 500_000, CHANNELS[0]);
        assert_eq!(g.evicted, 1);
        assert!(g.candidate(0xC0).is_none() && g.candidate(0x101).is_some() && g.candidate(0x999).is_some());
        assert!(g.session(0xD0).is_some());
    }

    #[test]
    fn a_contradicting_packet_is_ignored_by_a_followed_session() {
        // Before: one forged packet with the real drone's (cleartext) key
        // ID on another channel restarted the session from the forgery.
        let real = drone(200, 0, 40, PERIOD_US);
        let mut f = Follower::new();
        confirm(&mut f, 0xAB, &real);
        let before = *f.session(0xAB).unwrap();
        let t = real[3].0;
        let wrong = CHANNELS.iter().copied().find(|&c| c != real[3].1).unwrap();
        assert_eq!(f.observe(0xAB, t, wrong), Some(Observed::Contradicted));
        // Right channel, but 150 ms early: also a contradiction.
        assert_eq!(f.observe(0xAB, real[4].0 - 150_000, real[4].1), Some(Observed::Contradicted));
        // Beyond the longest gap (a confirmed session would have expired
        // first, but even so it is never restarted by a packet).
        let s = f.session(0xAB).unwrap();
        assert_eq!((s.cand, s.anchor_us, s.period_us, s.hits), (before.cand, before.anchor_us, before.period_us, before.hits));
        assert_eq!((s.contradictions, f.contradictions), (2, 2));
        // The real drone's next packets still match.
        assert!(matches!(f.observe(0xAB, real[4].0, real[4].1), Some(Observed::Matched { .. })));
        assert!(matches!(f.observe(0xAB, real[5].0, real[5].1), Some(Observed::Matched { .. })));
        // A followed session still ends by its own silence.
        assert!(f.next(real[5].0 + LOST_US + 1).is_none());
    }

    #[test]
    fn the_share_caps_follower_time_under_four_forged_sessions() {
        // Four forged schedule-perfect sessions, offset so that a window is
        // always coming. Simulate the node: visit when granted, else a plan
        // dwell (cut short by the next affordable window, at least 120 ms).
        let mut f = Follower::new();
        for (j, c0) in [3u8, 70, 140, 210].into_iter().enumerate() {
            let pubs = drone(c0, j as u64 * 160_000, CONFIRM_HITS as usize, PERIOD_US);
            confirm(&mut f, 0xF0 + j as u32, &pubs);
        }
        let (mut now, end) = (2_000_000u64, 62_000_000u64);
        let (mut follow_us, mut plan_us) = (0u64, 0u64);
        while now < end {
            // Keep the forged sessions alive with packets that fit (the
            // forger sends them; the node hears those it visits).
            if let Some(a) = f.next(now) {
                let depart = a.start_us.saturating_sub(30_000).max(now);
                if depart <= now + 120_000 && f.grant(now, a.end_us) {
                    let len = a.end_us - now;
                    follow_us += len;
                    now = a.end_us;
                    f.observe(a.id, a.at_us, a.freq);
                    continue;
                }
                let dwell = if f.affordable(&a, depart) { (depart - now).max(120_000) } else { 700_000 };
                plan_us += dwell;
                now += dwell;
            } else {
                plan_us += 700_000;
                now += 700_000;
            }
        }
        let total = follow_us + plan_us;
        let share = follow_us * 1000 / total;
        assert!(share <= SHARE_PERMILLE + 40, "follower took {share} per mille");
        assert!(share >= 300, "follower should still get its share: {share}");
        assert!(f.share.declined > 0 && f.share.granted > 0);
    }

    #[test]
    fn share_credit_accounting() {
        let mut s = Share::new();
        // Full credit pays for a 2 s visit at 1/2 cost, not a longer one.
        assert!(!s.affordable(10_000_000, 12_100_000));
        assert!(s.take(10_000_000, 12_000_000));
        assert!(!s.take(12_000_000, 12_100_000));
        // Plan time earns half of itself.
        assert!(s.affordable(12_200_000, 12_300_000));
        assert!(!s.affordable(12_200_000, 12_500_000));
        assert!(s.take(12_400_000, 12_600_000));
        assert_eq!((s.granted, s.declined, s.used_us), (2, 1, 2_200_000));
        // Credit is capped: a long idle stretch does not buy a long visit.
        assert!(!s.affordable(100_000_000, 102_500_000));
    }

    fn followed(f: &Follower, id: u32) -> bool {
        f.session(id).is_some()
    }

    /// A real drone heard by scanning every `every`-th publication, and a new
    /// forged key ID every `flood_us` on some O4 channel. Returns the
    /// publication at which the drone was followed.
    fn flooded(c0: u8, every: usize, flood_us: u64, pubs_n: usize) -> Option<usize> {
        let pubs = drone(c0, 3_000_000, pubs_n, PERIOD_US);
        let mut f = Follower::new();
        let mut next_new = 1_000_000u64;
        let mut new_id = 0x1000_0000u32;
        for (p, &(t, freq)) in pubs.iter().enumerate() {
            while next_new < t {
                f.observe(new_id, next_new, CHANNELS[new_id as usize % 7]);
                new_id += 1;
                next_new += flood_us;
            }
            if p % every == 0 {
                f.observe(5, t, freq);
                if followed(&f, 5) {
                    return Some(p);
                }
            }
        }
        None
    }

    #[test]
    fn a_flood_of_new_key_ids_cannot_keep_a_real_drone_from_being_followed() {
        // Bench 2026-11-18 (docs/BENCH.md step 4): with a new forged key ID
        // every 0.7 s the real drone's one-packet session was replaced long
        // before the plan heard it again (about every 8 s), so it never
        // reached CONFIRM_HITS. Same at the most valid decodes the node's
        // decode budget lets through (rawcap::budget, ~2.8 a second at the
        // fastest measured decode, 144 ms; tests/follow_forge.rs).
        for flood_us in [700_000u64, 350_000] {
            for (c0, every) in [(5u8, 13usize), (77, 11), (200, 17), (31, 23)] {
                // Heard every `every` publications (7-15 s apart).
                let p = flooded(c0, every, flood_us, 200);
                assert_eq!(p, Some(every * (CONFIRM_HITS as usize - 1)), "c0 {c0} every {every} flood {flood_us}");
            }
        }
    }

    #[test]
    fn a_real_drone_heard_by_scanning_is_confirmed_and_followed() {
        // Heard every 13th publication (the plan's chance on one channel),
        // then followed: once confirmed, the next window holds its packet.
        let pubs = drone(5, 0, 200, PERIOD_US);
        let mut f = Follower::new();
        let mut p = 0;
        while !f.session(4).is_some() {
            f.observe(4, pubs[p].0, pubs[p].1);
            p += 13;
        }
        assert_eq!(p, 13 * CONFIRM_HITS as usize);
        let last = p - 13;
        for q in last + 1..last + 40 {
            let a = f.next(pubs[q - 1].0 + 1_000).unwrap();
            assert_eq!(a.at_us, pubs[q].0);
            if a.freq == pubs[q].1 {
                assert!(matches!(f.observe(4, pubs[q].0, pubs[q].1), Some(Observed::Matched { .. })));
            } else {
                assert!(f.missed(&a));
            }
        }
        assert!(f.session(4).is_some());
    }
}
