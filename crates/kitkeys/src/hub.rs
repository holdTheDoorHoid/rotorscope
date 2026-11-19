//! The hub's `rskit` partition (8 KB, two 4 KB sectors):
//!
//! ```text
//! 0x0000  master record, copy A  (64 bytes)
//! 0x0800  master record, copy B
//! 0x1000  roster record, copy A  (240 bytes)
//! 0x1800  roster record, copy B
//! ```
//!
//! The master and the roster are separate records in separate sectors so
//! that `link-key hub-roster` can rewrite the roster (sector 1) without the
//! master ever travelling again (design section 4.5: "roster only, never
//! the master").
//!
//! Master record:
//!
//! ```text
//! 0  magic "RSH1"           4
//! 4  type (1 keyed, 2 open) 1
//! 5  kit_id                 4
//! 9  M (zero: in the secure element) 32
//! 41 reserved, zero         19
//! 60 CRC-32 (LE) of 0..60   4
//! ```
//!
//! Roster record:
//!
//! ```text
//! 0   magic "RSR1"          4
//! 4   kit_id                4
//! 8   count (0..16)         1
//! 9   reserved, zero        3
//! 12  16 entries of 14 bytes: slot, gen (u16 LE), mac (6), key_check (4),
//!     revoked (0/1); the first `count` in increasing slot order, the rest zero
//! 236 CRC-32 (LE) of 0..236 4
//! ```
//!
//! An open hub has an open master record (type 2, zeros) and an empty
//! roster with kit id zero.

use crate::roster::{Entry, Roster};
use crate::{KEY_LEN, KeyCheck, KitId, MasterKey, SLOTS, crc32, wipe};

pub const MASTER_MAGIC: [u8; 4] = *b"RSH1";
pub const ROSTER_MAGIC: [u8; 4] = *b"RSR1";
pub const TYPE_KEYED: u8 = 1;
pub const TYPE_OPEN: u8 = 2;
pub const MASTER_LEN: usize = 64;
pub const ENTRY_LEN: usize = 14;
pub const ROSTER_LEN: usize = 12 + SLOTS * ENTRY_LEN + 4;
pub const PARTITION_LEN: usize = 0x2000;
pub const SECTOR_LEN: usize = 0x1000;
/// Offsets of the master copies (sector 0).
pub const MASTER_OFFSETS: [usize; 2] = [0x0000, 0x0800];
/// Offset of the roster sector inside `rskit`.
pub const ROSTER_SECTOR: usize = 0x1000;
/// Offsets of the roster copies inside the roster sector.
pub const ROSTER_OFFSETS: [usize; 2] = [0x000, 0x800];

/// The master record's content.
pub enum Master {
    /// A keyed kit. `master` is `None` when the secure element holds it.
    Keyed { kit_id: KitId, master: Option<MasterKey> },
    Open,
}

impl core::fmt::Debug for Master {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Master::Keyed { kit_id, master } => f
                .debug_struct("Keyed")
                .field("kit_id", kit_id)
                .field("master", &if master.is_some() { "in flash" } else { "in element" })
                .finish(),
            Master::Open => f.write_str("Open"),
        }
    }
}

fn seal<const N: usize>(r: &mut [u8; N]) {
    let crc = crc32(&r[..N - 4]);
    r[N - 4..].copy_from_slice(&crc.to_le_bytes());
}

fn crc_ok(c: &[u8]) -> bool {
    let n = c.len();
    u32::from_le_bytes([c[n - 4], c[n - 3], c[n - 2], c[n - 1]]) == crc32(&c[..n - 4])
}

/// Encode a master record. With `master` the kit id is computed from it;
/// pass `None` and the element's kit id for an element-backed hub.
pub fn encode_master_keyed(master: Option<&MasterKey>, element_kit_id: KitId) -> MasterBytes {
    let mut r = [0u8; MASTER_LEN];
    r[..4].copy_from_slice(&MASTER_MAGIC);
    r[4] = TYPE_KEYED;
    let kit_id = master.map_or(element_kit_id, MasterKey::kit_id);
    r[5..9].copy_from_slice(&kit_id.0);
    if let Some(m) = master {
        r[9..41].copy_from_slice(m.bytes());
    }
    seal(&mut r);
    let out = MasterBytes(r);
    wipe(&mut r);
    out
}

pub fn encode_master_open() -> MasterBytes {
    let mut r = [0u8; MASTER_LEN];
    r[..4].copy_from_slice(&MASTER_MAGIC);
    r[4] = TYPE_OPEN;
    seal(&mut r);
    MasterBytes(r)
}

/// An encoded master record; wiped on drop, no `Debug`.
pub struct MasterBytes(pub [u8; MASTER_LEN]);

impl Drop for MasterBytes {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

/// Encode the roster record (`kit_id` zero for an open hub).
pub fn encode_roster(kit_id: KitId, roster: &Roster) -> [u8; ROSTER_LEN] {
    let mut r = [0u8; ROSTER_LEN];
    r[..4].copy_from_slice(&ROSTER_MAGIC);
    r[4..8].copy_from_slice(&kit_id.0);
    let mut n = 0usize;
    for e in roster.entries() {
        let o = 12 + n * ENTRY_LEN;
        r[o] = e.slot;
        r[o + 1..o + 3].copy_from_slice(&e.generation.to_le_bytes());
        r[o + 3..o + 9].copy_from_slice(&e.mac);
        r[o + 9..o + 13].copy_from_slice(&e.key_check.0);
        r[o + 13] = u8::from(e.revoked);
        n += 1;
    }
    r[8] = n as u8;
    seal(&mut r);
    r
}

/// The whole `rskit` image: both master copies, both roster copies, the
/// rest erased. Wiped on drop.
pub struct KitImage(pub [u8; PARTITION_LEN]);

impl Drop for KitImage {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

pub fn kit_image(master: &MasterBytes, roster: &[u8; ROSTER_LEN]) -> KitImage {
    let mut p = KitImage([0xFF; PARTITION_LEN]);
    for o in MASTER_OFFSETS {
        p.0[o..o + MASTER_LEN].copy_from_slice(&master.0);
    }
    p.0[ROSTER_SECTOR..].copy_from_slice(&roster_sector(roster));
    p
}

/// The roster sector alone (what `hub-roster` writes at `rskit + 0x1000`).
pub fn roster_sector(roster: &[u8; ROSTER_LEN]) -> [u8; SECTOR_LEN] {
    let mut s = [0xFF; SECTOR_LEN];
    for o in ROSTER_OFFSETS {
        s[o..o + ROSTER_LEN].copy_from_slice(roster);
    }
    s
}

/// Outcome of reading the master record.
#[derive(Debug)]
pub enum MasterState {
    Good(Master),
    /// Both copies erased.
    Missing,
    /// No good copy, or a good CRC around content that does not add up
    /// (unknown type, non-zero reserved bytes, a master whose kit id is not
    /// the recorded one).
    Damaged,
}

impl MasterState {
    pub fn name(&self) -> &'static str {
        match self {
            MasterState::Good(Master::Keyed { .. }) => "keyed",
            MasterState::Good(Master::Open) => "open",
            MasterState::Missing => "missing",
            MasterState::Damaged => "damaged",
        }
    }
}

fn erased(c: &[u8]) -> bool {
    c.iter().all(|&b| b == 0xFF) || c.iter().all(|&b| b == 0)
}

fn decode_master(c: &[u8]) -> Option<Master> {
    if c[..4] != MASTER_MAGIC || !crc_ok(c) || c[41..60].iter().any(|&b| b != 0) {
        return None;
    }
    match c[4] {
        TYPE_OPEN if c[5..60].iter().all(|&b| b == 0) => Some(Master::Open),
        TYPE_KEYED => {
            let kit_id = KitId([c[5], c[6], c[7], c[8]]);
            let mut m = [0u8; KEY_LEN];
            m.copy_from_slice(&c[9..41]);
            let master = MasterKey::from_bytes(m);
            wipe(&mut m);
            if let Some(mk) = &master
                && mk.kit_id() != kit_id
            {
                return None;
            }
            Some(Master::Keyed { kit_id, master })
        }
        _ => None,
    }
}

/// Read the master record from the `rskit` partition (first good copy).
pub fn read_master(partition: &[u8]) -> MasterState {
    if partition.len() < MASTER_OFFSETS[1] + MASTER_LEN {
        return MasterState::Damaged;
    }
    let mut all_erased = true;
    for o in MASTER_OFFSETS {
        let c = &partition[o..o + MASTER_LEN];
        if let Some(m) = decode_master(c) {
            return MasterState::Good(m);
        }
        all_erased &= erased(c);
    }
    if all_erased { MasterState::Missing } else { MasterState::Damaged }
}

/// Outcome of reading the roster record.
#[derive(Debug, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)] // no heap to box into; read once at boot
pub enum RosterState {
    Good(Roster),
    /// Both copies erased (a keyed hub with no nodes yet).
    Missing,
    Damaged,
    /// A good roster made for another kit: not used.
    OtherKit(KitId),
}

fn decode_roster(c: &[u8]) -> Option<(KitId, Roster)> {
    if c[..4] != ROSTER_MAGIC || !crc_ok(c) || c[9..12].iter().any(|&b| b != 0) {
        return None;
    }
    let kit_id = KitId([c[4], c[5], c[6], c[7]]);
    let n = usize::from(c[8]);
    if n > SLOTS {
        return None;
    }
    let mut roster = Roster::new();
    let mut last: Option<u8> = None;
    for i in 0..SLOTS {
        let e = &c[12 + i * ENTRY_LEN..12 + (i + 1) * ENTRY_LEN];
        if i >= n {
            if e.iter().any(|&b| b != 0) {
                return None;
            }
            continue;
        }
        if e[13] > 1 || last.is_some_and(|l| e[0] <= l) {
            return None;
        }
        last = Some(e[0]);
        let mut mac = [0u8; 6];
        mac.copy_from_slice(&e[3..9]);
        roster
            .insert(Entry {
                slot: e[0],
                generation: u16::from_le_bytes([e[1], e[2]]),
                mac,
                key_check: KeyCheck([e[9], e[10], e[11], e[12]]),
                revoked: e[13] == 1,
            })
            .ok()?;
    }
    Some((kit_id, roster))
}

/// Read the roster from the roster sector (`rskit + 0x1000`) for the kit
/// `kit_id` (from the master record or the element).
pub fn read_roster(sector: &[u8], kit_id: KitId) -> RosterState {
    if sector.len() < ROSTER_OFFSETS[1] + ROSTER_LEN {
        return RosterState::Damaged;
    }
    let mut all_erased = true;
    for o in ROSTER_OFFSETS {
        let c = &sector[o..o + ROSTER_LEN];
        if let Some((k, r)) = decode_roster(c) {
            return if k == kit_id { RosterState::Good(r) } else { RosterState::OtherKit(k) };
        }
        all_erased &= erased(c);
    }
    if all_erased { RosterState::Missing } else { RosterState::Damaged }
}
