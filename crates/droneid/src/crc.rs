//! The three CRCs in a DroneID burst.
//!
//! - CRC-24A (3GPP TS 36.212 §5.1.1): over the whole transport block,
//!   polynomial 0x864CFB, MSB first, init 0, no final XOR.
//! - DJI CRC-16: over the inner record, reflected polynomial 0x8408
//!   (0x1021 reversed), register initialised to 0x3692, no final XOR, stored
//!   little-endian. The same CRC DJI uses on its DUML links.
//! - DJI CRC-8: header check of the V1 (DUML-style) transport envelope,
//!   reflected polynomial 0x8C, init 0x77, no final XOR.

const fn crc24a_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = (i as u32) << 16;
        let mut b = 0;
        while b < 8 {
            c = if c & 0x80_0000 != 0 { (c << 1) ^ 0x86_4CFB } else { c << 1 };
            b += 1;
        }
        t[i] = c & 0xFF_FFFF;
        i += 1;
    }
    t
}

static CRC24A: [u32; 256] = crc24a_table();

/// CRC-24A of `data`, MSB first. Appending the result big-endian makes the
/// CRC of the whole block zero.
pub fn crc24a(data: &[u8]) -> u32 {
    let mut c = 0u32;
    for &b in data {
        c = ((c << 8) & 0xFF_FFFF) ^ CRC24A[(((c >> 16) as u8) ^ b) as usize];
    }
    c
}

const fn reflected_table16(poly: u16) -> [u16; 256] {
    let mut t = [0u16; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u16;
        let mut b = 0;
        while b < 8 {
            c = if c & 1 != 0 { (c >> 1) ^ poly } else { c >> 1 };
            b += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

static CRC16: [u16; 256] = reflected_table16(0x8408);

/// Initial register of DJI's CRC-16.
pub const DJI_CRC16_INIT: u16 = 0x3692;

/// DJI's CRC-16 over `data`.
pub fn dji_crc16(data: &[u8]) -> u16 {
    let mut c = DJI_CRC16_INIT;
    for &b in data {
        c = (c >> 8) ^ CRC16[((c as u8) ^ b) as usize];
    }
    c
}

/// DJI's CRC-8 (V1 envelope header).
pub fn dji_crc8(data: &[u8]) -> u8 {
    let mut c = 0x77u8;
    for &b in data {
        c ^= b;
        for _ in 0..8 {
            c = if c & 1 != 0 { (c >> 1) ^ 0x8C } else { c >> 1 };
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc24a_vectors() {
        // Check value of CRC-24/LTE-A for "123456789" (reveng catalogue).
        assert_eq!(crc24a(b"123456789"), 0xCDE703);
        assert_eq!(crc24a(&[]), 0);
        // Appending the CRC big-endian zeroes the remainder.
        let mut v = b"DroneID transport block".to_vec();
        let c = crc24a(&v);
        v.extend_from_slice(&[(c >> 16) as u8, (c >> 8) as u8, c as u8]);
        assert_eq!(crc24a(&v), 0);
    }

    #[test]
    fn crc16_vectors() {
        // Same algorithm as reveng's CRC-16/KERMIT apart from the initial
        // register; with init 0 it must reproduce KERMIT's check value.
        fn kermit(data: &[u8]) -> u16 {
            let mut c = 0u16;
            for &b in data {
                c = (c >> 8) ^ CRC16[((c as u8) ^ b) as usize];
            }
            c
        }
        assert_eq!(kermit(b"123456789"), 0x2189);
        // Independently computed bitwise with init 0x3692.
        let mut c = 0x3692u16;
        for &b in b"123456789" {
            c ^= b as u16;
            for _ in 0..8 {
                c = if c & 1 != 0 { (c >> 1) ^ 0x8408 } else { c >> 1 };
            }
        }
        assert_eq!(dji_crc16(b"123456789"), c);
    }

    #[test]
    fn crc8_reference() {
        // DUML header CRC-8 of the bytes 55 0d 04 is 0x33 in public DUML
        // captures (e.g. the "55 0d 04 33" heartbeat header).
        assert_eq!(dji_crc8(&[0x55, 0x0d, 0x04]), 0x33);
    }
}
