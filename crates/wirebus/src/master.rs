//! The hub's end of the bus: who is on it, whose turn it is, and what came
//! back.
//!
//! Drive it with two calls:
//! - [`Master::on_rx`] with every byte that arrives on the shared return
//!   wire, as soon as possible after it arrives (the P4 hub calls it from its
//!   UART interrupt), and
//! - [`Master::tick`] whenever the line may have something to do: right
//!   after `on_rx` (so the next poll follows an answer at once) and from the
//!   main loop (timeouts, the cycle timer, join slots). It returns the bytes
//!   to send on the hub's TX, if any.
//!
//! Frames come out through the `emit` callback as [`Event::Frame`], one
//! whole COBS frame at a time with the station MAC of the node that sent it,
//! only after the answer's CRC has been checked. Nothing in an answer is
//! passed on before its [`link::Frame::BusDone`] proves it intact.
//!
//! Scheduling: every present node is polled at least once per
//! [`MasterConfig::cycle_us`]; a node that says it has more is polled again
//! as soon as nobody else is due, so a busy node takes the spare time but
//! never another node's turn. Each answer is limited to a byte budget that
//! shrinks as nodes are added, so one full cycle stays near `cycle_us`. A
//! node that misses [`MasterConfig::absent_after`] polls in a row is marked
//! absent and asked only once every [`MasterConfig::absent_poll_us`], so an
//! unplugged node costs almost nothing; it comes back on its own when it
//! answers again.

use heapless::Vec;
use link::{Command, Frame};

use crate::{MAX_ADDR, MAX_ANSWER, MAX_BUDGET, MAX_DONE, MAX_UNIT, TAG_BUS_DONE, TAG_BUS_JOIN, chunk_tag, crc, wire_us};

/// Timing and sizes. All times in microseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MasterConfig {
    pub baud: u32,
    /// Every present node is polled at least this often.
    pub cycle_us: u32,
    /// From the end of a poll on the wire to the node's first byte. The node
    /// answers from a high-priority task; this covers its worst scheduling
    /// delay. A node that misses it stays silent (it checks for newer bytes
    /// before answering), so a late node costs time, never a collision.
    pub first_byte_us: u32,
    /// Longest silence inside an answer.
    pub gap_us: u32,
    /// Quiet time after a timeout or a broken answer before the next poll,
    /// so a straggler's last bytes cannot run into the next node's answer.
    pub guard_us: u32,
    /// "Anyone new?" this often in steady state...
    pub join_every_us: u32,
    /// ...and this often while the table is empty, for
    /// [`Self::join_fast_for_us`] after boot and after any join activity.
    pub join_fast_us: u32,
    pub join_fast_for_us: u32,
    /// Join slots per "anyone new?" and their length. A slot must hold one
    /// `BusJoin` (at most 16 bytes, 174 us at 921600) plus the spread of the
    /// nodes' reaction times.
    pub join_slots: u8,
    pub join_slot_us: u16,
    /// Consecutive silent polls before a node is marked absent.
    pub absent_after: u8,
    /// How often an absent node is still asked.
    pub absent_poll_us: u32,
    /// Bounds on the per-answer byte budget.
    pub min_budget: u16,
    pub max_budget: u16,
}

impl MasterConfig {
    /// The defaults for 921600 baud (docs/WIREBUS.md "Timing budget").
    pub const fn new() -> Self {
        Self {
            baud: crate::BAUD,
            cycle_us: 20_000,
            first_byte_us: 4_000,
            gap_us: 2_000,
            guard_us: 1_000,
            join_every_us: 1_000_000,
            join_fast_us: 100_000,
            join_fast_for_us: 5_000_000,
            join_slots: 8,
            join_slot_us: 600,
            absent_after: 10,
            absent_poll_us: 1_000_000,
            min_budget: 256,
            max_budget: MAX_BUDGET as u16,
        }
    }

    /// Bytes the line carries in one cycle.
    pub const fn cycle_bytes(&self) -> u32 {
        (self.cycle_us as u64 * self.baud as u64 / 10 / 1_000_000) as u32
    }
}

impl Default for MasterConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-node counters (running totals since the node first joined this hub
/// boot).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NodeStats {
    pub polls: u32,
    /// Good answers (CRC checked), with or without frames.
    pub answers: u32,
    /// Good answers that carried no frames ("nothing").
    pub empty: u32,
    /// Polls with no byte back at all.
    pub timeouts: u32,
    /// Answers that came back but did not check out (bad CRC, garbled or cut
    /// short, wrong address). The node resends them.
    pub corrupt: u32,
    /// Good answers the hub already had (its acknowledgement was lost); dropped.
    pub dups: u32,
    pub frames: u32,
    /// Bytes of good answers, `BusDone` included.
    pub bytes: u32,
    /// Times this MAC joined (1 + rejoins after reboots or image switches).
    pub joins: u32,
    /// Reply time: end of the poll on the wire to the first byte back
    /// (estimated from when the bytes were handed over), microseconds.
    pub reply_us_last: u32,
    pub reply_us_max: u32,
    pub reply_us_sum: u64,
    /// Longest time between two good answers: how long a frame queued on the
    /// node could have waited for its turn, microseconds.
    pub wait_us_max: u32,
}

impl NodeStats {
    /// Mean reply time, microseconds.
    pub fn reply_us_mean(&self) -> u32 {
        if self.answers == 0 { 0 } else { (self.reply_us_sum / u64::from(self.answers)) as u32 }
    }
}

/// One node the hub knows: a MAC bound to a bus address.
#[derive(Debug, Clone, Copy)]
pub struct Entry {
    pub mac: [u8; 6],
    pub addr: u8,
    /// Missed `absent_after` polls in a row; asked rarely until it answers.
    pub absent: bool,
    pub stats: NodeStats,
    /// Sequence number of the last answer taken (0: none since the grant).
    last_seq: u8,
    due_us: u64,
    misses: u8,
    last_ok_us: Option<u64>,
    last_poll_us: u64,
}

/// Bus-wide counters.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct BusStats {
    pub polls: u32,
    pub join_slots: u32,
    /// Good `BusJoin`s heard, and addresses granted.
    pub joins_heard: u32,
    pub grants: u32,
    /// Chunks in a join window that were not a good `BusJoin`: collisions.
    pub join_corrupt: u32,
    /// Joins refused because the table was full of present nodes.
    pub join_full: u32,
    /// Bytes and decodable `link` frames heard while no answer was expected
    /// (a straggler, or a device that does not speak the bus at all, such as
    /// an old gateway streaming on its own).
    pub stray_bytes: u32,
    pub stray_frames: u32,
    /// Bytes the hub sent (polls, join slots, grants).
    pub tx_bytes: u32,
    /// Time the line spent on polls, answers and join windows, microseconds
    /// (for the utilisation figure).
    pub busy_us: u64,
}

/// What the master tells its owner.
#[derive(Debug, PartialEq, Eq)]
pub enum Event<'a> {
    /// One whole `link` frame (COBS, zero delimiter included) from a checked
    /// answer, sent by the node with this station MAC.
    Frame { mac: [u8; 6], addr: u8, bytes: &'a [u8] },
    /// The end of one checked answer from this node (after its frames).
    AnswerEnd { mac: [u8; 6], addr: u8 },
    /// A node joined (`again`: it had this address before, e.g. after an
    /// image switch).
    Joined { mac: [u8; 6], addr: u8, again: bool },
    /// A node stopped answering.
    Absent { mac: [u8; 6], addr: u8 },
    /// An absent node answered again.
    Back { mac: [u8; 6], addr: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle { quiet_until: u64 },
    Answer { idx: usize, poll_us: u64, poll_end: u64, deadline: u64, hard: u64, first: Option<u64> },
    Join { start: u64, until: u64 },
}

/// The bus master for up to `N` nodes.
pub struct Master<const N: usize> {
    cfg: MasterConfig,
    epoch: u16,
    table: Vec<Entry, N>,
    phase: Phase,
    buf: [u8; MAX_ANSWER + 32],
    len: usize,
    chunk_start: usize,
    overflow: bool,
    joins: Vec<([u8; 6], u8), 8>,
    tx: [u8; 8 * 24],
    next_join_us: u64,
    fast_join_until: u64,
    /// Index after which the next tie is broken (round robin).
    rr: usize,
    stray: link::Decoder,
    stray_window: (u64, u32),
    last_good_us: Option<u64>,
    started: bool,
    pub stats: BusStats,
}

impl<const N: usize> Master<N> {
    /// `epoch`: a random number per hub boot (nodes holding an address from
    /// another boot join again).
    pub fn new(cfg: MasterConfig, epoch: u16) -> Self {
        Self {
            cfg,
            epoch,
            table: Vec::new(),
            phase: Phase::Idle { quiet_until: 0 },
            buf: [0; MAX_ANSWER + 32],
            len: 0,
            chunk_start: 0,
            overflow: false,
            joins: Vec::new(),
            tx: [0; 8 * 24],
            next_join_us: 0,
            fast_join_until: 0,
            rr: 0,
            stray: link::Decoder::new(),
            stray_window: (0, 0),
            last_good_us: None,
            started: false,
            stats: BusStats::default(),
        }
    }

    pub fn config(&self) -> &MasterConfig {
        &self.cfg
    }

    pub fn epoch(&self) -> u16 {
        self.epoch
    }

    /// The nodes the hub knows, present or absent.
    pub fn entries(&self) -> &[Entry] {
        &self.table
    }

    /// The node with this MAC, if it has joined.
    pub fn entry(&self, mac: &[u8; 6]) -> Option<&Entry> {
        self.table.iter().find(|e| &e.mac == mac)
    }

    /// Nodes answering polls.
    pub fn present(&self) -> usize {
        self.table.iter().filter(|e| !e.absent).count()
    }

    /// An answer or a join window is open: the return wire belongs to a node.
    pub fn busy(&self) -> bool {
        !matches!(self.phase, Phase::Idle { .. })
    }

    /// The byte budget a poll carries now.
    pub fn budget(&self) -> u16 {
        let n = self.present().max(1) as u32;
        // Per node: the poll (~8 bytes) and the BusDone (~9) and the two
        // reaction times (~0.5 ms, ~46 bytes) are overhead.
        let share = (self.cfg.cycle_bytes() / n).saturating_sub(64);
        share.clamp(u32::from(self.cfg.min_budget), u32::from(self.cfg.max_budget)) as u16
    }

    /// When `tick` next has something to do (absolute microseconds).
    pub fn next_deadline(&self) -> u64 {
        match self.phase {
            Phase::Answer { deadline, .. } => deadline,
            Phase::Join { until, .. } => until,
            Phase::Idle { quiet_until } => {
                let due = self.table.iter().map(|e| e.due_us).min().unwrap_or(u64::MAX);
                quiet_until.max(due.min(self.next_join_us))
            }
        }
    }

    /// Does the line look like a device that does not speak the bus (an old
    /// ESP-NOW gateway or a point-to-point node streaming on its own)? True
    /// after at least 3 whole `link` frames arrived unasked within 2 s while
    /// no node has answered a poll for 5 s. The owner can then stop polling
    /// and read the line the old way (docs/WIREBUS.md "Old gateways").
    pub fn looks_legacy(&self, now: u64) -> bool {
        let fresh = now.saturating_sub(self.stray_window.0) <= 2_000_000;
        let no_bus = self.last_good_us.is_none_or(|t| now.saturating_sub(t) > 5_000_000);
        fresh && self.stray_window.1 >= 3 && no_bus
    }

    /// Bytes from the return wire.
    pub fn on_rx(&mut self, now: u64, bytes: &[u8], emit: &mut impl FnMut(Event<'_>)) {
        let mut rest = bytes;
        while !rest.is_empty() {
            rest = match self.phase {
                Phase::Answer { .. } => self.answer_bytes(now, rest, emit),
                Phase::Join { .. } => {
                    self.join_bytes(rest);
                    &[]
                }
                Phase::Idle { .. } => {
                    self.stray_bytes(now, rest);
                    &[]
                }
            };
        }
    }

    fn push(&mut self, b: u8) {
        if self.len < self.buf.len() {
            self.buf[self.len] = b;
            self.len += 1;
        } else {
            self.overflow = true;
        }
    }

    fn reset_buf(&mut self) {
        self.len = 0;
        self.chunk_start = 0;
        self.overflow = false;
    }

    fn answer_bytes<'b>(&mut self, now: u64, bytes: &'b [u8], emit: &mut impl FnMut(Event<'_>)) -> &'b [u8] {
        let gap = u64::from(self.cfg.gap_us);
        let baud = self.cfg.baud;
        if let Phase::Answer { poll_end, deadline, hard, first, .. } = &mut self.phase {
            if first.is_none() {
                // The bytes were handed over in a batch; the first of them
                // arrived about their own wire time before now.
                *first = Some(now.saturating_sub(wire_us(bytes.len(), baud)).max(*poll_end));
            }
            *deadline = (now + gap).min(*hard);
        }
        for (i, &b) in bytes.iter().enumerate() {
            if b != 0 {
                self.push(b);
                continue;
            }
            if self.len == self.chunk_start {
                continue; // a lone zero (line glitch): not part of any frame
            }
            self.push(0);
            if self.overflow {
                continue;
            }
            if chunk_tag(&self.buf[self.chunk_start..self.len]) == Some(TAG_BUS_DONE) {
                self.finish(now, emit);
                return &bytes[i + 1..];
            }
            self.chunk_start = self.len;
        }
        &[]
    }

    /// A `BusDone`-tagged chunk ended at `self.len`: check the answer.
    fn finish(&mut self, now: u64, emit: &mut impl FnMut(Event<'_>)) {
        let Phase::Answer { idx, poll_us, poll_end, first, .. } = self.phase else { return };
        let body = self.chunk_start;
        let done = decode_small(&self.buf[body..self.len]);
        let cfg = self.cfg;
        let Some(e) = self.table.get_mut(idx) else { return };
        self.stats.busy_us += now.saturating_sub(poll_us);
        let good = match done {
            Some(Frame::BusDone { addr, seq, more, crc: c }) if addr == e.addr && (seq != 0 || body == 0) => {
                (c == crc::done_crc(&self.buf[..body], addr, seq, more)).then_some((seq, more))
            }
            _ => None,
        };
        match good {
            Some((seq, more)) => {
                let s = &mut e.stats;
                s.answers += 1;
                s.bytes = s.bytes.wrapping_add(self.len as u32);
                let reply = first.unwrap_or(now).saturating_sub(poll_end) as u32;
                s.reply_us_last = reply;
                s.reply_us_max = s.reply_us_max.max(reply);
                s.reply_us_sum += u64::from(reply);
                if let Some(t) = e.last_ok_us {
                    s.wait_us_max = s.wait_us_max.max(now.saturating_sub(t) as u32);
                }
                e.last_ok_us = Some(now);
                if seq == 0 {
                    s.empty += 1;
                } else if seq == e.last_seq {
                    s.dups += 1;
                } else {
                    e.last_seq = seq;
                    let (mac, addr) = (e.mac, e.addr);
                    let mut start = 0;
                    for end in 0..body {
                        if self.buf[end] == 0 {
                            s.frames += 1;
                            emit(Event::Frame { mac, addr, bytes: &self.buf[start..=end] });
                            start = end + 1;
                        }
                    }
                    emit(Event::AnswerEnd { mac, addr });
                }
                e.misses = 0;
                if e.absent {
                    e.absent = false;
                    emit(Event::Back { mac: e.mac, addr: e.addr });
                }
                e.due_us = if more { now } else { poll_us + u64::from(cfg.cycle_us) };
                self.last_good_us = Some(now);
                self.phase = Phase::Idle { quiet_until: now };
            }
            None => {
                e.stats.corrupt += 1;
                // The node resends; ask again after the guard.
                e.due_us = now + u64::from(cfg.guard_us);
                self.phase = Phase::Idle { quiet_until: now + u64::from(cfg.guard_us) };
            }
        }
        self.reset_buf();
    }

    fn join_bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            if b != 0 {
                self.push(b);
                continue;
            }
            if self.len == 0 {
                continue;
            }
            let chunk = &self.buf[..self.len];
            let ok = match (chunk_tag(chunk), self.overflow) {
                (Some(TAG_BUS_JOIN), false) => match decode_small(chunk) {
                    Some(Frame::BusJoin { mac, want, check }) if check == crc::join_check(&mac, want) => Some((mac, want)),
                    _ => None,
                },
                _ => None,
            };
            match ok {
                Some((mac, want)) => {
                    self.stats.joins_heard += 1;
                    if !self.joins.iter().any(|j| j.0 == mac) {
                        let _ = self.joins.push((mac, want));
                    }
                }
                None => self.stats.join_corrupt += 1,
            }
            self.reset_buf();
        }
    }

    fn stray_bytes(&mut self, now: u64, bytes: &[u8]) {
        self.stats.stray_bytes = self.stats.stray_bytes.wrapping_add(bytes.len() as u32);
        // Someone is still talking (a node that answered too late): keep the
        // line quiet until it stops, so it cannot run into the next answer.
        let guard = u64::from(self.cfg.guard_us);
        if let Phase::Idle { quiet_until } = &mut self.phase {
            *quiet_until = (*quiet_until).max(now + guard);
        }
        let mut frames = 0;
        self.stray.feed(bytes, |r| frames += u32::from(r.is_ok()));
        if frames > 0 {
            self.stats.stray_frames += frames;
            if now.saturating_sub(self.stray_window.0) > 2_000_000 {
                self.stray_window = (now, 0);
            }
            self.stray_window.1 += frames;
        }
    }

    /// Timeouts, the end of a join window, and the next poll or join slot.
    /// `tx_busy_us`: how long until bytes written now start on the wire (the
    /// hub's TX queue). Returns bytes to send on the hub's TX.
    pub fn tick(&mut self, now: u64, tx_busy_us: u64, emit: &mut impl FnMut(Event<'_>)) -> Option<&[u8]> {
        if !self.started {
            self.started = true;
            self.fast_join_until = now + u64::from(self.cfg.join_fast_for_us);
            self.next_join_us = now;
        }
        let cfg = self.cfg;
        match self.phase {
            Phase::Answer { idx, poll_us, deadline, first, .. } if now >= deadline => {
                self.stats.busy_us += now.saturating_sub(poll_us);
                if let Some(e) = self.table.get_mut(idx) {
                    if first.is_none() && self.len == 0 {
                        e.stats.timeouts += 1;
                        e.misses = e.misses.saturating_add(1);
                        if e.misses >= cfg.absent_after && !e.absent {
                            e.absent = true;
                            emit(Event::Absent { mac: e.mac, addr: e.addr });
                        }
                        e.due_us = if e.absent { now + u64::from(cfg.absent_poll_us) } else { poll_us + u64::from(cfg.cycle_us) };
                    } else {
                        // Started but never finished: cut short or garbled.
                        e.stats.corrupt += 1;
                        e.due_us = now + u64::from(cfg.guard_us);
                    }
                }
                self.reset_buf();
                self.phase = Phase::Idle { quiet_until: now + u64::from(cfg.guard_us) };
            }
            Phase::Join { start, until } if now >= until => {
                self.stats.busy_us += now.saturating_sub(start);
                self.reset_buf();
                self.phase = Phase::Idle { quiet_until: now };
                if !self.joins.is_empty() {
                    self.fast_join_until = now + u64::from(cfg.join_fast_for_us);
                }
                let mut n = 0;
                let joins = core::mem::take(&mut self.joins);
                for (mac, want) in joins {
                    let Some((addr, again)) = self.grant(now, mac, want) else { continue };
                    self.stats.grants += 1;
                    let cmd = Command::Address { mac, addr, epoch: self.epoch, check: crc::address_check(&mac, addr, self.epoch) };
                    let Ok(w) = link::encode_command(&cmd, &mut self.tx[n..]) else { break };
                    n += w.len();
                    emit(Event::Joined { mac, addr, again });
                }
                if n > 0 {
                    self.stats.tx_bytes += n as u32;
                    return Some(&self.tx[..n]);
                }
            }
            _ => {}
        }
        let Phase::Idle { quiet_until } = self.phase else { return None };
        if now < quiet_until {
            return None;
        }
        if now >= self.next_join_us {
            let fast = self.table.is_empty() || now < self.fast_join_until;
            self.next_join_us = now + u64::from(if fast { cfg.join_fast_us } else { cfg.join_every_us });
            let cmd = Command::JoinSlot { epoch: self.epoch, slots: cfg.join_slots, slot_us: cfg.join_slot_us };
            let n = link::encode_command(&cmd, &mut self.tx).ok()?.len();
            let end = now + tx_busy_us + wire_us(n, cfg.baud);
            let window = u64::from(cfg.join_slots) * u64::from(cfg.join_slot_us) + u64::from(cfg.first_byte_us);
            self.phase = Phase::Join { start: now, until: end + window };
            self.reset_buf();
            self.stats.join_slots += 1;
            self.stats.tx_bytes += n as u32;
            return Some(&self.tx[..n]);
        }
        let idx = self.pick(now)?;
        let budget = self.budget();
        let e = &mut self.table[idx];
        let cmd = Command::Poll { addr: e.addr, epoch: self.epoch, ack: e.last_seq, budget };
        let n = link::encode_command(&cmd, &mut self.tx).ok()?.len();
        let end = now + tx_busy_us + wire_us(n, cfg.baud);
        let longest = usize::from(budget).max(MAX_UNIT) + MAX_DONE + 32;
        let hard = end + u64::from(cfg.first_byte_us) + wire_us(longest, cfg.baud) + u64::from(cfg.gap_us);
        e.stats.polls += 1;
        e.last_poll_us = now;
        // Not due again until it answers (or times out).
        e.due_us = u64::MAX;
        self.rr = idx;
        self.phase = Phase::Answer { idx, poll_us: now, poll_end: end, deadline: end + u64::from(cfg.first_byte_us), hard, first: None };
        self.reset_buf();
        self.stats.polls += 1;
        self.stats.tx_bytes += n as u32;
        Some(&self.tx[..n])
    }

    /// The node to poll: the earliest due at or before `now`, ties going
    /// round robin from the last one polled.
    fn pick(&self, now: u64) -> Option<usize> {
        let n = self.table.len();
        let mut best: Option<(u64, usize)> = None;
        for k in 1..=n {
            let i = (self.rr + k) % n;
            let due = self.table[i].due_us;
            if due <= now && best.is_none_or(|(d, _)| due < d) {
                best = Some((due, i));
            }
        }
        best.map(|(_, i)| i)
    }

    /// The address for `mac`: the one it had, else the one it asks for if
    /// free, else the lowest free one. A full table makes room by forgetting
    /// the absent node heard from longest ago.
    fn grant(&mut self, now: u64, mac: [u8; 6], want: u8) -> Option<(u8, bool)> {
        if let Some(e) = self.table.iter_mut().find(|e| e.mac == mac) {
            e.last_seq = 0;
            e.misses = 0;
            e.absent = false;
            e.due_us = now;
            e.stats.joins += 1;
            return Some((e.addr, true));
        }
        if self.table.is_full() {
            let oldest = self
                .table
                .iter()
                .enumerate()
                .filter(|(_, e)| e.absent)
                .min_by_key(|(_, e)| e.last_ok_us.unwrap_or(0))
                .map(|(i, _)| i);
            match oldest {
                Some(i) => {
                    self.table.swap_remove(i);
                }
                None => {
                    self.stats.join_full += 1;
                    return None;
                }
            }
        }
        let used = |a: u8| self.table.iter().any(|e| e.addr == a);
        let addr = if (1..=MAX_ADDR).contains(&want) && !used(want) { want } else { (1..=MAX_ADDR).find(|&a| !used(a))? };
        let entry = Entry {
            mac,
            addr,
            absent: false,
            stats: NodeStats { joins: 1, ..NodeStats::default() },
            last_seq: 0,
            due_us: now,
            misses: 0,
            last_ok_us: None,
            last_poll_us: now,
        };
        self.table.push(entry).ok()?;
        Some((addr, false))
    }
}

/// Decode one short COBS frame (a `BusDone` or `BusJoin`); `None` if it is
/// too long to be one or does not decode.
fn decode_small(chunk: &[u8]) -> Option<Frame> {
    let mut tmp = [0u8; MAX_DONE + 8];
    let body = chunk.strip_suffix(&[0]).unwrap_or(chunk);
    let t = tmp.get_mut(..body.len())?;
    t.copy_from_slice(body);
    postcard::from_bytes_cobs::<Frame>(t).ok()
}
