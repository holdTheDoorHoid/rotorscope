//! The node's provisioning partition `rskey` (4 KB, data, subtype 0x40):
//! two copies of a 64-byte record, at 0x000 and 0x800. Nothing the firmware
//! does writes to it; `link-key node` and `link-key open` write the whole
//! partition with `espflash write-bin`.
//!
//! ```text
//! 0  magic "RSK1"            4
//! 4  type (1 node, 2 open)   1
//! 5  slot                    1
//! 6  gen, u16 LE             2
//! 8  mac                     6
//! 14 kit_id                  4
//! 18 K_node                  32
//! 50 key_check               4
//! 54 reserved, zero          6
//! 60 CRC-32 (LE) of 0..60    4
//! ```
//!
//! An open record has type 2 and zeros in every field from `slot` to
//! `reserved`; its CRC is still computed, so damage is still seen.
//!
//! At boot ([`read`]) the first copy with a good CRC wins; its MAC must
//! match the board's eFuse MAC.

use crate::{KeyCheck, KitId, KeySource, Mac, NodeIdent, NodeKey, crc32, wipe};

pub const MAGIC: [u8; 4] = *b"RSK1";
pub const TYPE_NODE: u8 = 1;
pub const TYPE_OPEN: u8 = 2;
/// One record.
pub const RECORD_LEN: usize = 64;
/// The partition.
pub const PARTITION_LEN: usize = 0x1000;
/// Where the two copies sit in the partition.
pub const COPY_OFFSETS: [usize; 2] = [0x000, 0x800];

/// A keyed node record (what a node holds).
pub struct NodeRecord {
    pub ident: NodeIdent,
    pub kit_id: KitId,
    pub key: NodeKey,
    pub key_check: KeyCheck,
}

impl core::fmt::Debug for NodeRecord {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("NodeRecord")
            .field("ident", &self.ident)
            .field("kit_id", &self.kit_id)
            .field("key", &self.key)
            .field("key_check", &self.key_check)
            .finish()
    }
}

impl NodeRecord {
    /// Make the record for `ident` from the kit's key source.
    pub fn provision<S: KeySource>(src: &mut S, ident: NodeIdent) -> Result<Self, S::Error> {
        let kit_id = src.kit_id()?;
        let key = src.derive(&ident)?;
        let key_check = key.check();
        Ok(NodeRecord { ident, kit_id, key, key_check })
    }

    /// What the board prints at boot after `auth`:
    /// `kit:<kit_id> slot:<n> generation:<g> check:<key_check>`.
    pub fn auth_summary(&self) -> AuthSummary {
        AuthSummary { kit_id: self.kit_id, slot: self.ident.slot, generation: self.ident.generation, key_check: self.key_check }
    }
}

/// The public part of a keyed record, for the boot line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthSummary {
    pub kit_id: KitId,
    pub slot: u8,
    pub generation: u16,
    pub key_check: KeyCheck,
}

impl core::fmt::Display for AuthSummary {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "kit:{} slot:{} gen:{} check:{}", self.kit_id, self.slot, self.generation, self.key_check)
    }
}

/// One encoded record. Holds a key when keyed: wiped on drop, no `Debug`.
pub struct RecordBytes(pub [u8; RECORD_LEN]);

impl Drop for RecordBytes {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

fn seal(r: &mut [u8; RECORD_LEN]) {
    let crc = crc32(&r[..60]);
    r[60..].copy_from_slice(&crc.to_le_bytes());
}

/// Encode a keyed record.
pub fn encode(rec: &NodeRecord) -> RecordBytes {
    let mut r = [0u8; RECORD_LEN];
    r[..4].copy_from_slice(&MAGIC);
    r[4] = TYPE_NODE;
    r[5] = rec.ident.slot;
    r[6..8].copy_from_slice(&rec.ident.generation.to_le_bytes());
    r[8..14].copy_from_slice(&rec.ident.mac);
    r[14..18].copy_from_slice(&rec.kit_id.0);
    r[18..50].copy_from_slice(rec.key.bytes());
    r[50..54].copy_from_slice(&rec.key_check.0);
    seal(&mut r);
    let out = RecordBytes(r);
    wipe(&mut r);
    out
}

/// Encode an open record.
pub fn encode_open() -> RecordBytes {
    let mut r = [0u8; RECORD_LEN];
    r[..4].copy_from_slice(&MAGIC);
    r[4] = TYPE_OPEN;
    seal(&mut r);
    RecordBytes(r)
}

/// The whole partition image: both copies, everything else erased (0xFF).
/// Wiped on drop.
pub struct PartitionImage(pub [u8; PARTITION_LEN]);

impl Drop for PartitionImage {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

pub fn partition_image(rec: &RecordBytes) -> PartitionImage {
    let mut p = PartitionImage([0xFF; PARTITION_LEN]);
    for o in COPY_OFFSETS {
        p.0[o..o + RECORD_LEN].copy_from_slice(&rec.0);
    }
    p
}

/// What one copy holds.
enum CopyState {
    Erased,
    Bad,
    Node(NodeRecord),
    Open,
}

fn decode_copy(c: &[u8]) -> CopyState {
    if c.iter().all(|&b| b == 0xFF) || c.iter().all(|&b| b == 0) {
        return CopyState::Erased;
    }
    let crc = u32::from_le_bytes([c[60], c[61], c[62], c[63]]);
    if c[..4] != MAGIC || crc32(&c[..60]) != crc {
        return CopyState::Bad;
    }
    match c[4] {
        TYPE_OPEN if c[5..60].iter().all(|&b| b == 0) => CopyState::Open,
        TYPE_NODE if c[54..60].iter().all(|&b| b == 0) => {
            let slot = c[5];
            if usize::from(slot) >= crate::SLOTS {
                return CopyState::Bad;
            }
            let mut k = [0u8; 32];
            k.copy_from_slice(&c[18..50]);
            let key = NodeKey::from_bytes(k);
            wipe(&mut k);
            let key_check = KeyCheck([c[50], c[51], c[52], c[53]]);
            // The check value must belong to the key: the boot line
            // prints it as this key's name.
            if key.check() != key_check {
                return CopyState::Bad;
            }
            let mut mac = [0u8; 6];
            mac.copy_from_slice(&c[8..14]);
            CopyState::Node(NodeRecord {
                ident: NodeIdent { slot, generation: u16::from_le_bytes([c[6], c[7]]), mac },
                kit_id: KitId([c[14], c[15], c[16], c[17]]),
                key,
                key_check,
            })
        }
        _ => CopyState::Bad,
    }
}

/// The outcome at boot (design section 4.4).
#[derive(Debug)]
pub enum State {
    /// A good keyed record for this board.
    Keyed(NodeRecord),
    /// An open record: unauthenticated link (version 1 packets).
    Open,
    /// Both copies erased: never provisioned.
    Missing,
    /// No copy has a good CRC (and they are not both erased).
    Damaged,
    /// A good record made for another board (`record_mac`).
    WrongBoard { record_mac: Mac },
}

impl State {
    /// `keyed`, `open`, `missing`, `damaged` or `wrong_board`: what the board
    /// prints on USB.
    pub fn name(&self) -> &'static str {
        match self {
            State::Keyed(_) => "keyed",
            State::Open => "open",
            State::Missing => "missing",
            State::Damaged => "damaged",
            State::WrongBoard { .. } => "wrong_board",
        }
    }
}

/// Read the two copies (`a` at 0x000, `b` at 0x800) on the board whose
/// eFuse station MAC is `efuse_mac`. The first copy with a good CRC wins,
/// even when the second is also good and differs.
pub fn read_copies(a: &[u8; RECORD_LEN], b: &[u8; RECORD_LEN], efuse_mac: &Mac) -> State {
    let ca = decode_copy(a);
    let chosen = match ca {
        CopyState::Node(_) | CopyState::Open => ca,
        _ => match decode_copy(b) {
            g @ (CopyState::Node(_) | CopyState::Open) => g,
            cb => {
                return if matches!(ca, CopyState::Erased) && matches!(cb, CopyState::Erased) {
                    State::Missing
                } else {
                    State::Damaged
                };
            }
        },
    };
    match chosen {
        CopyState::Open => State::Open,
        CopyState::Node(r) => {
            if r.ident.mac == *efuse_mac {
                State::Keyed(r)
            } else {
                State::WrongBoard { record_mac: r.ident.mac }
            }
        }
        _ => unreachable!(),
    }
}

/// [`read_copies`] on the whole partition (at least 0x840 bytes).
pub fn read(partition: &[u8], efuse_mac: &Mac) -> State {
    if partition.len() < COPY_OFFSETS[1] + RECORD_LEN {
        return State::Damaged;
    }
    let mut a = [0u8; RECORD_LEN];
    let mut b = [0u8; RECORD_LEN];
    a.copy_from_slice(&partition[COPY_OFFSETS[0]..COPY_OFFSETS[0] + RECORD_LEN]);
    b.copy_from_slice(&partition[COPY_OFFSETS[1]..COPY_OFFSETS[1] + RECORD_LEN]);
    let s = read_copies(&a, &b, efuse_mac);
    wipe(&mut a);
    wipe(&mut b);
    s
}
