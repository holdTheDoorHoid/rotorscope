//! Mode S CRC-24 (ICAO Annex 10 Vol IV, 3.1.2.3.3): generator polynomial
//! 0x1FFF409, computed over every bit but the last 24, which hold the
//! parity (possibly XORed with an address or interrogator code).

/// The 24 low bits of the generator polynomial.
pub const POLY: u32 = 0xFF_F409;

const fn make_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = (i as u32) << 16;
        let mut b = 0;
        while b < 8 {
            c = if c & 0x80_0000 != 0 { (c << 1) ^ POLY } else { c << 1 };
            b += 1;
        }
        t[i] = c & 0xFF_FFFF;
        i += 1;
    }
    t
}

static TABLE: [u32; 256] = make_table();

/// CRC-24 of `data` (MSB first, zero initial value).
pub fn crc24(data: &[u8]) -> u32 {
    let mut c = 0u32;
    for &b in data {
        c = ((c << 8) & 0xFF_FFFF) ^ TABLE[(((c >> 16) as u8) ^ b) as usize];
    }
    c
}

/// The last three bytes of a frame as a number.
pub fn parity_field(frame: &[u8]) -> u32 {
    let n = frame.len();
    if n < 3 {
        return 0;
    }
    u32::from(frame[n - 3]) << 16 | u32::from(frame[n - 2]) << 8 | u32::from(frame[n - 1])
}

/// CRC of the data bits XOR the parity field. Zero for a clean DF17/18;
/// the aircraft address for address/parity formats (DF0/4/5/16/20/21);
/// the interrogator code for DF11.
pub fn syndrome(frame: &[u8]) -> u32 {
    if frame.len() < 4 {
        return 0;
    }
    crc24(&frame[..frame.len() - 3]) ^ parity_field(frame)
}

/// Syndrome of a single flipped bit `bit` (0 = first bit sent) in an
/// otherwise all-zero frame of `bytes` bytes. Syndromes are linear, so a
/// frame with that bit wrong has its syndrome XORed with this.
const fn bit_syndrome(bytes: usize, bit: usize) -> u32 {
    let data_bits = (bytes - 3) * 8;
    if bit >= data_bits {
        // A parity bit: it shows up as itself.
        return 1 << (bytes * 8 - 1 - bit);
    }
    // CRC of a frame with only `bit` set, done bitwise (const context).
    let mut c: u32 = 0;
    let mut i = 0;
    while i < data_bits {
        let inbit = if i == bit { 1 } else { 0 };
        let top = ((c >> 23) & 1) ^ inbit;
        c = (c << 1) & 0xFF_FFFF;
        if top != 0 {
            c ^= POLY;
        }
        i += 1;
    }
    c
}

const fn make_bit_syndromes<const N: usize>(bytes: usize) -> [u32; N] {
    let mut t = [0u32; N];
    let mut i = 0;
    while i < N {
        t[i] = bit_syndrome(bytes, i);
        i += 1;
    }
    t
}

/// Single-bit syndromes for a 112-bit (long) frame, by bit number.
pub static SYN_LONG: [u32; 112] = make_bit_syndromes::<112>(14);
/// Single-bit syndromes for a 56-bit (short) frame.
pub static SYN_SHORT: [u32; 56] = make_bit_syndromes::<56>(7);
