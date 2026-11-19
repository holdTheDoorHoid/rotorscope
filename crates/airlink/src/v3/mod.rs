//! ESP-NOW packet version 3: per-node keys, a tick clock, and a gateway that
//! holds no secret (docs/LINK-KEYS-DESIGN.md sections 6.1, 6.2 and 12.1).
//!
//! Version 2 ([`crate::auth`]) stays as it is until the cut-over package
//! deletes it; nothing here changes how a version 2 packet is built or read.
//! A version 3 receiver refuses a version 2 packet as
//! [`Reject::OldFirmware`]: that is how an un-reflashed board shows up.
//!
//! # Three roles, no shared secret
//!
//! | Role | Holds | Can |
//! |---|---|---|
//! | Hub ([`Hub`]) | per roster slot: the node's MAC, `K_up`, `K_down` | seal Sync, Command, Beacon; open Frames |
//! | Node ([`Node`]) | its own slot, MAC, `K_up`, `K_down` | seal Frames; open Sync, Command and its own Beacon tag |
//! | Gateway | nothing | [`inspect`]: version, kind, sizes, sequence. No function here verifies or makes a tag without a key. |
//!
//! Keys come from `kitkeys` (`NodeKey::up()` and `NodeKey::down()`). Up and down
//! keys are different, so a packet can never be reflected back the other way,
//! whatever its kind byte says.
//!
//! # Packets
//!
//! Header (6): `"RS"`, version 3, kind, sequence (u16 LE). Then:
//!
//! | Kind | Direction | After the header | Size |
//! |---|---|---|---|
//! | 1 Sync | hub to one node | channel (1), stamp (16), tag (12, `K_down`) | [`SYNC_LEN`] = 35 |
//! | 2 Frames | node to hub | COBS frames (1..=990), stamp, tag (12, `K_up`) | up to [`MAX_PACKET`] |
//! | 3 Command | hub to one node | COBS commands, stamp, tag (12, `K_down`) | up to [`MAX_PACKET`] |
//! | 4 Beacon | hub to all | channel (1), n (1, 1..=16), stamp (16, session 0), n tags of 8 | [`beacon_len`]`(n)` = 24 + 8n: 152 for 16 |
//! | 5 Unprovisioned | node, unsealed | MAC (6), image (1), firmware (16) | [`UNPROVISIONED_LEN`] = 29 |
//!
//! Tag = `HMAC(K, sender_mac | everything before the tag)`, truncated. The
//! sender is the radio's source address: the node's MAC for Frames, the
//! gateway's MAC for everything the hub makes. Beacon tag `i` is under slot
//! `i`'s `K_down` over `gateway_mac | header | channel | n | stamp`; empty or
//! revoked slots get eight zero bytes, and a node checks only its own slot.
//!
//! # The tick clock (section 6.1)
//!
//! The hub's clock is `max(previous + 1, hub_uptime_ms / 10)` per sealed
//! packet ([`Hub::next_clock`]): one tick per 10 ms, strictly increasing. At
//! `u32::MAX` (about 497 days) the hub refuses to seal and must restart for a
//! new epoch.
//!
//! A node keeps two numbers derived from the highest hub clock it accepted:
//!
//! * the **floor** ([`Node::floor`]): that clock plus [`FLOOR_TICKS_PER_S`] (90)
//!   per second of node uptime since. The real clock grows 100 a second; the
//!   10 % margin covers crystal error. Taking a packet raises the floor to its
//!   clock, never lowers it.
//! * the **estimate** ([`Node::estimate`]): that clock plus one tick per 10 ms
//!   since, never decreasing within an epoch. It is the `clock` of every stamp
//!   the node seals, so the hub can bound the age of a batch.
//!
//! Lateness ([`DOWN_SLACK`], [`TIMESYNC_SLACK`], [`UPLINK_MAX_AGE`],
//! [`UPLINK_MAX_AHEAD`]):
//!
//! | Packet | Refused if | As |
//! |---|---|---|
//! | Sync, Command, Beacon at the node | `clock + 300 < floor` | [`Reject::Late`] |
//! | A Command carrying a `TimeSync` at the node | `clock + 50 < floor` | [`Reject::Late`] |
//! | Frames at the hub | `now - clock > 200` | [`Reject::LateUplink`] |
//! | Frames at the hub | `clock > now + 50` | [`Reject::FutureUplink`] |
//!
//! A node that heard the hub up to a recording refuses it about 3.3 s later
//! (300 ticks at the floor's 90 a second), and 3 s later when it keeps hearing
//! beacons, which lift the floor to the real clock. A node jammed for `J`
//! seconds before the recording, and still jammed until the release, refuses it
//! after `(300 + 10 J) / 90` seconds: 3.3 s plus about 0.11 s per jammed
//! second. After 60 s of hearing nothing it starts a new session (below), and
//! every recording made for the old one is dead. A `TimeSync` is the same with
//! 50 for 300: 0.55 s plus about 0.11 s per jammed second.
//!
//! The node re-seals every retry of a batch ([`Node::seal_frames`]), so a batch
//! retried for its full 2 s is never late. The gateway repeats the hub's
//! bytes, so a hub packet ages while it is retried: commands give up after
//! 2 s and `TimeSync`s after 300 ms, both inside their slack.
//!
//! Order is kept as before. At the hub, per slot, `(clock, counter)` must be
//! above the last accepted pair (equal is a duplicate). At the node, an
//! echoed packet's `(echoed counter, clock)` must be above the last one taken
//! in the current epoch.
//!
//! # Epochs are ordered (section 12, item 1)
//!
//! The epoch is the hub's boot counter (`rsboot`), one higher at every hub
//! boot. A beacon carries no proof of when it was made; a Sync or Command
//! echoes the node's random per-boot `session` and its latest counter, which
//! proves it was made during this node session, after the node sent that
//! counter. The node keeps one epoch, either **tentative** (learned from a
//! beacon) or **proven** (an echoed packet of that epoch was taken). The rule:
//!
//! 1. **First packet after boot (no epoch):** a genuine beacon of any epoch is
//!    taken, tentatively: it teaches the gateway address and a clock. An
//!    echoed packet cannot be genuine yet (the node has sent nothing in this
//!    session); a Sync is still taken (it proves itself), a Command is refused
//!    as [`Reject::Unproven`].
//! 2. **Same epoch:** taken if in order and not late.
//! 3. **Higher epoch:** a Sync moves the node there and proves it (the hub
//!    rebooted). A beacon moves a *tentative* node there; a node with a proven
//!    epoch refuses it as [`Reject::Unproven`] (it may be a recording from an
//!    earlier provisioning, rule 5), but it still shows a gateway is there. A
//!    Command never moves the node: [`Reject::Unproven`].
//! 4. **Lower epoch:** refused as [`Reject::Stale`], except that a Sync may
//!    move a *tentative* node down (the tentative epoch was never proven, the
//!    Sync is).
//! 5. **Relink.** A node that has taken nothing for [`RELINK_MS`] (60 s) starts
//!    a new session: new random `session`, counter 0, no epoch, no floor. It is
//!    then exactly a node that has just booted. This is how a kit recovers
//!    from a hub that was re-provisioned and came back with a *lower* counter:
//!    the node refuses everything of the lower epoch (rule 4) for 60 s,
//!    relinks, takes the hub's next beacon tentatively, and its next uplink is
//!    accepted. No reflash, no power cycle. It reopens nothing: every Command,
//!    `TimeSync` and Sync recorded before echoes the old session (refused as
//!    [`Reject::Replay`]), and a recorded beacon of the old, higher epochs is
//!    at most a tentative epoch until the first Sync, exactly as after a boot,
//!    and is refused as [`Reject::Unproven`] once the node is proven.
//! 6. **`TimeSync` needs a proven epoch.** A Command carrying a `TimeSync` is
//!    refused as [`Reject::Unproven`] unless the node's epoch is proven and equal
//!    to the packet's. It never moves or proves an epoch.
//!
//! Consequences, each tested: a command or `TimeSync` recorded before a node
//! reboot (or relink) is dead after it (session); an old-epoch beacon replayed
//! to a node that has just booted (or relinked after a long jam) costs at
//! most the uplinks sent until the hub's Sync, and can never move a proven
//! node; a recorded Command of a dead hub boot is refused once the node has
//! moved to the newer epoch.
//!
//! What this needs from other packages:
//!
//! * **13e (hub):** answer a *genuine* refused uplink ([`HubVerdict::wants_sync`]:
//!   `Stale`, `LateUplink`, `FutureUplink`) with a Sync echoing it, rate-limited;
//!   send a Sync before the first `TimeSync` to a node whose session it has not
//!   synced yet (rule 6); send each `TimeSync` alone in its own Command packet,
//!   sealed when written to the UART.
//! * **13g (node):** call [`Node::accept`] with `timesync = true` for a Command
//!   packet that holds a `TimeSync` frame (strictest rule for the whole packet);
//!   take randomness for [`Node::new`] from the TRNG after the radio starts
//!   (section 7.4: a session that repeats would undo rule 5); call
//!   [`Node::seal_frames`] (or [`crate::Batch::sealed_v3`]) on every retry;
//!   feed both with the same monotonic uptime. The relink is done here; the
//!   firmware must not reset the link on its own deaf fall-back, only report it.
//! * **13a / `link-key hub`:** when re-provisioning a hub whose `rsboot` still
//!   reads good, keep its value (seed above it) rather than drawing a fresh
//!   random seed. A random seed lands below the old counter about half the
//!   time, and every node then needs the 60 s relink to follow. Correctness does
//!   not depend on it; recovery speed does.
//! * **13c (wire types):** detail codes for the new reasons (see [`Reject`]),
//!   and `NodeHealth`'s per-reason counters sized for the node-side reasons.
//!
//! # Cost
//!
//! One tag costs [`tag_blocks`] SHA-256 blocks (the key's pads are absorbed
//! once): 18 for a full 1024-byte packet, 2 for a Sync or one beacon tag. A
//! beacon costs the hub 2 per occupied slot (32 for 16), and a node 2.

use kitkeys::{LinkKey, Mac, NodeKey, SLOTS};

use crate::auth::Stamp;
use crate::{HEADER, MAGIC, MAX_PACKET};

mod hub;
mod node;
#[cfg(test)]
mod tests;

pub use hub::{Hub, HubVerdict, Peer, SealError, Uplink};
pub use node::{Down, Node, NodeKeys, NodeRejects, NodeVerdict};

/// Header version of a version 3 packet.
pub const VERSION_V3: u8 = 3;
/// The open-kit (unauthenticated) version.
pub const VERSION_OPEN: u8 = crate::VERSION;
/// The kit-key version this one replaces.
pub const VERSION_OLD: u8 = crate::auth::VERSION_AUTH;
pub const STAMP_LEN: usize = 16;
/// Tag on Sync, Frames and Command.
pub const TAG_LEN: usize = 12;
/// One beacon tag.
pub const BEACON_TAG_LEN: usize = 8;
/// Stamp and tag after a Frames or Command payload.
pub const TRAILER: usize = STAMP_LEN + TAG_LEN;
/// Largest Frames or Command payload (the same 990 as version 2).
pub const MAX_PAYLOAD: usize = MAX_PACKET - HEADER - TRAILER;
/// A Sync: header, channel, stamp, tag.
pub const SYNC_LEN: usize = HEADER + 1 + STAMP_LEN + TAG_LEN;
/// A beacon before its tags: header, channel, n, stamp.
pub const BEACON_FIXED: usize = HEADER + 2 + STAMP_LEN;
/// A beacon with all 16 slots.
pub const BEACON_MAX: usize = beacon_len(SLOTS);
/// Firmware name in an Unprovisioned announcement.
pub const FIRMWARE_LEN: usize = 16;
/// An Unprovisioned announcement: header, MAC, image, firmware.
pub const UNPROVISIONED_LEN: usize = HEADER + 6 + 1 + FIRMWARE_LEN;

/// One clock tick, ms.
pub const TICK_MS: u64 = 10;
/// The node's floor grows this much per second of its own uptime.
pub const FLOOR_TICKS_PER_S: u64 = 90;
/// Sync, Command, Beacon at the node: late if `clock + DOWN_SLACK < floor`.
pub const DOWN_SLACK: u32 = 300;
/// A `TimeSync` at the node: late if `clock + TIMESYNC_SLACK < floor`.
pub const TIMESYNC_SLACK: u32 = 50;
/// A batch at the hub is late if `now - clock > UPLINK_MAX_AGE`.
pub const UPLINK_MAX_AGE: u32 = 200;
/// A batch at the hub is from the future if `clock > now + UPLINK_MAX_AHEAD`.
pub const UPLINK_MAX_AHEAD: u32 = 50;
/// A node that has taken nothing for this long starts a new session (rule 5).
pub const RELINK_MS: u64 = 60_000;

/// Size of a beacon carrying `n` tags.
pub const fn beacon_len(n: usize) -> usize {
    BEACON_FIXED + n * BEACON_TAG_LEN
}

/// SHA-256 blocks one tag over `signed` bytes (sender MAC excluded) costs,
/// with the key's pads absorbed in advance: the inner hash over MAC and
/// message with its 9 bytes of padding, plus one block for the outer hash.
pub const fn tag_blocks(signed: usize) -> usize {
    (6 + signed + 9).div_ceil(64) + 1
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Sync = 1,
    Frames = 2,
    Command = 3,
    Beacon = 4,
    Unprovisioned = 5,
}

impl Kind {
    fn from_u8(b: u8) -> Option<Kind> {
        Some(match b {
            1 => Kind::Sync,
            2 => Kind::Frames,
            3 => Kind::Command,
            4 => Kind::Beacon,
            5 => Kind::Unprovisioned,
            _ => return None,
        })
    }
}

/// Why a packet was not taken. Every reason is its own count. `code` is the
/// number for `link::ThreatKind::ForgedLink`'s `detail`: 1-4 and 7 as in
/// version 2, 8, 9, 11 and 12 as the design names them; 13 and up are new here
/// and need 13c to reserve them (5, 6 and 10 belong to others).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Reject {
    /// The tag does not match: forged, corrupted, or made with another key.
    BadTag = 1,
    /// Genuine but seen before (an older pair), or echoing another session
    /// (made for an earlier boot or relink of the node).
    Replay = 2,
    /// A version 1 (open-kit) packet at a keyed receiver.
    Unauthenticated = 3,
    /// Genuine but from an epoch the receiver is past.
    Stale = 4,
    /// Node: genuine, in order, but older than the floor allows.
    Late = 7,
    /// A version 2 (kit key) packet: a board that was not reflashed.
    OldFirmware = 8,
    /// Hub: the sender's MAC is not in the roster.
    NotInRoster = 9,
    /// Hub: a batch whose clock is more than 200 ticks behind the hub's.
    LateUplink = 11,
    /// Hub: a batch whose clock is more than 50 ticks ahead of the hub's.
    FutureUplink = 12,
    /// Node: a beacon with no tag (or a zero tag) for this node's slot.
    NotForThisSlot = 13,
    /// Node: genuine, but from an epoch the node has not been moved to by a
    /// Sync, or a `TimeSync` before the node's epoch is proven (rules 1, 3, 6).
    Unproven = 14,
    /// Ours (magic "RS") but the wrong size, kind or direction.
    Malformed = 15,
    /// Not ours at all: other ESP-NOW users share the air. Not an attack.
    Foreign = 16,
}

/// Reasons a [`Reject`] can have: the size of per-reason count arrays.
pub const N_REJECT: usize = 13;

impl Reject {
    pub const ALL: [Reject; N_REJECT] = [
        Reject::BadTag,
        Reject::Replay,
        Reject::Unauthenticated,
        Reject::Stale,
        Reject::Late,
        Reject::OldFirmware,
        Reject::NotInRoster,
        Reject::LateUplink,
        Reject::FutureUplink,
        Reject::NotForThisSlot,
        Reject::Unproven,
        Reject::Malformed,
        Reject::Foreign,
    ];

    pub const fn code(self) -> u32 {
        self as u32
    }

    /// Index into per-reason count arrays (`0..N_REJECT`).
    pub const fn index(self) -> usize {
        match self {
            Reject::BadTag => 0,
            Reject::Replay => 1,
            Reject::Unauthenticated => 2,
            Reject::Stale => 3,
            Reject::Late => 4,
            Reject::OldFirmware => 5,
            Reject::NotInRoster => 6,
            Reject::LateUplink => 7,
            Reject::FutureUplink => 8,
            Reject::NotForThisSlot => 9,
            Reject::Unproven => 10,
            Reject::Malformed => 11,
            Reject::Foreign => 12,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Reject::BadTag => "bad_tag",
            Reject::Replay => "replay",
            Reject::Unauthenticated => "unauthenticated",
            Reject::Stale => "stale",
            Reject::Late => "late",
            Reject::OldFirmware => "old_firmware",
            Reject::NotInRoster => "not_in_roster",
            Reject::LateUplink => "late_uplink",
            Reject::FutureUplink => "future_uplink",
            Reject::NotForThisSlot => "not_for_this_slot",
            Reject::Unproven => "unproven",
            Reject::Malformed => "malformed",
            Reject::Foreign => "foreign",
        }
    }
}

/// Outcome of a whole receive check, without the packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Accept,
    /// Exactly the last accepted packet again (a radio retry): drop quietly.
    Duplicate,
    Reject(Reject),
}

/// What [`inspect`] can tell without any key. Nothing in it is verified: a
/// forged packet inspects exactly like a genuine one of the same shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shape {
    pub kind: Kind,
    pub seq: u16,
    /// Whole packet length.
    pub len: usize,
    /// Frames or Command: the COBS bytes. Zero for the other kinds.
    pub payload_len: usize,
    /// Beacon: how many tags it carries. Zero for the other kinds.
    pub slots: u8,
}

impl Shape {
    /// Where the stamp is, for the sealed kinds.
    fn stamp_at(&self) -> Option<usize> {
        match self.kind {
            Kind::Sync => Some(HEADER + 1),
            Kind::Beacon => Some(HEADER + 2),
            Kind::Frames | Kind::Command => Some(self.len - TRAILER),
            Kind::Unprovisioned => None,
        }
    }

    /// End of the bytes a 12-byte tag covers (Sync, Frames, Command).
    fn signed_end(&self) -> usize {
        self.len - TAG_LEN
    }
}

/// The gateway's whole view of a packet: version, kind, sizes and sequence.
/// Takes no key and verifies nothing. `Err` says what it is instead:
/// [`Reject::Foreign`] (not ours), [`Reject::Unauthenticated`] (version 1),
/// [`Reject::OldFirmware`] (version 2) or [`Reject::Malformed`] (ours, but the
/// size does not match the kind, or an unknown version or kind).
///
/// Sizes are exact: Sync 35, Beacon `24 + 8n` with `1 <= n <= 16`,
/// Unprovisioned 29, Frames and Command at least one payload byte plus the
/// trailer, nothing over [`MAX_PACKET`]. It never reads past `data`.
pub fn inspect(data: &[u8]) -> Result<Shape, Reject> {
    if data.len() < 3 || data[..2] != MAGIC {
        return Err(Reject::Foreign);
    }
    match data[2] {
        VERSION_V3 => {}
        VERSION_OPEN => return Err(Reject::Unauthenticated),
        VERSION_OLD => return Err(Reject::OldFirmware),
        _ => return Err(Reject::Malformed),
    }
    let len = data.len();
    if !(HEADER..=MAX_PACKET).contains(&len) {
        return Err(Reject::Malformed);
    }
    let kind = Kind::from_u8(data[3]).ok_or(Reject::Malformed)?;
    let seq = u16::from_le_bytes([data[4], data[5]]);
    let mut shape = Shape { kind, seq, len, payload_len: 0, slots: 0 };
    let ok = match kind {
        Kind::Sync => len == SYNC_LEN,
        Kind::Unprovisioned => len == UNPROVISIONED_LEN,
        Kind::Frames | Kind::Command => {
            shape.payload_len = len.saturating_sub(HEADER + TRAILER);
            len > HEADER + TRAILER
        }
        Kind::Beacon => {
            let n = if len >= BEACON_FIXED { usize::from(data[HEADER + 1]) } else { 0 };
            shape.slots = n as u8;
            (1..=SLOTS).contains(&n) && len == beacon_len(n)
        }
    };
    if ok { Ok(shape) } else { Err(Reject::Malformed) }
}

/// The payload bytes of a Frames or Command packet [`inspect`] accepted
/// (unverified).
pub fn payload<'a>(data: &'a [u8], shape: &Shape) -> &'a [u8] {
    match shape.kind {
        Kind::Frames | Kind::Command => &data[HEADER..HEADER + shape.payload_len],
        _ => &[],
    }
}

/// The whole COBS frames in a payload; a truncated tail is skipped.
pub fn frames(payload: &[u8]) -> impl Iterator<Item = &[u8]> {
    payload.split_inclusive(|&b| b == 0).filter(|f| f.last() == Some(&0))
}

/// An Unprovisioned announcement (section 4.4): information only, anyone can
/// send one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unprovisioned {
    pub mac: Mac,
    /// Which node image is running (the node's own numbering).
    pub image: u8,
    /// Firmware name or version, zero-padded.
    pub firmware: [u8; FIRMWARE_LEN],
}

/// Build an Unprovisioned announcement in `buf` (at least
/// [`UNPROVISIONED_LEN`] bytes); `None` if it does not fit.
pub fn unprovisioned<'a>(buf: &'a mut [u8], seq: u16, a: &Unprovisioned) -> Option<&'a [u8]> {
    let b = buf.get_mut(..UNPROVISIONED_LEN)?;
    header(b, Kind::Unprovisioned, seq);
    b[HEADER..HEADER + 6].copy_from_slice(&a.mac);
    b[HEADER + 6] = a.image;
    b[HEADER + 7..].copy_from_slice(&a.firmware);
    Some(b)
}

/// Read an Unprovisioned announcement.
pub fn parse_unprovisioned(data: &[u8]) -> Option<Unprovisioned> {
    let s = inspect(data).ok()?;
    if s.kind != Kind::Unprovisioned {
        return None;
    }
    let mut mac = [0u8; 6];
    mac.copy_from_slice(&data[HEADER..HEADER + 6]);
    let mut firmware = [0u8; FIRMWARE_LEN];
    firmware.copy_from_slice(&data[HEADER + 7..UNPROVISIONED_LEN]);
    Some(Unprovisioned { mac, image: data[HEADER + 6], firmware })
}

// ------------------------------------------------------------- internals --

pub(crate) fn header(buf: &mut [u8], kind: Kind, seq: u16) {
    buf[..2].copy_from_slice(&MAGIC);
    buf[2] = VERSION_V3;
    buf[3] = kind as u8;
    buf[4..6].copy_from_slice(&seq.to_le_bytes());
}

fn read_stamp(data: &[u8], shape: &Shape) -> Option<Stamp> {
    let at = shape.stamp_at()?;
    Some(Stamp::read(&data[at..at + STAMP_LEN]))
}

/// Constant time: an attacker must not learn how many bytes matched.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    kitkeys::ct_eq(a, b)
}

fn tag(key: &LinkKey, src: &Mac, signed: &[u8]) -> [u8; 32] {
    key.mac(&[src, signed])
}

/// Append stamp and 12-byte tag to `buf[..len]` (header and payload already
/// there). Returns the sealed length.
fn seal(key: &LinkKey, src: &Mac, buf: &mut [u8], len: usize, stamp: &Stamp) -> Option<usize> {
    let end = len.checked_add(TRAILER)?;
    if len < HEADER || end > buf.len() || end > MAX_PACKET {
        return None;
    }
    stamp.write(&mut buf[len..len + STAMP_LEN]);
    let t = tag(key, src, &buf[..len + STAMP_LEN]);
    buf[len + STAMP_LEN..end].copy_from_slice(&t[..TAG_LEN]);
    Some(end)
}

/// Check the 12-byte tag of a Sync, Frames or Command packet.
fn verify12(key: &LinkKey, src: &Mac, data: &[u8], shape: &Shape) -> bool {
    let split = shape.signed_end();
    let want = tag(key, src, &data[..split]);
    ct_eq(&want[..TAG_LEN], &data[split..])
}

/// Beacon tag of one slot.
fn beacon_tag(key: &LinkKey, gateway: &Mac, fixed: &[u8]) -> [u8; BEACON_TAG_LEN] {
    let m = tag(key, gateway, fixed);
    let mut t = [0u8; BEACON_TAG_LEN];
    t.copy_from_slice(&m[..BEACON_TAG_LEN]);
    t
}

/// The node's two keys for one slot, from its node key.
fn split(key: &NodeKey) -> (LinkKey, LinkKey) {
    (key.up(), key.down())
}
