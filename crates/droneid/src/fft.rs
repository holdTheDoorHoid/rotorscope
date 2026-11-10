//! 1024-point fixed-point FFT: radix-2 decimation in time on `i16` data with
//! block floating point.
//!
//! Before each of the ten stages the block is shifted right by 0, 1 or 2 bits
//! so that no butterfly can overflow (a butterfly grows a component by at
//! most 1 + sqrt 2). The total shift is returned as an exponent, so
//! `true_dft = output * 2^exp`. Keeping the block near full scale gives
//! roughly 13 bits of working precision whatever the input level, which is
//! what lets 4-bit captures and full-scale ones share one code path.
//!
//! Cost per transform: 5120 butterflies, each 4 multiplies and 6 adds, plus
//! a bit-reversal pass. Twiddles come from [`crate::math::UNIT`].

use crate::math::{C16, UNIT};

pub const N: usize = 1024;
const LOG2N: u32 = 10;

/// Forward (`inverse = false`, kernel `exp(-j...)`) or inverse (no 1/N)
/// transform in place. Returns the block exponent.
pub fn fft(buf: &mut [C16; N], inverse: bool) -> i32 {
    // Bit-reversal permutation.
    for i in 0..N {
        let j = (i as u32).reverse_bits() >> (32 - LOG2N);
        let j = j as usize;
        if j > i {
            buf.swap(i, j);
        }
    }
    let mut exp = 0i32;
    let mut maxv = max_abs(buf);
    // Small inputs are scaled up first, so twiddle rounding stays far below
    // the signal (a 4-bit capture would otherwise lose ~20 dB here).
    if maxv > 0 && maxv < 4096 {
        let up = (4096i32 / maxv).ilog2().min(14) as i32;
        let up = if (maxv << (up + 1)) < 8192 { up + 1 } else { up };
        for v in buf.iter_mut() {
            v.re = ((v.re as i32) << up) as i16;
            v.im = ((v.im as i32) << up) as i16;
        }
        maxv <<= up;
        exp -= up;
    }
    let mut half = 1usize;
    while half < N {
        // Block scaling so that 2.42 * max stays within i16.
        let shift = if maxv >= 16384 {
            2
        } else if maxv >= 8192 {
            1
        } else {
            0
        };
        exp += shift;
        let rnd = if shift > 0 { 1i32 << (shift - 1) } else { 0 };
        let tw_step = N / (2 * half);
        let mut newmax = 0i32;
        let mut start = 0;
        while start < N {
            for k in 0..half {
                let w = UNIT[k * tw_step];
                // Forward kernel uses conj(w).
                let (wr, wi) = (w.re as i32, if inverse { w.im as i32 } else { -(w.im as i32) });
                let a = buf[start + k];
                let b = buf[start + k + half];
                let (ar, ai) = ((a.re as i32 + rnd) >> shift, (a.im as i32 + rnd) >> shift);
                let (br, bi) = ((b.re as i32 + rnd) >> shift, (b.im as i32 + rnd) >> shift);
                let tr = (br * wr - bi * wi + (1 << 14)) >> 15;
                let ti = (br * wi + bi * wr + (1 << 14)) >> 15;
                let (xr, xi, yr, yi) = (ar + tr, ai + ti, ar - tr, ai - ti);
                newmax = newmax.max(xr.abs()).max(xi.abs()).max(yr.abs()).max(yi.abs());
                buf[start + k] = C16 { re: xr as i16, im: xi as i16 };
                buf[start + k + half] = C16 { re: yr as i16, im: yi as i16 };
            }
            start += 2 * half;
        }
        maxv = newmax;
        half *= 2;
    }
    exp
}

fn max_abs(buf: &[C16; N]) -> i32 {
    buf.iter().fold(0i32, |m, v| m.max((v.re as i32).abs()).max((v.im as i32).abs()))
}

/// FFT bin index of subcarrier `k` (-512..512): negative carriers wrap.
#[inline]
pub fn bin(k: i32) -> usize {
    (k.rem_euclid(N as i32)) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dft(x: &[(f64, f64)], inverse: bool) -> Vec<(f64, f64)> {
        let n = x.len();
        let sgn = if inverse { 1.0 } else { -1.0 };
        (0..n)
            .map(|k| {
                let mut acc = (0.0, 0.0);
                for (t, &(r, i)) in x.iter().enumerate() {
                    let a = sgn * 2.0 * std::f64::consts::PI * ((k * t) % n) as f64 / n as f64;
                    acc.0 += r * a.cos() - i * a.sin();
                    acc.1 += r * a.sin() + i * a.cos();
                }
                acc
            })
            .collect()
    }

    fn check(amp: f64, inverse: bool) {
        let mut seed = 12345u64;
        let mut rnd = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 33) as f64 / (1u64 << 31) as f64) * 2.0 - 1.0
        };
        let x: Vec<(f64, f64)> = (0..N).map(|_| (rnd() * amp, rnd() * amp)).collect();
        let mut buf = [C16::default(); N];
        for (b, &(r, i)) in buf.iter_mut().zip(&x) {
            *b = C16 { re: r.round() as i16, im: i.round() as i16 };
        }
        let xr: Vec<(f64, f64)> = buf.iter().map(|c| (c.re as f64, c.im as f64)).collect();
        let want = dft(&xr, inverse);
        let e = fft(&mut buf, inverse);
        let scale = (2.0f64).powi(e);
        let (mut err, mut pow) = (0.0, 0.0);
        for (g, w) in buf.iter().zip(&want) {
            let (gr, gi) = (g.re as f64 * scale, g.im as f64 * scale);
            err += (gr - w.0).powi(2) + (gi - w.1).powi(2);
            pow += w.0 * w.0 + w.1 * w.1;
        }
        let snr = 10.0 * (pow / err).log10();
        assert!(snr > 55.0, "fft SNR {snr:.1} dB at amp {amp}");
    }

    #[test]
    fn matches_dft_at_all_levels() {
        for amp in [6.0, 100.0, 2000.0, 30000.0] {
            check(amp, false);
            check(amp, true);
        }
    }

    #[test]
    fn single_tone_lands_in_its_bin() {
        let mut buf = [C16::default(); N];
        for (t, b) in buf.iter_mut().enumerate() {
            *b = UNIT[(t * 37) % N];
            b.re /= 4;
            b.im /= 4;
        }
        fft(&mut buf, false);
        let peak = (0..N).max_by_key(|&k| buf[k].norm2()).unwrap();
        assert_eq!(peak, 37);
        assert_eq!(bin(-1), 1023);
    }
}
