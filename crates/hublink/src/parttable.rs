//! The ESP-IDF partition table at flash `0x8000`, read by the hub to find
//! its `rskit` and `rsboot` partitions (`tools/partitions-hub.csv`) rather
//! than trusting a compiled-in offset: a hub still flashed with Elecrow's own
//! table (no such partitions) is then reported as such, never written to.
//!
//! Entry (32 bytes): magic `AA 50`, type, subtype, offset (u32 LE), size
//! (u32 LE), label (16, NUL-padded), flags (u32 LE). The table ends at an
//! erased entry or the MD5 entry (magic `EB EB`).

use crate::flash::SECTOR;

pub const TABLE_ADDR: u32 = 0x8000;
pub const TABLE_LEN: usize = 0xC00;
pub const ENTRY_LEN: usize = 32;
pub const TYPE_APP: u8 = 0;
pub const TYPE_DATA: u8 = 1;
/// The hub's partitions (`tools/partitions-hub.csv`).
pub const RSKIT: &str = "rskit";
pub const RSBOOT: &str = "rsboot";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub ptype: u8,
    pub subtype: u8,
    pub offset: u32,
    pub size: u32,
    pub label: [u8; 16],
}

impl Entry {
    pub fn name(&self) -> &[u8] {
        let n = self.label.iter().position(|&b| b == 0).unwrap_or(16);
        &self.label[..n]
    }

    fn end(&self) -> u64 {
        u64::from(self.offset) + u64::from(self.size)
    }
}

/// The entries of a table read from flash, in order.
pub fn entries(table: &[u8]) -> impl Iterator<Item = Entry> + '_ {
    table.as_chunks::<ENTRY_LEN>().0.iter().map_while(|e| {
        if e[0] != 0xAA || e[1] != 0x50 {
            return None; // erased, the MD5 entry, or not a table at all
        }
        let w = |i: usize| u32::from_le_bytes([e[i], e[i + 1], e[i + 2], e[i + 3]]);
        let mut label = [0u8; 16];
        label.copy_from_slice(&e[12..28]);
        Some(Entry { ptype: e[2], subtype: e[3], offset: w(4), size: w(8), label })
    })
}

/// Why a partition cannot be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartError {
    /// No valid entry at all: not a partition table.
    NoTable,
    /// No partition by that name (an older table, e.g. Elecrow's own).
    Missing,
    /// It is there, but not a data partition of exactly the size the
    /// records need, not sector-aligned, or it overlaps another partition
    /// or the table and bootloader below `0x9000`.
    Unsafe,
}

impl PartError {
    pub fn name(self) -> &'static str {
        match self {
            PartError::NoTable => "no partition table",
            PartError::Missing => "no such partition (flash with tools/partitions-hub.csv)",
            PartError::Unsafe => "partition entry not usable (type, size, alignment or overlap)",
        }
    }
}

/// Find the data partition `name` of exactly `size` bytes, and check it is
/// safe to write: sector-aligned, past the partition table, and overlapping
/// no other entry (in particular no app partition).
pub fn data_partition(table: &[u8], name: &str, size: u32) -> Result<Entry, PartError> {
    if entries(table).next().is_none() {
        return Err(PartError::NoTable);
    }
    let e = entries(table).find(|e| e.name() == name.as_bytes()).ok_or(PartError::Missing)?;
    let aligned = e.offset % SECTOR == 0 && e.size % SECTOR == 0;
    let past_table = u64::from(e.offset) >= u64::from(TABLE_ADDR) + 0x1000;
    let overlaps = entries(table)
        .filter(|o| o.name() != e.name() || o.offset != e.offset)
        .any(|o| u64::from(o.offset) < e.end() && u64::from(e.offset) < o.end());
    let twice = entries(table).filter(|o| o.name() == name.as_bytes()).count() > 1;
    if e.ptype != TYPE_DATA || e.size != size || !aligned || !past_table || overlaps || twice {
        return Err(PartError::Unsafe);
    }
    Ok(e)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::vec::Vec;

    pub fn entry(name: &str, ptype: u8, subtype: u8, offset: u32, size: u32) -> [u8; 32] {
        let mut e = [0u8; 32];
        e[0] = 0xAA;
        e[1] = 0x50;
        e[2] = ptype;
        e[3] = subtype;
        e[4..8].copy_from_slice(&offset.to_le_bytes());
        e[8..12].copy_from_slice(&size.to_le_bytes());
        e[12..12 + name.len()].copy_from_slice(name.as_bytes());
        e
    }

    pub fn table(entries: &[[u8; 32]]) -> Vec<u8> {
        let mut t = std::vec![0xFFu8; TABLE_LEN];
        for (i, e) in entries.iter().enumerate() {
            t[i * 32..i * 32 + 32].copy_from_slice(e);
        }
        // The MD5 entry espflash and gen_esp32part write after the last.
        let n = entries.len() * 32;
        t[n] = 0xEB;
        t[n + 1] = 0xEB;
        t
    }

    /// Elecrow's factory table (decoded from the backup) plus the two new
    /// partitions, exactly as `tools/partitions-hub.csv` lays them out.
    pub fn hub_table() -> Vec<u8> {
        table(&[
            entry("nvs", 1, 2, 0x9000, 0x4000),
            entry("otadata", 1, 0, 0xd000, 0x2000),
            entry("phy_init", 1, 1, 0xf000, 0x1000),
            entry("factory", 0, 0, 0x10000, 0xC00000),
            entry("rskit", 1, 6, 0xC10000, 0x2000),
            entry("rsboot", 1, 6, 0xC12000, 0x2000),
        ])
    }

    #[test]
    fn finds_the_hub_partitions() {
        let t = hub_table();
        let k = data_partition(&t, RSKIT, 0x2000).unwrap();
        assert_eq!((k.offset, k.size, k.subtype), (0xC10000, 0x2000, 6));
        let b = data_partition(&t, RSBOOT, 0x2000).unwrap();
        assert_eq!(b.offset, 0xC12000);
        assert_eq!(entries(&t).count(), 6);
    }

    /// The CSV in the repository says the same as the table the tests use.
    #[test]
    fn csv_matches() {
        let csv = include_str!("../../../tools/partitions-hub.csv");
        let rows: Vec<Vec<&str>> = csv
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| l.split(',').map(str::trim).collect())
            .collect();
        let num = |s: &str| u32::from_str_radix(s.trim_start_matches("0x"), 16).unwrap();
        let want: Vec<(String, u32, u32)> =
            entries(&hub_table()).map(|e| (String::from_utf8(e.name().to_vec()).unwrap(), e.offset, e.size)).collect();
        let got: Vec<(String, u32, u32)> = rows.iter().map(|r| (r[0].to_string(), num(r[3]), num(r[4]))).collect();
        assert_eq!(got, want);
    }

    #[test]
    fn elecrow_table_has_no_kit_partition() {
        let t = table(&[
            entry("nvs", 1, 2, 0x9000, 0x4000),
            entry("otadata", 1, 0, 0xd000, 0x2000),
            entry("phy_init", 1, 1, 0xf000, 0x1000),
            entry("factory", 0, 0, 0x10000, 0xC00000),
        ]);
        assert_eq!(data_partition(&t, RSKIT, 0x2000), Err(PartError::Missing));
        assert_eq!(data_partition(&[0xFF; TABLE_LEN], RSKIT, 0x2000), Err(PartError::NoTable));
    }

    /// A table that would point the boot counter at the application, the
    /// table or the bootloader, or at a wrong-sized area, is refused.
    #[test]
    fn unsafe_entries_are_refused() {
        let base = [
            entry("factory", 0, 0, 0x10000, 0xC00000),
            entry("rskit", 1, 6, 0xC10000, 0x2000),
        ];
        let cases: [[u8; 32]; 6] = [
            entry("rsboot", 1, 6, 0x10000, 0x2000),  // inside the app
            entry("rsboot", 1, 6, 0xC0F000, 0x2000), // straddles the app's end
            entry("rsboot", 1, 6, 0x8000, 0x2000),   // the table itself
            entry("rsboot", 1, 6, 0xC12000, 0x1000), // wrong size
            entry("rsboot", 0, 0x10, 0xC12000, 0x2000), // an app partition
            entry("rsboot", 1, 6, 0xC12800, 0x2000), // not sector-aligned
        ];
        for c in cases {
            let t = table(&[base[0], base[1], c]);
            assert_eq!(data_partition(&t, RSBOOT, 0x2000), Err(PartError::Unsafe), "{c:?}");
        }
        // Two entries with the name: refused.
        let t = table(&[base[0], base[1], entry("rsboot", 1, 6, 0xC12000, 0x2000), entry("rsboot", 1, 6, 0xC14000, 0x2000)]);
        assert_eq!(data_partition(&t, RSBOOT, 0x2000), Err(PartError::Unsafe));
        // Overlapping rskit: refused.
        let t = table(&[base[0], base[1], entry("rsboot", 1, 6, 0xC11000, 0x2000)]);
        assert_eq!(data_partition(&t, RSBOOT, 0x2000), Err(PartError::Unsafe));
    }
}
