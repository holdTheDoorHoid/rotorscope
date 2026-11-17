//! Manchester chips to bits, as SoftRF does it (`libraries/OGN/manchester.h`,
//! from the OGN tracker, GPLv3).
//!
//! One chip byte holds four chip pairs, first pair in the top bits. A pair
//! "10" is a 0 and "01" is a 1 (the bit is the second chip); "00" and "11"
//! are errors. `DECODE[b]`: low nibble = the four bits, high nibble = one
//! error flag per pair, exactly SoftRF's `ManchesterDecode` table (which is
//! generated here rather than copied, and checked against it).

const fn make_decode() -> [u8; 256] {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        let mut data = 0u8;
        let mut err = 0u8;
        let mut p = 0;
        while p < 4 {
            let pair = (i >> (6 - 2 * p)) & 3;
            let bit = (pair & 1) as u8;
            let bad = (pair == 0 || pair == 3) as u8;
            data |= bit << (3 - p);
            err |= bad << (3 - p);
            p += 1;
        }
        t[i] = (err << 4) | data;
        i += 1;
    }
    t
}

pub static DECODE: [u8; 256] = make_decode();

/// SoftRF's `ManchesterEncode`: a nibble to its chip byte (for tests and
/// synthetic streams; the node never encodes anything to send).
pub const fn encode_nibble(n: u8) -> u8 {
    let mut out = 0u8;
    let mut p = 0;
    while p < 4 {
        let bit = (n >> (3 - p)) & 1;
        let pair = if bit == 1 { 0b01 } else { 0b10 };
        out |= pair << (6 - 2 * p);
        p += 1;
    }
    out
}

/// Decode `chips` (two chip bytes per data byte) into `out`, SoftRF's loop
/// `RxBuffer[i>>1] = ((val1 & 0x0F) << 4) | (val2 & 0x0F)`, with `invert`
/// for the ES1090 variant `((~val1 & 0x0F) << 4) | (~val2 & 0x0F)`.
/// Returns the number of bad chip pairs (0 for a clean frame). `out` must
/// hold `chips.len() / 2` bytes; a trailing odd chip byte is ignored.
pub fn decode(chips: &[u8], out: &mut [u8], invert: bool) -> u32 {
    let mut errors = 0;
    for (o, pair) in out.iter_mut().zip(chips.chunks_exact(2)) {
        let v1 = DECODE[pair[0] as usize];
        let v2 = DECODE[pair[1] as usize];
        errors += (v1 >> 4).count_ones() + (v2 >> 4).count_ones();
        let (a, b) = if invert { (!v1, !v2) } else { (v1, v2) };
        *o = ((a & 0x0F) << 4) | (b & 0x0F);
    }
    errors
}

/// Chip bytes for `data` (MSB first), two per byte.
pub fn encode(data: &[u8], out: &mut [u8]) {
    for (d, o) in data.iter().zip(out.chunks_exact_mut(2)) {
        o[0] = encode_nibble(d >> 4);
        o[1] = encode_nibble(d & 0x0F);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Entries read off SoftRF's `ManchesterDecode` table (row = high
    /// nibble of the index, column = low nibble).
    #[test]
    fn matches_softrf_table() {
        let softrf = [
            (0x00, 0xF0),
            (0x01, 0xE1),
            (0x02, 0xE0),
            (0x03, 0xF1),
            (0x0F, 0xF3),
            (0x10, 0xB4),
            (0x3C, 0xF6),
            (0x55, 0x0F),
            (0x5A, 0x0C),
            (0x95, 0x07),
            (0x96, 0x06),
            (0xA6, 0x02),
            (0xAA, 0x00),
            (0xBB, 0x55),
            (0xFF, 0xFF),
        ];
        for (i, v) in softrf {
            assert_eq!(DECODE[i], v, "index {i:#04x}");
        }
        // Row 0x40 of the C table, all sixteen.
        let row4 = [0x78, 0x69, 0x68, 0x79, 0x5A, 0x4B, 0x4A, 0x5B, 0x58, 0x49, 0x48, 0x59, 0x7A, 0x6B, 0x6A, 0x7B];
        assert_eq!(DECODE[0x40..0x50], row4);
    }

    /// SoftRF's `ManchesterEncode` table.
    #[test]
    fn encode_matches_softrf() {
        let t = [0xAA, 0xA9, 0xA6, 0xA5, 0x9A, 0x99, 0x96, 0x95, 0x6A, 0x69, 0x66, 0x65, 0x5A, 0x59, 0x56, 0x55];
        for n in 0..16 {
            assert_eq!(encode_nibble(n), t[n as usize]);
            assert_eq!(DECODE[t[n as usize] as usize], n);
        }
    }

    #[test]
    fn round_trip_and_error_count() {
        let data = [0x00, 0x12, 0x8D, 0xFF, 0x5A];
        let mut chips = [0u8; 10];
        encode(&data, &mut chips);
        let mut out = [0u8; 5];
        assert_eq!(decode(&chips, &mut out, false), 0);
        assert_eq!(out, data);
        chips[3] = 0x00; // four bad pairs
        assert_eq!(decode(&chips, &mut out, false), 4);
        // Inverted decode of normally encoded data gives the complement.
        encode(&data, &mut chips);
        decode(&chips, &mut out, true);
        assert_eq!(out, data.map(|b| !b));
    }
}
