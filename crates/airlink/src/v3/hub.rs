//! The hub's side: per-slot keys, the tick clock, per-slot uplink order.

use super::{
    BEACON_FIXED, BEACON_TAG_LEN, Kind, LinkKey, MAX_PAYLOAD, Mac, NodeKey, Reject, SLOTS, STAMP_LEN,
    TICK_MS, UPLINK_MAX_AGE, UPLINK_MAX_AHEAD, Verdict, beacon_len, beacon_tag, header, inspect, read_stamp, seal,
    split, verify12,
};
use super::Stamp;
use crate::HEADER;

/// What the hub knows about one slot's uplink.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Peer {
    /// `(clock, counter)` of the newest accepted batch.
    last: Option<(u32, u32)>,
    /// `(session, counter)` of the newest accepted batch: what a Command to
    /// this node echoes. `None` until the node is heard this hub boot.
    pub echo: Option<(u32, u32)>,
}

impl Peer {
    /// Freshness of a batch at hub clock `now` in hub epoch `epoch`, without
    /// changing anything (it runs before the tag is checked). Order comes
    /// before age: a batch below the last accepted pair is a replay whatever
    /// its age.
    pub fn check(&self, epoch: u32, now: u32, s: &Stamp) -> Verdict {
        if s.epoch != epoch {
            return Verdict::Reject(Reject::Stale);
        }
        let k = (s.clock, s.counter);
        match self.last {
            Some(l) if k == l => return Verdict::Duplicate,
            Some(l) if k < l => return Verdict::Reject(Reject::Replay),
            _ => {}
        }
        if u64::from(s.clock) > u64::from(now) + u64::from(UPLINK_MAX_AHEAD) {
            return Verdict::Reject(Reject::FutureUplink);
        }
        if now.saturating_sub(s.clock) > UPLINK_MAX_AGE {
            return Verdict::Reject(Reject::LateUplink);
        }
        Verdict::Accept
    }

    fn commit(&mut self, s: &Stamp) {
        self.last = Some((s.clock, s.counter));
        self.echo = Some((s.session, s.counter));
    }
}

/// A batch the hub verified and took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Uplink<'a> {
    /// The roster slot of the sender: the verified source.
    pub slot: u8,
    pub seq: u16,
    pub stamp: Stamp,
    /// The COBS frames.
    pub payload: &'a [u8],
}

/// Outcome of [`Hub::open_frames`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HubVerdict<'a> {
    Accept(Uplink<'a>),
    /// The last accepted batch again (a radio retry). The tag was not checked.
    Duplicate { slot: u8 },
    Reject {
        reason: Reject,
        /// The roster slot of the claimed sender, when it is in the roster.
        slot: Option<u8>,
        /// The packet's stamp when its tag was good (a genuine packet refused
        /// for freshness): what a Sync answer echoes.
        genuine: Option<Stamp>,
    },
}

impl HubVerdict<'_> {
    pub fn verdict(&self) -> Verdict {
        match self {
            HubVerdict::Accept(_) => Verdict::Accept,
            HubVerdict::Duplicate { .. } => Verdict::Duplicate,
            HubVerdict::Reject { reason, .. } => Verdict::Reject(*reason),
        }
    }

    /// `(slot, (session, counter))` to echo in a Sync, for a genuine batch
    /// refused for its epoch or its age: the node is following the wrong hub
    /// boot, or its clock estimate is off. Rate-limit the answers.
    pub fn wants_sync(&self) -> Option<(u8, (u32, u32))> {
        match *self {
            HubVerdict::Reject {
                reason: Reject::Stale | Reject::LateUplink | Reject::FutureUplink,
                slot: Some(slot),
                genuine: Some(s),
            } => Some((slot, (s.session, s.counter))),
            _ => None,
        }
    }
}

/// Why the hub could not seal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SealError {
    /// The gateway's MAC is not known yet (from its Hello).
    NoGateway,
    /// The slot is out of range or holds no node.
    NoSlot,
    /// The roster is empty: no beacon to make.
    NoSlots,
    /// The node has not been heard this hub boot: nothing to echo.
    NotHeard,
    /// `buf` is too small.
    BufferTooSmall,
    /// Commands must be one or more whole COBS frames.
    NotWholeFrames,
    /// More than [`MAX_PAYLOAD`] bytes of commands.
    TooBig,
    /// The clock has run out (about 497 days): restart for a new epoch.
    ClockExhausted,
}

#[derive(Debug)]
struct Slot {
    mac: Mac,
    up: LinkKey,
    down: LinkKey,
    peer: Peer,
}

/// The hub: its epoch and clock, the gateway's MAC, and per roster slot the
/// node's MAC and keys (derived once at boot, held as keyed HMAC states).
#[derive(Debug)]
pub struct Hub {
    epoch: u32,
    prev: Option<u32>,
    gateway: Option<Mac>,
    slots: [Option<Slot>; SLOTS],
}

impl Hub {
    /// `epoch`: this boot's value of the `rsboot` counter.
    pub fn new(epoch: u32) -> Self {
        Hub { epoch, prev: None, gateway: None, slots: core::array::from_fn(|_| None) }
    }

    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    /// The gateway's station MAC, from its Hello: the sender address of
    /// everything the hub seals.
    pub fn set_gateway(&mut self, mac: Mac) {
        self.gateway = Some(mac);
    }

    pub fn gateway(&self) -> Option<Mac> {
        self.gateway
    }

    /// Put a node in `slot` (from the roster and its derived key). Its order
    /// state starts over. False if the slot is out of range or another slot
    /// already has this MAC.
    pub fn set_slot(&mut self, slot: u8, mac: Mac, key: &NodeKey) -> bool {
        let i = usize::from(slot);
        if i >= SLOTS || self.slots.iter().enumerate().any(|(j, s)| j != i && s.as_ref().is_some_and(|s| s.mac == mac)) {
            return false;
        }
        let (up, down) = split(key);
        self.slots[i] = Some(Slot { mac, up, down, peer: Peer::default() });
        true
    }

    /// Empty or revoke `slot`: its keys are dropped, its beacon tag is zero.
    pub fn clear_slot(&mut self, slot: u8) {
        if let Some(s) = self.slots.get_mut(usize::from(slot)) {
            *s = None;
        }
    }

    /// The slot of the node with this MAC.
    pub fn slot_of(&self, mac: &Mac) -> Option<u8> {
        self.slots.iter().position(|s| s.as_ref().is_some_and(|s| &s.mac == mac)).map(|i| i as u8)
    }

    /// What a Command to `slot` would echo.
    pub fn echo(&self, slot: u8) -> Option<(u32, u32)> {
        self.slot(slot).and_then(|s| s.peer.echo)
    }

    fn slot(&self, slot: u8) -> Option<&Slot> {
        self.slots.get(usize::from(slot)).and_then(Option::as_ref)
    }

    /// The hub clock at uptime `uptime_ms`, for judging a received batch: at
    /// least every clock it has sealed.
    pub fn now(&self, uptime_ms: u64) -> u32 {
        let t = (uptime_ms / TICK_MS).min(u64::from(u32::MAX)) as u32;
        t.max(self.prev.unwrap_or(0))
    }

    /// The clock for the next sealed packet: `max(previous + 1, uptime_ms / 10)`.
    /// `None` once it has run out.
    pub fn next_clock(&mut self, uptime_ms: u64) -> Option<u32> {
        let t = u32::try_from(uptime_ms / TICK_MS).ok()?;
        let c = match self.prev {
            None => t,
            Some(p) => p.checked_add(1)?.max(t),
        };
        self.prev = Some(c);
        Some(c)
    }

    fn stamp(&mut self, uptime_ms: u64, echo: (u32, u32)) -> Result<Stamp, SealError> {
        let clock = self.next_clock(uptime_ms).ok_or(SealError::ClockExhausted)?;
        Ok(Stamp { epoch: self.epoch, clock, session: echo.0, counter: echo.1 })
    }

    /// Open a batch heard from `src` (the radio's source address) at hub
    /// uptime `uptime_ms`. Cheap checks first (version, kind, size, roster,
    /// order); the tag is checked before anything changes, and also for a
    /// batch refused for its epoch or age, so a genuine one can be answered
    /// with a Sync ([`HubVerdict::wants_sync`]) and a forged one is counted as
    /// [`Reject::BadTag`].
    pub fn open_frames<'a>(&mut self, src: &Mac, data: &'a [u8], uptime_ms: u64) -> HubVerdict<'a> {
        let reject = |reason, slot| HubVerdict::Reject { reason, slot, genuine: None };
        let shape = match inspect(data) {
            Ok(s) if s.kind == Kind::Frames => s,
            Ok(_) => return reject(Reject::Malformed, None),
            Err(r) => return reject(r, None),
        };
        let Some(slot) = self.slot_of(src) else { return reject(Reject::NotInRoster, None) };
        let now = self.now(uptime_ms);
        let epoch = self.epoch;
        let s = self.slots[usize::from(slot)].as_mut().expect("slot_of found it");
        let Some(stamp) = read_stamp(data, &shape) else { return reject(Reject::Malformed, Some(slot)) };
        let v = s.peer.check(epoch, now, &stamp);
        match v {
            Verdict::Duplicate => return HubVerdict::Duplicate { slot },
            Verdict::Reject(Reject::Replay) => return reject(Reject::Replay, Some(slot)),
            _ => {}
        }
        if !verify12(&s.up, src, data, &shape) {
            return reject(Reject::BadTag, Some(slot));
        }
        match v {
            Verdict::Accept => {
                s.peer.commit(&stamp);
                let payload = &data[HEADER..HEADER + shape.payload_len];
                HubVerdict::Accept(Uplink { slot, seq: shape.seq, stamp, payload })
            }
            Verdict::Reject(reason) => HubVerdict::Reject { reason, slot: Some(slot), genuine: Some(stamp) },
            Verdict::Duplicate => unreachable!(),
        }
    }

    /// Seal a Sync for `slot` echoing `echo` (the `(session, counter)` of the
    /// node's refused batch, from [`HubVerdict::wants_sync`]).
    pub fn seal_sync(&mut self, slot: u8, echo: (u32, u32), buf: &mut [u8], seq: u16, channel: u8, uptime_ms: u64) -> Result<usize, SealError> {
        let gw = self.gateway.ok_or(SealError::NoGateway)?;
        self.slot(slot).ok_or(SealError::NoSlot)?;
        if buf.len() < super::SYNC_LEN {
            return Err(SealError::BufferTooSmall);
        }
        let stamp = self.stamp(uptime_ms, echo)?;
        header(buf, Kind::Sync, seq);
        buf[HEADER] = channel;
        let s = self.slot(slot).expect("checked");
        seal(&s.down, &gw, buf, HEADER + 1, &stamp).ok_or(SealError::BufferTooSmall)
    }

    /// Seal a Command for `slot`: `frames` are whole COBS command frames. It
    /// echoes the node's newest accepted batch, so the node must have been
    /// heard this hub boot.
    pub fn seal_command(&mut self, slot: u8, buf: &mut [u8], seq: u16, frames: &[u8], uptime_ms: u64) -> Result<usize, SealError> {
        let gw = self.gateway.ok_or(SealError::NoGateway)?;
        let echo = self.slot(slot).ok_or(SealError::NoSlot)?.peer.echo.ok_or(SealError::NotHeard)?;
        if frames.last() != Some(&0) {
            return Err(SealError::NotWholeFrames);
        }
        if frames.len() > MAX_PAYLOAD {
            return Err(SealError::TooBig);
        }
        let len = HEADER + frames.len();
        if buf.len() < len + super::TRAILER {
            return Err(SealError::BufferTooSmall);
        }
        let stamp = self.stamp(uptime_ms, echo)?;
        header(buf, Kind::Command, seq);
        buf[HEADER..len].copy_from_slice(frames);
        let s = self.slot(slot).expect("checked");
        seal(&s.down, &gw, buf, len, &stamp).ok_or(SealError::BufferTooSmall)
    }

    /// Seal a broadcast beacon: one 8-byte tag per slot up to the highest
    /// occupied one, each under that slot's `K_down`; zeros for an empty or
    /// revoked slot. Returns its length (`24 + 8n`).
    pub fn seal_beacon(&mut self, buf: &mut [u8], seq: u16, channel: u8, uptime_ms: u64) -> Result<usize, SealError> {
        let gw = self.gateway.ok_or(SealError::NoGateway)?;
        let n = self.slots.iter().rposition(Option::is_some).ok_or(SealError::NoSlots)? + 1;
        let len = beacon_len(n);
        if buf.len() < len {
            return Err(SealError::BufferTooSmall);
        }
        let stamp = self.stamp(uptime_ms, (0, 0))?;
        header(buf, Kind::Beacon, seq);
        buf[HEADER] = channel;
        buf[HEADER + 1] = n as u8;
        stamp.write(&mut buf[HEADER + 2..HEADER + 2 + STAMP_LEN]);
        let (fixed, tags) = buf[..len].split_at_mut(BEACON_FIXED);
        for (i, slot) in self.slots[..n].iter().enumerate() {
            let t = &mut tags[i * BEACON_TAG_LEN..(i + 1) * BEACON_TAG_LEN];
            match slot {
                Some(s) => t.copy_from_slice(&beacon_tag(&s.down, &gw, fixed)),
                None => t.fill(0),
            }
        }
        Ok(len)
    }
}
