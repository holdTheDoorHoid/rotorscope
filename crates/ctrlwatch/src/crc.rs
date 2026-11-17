//! The checksums and scramblers the recognised systems use. All table-free
//! or with `const` tables, so nothing is built at run time.

/// ExpressLRS `Crc2Byte` (src/lib/CRC/crc.cpp, GPLv3): an MSB-first CRC of
/// `bits` width (14 for OTA4, 16 for OTA8) with a caller-chosen initial
/// value. Ported bit for bit, including the u16 wrap-around of the table
/// entries, so that garbage above `bits` behaves exactly as on the radio.
pub struct ElrsCrc {
    tab: [u16; 256],
    bits: u8,
}

impl ElrsCrc {
    pub const fn new(bits: u8, poly: u16) -> Self {
        let mut tab = [0u16; 256];
        let highbit: u16 = 1 << (bits - 1);
        let mut i = 0;
        while i < 256 {
            let mut crc: u16 = (i as u16) << (bits - 8);
            let mut j = 0;
            while j < 8 {
                crc = (crc << 1) ^ if crc & highbit != 0 { poly } else { 0 };
                j += 1;
            }
            tab[i] = crc;
            i += 1;
        }
        Self { tab, bits }
    }

    pub fn calc(&self, data: &[u8], init: u16) -> u16 {
        let mut crc = init;
        for &b in data {
            let idx = ((crc >> (self.bits - 8)) ^ b as u16) & 0xFF;
            crc = (crc << 8) ^ self.tab[idx as usize];
        }
        crc & (((1u32 << self.bits) - 1) as u16)
    }
}

/// `ELRS_CRC14_POLY`, src/lib/OTA/OTA.h.
pub static ELRS_CRC14: ElrsCrc = ElrsCrc::new(14, 0x2E57);
/// `ELRS_CRC16_POLY`, src/lib/OTA/OTA.h.
pub static ELRS_CRC16: ElrsCrc = ElrsCrc::new(16, 0x3D65);

/// MAVLink's X.25 / CRC-16/MCRF4XX accumulate (init 0xFFFF, no final XOR).
/// mLRS uses the same routine (`fmav_crc_accumulate`) for its frames.
pub fn x25_accumulate(crc: u16, b: u8) -> u16 {
    let mut tmp = b ^ (crc as u8);
    tmp ^= tmp << 4;
    let tmp = tmp as u16;
    (crc >> 8) ^ (tmp << 8) ^ (tmp << 3) ^ (tmp >> 4)
}

pub fn x25(data: &[u8]) -> u16 {
    x25_continue(0xFFFF, data)
}

pub fn x25_continue(mut crc: u16, data: &[u8]) -> u16 {
    for &b in data {
        crc = x25_accumulate(crc, b);
    }
    crc
}

/// CRSF's CRC-8 (DVB-S2, polynomial 0xD5, init 0).
pub fn crc8_d5(data: &[u8]) -> u8 {
    let mut crc = 0u8;
    for &b in data {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0xD5
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// MSB-first, non-reflected CRC-16 with the given polynomial and initial
/// value (no final XOR). Si443x packet handler CRCs: CCITT = 0x1021,
/// "CRC-16 (IBM)" = 0x8005.
pub fn crc16_msb(poly: u16, init: u16, data: &[u8]) -> u16 {
    let mut crc = init;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ poly
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// PN9 whitening (x^9 + x^5 + 1, seed 0x1FF), the sequence the Si443x
/// (SiK radios) XORs over everything after the sync word when `enwhite` is
/// set. This is the same PN9 as TI's CC1101 (first bytes FF E1 1D 9A ED 85).
/// UNVERIFIED against a real Si1000: the datasheet names the polynomial and
/// seed but not the bit order; see docs/CONTROL-LINKS.md.
pub fn pn9_xor(data: &mut [u8]) {
    let mut key: u16 = 0x1FF;
    for b in data.iter_mut() {
        *b ^= key as u8;
        for _ in 0..8 {
            let msb = ((key >> 5) ^ key) & 1;
            key = (key >> 1) | (msb << 8);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x25_check_value() {
        // CRC-16/MCRF4XX check value for "123456789".
        assert_eq!(x25(b"123456789"), 0x6F91);
    }

    #[test]
    fn crc8_check_value() {
        // CRC-8/DVB-S2 check value.
        assert_eq!(crc8_d5(b"123456789"), 0xBC);
    }

    #[test]
    fn ccitt_false_check_value() {
        assert_eq!(crc16_msb(0x1021, 0xFFFF, b"123456789"), 0x29B1);
        // CRC-16/UMTS (BUYPASS): poly 0x8005, init 0.
        assert_eq!(crc16_msb(0x8005, 0, b"123456789"), 0xFEE8);
    }

    #[test]
    fn pn9_matches_cc1101_sequence() {
        let mut z = [0u8; 8];
        pn9_xor(&mut z);
        assert_eq!(z, [0xFF, 0xE1, 0x1D, 0x9A, 0xED, 0x85, 0x33, 0x24]);
    }

    #[test]
    fn elrs_crc_masks_to_width() {
        let c = ELRS_CRC14.calc(&[1, 2, 3, 4, 5, 6, 7], 0x1234);
        assert!(c < 1 << 14);
    }
}
