//! The secret-free gateway's relay logic (docs/LINK-KEYS-DESIGN.md sections 5,
//! 6.3 and 14): pure, host-tested, and **keyless**. Nothing in this module
//! takes, holds or makes a key: the gateway cannot verify or forge a tag. It
//! looks at packets only through [`v3::inspect`] (version, kind, size).
//!
//! # Up: radio to hub ([`Uplink`])
//!
//! Every packet heard goes through [`Uplink::offer`], which decides with
//! non-secret filters only, in this order:
//!
//! 1. Not ours (no `"RS"` magic), or a hub-to-node kind (Sync, Command,
//!    Beacon: another kit's gateway, or our own hub's packets heard back):
//!    counted as foreign, not an attack.
//! 2. Version 2 (the retired kit-key firmware): refused as
//!    [`Refuse::OldFirmware`]; how an un-reflashed board shows up.
//! 3. Ours but malformed (bad size for its kind, unknown version or kind):
//!    refused as [`Refuse::Malformed`].
//! 4. Version 3 `Unprovisioned` announcement (unsealed, anyone may send one):
//!    forwarded, at most one per [`UNPROVISIONED_EVERY_MS`] in total, from any
//!    address, in the shared lane.
//! 5. Version 3 `Frames`: the sender must be in the roster the hub pushed
//!    ([`Uplink::set_roster`]); before the first roster arrives nobody is
//!    ([`Refuse::NotInRoster`]).
//! 6. Version 1 (open kit, unsealed) `Frames`: forwarded from a roster
//!    address in its own lane, from anyone else in the shared lane (an open
//!    kit's hub has no roster to push; a keyed hub counts them as
//!    unauthenticated, which is how a node left open in a sealed kit shows up).
//! 7. Byte-identical to the previous packet from the same address (a radio
//!    retry): dropped ([`Drop::Duplicate`]).
//! 8. The lane's budget, [`PER_MAC_PACKETS_PER_S`] packets and
//!    [`PER_MAC_BYTES_PER_S`] bytes a second (token buckets holding one
//!    second): over it, refused as [`Refuse::OverBudget`].
//! 9. Room in the queue (below): else dropped ([`Drop::QueueFull`], reported
//!    to the hub as a UART drop).
//!
//! Lanes: one per roster address (in roster order) and one shared lane
//! ([`SHARED`]) for everything else that may pass. Queued packets sit in a
//! pool of `SLOTS` packet buffers; a lane holds at most [`PER_LANE`] of them,
//! and the last [`RESERVE`] free buffers go only to a lane that holds none,
//! so flooded lanes cannot starve a quiet one of its next packet.
//!
//! [`Uplink::pop`] serves the lanes **round-robin**, one packet per lane per
//! turn, under a **total cap** of [`TOTAL_BYTES_PER_S`] (counting
//! [`UP_OVERHEAD`] per packet for the message around it), and drops a queued
//! packet older than [`UP_MAX_HOLD_MS`] (the hub would refuse it as late).
//!
//! Every refusal is counted per address and reason in a [`Refusals`] log,
//! which the gateway drains once a second into `GwUp::Refused` messages.
//!
//! # Down: hub to radio ([`TxQueue`], [`BeaconSlot`])
//!
//! A beacon from the hub is broadcast once, as it arrived; a newer one
//! replaces one not yet sent, and none is ever repeated. A `Transmit` is sent
//! unicast while the node is on channel: at once, again whenever the node is
//! heard ([`TxQueue::heard`], at most every [`MIN_RETRY_MS`]), and blind every
//! [`BLIND_RETRY_MS`] (a 2.4 GHz node sits on the gateway channel most of
//! the time), until the radio acknowledges it or `give_up_ms` from its
//! arrival. Urgent ones (time syncs) go before anything else and are dropped
//! [`URGENT_MAX_AGE_MS`] after arrival whatever `give_up_ms` says. `Cancel`
//! drops whatever is queued for a node. Each one ends in exactly one
//! [`TxDone`].

use crate::v3::{self, Kind, Reject};
use crate::{HEADER, MAX_PACKET};

pub type Mac = [u8; 6];

/// Per-roster-address budget, packets a second (design section 12).
pub const PER_MAC_PACKETS_PER_S: u32 = 40;
/// Per-roster-address budget, bytes a second.
pub const PER_MAC_BYTES_PER_S: u32 = 16 * 1024;
/// Everything to the hub together, bytes a second (section 14: about 50 KB/s,
/// a little over half the 921,600-baud line).
pub const TOTAL_BYTES_PER_S: u32 = 50 * 1024;
/// The total cap's bucket holds this much: a quarter of a second, so a
/// burst cannot fill the line for long. At least one full packet.
pub const TOTAL_BURST: u32 = TOTAL_BYTES_PER_S / 4;
/// What a `GwUp::Packet` adds to its data on the wire (fields, COBS,
/// delimiter), rounded up; counted against the total cap.
pub const UP_OVERHEAD: usize = 24;
/// Unprovisioned announcements forwarded at most this often, in total.
pub const UNPROVISIONED_EVERY_MS: u64 = 1000;
/// A queued packet older than this is dropped (the hub refuses a batch
/// whose stamp is 2 s old; this keeps a backlog well inside that).
pub const UP_MAX_HOLD_MS: u64 = 1000;
/// Roster addresses (the kit's slots).
pub const ROSTER_MAX: usize = 16;
/// Lanes: one per roster address plus the shared one.
pub const LANES: usize = ROSTER_MAX + 1;
/// The shared lane's index.
pub const SHARED: usize = ROSTER_MAX;
/// Most queued packets one lane may hold.
pub const PER_LANE: usize = 4;
/// Free buffers kept for lanes that hold none.
pub const RESERVE: usize = 8;

/// Urgent transmits (time syncs) are dropped this long after arrival.
pub const URGENT_MAX_AGE_MS: u64 = 300;
/// Retry a transmit when its node is heard, at most this often.
pub const MIN_RETRY_MS: u64 = 20;
/// Retry a transmit blind this often while it lasts.
pub const BLIND_RETRY_MS: u64 = 50;

/// Why the gateway refused packets: the same four reasons, in the same
/// order, as `link::gw::RefuseReason` (this crate does not depend on `link`;
/// a test holds the two equal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refuse {
    NotInRoster,
    OverBudget,
    OldFirmware,
    Malformed,
}

impl Refuse {
    pub const COUNT: usize = 4;
    pub const ALL: [Refuse; Refuse::COUNT] = [Refuse::NotInRoster, Refuse::OverBudget, Refuse::OldFirmware, Refuse::Malformed];

    pub const fn index(self) -> usize {
        self as usize
    }

    pub const fn name(self) -> &'static str {
        match self {
            Refuse::NotInRoster => "not_in_roster",
            Refuse::OverBudget => "over_budget",
            Refuse::OldFirmware => "old_firmware",
            Refuse::Malformed => "malformed",
        }
    }
}

/// What [`Uplink::offer`] did with a packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Queued for the hub.
    Queued,
    Dropped(Drop),
}

/// Why a packet did not go to the hub.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drop {
    /// Not ours, or not a node-to-hub packet. Not an attack.
    Foreign,
    /// Refused, counted per address and reason, reported to the hub.
    Refused(Refuse),
    /// Byte-identical to the previous packet from the same address.
    Duplicate,
    /// An Unprovisioned announcement within a second of the last one.
    UnprovisionedRate,
    /// No buffer for it (the UART to the hub is behind).
    QueueFull,
}

// ------------------------------------------------------------ budgets --

/// Token bucket: `RATE` units a second, holding up to `BURST` units, full
/// at time 0. It keeps what has been spent, in thousandths of a unit (a rate
/// in units a second then refills exactly `RATE` a millisecond), so a fresh
/// bucket is all zero bytes and a `static` holding many of them needs no
/// initialised data.
#[derive(Debug, Clone, Copy, Default)]
pub struct Bucket<const RATE: u32, const BURST: u32> {
    spent: u64,
    at_ms: u64,
}

impl<const RATE: u32, const BURST: u32> Bucket<RATE, BURST> {
    const CAP: u64 = BURST as u64 * 1000;

    pub const fn new() -> Self {
        Bucket { spent: 0, at_ms: 0 }
    }

    fn refill(&mut self, now_ms: u64) {
        let dt = now_ms.saturating_sub(self.at_ms);
        self.spent = self.spent.saturating_sub(dt.saturating_mul(u64::from(RATE)));
        self.at_ms = self.at_ms.max(now_ms);
    }

    /// What it holds now, in thousandths.
    fn level(&mut self, now_ms: u64) -> u64 {
        self.refill(now_ms);
        Self::CAP.saturating_sub(self.spent)
    }

    /// Room for `n` units now?
    pub fn has(&mut self, n: u32, now_ms: u64) -> bool {
        self.level(now_ms) >= u64::from(n) * 1000
    }

    /// Spend `n` units; false (and nothing spent) if there are not enough.
    pub fn take(&mut self, n: u32, now_ms: u64) -> bool {
        if !self.has(n, now_ms) {
            return false;
        }
        self.spent += u64::from(n) * 1000;
        true
    }

    /// Milliseconds until it holds `n` units (0: now).
    pub fn wait_ms(&mut self, n: u32, now_ms: u64) -> u64 {
        (u64::from(n) * 1000).saturating_sub(self.level(now_ms)).div_ceil(u64::from(RATE))
    }
}

/// One lane's budget: packets and bytes, both must have room.
#[derive(Debug, Clone, Copy, Default)]
pub struct Budget {
    packets: Bucket<PER_MAC_PACKETS_PER_S, PER_MAC_PACKETS_PER_S>,
    bytes: Bucket<PER_MAC_BYTES_PER_S, PER_MAC_BYTES_PER_S>,
}

impl Budget {
    pub const fn new() -> Self {
        Budget { packets: Bucket::new(), bytes: Bucket::new() }
    }

    /// Spend one packet of `len` bytes, or nothing.
    pub fn take(&mut self, len: usize, now_ms: u64) -> bool {
        let len = len as u32;
        if !(self.packets.has(1, now_ms) && self.bytes.has(len, now_ms)) {
            return false;
        }
        self.packets.take(1, now_ms) && self.bytes.take(len, now_ms)
    }
}

/// FNV-1a over the length and the bytes: enough to tell a radio retry
/// (byte-identical) from a new packet. A sender cannot use a collision to
/// hide a genuine packet: it would have to know that packet, tag included,
/// before it was sent.
pub fn fingerprint(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in (data.len() as u32).to_le_bytes().iter().chain(data) {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

// ------------------------------------------------------------- refusals --

/// One address's refusals for one reason in the current window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refusal {
    /// All zero: many addresses, shared (the log was full).
    pub src: Mac,
    pub reason: Refuse,
    pub count: u32,
    /// Strongest seen.
    pub rssi: i8,
    pub window_ms: u32,
}

/// Refusals per address and reason, drained once a window. `N` entries;
/// past that, further addresses share one all-zero entry per reason. An
/// entry with `count` 0 is free (so a fresh log is all zero bytes).
pub struct Refusals<const N: usize> {
    entries: [(Refusal, u64); N],
    shared: [(Refusal, u64); Refuse::COUNT],
    /// Since boot, per reason (wrapping).
    pub totals: [u32; Refuse::COUNT],
}

impl<const N: usize> Refusals<N> {
    const FREE: (Refusal, u64) = (Refusal { src: [0; 6], reason: Refuse::NotInRoster, count: 0, rssi: 0, window_ms: 0 }, 0);

    pub const fn new() -> Self {
        Refusals { entries: [Self::FREE; N], shared: [Self::FREE; Refuse::COUNT], totals: [0; Refuse::COUNT] }
    }

    pub fn record(&mut self, src: Mac, reason: Refuse, rssi: i8, now_ms: u64) {
        let i = reason.index();
        self.totals[i] = self.totals[i].wrapping_add(1);
        let k = self.entries.iter().position(|(r, _)| r.count > 0 && r.src == src && r.reason == reason);
        let (e, who) = match k.or_else(|| self.entries.iter().position(|(r, _)| r.count == 0)) {
            Some(k) => (&mut self.entries[k], src),
            None => (&mut self.shared[i], [0; 6]),
        };
        if e.0.count == 0 {
            *e = (Refusal { src: who, reason, count: 0, rssi, window_ms: 0 }, now_ms);
        }
        e.0.count = e.0.count.saturating_add(1);
        e.0.rssi = e.0.rssi.max(rssi);
    }

    /// Hand every entry to `out` (with its window: from its first refusal
    /// to `now_ms`) and start afresh.
    pub fn drain(&mut self, now_ms: u64, mut out: impl FnMut(Refusal)) {
        for e in self.entries.iter_mut().chain(self.shared.iter_mut()) {
            if e.0.count > 0 {
                let (mut r, first) = core::mem::replace(e, Self::FREE);
                r.window_ms = now_ms.saturating_sub(first).min(u64::from(u32::MAX)) as u32;
                out(r);
            }
        }
    }
}

impl<const N: usize> Default for Refusals<N> {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------- up --

/// A packet waiting for the UART.
pub struct Queued {
    pub src: Mac,
    pub rssi: i8,
    /// When it was heard, in the caller's microsecond clock (for `held_us`).
    pub at_us: u64,
    at_ms: u64,
    len: u16,
    data: [u8; MAX_PACKET],
}

impl Queued {
    const EMPTY: Queued = Queued { src: [0; 6], rssi: 0, at_us: 0, at_ms: 0, len: 0, data: [0; MAX_PACKET] };

    pub fn data(&self) -> &[u8] {
        &self.data[..usize::from(self.len)]
    }
}

/// A lane's FIFO of buffer indexes.
#[derive(Debug, Clone, Copy)]
struct Fifo {
    at: [u8; PER_LANE],
    head: u8,
    len: u8,
}

impl Fifo {
    const EMPTY: Fifo = Fifo { at: [0; PER_LANE], head: 0, len: 0 };

    fn push(&mut self, slot: u8) -> bool {
        if usize::from(self.len) == PER_LANE {
            return false;
        }
        self.at[(usize::from(self.head) + usize::from(self.len)) % PER_LANE] = slot;
        self.len += 1;
        true
    }

    fn front(&self) -> Option<u8> {
        (self.len > 0).then(|| self.at[usize::from(self.head)])
    }

    fn pop(&mut self) -> Option<u8> {
        let s = self.front()?;
        self.head = ((usize::from(self.head) + 1) % PER_LANE) as u8;
        self.len -= 1;
        Some(s)
    }
}

#[derive(Debug, Clone, Copy)]
struct Lane {
    mac: Option<Mac>,
    budget: Budget,
    /// The last packet taken from this lane's address: (address, fingerprint).
    last: Option<(Mac, u64)>,
    queue: Fifo,
}

impl Lane {
    const EMPTY: Lane = Lane { mac: None, budget: Budget::new(), last: None, queue: Fifo::EMPTY };
}

/// Running counts of what [`Uplink`] did (since boot, wrapping).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UpCounts {
    pub queued: u32,
    pub sent: u32,
    pub foreign: u32,
    pub duplicates: u32,
    pub unprovisioned_limited: u32,
    /// No buffer when it arrived.
    pub queue_full: u32,
    /// Waited past [`UP_MAX_HOLD_MS`].
    pub stale: u32,
    /// Queued for an address a new roster no longer lists.
    pub unlisted: u32,
}

impl UpCounts {
    /// Everything lost on the way to the UART (`GwStats::uart_drops`).
    pub fn uart_drops(&self) -> u32 {
        self.queue_full.wrapping_add(self.stale).wrapping_add(self.unlisted)
    }
}

/// The up direction: roster, filters, budgets and the round-robin queue.
/// `SLOTS` packet buffers of [`MAX_PACKET`] bytes each.
pub struct Uplink<const SLOTS: usize> {
    roster: Option<([Mac; ROSTER_MAX], usize)>,
    lanes: [Lane; LANES],
    slots: [Queued; SLOTS],
    used: [bool; SLOTS],
    total: Bucket<TOTAL_BYTES_PER_S, TOTAL_BURST>,
    unprovisioned_at: Option<u64>,
    /// The next lane round-robin serves.
    turn: usize,
    pub refusals: Refusals<16>,
    pub counts: UpCounts,
}

impl<const SLOTS: usize> Uplink<SLOTS> {
    const OK: () = assert!(SLOTS <= 255 && SLOTS > RESERVE && TOTAL_BURST as usize >= MAX_PACKET + UP_OVERHEAD);

    pub const fn new() -> Self {
        let () = Self::OK;
        Uplink {
            roster: None,
            lanes: [Lane::EMPTY; LANES],
            slots: [Queued::EMPTY; SLOTS],
            used: [false; SLOTS],
            total: Bucket::new(),
            unprovisioned_at: None,
            turn: 0,
            refusals: Refusals::new(),
            counts: UpCounts { queued: 0, sent: 0, foreign: 0, duplicates: 0, unprovisioned_limited: 0, queue_full: 0, stale: 0, unlisted: 0 },
        }
    }

    /// The roster has arrived (or changed). Addresses that stay keep their
    /// budget, last packet and queue; a lane whose address left is emptied
    /// (its queued packets counted as `unlisted`). At most [`ROSTER_MAX`]
    /// are taken, duplicates once.
    pub fn set_roster(&mut self, macs: &[Mac]) {
        let mut list = [[0u8; 6]; ROSTER_MAX];
        let mut n = 0;
        for m in macs.iter().take(ROSTER_MAX) {
            if !list[..n].contains(m) {
                list[n] = *m;
                n += 1;
            }
        }
        let old = self.lanes;
        let mut kept = [false; ROSTER_MAX];
        let mut lanes = [Lane::EMPTY; LANES];
        for (i, m) in list[..n].iter().enumerate() {
            match old[..ROSTER_MAX].iter().position(|l| l.mac == Some(*m)) {
                Some(j) => {
                    lanes[i] = old[j];
                    kept[j] = true;
                }
                None => lanes[i].mac = Some(*m),
            }
        }
        lanes[SHARED] = old[SHARED];
        for (j, l) in old[..ROSTER_MAX].iter().enumerate() {
            if !kept[j] {
                let mut q = l.queue;
                while let Some(s) = q.pop() {
                    self.used[usize::from(s)] = false;
                    self.counts.unlisted = self.counts.unlisted.wrapping_add(1);
                }
            }
        }
        self.lanes = lanes;
        self.roster = Some((list, n));
        if self.turn >= LANES {
            self.turn = 0;
        }
    }

    /// Whether a roster has arrived yet.
    pub fn has_roster(&self) -> bool {
        self.roster.is_some()
    }

    /// The roster as the hub last sent it (empty before the first).
    pub fn roster(&self) -> &[Mac] {
        self.roster.as_ref().map_or(&[], |(list, n)| &list[..*n])
    }

    /// The roster lane of `mac`, if listed.
    pub fn roster_lane(&self, mac: &Mac) -> Option<usize> {
        let (list, n) = self.roster.as_ref()?;
        list[..*n].iter().position(|m| m == mac)
    }

    /// Packets queued, all lanes.
    pub fn queued(&self) -> usize {
        self.used.iter().filter(|u| **u).count()
    }

    /// Decide on one packet heard from `src` (see the module docs for the
    /// order of the filters). `now_ms` and `at_us` are the same instant in
    /// the caller's two clocks.
    pub fn offer(&mut self, src: Mac, rssi: i8, data: &[u8], now_ms: u64, at_us: u64) -> Outcome {
        let lane = match self.classify(src, data, now_ms) {
            Ok(l) => l,
            Err(d) => {
                match d {
                    Drop::Foreign => self.counts.foreign = self.counts.foreign.wrapping_add(1),
                    Drop::Refused(r) => self.refusals.record(src, r, rssi, now_ms),
                    Drop::UnprovisionedRate => self.counts.unprovisioned_limited = self.counts.unprovisioned_limited.wrapping_add(1),
                    _ => {}
                }
                return Outcome::Dropped(d);
            }
        };
        let fp = fingerprint(data);
        let l = &mut self.lanes[lane];
        if l.last == Some((src, fp)) {
            self.counts.duplicates = self.counts.duplicates.wrapping_add(1);
            return Outcome::Dropped(Drop::Duplicate);
        }
        if !l.budget.take(data.len(), now_ms) {
            self.refusals.record(src, Refuse::OverBudget, rssi, now_ms);
            return Outcome::Dropped(Drop::Refused(Refuse::OverBudget));
        }
        l.last = Some((src, fp));
        // A buffer: this lane under its share, and the reserve only for a
        // lane holding none.
        let free = self.used.iter().filter(|u| !**u).count();
        let holds = usize::from(self.lanes[lane].queue.len);
        let slot = if holds < PER_LANE && (free > RESERVE || (holds == 0 && free > 0)) { self.used.iter().position(|u| !*u) } else { None };
        let Some(s) = slot else {
            self.counts.queue_full = self.counts.queue_full.wrapping_add(1);
            return Outcome::Dropped(Drop::QueueFull);
        };
        let q = &mut self.slots[s];
        q.src = src;
        q.rssi = rssi;
        q.at_us = at_us;
        q.at_ms = now_ms;
        q.len = data.len() as u16;
        q.data[..data.len()].copy_from_slice(data);
        self.used[s] = true;
        let pushed = self.lanes[lane].queue.push(s as u8);
        debug_assert!(pushed);
        self.counts.queued = self.counts.queued.wrapping_add(1);
        Outcome::Queued
    }

    /// Filters 1-6: the lane a packet goes to, or why it goes nowhere.
    fn classify(&mut self, src: Mac, data: &[u8], now_ms: u64) -> Result<usize, Drop> {
        if data.len() > MAX_PACKET {
            return Err(if data.starts_with(&crate::MAGIC) { Drop::Refused(Refuse::Malformed) } else { Drop::Foreign });
        }
        match v3::inspect(data) {
            Ok(shape) => match shape.kind {
                Kind::Frames => self.roster_lane(&src).ok_or(Drop::Refused(Refuse::NotInRoster)),
                Kind::Unprovisioned => {
                    if self.unprovisioned_at.is_some_and(|t| now_ms.saturating_sub(t) < UNPROVISIONED_EVERY_MS) {
                        return Err(Drop::UnprovisionedRate);
                    }
                    self.unprovisioned_at = Some(now_ms);
                    Ok(SHARED)
                }
                // Hub-to-node kinds: never from a node.
                Kind::Sync | Kind::Command | Kind::Beacon => Err(Drop::Foreign),
            },
            Err(Reject::Foreign) => Err(Drop::Foreign),
            Err(Reject::OldFirmware) => Err(Drop::Refused(Refuse::OldFirmware)),
            Err(Reject::Unauthenticated) => match crate::parse(data) {
                Some(p) if p.kind == crate::Kind::Frames && data.len() > HEADER => {
                    Ok(self.roster_lane(&src).unwrap_or(SHARED))
                }
                Some(_) => Err(Drop::Foreign),
                None => Err(Drop::Refused(Refuse::Malformed)),
            },
            Err(_) => Err(Drop::Refused(Refuse::Malformed)),
        }
    }

    /// The next packet for the hub, round-robin over the lanes, if the
    /// total cap has room for it: `send` gets it (encode it there), and its
    /// buffer is free afterwards. Packets that waited past
    /// [`UP_MAX_HOLD_MS`] are dropped on the way. `None`: nothing to send,
    /// or the cap says wait.
    pub fn pop<R>(&mut self, now_ms: u64, send: impl FnOnce(&Queued) -> R) -> Option<R> {
        for k in 0..LANES {
            let lane = (self.turn + k) % LANES;
            // Drop what is too old at the front of this lane.
            while let Some(s) = self.lanes[lane].queue.front() {
                let q = &self.slots[usize::from(s)];
                if now_ms.saturating_sub(q.at_ms) <= UP_MAX_HOLD_MS {
                    break;
                }
                self.lanes[lane].queue.pop();
                self.used[usize::from(s)] = false;
                self.counts.stale = self.counts.stale.wrapping_add(1);
            }
            let Some(s) = self.lanes[lane].queue.front() else { continue };
            let cost = usize::from(self.slots[usize::from(s)].len) + UP_OVERHEAD;
            if !self.total.take(cost as u32, now_ms) {
                // Wait for the cap; this lane keeps its turn.
                self.turn = lane;
                return None;
            }
            self.lanes[lane].queue.pop();
            self.turn = (lane + 1) % LANES;
            let r = send(&self.slots[usize::from(s)]);
            self.used[usize::from(s)] = false;
            self.counts.sent = self.counts.sent.wrapping_add(1);
            return Some(r);
        }
        None
    }

    /// Milliseconds until the total cap has room for the packet waiting
    /// longest at the front of a lane, if any is waiting (0: now).
    pub fn wait_ms(&mut self, now_ms: u64) -> Option<u64> {
        let mut need: Option<u32> = None;
        for l in &self.lanes {
            if let Some(s) = l.queue.front() {
                let c = (usize::from(self.slots[usize::from(s)].len) + UP_OVERHEAD) as u32;
                need = Some(need.map_or(c, |n| n.min(c)));
            }
        }
        let need = need?;
        Some(self.total.wait_ms(need, now_ms))
    }
}

impl<const SLOTS: usize> Default for Uplink<SLOTS> {
    fn default() -> Self {
        Self::new()
    }
}

// ------------------------------------------------------------- down --

/// The hub's latest beacon, sent once.
pub struct BeaconSlot {
    data: [u8; v3::BEACON_MAX + 8],
    len: usize,
    full: bool,
    /// Beacons replaced by a newer one before they went out.
    pub superseded: u32,
}

impl BeaconSlot {
    /// Largest beacon taken (`link::gw::MAX_BEACON`).
    pub const MAX: usize = v3::BEACON_MAX + 8;

    pub const fn new() -> Self {
        BeaconSlot { data: [0; Self::MAX], len: 0, full: false, superseded: 0 }
    }

    /// A new beacon from the hub. False (and nothing kept) if it is empty
    /// or too long.
    pub fn put(&mut self, b: &[u8]) -> bool {
        if b.is_empty() || b.len() > Self::MAX {
            return false;
        }
        if self.full {
            self.superseded = self.superseded.wrapping_add(1);
        }
        self.data[..b.len()].copy_from_slice(b);
        self.len = b.len();
        self.full = true;
        true
    }

    pub fn is_pending(&self) -> bool {
        self.full
    }

    /// Copy the pending beacon into `out` and forget it (it is never sent
    /// twice). Its length, or `None`.
    pub fn take(&mut self, out: &mut [u8]) -> Option<usize> {
        if !self.full || out.len() < self.len {
            return None;
        }
        self.full = false;
        out[..self.len].copy_from_slice(&self.data[..self.len]);
        Some(self.len)
    }
}

impl Default for BeaconSlot {
    fn default() -> Self {
        Self::new()
    }
}

/// A finished transmit (`GwUp::TxDone`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TxDone {
    pub id: u16,
    pub to: Mac,
    pub radio_acked: bool,
    pub attempts: u8,
    /// From its arrival to its first send (to now if never sent).
    pub held_ms: u16,
}

/// One hub `Transmit` as the queue keeps it.
pub struct TxEntry {
    pub id: u16,
    pub to: Mac,
    pub urgent: bool,
    give_up_ms: u16,
    rx_ms: u64,
    attempts: u8,
    first_ms: Option<u64>,
    last_ms: u64,
    /// Heard since the last attempt: on channel now.
    heard: bool,
    in_flight: bool,
    cancelled: bool,
    /// Bench (`gwforge delay`): not before this; then one attempt,
    /// whatever its age.
    hold_until: Option<u64>,
    len: u16,
    data: [u8; MAX_PACKET],
}

impl TxEntry {
    const EMPTY: TxEntry = TxEntry {
        id: 0,
        to: [0; 6],
        urgent: false,
        give_up_ms: 0,
        rx_ms: 0,
        attempts: 0,
        first_ms: None,
        last_ms: 0,
        heard: false,
        in_flight: false,
        cancelled: false,
        hold_until: None,
        len: 0,
        data: [0; MAX_PACKET],
    };

    pub fn data(&self) -> &[u8] {
        &self.data[..usize::from(self.len)]
    }

    /// When it is given up.
    fn deadline(&self) -> u64 {
        let life = if self.urgent { u64::from(self.give_up_ms).min(URGENT_MAX_AGE_MS) } else { u64::from(self.give_up_ms) };
        self.rx_ms + life
    }

    fn ready(&self, now_ms: u64) -> bool {
        if self.in_flight || self.cancelled {
            return false;
        }
        if let Some(t) = self.hold_until {
            return now_ms >= t && self.attempts == 0;
        }
        if self.attempts == 0 {
            return true;
        }
        let since = now_ms.saturating_sub(self.last_ms);
        since >= BLIND_RETRY_MS || (self.heard && since >= MIN_RETRY_MS)
    }

    /// The next moment it may be ready (for a timer).
    fn ready_at(&self) -> u64 {
        match self.hold_until {
            Some(t) if self.attempts == 0 => t,
            _ if self.attempts == 0 => 0,
            _ => self.last_ms + if self.heard { MIN_RETRY_MS } else { BLIND_RETRY_MS },
        }
    }

    fn done(&self, acked: bool, now_ms: u64) -> TxDone {
        let held = self.first_ms.unwrap_or(now_ms).saturating_sub(self.rx_ms);
        TxDone { id: self.id, to: self.to, radio_acked: acked, attempts: self.attempts, held_ms: held.min(u64::from(u16::MAX)) as u16 }
    }
}

/// A `GwDown::Transmit` from the hub.
#[derive(Debug, Clone, Copy)]
pub struct Transmit<'a> {
    pub id: u16,
    pub to: Mac,
    pub urgent: bool,
    pub give_up_ms: u16,
    pub data: &'a [u8],
}

/// Why [`TxQueue::push`] did not take a transmit (it is finished at once).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushError {
    /// Every slot busy.
    Full,
    /// Empty, too long, or not a kit packet (no `"RS"`): the gateway sends
    /// nothing on the air but the kit's own packets.
    NotAPacket,
}

/// Transmits from the hub waiting for the radio. `N` slots of
/// [`MAX_PACKET`] bytes.
pub struct TxQueue<const N: usize> {
    slots: [TxEntry; N],
    used: [bool; N],
    /// Since boot: taken, refused (full / not a packet), given up, acked,
    /// cancelled.
    pub taken: u32,
    pub refused: u32,
    pub given_up: u32,
    pub acked: u32,
    pub cancelled: u32,
}

impl<const N: usize> TxQueue<N> {
    pub const fn new() -> Self {
        TxQueue { slots: [TxEntry::EMPTY; N], used: [false; N], taken: 0, refused: 0, given_up: 0, acked: 0, cancelled: 0 }
    }

    pub fn len(&self) -> usize {
        self.used.iter().filter(|u| **u).count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// A `Transmit` from the hub, arrived at `now_ms`. `hold_ms` is the
    /// bench delay (`gwforge delay`), `None` normally. On `Err` the hub gets
    /// the [`TxDone`] returned (nothing sent).
    pub fn push(&mut self, t: &Transmit<'_>, now_ms: u64, hold_ms: Option<u64>) -> Result<(), (PushError, TxDone)> {
        let Transmit { id, to, urgent, give_up_ms, data } = *t;
        let fail = |e| {
            Err((e, TxDone { id, to, radio_acked: false, attempts: 0, held_ms: 0 }))
        };
        if data.len() <= HEADER || data.len() > MAX_PACKET || !data.starts_with(&crate::MAGIC) {
            self.refused = self.refused.wrapping_add(1);
            return fail(PushError::NotAPacket);
        }
        let Some(i) = self.used.iter().position(|u| !*u) else {
            self.refused = self.refused.wrapping_add(1);
            return fail(PushError::Full);
        };
        let e = &mut self.slots[i];
        e.id = id;
        e.to = to;
        e.urgent = urgent;
        e.give_up_ms = give_up_ms;
        e.rx_ms = now_ms;
        e.attempts = 0;
        e.first_ms = None;
        e.last_ms = 0;
        e.heard = false;
        e.in_flight = false;
        e.cancelled = false;
        e.hold_until = hold_ms.map(|h| now_ms + h);
        e.len = data.len() as u16;
        e.data[..data.len()].copy_from_slice(data);
        self.used[i] = true;
        self.taken = self.taken.wrapping_add(1);
        Ok(())
    }

    /// `Cancel { to }`: drop everything queued for `to`. Each ends in a
    /// [`TxDone`] (not acked); one in flight ends when its send does.
    pub fn cancel(&mut self, to: Mac, now_ms: u64, mut done: impl FnMut(TxDone)) {
        for i in 0..N {
            if self.used[i] && self.slots[i].to == to && !self.slots[i].cancelled {
                self.cancelled = self.cancelled.wrapping_add(1);
                if self.slots[i].in_flight {
                    self.slots[i].cancelled = true;
                } else {
                    done(self.slots[i].done(false, now_ms));
                    self.used[i] = false;
                }
            }
        }
    }

    /// `mac` was just heard: it is on channel, so its transmits may be
    /// retried now (at most every [`MIN_RETRY_MS`]).
    pub fn heard(&mut self, mac: &Mac) {
        for i in 0..N {
            if self.used[i] && self.slots[i].to == *mac {
                self.slots[i].heard = true;
            }
        }
    }

    /// Give up whatever is past its time (urgent: [`URGENT_MAX_AGE_MS`] after
    /// arrival at most; others `give_up_ms`). A bench-held one lives until
    /// its one attempt.
    pub fn expire(&mut self, now_ms: u64, mut done: impl FnMut(TxDone)) {
        for i in 0..N {
            let e = &self.slots[i];
            if !self.used[i] || e.in_flight || e.hold_until.is_some() {
                continue;
            }
            if now_ms >= e.deadline() {
                self.given_up = self.given_up.wrapping_add(1);
                done(e.done(false, now_ms));
                self.used[i] = false;
            }
        }
    }

    /// The slot to send next, if any is ready: urgent ones first, then by
    /// arrival. Call [`TxQueue::expire`] first.
    pub fn next(&self, now_ms: u64) -> Option<usize> {
        let ready = (0..N).filter(|&i| self.used[i] && self.slots[i].ready(now_ms));
        ready.min_by_key(|&i| (!self.slots[i].urgent, self.slots[i].rx_ms))
    }

    /// Whether an urgent transmit is ready now (it goes before a beacon).
    pub fn urgent_ready(&self, now_ms: u64) -> bool {
        self.next(now_ms).is_some_and(|i| self.slots[i].urgent)
    }

    /// Start an attempt on `slot`: its entry, to copy the bytes from. The
    /// send's outcome goes to [`TxQueue::finish`].
    pub fn begin(&mut self, slot: usize, now_ms: u64) -> &TxEntry {
        let e = &mut self.slots[slot];
        e.in_flight = true;
        e.attempts = e.attempts.saturating_add(1);
        e.first_ms.get_or_insert(now_ms);
        e.last_ms = now_ms;
        e.heard = false;
        e
    }

    /// The attempt on `slot` is over. Acked: done. Not acked: it stays for
    /// a retry unless it is cancelled, past its time, or a bench-held one
    /// (one attempt only).
    pub fn finish(&mut self, slot: usize, acked: bool, now_ms: u64, mut done: impl FnMut(TxDone)) {
        if !self.used[slot] {
            return;
        }
        let e = &mut self.slots[slot];
        e.in_flight = false;
        let over = e.cancelled || e.hold_until.is_some() || now_ms >= e.deadline();
        if acked {
            self.acked = self.acked.wrapping_add(1);
        } else if !over {
            return;
        } else if !e.cancelled {
            self.given_up = self.given_up.wrapping_add(1);
        }
        done(e.done(acked, now_ms));
        self.used[slot] = false;
    }

    /// The next moment something may change (a retry, a deadline, a held
    /// one's release), for the transmit task's timer.
    pub fn wake_at(&self) -> Option<u64> {
        (0..N)
            .filter(|&i| self.used[i] && !self.slots[i].in_flight)
            .map(|i| {
                let e = &self.slots[i];
                if e.hold_until.is_some() { e.ready_at() } else { e.ready_at().min(e.deadline()) }
            })
            .min()
    }
}

impl<const N: usize> Default for TxQueue<N> {
    fn default() -> Self {
        Self::new()
    }
}

// ------------------------------------------------------------- bench --

/// Bench only (gateway `gwforge` feature, docs/LINK-KEYS-DESIGN.md section
/// 9): what a captured gateway can do. Every command alters, holds, repeats
/// or drops the kit's own traffic, or sends the hub a made-up packet over
/// the wire; none puts anything new on the air.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Forge {
    /// Send the hub a made-up packet claiming a roster address.
    Batch,
    /// Alter one byte of the next transmit to this node.
    Flip(Mac),
    /// Send the hub the last relayed packet again.
    Replay,
    /// Hold the next urgent transmit this long.
    Delay(u16),
    /// Radio-acknowledge this address's packets, then discard this percent,
    /// scattered ([`DropPattern::new`]).
    Drop(Mac, u8),
    /// The same in a steady pattern ([`DropPattern::steady`]): against a
    /// node's regular traffic it can remove every packet of one kind, such
    /// as every health report.
    DropSteady(Mac, u8),
    /// Stop dropping.
    DropOff,
}

/// Parse one console line (`gwforge ...`). `None` if it is not one.
pub fn parse_forge(line: &str) -> Option<Forge> {
    let mut w = line.split_ascii_whitespace();
    if w.next()? != "gwforge" {
        return None;
    }
    let cmd = w.next()?;
    let f = match cmd {
        "batch" => Forge::Batch,
        "replay" => Forge::Replay,
        "flip" => Forge::Flip(parse_mac(w.next()?)?),
        "delay" => Forge::Delay(w.next()?.parse().ok()?),
        "drop" => match w.next()? {
            "off" => Forge::DropOff,
            m => {
                let mac = parse_mac(m)?;
                let p: u8 = w.next()?.parse().ok()?;
                if p > 100 {
                    return None;
                }
                match w.next() {
                    None => return Some(Forge::Drop(mac, p)),
                    Some("steady") => Forge::DropSteady(mac, p),
                    Some(_) => return None,
                }
            }
        },
        _ => return None,
    };
    w.next().is_none().then_some(f)
}

/// `aa:bb:cc:dd:ee:ff`.
pub fn parse_mac(s: &str) -> Option<Mac> {
    let mut m = [0u8; 6];
    let mut parts = s.split(':');
    for b in m.iter_mut() {
        let p = parts.next()?;
        if p.len() != 2 {
            return None;
        }
        *b = u8::from_str_radix(p, 16).ok()?;
    }
    parts.next().is_none().then_some(m)
}

/// Drops exactly `percent` of every 100, with no randomness needed on the
/// bench. Two orders:
///
/// - [`DropPattern::new`], scattered: packet `i` of each 100 is dropped when
///   `37 * i mod 100 < percent`. A node's traffic is regular (a few packets
///   every 5 s); a steady pattern lines up with it and takes the same packet
///   of every round, so on the bench (2026-11-19) "80 %" removed every
///   health report and no other kind. Scattered, each kind of packet loses
///   about `percent`.
/// - [`DropPattern::steady`]: that steady pattern, kept on purpose, for the
///   case of a gateway that drops one kind of packet.
#[derive(Debug, Clone, Copy, Default)]
pub struct DropPattern {
    pub percent: u8,
    steady: bool,
    acc: u8,
}

impl DropPattern {
    pub const fn new(percent: u8) -> Self {
        DropPattern { percent, steady: false, acc: 0 }
    }

    pub const fn steady(percent: u8) -> Self {
        DropPattern { percent, steady: true, acc: 0 }
    }

    /// Whether to drop the next one.
    pub fn should_drop(&mut self) -> bool {
        let p = u16::from(self.percent.min(100));
        if self.steady {
            let a = u16::from(self.acc) + p;
            if a >= 100 {
                self.acc = (a - 100) as u8;
                true
            } else {
                self.acc = a as u8;
                false
            }
        } else {
            // `acc` is the packet's place in the current 100.
            let i = u16::from(self.acc);
            self.acc = if i >= 99 { 0 } else { self.acc + 1 };
            (37 * i) % 100 < p
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use std::vec::Vec;

    use super::*;

    const A: Mac = [0x02, 0, 0, 0, 0, 0x0a];
    const B: Mac = [0x02, 0, 0, 0, 0, 0x0b];
    const C: Mac = [0x02, 0, 0, 0, 0, 0x0c];
    const STRANGER: Mac = [0x02, 0, 0, 0, 0, 0xee];

    type Up = Uplink<32>;

    /// A version 3 Frames packet of `len` bytes, distinct per `seq`. Its
    /// stamp and tag are whatever bytes `fill` gives: the gateway cannot
    /// tell a good tag from a bad one.
    fn frames(seq: u16, len: usize, fill: u8) -> Vec<u8> {
        assert!(len > HEADER + v3::TRAILER);
        let mut p = std::vec![fill; len];
        p[..2].copy_from_slice(b"RS");
        p[2] = v3::VERSION_V3;
        p[3] = 2;
        p[4..6].copy_from_slice(&seq.to_le_bytes());
        p[len - v3::TRAILER - 1] = 0;
        p
    }

    fn small(seq: u16) -> Vec<u8> {
        frames(seq, 40, 7)
    }

    fn up_with(roster: &[Mac]) -> std::boxed::Box<Up> {
        let mut u = std::boxed::Box::new(Up::new());
        u.set_roster(roster);
        u
    }

    /// Pop everything the cap allows at `now`; the sources in order.
    fn drain(u: &mut Up, now: u64) -> Vec<Mac> {
        let mut out = Vec::new();
        while let Some(m) = u.pop(now, |q| q.src) {
            out.push(m);
        }
        out
    }

    #[test]
    fn refuse_reasons_match_the_wire() {
        let ours: Vec<&str> = Refuse::ALL.iter().map(|r| r.name()).collect();
        let wire: Vec<&str> = link::gw::RefuseReason::ALL.iter().map(|r| r.name()).collect();
        assert_eq!(ours, wire);
        for (i, r) in Refuse::ALL.iter().enumerate() {
            assert_eq!(r.index(), i);
            assert_eq!(link::gw::RefuseReason::ALL[i].index(), i);
        }
        assert_eq!(MAX_PACKET, link::gw::MAX_PACKET);
        assert_eq!(BeaconSlot::MAX, link::gw::MAX_BEACON);
        assert_eq!(ROSTER_MAX, link::gw::MAX_ROSTER);
        assert_eq!(ROSTER_MAX, kitkeys::SLOTS);
    }

    #[test]
    fn per_mac_budget() {
        // Packets: 40 a second per address, drained as fast as they come.
        let mut u = up_with(&[A, B]);
        let mut ok = 0;
        for s in 0..60u16 {
            match u.offer(A, -50, &small(s), 0, 0) {
                Outcome::Queued => ok += 1,
                Outcome::Dropped(Drop::Refused(Refuse::OverBudget)) => {}
                o => panic!("{o:?}"),
            }
            drain(&mut u, 0);
        }
        assert_eq!(ok, PER_MAC_PACKETS_PER_S);
        assert_eq!(u.refusals.totals[Refuse::OverBudget.index()], 20);
        // Another address has its own budget.
        assert_eq!(u.offer(B, -50, &small(1), 0, 0), Outcome::Queued);
        // Half a second refills half.
        drain(&mut u, 500);
        let mut ok = 0;
        for s in 100..140u16 {
            if u.offer(A, -50, &small(s), 500, 0) == Outcome::Queued {
                ok += 1;
            }
            drain(&mut u, 500);
        }
        assert_eq!(ok, 20);
        // Bytes: 16 KB a second.
        let mut b = Budget::new();
        let mut n = 0;
        while b.take(1000, 0) {
            n += 1;
        }
        assert_eq!(n, PER_MAC_BYTES_PER_S / 1000);
        assert!(!b.take(1000, 0));
        assert!(b.take(1000, 62), "62 ms refills 1,015 bytes");
        // A refused packet spends nothing: small ones still fit the bytes.
        assert!(b.take(10, 62));
    }

    #[test]
    fn total_cap() {
        // Eight roster addresses, each offering a full-size packet every
        // 25 ms (8 x 40 KB/s, far over the cap), for ten seconds.
        let macs: Vec<Mac> = (0..8u8).map(|i| [0x02, 0, 0, 0, 1, i]).collect();
        let mut u = up_with(&macs);
        let mut sent_bytes = 0usize;
        let mut seq = 0u16;
        for t in 0..10_000u64 {
            if t % 25 == 0 {
                for m in &macs {
                    seq = seq.wrapping_add(1);
                    let _ = u.offer(*m, -40, &frames(seq, 1000, 3), t, t * 1000);
                }
            }
            while let Some(n) = u.pop(t, |q| q.data().len() + UP_OVERHEAD) {
                sent_bytes += n;
            }
        }
        let limit = TOTAL_BYTES_PER_S as usize * 10 + TOTAL_BURST as usize;
        assert!(sent_bytes <= limit, "{sent_bytes} > {limit}");
        assert!(sent_bytes >= TOTAL_BYTES_PER_S as usize * 9, "the cap is used: {sent_bytes}");
        // Each address got close to an eighth.
        assert!(u.counts.queue_full > 0);
        // When it says wait, it says how long.
        let mut u = up_with(&[A]);
        for s in 0..3u16 {
            u.offer(A, 0, &frames(s, 1000, 1), 0, 0);
        }
        while u.pop(0, |_| ()).is_some() {}
        if u.queued() > 0 {
            let w = u.wait_ms(0).unwrap();
            assert!(w > 0 && w <= 25, "{w}");
            assert!(u.pop(w, |_| ()).is_some());
        }
    }

    #[test]
    fn round_robin_uart_queue() {
        let mut u = up_with(&[A, B, C]);
        // A floods: it gets at most PER_LANE buffers.
        let mut queued = 0;
        for s in 0..10u16 {
            if u.offer(A, 0, &small(s), 0, 0) == Outcome::Queued {
                queued += 1;
            }
        }
        assert_eq!(queued, PER_LANE);
        assert_eq!(u.offer(B, 0, &small(100), 0, 0), Outcome::Queued);
        assert_eq!(u.offer(C, 0, &small(200), 0, 0), Outcome::Queued);
        // One per lane per turn: B and C do not wait behind A's backlog.
        let order = drain(&mut u, 0);
        assert_eq!(order, [A, B, C, A, A, A]);
        assert_eq!(u.counts.queue_full, (10 - PER_LANE) as u32);
    }

    #[test]
    fn flooded_lanes_cannot_starve_a_quiet_one() {
        // Six flooded addresses fill every buffer they may; a quiet one
        // still gets its packet in, and out next round.
        let flood: Vec<Mac> = (0..6u8).map(|i| [0x02, 0, 0, 0, 2, i]).collect();
        let mut roster = flood.clone();
        roster.push(A);
        let mut u = up_with(&roster);
        for s in 0..20u16 {
            for m in &flood {
                let _ = u.offer(*m, 0, &small(s), 0, 0);
            }
        }
        assert_eq!(u.queued(), 32 - RESERVE);
        assert_eq!(u.offer(A, 0, &small(999), 0, 0), Outcome::Queued);
        // A second quiet packet from A while its first waits: no reserve.
        assert_eq!(u.offer(A, 0, &small(998), 0, 0), Outcome::Dropped(Drop::QueueFull));
        let order = drain(&mut u, 0);
        let pos = order.iter().position(|m| *m == A).unwrap();
        assert!(pos <= roster.len(), "A at {pos}: {order:?}");
    }

    #[test]
    fn duplicate_retry_drop() {
        let mut u = up_with(&[A, B]);
        let p = small(1);
        assert_eq!(u.offer(A, 0, &p, 0, 0), Outcome::Queued);
        assert_eq!(u.offer(A, 0, &p, 1, 0), Outcome::Dropped(Drop::Duplicate));
        // The same bytes from another address are not a retry of it.
        assert_eq!(u.offer(B, 0, &p, 1, 0), Outcome::Queued);
        // Only the previous one counts: A, then something else, then A again.
        assert_eq!(u.offer(A, 0, &small(2), 2, 0), Outcome::Queued);
        assert_eq!(u.offer(A, 0, &p, 3, 0), Outcome::Queued);
        assert_eq!(u.counts.duplicates, 1);
        // A duplicate costs no budget and is not a refusal.
        assert_eq!(u.refusals.totals, [0; 4]);
        assert_eq!(fingerprint(&p), fingerprint(&small(1)));
        assert_ne!(fingerprint(&p), fingerprint(&small(2)));
        assert_ne!(fingerprint(&[]), fingerprint(&[0]));
    }

    #[test]
    fn roster_filter_and_before_the_first_roster() {
        let mut u = std::boxed::Box::new(Up::new());
        assert!(!u.has_roster());
        assert!(u.roster().is_empty());
        // Before any roster, no node packet passes.
        assert_eq!(u.offer(A, 0, &small(1), 0, 0), Outcome::Dropped(Drop::Refused(Refuse::NotInRoster)));
        u.set_roster(&[A]);
        assert_eq!(u.offer(A, 0, &small(1), 0, 0), Outcome::Queued);
        assert_eq!(u.offer(STRANGER, 0, &small(2), 0, 0), Outcome::Dropped(Drop::Refused(Refuse::NotInRoster)));
        // An empty roster lets nobody in.
        u.set_roster(&[]);
        assert_eq!(u.offer(A, 0, &small(3), 0, 0), Outcome::Dropped(Drop::Refused(Refuse::NotInRoster)));
        assert_eq!(u.counts.unlisted, 1, "A's queued packet went with it");
        assert_eq!(u.queued(), 0);
    }

    #[test]
    fn roster_change_keeps_lane_state() {
        let mut u = up_with(&[A, B]);
        let p = small(5);
        assert_eq!(u.offer(B, 0, &p, 0, 0), Outcome::Queued);
        // B moves to another place in the roster: same queue, same last packet.
        u.set_roster(&[C, B, B]);
        assert_eq!(u.roster_lane(&B), Some(1));
        assert_eq!(u.roster_lane(&A), None);
        assert_eq!(u.offer(B, 0, &p, 1, 0), Outcome::Dropped(Drop::Duplicate));
        assert_eq!(drain(&mut u, 1), [B]);
        // More than sixteen: the first sixteen.
        let many: Vec<Mac> = (0..20u8).map(|i| [0x02, 0, 0, 0, 3, i]).collect();
        u.set_roster(&many);
        assert_eq!(u.roster_lane(&many[15]), Some(15));
        assert_eq!(u.roster_lane(&many[16]), None);
        assert_eq!(u.roster(), &many[..16]);
    }

    #[test]
    fn versions_and_shapes() {
        let mut u = up_with(&[A]);
        // Version 2: a board that was not reflashed, even from a roster address.
        let mut v2 = small(1);
        v2[2] = crate::VERSION_KIT_KEY;
        assert_eq!(u.offer(A, 0, &v2, 0, 0), Outcome::Dropped(Drop::Refused(Refuse::OldFirmware)));
        // Not ours.
        assert_eq!(u.offer(A, 0, b"hello world", 0, 0), Outcome::Dropped(Drop::Foreign));
        assert_eq!(u.offer(A, 0, &[], 0, 0), Outcome::Dropped(Drop::Foreign));
        // Ours but the wrong shape.
        let mut bad = small(2);
        bad[3] = 9;
        assert_eq!(u.offer(A, 0, &bad, 0, 0), Outcome::Dropped(Drop::Refused(Refuse::Malformed)));
        assert_eq!(u.offer(A, 0, &small(3)[..HEADER + v3::TRAILER], 0, 0), Outcome::Dropped(Drop::Refused(Refuse::Malformed)));
        let mut big = frames(4, MAX_PACKET, 1);
        big.push(1);
        assert_eq!(u.offer(A, 0, &big, 0, 0), Outcome::Dropped(Drop::Refused(Refuse::Malformed)));
        assert_eq!(u.offer(A, 0, b"RS\x01", 0, 0), Outcome::Dropped(Drop::Refused(Refuse::Malformed)));
        // A full-size packet passes.
        assert_eq!(u.offer(A, 0, &frames(5, MAX_PACKET, 1), 0, 0), Outcome::Queued);
        // Hub-to-node kinds heard on the air: another kit's gateway, not ours to relay.
        let mut sync = std::vec![0u8; v3::SYNC_LEN];
        sync[..4].copy_from_slice(&[b'R', b'S', 3, 1]);
        assert_eq!(u.offer(A, 0, &sync, 0, 0), Outcome::Dropped(Drop::Foreign));
        let mut beacon = std::vec![0u8; v3::beacon_len(2)];
        beacon[..4].copy_from_slice(&[b'R', b'S', 3, 4]);
        beacon[HEADER + 1] = 2;
        assert_eq!(u.offer(STRANGER, 0, &beacon, 0, 0), Outcome::Dropped(Drop::Foreign));
        assert_eq!(u.counts.foreign, 4);
        assert_eq!(u.refusals.totals, [0, 0, 1, 4]);
    }

    #[test]
    fn open_kit_packets_pass_in_the_shared_lane() {
        let mut u = up_with(&[A]);
        let mut buf = [0u8; 64];
        let mut b = crate::Batch::default();
        assert!(b.push(&[3, 1, 2, 0]));
        let p = b.packet(1).to_vec();
        buf[..p.len()].copy_from_slice(&p);
        // From a roster address: its own lane. From anyone else: shared.
        assert_eq!(u.offer(A, 0, &p, 0, 0), Outcome::Queued);
        assert_eq!(u.offer(STRANGER, 0, &p, 0, 0), Outcome::Queued);
        assert_eq!(drain(&mut u, 0), [A, STRANGER]);
        // An open beacon is not a node packet.
        let mut beacon = [0u8; crate::BEACON_LEN];
        crate::beacon(&mut beacon, 1, 6);
        assert_eq!(u.offer(STRANGER, 0, &beacon, 0, 0), Outcome::Dropped(Drop::Foreign));
    }

    #[test]
    fn unprovisioned_rate_limit() {
        let mut u = std::boxed::Box::new(Up::new());
        let ann = |mac: Mac, seq| {
            let a = v3::Unprovisioned { mac, image: 1, firmware: [b'x'; v3::FIRMWARE_LEN] };
            let mut buf = [0u8; v3::UNPROVISIONED_LEN];
            v3::unprovisioned(&mut buf, seq, &a).unwrap().to_vec()
        };
        // No roster needed; at most one a second in total, from anyone.
        assert_eq!(u.offer(STRANGER, 0, &ann(STRANGER, 1), 0, 0), Outcome::Queued);
        assert_eq!(u.offer(B, 0, &ann(B, 1), 999, 0), Outcome::Dropped(Drop::UnprovisionedRate));
        assert_eq!(u.offer(B, 0, &ann(B, 2), 1000, 0), Outcome::Queued);
        assert_eq!(u.offer(C, 0, &ann(C, 1), 1500, 0), Outcome::Dropped(Drop::UnprovisionedRate));
        assert_eq!(u.counts.unprovisioned_limited, 2);
        assert_eq!(u.refusals.totals, [0; 4]);
        let mut got = Vec::new();
        while let Some(d) = u.pop(1000, |q| v3::parse_unprovisioned(q.data()).map(|a| a.mac)) {
            got.push(d);
        }
        assert_eq!(got, [Some(STRANGER), Some(B)]);
    }

    #[test]
    fn stale_queued_packets_are_dropped() {
        let mut u = up_with(&[A]);
        assert_eq!(u.offer(A, 0, &small(1), 0, 0), Outcome::Queued);
        assert_eq!(u.offer(A, 0, &small(2), 600, 0), Outcome::Queued);
        let got: Vec<u16> = std::iter::from_fn(|| u.pop(1001, |q| u16::from_le_bytes([q.data()[4], q.data()[5]]))).collect();
        assert_eq!(got, [2]);
        assert_eq!(u.counts.stale, 1);
        assert_eq!(u.counts.uart_drops(), 1);
    }

    #[test]
    fn queued_packet_is_handed_out_untouched() {
        let mut u = up_with(&[A]);
        let p = frames(9, 300, 0x5A);
        assert_eq!(u.offer(A, -71, &p, 5, 5_123), Outcome::Queued);
        let (src, rssi, at, data) = u.pop(6, |q| (q.src, q.rssi, q.at_us, q.data().to_vec())).unwrap();
        assert_eq!((src, rssi, at), (A, -71, 5_123));
        assert_eq!(data, p);
    }

    /// The gateway relays a forged packet exactly as it relays a genuine
    /// one: it holds no key, so it cannot tell them apart, and the hub
    /// decides. Nothing in this module takes or makes a key.
    #[test]
    fn relay_needs_and_accepts_no_key() {
        // A genuine packet, made by a real hub-and-node pair (test key).
        let key = kitkeys::NodeKey::from_bytes([0x11; 32]);
        let gw: Mac = [0x02, 0, 0, 0, 0, 0x99];
        let mut hub = v3::Hub::new(5);
        hub.set_gateway(gw);
        assert!(hub.set_slot(0, A, &key));
        let mut node = v3::Node::new(v3::NodeKeys::new(0, A, &key), || 7);
        let mut beacon = [0u8; v3::BEACON_MAX];
        let n = hub.seal_beacon(&mut beacon, 1, 6, 10_000).unwrap();
        assert!(matches!(node.accept(&gw, &beacon[..n], 1000, false), v3::NodeVerdict::Accept(_)));
        let mut b = crate::Batch::default();
        assert!(b.push(&[3, 1, 2, 0]));
        let genuine = b.sealed_v3(1, &mut node, 1000).unwrap().to_vec();
        let mut forged = genuine.clone();
        let n = forged.len();
        forged[n - 1] ^= 0xFF;
        let mut u = up_with(&[A]);
        assert_eq!(u.offer(A, 0, &genuine, 0, 0), Outcome::Queued);
        assert_eq!(u.offer(A, 0, &forged, 0, 0), Outcome::Queued);
        assert_eq!(drain(&mut u, 0).len(), 2);
        // The source names no key type or key function.
        let src = include_str!("relay.rs");
        // Code only (comments may say what the gateway cannot do).
        let code: std::string::String = src[..src.find("#[cfg(test)]").unwrap()]
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for word in [["Link", "Key"], ["Node", "Key"], ["Kit", "Key"], ["KIT_", "KEY"], ["kit", "keys"], ["auth::", "Key"], ["ver", "ify"], ["se", "al("], ["Hub", "::"]] {
            let w = word.concat();
            assert!(!code.contains(&w), "relay.rs mentions {w}");
        }
    }

    #[test]
    fn refusals_once_a_window_per_address_and_reason() {
        let mut r: Refusals<2> = Refusals::new();
        for _ in 0..3 {
            r.record(A, Refuse::NotInRoster, -80, 100);
        }
        r.record(A, Refuse::NotInRoster, -40, 200);
        r.record(A, Refuse::OverBudget, -60, 300);
        // The log is full: further addresses share an all-zero entry.
        r.record(B, Refuse::NotInRoster, -70, 400);
        r.record(C, Refuse::NotInRoster, -90, 500);
        let mut out = Vec::new();
        r.drain(1100, |x| out.push(x));
        assert_eq!(
            out,
            [
                Refusal { src: A, reason: Refuse::NotInRoster, count: 4, rssi: -40, window_ms: 1000 },
                Refusal { src: A, reason: Refuse::OverBudget, count: 1, rssi: -60, window_ms: 800 },
                Refusal { src: [0; 6], reason: Refuse::NotInRoster, count: 2, rssi: -70, window_ms: 700 },
            ]
        );
        assert_eq!(r.totals, [6, 1, 0, 0]);
        let mut again = 0;
        r.drain(2100, |_| again += 1);
        assert_eq!(again, 0, "each refusal is reported once");
    }

    // ---------------------------------------------------------- down --

    type Tx = TxQueue<4>;

    fn cmd(n: u8) -> Vec<u8> {
        let mut p = std::vec![n; 60];
        p[..4].copy_from_slice(&[b'R', b'S', 3, 3]);
        p
    }

    /// Run the queue from `from` to `to` ms, never acked; the attempt times.
    fn run_unacked(q: &mut Tx, from: u64, to: u64, done: &mut Vec<(u64, TxDone)>) -> Vec<u64> {
        let mut tries = Vec::new();
        for t in from..=to {
            q.expire(t, |d| done.push((t, d)));
            while let Some(i) = q.next(t) {
                q.begin(i, t);
                tries.push(t);
                q.finish(i, false, t, |d| done.push((t, d)));
            }
        }
        tries
    }

    #[test]
    fn urgent_ordering_and_the_300_ms_rule() {
        let mut q = Tx::new();
        q.push(&Transmit { id: 1, to: A, urgent: false, give_up_ms: 2000, data: &cmd(1) }, 0, None).unwrap();
        q.push(&Transmit { id: 2, to: B, urgent: true, give_up_ms: 2000, data: &cmd(2) }, 5, None).unwrap();
        q.push(&Transmit { id: 3, to: C, urgent: true, give_up_ms: 300, data: &cmd(3) }, 6, None).unwrap();
        // Urgent first (oldest first), then the rest.
        assert!(q.urgent_ready(10));
        let i = q.next(10).unwrap();
        assert_eq!(q.begin(i, 10).id, 2);
        let mut done = Vec::new();
        q.finish(i, true, 11, |d| done.push(d));
        assert_eq!(done, [TxDone { id: 2, to: B, radio_acked: true, attempts: 1, held_ms: 5 }]);
        let i = q.next(11).unwrap();
        assert_eq!(q.begin(i, 11).id, 3);
        q.finish(i, false, 12, |d| done.push(d));
        let i = q.next(12).unwrap();
        assert_eq!(q.begin(i, 12).id, 1);
        q.finish(i, false, 13, |d| done.push(d));
        assert_eq!(done.len(), 1);
        // An urgent one is dropped 300 ms after arrival, even with give_up 2000.
        let mut q = Tx::new();
        q.push(&Transmit { id: 7, to: B, urgent: true, give_up_ms: 2000, data: &cmd(7) }, 100, None).unwrap();
        let mut done = Vec::new();
        let tries = run_unacked(&mut q, 100, 1000, &mut done);
        assert_eq!(done.len(), 1);
        let (t, d) = done[0];
        assert_eq!(t, 400, "dropped at arrival + 300 ms");
        assert!(!d.radio_acked);
        assert!(tries.iter().all(|&t| t < 400));
        assert_eq!(usize::from(d.attempts), tries.len());
        assert_eq!(d.held_ms, 0);
        // One that arrives already waiting behind nothing goes at once.
        assert_eq!(tries[0], 100);
    }

    #[test]
    fn give_up_timing() {
        let mut q = Tx::new();
        q.push(&Transmit { id: 9, to: A, urgent: false, give_up_ms: 2000, data: &cmd(9) }, 1000, None).unwrap();
        let mut done = Vec::new();
        let tries = run_unacked(&mut q, 1000, 4000, &mut done);
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].0, 3000);
        assert_eq!(done[0].1, TxDone { id: 9, to: A, radio_acked: false, attempts: tries.len() as u8, held_ms: 0 });
        // Blind retries every BLIND_RETRY_MS.
        assert_eq!(tries.len() as u64, 2000 / BLIND_RETRY_MS);
        assert!(tries.windows(2).all(|w| w[1] - w[0] == BLIND_RETRY_MS));
        assert!(q.is_empty());
        assert_eq!(q.given_up, 1);
    }

    #[test]
    fn heard_node_is_retried_sooner() {
        let mut q = Tx::new();
        q.push(&Transmit { id: 1, to: A, urgent: false, give_up_ms: 2000, data: &cmd(1) }, 0, None).unwrap();
        let i = q.next(0).unwrap();
        q.begin(i, 0);
        q.finish(i, false, 1, |_| panic!());
        assert_eq!(q.next(19), None);
        q.heard(&B);
        assert_eq!(q.next(20), None, "another node heard");
        q.heard(&A);
        assert_eq!(q.next(19), None, "not more often than MIN_RETRY_MS");
        assert_eq!(q.next(20), Some(i));
        assert_eq!(q.wake_at(), Some(MIN_RETRY_MS));
        let mut done = Vec::new();
        q.begin(i, 20);
        q.finish(i, true, 22, |d| done.push(d));
        assert_eq!(done, [TxDone { id: 1, to: A, radio_acked: true, attempts: 2, held_ms: 0 }]);
    }

    #[test]
    fn held_ms_is_until_the_first_send() {
        let mut q = Tx::new();
        q.push(&Transmit { id: 1, to: A, urgent: false, give_up_ms: 2000, data: &cmd(1) }, 0, None).unwrap();
        q.push(&Transmit { id: 2, to: B, urgent: false, give_up_ms: 2000, data: &cmd(2) }, 0, None).unwrap();
        let i = q.next(0).unwrap();
        q.begin(i, 0);
        // The other waits behind the first send.
        let mut done = Vec::new();
        q.finish(i, true, 30, |d| done.push(d));
        let j = q.next(30).unwrap();
        q.begin(j, 30);
        q.finish(j, true, 31, |d| done.push(d));
        assert_eq!(done[1].held_ms, 30);
    }

    #[test]
    fn cancel_ends_queued_and_in_flight() {
        let mut q = Tx::new();
        q.push(&Transmit { id: 1, to: A, urgent: false, give_up_ms: 2000, data: &cmd(1) }, 0, None).unwrap();
        q.push(&Transmit { id: 2, to: A, urgent: false, give_up_ms: 2000, data: &cmd(2) }, 0, None).unwrap();
        q.push(&Transmit { id: 3, to: B, urgent: false, give_up_ms: 2000, data: &cmd(3) }, 0, None).unwrap();
        let i = q.next(0).unwrap();
        assert_eq!(q.begin(i, 0).id, 1);
        let mut done = Vec::new();
        q.cancel(A, 5, |d| done.push(d));
        assert_eq!(done, [TxDone { id: 2, to: A, radio_acked: false, attempts: 0, held_ms: 5 }]);
        // The one in flight is never sent again; it ends with its send.
        q.finish(i, false, 6, |d| done.push(d));
        assert_eq!(done[1], TxDone { id: 1, to: A, radio_acked: false, attempts: 1, held_ms: 0 });
        assert_eq!(q.len(), 1);
        assert_eq!(q.cancelled, 2);
        assert_eq!(q.given_up, 0);
    }

    #[test]
    fn full_queue_and_non_packets_are_refused_at_once() {
        let mut q = Tx::new();
        for id in 0..4 {
            q.push(&Transmit { id, to: A, urgent: false, give_up_ms: 2000, data: &cmd(1) }, 0, None).unwrap();
        }
        let e = q.push(&Transmit { id: 9, to: B, urgent: true, give_up_ms: 300, data: &cmd(1) }, 0, None).unwrap_err();
        assert_eq!(e, (PushError::Full, TxDone { id: 9, to: B, radio_acked: false, attempts: 0, held_ms: 0 }));
        let mut q = Tx::new();
        assert_eq!(q.push(&Transmit { id: 1, to: A, urgent: false, give_up_ms: 2000, data: b"not a kit packet" }, 0, None).unwrap_err().0, PushError::NotAPacket);
        assert_eq!(q.push(&Transmit { id: 1, to: A, urgent: false, give_up_ms: 2000, data: b"RS" }, 0, None).unwrap_err().0, PushError::NotAPacket);
        assert_eq!(q.push(&Transmit { id: 1, to: A, urgent: false, give_up_ms: 2000, data: &[b'R'; MAX_PACKET + 1] }, 0, None).unwrap_err().0, PushError::NotAPacket);
        assert_eq!(q.refused, 3);
    }

    #[test]
    fn bench_hold_sends_once_after_the_delay() {
        // `gwforge delay 1000`: a captured gateway ignores the 300 ms rule.
        let mut q = Tx::new();
        q.push(&Transmit { id: 4, to: A, urgent: true, give_up_ms: 300, data: &cmd(4) }, 0, Some(1000)).unwrap();
        let mut done = Vec::new();
        let tries = run_unacked(&mut q, 0, 3000, &mut done);
        assert_eq!(tries, [1000]);
        assert_eq!(done, [(1000, TxDone { id: 4, to: A, radio_acked: false, attempts: 1, held_ms: 1000 })]);
    }

    #[test]
    fn beacons_are_never_repeated() {
        let mut s = BeaconSlot::new();
        let mut out = [0u8; BeaconSlot::MAX];
        assert_eq!(s.take(&mut out), None);
        assert!(s.put(&[1; 40]));
        assert!(s.put(&[2; 48]));
        assert_eq!(s.superseded, 1);
        assert_eq!(s.take(&mut out), Some(48));
        assert_eq!(out[0], 2);
        assert_eq!(s.take(&mut out), None, "sent once");
        assert!(!s.put(&[]));
        assert!(!s.put(&[0; BeaconSlot::MAX + 1]));
        assert!(!s.is_pending());
    }

    #[test]
    fn forge_commands_parse() {
        assert_eq!(parse_forge("gwforge batch"), Some(Forge::Batch));
        assert_eq!(parse_forge("  gwforge   replay "), Some(Forge::Replay));
        assert_eq!(parse_forge("gwforge flip 02:00:00:00:00:0a"), Some(Forge::Flip(A)));
        assert_eq!(parse_forge("gwforge delay 1000"), Some(Forge::Delay(1000)));
        assert_eq!(parse_forge("gwforge drop 02:00:00:00:00:0b 50"), Some(Forge::Drop(B, 50)));
        assert_eq!(parse_forge("gwforge drop 02:00:00:00:00:0b 80 steady"), Some(Forge::DropSteady(B, 80)));
        assert_eq!(parse_forge("gwforge drop 02:00:00:00:00:0b 80 often"), None);
        assert_eq!(parse_forge("gwforge drop off"), Some(Forge::DropOff));
        for bad in ["", "gwforge", "gwforge flip", "gwforge flip 02:00:00:00:00", "gwforge flip 02:00:00:00:00:0a:01", "gwforge drop 02:00:00:00:00:0b 101", "gwforge delay -1", "gwforge batch now", "fault panic", "gwforge zap"] {
            assert_eq!(parse_forge(bad), None, "{bad}");
        }
    }

    #[test]
    fn drop_pattern_is_exact() {
        for p in [0u8, 1, 33, 50, 99, 100] {
            for mut d in [DropPattern::new(p), DropPattern::steady(p)] {
                for round in 0..3 {
                    let n = (0..100).filter(|_| d.should_drop()).count();
                    assert_eq!(n, usize::from(p), "{p} round {round}");
                }
            }
        }
    }

    /// A node sends a few packets every 5 s, the health report always in
    /// the same place. The steady pattern can take that one packet every
    /// time; the scattered one takes about its share of every place.
    #[test]
    fn scattered_drops_do_not_line_up_with_regular_traffic() {
        for per_round in [2usize, 4, 5, 8, 10] {
            let mut d = DropPattern::new(80);
            let mut kept = std::vec![0u32; per_round];
            for i in 0..per_round * 100 {
                if !d.should_drop() {
                    kept[i % per_round] += 1;
                }
            }
            for (place, k) in kept.iter().enumerate() {
                assert!((10..=30).contains(k), "{per_round} a round, place {place}: {k} of 100 kept");
            }
        }
        // The steady pattern at 80 % with five packets a round keeps one
        // place and never the others.
        let mut d = DropPattern::steady(80);
        let mut kept = [0u32; 5];
        for i in 0..500 {
            if !d.should_drop() {
                kept[i % 5] += 1;
            }
        }
        assert_eq!(kept.iter().filter(|&&k| k == 0).count(), 4, "{kept:?}");
    }
}
