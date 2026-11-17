//! Per-packet authentication for the ESP-NOW link (docs/ESPNOW.md, "Securing
//! the link").
//!
//! Without it anyone nearby can send the gateway packets that it relays as one
//! of our nodes (fake drones in the picture, or a real node's reports drowned
//! out), or send a node a fake `Assign` (make it listen somewhere useless).
//! ESP-NOW's own encryption allows only a handful of peers, so every packet
//! instead carries our own tag under one 32-byte key shared by the kit.
//!
//! # Packet
//!
//! A sealed packet is an ordinary `airlink` packet with version
//! [`VERSION_AUTH`] in its header and a trailer after the payload:
//!
//! ```text
//! header (6) | payload | stamp (16: four u32 LE) | tag (12)
//! ```
//!
//! The tag is HMAC-SHA256 under the kit key, truncated to [`TAG_LEN`] bytes,
//! over the sender's MAC address followed by everything before the tag
//! (header, payload, stamp). Covering the sender's address stops a genuine
//! packet from one node being replayed under another node's address. The
//! header's kind is covered, so a node's batch cannot pass as a gateway
//! command or beacon.
//!
//! # Freshness: the [`Stamp`]
//!
//! Four numbers, the same layout both ways:
//!
//! | field | node to gateway | gateway to node |
//! |---|---|---|
//! | `epoch` | the gateway epoch the node last learned | the gateway's epoch: random, new at every gateway boot |
//! | `clock` | the highest gateway `clock` the node has heard | the gateway's packet counter, +1 per packet it sends |
//! | `session` | the node's session: random, new at every node boot | echo of the node's `session`, or 0 (broadcast beacon) |
//! | `counter` | the node's packet counter, +1 per transmission | echo of the node's newest accepted `counter` |
//!
//! **Gateway accepting a node packet** ([`Peer::check`]): the epoch must be
//! the gateway's current one (anything recorded before the gateway's last boot
//! is dead), and `(clock, counter)` must be greater than the last accepted
//! pair from that address. Equal is a harmless duplicate (the radio repeats
//! a frame whose ACK it missed); smaller is a replay.
//!
//! Why `(clock, counter)` stays increasing across a node reboot without any
//! flash: after booting the node can only send once it has heard a gateway
//! packet sent after the boot, whose clock is higher than any clock the node
//! knew before. Within a session the clock never goes down and the counter
//! always goes up.
//!
//! **Node accepting a gateway packet** ([`Node::on_gateway`]): a command (and a
//! "sync", a beacon sent to one node) echoes the node's own random `session`,
//! which proves it was made during this node boot, and `(echoed counter,
//! clock)` must be greater than the last one taken. The echoed counter keeps
//! growing across gateway reboots (it is the node's counter), so a command
//! recorded before a gateway reboot cannot be replayed after it either.
//! A broadcast beacon carries no echo: it only teaches a node that has no
//! gateway yet where to send, and moves the node's clock forward within the
//! epoch it already has. A beacon from another epoch is not believed (it may
//! be a recording); the node keeps using its epoch.
//!
//! **Resync.** A node packet with a valid tag but a stale epoch (the gateway
//! rebooted) or an old pair (the node rebooted and first heard a replayed
//! beacon) makes the gateway answer with a sync: a unicast beacon echoing the
//! session and counter of that packet. The node takes the sync's epoch and
//! clock, and its next packet is accepted. Rate-limited by the gateway.
//!
//! **Wrap.** A node's counter is a u32 (13 years at 10 packets a second).
//! After `u32::MAX` it starts a new session and sends nothing until it has
//! heard a gateway clock above its old one. The gateway's clock runs out
//! after `u32::MAX` packets (68 years at two a second); it then refuses to
//! stamp, and the gateway firmware restarts to take a new epoch.
//!
//! # Cost
//!
//! 28 bytes per packet (about 0.2 ms of air at the 1 Mbit/s default), and one
//! SHA-256 block per 64 bytes signed plus one for the outer hash (the pads are
//! absorbed once when the key is loaded).
//!
//! # What this does not do
//!
//! Nothing is encrypted: detections are what anyone nearby can hear anyway.
//! The key is in every board's flash; a captured board gives it away (make a
//! new key and reflash the kit). Jamming still works; it is detected elsewhere.

use crate::hmac::Hmac;
use crate::{HEADER, Kind, MAGIC, MAX_PACKET, VERSION};

pub const KEY_LEN: usize = 32;
/// Truncated tag: 96 bits. An attacker can only try tags over the air, a few
/// thousand a second at best.
pub const TAG_LEN: usize = 12;
pub const STAMP_LEN: usize = 16;
/// Bytes a sealed packet adds after the payload.
pub const TRAILER: usize = STAMP_LEN + TAG_LEN;
/// Header version of a sealed packet. Version-1 packets are unauthenticated.
pub const VERSION_AUTH: u8 = 2;

// The kit key from the build (see build.rs): `KIT_KEY` and `KEY_ID`.
include!(concat!(env!("OUT_DIR"), "/link_key.rs"));

/// The freshness fields of a sealed packet (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stamp {
    pub epoch: u32,
    pub clock: u32,
    pub session: u32,
    pub counter: u32,
}

impl Stamp {
    fn write(&self, b: &mut [u8]) {
        b[0..4].copy_from_slice(&self.epoch.to_le_bytes());
        b[4..8].copy_from_slice(&self.clock.to_le_bytes());
        b[8..12].copy_from_slice(&self.session.to_le_bytes());
        b[12..16].copy_from_slice(&self.counter.to_le_bytes());
    }

    fn read(b: &[u8]) -> Self {
        let w = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        Stamp { epoch: w(0), clock: w(4), session: w(8), counter: w(12) }
    }
}

/// Why a packet was not taken. The number is what the gateway reports in
/// `link::ThreatKind::ForgedLink`'s `detail`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Reject {
    /// The tag does not match: forged, corrupted, or made with another key.
    BadTag = 1,
    /// A genuine packet seen before (an older counter): a replay.
    Replay = 2,
    /// No tag at all (a version-1 packet) where a key is configured: a node
    /// built without the key, or a forgery that does not even try.
    Unauthenticated = 3,
    /// Genuine but from before the receiver's last boot (another epoch).
    Stale = 4,
}

impl Reject {
    pub const ALL: [Reject; 4] = [Reject::BadTag, Reject::Replay, Reject::Unauthenticated, Reject::Stale];

    pub const fn code(self) -> u32 {
        self as u32
    }

    pub const fn name(self) -> &'static str {
        match self {
            Reject::BadTag => "bad_tag",
            Reject::Replay => "replay",
            Reject::Unauthenticated => "unauthenticated",
            Reject::Stale => "stale",
        }
    }
}

/// Outcome of a freshness check on a packet whose tag was good.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Accept,
    /// Exactly the last accepted packet again: drop quietly, not an attack.
    Duplicate,
    Reject(Reject),
}

/// The kit key, ready to tag (both HMAC pads absorbed). Never printed:
/// `Debug` shows only that it is a key.
#[derive(Clone)]
pub struct Key(Hmac);

impl core::fmt::Debug for Key {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Key(..)")
    }
}

impl Key {
    pub fn new(key: &[u8; KEY_LEN]) -> Self {
        Key(Hmac::new(key))
    }

    /// The build's key, if one was configured.
    pub fn kit() -> Option<Self> {
        KIT_KEY.as_ref().map(Key::new)
    }

    /// Four bytes that identify the key without revealing it (what
    /// [`KEY_ID`] spells in hex): compare them across boards.
    pub fn id(&self) -> [u8; 4] {
        let m = self.0.mac(&[b"rotorscope link key id"]);
        [m[0], m[1], m[2], m[3]]
    }

    fn tag(&self, src: &[u8; 6], signed: &[u8]) -> [u8; TAG_LEN] {
        let m = self.0.mac(&[src, signed]);
        let mut t = [0u8; TAG_LEN];
        t.copy_from_slice(&m[..TAG_LEN]);
        t
    }
}

/// Seal the packet in `buf[..len]` (header and payload, as built by this
/// crate) sent by `src`: set the header's version, append `stamp` and the
/// tag. Returns the sealed length, or `None` if it does not fit.
pub fn seal(key: &Key, src: &[u8; 6], buf: &mut [u8], len: usize, stamp: Stamp) -> Option<usize> {
    let end = len.checked_add(TRAILER)?;
    if len < HEADER || end > buf.len() || end > MAX_PACKET || buf[..2] != MAGIC {
        return None;
    }
    buf[2] = VERSION_AUTH;
    stamp.write(&mut buf[len..len + STAMP_LEN]);
    let tag = key.tag(src, &buf[..len + STAMP_LEN]);
    buf[len + STAMP_LEN..end].copy_from_slice(&tag);
    Some(end)
}

/// Check a received packet's tag (sender `src`, as the radio reports it).
/// Returns its stamp; freshness is checked separately ([`Peer::check`],
/// [`Node::on_gateway`]). Call on anything [`crate::parse`] accepted.
pub fn verify(key: &Key, src: &[u8; 6], data: &[u8]) -> Result<Stamp, Reject> {
    if data.len() >= 3 && data[..2] == MAGIC && data[2] == VERSION {
        return Err(Reject::Unauthenticated);
    }
    if data.len() < HEADER + TRAILER || data[..2] != MAGIC || data[2] != VERSION_AUTH {
        return Err(Reject::BadTag);
    }
    let split = data.len() - TAG_LEN;
    let want = key.tag(src, &data[..split]);
    // Constant time: an attacker must not learn how many bytes matched.
    let diff = want.iter().zip(&data[split..]).fold(0u8, |d, (a, b)| d | (a ^ b));
    if core::hint::black_box(diff) != 0 {
        return Err(Reject::BadTag);
    }
    Ok(Stamp::read(&data[split - STAMP_LEN..split]))
}

/// The gateway's side: its epoch and packet clock.
#[derive(Debug, Clone)]
pub struct Gateway {
    pub key: Key,
    epoch: u32,
    clock: u32,
}

impl Gateway {
    /// `epoch`: random, fresh at every boot (the hardware RNG with the radio on).
    pub fn new(key: Key, epoch: u32) -> Self {
        Gateway { key, epoch, clock: 0 }
    }

    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    pub fn clock(&self) -> u32 {
        self.clock
    }

    /// Stamp for the next packet: a broadcast beacon (`echo` None), or a
    /// command or sync for one node, echoing that node's `(session, counter)`.
    /// `None` once the clock has run out: restart for a new epoch.
    pub fn stamp(&mut self, echo: Option<(u32, u32)>) -> Option<Stamp> {
        if self.clock == u32::MAX {
            return None;
        }
        self.clock += 1;
        let (session, counter) = echo.unwrap_or((0, 0));
        Some(Stamp { epoch: self.epoch, clock: self.clock, session, counter })
    }
}

/// What the gateway knows about one node address.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Peer {
    /// `(clock, counter)` of the newest accepted packet.
    last: Option<(u32, u32)>,
    /// `(session, counter)` of the newest accepted packet: what a command to
    /// this node must echo. `None` until the node has been heard this boot,
    /// and a keyed gateway cannot command a node before that.
    pub echo: Option<(u32, u32)>,
}

impl Peer {
    /// Freshness of a node packet whose tag was good. `epoch` = the gateway's.
    pub fn check(&mut self, epoch: u32, s: &Stamp) -> Verdict {
        if s.epoch != epoch {
            return Verdict::Reject(Reject::Stale);
        }
        let k = (s.clock, s.counter);
        match self.last {
            Some(l) if k == l => Verdict::Duplicate,
            Some(l) if k < l => Verdict::Reject(Reject::Replay),
            _ => {
                self.last = Some(k);
                self.echo = Some((s.session, s.counter));
                Verdict::Accept
            }
        }
    }
}

/// A node's side: its session and counter, and what it knows of the gateway.
#[derive(Debug, Clone)]
pub struct Node {
    pub key: Key,
    rng: fn() -> u32,
    session: u32,
    counter: u32,
    epoch: Option<u32>,
    clock: u32,
    /// `(echoed counter, clock)` of the newest gateway packet that echoed us.
    last_echo: Option<(u32, u32)>,
    /// After a counter wrap: send nothing until the clock is above this.
    floor: Option<u32>,
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
    /// `rng`: the hardware RNG (true random while the radio is on). Called
    /// for each new session (boot, counter wrap).
    pub fn new(key: Key, rng: fn() -> u32) -> Self {
        Node { key, rng, session: nonzero(rng), counter: 0, epoch: None, clock: 0, last_echo: None, floor: None }
    }

    pub fn session(&self) -> u32 {
        self.session
    }

    /// The gateway epoch the node is using, once it has heard one.
    pub fn epoch(&self) -> Option<u32> {
        self.epoch
    }

    /// Freshness of a gateway packet (beacon or command) whose tag was good.
    /// On `Accept` the node has taken the packet's epoch and clock.
    pub fn on_gateway(&mut self, kind: Kind, s: &Stamp) -> Verdict {
        if s.session == 0 {
            // A broadcast beacon: no proof of freshness.
            if kind != Kind::Beacon {
                return Verdict::Reject(Reject::Replay);
            }
            return match self.epoch {
                None => {
                    self.epoch = Some(s.epoch);
                    self.clock = s.clock;
                    Verdict::Accept
                }
                Some(e) if e == s.epoch => {
                    self.clock = self.clock.max(s.clock);
                    Verdict::Accept
                }
                Some(_) => Verdict::Reject(Reject::Stale),
            };
        }
        if kind == Kind::Frames {
            return Verdict::Reject(Reject::BadTag);
        }
        if s.session != self.session {
            // Made for an earlier boot of ours (or another session).
            return Verdict::Reject(Reject::Replay);
        }
        let k = (s.counter, s.clock);
        match self.last_echo {
            Some(l) if k == l => return Verdict::Duplicate,
            Some(l) if k < l => return Verdict::Reject(Reject::Replay),
            _ => {}
        }
        self.last_echo = Some(k);
        if self.epoch == Some(s.epoch) {
            self.clock = self.clock.max(s.clock);
        } else {
            self.epoch = Some(s.epoch);
            self.clock = s.clock;
        }
        Verdict::Accept
    }

    /// Stamp for the next transmission (each retry is a new transmission).
    /// `None` while the node has no gateway epoch yet, or after a counter
    /// wrap until it has heard a newer gateway clock.
    pub fn next_up(&mut self) -> Option<Stamp> {
        let epoch = self.epoch?;
        if let Some(f) = self.floor {
            if self.clock <= f {
                return None;
            }
            self.floor = None;
        }
        let s = Stamp { epoch, clock: self.clock, session: self.session, counter: self.counter };
        if self.counter == u32::MAX {
            self.session = nonzero(self.rng);
            self.counter = 0;
            self.last_echo = None;
            self.floor = Some(self.clock);
        } else {
            self.counter += 1;
        }
        Some(s)
    }
}

/// One report the gateway owes the hub: packets claiming to be from `source`
/// rejected for `reason` during the last window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RejectReport {
    /// The claimed sender; all zero when the table was full (many addresses).
    pub source: [u8; 6],
    pub reason: Reject,
    pub count: u32,
    /// Strongest RSSI among them, dBm.
    pub rssi: i8,
    pub window_ms: u32,
}

#[derive(Debug, Clone, Copy)]
struct RejectEntry {
    source: [u8; 6],
    counts: [u32; 4],
    rssi: i8,
}

/// Rejected packets per claimed address and reason, reported once a window.
/// Fixed size: when it is full, further addresses share one all-zero entry
/// (an attacker cycling random addresses must not grow it).
#[derive(Debug, Clone)]
pub struct RejectLog<const N: usize> {
    entries: [Option<RejectEntry>; N],
    since_ms: u64,
    /// Totals since boot, by reason (index = code - 1).
    pub totals: [u32; 4],
}

impl<const N: usize> Default for RejectLog<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> RejectLog<N> {
    pub const fn new() -> Self {
        RejectLog { entries: [None; N], since_ms: 0, totals: [0; 4] }
    }

    pub fn record(&mut self, source: [u8; 6], reason: Reject, rssi: i8) {
        let r = reason.code() as usize - 1;
        self.totals[r] = self.totals[r].saturating_add(1);
        let pos = |entries: &[Option<RejectEntry>], s: [u8; 6]| entries.iter().position(|e| e.is_some_and(|e| e.source == s));
        let zero = [0u8; 6];
        let (i, source) = match pos(&self.entries, source) {
            Some(i) => (i, source),
            // The last slot is kept for the shared overflow entry.
            None => match self.entries[..N - 1].iter().position(Option::is_none) {
                Some(i) if source != zero => (i, source),
                _ => (pos(&self.entries, zero).unwrap_or(N - 1), zero),
            },
        };
        self.entries[i].get_or_insert(RejectEntry { source, counts: [0; 4], rssi: i8::MIN });
        let e = self.entries[i].as_mut().unwrap();
        e.counts[r] = e.counts[r].saturating_add(1);
        e.rssi = e.rssi.max(rssi);
    }

    /// If at least `window_ms` passed since the last call that reported,
    /// hand out one report per address and reason seen, and start a new
    /// window. Returns how many were reported.
    pub fn drain(&mut self, now_ms: u64, window_ms: u32, mut report: impl FnMut(RejectReport)) -> usize {
        if now_ms.saturating_sub(self.since_ms) < u64::from(window_ms) {
            return 0;
        }
        let window = now_ms.saturating_sub(self.since_ms).min(u64::from(u32::MAX)) as u32;
        self.since_ms = now_ms;
        let mut n = 0;
        for slot in &mut self.entries {
            let Some(e) = slot.take() else { continue };
            for reason in Reject::ALL {
                let count = e.counts[reason.code() as usize - 1];
                if count > 0 {
                    report(RejectReport { source: e.source, reason, count, rssi: e.rssi, window_ms: window });
                    n += 1;
                }
            }
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BEACON_LEN, Batch, MAX_PAYLOAD, beacon, command, frames, parse};

    // Obvious fixed test keys; never a real kit key.
    const K1: [u8; 32] = [0x11; 32];
    const K2: [u8; 32] = [0x22; 32];
    const NODE: [u8; 6] = [0x02, 0, 0, 0, 0, 0x0a];
    const NODE_B: [u8; 6] = [0x02, 0, 0, 0, 0, 0x0b];
    const GW: [u8; 6] = [0x02, 0, 0, 0, 0, 0x99];

    fn hex(b: &[u8]) -> std::string::String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// Deterministic "random" sessions for tests.
    fn rng_a() -> u32 {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0x1000);
        N.fetch_add(1, Ordering::Relaxed)
    }

    #[test]
    fn hmac_rfc4231_vectors() {
        // RFC 4231 test case 1 and 2.
        let m = Hmac::new(&[0x0b; 20]).mac(&[b"Hi There"]);
        assert_eq!(hex(&m), "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7");
        let m = Hmac::new(b"Jefe").mac(&[b"what do ya want ", b"for nothing?"]);
        assert_eq!(hex(&m), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
    }

    #[test]
    fn key_debug_hides_the_key_and_id_is_stable() {
        let k = Key::new(&K1);
        assert_eq!(format!("{k:?}"), "Key(..)");
        assert_eq!(k.id(), Key::new(&K1).id());
        assert_ne!(k.id(), Key::new(&K2).id());
    }

    fn sealed_batch(key: &Key, src: &[u8; 6], stamp: Stamp) -> std::vec::Vec<u8> {
        let mut b = Batch::new();
        assert!(b.push(&[1, 2, 3, 0]));
        assert!(b.push(&[9, 0]));
        b.sealed(7, key, src, stamp).to_vec()
    }

    #[test]
    fn round_trip_batch_beacon_command() {
        let key = Key::new(&K1);
        let st = Stamp { epoch: 0xdead_beef, clock: 5, session: 77, counter: 3 };
        let pkt = sealed_batch(&key, &NODE, st);
        let p = parse(&pkt).unwrap();
        assert!(p.sealed);
        assert_eq!((p.kind, p.seq, p.payload), (Kind::Frames, 7, &[1, 2, 3, 0, 9, 0][..]));
        assert_eq!(frames(&p).count(), 2, "the trailer is never taken for a frame");
        assert_eq!(verify(&key, &NODE, &pkt), Ok(st));

        let mut buf = [0u8; BEACON_LEN + TRAILER];
        beacon(&mut buf, 1, 6);
        let n = seal(&key, &GW, &mut buf, BEACON_LEN, st).unwrap();
        let p = parse(&buf[..n]).unwrap();
        assert_eq!(crate::beacon_channel(&p), Some(6));
        assert_eq!(verify(&key, &GW, &buf[..n]), Ok(st));

        let mut buf = [0u8; 64];
        let len = command(&mut buf, 2, &[4, 1, 2, 3, 0]).unwrap().len();
        let n = seal(&key, &GW, &mut buf, len, st).unwrap();
        let p = parse(&buf[..n]).unwrap();
        assert_eq!((p.kind, p.payload), (Kind::Command, &[4, 1, 2, 3, 0][..]));
        assert_eq!(verify(&key, &GW, &buf[..n]), Ok(st));
    }

    #[test]
    fn every_bit_flip_is_caught() {
        let key = Key::new(&K1);
        let pkt = sealed_batch(&key, &NODE, Stamp { epoch: 1, clock: 2, session: 3, counter: 4 });
        for i in 0..pkt.len() {
            for bit in 0..8 {
                let mut t = pkt.clone();
                t[i] ^= 1 << bit;
                let r = verify(&key, &NODE, &t);
                // Flipping the version byte to 1 makes it an unauthenticated packet.
                assert!(matches!(r, Err(Reject::BadTag | Reject::Unauthenticated)), "byte {i} bit {bit}: {r:?}");
            }
        }
    }

    #[test]
    fn wrong_key_and_wrong_sender_fail() {
        let pkt = sealed_batch(&Key::new(&K1), &NODE, Stamp::default());
        assert_eq!(verify(&Key::new(&K2), &NODE, &pkt), Err(Reject::BadTag));
        // A genuine packet of node A replayed under node B's address.
        assert_eq!(verify(&Key::new(&K1), &NODE_B, &pkt), Err(Reject::BadTag));
    }

    #[test]
    fn truncated_or_extended_packets_fail() {
        let key = Key::new(&K1);
        let pkt = sealed_batch(&key, &NODE, Stamp::default());
        for n in 0..pkt.len() {
            assert!(verify(&key, &NODE, &pkt[..n]).is_err(), "truncated to {n}");
        }
        let mut longer = pkt.clone();
        longer.push(0);
        assert_eq!(verify(&key, &NODE, &longer), Err(Reject::BadTag));
    }

    #[test]
    fn unauthenticated_packets_are_named_as_such() {
        let key = Key::new(&K1);
        let mut b = Batch::new();
        b.push(&[1, 0]);
        assert_eq!(verify(&key, &NODE, b.packet(0)), Err(Reject::Unauthenticated));
        assert_eq!(verify(&key, &NODE, b"junk"), Err(Reject::BadTag));
    }

    #[test]
    fn sealed_batch_never_exceeds_max_packet() {
        let key = Key::new(&K1);
        let mut b = Batch::new();
        let mut frame = [7u8; 100];
        frame[99] = 0;
        while b.push(&frame) {}
        assert!(b.push(&[1, 0]) || b.payload_len() + 2 > MAX_PAYLOAD);
        let n = b.sealed(0, &key, &NODE, Stamp::default()).len();
        assert!(n <= MAX_PACKET);
        let pkt = b.sealed(0, &key, &NODE, Stamp::default()).to_vec();
        assert!(verify(&key, &NODE, &pkt).is_ok());
    }

    /// One gateway and one node exchanging real packets.
    struct Bench {
        gw: Gateway,
        peer: Peer,
        node: Node,
    }

    impl Bench {
        fn new() -> Self {
            Bench { gw: Gateway::new(Key::new(&K1), 0xa1), peer: Peer::default(), node: Node::new(Key::new(&K1), rng_a) }
        }
        /// Gateway beacon (broadcast, or a sync echoing `echo`), as bytes.
        fn gw_packet(&mut self, kind: Kind, echo: Option<(u32, u32)>) -> std::vec::Vec<u8> {
            let st = self.gw.stamp(echo).unwrap();
            let mut buf = [0u8; 128];
            let len = match kind {
                Kind::Beacon => beacon(&mut buf, 0, 6).len(),
                _ => command(&mut buf, 0, &[4, 1, 2, 3, 0]).unwrap().len(),
            };
            let n = seal(&self.gw.key, &GW, &mut buf, len, st).unwrap();
            buf[..n].to_vec()
        }
        fn node_hears(&mut self, pkt: &[u8]) -> Verdict {
            let st = verify(&self.node.key, &GW, pkt).unwrap();
            self.node.on_gateway(parse(pkt).unwrap().kind, &st)
        }
        fn node_packet(&mut self) -> Option<std::vec::Vec<u8>> {
            let st = self.node.next_up()?;
            Some(sealed_batch(&self.node.key, &NODE, st))
        }
        fn gw_hears(&mut self, pkt: &[u8]) -> Verdict {
            match verify(&self.gw.key, &NODE, pkt) {
                Ok(st) => self.peer.check(self.gw.epoch(), &st),
                Err(r) => Verdict::Reject(r),
            }
        }
    }

    #[test]
    fn node_needs_a_gateway_before_sending() {
        let mut b = Bench::new();
        assert!(b.node_packet().is_none());
        let bc = b.gw_packet(Kind::Beacon, None);
        assert_eq!(b.node_hears(&bc), Verdict::Accept);
        let p = b.node_packet().unwrap();
        assert_eq!(b.gw_hears(&p), Verdict::Accept);
        assert_eq!(b.peer.echo, Some((b.node.session(), 0)));
    }

    #[test]
    fn uplink_replay_and_duplicate() {
        let mut b = Bench::new();
        let bc = b.gw_packet(Kind::Beacon, None);
        b.node_hears(&bc);
        let p1 = b.node_packet().unwrap();
        let p2 = b.node_packet().unwrap();
        assert_eq!(b.gw_hears(&p1), Verdict::Accept);
        assert_eq!(b.gw_hears(&p2), Verdict::Accept);
        assert_eq!(b.gw_hears(&p2), Verdict::Duplicate, "the radio repeating a frame");
        assert_eq!(b.gw_hears(&p1), Verdict::Reject(Reject::Replay));
        // A newer beacon moves the clock on; the counter keeps going.
        let bc = b.gw_packet(Kind::Beacon, None);
        b.node_hears(&bc);
        let p3 = b.node_packet().unwrap();
        assert_eq!(b.gw_hears(&p3), Verdict::Accept);
        assert_eq!(b.gw_hears(&p2), Verdict::Reject(Reject::Replay));
    }

    #[test]
    fn node_reboot_resyncs_and_old_session_stays_dead() {
        let mut b = Bench::new();
        let bc = b.gw_packet(Kind::Beacon, None);
        b.node_hears(&bc);
        let mut old = std::vec::Vec::new();
        for _ in 0..5 {
            let p = b.node_packet().unwrap();
            assert_eq!(b.gw_hears(&p), Verdict::Accept);
            old.push(p);
        }
        // Reboot: new session, counter 0, knows nothing.
        b.node = Node::new(Key::new(&K1), rng_a);
        let bc2 = b.gw_packet(Kind::Beacon, None);
        b.node_hears(&bc2);
        let p = b.node_packet().unwrap();
        assert_eq!(b.gw_hears(&p), Verdict::Accept, "a fresh beacon puts it past the old session");
        for p in &old {
            assert_eq!(b.gw_hears(p), Verdict::Reject(Reject::Replay));
        }
    }

    #[test]
    fn node_reboot_on_a_replayed_beacon_recovers_by_sync() {
        let mut b = Bench::new();
        let old_beacon = b.gw_packet(Kind::Beacon, None);
        b.node_hears(&old_beacon);
        for _ in 0..3 {
            let bc = b.gw_packet(Kind::Beacon, None);
            b.node_hears(&bc);
            let p = b.node_packet().unwrap();
            assert_eq!(b.gw_hears(&p), Verdict::Accept);
        }
        b.node = Node::new(Key::new(&K1), rng_a);
        assert_eq!(b.node_hears(&old_beacon), Verdict::Accept, "an attacker's recording, first thing after boot");
        let p = b.node_packet().unwrap();
        assert_eq!(b.gw_hears(&p), Verdict::Reject(Reject::Replay));
        // The gateway answers a valid-tag reject with a sync echoing that packet.
        let st = verify(&b.gw.key, &NODE, &p).unwrap();
        let sync = b.gw_packet(Kind::Beacon, Some((st.session, st.counter)));
        assert_eq!(b.node_hears(&sync), Verdict::Accept);
        let p = b.node_packet().unwrap();
        assert_eq!(b.gw_hears(&p), Verdict::Accept);
    }

    #[test]
    fn gateway_reboot_resyncs_and_old_commands_stay_dead() {
        let mut b = Bench::new();
        let bc_old = b.gw_packet(Kind::Beacon, None);
        b.node_hears(&bc_old);
        let p = b.node_packet().unwrap();
        assert_eq!(b.gw_hears(&p), Verdict::Accept);
        let cmd_old = b.gw_packet(Kind::Command, b.peer.echo);
        assert_eq!(b.node_hears(&cmd_old), Verdict::Accept);
        assert_eq!(b.node_hears(&cmd_old), Verdict::Duplicate);
        let old_up = b.node_packet().unwrap();
        assert_eq!(b.gw_hears(&old_up), Verdict::Accept);

        // Gateway reboot: new epoch, clock from 0, no peers.
        b.gw = Gateway::new(Key::new(&K1), 0xb2);
        b.peer = Peer::default();
        // Its broadcast beacons do not move the node off its epoch...
        let bc = b.gw_packet(Kind::Beacon, None);
        assert_eq!(b.node_hears(&bc), Verdict::Reject(Reject::Stale));
        // ...its next packet is stale, and the gateway syncs it.
        let p = b.node_packet().unwrap();
        assert_eq!(b.gw_hears(&p), Verdict::Reject(Reject::Stale));
        assert_eq!(b.gw_hears(&old_up), Verdict::Reject(Reject::Stale), "recordings from before the reboot");
        let st = verify(&b.gw.key, &NODE, &p).unwrap();
        let sync = b.gw_packet(Kind::Beacon, Some((st.session, st.counter)));
        assert_eq!(b.node_hears(&sync), Verdict::Accept);
        let p = b.node_packet().unwrap();
        assert_eq!(b.gw_hears(&p), Verdict::Accept);
        let cmd_new = b.gw_packet(Kind::Command, b.peer.echo);
        assert_eq!(b.node_hears(&cmd_new), Verdict::Accept);
        // The command from before the gateway's reboot (higher clock than the
        // new gateway's) must not be taken.
        assert_eq!(b.node_hears(&cmd_old), Verdict::Reject(Reject::Replay));
        // Nor is a recorded beacon from the previous epoch believed now.
        assert_eq!(b.node_hears(&bc_old), Verdict::Reject(Reject::Stale));
        assert_eq!(b.node_hears(&bc), Verdict::Accept, "the current epoch's beacon is fine");
    }

    #[test]
    fn commands_need_this_boots_session() {
        let mut b = Bench::new();
        let bc = b.gw_packet(Kind::Beacon, None);
        b.node_hears(&bc);
        let p = b.node_packet().unwrap();
        b.gw_hears(&p);
        let cmd = b.gw_packet(Kind::Command, b.peer.echo);
        b.node = Node::new(Key::new(&K1), rng_a); // node reboot
        b.node_hears(&bc);
        assert_eq!(b.node_hears(&cmd), Verdict::Reject(Reject::Replay));
        // A command without an echo is never fresh.
        let bare = b.gw_packet(Kind::Command, None);
        assert_eq!(b.node_hears(&bare), Verdict::Reject(Reject::Replay));
        // Older command (smaller echo) after a newer one.
        let p1 = b.node_packet().unwrap();
        b.gw_hears(&p1);
        let c1 = b.gw_packet(Kind::Command, b.peer.echo);
        let p2 = b.node_packet().unwrap();
        b.gw_hears(&p2);
        let c2 = b.gw_packet(Kind::Command, b.peer.echo);
        assert_eq!(b.node_hears(&c2), Verdict::Accept);
        assert_eq!(b.node_hears(&c1), Verdict::Reject(Reject::Replay));
    }

    #[test]
    fn a_node_never_takes_a_batch_as_a_gateway_packet() {
        let mut b = Bench::new();
        let st = Stamp { epoch: 1, clock: 1, session: b.node.session(), counter: 0 };
        assert_eq!(b.node.on_gateway(Kind::Frames, &st), Verdict::Reject(Reject::BadTag));
    }

    #[test]
    fn node_counter_wrap_starts_a_new_session() {
        let mut b = Bench::new();
        let bc = b.gw_packet(Kind::Beacon, None);
        b.node_hears(&bc);
        b.node.counter = u32::MAX - 1;
        let s1 = b.node.session();
        let p = b.node_packet().unwrap();
        assert_eq!(b.gw_hears(&p), Verdict::Accept);
        let last = b.node_packet().unwrap();
        assert_eq!(b.gw_hears(&last), Verdict::Accept);
        assert_ne!(b.node.session(), s1, "new session after u32::MAX");
        assert!(b.node_packet().is_none(), "waits for a newer gateway clock");
        let bc = b.gw_packet(Kind::Beacon, None);
        b.node_hears(&bc);
        let p = b.node_packet().unwrap();
        assert_eq!(verify(&b.node.key, &NODE, &p).unwrap().counter, 0);
        assert_eq!(b.gw_hears(&p), Verdict::Accept, "(new clock, 0) is past (old clock, MAX)");
        assert_eq!(b.gw_hears(&last), Verdict::Reject(Reject::Replay));
    }

    #[test]
    fn gateway_clock_runs_out_instead_of_wrapping() {
        let mut g = Gateway::new(Key::new(&K1), 1);
        g.clock = u32::MAX - 1;
        assert_eq!(g.stamp(None).unwrap().clock, u32::MAX);
        assert!(g.stamp(None).is_none());
    }

    #[test]
    fn reject_log_reports_per_source_and_reason_and_bounds_its_table() {
        let mut log = RejectLog::<4>::new();
        assert_eq!(log.drain(0, 1000, |_| {}), 0);
        log.record(NODE, Reject::BadTag, -50);
        log.record(NODE, Reject::BadTag, -40);
        log.record(NODE, Reject::Replay, -60);
        log.record(NODE_B, Reject::Unauthenticated, -70);
        let mut got = std::vec::Vec::new();
        assert_eq!(log.drain(500, 1000, |r| got.push(r)), 0, "not a full window yet");
        assert_eq!(log.drain(1000, 1000, |r| got.push(r)), 3);
        assert_eq!(got[0], RejectReport { source: NODE, reason: Reject::BadTag, count: 2, rssi: -40, window_ms: 1000 });
        assert_eq!((got[1].reason, got[1].count), (Reject::Replay, 1));
        assert_eq!((got[2].source, got[2].reason), (NODE_B, Reject::Unauthenticated));
        assert_eq!(log.drain(2000, 1000, |_| {}), 0, "nothing new");
        // Many random addresses: three get slots, the rest share the zero entry.
        for i in 0..50u8 {
            log.record([0x02, 1, 2, 3, 4, i], Reject::BadTag, -80);
        }
        let mut got = std::vec::Vec::new();
        log.drain(3000, 1000, |r| got.push(r));
        assert_eq!(got.len(), 4);
        assert_eq!((got[3].source, got[3].count), ([0; 6], 47));
        assert_eq!(log.totals, [52, 1, 1, 0]);
    }
}
