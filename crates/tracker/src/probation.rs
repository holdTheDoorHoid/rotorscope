//! Probation: what a new entry must do before it can cost anyone else
//! anything (WP-09, docs/SPOOFING.md "Floods").
//!
//! Every input is an unauthenticated broadcast. One cheap transmitter can
//! send hundreds of well-formed drone (or aircraft, or control-link)
//! identities a second, each new, each claiming to be right on top of the
//! hub. Without a notion of standing, every one of them got a track, an alert
//! and a row at the top of the screen, and the table's eviction rule threw a
//! real drone out between two of its own frames.
//!
//! # The rule for drone tracks
//!
//! - **Unconfirmed** is how every track starts. It is shown, quietly, but
//!   never announced, never sent to TAK and never evaluated before a
//!   confirmed one.
//! - **Confirmed** once it has been heard again at least [`CONFIRM_SPAN_MS`]
//!   (1 s) after it was first heard ([`CONFIRM_MIN_FRAMES`] frames). Remote
//!   ID sends a Location at least once a second, so a real drone is
//!   confirmed, and announced, at most [`WORST_CASE_ANNOUNCE_MS`] (2 s)
//!   after its first frame; most send several frames a second and make it in
//!   one. A single forged frame never is, which is what the cheap flood
//!   sends: one frame per invented identity.
//! - **Unconfirmed tracks go first.** A new track never displaces a
//!   confirmed one that was heard in the last [`QUIET_EVICT_MS`] (15 s);
//!   real drones are heard every second, so in practice never. Unconfirmed
//!   tracks also leave sooner: after [`UNCONFIRMED_REMOVE_MS`] (30 s)
//!   without a frame, not ten minutes.
//! - **Admission when the table is full.** With a free slot (or a track not
//!   heard for a minute) a new identity gets its track at once, as before.
//!   When every slot is busy, a new identity is first only *remembered* in a
//!   small table of sightings ([`Candidates`], four per track slot, 12 bytes
//!   each): it gets a track (in place of the weakest unconfirmed one) when
//!   it is heard again at least [`SIGHTING_GAP_MS`] later. A single-frame
//!   fake never comes back, so it never costs a track. The candidates table
//!   forgets the least-heard, oldest sighting first, so a real drone's first
//!   frame survives a flood of up to `4 x N` new identities a second (256/s
//!   for 64 tracks) for the second it takes to send the next one.
//! - **Bounded rate.** Taking a busy table's slot costs a token from a
//!   bucket of [`PROMOTE_BURST`] (4) refilled [`PROMOTE_PER_S`] (4) a second,
//!   and the bar rises as the bucket drains: with `k` tokens left an
//!   identity needs `2 + (4 - k)` sightings. An attacker who repeats each fake
//!   once uses the first tokens, and then a real drone, still transmitting,
//!   outranks every fake that is not kept alive as long. Tracks are made at
//!   most at `N` per 30 s (free slots, refilled only as unconfirmed tracks
//!   expire) plus 4 a second.
//!
//! # The flood alarm
//!
//! [`FloodWatch`] counts new identities: frames whose drone matches no
//! track and no remembered sighting. When their rate over the last
//! [`FLOOD_WINDOW_S`] whole seconds reaches [`FLOOD_NEW_PER_S`] (20 a
//! second, 100 in 5 s), the tracker raises a hub threat
//! ([`link::ThreatKind::IdFlood`], "Flood of new drone IDs", `detail` = the
//! rate) once a second while it lasts; it ends 10 s after the rate falls
//! ([`crate::threats::THREAT_ACTIVE_MS`]). Real crowds: a race or a show
//! whose drones all fit the table never reaches it (each drone is new once);
//! a show of more drones than the table and the sightings table can hold
//! (about 5 x N, 320 for 64 tracks) does, and that is the truth too: the
//! hub can no longer follow them all.
//!
//! # Cost
//!
//! Per frame of an unknown identity: one pass over the sightings table
//! (`4 x N` entries, a 32-bit compare each), plus, only when it is admitted
//! into a busy table, one pass over the tracks. Per frame of a known one:
//! nothing more than before. Per [`crate::Tracker::expire`]: eight counters.

use link::{ThreatKind, ThreatSighting};

use crate::threats::Threat;

/// Frames a track needs before it is confirmed (with [`CONFIRM_SPAN_MS`]).
pub const CONFIRM_MIN_FRAMES: u32 = 2;
/// ...spread over at least this long, first to latest.
pub const CONFIRM_SPAN_MS: u64 = 1_000;
/// A drone sending at least once a second is announced within this long of
/// its first frame, on a quiet site: one second plus one frame interval.
pub const WORST_CASE_ANNOUNCE_MS: u64 = CONFIRM_SPAN_MS + 1_000;
/// The same during a single-frame flood of fewer than `4 x N` new IDs a
/// second: the first frame is remembered, the next one admits the track
/// and, being a second later, confirms it.
pub const WORST_CASE_FLOOD_ANNOUNCE_MS: u64 = WORST_CASE_ANNOUNCE_MS;
/// An unconfirmed track not heard for this long is removed.
pub const UNCONFIRMED_REMOVE_MS: u64 = 30_000;
/// A confirmed track may make room for a new one only when it has not been
/// heard for this long (or has gone stale).
pub const QUIET_EVICT_MS: u64 = 15_000;
/// Sightings remembered per track slot while the table is full.
pub const CANDIDATES_PER_TRACK: usize = 4;
/// Two sightings closer than this count as one (a drone's beacon may come
/// ten times a second; standing is earned over time, not frames).
pub const SIGHTING_GAP_MS: u64 = 500;
/// Sightings an identity needs to take a busy table's slot, with a full
/// token bucket.
pub const PROMOTE_MIN_SIGHTINGS: u32 = 2;
/// Tokens in the bucket (busy-table admissions in a burst)...
pub const PROMOTE_BURST: u32 = 4;
/// ...refilled at this many a second.
pub const PROMOTE_PER_S: u32 = 4;
/// The flood alarm's window, whole seconds.
pub const FLOOD_WINDOW_S: u64 = 5;
/// New drone identities a second (averaged over the window) that raise it.
pub const FLOOD_NEW_PER_S: u32 = 20;
/// Unproven aircraft, still being reported, thrown out a second for newer
/// ones (averaged over the window): raise it for ADS-B. A real sky does not
/// do this (real aircraft are heard every second, so never quiet enough to
/// be replaced); a forged-address flood replaces up to about
/// `A / 2 / 1.5 s` (10 a second for 32 entries), so the line sits at half
/// of that.
pub const AIRCRAFT_FLOOD_PER_S: u32 = 5;
/// New control links a second (the table holds 8), averaged: raise it.
pub const CONTROL_FLOOD_PER_S: u32 = 2;

/// Which table a flood alarm is about (`detail >> 24`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FloodTable {
    Drones = 0,
    Aircraft = 1,
    ControlLinks = 2,
    /// One node discarding frames while the new drone IDs it does deliver
    /// never confirm ([`ShedWatch`]): `node` names it.
    NodeShed = 3,
}

impl FloodTable {
    pub const fn label(self) -> &'static str {
        match self {
            FloodTable::Drones => "Flood of new drone IDs",
            FloodTable::Aircraft => "Flood of new aircraft addresses",
            FloodTable::ControlLinks => "Flood of new control links",
            FloodTable::NodeShed => "Node shedding a flood of drone IDs",
        }
    }

    /// Short name for JSON (`"table"` of a threat line).
    pub const fn name(self) -> &'static str {
        match self {
            FloodTable::Drones => "drones",
            FloodTable::Aircraft => "aircraft",
            FloodTable::ControlLinks => "control",
            FloodTable::NodeShed => "node",
        }
    }

    pub const fn of_detail(detail: u32) -> FloodTable {
        match detail >> 24 {
            1 => FloodTable::Aircraft,
            2 => FloodTable::ControlLinks,
            3 => FloodTable::NodeShed,
            _ => FloodTable::Drones,
        }
    }
}

/// The table an [`ThreatKind::IdFlood`] threat is about (`None` for other
/// kinds).
pub fn flood_table(t: &Threat) -> Option<FloodTable> {
    (t.kind == ThreatKind::IdFlood).then(|| FloodTable::of_detail(t.detail))
}

/// New identities a second the latest report of an `IdFlood` saw (for
/// [`FloodTable::NodeShed`]: new drone IDs a second from that node that
/// never confirmed).
pub fn flood_rate(t: &Threat) -> u32 {
    if FloodTable::of_detail(t.detail) == FloodTable::NodeShed {
        return (t.detail >> 16) & 0xff;
    }
    t.detail & 0xffff
}

/// Frames a second the node discarded, for a [`FloodTable::NodeShed`]
/// alarm (`None` for any other threat).
pub fn shed_rate(t: &Threat) -> Option<u32> {
    (flood_table(t) == Some(FloodTable::NodeShed)).then_some(t.detail & 0xffff)
}

/// Plain text for the evidence log and console.
pub fn describe(t: &Threat) -> heapless::String<128> {
    use core::fmt::Write;
    let mut s = heapless::String::new();
    let table = FloodTable::of_detail(t.detail);
    if table == FloodTable::NodeShed {
        let _ = write!(
            s,
            "{} (node {}): {} frames a second discarded, {} new drone IDs a second never confirmed, over {} s",
            table.label(),
            t.node,
            t.detail & 0xffff,
            flood_rate(t),
            SHED_WINDOW_S,
        );
        return s;
    }
    let _ = write!(
        s,
        "{}: {} new a second over {} s (limit {}); unconfirmed ones are kept out of alerts",
        table.label(),
        flood_rate(t),
        FLOOD_WINDOW_S,
        match table {
            FloodTable::Drones => FLOOD_NEW_PER_S,
            FloodTable::Aircraft => AIRCRAFT_FLOOD_PER_S,
            FloodTable::ControlLinks => CONTROL_FLOOD_PER_S,
            FloodTable::NodeShed => SHED_NEW_PER_S,
        },
    );
    s
}

/// FNV-1a, 32-bit: the key a sighting is remembered by.
pub(crate) fn hash(parts: &[&[u8]]) -> u32 {
    let mut h = 0x811c_9dc5u32;
    for p in parts {
        for b in *p {
            h = (h ^ u32::from(*b)).wrapping_mul(0x0100_0193);
        }
        h = (h ^ 0xff).wrapping_mul(0x0100_0193);
    }
    // 0 marks a free entry.
    h.max(1)
}

/// One remembered sighting: 12 bytes.
#[derive(Debug, Clone, Copy, Default)]
struct Cand {
    key: u32,
    /// Hub ms (low 32 bits) of the latest counted sighting.
    last: u32,
    /// Sightings (top 8 bits) and ms from the first to `last` (low 24).
    count_span: u32,
}

impl Cand {
    fn count(&self) -> u32 {
        self.count_span >> 24
    }
    fn span(&self) -> u32 {
        self.count_span & 0x00ff_ffff
    }
}

/// What [`Candidates::sight`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sighting {
    /// Sightings so far, this one included.
    pub count: u32,
    /// When it was first heard (hub ms).
    pub first_ms: u64,
    /// Not remembered before: a new identity.
    pub new: bool,
}

/// Identities heard while the table was full, with how often and since
/// when; `CANDIDATES_PER_TRACK x N` of them. Plus the admission bucket.
#[derive(Debug, Clone)]
pub struct Candidates<const N: usize> {
    c: [[Cand; CANDIDATES_PER_TRACK]; N],
    /// Milli-tokens, and the hub time they were counted at.
    bucket: u32,
    bucket_ms: u64,
}

impl<const N: usize> Default for Candidates<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Candidates<N> {
    pub const fn new() -> Self {
        Self {
            c: [[Cand { key: 0, last: 0, count_span: 0 }; CANDIDATES_PER_TRACK]; N],
            bucket: PROMOTE_BURST * 1000,
            bucket_ms: 0,
        }
    }

    fn all(&mut self) -> &mut [Cand] {
        self.c.as_flattened_mut()
    }

    /// Record one frame of identity `key`. One pass: finds it, or the entry
    /// to give up for it (free, else fewest sightings, else oldest).
    pub fn sight(&mut self, key: u32, now_ms: u64) -> Sighting {
        let now = now_ms as u32;
        let mut victim = 0usize;
        let mut worst = (u32::MAX, 0u32);
        let all = self.all();
        for (i, c) in all.iter_mut().enumerate() {
            if c.count() > 0 && c.key == key {
                let since = now.wrapping_sub(c.last);
                if u64::from(since) >= SIGHTING_GAP_MS && c.count() < 255 {
                    let span = (c.span() + since).min(0x00ff_ffff);
                    c.count_span = ((c.count() + 1) << 24) | span;
                    c.last = now;
                }
                return Sighting {
                    count: c.count(),
                    first_ms: now_ms.saturating_sub(u64::from(c.span() + now.wrapping_sub(c.last))),
                    new: false,
                };
            }
            // Free first (count 0), then fewest sightings, then oldest.
            let rank = (c.count(), now.wrapping_sub(c.last));
            if rank.0 < worst.0 || (rank.0 == worst.0 && rank.1 > worst.1) {
                worst = rank;
                victim = i;
            }
        }
        if let Some(c) = all.get_mut(victim) {
            *c = Cand { key, last: now, count_span: 1 << 24 };
        }
        Sighting { count: 1, first_ms: now_ms, new: true }
    }

    /// Forget `key` (it has a track now).
    pub fn forget(&mut self, key: u32) {
        for c in self.all() {
            if c.key == key {
                *c = Cand::default();
            }
        }
    }

    fn refill(&mut self, now_ms: u64) {
        let dt = now_ms.saturating_sub(self.bucket_ms);
        let add = dt.saturating_mul(u64::from(PROMOTE_PER_S)).min(u64::from(u32::MAX)) as u32;
        self.bucket = self.bucket.saturating_add(add).min(PROMOTE_BURST * 1000);
        self.bucket_ms = self.bucket_ms.max(now_ms);
    }

    /// Sightings an identity needs right now to take a busy table's slot
    /// (`None`: no token left at all).
    pub fn bar(&mut self, now_ms: u64) -> Option<u32> {
        self.refill(now_ms);
        let whole = self.bucket / 1000;
        (whole > 0).then(|| PROMOTE_MIN_SIGHTINGS + (PROMOTE_BURST - whole))
    }

    /// Spend one token (an identity took a busy table's slot).
    pub fn spend(&mut self) {
        self.bucket = self.bucket.saturating_sub(1000);
    }
}

/// Counts new identities per second over the last few seconds and decides
/// when that is a flood.
#[derive(Debug, Clone, Default)]
pub struct FloodWatch {
    /// New identities in each of the last eight seconds (by `second % 8`).
    per_s: [u16; 8],
    /// The second `per_s` was last advanced to.
    second: u64,
    /// The second the alarm was last raised, and since when it has held.
    raised_s: Option<u64>,
    since_ms: Option<u64>,
    /// New identities refused a track (counted since boot).
    pub refused: u32,
}

/// A flood as the phone page shows it (`"flood"` in `/api/state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct FloodStatus {
    /// New drone identities a second, over the last [`FLOOD_WINDOW_S`] s.
    pub new_per_s: u32,
    /// New identities refused a track since boot (the table was full and
    /// they had not earned a slot).
    pub refused: u32,
    /// Hub ms the alarm was first raised.
    pub since_ms: u64,
}

impl FloodWatch {
    pub const fn new() -> Self {
        Self { per_s: [0; 8], second: 0, raised_s: None, since_ms: None, refused: 0 }
    }

    fn advance(&mut self, now_ms: u64) {
        let s = now_ms / 1000;
        if s <= self.second {
            return;
        }
        for k in (self.second + 1)..=s.min(self.second + 8) {
            self.per_s[(k % 8) as usize] = 0;
        }
        self.second = s;
    }

    /// One new identity.
    pub fn note(&mut self, now_ms: u64) {
        self.advance(now_ms);
        let c = &mut self.per_s[(self.second % 8) as usize];
        *c = c.saturating_add(1);
    }

    /// New identities a second over the last [`FLOOD_WINDOW_S`] whole
    /// seconds (the current, partial one left out).
    pub fn rate(&mut self, now_ms: u64) -> u32 {
        self.advance(now_ms);
        let sum: u32 = (1..=FLOOD_WINDOW_S)
            .map(|k| u32::from(self.per_s[((self.second + 8 - k) % 8) as usize]))
            .sum();
        sum / FLOOD_WINDOW_S as u32
    }

    /// Called often (every frame is fine): the sighting to raise, at most
    /// once a second, while the rate is at or over `limit`.
    pub fn check(&mut self, now_ms: u64, limit: u32, table: FloodTable) -> Option<ThreatSighting> {
        let rate = self.rate(now_ms);
        let s = now_ms / 1000;
        let active = self
            .raised_s
            .is_some_and(|r| now_ms.saturating_sub(r * 1000) < crate::threats::THREAT_ACTIVE_MS);
        if !active {
            self.since_ms = None;
        }
        if rate < limit || self.raised_s == Some(s) {
            return None;
        }
        self.raised_s = Some(s);
        self.since_ms.get_or_insert(now_ms);
        Some(ThreatSighting {
            kind: ThreatKind::IdFlood,
            channel: 0,
            freq: 0,
            width_khz: 0,
            level: 0,
            count: rate.saturating_mul(FLOOD_WINDOW_S as u32),
            window_ms: (FLOOD_WINDOW_S * 1000) as u32,
            source: [0; 6],
            target: [0; 6],
            detail: (table as u32) << 24 | rate.min(0xffff),
            ssid: heapless::String::new(),
            uptime_us: 0,
        })
    }

    /// The flood while its alarm is active.
    pub fn status(&self, now_ms: u64) -> Option<FloodStatus> {
        let r = self.raised_s?;
        if now_ms.saturating_sub(r * 1000) >= crate::threats::THREAT_ACTIVE_MS {
            return None;
        }
        let sum: u32 = (1..=FLOOD_WINDOW_S)
            .map(|k| u32::from(self.per_s[((self.second + 8 - k) % 8) as usize]))
            .sum();
        Some(FloodStatus {
            new_per_s: sum / FLOOD_WINDOW_S as u32,
            refused: self.refused,
            since_ms: self.since_ms.unwrap_or(r * 1000),
        })
    }
}

// ------------------------------------------------------- one node shedding --

/// The node-shed alarm's window, whole seconds (drops: the latest seconds;
/// new and confirmed IDs: the same length, [`SHED_LAG_S`] earlier).
pub const SHED_WINDOW_S: u64 = 5;
/// How long a new ID is given to confirm before it counts as unconfirmed:
/// a drone sending once a second is confirmed by its frame two seconds after
/// its first ([`WORST_CASE_ANNOUNCE_MS`]), plus a second of slack.
pub const SHED_LAG_S: u64 = WORST_CASE_ANNOUNCE_MS / 1000 + 1;
/// Frames a second one node discards (its `Stats.dropped`, averaged over
/// [`SHED_WINDOW_S`]) that count as shedding: one whole new-source Remote ID
/// budget of node-wifi (`wifiwatch::shed`, 8 a second) thrown away.
pub const SHED_DROP_PER_S: u32 = 8;
/// New drone IDs a second from that node that did not confirm within
/// [`SHED_LAG_S`] (averaged): half of what node-wifi lets through from new
/// sources (8 a second).
pub const SHED_NEW_PER_S: u32 = 4;
/// A flood that one node sheds while still delivering at least
/// [`SHED_NEW_PER_S`] new IDs a second is raised within this long of its
/// start: the lagged window filled, plus up to a second of alignment.
pub const SHED_RAISE_MS: u64 = (SHED_WINDOW_S + SHED_LAG_S + 2) * 1000;
/// Nodes followed at once (the hub's node tables hold 16).
pub const SHED_NODES: usize = 16;
/// Seconds of history per node: the lagged window plus the current second.
const SHED_RING: usize = (SHED_WINDOW_S + SHED_LAG_S + 2) as usize;
/// A Stats report this long after the previous one starts again rather
/// than spread a long gap's drops over one second.
const SHED_MAX_GAP_US: u64 = SHED_WINDOW_S * 1_000_000;

#[derive(Debug, Clone, Copy, Default)]
struct NodeShed {
    node: u8,
    used: bool,
    /// The previous Stats: node uptime (ms, low 32 bits) and drop total.
    based: bool,
    up_ms: u32,
    dropped: u32,
    /// The second the rings were last advanced to (hub seconds).
    second: u32,
    /// Hub second the latest check ran (once a second).
    checked: u32,
    /// Per hub second (`second % SHED_RING`): new drone IDs this node
    /// delivered, how many of those (by first-heard second) have confirmed,
    /// and frames it said it discarded.
    new: [u8; SHED_RING],
    conf: [u8; SHED_RING],
    drop: [u16; SHED_RING],
}

impl NodeShed {
    fn advance(&mut self, s: u32) {
        if s <= self.second {
            return;
        }
        for k in (self.second + 1)..=s.min(self.second + SHED_RING as u32) {
            let i = k as usize % SHED_RING;
            self.new[i] = 0;
            self.conf[i] = 0;
            self.drop[i] = 0;
        }
        self.second = s;
    }

    /// Sums over `from..to` (hub seconds, `to` exclusive).
    fn sum(&self, from: u32, to: u32, f: impl Fn(&Self, usize) -> u32) -> u32 {
        (from..to).map(|k| f(self, k as usize % SHED_RING)).sum()
    }
}

/// One node discarding traffic at a flood-like rate (docs/SPOOFING.md
/// "Floods"). A Wi-Fi node lets at most 8 Remote ID frames a second from
/// new sources through (`wifiwatch::shed`) and sheds the rest, so a flood
/// heard by one node reaches the hub as a trickle of new, never-confirmed
/// IDs well under [`FLOOD_NEW_PER_S`]. What does cross the link: the node's
/// drop total (once a second) and the IDs it delivers. The node is shedding
/// a flood when, over [`SHED_WINDOW_S`]:
///
/// - it discarded at least [`SHED_DROP_PER_S`] frames a second, and
/// - of the new drone IDs it delivered (counted [`SHED_LAG_S`] back, so each
///   had time to confirm) at least [`SHED_NEW_PER_S`] a second never
///   confirmed.
///
/// A busy venue (drops, no new IDs), a booting node (its first Stats is a
/// baseline, a reboot starts again) and a crowd of real drones (new IDs
/// that confirm) do not meet both.
#[derive(Debug, Clone)]
pub struct ShedWatch {
    v: [NodeShed; SHED_NODES],
}

impl Default for ShedWatch {
    fn default() -> Self {
        Self::new()
    }
}

impl ShedWatch {
    pub const fn new() -> Self {
        const E: NodeShed = NodeShed {
            node: 0,
            used: false,
            based: false,
            up_ms: 0,
            dropped: 0,
            second: 0,
            checked: 0,
            new: [0; SHED_RING],
            conf: [0; SHED_RING],
            drop: [0; SHED_RING],
        };
        Self { v: [E; SHED_NODES] }
    }

    /// The entry for `node`, made (in place of the one not touched
    /// longest) if new. `None` for the unattributed node number.
    fn entry(&mut self, node: u8, s: u32) -> Option<&mut NodeShed> {
        if node == crate::threats::NODE_ANY {
            return None;
        }
        let i = match self.v.iter().position(|e| e.used && e.node == node) {
            Some(i) => i,
            None => {
                let i = self
                    .v
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, e)| (e.used, e.second))
                    .map(|(i, _)| i)?;
                self.v[i] = NodeShed { node, used: true, second: s, checked: s, ..NodeShed::default() };
                i
            }
        };
        let e = &mut self.v[i];
        e.advance(s);
        Some(e)
    }

    /// `node` delivered a drone ID the hub had not heard.
    pub fn new_id(&mut self, node: u8, now_ms: u64) {
        let s = (now_ms / 1000) as u32;
        if let Some(e) = self.entry(node, s) {
            let c = &mut e.new[s as usize % SHED_RING];
            *c = c.saturating_add(1);
        }
    }

    /// A track first delivered by `node` at `first_ms` has confirmed.
    pub fn confirmed(&mut self, node: u8, first_ms: u64, now_ms: u64) {
        let s = (now_ms / 1000) as u32;
        let first = (first_ms / 1000) as u32;
        if node == crate::threats::NODE_ANY || s.saturating_sub(first) >= SHED_RING as u32 {
            return;
        }
        if let Some(e) = self.v.iter_mut().find(|e| e.used && e.node == node) {
            e.advance(s);
            let c = &mut e.conf[first as usize % SHED_RING];
            *c = c.saturating_add(1);
        }
    }

    /// One Stats report from `node`: its uptime and drop total.
    pub fn stats(&mut self, node: u8, uptime_us: u64, dropped: u32, now_ms: u64) {
        let s = (now_ms / 1000) as u32;
        let Some(e) = self.entry(node, s) else { return };
        let up_ms = (uptime_us / 1000) as u32;
        let gap_us = u64::from(up_ms.wrapping_sub(e.up_ms)) * 1000;
        // A first report, a reboot (uptime back), a counter that went back
        // or a long gap: a new baseline, nothing counted.
        if e.based && up_ms > e.up_ms && dropped >= e.dropped && gap_us <= SHED_MAX_GAP_US {
            let d = (dropped - e.dropped).min(u32::from(u16::MAX)) as u16;
            let c = &mut e.drop[s as usize % SHED_RING];
            *c = c.saturating_add(d);
        }
        e.based = true;
        e.up_ms = up_ms;
        e.dropped = dropped;
    }

    /// Called often: for each node at most once a second, `(node, sighting)`
    /// to raise while it sheds a flood.
    pub fn check(&mut self, now_ms: u64, mut raise: impl FnMut(u8, ThreatSighting)) {
        let s = (now_ms / 1000) as u32;
        let w = SHED_WINDOW_S as u32;
        let lag = SHED_LAG_S as u32;
        for e in self.v.iter_mut().filter(|e| e.used && e.checked != s) {
            e.advance(s);
            e.checked = s;
            let dropped = e.sum(s.saturating_sub(w), s, |e, i| u32::from(e.drop[i]));
            let (from, to) = (s.saturating_sub(w + lag), s.saturating_sub(lag));
            let new = e.sum(from, to, |e, i| u32::from(e.new[i]));
            let conf = e.sum(from, to, |e, i| u32::from(e.conf[i]));
            let unconfirmed = new.saturating_sub(conf);
            if dropped < SHED_DROP_PER_S * w || unconfirmed < SHED_NEW_PER_S * w {
                continue;
            }
            let (drop_s, new_s) = (dropped / w, unconfirmed / w);
            raise(
                e.node,
                ThreatSighting {
                    kind: ThreatKind::IdFlood,
                    channel: 0,
                    freq: 0,
                    width_khz: 0,
                    level: 0,
                    count: dropped,
                    window_ms: w * 1000,
                    source: [0; 6],
                    target: [0; 6],
                    detail: (FloodTable::NodeShed as u32) << 24 | new_s.min(0xff) << 16 | drop_s.min(0xffff),
                    ssid: heapless::String::new(),
                    uptime_us: 0,
                },
            );
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sightings_count_over_time_and_forget_the_least_heard_first() {
        let mut c: Candidates<2> = Candidates::new();
        assert!(c.sight(7, 1_000).new);
        // Too soon: the same sighting.
        assert_eq!(c.sight(7, 1_100).count, 1);
        let s = c.sight(7, 1_600);
        assert_eq!((s.count, s.first_ms, s.new), (2, 1_000, false));
        // Fill the other seven entries, then one more: a one-sighting entry
        // goes, never key 7.
        for k in 100..108 {
            c.sight(k, 2_000);
        }
        assert_eq!(c.sight(7, 2_200).count, 3);
        c.forget(7);
        assert!(c.sight(7, 3_000).new);
    }

    #[test]
    fn the_bar_rises_as_the_bucket_drains() {
        let mut c: Candidates<1> = Candidates::new();
        assert_eq!(c.bar(0), Some(2));
        c.spend();
        assert_eq!(c.bar(0), Some(3));
        c.spend();
        c.spend();
        assert_eq!(c.bar(0), Some(5));
        c.spend();
        assert_eq!(c.bar(0), None);
        // 4 a second.
        assert_eq!(c.bar(250), Some(5));
        assert_eq!(c.bar(1_000), Some(2));
        assert_eq!(c.bar(60_000), Some(2), "never more than the burst");
    }

    #[test]
    fn flood_watch_rate_and_alarm() {
        let mut f = FloodWatch::new();
        for ms in (0..6_000).step_by(10) {
            f.note(ms);
        }
        assert_eq!(f.rate(6_000), 100);
        let s = f.check(6_000, FLOOD_NEW_PER_S, FloodTable::Drones).unwrap();
        assert_eq!(s.detail, 100);
        assert!(f.check(6_500, FLOOD_NEW_PER_S, FloodTable::Drones).is_none(), "once a second");
        assert!(f.check(7_000, FLOOD_NEW_PER_S, FloodTable::Drones).is_some());
        assert_eq!(f.status(7_000).unwrap().since_ms, 6_000);
        // Quiet again: the rate falls, the alarm ends.
        assert_eq!(f.rate(20_000), 0);
        assert!(f.check(20_000, FLOOD_NEW_PER_S, FloodTable::Drones).is_none());
        assert!(f.status(20_000).is_none());
    }

    #[test]
    fn hash_never_zero_and_parts_matter() {
        assert_ne!(hash(&[b"ab", b"c"]), hash(&[b"a", b"bc"]));
        assert_ne!(hash(&[]), 0);
    }
}
