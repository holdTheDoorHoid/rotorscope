//! LTE turbo rate matching (3GPP TS 36.212 §5.1.4.1), receive side, for
//! DroneID's fixed sizes: D = K + 4 = 1412 bits per stream, a 45 x 32
//! sub-block interleaver with 28 leading dummy bits, a 4320-bit circular
//! buffer, redundancy version 0 (start at k0 = 2R = 90) and E = 7200.
//!
//! [`derate`] walks the same circular buffer the transmitter read and adds
//! every received soft value into the stream position it came from, so the
//! systematic bits sent twice are combined (the buffer wraps once: 7200 >
//! 4236 transmitted positions).

use crate::{D, E};

/// Columns of the sub-block interleaver.
pub const COLS: usize = 32;
/// Rows: ceil(D / 32).
pub const ROWS: usize = D.div_ceil(COLS);
/// Interleaver size per stream.
pub const K_PI: usize = ROWS * COLS;
/// Leading dummy (NULL) bits per stream.
pub const N_DUMMY: usize = K_PI - D;
/// Circular buffer length (no limited-buffer rate matching).
pub const N_CB: usize = 3 * K_PI;
/// Start position for RV0: R * (2 * ceil(N_cb / (8R)) * rv + 2).
pub const K0: usize = 2 * ROWS;

/// Inter-column permutation pattern (36.212 Table 5.1.4-1).
pub const PERM: [u8; COLS] = [
    0, 16, 8, 24, 4, 20, 12, 28, 2, 18, 10, 26, 6, 22, 14, 30, 1, 17, 9, 25, 5, 21, 13, 29, 3, 19,
    11, 27, 7, 23, 15, 31,
];

/// Stream and position (in `d`) of circular-buffer entry `w`, or `None` for a
/// dummy bit.
#[inline]
pub fn locate(w: usize) -> Option<(usize, usize)> {
    let (stream, k) = if w < K_PI { (0, w) } else { (1 + ((w - K_PI) & 1), (w - K_PI) >> 1) };
    let col = PERM[k / ROWS] as usize;
    let row = k % ROWS;
    let y = if stream == 2 { (col + COLS * row + 1) % K_PI } else { col + COLS * row };
    (y >= N_DUMMY).then(|| (stream, y - N_DUMMY))
}

/// Iterator over the `E` transmitted bits in order, yielding the stream
/// (0 systematic, 1 and 2 parity) and position each came from.
pub fn positions() -> impl Iterator<Item = (usize, usize)> {
    (0..).map(|j| (K0 + j) % N_CB).filter_map(locate).take(E)
}

/// Soft de-rate-matching: `llr` in transmission order (already
/// descrambled) is summed into the three streams, saturating at `i16`.
pub fn derate(llr: &[i8], out: &mut [[i16; D]; 3]) {
    for s in out.iter_mut() {
        s.fill(0);
    }
    for (&v, (s, i)) in llr.iter().zip(positions()) {
        out[s][i] = out[s][i].saturating_add(v as i16);
    }
}

/// Transmit-side rate matching, for tests only: three streams of bits to the
/// 7200-bit output.
#[cfg(test)]
pub fn rate_match(d: &[[u8; D]; 3]) -> [u8; E] {
    let mut e = [0u8; E];
    for (o, (s, i)) in e.iter_mut().zip(positions()) {
        *o = d[s][i];
    }
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_matches_spec() {
        assert_eq!((ROWS, K_PI, N_DUMMY, K0), (45, 1440, 28, 90));
        // Every stream position appears in the circular buffer exactly once.
        let mut seen = [[0u8; D]; 3];
        for w in 0..N_CB {
            if let Some((s, i)) = locate(w) {
                seen[s][i] += 1;
            }
        }
        assert!(seen.iter().all(|s| s.iter().all(|&c| c == 1)));
        // 3 * 1412 = 4236 real bits in the buffer.
        assert_eq!((0..N_CB).filter_map(locate).count(), 3 * D);
    }

    #[test]
    fn systematic_restarts_where_the_reference_says() {
        // The NDSS receiver reads the systematic stream from output bit 4148
        // (after the buffer wraps) for 1412 bits. Check our walk agrees.
        let pos: Vec<_> = positions().collect();
        assert!(pos[4148..4148 + D].iter().all(|&(s, _)| s == 0));
        assert_ne!(pos[4147].0, 0);
        assert_ne!(pos[4148 + D].0, 0);
        // And the whole systematic stream appears there, in interleaved order.
        let mut idx: Vec<_> = pos[4148..4148 + D].iter().map(|&(_, i)| i).collect();
        idx.sort_unstable();
        assert!(idx.iter().enumerate().all(|(n, &i)| n == i));
    }

    #[test]
    fn round_trip() {
        let mut d = [[0u8; D]; 3];
        let mut x = 0x9E37_79B9u32;
        for s in d.iter_mut() {
            for b in s.iter_mut() {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                *b = (x & 1) as u8;
            }
        }
        let e = rate_match(&d);
        let llr: Vec<i8> = e.iter().map(|&b| if b == 0 { 10 } else { -10 }).collect();
        let mut out = [[0i16; D]; 3];
        derate(&llr, &mut out);
        for s in 0..3 {
            for i in 0..D {
                assert!(out[s][i] != 0, "position {s}/{i} never transmitted");
                assert_eq!(out[s][i] < 0, d[s][i] == 1);
            }
        }
        // Repeated positions were combined.
        assert!(out[0].iter().any(|&v| v.abs() == 20));
    }
}
