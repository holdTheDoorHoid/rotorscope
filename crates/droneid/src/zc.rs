//! Zadoff-Chu reference symbols and the 600-carrier layout.
//!
//! A DroneID burst occupies subcarriers -300..=300 of a 1024-point FFT at
//! 15 kHz spacing, with DC unused. The reference symbols carry a length-601
//! Zadoff-Chu sequence `z_u(i) = exp(-j pi u i (i + 1) / 601)` with
//! `i = carrier + 300`, so element 300 falls on DC and is not sent. Roots 600
//! (first reference) and 147 (second). This agrees with the NDSS receiver's
//! convention and with the O4 spec's index formula (`n(n+1)` is periodic in
//! `n` with period 601, which makes the two descriptions identical).
//!
//! Values come from a 601-entry unit-circle table built at compile time:
//! `i (i + 1) / 2` is an integer, so the phase is a whole multiple of
//! `2 pi / 601` and the lookup is exact to Q15.

use crate::math::{C16, PI, round_i32, sin_cos};

/// Sequence length.
pub const NZC: usize = 601;
/// Data carriers per symbol (601 minus DC).
pub const NCAR: usize = 600;
/// Root of the first reference symbol (the one used for detection).
pub const ROOT_A: u32 = 600;
/// Root of the second reference symbol.
pub const ROOT_B: u32 = 147;

static CIRCLE: [C16; NZC] = {
    let mut t = [C16 { re: 0, im: 0 }; NZC];
    let mut p = 0;
    while p < NZC {
        let (s, c) = sin_cos(2.0 * PI * p as f64 / NZC as f64);
        let re = round_i32(c * 32767.0);
        let im = round_i32(s * 32767.0);
        t[p] = C16 { re: re as i16, im: im as i16 };
        p += 1;
    }
    t
};

/// Subcarrier number of data index `m` (0..600): -300..-1 then 1..300.
#[inline]
pub const fn carrier(m: usize) -> i32 {
    if m < 300 { m as i32 - 300 } else { m as i32 - 299 }
}

/// Zadoff-Chu root `u` on subcarrier `k` (-300..=300, k != 0), Q15.
#[inline]
pub fn zc(u: u32, k: i32) -> C16 {
    let i = (k + 300) as u64;
    let p = (u as u64 * ((i * (i + 1) / 2) % NZC as u64)) % NZC as u64;
    let c = CIRCLE[p as usize];
    // exp(-j 2 pi p / 601)
    C16 { re: c.re, im: -c.im }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_formula_and_published_index_mapping() {
        for m in 0..NCAR {
            let k = carrier(m);
            for u in [ROOT_A, ROOT_B] {
                let i = (k + 300) as f64;
                let a = -PI * u as f64 * i * (i + 1.0) / 601.0;
                let z = zc(u, k);
                assert!((z.re as f64 / 32767.0 - a.cos()).abs() < 1e-4);
                assert!((z.im as f64 / 32767.0 - a.sin()).abs() < 1e-4);
            }
            // O4 spec form: n = m for m < 300, else m - 600;
            // R600 = exp(+j pi n (n+1) / 601), R147 = exp(-j pi 147 n (n+1) / 601).
            let n = if m < 300 { m as f64 } else { m as f64 - 600.0 };
            let a600 = PI * n * (n + 1.0) / 601.0;
            let z = zc(ROOT_A, k);
            assert!((z.re as f64 / 32767.0 - a600.cos()).abs() < 1e-4, "m {m}");
            assert!((z.im as f64 / 32767.0 - a600.sin()).abs() < 1e-4, "m {m}");
        }
        assert_eq!((carrier(0), carrier(299), carrier(300), carrier(599)), (-300, -1, 1, 300));
    }

    #[test]
    fn roots_are_nearly_orthogonal() {
        let mut acc = (0i64, 0i64);
        for m in 0..NCAR {
            let a = zc(ROOT_A, carrier(m));
            let b = zc(ROOT_B, carrier(m));
            let p = a.mul_conj(b);
            acc.0 += p.re as i64;
            acc.1 += p.im as i64;
        }
        let mag = ((acc.0 as f64).powi(2) + (acc.1 as f64).powi(2)).sqrt() / (32767.0f64 * 32767.0 * 600.0);
        assert!(mag < 0.1, "cross-correlation {mag}");
    }
}
