//! From the bytes the LR2021 hands over to what goes to the hub, or not.
//!
//! The node decodes nothing beyond what it takes to keep junk off the link:
//! - 1090: SoftRF's DF17 trick (prepend the sync byte 0x95, Manchester
//!   decode with inversion), then the Mode S CRC (`modes::crc`) must be clean
//!   and the format DF17. A false sync on noise almost never passes a
//!   24-bit CRC.
//! - UAT: forwarded raw (48 bytes); the hub runs Reed-Solomon. Only an
//!   all-zero buffer (SoftRF's `memeqzero` check) is dropped.
//! - FLARM: Manchester decode, then SoftRF's CRC-16 check (CCITT, seed
//!   0xFFFF, over the three nRF905 "address" bytes 0x31 0xFA 0xB6 and the 24
//!   payload bytes).
//! - OGN: Manchester decode; the LDPC check is the hub's. Dropped when more
//!   than [`OGN_MAX_CHIP_ERRORS`] chip pairs are invalid (noise after a false
//!   sync is about half invalid).

use heapless::Vec;
use link::{AirProto, MAX_AIR_RAW};

use crate::manchester;
use crate::roles;

pub type Raw = Vec<u8, MAX_AIR_RAW>;

/// Bad Manchester pairs tolerated in an OGN frame (of 208).
pub const OGN_MAX_CHIP_ERRORS: u32 = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    /// Not the length the configuration reads.
    Length,
    /// All zero bytes (SoftRF drops these too).
    Empty,
    /// Bad CRC (1090, FLARM).
    Crc,
    /// A clean frame of another Mode S format (1090).
    NotDf17,
    /// Too many invalid Manchester chip pairs (OGN).
    Manchester,
}

/// SoftRF `update_crc_ccitt` (CRC-16/CCITT, MSB first, polynomial 0x1021).
pub fn crc_ccitt(mut crc: u16, data: &[u8]) -> u16 {
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

/// The nRF905 address bytes FLARM's CRC covers before the payload.
pub const FLARM_CRC_PREFIX: [u8; 3] = [0x31, 0xFA, 0xB6];

/// 1090: the 27 chip bytes after the 0x95 sync -> the 14-byte DF17 frame.
pub fn es1090(chips: &[u8]) -> Result<[u8; 14], Reject> {
    if chips.len() != roles::ES1090_CHIP_LEN as usize {
        return Err(Reject::Length);
    }
    if chips.iter().all(|&b| b == 0) {
        return Err(Reject::Empty);
    }
    let mut raw = [0u8; 28];
    raw[0] = roles::ES1090_SYNC;
    raw[1..].copy_from_slice(chips);
    let mut frame = [0u8; 14];
    manchester::decode(&raw, &mut frame, true);
    if modes::crc::syndrome(&frame) != 0 {
        return Err(Reject::Crc);
    }
    if frame[0] >> 3 != 17 {
        return Err(Reject::NotDf17);
    }
    Ok(frame)
}

pub fn uat978(bytes: &[u8]) -> Result<Raw, Reject> {
    if bytes.len() != roles::UAT978_LEN as usize {
        return Err(Reject::Length);
    }
    if bytes.iter().all(|&b| b == 0) {
        return Err(Reject::Empty);
    }
    Ok(Vec::from_slice(bytes).unwrap_or_default())
}

/// FLARM: 52 chip bytes -> 26 bytes (24 payload + CRC), CRC checked.
pub fn flarm(chips: &[u8]) -> Result<Raw, Reject> {
    let out = manchester_26(chips)?;
    let crc = crc_ccitt(crc_ccitt(0xFFFF, &FLARM_CRC_PREFIX), &out.0[..24]);
    if crc != u16::from_be_bytes([out.0[24], out.0[25]]) {
        return Err(Reject::Crc);
    }
    Ok(Vec::from_slice(&out.0).unwrap_or_default())
}

/// OGN: 52 chip bytes -> 26 bytes (20 payload + 6 LDPC parity).
pub fn ogn(chips: &[u8]) -> Result<Raw, Reject> {
    let out = manchester_26(chips)?;
    if out.1 > OGN_MAX_CHIP_ERRORS {
        return Err(Reject::Manchester);
    }
    Ok(Vec::from_slice(&out.0).unwrap_or_default())
}

fn manchester_26(chips: &[u8]) -> Result<([u8; 26], u32), Reject> {
    if chips.len() != 52 {
        return Err(Reject::Length);
    }
    if chips.iter().all(|&b| b == 0) {
        return Err(Reject::Empty);
    }
    let mut out = [0u8; 26];
    let errors = manchester::decode(chips, &mut out, false);
    Ok((out, errors))
}

/// Dispatch on the protocol: the raw bytes for `Frame::Air`.
pub fn accept(proto: AirProto, fifo: &[u8]) -> Result<Raw, Reject> {
    match proto {
        AirProto::Es1090 => es1090(fifo).map(|f| Vec::from_slice(&f).unwrap_or_default()),
        AirProto::Uat978 => uat978(fifo),
        AirProto::Flarm => flarm(fifo),
        AirProto::Ogn => ogn(fifo),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> std::vec::Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    /// What a 1090 transmitter sends, as 2 Mchip/s chips: bit 1 = pulse
    /// then gap ("10"), bit 0 = "01". (The inverse of SoftRF's Manchester
    /// convention, hence its inversion on decode.)
    fn ppm_chips(frame: &[u8]) -> std::vec::Vec<u8> {
        let mut chips = std::vec![0u8; frame.len() * 2];
        manchester::encode(&frame.iter().map(|b| !b).collect::<std::vec::Vec<_>>(), &mut chips);
        chips
    }

    /// Published DF17 frames (the same ones crates/modes tests with).
    const DF17: [&str; 6] = [
        "8D4840D6202CC371C32CE0576098",
        "8D40621D58C382D690C8AC2863A7",
        "8D40621D58C386435CC412692AD6",
        "8D485020994409940838175B284F",
        "8DA05F219B06B6AF189400CBC33F",
        "8D406B902015A678D4D220AA4BDA",
    ];

    #[test]
    fn es1090_from_synthetic_chips() {
        for f in DF17 {
            let frame = hex(f);
            let chips = ppm_chips(&frame);
            // The first chip byte is the sync word the radio matched.
            assert_eq!(chips[0], 0x95, "{f}");
            assert_eq!(chips[1], 0xA6, "chips of the D in 8D");
            assert_eq!(es1090(&chips[1..]).unwrap().as_slice(), frame.as_slice());
            assert_eq!(accept(AirProto::Es1090, &chips[1..]).unwrap().as_slice(), frame.as_slice());
        }
    }

    #[test]
    fn es1090_rejects() {
        let frame = hex(DF17[0]);
        let chips = ppm_chips(&frame);
        // One bit wrong: a chip pair swapped.
        let mut bad = chips[1..].to_vec();
        bad[10] ^= 0b1100_0000;
        assert_eq!(es1090(&bad), Err(Reject::Crc));
        assert_eq!(es1090(&chips[2..]), Err(Reject::Length));
        assert_eq!(es1090(&[0u8; 27]), Err(Reject::Empty));
        // A clean DF16 (8x with CA bits 0-7 above 0x87? no: 0x80-0x87) has
        // address/parity, so its syndrome is the address: rejected as CRC.
        // Construct a clean frame of another format that starts with nibble
        // 8 and has syndrome 0: DF16 with parity set to the CRC.
        let mut df16 = hex("80E1969958B9020000000000000000");
        df16.truncate(14);
        let c = modes::crc::crc24(&df16[..11]);
        df16[11..].copy_from_slice(&c.to_be_bytes()[1..]);
        assert_eq!(es1090(&ppm_chips(&df16)[1..]), Err(Reject::NotDf17));
        // Noise.
        let mut x: u32 = 99;
        let mut pass = 0;
        for _ in 0..20_000 {
            let n: std::vec::Vec<u8> = (0..27)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    x as u8
                })
                .collect();
            pass += es1090(&n).is_ok() as u32;
        }
        assert_eq!(pass, 0);
    }

    #[test]
    fn crc_ccitt_check_value() {
        // CRC-16/CCITT-FALSE check value.
        assert_eq!(crc_ccitt(0xFFFF, b"123456789"), 0x29B1);
    }

    fn flarm_chips(payload: &[u8; 24]) -> std::vec::Vec<u8> {
        let crc = crc_ccitt(crc_ccitt(0xFFFF, &FLARM_CRC_PREFIX), payload);
        let mut bytes = payload.to_vec();
        bytes.extend_from_slice(&crc.to_be_bytes());
        let mut chips = std::vec![0u8; 52];
        manchester::encode(&bytes, &mut chips);
        chips
    }

    #[test]
    fn flarm_frames() {
        let payload: [u8; 24] = core::array::from_fn(|i| (i as u8).wrapping_mul(37) ^ 0x5C);
        let chips = flarm_chips(&payload);
        let raw = flarm(&chips).unwrap();
        assert_eq!(&raw[..24], &payload);
        assert_eq!(raw.len(), 26);
        let mut bad = chips.clone();
        bad[5] = manchester::encode_nibble((manchester::DECODE[chips[5] as usize] & 0xF) ^ 1);
        assert_eq!(flarm(&bad), Err(Reject::Crc));
        assert_eq!(flarm(&chips[..51]), Err(Reject::Length));
    }

    #[test]
    fn ogn_frames() {
        let data: [u8; 26] = core::array::from_fn(|i| i as u8 * 7);
        let mut chips = [0u8; 52];
        manchester::encode(&data, &mut chips);
        assert_eq!(ogn(&chips).unwrap().as_slice(), data.as_slice());
        // A few damaged pairs pass (the LDPC can fix them), noise does not.
        chips[0] = 0x00; // 4 bad pairs
        chips[1] = 0xFF; // 4 more
        assert!(ogn(&chips).is_ok());
        let noise: [u8; 52] = core::array::from_fn(|i| (i as u8).wrapping_mul(97).wrapping_add(13));
        assert_eq!(ogn(&noise), Err(Reject::Manchester));
    }

    #[test]
    fn uat_raw() {
        let b: [u8; 48] = core::array::from_fn(|i| i as u8 + 1);
        assert_eq!(uat978(&b).unwrap().as_slice(), b.as_slice());
        assert_eq!(uat978(&[0; 48]), Err(Reject::Empty));
        assert_eq!(uat978(&b[..47]), Err(Reject::Length));
    }
}
