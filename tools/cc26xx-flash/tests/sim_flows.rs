//! The full info / read / flash flows against the in-process bootloader.

use cc26xx_flash::ccfg::{self, CCFG_LEN, Ccfg, OFF_BL_CONFIG, OFF_ERASE_CONF, OFF_IMAGE_VALID};
use cc26xx_flash::crc::crc32;
use cc26xx_flash::hex;
use cc26xx_flash::ops::{self, CC2652P_FLASH, Erase};
use cc26xx_flash::proto::{Bsl, Error, Link, cmd};
use cc26xx_flash::sim::{Mode, Sim};

const SIZE: usize = CC2652P_FLASH as usize;

/// The CCFG records (0x57FA8..0x58000) of Sniffle v1.11.0's CC2652P image
/// (nccgroup/Sniffle, GPL-3.0), copied verbatim from
/// `sniffle_cc1352p1_cc2652p1.hex` (SHA-256 5a1332ae...62daf1).
const SNIFFLE_CCFG: &str =
    include_str!("data/sniffle-v1.11.0-cc2652p1-ccfg.hex");

fn set_word(img: &mut [u8], ccfg_off: usize, v: u32) {
    let at = img.len() - CCFG_LEN + ccfg_off;
    img[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

/// A plausible firmware: pseudo-random code with blank gaps, and the
/// Sniffle image's real CCFG.
fn image(seed: u32) -> Vec<u8> {
    let mut img = hex::to_image(&hex::parse(SNIFFLE_CCFG).unwrap(), SIZE).unwrap();
    let mut x = seed | 1;
    for (i, b) in img[..0x30000].iter_mut().enumerate() {
        if (i / 0x1000) % 5 == 3 {
            continue; // a blank gap, skipped when writing
        }
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        *b = x as u8;
    }
    img
}

fn connected(sim: Sim) -> Bsl<Sim> {
    let mut b = Bsl::new(sim);
    ops::connect(&mut b).expect("connect");
    b
}

#[test]
fn sniffle_ccfg_is_safe() {
    let img = image(1);
    let c = Ccfg::from_image(&img).unwrap();
    assert_eq!(c.bl_config, 0xC5FE_0FC5);
    assert_eq!(c.image_valid, 0);
    let r = ops::preflight(&img, CC2652P_FLASH).unwrap();
    assert!(r.notes.is_empty());
}

#[test]
fn bad_images_are_refused() {
    for (off, v, why) in [
        (OFF_BL_CONFIG, 0x00FE_0FC5, "BOOTLOADER_ENABLE"),
        (OFF_BL_CONFIG, 0xC5FE_0F00, "BL_ENABLE"),
        (OFF_BL_CONFIG, 0xC5FE_0BC5, "DIO11"),
        (OFF_BL_CONFIG, 0xC5FF_0FC5, "active-high"),
        (OFF_ERASE_CONF, 0xFFFF_FEFF, "CHIP_ERASE"),
        (OFF_ERASE_CONF, 0xFFFF_FFFE, "BANK_ERASE"),
    ] {
        let mut img = image(2);
        set_word(&mut img, off, v);
        let e = ops::preflight(&img, CC2652P_FLASH).unwrap_err();
        assert!(e.contains(why), "{why}: {e}");
    }
    // An image with no CCFG at all (e.g. a bare application binary).
    let mut img = image(3);
    let n = img.len();
    img[n - CCFG_LEN..].fill(0xFF);
    assert!(ops::preflight(&img, CC2652P_FLASH).is_err());
    // Wrong size.
    assert!(ops::preflight(&img[..0x40000], CC2652P_FLASH).is_err());
}

#[test]
fn entry_sequence_reaches_bootloader() {
    let mut sim = Sim::new(image(4));
    assert_eq!(sim.mode, Mode::App);
    // Without the pin dance the firmware ignores 55 55.
    let mut b = Bsl::new(sim);
    assert!(matches!(b.sync(), Err(Error::Timeout(_))));
    sim = b.into_link();
    let b = connected(sim);
    let s = b.into_link();
    assert_eq!(s.mode, Mode::Ready);
    assert_eq!(s.boots_to_bootloader, 1);
}

#[test]
fn info_identifies_and_reset_boots_app() {
    let mut b = connected(Sim::new(image(5)));
    let info = ops::identify(&mut b).unwrap();
    assert_eq!(info.wafer_id(), 0xBB41);
    assert!(info.is_agama());
    assert_eq!(info.flash_size(), CC2652P_FLASH);
    assert_eq!(info.ccfg.bl_config, 0xC5FE_0FC5);
    assert!(ccfg::check(&info.ccfg, info.sectors).safe());
    assert_eq!(info.ieee, [0x00, 0x12, 0x4B, 0x00, 0x44, 0x33, 0x22, 0x11]);
    b.reset().unwrap();
    let s = b.into_link();
    assert_eq!(s.mode, Mode::App);
    assert_eq!(s.host_nacks, 0);
}

#[test]
fn pin_reset_boots_app() {
    let mut b = connected(Sim::new(image(5)));
    b.reset_by_pins_sonoff().unwrap();
    b.link().delay(std::time::Duration::from_millis(5));
    assert_eq!(b.into_link().mode, Mode::App);
}

#[test]
fn backup_reads_everything_and_matches_crc() {
    let img = image(6);
    let mut b = connected(Sim::new(img.clone()));
    let mut calls = 0;
    let (data, crc) = ops::backup(&mut b, CC2652P_FLASH, &mut |_, _| calls += 1).unwrap();
    assert_eq!(data, img);
    assert_eq!(crc, crc32(&img));
    assert_eq!(calls, SIZE.div_ceil(252));
    // Nothing was written.
    assert!(
        !b.link()
            .commands
            .iter()
            .any(|&c| [cmd::BANK_ERASE, cmd::SEND_DATA, cmd::SECTOR_ERASE].contains(&c))
    );
}

#[test]
fn backup_detects_crc_disagreement() {
    let mut s = Sim::new(image(7));
    s.corrupt_crcs = 1;
    let mut b = connected(s);
    assert!(matches!(
        ops::backup(&mut b, CC2652P_FLASH, &mut |_, _| {}),
        Err(Error::Verify(_))
    ));
}

#[test]
fn bad_reply_checksum_is_nacked() {
    let mut b = connected(Sim::new(image(7)));
    b.link().corrupt_packets = 1;
    assert!(matches!(b.chip_id(), Err(Error::BadPacket(_))));
    assert_eq!(b.link().host_nacks, 1);
}

#[test]
fn flash_replaces_firmware_and_boots_it() {
    let old = image(8);
    let new = image(9);
    let mut b = connected(Sim::new(old));
    let p = ops::program(&mut b, &new, &mut |_, _, _| {}).unwrap();
    assert_eq!(p.erase, Erase::Bank);
    assert_eq!(p.crc, crc32(&new));
    assert!(p.bytes_sent < SIZE, "blank chunks skipped");
    assert_eq!(b.link().flash, new);
    b.reset().unwrap();
    assert_eq!(b.into_link().mode, Mode::App);
}

#[test]
fn the_real_sniffle_layout_round_trips() {
    // Same run structure as the real file: data low, CCFG at the top.
    let mut new = vec![0xFF; SIZE];
    new[..0xE400]
        .iter_mut()
        .enumerate()
        .for_each(|(i, b)| *b = (i * 7) as u8);
    let tail = hex::to_image(&hex::parse(SNIFFLE_CCFG).unwrap(), SIZE).unwrap();
    new[SIZE - CCFG_LEN..].copy_from_slice(&tail[SIZE - CCFG_LEN..]);
    let runs = ops::runs(&new);
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[1].1, SIZE);
    let mut b = connected(Sim::new(image(10)));
    ops::program(&mut b, &new, &mut |_, _, _| {}).unwrap();
    assert_eq!(b.link().flash, new);
}

#[test]
fn bank_erase_refused_falls_back_to_sectors() {
    let mut s = Sim::new(image(11));
    s.refuse_bank_erase = true;
    let new = image(12);
    let mut b = connected(s);
    let p = ops::program(&mut b, &new, &mut |_, _, _| {}).unwrap();
    assert_eq!(p.erase, Erase::Sectors);
    assert_eq!(b.link().flash, new);
}

#[test]
fn write_failure_retries_from_the_bootloader() {
    let mut s = Sim::new(image(13));
    s.fail_send_data_at = Some(40);
    let new = image(14);
    let mut b = connected(s);
    let mut retries = Vec::new();
    let p = ops::program_with_retry(&mut b, &new, 3, &mut |_, _, _| {}, &mut |n, e| {
        retries.push((n, e.to_string()))
    })
    .unwrap();
    assert_eq!(retries.len(), 1);
    assert!(retries[0].1.contains("SEND_DATA"), "{retries:?}");
    assert_eq!(p.crc, crc32(&new));
    let s = b.into_link();
    assert_eq!(s.flash, new);
    // Booted into the bootloader twice: the first connect and the retry.
    // The second time the image was half written with no CCFG yet, and the
    // backdoor got it there anyway.
    assert_eq!(s.boots_to_bootloader, 2);
}

#[test]
fn erased_chip_always_boots_the_bootloader() {
    // A run that died right after the erase: IMAGE_VALID_CONF reads
    // 0xFFFFFFFF, so even a plain reset with DIO15 high lands in the ROM.
    let mut s = Sim::new(vec![0xFF; SIZE]);
    s.mode = Mode::Reset;
    let mut b = Bsl::new(s);
    b.link().delay(std::time::Duration::from_millis(5));
    // Release reset without touching DIO15 (DTR and RTS both asserted).
    b.link().set_dtr(true).unwrap();
    b.link().set_rts(true).unwrap();
    b.link().delay(std::time::Duration::from_millis(5));
    b.sync().unwrap();
    b.ping().unwrap();
}

#[test]
fn verify_failure_is_reported() {
    let mut s = Sim::new(image(15));
    s.corrupt_crcs = 1; // the blank check after the erase
    let mut b = connected(s);
    let e = ops::program(&mut b, &image(16), &mut |_, _, _| {}).unwrap_err();
    assert!(matches!(e, Error::Verify(_)), "{e}");
}

#[test]
fn protected_chip_refuses_erase() {
    let mut img = image(17);
    set_word(&mut img, ccfg::OFF_PROT, 0xFFFF_FFFE); // sector 0 locked
    let mut b = connected(Sim::new(img));
    let e = ops::program(&mut b, &image(18), &mut |_, _, _| {}).unwrap_err();
    assert!(matches!(e, Error::Status { .. }), "{e}");
}

/// The real Sniffle file, when it is on this machine.
#[test]
#[ignore = "needs SNIFFLE_HEX=path/to/sniffle_cc1352p1_cc2652p1.hex"]
fn full_sniffle_file() {
    let p = std::env::var("SNIFFLE_HEX").expect("SNIFFLE_HEX");
    let img = ops::load_image(std::path::Path::new(&p), CC2652P_FLASH).unwrap();
    ops::preflight(&img, CC2652P_FLASH).unwrap();
    let mut b = connected(Sim::new(image(19)));
    ops::program(&mut b, &img, &mut |_, _, _| {}).unwrap();
    assert_eq!(b.link().flash, img);
}

#[test]
fn off_image_valid_constant() {
    assert_eq!(OFF_IMAGE_VALID, 0x44);
}
