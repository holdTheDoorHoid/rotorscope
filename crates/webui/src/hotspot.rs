//! The hotspot password: derivation, the stored salt record, and where in
//! flash the salt lives. Pure logic; the hub's `hotspot.rs` does the I/O.
//!
//! The password is `SHA-256(DOMAIN || salt || OPTIONAL_UNIQUE_ID || MAC)`,
//! cut to 60 bits and written as `xxxx-xxxx-xxxx` in Crockford base32.
//!
//! - `salt` is 16 bytes from the chip's true random number generator, made
//!   on the first boot and kept in flash. It is what makes the password
//!   secret: the MAC is broadcast in every beacon, this code is public, and
//!   the `OPTIONAL_UNIQUE_ID` eFuse is blank on some chips. Making a new salt
//!   (BOOT held + `R` on USB) makes a new password, so a leaked one can be
//!   replaced.
//! - The unique ID and the MAC stay in the hash so a salt copied to another
//!   board (a flash image cloned whole) still gives a different password.

use sha2::{Digest, Sha256};

/// Salt length, bytes.
pub const SALT_LEN: usize = 16;
/// The salt as stored: magic, version, salt, check. Word-sized, so the
/// flash driver reads and writes it without a bounce buffer.
pub const RECORD_LEN: usize = 32;
/// Password length: 12 symbols and two dashes.
pub const PASSWORD_LEN: usize = 14;

const DOMAIN: &[u8] = b"rotorscope hotspot password v2";
const MAGIC: [u8; 4] = *b"RShs";
const VERSION: u8 = 1;

/// Crockford base32, lower case: no i, l, o, u, so nothing is mistaken for
/// 1 or 0 when read off a small screen and typed on a phone.
pub const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// The per-device password, ASCII.
pub fn derive_password(salt: &[u8; SALT_LEN], unique_id: &[u8; 16], mac: &[u8; 6]) -> [u8; PASSWORD_LEN] {
    let mut h = Sha256::new();
    h.update(DOMAIN);
    h.update(salt);
    h.update(unique_id);
    h.update(mac);
    let digest = h.finalize();
    let mut first = [0u8; 8];
    first.copy_from_slice(&digest[..8]);
    let bits = u64::from_be_bytes(first);
    let mut out = [0u8; PASSWORD_LEN];
    let mut at = 0;
    for i in 0..12 {
        if i > 0 && i % 4 == 0 {
            out[at] = b'-';
            at += 1;
        }
        out[at] = ALPHABET[((bits >> (59 - 5 * i)) & 0x1f) as usize];
        at += 1;
    }
    out
}

/// A salt worth using: not all one byte value (erased flash reads 0xff, a
/// broken RNG or a zeroed buffer gives 0x00).
pub fn salt_usable(salt: &[u8; SALT_LEN]) -> bool {
    salt.iter().any(|&b| b != salt[0])
}

fn check(head: &[u8]) -> [u8; 8] {
    let d = Sha256::digest(head);
    let mut c = [0u8; 8];
    c.copy_from_slice(&d[..8]);
    c
}

/// The record that holds `salt` in flash.
pub fn encode_record(salt: &[u8; SALT_LEN]) -> [u8; RECORD_LEN] {
    let mut r = [0u8; RECORD_LEN];
    r[..4].copy_from_slice(&MAGIC);
    r[4] = VERSION;
    r[8..24].copy_from_slice(salt);
    let c = check(&r[..24]);
    r[24..].copy_from_slice(&c);
    r
}

/// What the salt sector held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stored {
    Salt([u8; SALT_LEN]),
    /// Erased flash: first boot (or the sector was wiped).
    Erased,
    /// Something else: an interrupted write, another program's data.
    Corrupt,
}

pub fn decode_record(r: &[u8; RECORD_LEN]) -> Stored {
    if r.iter().all(|&b| b == 0xff) {
        return Stored::Erased;
    }
    if r[..4] != MAGIC || r[4] != VERSION || r[24..] != check(&r[..24]) {
        return Stored::Corrupt;
    }
    let mut salt = [0u8; SALT_LEN];
    salt.copy_from_slice(&r[8..24]);
    if salt_usable(&salt) { Stored::Salt(salt) } else { Stored::Corrupt }
}

/// Where the ESP-IDF bootloader (and espflash) put the partition table.
pub const TABLE_OFFSET: u32 = 0x8000;
/// One partition table entry, bytes.
pub const ENTRY_LEN: usize = 32;
/// Entries that fit in the table's 0xC00 bytes.
pub const MAX_ENTRIES: usize = 0xC00 / ENTRY_LEN;
/// Flash sector (erase unit), bytes.
pub const SECTOR: u32 = 4096;

/// The flash sector for the salt, from the partition table entries in
/// order: the **last sector of the `nvs` data partition** (type 1, subtype
/// 2). `None` if the table has no usable one.
///
/// Why there: the hub is flashed with espflash's default table (`nvs`
/// 0x9000+0x6000, `phy_init` 0xf000, `factory` app 0x10000 to the end of the
/// flash), so there is no free space after the app, and the app image is
/// rewritten on every flash. Nothing in this firmware uses NVS: esp-radio
/// 1.0.0-beta.1 runs the Wi-Fi blobs with NVS disabled (`nvs_enable: 0`, and
/// its `nvs_*` hooks are stubs that never touch flash). espflash writes only
/// the bootloader, the table and the app, so a reflash keeps the salt (and
/// the password); `espflash erase-flash` or `erase-parts nvs` makes a new one.
pub fn salt_sector(entries: impl IntoIterator<Item = [u8; ENTRY_LEN]>) -> Option<u32> {
    for e in entries.into_iter().take(MAX_ENTRIES) {
        if e[0] == 0xff && e[1] == 0xff {
            break; // end of table
        }
        if e[0] != 0xaa || e[1] != 0x50 {
            continue; // the MD5 entry (0xebeb), or junk
        }
        let (kind, subtype) = (e[2], e[3]);
        let offset = u32::from_le_bytes([e[4], e[5], e[6], e[7]]);
        let size = u32::from_le_bytes([e[8], e[9], e[10], e[11]]);
        if kind == 1 && subtype == 2 {
            let usable = offset >= TABLE_OFFSET + SECTOR
                && offset % SECTOR == 0
                && size >= SECTOR
                && size % SECTOR == 0
                && offset.checked_add(size).is_some();
            return usable.then(|| offset + size - SECTOR);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    fn entry(kind: u8, subtype: u8, offset: u32, size: u32, label: &str) -> [u8; ENTRY_LEN] {
        let mut e = [0u8; ENTRY_LEN];
        e[0] = 0xaa;
        e[1] = 0x50;
        e[2] = kind;
        e[3] = subtype;
        e[4..8].copy_from_slice(&offset.to_le_bytes());
        e[8..12].copy_from_slice(&size.to_le_bytes());
        e[12..12 + label.len()].copy_from_slice(label.as_bytes());
        e
    }

    const END: [u8; ENTRY_LEN] = [0xff; ENTRY_LEN];

    #[test]
    fn espflash_default_table_puts_the_salt_at_0xe000() {
        // espflash 4.7 `default_partition_table` for an S3 with 4 MB flash.
        let table = [
            entry(1, 2, 0x9000, 0x6000, "nvs"),
            entry(1, 1, 0xf000, 0x1000, "phy_init"),
            entry(0, 0, 0x10000, 0x3f_0000, "factory"),
            END,
        ];
        assert_eq!(salt_sector(table), Some(0xe000));
    }

    #[test]
    fn node_table_and_missing_nvs() {
        // tools/partitions-node.csv: nvs 0x9000+0x4000.
        let node = [entry(1, 2, 0x9000, 0x4000, "nvs"), entry(1, 0, 0xd000, 0x2000, "otadata"), END];
        assert_eq!(salt_sector(node), Some(0xc000));
        let none = [entry(0, 0, 0x10000, 0x10_0000, "factory"), END];
        assert_eq!(salt_sector(none), None);
        // An nvs entry after the end marker does not count.
        assert_eq!(salt_sector([END, entry(1, 2, 0x9000, 0x6000, "nvs")]), None);
        // Nonsense sizes and offsets are refused rather than trusted.
        assert_eq!(salt_sector([entry(1, 2, 0x9000, 0x800, "nvs")]), None);
        assert_eq!(salt_sector([entry(1, 2, 0x0, 0x6000, "nvs")]), None);
        assert_eq!(salt_sector([entry(1, 2, 0xffff_f000, 0x2000, "nvs")]), None);
    }

    #[test]
    fn record_round_trip_and_damage() {
        let salt = *b"0123456789abcdef";
        let r = encode_record(&salt);
        assert_eq!(decode_record(&r), Stored::Salt(salt));
        assert_eq!(decode_record(&[0xff; RECORD_LEN]), Stored::Erased);
        assert_eq!(decode_record(&[0x00; RECORD_LEN]), Stored::Corrupt);
        for i in 0..RECORD_LEN {
            let mut bad = r;
            bad[i] ^= 0x01;
            assert!(matches!(decode_record(&bad), Stored::Corrupt), "byte {i}");
        }
        // A record of an unusable salt is not accepted either.
        assert_eq!(decode_record(&encode_record(&[0; SALT_LEN])), Stored::Corrupt);
    }

    #[test]
    fn password_shape_and_dependence_on_the_salt() {
        let uid = [0u8; 16]; // blank eFuse: the salt alone must make it secret
        let mac = [0x24, 0x6f, 0x28, 0x01, 0x02, 0x03];
        let a = derive_password(&[1; SALT_LEN], &uid, &mac);
        let s = core::str::from_utf8(&a).unwrap();
        assert_eq!(s.len(), PASSWORD_LEN);
        assert_eq!((s.as_bytes()[4], s.as_bytes()[9]), (b'-', b'-'));
        assert!(s.bytes().filter(|&b| b != b'-').all(|b| ALPHABET.contains(&b)));
        // Same inputs, same password; any change in salt, ID or MAC, another.
        assert_eq!(a, derive_password(&[1; SALT_LEN], &uid, &mac));
        let mut salt2 = [1; SALT_LEN];
        salt2[15] = 2;
        assert_ne!(a, derive_password(&salt2, &uid, &mac));
        assert_ne!(a, derive_password(&[1; SALT_LEN], &[9; 16], &mac));
        assert_ne!(a, derive_password(&[1; SALT_LEN], &uid, &[0x24, 0x6f, 0x28, 0x01, 0x02, 0x04]));
        assert!(salt_usable(&salt2) && !salt_usable(&[0xff; SALT_LEN]) && !salt_usable(&[0; SALT_LEN]));
    }
}
