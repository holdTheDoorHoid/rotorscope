//! The hub's boot counter, `rsboot` (8 KB: two 4 KB sectors). An
//! append-only log: each boot writes one 8-byte entry with the next value,
//! and a sector is erased only when the log moves into it.
//!
//! ```text
//! entry = value (u32 LE) | CRC-32 (LE) of "RSB1" | value
//! ```
//!
//! Erased flash (all 0xFF) is an empty entry. An entry whose check does not
//! match is torn (power lost while writing) or damaged: it is skipped and
//! its place is never reused. The current value is the largest good one in
//! either sector; the next entry goes after the last written place in that
//! sector, or, when that sector is full, at the start of the other sector
//! after erasing it. So the latest value is in flash at every moment.
//!
//! `link-key hub` seeds the log with a random value below 2^31 from the
//! computer (the hub has no proven random source). Values must be unique
//! per boot, not secret.

use crate::crc32;

pub const ENTRY_LEN: usize = 8;
pub const SECTOR_LEN: usize = 0x1000;
pub const SECTORS: usize = 2;
pub const PARTITION_LEN: usize = SECTOR_LEN * SECTORS;
pub const ENTRIES_PER_SECTOR: usize = SECTOR_LEN / ENTRY_LEN;
/// Seeds are below this, leaving 2^31 boots of headroom.
pub const SEED_LIMIT: u32 = 1 << 31;
/// The largest value ever written (0xFFFF_FFFF looks like erased flash).
pub const MAX_VALUE: u32 = u32::MAX - 1;

const TAG: &[u8; 4] = b"RSB1";

/// One encoded entry.
pub fn entry(value: u32) -> [u8; ENTRY_LEN] {
    let v = value.to_le_bytes();
    let c = crc32(&[TAG[0], TAG[1], TAG[2], TAG[3], v[0], v[1], v[2], v[3]]);
    let mut e = [0u8; ENTRY_LEN];
    e[..4].copy_from_slice(&v);
    e[4..].copy_from_slice(&c.to_le_bytes());
    e
}

fn decode(e: &[u8]) -> Slot {
    if e.iter().all(|&b| b == 0xFF) {
        return Slot::Empty;
    }
    let v = u32::from_le_bytes([e[0], e[1], e[2], e[3]]);
    if v <= MAX_VALUE && entry(v) == e[..ENTRY_LEN] { Slot::Good(v) } else { Slot::Bad }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Slot {
    Empty,
    Bad,
    Good(u32),
}

/// The partition image `link-key hub` writes: `seed` first, all else erased.
pub fn seed_image(seed: u32) -> [u8; PARTITION_LEN] {
    let mut p = [0xFF; PARTITION_LEN];
    p[..ENTRY_LEN].copy_from_slice(&entry(seed));
    p
}

/// Turn 4 random bytes into a seed in `1..SEED_LIMIT`.
pub fn seed_from_random(r: [u8; 4]) -> u32 {
    (u32::from_le_bytes(r) % (SEED_LIMIT - 1)) + 1
}

/// Where and what to write for this boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NextBoot {
    /// The value to write and use (the boot's epoch).
    pub value: u32,
    /// Byte offset in the partition of the entry to write.
    pub offset: usize,
    /// Erase this sector (index) before writing.
    pub erase_sector: Option<usize>,
}

/// What the log says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// The latest value, and how to record the next boot.
    Good { latest: u32, next: NextBoot },
    /// Both sectors erased: never provisioned.
    Missing,
    /// Something is written but no entry is good.
    Damaged,
    /// The latest value is [`MAX_VALUE`]: provision the hub again.
    Exhausted { latest: u32 },
}

impl State {
    pub fn name(&self) -> &'static str {
        match self {
            State::Good { .. } => "good",
            State::Missing => "missing",
            State::Damaged => "damaged",
            State::Exhausted { .. } => "exhausted",
        }
    }
}

/// Read the log (the whole 8 KB partition).
pub fn read(partition: &[u8]) -> State {
    if partition.len() < PARTITION_LEN {
        return State::Damaged;
    }
    let mut best: Option<(u32, usize)> = None; // (value, sector)
    let mut anything = false;
    for s in 0..SECTORS {
        for i in 0..ENTRIES_PER_SECTOR {
            let o = s * SECTOR_LEN + i * ENTRY_LEN;
            match decode(&partition[o..o + ENTRY_LEN]) {
                Slot::Empty => {}
                Slot::Bad => anything = true,
                Slot::Good(v) => {
                    anything = true;
                    if best.is_none_or(|(b, _)| v > b) {
                        best = Some((v, s));
                    }
                }
            }
        }
    }
    let Some((latest, sector)) = best else {
        return if anything { State::Damaged } else { State::Missing };
    };
    if latest >= MAX_VALUE {
        return State::Exhausted { latest };
    }
    // After the last written place (good or torn) in the latest's sector.
    let base = sector * SECTOR_LEN;
    let used = (0..ENTRIES_PER_SECTOR)
        .rev()
        .find(|&i| decode(&partition[base + i * ENTRY_LEN..base + (i + 1) * ENTRY_LEN]) != Slot::Empty)
        .map_or(0, |i| i + 1);
    let next = if used < ENTRIES_PER_SECTOR {
        NextBoot { value: latest + 1, offset: base + used * ENTRY_LEN, erase_sector: None }
    } else {
        let other = (sector + 1) % SECTORS;
        NextBoot { value: latest + 1, offset: other * SECTOR_LEN, erase_sector: Some(other) }
    };
    State::Good { latest, next }
}

/// Apply `next` to an in-memory copy of the partition (what the firmware
/// does to flash; used by tests and simulations).
pub fn apply(partition: &mut [u8], next: &NextBoot) {
    if let Some(s) = next.erase_sector {
        partition[s * SECTOR_LEN..(s + 1) * SECTOR_LEN].fill(0xFF);
    }
    let e = entry(next.value);
    // Flash programming only clears bits.
    for (d, s) in partition[next.offset..next.offset + ENTRY_LEN].iter_mut().zip(e) {
        *d &= s;
    }
}
