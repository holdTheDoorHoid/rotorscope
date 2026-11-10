//! The LTE Gold pseudo-random sequence (3GPP TS 36.211 §7.2) with DroneID's
//! fixed `c_init = 0x12345678`, applied to all 7200 rate-matched bits.
//!
//! The whole 7200-bit sequence is built at compile time (900 bytes), so
//! descrambling is a table lookup per bit.

use crate::E;

/// DroneID's scrambler initialisation.
pub const C_INIT: u32 = 0x1234_5678;
const NC: usize = 1600;

/// `c(n)` for `n in 0..len`, packed MSB first. `const fn`.
pub const fn gold<const BYTES: usize>(c_init: u32) -> [u8; BYTES] {
    let len = BYTES * 8;
    // x1 and x2 as 31-bit shift registers, bit i = x(n + i).
    let mut x1: u32 = 1;
    let mut x2: u32 = c_init & 0x7FFF_FFFF;
    let mut out = [0u8; BYTES];
    let mut n = 0;
    while n < NC + len {
        if n >= NC {
            let bit = ((x1 ^ x2) & 1) as u8;
            let i = n - NC;
            out[i / 8] |= bit << (7 - (i % 8));
        }
        // x1(n+31) = x1(n+3) ^ x1(n); x2(n+31) = x2(n+3)^x2(n+2)^x2(n+1)^x2(n).
        let f1 = ((x1 >> 3) ^ x1) & 1;
        let f2 = ((x2 >> 3) ^ (x2 >> 2) ^ (x2 >> 1) ^ x2) & 1;
        x1 = (x1 >> 1) | (f1 << 30);
        x2 = (x2 >> 1) | (f2 << 30);
        n += 1;
    }
    out
}

/// The DroneID scrambling sequence, 7200 bits packed MSB first.
pub static SEQUENCE: [u8; E / 8] = gold::<{ E / 8 }>(C_INIT);

/// Scrambling bit `n` (0 or 1).
#[inline]
pub fn bit(n: usize) -> u8 {
    (SEQUENCE[n / 8] >> (7 - (n % 8))) & 1
}

/// Descramble soft values in place: flip the sign where `c(n) = 1`.
pub fn descramble(llr: &mut [i8]) {
    for (n, v) in llr.iter_mut().enumerate() {
        if bit(n) == 1 {
            *v = v.saturating_neg();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_bits_match_published_example() {
        // EdwardBlair/dji-droneid-spec RF_PHY.md: rate-matched bits
        // 0011... XOR c(n) gives 1110 0001 1101 0010 1110 1101 0100 1010.
        let rm = 0x3333_3333u32;
        let scrambled = 0b1110_0001_1101_0010_1110_1101_0100_1010u32;
        let c = u32::from_be_bytes([SEQUENCE[0], SEQUENCE[1], SEQUENCE[2], SEQUENCE[3]]);
        assert_eq!(rm ^ c, scrambled);
    }

    #[test]
    fn matches_bitwise_reference() {
        // Straight transcription of the 36.211 recurrences with arrays.
        let total = NC + E;
        let mut x1 = vec![0u8; total + 31];
        let mut x2 = vec![0u8; total + 31];
        x1[0] = 1;
        for (i, x) in x2.iter_mut().enumerate().take(31) {
            *x = ((C_INIT >> i) & 1) as u8;
        }
        for n in 0..total {
            x1[n + 31] = x1[n + 3] ^ x1[n];
            x2[n + 31] = x2[n + 3] ^ x2[n + 2] ^ x2[n + 1] ^ x2[n];
        }
        for n in 0..E {
            assert_eq!(bit(n), x1[n + NC] ^ x2[n + NC], "bit {n}");
        }
    }

    #[test]
    fn descramble_flips_signs() {
        let mut v = [10i8; 64];
        descramble(&mut v);
        for (n, &x) in v.iter().enumerate() {
            assert_eq!(x, if bit(n) == 1 { -10 } else { 10 });
        }
    }
}
