//! Finding the provisioning partition `rskey` (docs/LINK-KEYS-DESIGN.md
//! section 4.4) in the board's partition table, without reading the whole
//! table into RAM.
//!
//! The ESP-IDF partition table sits at [`TABLE_OFFSET`] and is a list of
//! 32-byte entries: magic `AA 50`, type, subtype, offset (u32 LE), size (u32
//! LE), a 16-byte NUL-padded label and 4 bytes of flags. It ends at the first
//! entry without that magic (the MD5 entry `EB EB`, or erased flash). The node
//! reads it one entry at a time ([`find`]), so the search costs 32 bytes of
//! stack instead of the 3 KB table buffer `esp-bootloader-esp-idf` wants,
//! which matters on node-sdr.
//!
//! `rskey` is a data partition (type 1) of subtype [`SUBTYPE`] (0x40), 4 KB,
//! labelled `rskey`. A partition with that label but another type, subtype or
//! a smaller size is not taken ([`Found::Unusable`]): the node then behaves as
//! unprovisioned and says why. Nothing here, or anywhere in the firmware,
//! writes it.
//!
//! Where it is (the offset is fixed for good, so an app reflash with the same
//! table never moves it): [`MULTI_IMAGE_OFFSET`] in `tools/partitions-node.csv`
//! (multi-image C5 nodes), [`SINGLE_IMAGE_OFFSET`] in
//! `tools/partitions-single.csv` (S3 Wi-Fi, C3 aero and single-image C5
//! boards). The firmware does not rely on either number: it goes by the table.

/// Where the bootloader reads the partition table (ESP-IDF's default, which
/// `esp-bootloader-esp-idf` and espflash use too).
pub const TABLE_OFFSET: u32 = 0x8000;
/// Longest partition table, bytes (ESP-IDF `ESP_PARTITION_TABLE_MAX_LEN`).
pub const TABLE_MAX_LEN: u32 = 0xC00;
/// One table entry.
pub const ENTRY_LEN: usize = 32;
/// Most entries a table can hold.
pub const MAX_ENTRIES: u32 = TABLE_MAX_LEN / ENTRY_LEN as u32;
/// First two bytes of a partition entry.
pub const ENTRY_MAGIC: [u8; 2] = [0xAA, 0x50];

/// The partition's label, type and subtype.
pub const LABEL: &str = "rskey";
pub const TYPE_DATA: u8 = 1;
pub const SUBTYPE: u8 = 0x40;
/// Its size (`kitkeys::record::PARTITION_LEN`; a host test holds them equal).
pub const PARTITION_LEN: u32 = 0x1000;
/// Where in the partition the two record copies are
/// (`kitkeys::record::COPY_OFFSETS`).
pub const COPY_OFFSETS: [u32; 2] = [0x000, 0x800];

/// `rskey` in `tools/partitions-node.csv`.
pub const MULTI_IMAGE_OFFSET: u32 = 0x61_1000;
/// `rskey` in `tools/partitions-single.csv`.
pub const SINGLE_IMAGE_OFFSET: u32 = 0x3F_0000;

/// One partition table entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub ty: u8,
    pub subtype: u8,
    pub offset: u32,
    pub size: u32,
    label: [u8; 16],
}

impl Entry {
    /// The label up to its first NUL ("" if it is not UTF-8).
    pub fn label(&self) -> &str {
        let n = self.label.iter().position(|&b| b == 0).unwrap_or(self.label.len());
        core::str::from_utf8(&self.label[..n]).unwrap_or("")
    }
}

/// Read one raw entry: `None` at the end of the table (no entry magic).
pub fn parse_entry(raw: &[u8; ENTRY_LEN]) -> Option<Entry> {
    if raw[..2] != ENTRY_MAGIC {
        return None;
    }
    let u32_at = |i: usize| u32::from_le_bytes([raw[i], raw[i + 1], raw[i + 2], raw[i + 3]]);
    let mut label = [0u8; 16];
    label.copy_from_slice(&raw[12..28]);
    Some(Entry { ty: raw[2], subtype: raw[3], offset: u32_at(4), size: u32_at(8), label })
}

/// What the search found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Found {
    /// The partition: read the record copies at `offset + COPY_OFFSETS[i]`.
    At { offset: u32 },
    /// No entry labelled `rskey` (a board flashed with an older table, or
    /// espflash's default one).
    NotInTable,
    /// An entry labelled `rskey` that is not a 4 KB data partition of
    /// subtype 0x40.
    Unusable { ty: u8, subtype: u8, size: u32 },
    /// A flash read failed.
    ReadError,
}

impl Found {
    /// One word for the boot line when there is no usable partition.
    pub const fn name(self) -> &'static str {
        match self {
            Found::At { .. } => "ok",
            Found::NotInTable => "no_partition",
            Found::Unusable { .. } => "bad_partition",
            Found::ReadError => "flash_read",
        }
    }
}

/// Walk the table with `read(address, entry)` (true when the read worked)
/// and find `rskey`. Reads at most [`MAX_ENTRIES`] entries.
pub fn find(mut read: impl FnMut(u32, &mut [u8; ENTRY_LEN]) -> bool) -> Found {
    let mut raw = [0u8; ENTRY_LEN];
    for i in 0..MAX_ENTRIES {
        if !read(TABLE_OFFSET + i * ENTRY_LEN as u32, &mut raw) {
            return Found::ReadError;
        }
        let Some(e) = parse_entry(&raw) else { return Found::NotInTable };
        if e.label() != LABEL {
            continue;
        }
        if e.ty != TYPE_DATA || e.subtype != SUBTYPE || e.size < PARTITION_LEN {
            return Found::Unusable { ty: e.ty, subtype: e.subtype, size: e.size };
        }
        return Found::At { offset: e.offset };
    }
    Found::NotInTable
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    fn entry(ty: u8, subtype: u8, offset: u32, size: u32, label: &str) -> [u8; ENTRY_LEN] {
        let mut e = [0u8; ENTRY_LEN];
        e[..2].copy_from_slice(&ENTRY_MAGIC);
        e[2] = ty;
        e[3] = subtype;
        e[4..8].copy_from_slice(&offset.to_le_bytes());
        e[8..12].copy_from_slice(&size.to_le_bytes());
        e[12..12 + label.len()].copy_from_slice(label.as_bytes());
        e
    }

    /// A flash image of a partition table (then erased flash).
    fn flash(entries: &[[u8; ENTRY_LEN]]) -> Vec<u8> {
        let mut f = std::vec![0xFFu8; TABLE_MAX_LEN as usize];
        for (i, e) in entries.iter().enumerate() {
            f[i * ENTRY_LEN..(i + 1) * ENTRY_LEN].copy_from_slice(e);
        }
        f
    }

    fn search(f: &[u8]) -> (Found, usize) {
        let mut reads = 0;
        let r = find(|at, out| {
            reads += 1;
            let i = (at - TABLE_OFFSET) as usize;
            match f.get(i..i + ENTRY_LEN) {
                Some(b) => {
                    out.copy_from_slice(b);
                    true
                }
                None => false,
            }
        });
        (r, reads)
    }

    /// Parse a partition CSV the way `gen_esp32part.py` / espflash read ours
    /// (only the forms the repository's tables use).
    fn csv_entries(csv: &str) -> Vec<[u8; ENTRY_LEN]> {
        let num = |s: &str| {
            let s = s.trim();
            s.strip_prefix("0x").map_or_else(|| s.parse().unwrap(), |h| u32::from_str_radix(h, 16).unwrap())
        };
        let mut out = Vec::new();
        for l in csv.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            let f: Vec<&str> = l.split(',').map(str::trim).collect();
            let ty = match f[1] {
                "app" => 0,
                "data" => 1,
                t => num(t) as u8,
            };
            let subtype = match f[2] {
                "factory" => 0,
                "ota_0" => 0x10,
                "ota_1" => 0x11,
                "ota_2" => 0x12,
                "ota" => 0,
                "phy" => 1,
                "nvs" => 2,
                "undefined" => 6,
                s => num(s) as u8,
            };
            out.push(entry(ty, subtype, num(f[3]), num(f[4]), f[0]));
        }
        out
    }

    fn table(name: &str) -> String {
        std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools").join(name)).unwrap()
    }

    #[test]
    fn finds_rskey_in_both_repository_tables() {
        for (name, at) in [("partitions-node.csv", MULTI_IMAGE_OFFSET), ("partitions-single.csv", SINGLE_IMAGE_OFFSET)] {
            let entries = csv_entries(&table(name));
            let (r, reads) = search(&flash(&entries));
            assert_eq!(r, Found::At { offset: at }, "{name}");
            assert!(reads <= entries.len(), "{name}: stops at rskey");
            // The two copies lie inside the partition.
            assert!(COPY_OFFSETS[1] + 64 <= PARTITION_LEN);
        }
    }

    /// The offsets are fixed for good: an app reflash with these tables
    /// leaves the record where the previous image found it, and no other
    /// partition overlaps it.
    #[test]
    fn repository_tables_keep_rskey_apart() {
        for (name, flash_size) in [("partitions-node.csv", 8u32 << 20), ("partitions-single.csv", 4 << 20)] {
            let entries: Vec<Entry> = csv_entries(&table(name)).iter().map(|e| parse_entry(e).unwrap()).collect();
            let key = entries.iter().find(|e| e.label() == LABEL).unwrap();
            assert_eq!((key.ty, key.subtype, key.size), (TYPE_DATA, SUBTYPE, PARTITION_LEN), "{name}");
            assert_eq!(key.offset % 0x1000, 0, "{name}: sector aligned");
            for e in &entries {
                assert!(e.offset + e.size <= flash_size, "{name}: {} fits the flash", e.label());
                if e.label() != LABEL {
                    let apart = e.offset + e.size <= key.offset || key.offset + key.size <= e.offset;
                    assert!(apart, "{name}: {} overlaps rskey", e.label());
                }
            }
            // The single-image table: one app, and no OTA data (nothing
            // to switch to; node-common refuses switches there).
            if name == "partitions-single.csv" {
                assert_eq!(entries.iter().filter(|e| e.ty == 0).count(), 1);
                assert!(!entries.iter().any(|e| e.ty == 1 && e.subtype == 0), "no otadata");
            }
        }
    }

    #[test]
    fn missing_and_unusable_partitions() {
        // espflash's default table: no rskey.
        let default = [entry(1, 2, 0x9000, 0x6000, "nvs"), entry(1, 1, 0xf000, 0x1000, "phy_init"), entry(0, 0, 0x10000, 0x3f0000, "factory")];
        assert_eq!(search(&flash(&default)).0, Found::NotInTable);
        // Erased flash, or an MD5 entry right away.
        assert_eq!(search(&flash(&[])).0, Found::NotInTable);
        let mut md5 = [0u8; ENTRY_LEN];
        md5[..2].copy_from_slice(&[0xEB, 0xEB]);
        assert_eq!(search(&flash(&[md5])).0, Found::NotInTable);
        // Labelled rskey but the wrong shape.
        for bad in [entry(1, 6, 0x611000, 0x1000, "rskey"), entry(0, 0x40, 0x611000, 0x1000, "rskey"), entry(1, 0x40, 0x611000, 0x800, "rskey")] {
            assert!(matches!(search(&flash(&[bad])).0, Found::Unusable { .. }));
        }
        // A label that only starts with "rskey" is someone else's.
        assert_eq!(search(&flash(&[entry(1, 0x40, 0x611000, 0x1000, "rskeyx")])).0, Found::NotInTable);
        // A read error.
        assert_eq!(find(|_, _| false), Found::ReadError);
        // A full table without rskey: every entry read, no more.
        let full: Vec<_> = (0..MAX_ENTRIES).map(|i| entry(1, 6, 0x10000 * i, 0x1000, "x")).collect();
        let (r, reads) = search(&flash(&full));
        assert_eq!((r, reads), (Found::NotInTable, MAX_ENTRIES as usize));
    }

    #[test]
    fn constants_match_kitkeys() {
        assert_eq!(PARTITION_LEN as usize, kitkeys::record::PARTITION_LEN);
        assert_eq!(COPY_OFFSETS.map(|o| o as usize), kitkeys::record::COPY_OFFSETS);
    }
}
