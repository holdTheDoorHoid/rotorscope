//! A node's side: its own keys, session and counter, and what it knows of
//! the hub's epoch and clock (the rules are in the module docs of `v3`).

use super::{
    BEACON_FIXED, BEACON_TAG_LEN, DOWN_SLACK, FLOOR_TICKS_PER_S, Kind, LinkKey, MAX_PACKET, Mac, N_REJECT, NodeKey,
    RELINK_MS, Reject, Shape, TICK_MS, TIMESYNC_SLACK, TRAILER, Verdict, beacon_tag, ct_eq, header, inspect,
    read_stamp, seal, split, verify12,
};
use super::Stamp;
use crate::HEADER;

/// A node's identity on the link and its two keys.
#[derive(Debug)]
pub struct NodeKeys {
    slot: u8,
    mac: Mac,
    up: LinkKey,
    down: LinkKey,
}

impl NodeKeys {
    /// From the node's record: its roster slot, its station MAC (checked
    /// against eFuse by the caller) and its node key.
    pub fn new(slot: u8, mac: Mac, key: &NodeKey) -> Self {
        let (up, down) = split(key);
        NodeKeys { slot, mac, up, down }
    }

    pub fn slot(&self) -> u8 {
        self.slot
    }

    pub fn mac(&self) -> Mac {
        self.mac
    }
}

/// What the node knows of the hub in its current epoch.
#[derive(Debug, Clone, Copy, Default)]
struct Link {
    epoch: Option<u32>,
    /// An echoed packet of this epoch was taken (otherwise it came from a beacon).
    proven: bool,
    /// `(ticks, node ms)`: the floor's base.
    floor: Option<(u32, u64)>,
    /// `(hub clock, node ms)`: the estimate's base.
    est: Option<(u32, u64)>,
    /// The last clock stamped on an uplink in this epoch.
    last_est: u32,
    /// `(echoed counter, clock)` of the newest echoed packet taken.
    last_echo: Option<(u32, u32)>,
}

/// A hub packet the node verified and took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Down<'a> {
    pub kind: Kind,
    pub seq: u16,
    pub stamp: Stamp,
    /// Sync and Beacon: the gateway's channel.
    pub channel: Option<u8>,
    /// Command: the COBS command frames (see [`super::frames`]).
    pub payload: &'a [u8],
}

/// Outcome of [`Node::accept`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeVerdict<'a> {
    Accept(Down<'a>),
    /// The last echoed packet taken, again (a retry): drop quietly.
    Duplicate,
    Reject(Reject),
}

impl NodeVerdict<'_> {
    pub fn verdict(&self) -> Verdict {
        match self {
            NodeVerdict::Accept(_) => Verdict::Accept,
            NodeVerdict::Duplicate => Verdict::Duplicate,
            NodeVerdict::Reject(r) => Verdict::Reject(*r),
        }
    }

    /// Whether the packet was genuine (made by the hub for this node), so the
    /// node may learn or keep the gateway address from it: taken, or refused
    /// only for its epoch. Sending to that gateway earns the Sync that puts
    /// the node right.
    pub fn proves_gateway(&self) -> bool {
        matches!(self, NodeVerdict::Accept(_) | NodeVerdict::Duplicate | NodeVerdict::Reject(Reject::Stale | Reject::Unproven))
    }
}

/// Counts of hub packets a node did not take, by reason, and duplicates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NodeRejects {
    pub by_reason: [u32; N_REJECT],
    pub dups: u32,
}

impl NodeRejects {
    /// Count `v`; true if the packet is to be used.
    pub fn take(&mut self, v: Verdict) -> bool {
        match v {
            Verdict::Accept => true,
            Verdict::Duplicate => {
                self.dups = self.dups.wrapping_add(1);
                false
            }
            Verdict::Reject(r) => {
                let i = r.index();
                self.by_reason[i] = self.by_reason[i].wrapping_add(1);
                false
            }
        }
    }
}

/// One node's link state.
#[derive(Debug)]
pub struct Node {
    keys: NodeKeys,
    rng: fn() -> u32,
    session: u32,
    counter: u32,
    link: Link,
    /// After a counter wrap: send nothing until the estimate is above this.
    wrap: Option<u32>,
    /// Node ms of the last packet taken (for the relink, rule 5).
    last_accept: Option<u64>,
    relinks: u32,
}

fn nonzero(rng: fn() -> u32) -> u32 {
    loop {
        let v = rng();
        if v != 0 {
            return v;
        }
    }
}

impl Node {
    /// `rng`: the hardware TRNG, started after the radio. Called for each new
    /// session (boot, relink, counter wrap).
    pub fn new(keys: NodeKeys, rng: fn() -> u32) -> Self {
        Node { keys, rng, session: nonzero(rng), counter: 0, link: Link::default(), wrap: None, last_accept: None, relinks: 0 }
    }

    pub fn keys(&self) -> &NodeKeys {
        &self.keys
    }

    pub fn session(&self) -> u32 {
        self.session
    }

    /// The hub epoch the node follows, once it has one.
    pub fn epoch(&self) -> Option<u32> {
        self.link.epoch
    }

    /// Whether that epoch is proven by an echoed packet (not only a beacon).
    pub fn proven(&self) -> bool {
        self.link.proven
    }

    /// New sessions started because nothing was taken for [`RELINK_MS`].
    pub fn relinks(&self) -> u32 {
        self.relinks
    }

    /// The floor at node uptime `now_ms`: the hub clock the node knows has
    /// passed. `None` before it has an epoch.
    pub fn floor(&self, now_ms: u64) -> Option<u64> {
        let (f, at) = self.link.floor?;
        Some(u64::from(f) + now_ms.saturating_sub(at) * FLOOR_TICKS_PER_S / 1000)
    }

    /// The node's estimate of the hub clock at `now_ms` (before the
    /// never-decreasing rule): the highest accepted clock plus one tick per 10 ms.
    pub fn estimate(&self, now_ms: u64) -> Option<u64> {
        let (c, at) = self.link.est?;
        Some(u64::from(c) + now_ms.saturating_sub(at) / TICK_MS)
    }

    fn late(&self, clock: u32, now_ms: u64, slack: u32) -> bool {
        self.floor(now_ms).is_some_and(|f| u64::from(clock) + u64::from(slack) < f)
    }

    /// Raise floor and estimate for a taken packet with `clock`.
    fn heard(&mut self, clock: u32, now_ms: u64) {
        let f = self.floor(now_ms).unwrap_or(0).max(u64::from(clock)).min(u64::from(u32::MAX)) as u32;
        self.link.floor = Some((f, now_ms));
        if self.estimate(now_ms).is_none_or(|e| u64::from(clock) >= e) {
            self.link.est = Some((clock, now_ms));
        }
    }

    /// Start following `epoch` from a packet with `clock`.
    fn enter(&mut self, epoch: u32, proven: bool, clock: u32, now_ms: u64) {
        self.link = Link {
            epoch: Some(epoch),
            proven,
            floor: Some((clock, now_ms)),
            est: Some((clock, now_ms)),
            last_est: 0,
            last_echo: None,
        };
    }

    /// Rule 5: after [`RELINK_MS`] with nothing taken, start over as if just booted.
    fn relink_if_idle(&mut self, now_ms: u64) {
        if self.last_accept.is_some_and(|t| now_ms.saturating_sub(t) >= RELINK_MS) {
            self.session = nonzero(self.rng);
            self.counter = 0;
            self.link = Link::default();
            self.wrap = None;
            self.last_accept = None;
            self.relinks = self.relinks.wrapping_add(1);
        }
    }

    #[cfg(test)]
    pub(super) fn test_set_counter(&mut self, c: u32) {
        self.counter = c;
    }

    /// Stamp for the next uplink transmission (every retry is a new one).
    /// `None` while the node has no epoch, or after a counter wrap until its
    /// estimate has moved past the old one.
    pub fn next_up(&mut self, now_ms: u64) -> Option<Stamp> {
        self.relink_if_idle(now_ms);
        let epoch = self.link.epoch?;
        let est = self.estimate(now_ms)?.min(u64::from(u32::MAX)) as u32;
        let est = est.max(self.link.last_est);
        if let Some(f) = self.wrap {
            if est <= f {
                return None;
            }
            self.wrap = None;
        }
        self.link.last_est = est;
        let s = Stamp { epoch, clock: est, session: self.session, counter: self.counter };
        if self.counter == u32::MAX {
            self.session = nonzero(self.rng);
            self.counter = 0;
            self.link.last_echo = None;
            self.wrap = Some(est);
        } else {
            self.counter += 1;
        }
        Some(s)
    }

    /// Seal a Frames packet: `buf[HEADER..len]` holds the COBS frames (at
    /// least one byte); the header is written here. Call on every retry.
    /// Returns the sealed length; `None` if it does not fit or the node may
    /// not send yet ([`Node::next_up`]).
    pub fn seal_frames(&mut self, buf: &mut [u8], len: usize, seq: u16, now_ms: u64) -> Option<usize> {
        if len <= HEADER || len + TRAILER > buf.len() || len + TRAILER > MAX_PACKET {
            return None;
        }
        let stamp = self.next_up(now_ms)?;
        header(buf, Kind::Frames, seq);
        seal(&self.keys.up, &self.keys.mac, buf, len, &stamp)
    }

    /// The whole receive check for one packet heard from `src` (the radio's
    /// source address) at node uptime `now_ms`. `timesync`: the packet is a
    /// Command holding a `TimeSync` frame (the caller looks at the frames
    /// first; nothing is trusted until this returns `Accept`).
    pub fn accept<'a>(&mut self, src: &Mac, data: &'a [u8], now_ms: u64, timesync: bool) -> NodeVerdict<'a> {
        self.relink_if_idle(now_ms);
        let shape = match inspect(data) {
            Ok(s) => s,
            Err(r) => return NodeVerdict::Reject(r),
        };
        let v = match shape.kind {
            Kind::Beacon => self.beacon(src, data, &shape, now_ms),
            Kind::Sync | Kind::Command => self.unicast(src, data, &shape, now_ms, timesync && shape.kind == Kind::Command),
            Kind::Frames | Kind::Unprovisioned => NodeVerdict::Reject(Reject::Malformed),
        };
        if matches!(v, NodeVerdict::Accept(_)) {
            self.last_accept = Some(now_ms);
        }
        v
    }

    fn beacon<'a>(&mut self, src: &Mac, data: &'a [u8], shape: &Shape, now_ms: u64) -> NodeVerdict<'a> {
        let slot = usize::from(self.keys.slot);
        if slot >= usize::from(shape.slots) {
            return NodeVerdict::Reject(Reject::NotForThisSlot);
        }
        let at = BEACON_FIXED + slot * BEACON_TAG_LEN;
        let got = &data[at..at + BEACON_TAG_LEN];
        if got.iter().all(|&b| b == 0) {
            return NodeVerdict::Reject(Reject::NotForThisSlot);
        }
        let Some(s) = read_stamp(data, shape) else { return NodeVerdict::Reject(Reject::Malformed) };
        if s.session != 0 || s.counter != 0 {
            return NodeVerdict::Reject(Reject::Malformed);
        }
        if !ct_eq(&beacon_tag(&self.keys.down, src, &data[..BEACON_FIXED]), got) {
            return NodeVerdict::Reject(Reject::BadTag);
        }
        match self.link.epoch {
            None => self.enter(s.epoch, false, s.clock, now_ms),
            Some(e) if s.epoch == e => {
                if self.late(s.clock, now_ms, DOWN_SLACK) {
                    return NodeVerdict::Reject(Reject::Late);
                }
                self.heard(s.clock, now_ms);
            }
            Some(e) if s.epoch > e => {
                if self.link.proven {
                    return NodeVerdict::Reject(Reject::Unproven);
                }
                self.enter(s.epoch, false, s.clock, now_ms);
            }
            Some(_) => return NodeVerdict::Reject(Reject::Stale),
        }
        NodeVerdict::Accept(Down { kind: Kind::Beacon, seq: shape.seq, stamp: s, channel: Some(data[HEADER]), payload: &[] })
    }

    fn unicast<'a>(&mut self, src: &Mac, data: &'a [u8], shape: &Shape, now_ms: u64, timesync: bool) -> NodeVerdict<'a> {
        if !verify12(&self.keys.down, src, data, shape) {
            return NodeVerdict::Reject(Reject::BadTag);
        }
        let Some(s) = read_stamp(data, shape) else { return NodeVerdict::Reject(Reject::Malformed) };
        if s.session != self.session {
            return NodeVerdict::Reject(Reject::Replay);
        }
        let sync = shape.kind == Kind::Sync;
        let k = (s.counter, s.clock);
        match self.link.epoch {
            Some(e) if s.epoch == e => {
                match self.link.last_echo {
                    Some(l) if k == l => return NodeVerdict::Duplicate,
                    Some(l) if k < l => return NodeVerdict::Reject(Reject::Replay),
                    _ => {}
                }
                if timesync && !self.link.proven {
                    return NodeVerdict::Reject(Reject::Unproven);
                }
                let slack = if timesync { TIMESYNC_SLACK } else { DOWN_SLACK };
                if self.late(s.clock, now_ms, slack) {
                    return NodeVerdict::Reject(Reject::Late);
                }
                self.link.proven = true;
                self.heard(s.clock, now_ms);
            }
            // Rules 1, 3, 4: only a Sync moves the node to another epoch: up
            // always, down only from an unproven (beacon-learned) epoch.
            None => {
                if !sync {
                    return NodeVerdict::Reject(Reject::Unproven);
                }
                self.enter(s.epoch, true, s.clock, now_ms);
            }
            Some(e) if s.epoch > e => {
                if !sync {
                    return NodeVerdict::Reject(Reject::Unproven);
                }
                self.enter(s.epoch, true, s.clock, now_ms);
            }
            Some(_) => {
                if !sync || self.link.proven {
                    return NodeVerdict::Reject(Reject::Stale);
                }
                self.enter(s.epoch, true, s.clock, now_ms);
            }
        }
        self.link.last_echo = Some(k);
        let (channel, payload) = if sync {
            (Some(data[HEADER]), &[][..])
        } else {
            (None, &data[HEADER..HEADER + shape.payload_len])
        };
        NodeVerdict::Accept(Down { kind: shape.kind, seq: shape.seq, stamp: s, channel, payload })
    }
}
