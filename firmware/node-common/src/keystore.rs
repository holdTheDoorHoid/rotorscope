//! The node's provisioning record at boot (docs/LINK-KEYS-DESIGN.md 4.4,
//! feature `espnow`).
//!
//! Finds the `rskey` partition in the partition table
//! (`nodeimage::keystore::find`, one 32-byte entry at a time), reads its two
//! 64-byte record copies and hands them to `kitkeys::record::read_copies`
//! with the chip's eFuse station MAC: the first copy with a good CRC wins,
//! and a record made for another board is `wrong_board`. A board whose table
//! has no `rskey` (flashed with an older table, or espflash's default) is
//! `missing`; [`Keystore::found`] says which.
//!
//! Read only. Nothing in any firmware writes `rskey`: `tools/link-key` does,
//! with espflash. The copies are wiped from the stack after decoding.
//!
//! Flash access: `esp-storage` through `esp-bootloader-esp-idf`'s
//! `FlashStorage` (the same driver `Images` uses), on the S3, C5 and C3
//! alike; flash encryption is off on every board (WP-15).

use esp_bootloader_esp_idf::partitions::FlashStorage;
use kitkeys::Mac;
use kitkeys::record::{self, RECORD_LEN, State};
use nodeimage::keystore::{self as ks, Found};

/// What the board found at boot.
pub struct Keystore {
    pub state: State,
    /// Where `rskey` is, or why there is none.
    pub found: Found,
    /// The eFuse station MAC the record was checked against.
    pub mac: Mac,
}

/// The factory station MAC from eFuse (what the radio sends from, unless
/// something overrides it; nothing in these firmwares does).
pub fn efuse_mac() -> Mac {
    let m = esp_hal::efuse::base_mac_address();
    let mut out = [0u8; 6];
    out.copy_from_slice(m.as_bytes());
    out
}

fn wipe(b: &mut [u8]) {
    b.fill(0);
    core::hint::black_box(&*b);
}

/// Read the record with a flash handle already open (`Images` holds one).
pub fn read_from(flash: &mut FlashStorage<'_>) -> Keystore {
    let mac = efuse_mac();
    let found = ks::find(|at, buf| flash.read(at, buf).is_ok());
    let state = match found {
        Found::At { offset } => {
            let mut a = [0u8; RECORD_LEN];
            let mut b = [0u8; RECORD_LEN];
            let read = flash.read(offset + ks::COPY_OFFSETS[0], &mut a).is_ok()
                && flash.read(offset + ks::COPY_OFFSETS[1], &mut b).is_ok();
            let s = if read { record::read_copies(&a, &b, &mac) } else { State::Damaged };
            wipe(&mut a);
            wipe(&mut b);
            s
        }
        _ => State::Missing,
    };
    Keystore { state, found, mac }
}

/// Read the record on a board that does not use `Images` (single image,
/// node-sdr, node-lr). Call once, at boot.
#[inline(never)]
pub fn read(flash: esp_hal::peripherals::FLASH<'_>) -> Keystore {
    let mut f = FlashStorage::new(flash);
    read_from(&mut f)
}
