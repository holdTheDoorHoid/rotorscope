//! A radio node's end of the bus.
//!
//! The node listens to every command the hub sends and talks only when told
//! to: when polled by its own address, or in a join slot it picked while it
//! has no address. Drive it with:
//! - [`Node::on_rx`] with bytes from the hub. If the last command in them
//!   was a poll for this node, it returns the answer to send *now*. The
//!   firmware first checks that no newer byte has arrived meanwhile (feeding
//!   it to `on_rx` again, which cancels the answer if the hub has moved on),
//!   so a node that was too slow stays silent instead of talking over the
//!   next node.
//! - [`Node::tick`] at [`Node::next_deadline`]: sends a join answer in its
//!   slot, and drops an address the hub has stopped polling.
//!
//! Frames to send come from an [`Outbox`] of *units*: one or more whole COBS
//! frames that must travel together (a single frame, or a gateway's `Via`
//! with its frame). An answer holds whole units only, up to the poll's byte
//! budget, and always at least one if any is waiting, so a unit longer than
//! the budget still goes and no frame is ever split between two answers.
//!
//! An answer stays in the node until the hub acknowledges it (the `ack` in
//! the next poll); until then every poll gets the same answer again, so a
//! frame is lost only if the node reboots.

use link::{Assignment, Command, CommandDecoder, Frame};

use crate::{MAX_ADDR, MAX_ANSWER, MAX_DONE, crc};

/// Frames waiting to go to the hub.
pub trait Outbox {
    /// The next unit, without taking it.
    fn peek(&mut self) -> Option<&[u8]>;
    /// Take the unit `peek` returned (it is now in an answer).
    fn pop(&mut self);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeConfig {
    /// Not polled for this long while holding an address: give it up and
    /// join again (the hub restarted, or this node was replaced).
    pub silence_us: u32,
    /// Join backoff: after `k` joins without an answer, skip a random
    /// 0..2^min(k, max) join slots before the next try.
    pub max_backoff_exp: u8,
}

impl NodeConfig {
    pub const fn new() -> Self {
        // The hub asks even an absent node once a second.
        Self { silence_us: 3_000_000, max_backoff_exp: 4 }
    }
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Running totals since boot.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NodeCounters {
    /// Polls for this node (current address and epoch).
    pub polls: u32,
    /// Answers handed out to send, and how many of them were resends.
    pub answers: u32,
    pub resends: u32,
    /// Answers that carried nothing.
    pub empty: u32,
    /// Answers cancelled because a newer command arrived before we could
    /// send (we were too slow; the hub moved on).
    pub stale: u32,
    /// Join answers sent, and addresses received.
    pub joins: u32,
    pub granted: u32,
    /// Addresses given up (hub restarted, silence, taken by another node).
    pub lost: u32,
    /// `Assign`s for this node taken from the bus.
    pub commands: u32,
    /// Command bytes that did not decode.
    pub bad: u32,
    /// Frames and bytes sent in answers (resends counted again).
    pub frames: u32,
    pub bytes: u32,
    /// A unit too large for any answer (cannot happen with `link` frames).
    pub oversize: u32,
}

/// Everything but the decoder, so the decoder's callback can borrow it.
struct Core {
    cfg: NodeConfig,
    mac: [u8; 6],
    rng: u32,
    addr: Option<u8>,
    epoch: Option<u16>,
    /// Last address held (offered back to the hub when joining).
    want: u8,
    to_me: bool,
    /// The answer being offered: its frames (`body` bytes), then the
    /// `BusDone` (`len - body` bytes). Kept until acknowledged.
    out: [u8; MAX_ANSWER],
    body: usize,
    len: usize,
    /// Sequence number of the frames in `out` (0: none held).
    held_seq: u8,
    next_seq: u8,
    /// `out` holds an answer to the latest command: send it now.
    ready: bool,
    join_at: Option<u64>,
    join_sent: bool,
    join_fails: u8,
    join_skip: u8,
    last_polled: u64,
    stats: NodeCounters,
    /// Protocol 11: the latest `TimeSync` (arrival time, UTC ms, PPS), not yet taken.
    time_sync: Option<TimeSyncRx>,
}

/// A [`Command::TimeSync`] as it reached this node: `at_us` is the `now`
/// passed to [`Node::on_rx`] with the bytes that completed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeSyncRx {
    pub at_us: u64,
    pub utc_ms: u64,
    pub pps: bool,
}

pub struct Node {
    dec: CommandDecoder,
    core: Core,
}

impl Node {
    /// `mac`: the station MAC (the same in every image on this board).
    /// `seed`: any random number (join backoff). `want`: an address held
    /// before a restart, if remembered (0: none).
    pub fn new(cfg: NodeConfig, mac: [u8; 6], seed: u32, want: u8) -> Self {
        let mut rng = seed ^ u32::from_le_bytes([mac[2], mac[3], mac[4], mac[5]]);
        if rng == 0 {
            rng = 0x9E37_79B9;
        }
        Self {
            dec: CommandDecoder::new(),
            core: Core {
                cfg,
                mac,
                rng,
                addr: None,
                epoch: None,
                want: if (1..=MAX_ADDR).contains(&want) { want } else { 0 },
                to_me: false,
                out: [0; MAX_ANSWER],
                body: 0,
                len: 0,
                held_seq: 0,
                next_seq: 1,
                ready: false,
                join_at: None,
                join_sent: false,
                join_fails: 0,
                join_skip: 0,
                last_polled: 0,
                stats: NodeCounters::default(),
                time_sync: None,
            },
        }
    }

    pub fn mac(&self) -> [u8; 6] {
        self.core.mac
    }

    /// The bus address held now, if any.
    pub fn addr(&self) -> Option<u8> {
        self.core.addr
    }

    pub fn epoch(&self) -> Option<u16> {
        self.core.epoch
    }

    pub fn counters(&self) -> &NodeCounters {
        &self.core.stats
    }

    /// Frames waiting for an acknowledgement.
    pub fn holding(&self) -> bool {
        self.core.held_seq != 0
    }

    /// Bytes from the hub. Returns the answer to send now, if the newest
    /// command is a poll for this node. `on_assign` gets every `Assign` that
    /// follows a `To` naming this node.
    pub fn on_rx(&mut self, now: u64, bytes: &[u8], outbox: &mut impl Outbox, on_assign: &mut impl FnMut(Assignment)) -> Option<&[u8]> {
        let core = &mut self.core;
        self.dec.feed(bytes, |r| match r {
            Ok(c) => core.command(now, c, outbox, on_assign),
            Err(_) => {
                core.stats.bad += 1;
                core.cancel_ready();
                core.to_me = false;
            }
        });
        self.answer()
    }

    /// The latest `TimeSync` from the hub since the last call, if any.
    /// It is a broadcast: taken with or without a `To` naming this node.
    pub fn take_time_sync(&mut self) -> Option<TimeSyncRx> {
        self.core.time_sync.take()
    }

    /// The answer `on_rx` returned, while it is still current.
    pub fn answer(&self) -> Option<&[u8]> {
        self.core.ready.then(|| &self.core.out[..self.core.len])
    }

    /// The firmware sent the answer (or gave up on it): it is no longer
    /// "ready", but it is kept for a resend until acknowledged.
    pub fn sent(&mut self) {
        let c = &mut self.core;
        if c.ready {
            c.ready = false;
            c.stats.answers += 1;
            c.stats.bytes = c.stats.bytes.wrapping_add(c.len as u32);
            c.stats.frames += c.out[..c.body].iter().filter(|&&b| b == 0).count() as u32;
        }
    }

    /// When [`Self::tick`] has something to do (absolute microseconds).
    pub fn next_deadline(&self) -> Option<u64> {
        let c = &self.core;
        let silence = c.addr.map(|_| c.last_polled + u64::from(c.cfg.silence_us));
        match (c.join_at, silence) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The join answer when its slot comes, and the silence rule. Returns
    /// bytes to send now.
    pub fn tick(&mut self, now: u64) -> Option<&[u8]> {
        let c = &mut self.core;
        if c.addr.is_some() && now.saturating_sub(c.last_polled) > u64::from(c.cfg.silence_us) {
            c.drop_address();
        }
        match c.join_at {
            Some(t) if now >= t && c.addr.is_none() => {
                c.join_at = None;
                c.join_sent = true;
                c.stats.joins += 1;
                let f = Frame::BusJoin { mac: c.mac, want: c.want, check: crc::join_check(&c.mac, c.want) };
                // The join goes out of a scratch area after any held answer,
                // so the held answer survives for its resend.
                let at = c.len.min(c.out.len() - MAX_DONE);
                let n = link::encode(&f, &mut c.out[at..]).ok()?.len();
                Some(&self.core.out[at..at + n])
            }
            _ => None,
        }
    }
}

impl Core {
    fn random(&mut self) -> u32 {
        // xorshift32
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        x
    }

    fn cancel_ready(&mut self) {
        if self.ready {
            self.ready = false;
            self.stats.stale += 1;
        }
    }

    fn drop_address(&mut self) {
        if self.addr.take().is_some() {
            self.stats.lost += 1;
        }
        self.epoch = None;
    }

    fn command(&mut self, now: u64, c: Command, outbox: &mut impl Outbox, on_assign: &mut impl FnMut(Assignment)) {
        // Anything newer than a poll for us means the hub has moved on.
        self.cancel_ready();
        // ...and that our join slot (if any) is over.
        if !matches!(c, Command::JoinSlot { .. }) {
            self.join_at = None;
        }
        let to_me = core::mem::replace(&mut self.to_me, false);
        match c {
            Command::To { node } => self.to_me = node == self.mac,
            Command::Assign(a) => {
                if to_me {
                    self.stats.commands += 1;
                    on_assign(a);
                }
            }
            Command::Poll { addr, epoch, ack, budget } => {
                if self.addr != Some(addr) {
                    return;
                }
                if self.epoch != Some(epoch) {
                    // Our address is from another hub boot.
                    self.drop_address();
                    return;
                }
                self.last_polled = now;
                self.stats.polls += 1;
                self.answer(ack, budget, outbox);
            }
            Command::JoinSlot { epoch, slots, slot_us } => {
                if self.addr.is_some() {
                    if self.epoch == Some(epoch) {
                        return;
                    }
                    self.drop_address();
                }
                if core::mem::take(&mut self.join_sent) {
                    // The last join got no address: a collision, or the grant was lost.
                    self.join_fails = self.join_fails.saturating_add(1);
                    let exp = self.join_fails.min(self.cfg.max_backoff_exp).min(15);
                    self.join_skip = (self.random() % (1u32 << exp)) as u8;
                }
                if self.join_skip > 0 {
                    self.join_skip -= 1;
                    self.join_at = None;
                    return;
                }
                let slot = self.random() % u32::from(slots.max(1));
                // An eighth of a slot in, so a slightly early clock still lands inside it.
                self.join_at = Some(now + u64::from(slot) * u64::from(slot_us) + u64::from(slot_us / 8));
            }
            Command::TimeSync { utc_ms, pps } => {
                self.time_sync = Some(TimeSyncRx { at_us: now, utc_ms, pps });
            }
            Command::Address { mac, addr, epoch, check } => {
                if check != crc::address_check(&mac, addr, epoch) || !(1..=MAX_ADDR).contains(&addr) {
                    self.stats.bad += 1;
                    return;
                }
                if mac == self.mac {
                    if self.addr != Some(addr) || self.epoch != Some(epoch) {
                        self.stats.granted += 1;
                    }
                    self.addr = Some(addr);
                    self.epoch = Some(epoch);
                    self.want = addr;
                    self.join_at = None;
                    self.join_sent = false;
                    self.join_fails = 0;
                    self.join_skip = 0;
                    self.last_polled = now;
                } else if self.addr == Some(addr) && self.epoch == Some(epoch) {
                    // The hub gave our address to someone else (it forgot us).
                    self.drop_address();
                }
            }
        }
    }

    /// Build (or rebuild) the answer to a poll.
    fn answer(&mut self, ack: u8, budget: u16, outbox: &mut impl Outbox) {
        if self.held_seq != 0 && ack == self.held_seq {
            self.held_seq = 0; // the hub has it
        }
        let resend = self.held_seq != 0;
        if !resend {
            self.body = 0;
            let budget = usize::from(budget).min(self.out.len() - MAX_DONE);
            while let Some(unit) = outbox.peek() {
                let n = unit.len();
                if n > self.out.len() - MAX_DONE {
                    self.stats.oversize += 1;
                    outbox.pop();
                    continue;
                }
                if self.body > 0 && self.body + n > budget {
                    break;
                }
                self.out[self.body..self.body + n].copy_from_slice(unit);
                self.body += n;
                outbox.pop();
            }
            if self.body > 0 {
                self.held_seq = self.next_seq;
                self.next_seq = if self.next_seq == u8::MAX { 1 } else { self.next_seq + 1 };
            }
        } else {
            self.stats.resends += 1;
        }
        let Some(addr) = self.addr else { return };
        let seq = self.held_seq;
        if seq == 0 {
            self.stats.empty += 1;
        }
        let more = outbox.peek().is_some();
        let done = Frame::BusDone { addr, seq, more, crc: crc::done_crc(&self.out[..self.body], addr, seq, more) };
        let Ok(w) = link::encode(&done, &mut self.out[self.body..]) else { return };
        self.len = self.body + w.len();
        self.ready = true;
    }
}
