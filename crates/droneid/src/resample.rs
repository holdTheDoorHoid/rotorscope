//! Frequency shift plus rational resampling to 15.36 MS/s.
//!
//! A polyphase FIR with `L` phases of [`TAPS`] taps each implements
//! up-by-L, low-pass, down-by-M for `L / M = 15.36 MHz / fs_in` in lowest
//! terms (50 MS/s: 192/625, 40: 48/125, 30.72: 1/2, 20: 96/125, 15.36: 1/1).
//! Taps are Q14 `i16`; the multiply-accumulate is `i16 x i16 -> i32`, about
//! `TAPS` MACs per output sample.
//!
//! The input is mixed by an integer NCO on the way in (once per input
//! sample, in chunks), because recordings are not centred on the burst and
//! the low-pass must see it at DC.
//!
//! Filter design (Kaiser-windowed sinc, cut-off 7.6 MHz, beta 5) runs once
//! per input rate in `f64`; on a chip without an FPU that is soft-float, a
//! few tens of milliseconds at start-up, never per burst.

use crate::math::{C16, PI, sin_cos};

/// Output rate.
pub const FS_OUT: u32 = 15_360_000;
/// Taps per polyphase branch.
pub const TAPS: usize = 24;
/// Largest supported interpolation factor `L`.
pub const MAX_L: usize = 192;
/// Low-pass cut-off. The burst spans +-4.5 MHz; images of anything above
/// 10.86 MHz would alias back into the band, so the transition sits between.
const CUTOFF_HZ: f64 = 7.6e6;
const KAISER_BETA: f64 = 5.0;

/// Why a rate is not accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RateError {
    /// Below 15.36 MS/s the burst does not fit.
    TooLow,
    /// `L` after reduction exceeds [`MAX_L`].
    Awkward,
}

pub struct Resampler {
    pub fs_in: u32,
    pub l: u32,
    pub m: u32,
    taps: [[i16; TAPS]; MAX_L],
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

fn sqrt_f64(v: f64) -> f64 {
    if v <= 0.0 {
        return 0.0;
    }
    let mut x = if v > 1.0 { v } else { 1.0 };
    for _ in 0..60 {
        let y = 0.5 * (x + v / x);
        if (y - x).abs() <= 1e-15 * x {
            return y;
        }
        x = y;
    }
    x
}

/// Modified Bessel function I0 by its power series.
fn bessel_i0(x: f64) -> f64 {
    let q = x * x / 4.0;
    let mut term = 1.0;
    let mut sum = 1.0;
    let mut k = 1.0;
    while term > 1e-12 * sum {
        term *= q / (k * k);
        sum += term;
        k += 1.0;
    }
    sum
}

impl Resampler {
    /// Design the filter for `fs_in` samples per second.
    pub fn new(fs_in: u32) -> Result<Self, RateError> {
        if fs_in < FS_OUT {
            return Err(RateError::TooLow);
        }
        let g = gcd(FS_OUT as u64, fs_in as u64);
        let l = (FS_OUT as u64 / g) as u32;
        let m = (fs_in as u64 / g) as u32;
        if l as usize > MAX_L {
            return Err(RateError::Awkward);
        }
        let mut taps = [[0i16; TAPS]; MAX_L];
        if l == 1 && m == 1 {
            taps[0][0] = 1 << 14;
        } else {
            let n = l as usize * TAPS;
            let fs_hi = fs_in as f64 * l as f64;
            let fc = CUTOFF_HZ / fs_hi; // cycles per high-rate sample
            let centre = (n as f64 - 1.0) / 2.0;
            let i0b = bessel_i0(KAISER_BETA);
            // One branch at a time (no full-length prototype on the stack),
            // each normalised to unit DC gain so no phase is louder.
            for (ph, row) in taps.iter_mut().enumerate().take(l as usize) {
                let mut v = [0f64; TAPS];
                let mut sum = 0.0;
                for (t, vt) in v.iter_mut().enumerate() {
                    let j = ph + t * l as usize;
                    let x = j as f64 - centre;
                    let sinc =
                        if x.abs() < 1e-9 { 2.0 * fc } else { sin_cos(2.0 * PI * fc * x).0 / (PI * x) };
                    let r = 2.0 * x / (n as f64 - 1.0);
                    let w = bessel_i0(KAISER_BETA * sqrt_f64(1.0 - r * r)) / i0b;
                    *vt = sinc * w;
                    sum += *vt;
                }
                for (dst, &vt) in row.iter_mut().zip(&v) {
                    *dst = crate::math::round_i32(vt / sum * 16384.0).clamp(-32768, 32767) as i16;
                }
            }
        }
        Ok(Self { fs_in, l, m, taps })
    }

    /// Number of taps actually used (1 when no resampling is needed).
    fn ntaps(&self) -> usize {
        if self.l == 1 && self.m == 1 { 1 } else { TAPS }
    }

    /// Group delay in input samples (the output lags the input by this).
    pub fn delay_in(&self) -> f64 {
        if self.ntaps() == 1 { 0.0 } else { (self.l as f64 * TAPS as f64 - 1.0) / 2.0 / self.l as f64 }
    }

    /// Resample `x[start..]` after subtracting `dc` and multiplying sample
    /// `i` by `exp(j * step * i)` (binary angle per sample, absolute index),
    /// filling `out`. Output `n` is centred on input position `start + n * M
    /// / L - delay`. Samples outside `x` count as zero. Returns outputs
    /// written.
    pub fn run(&self, x: &[C16], start: usize, step: u32, dc: C16, out: &mut [C16]) -> usize {
        const CH: usize = 1024;
        let t = self.ntaps();
        let mut buf = [C16::default(); CH + TAPS];
        let (l, m) = (self.l as u64, self.m as u64);
        // buf[j] holds the mixed x[origin + j].
        let mut origin: i64 = i64::MIN;
        let mut written = 0;
        for (n, o) in out.iter_mut().enumerate() {
            let num = n as u64 * m;
            let base = start as i64 + (num / l) as i64;
            let ph = (num % l) as usize;
            if base >= x.len() as i64 + t as i64 {
                break;
            }
            if origin == i64::MIN || base >= origin + (CH + t - 1) as i64 {
                origin = base - (t as i64 - 1);
                for (j, b) in buf.iter_mut().enumerate().take(CH + t - 1) {
                    let i = origin + j as i64;
                    *b = if i >= 0 && (i as usize) < x.len() {
                        let w = crate::math::cis(step.wrapping_mul(i as u32));
                        let v = x[i as usize];
                        let v = C16 { re: v.re.saturating_sub(dc.re), im: v.im.saturating_sub(dc.im) };
                        v.mul_q15(w)
                    } else {
                        C16::default()
                    };
                }
            }
            let taps = &self.taps[ph];
            let mut acc = (0i32, 0i32);
            let b0 = (base - origin) as usize;
            for (k, &h) in taps.iter().enumerate().take(t) {
                let v = buf[b0 - k];
                acc.0 += v.re as i32 * h as i32;
                acc.1 += v.im as i32 * h as i32;
            }
            *o = C16 { re: crate::math::sat16((acc.0 + (1 << 13)) >> 14), im: crate::math::sat16((acc.1 + (1 << 13)) >> 14) };
            written += 1;
        }
        written
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone_through(fs: u32, f_tone: f64) -> f64 {
        let r = Resampler::new(fs).unwrap();
        let n_in = 40_000;
        let x: Vec<C16> = (0..n_in)
            .map(|i| {
                let a = 2.0 * std::f64::consts::PI * f_tone * i as f64 / fs as f64;
                C16 { re: (a.cos() * 8000.0) as i16, im: (a.sin() * 8000.0) as i16 }
            })
            .collect();
        let mut out = vec![C16::default(); 4000];
        let got = r.run(&x, 2000, 0, C16::default(), &mut out);
        assert_eq!(got, 4000);
        let p: f64 = out[100..].iter().map(|c| c.norm2() as f64).sum::<f64>() / (out.len() - 100) as f64;
        10.0 * (p / 64e6).log10()
    }

    #[test]
    fn rates_reduce() {
        for (fs, l, m) in [(50_000_000, 192, 625), (40_000_000, 48, 125), (30_720_000, 1, 2), (20_000_000, 96, 125), (15_360_000, 1, 1)] {
            let r = Resampler::new(fs).unwrap();
            assert_eq!((r.l, r.m), (l, m), "{fs}");
        }
        assert_eq!(Resampler::new(10_000_000).err(), Some(RateError::TooLow));
    }

    #[test]
    fn passband_flat_stopband_quiet() {
        for fs in [50_000_000, 40_000_000, 30_720_000, 20_000_000] {
            for f in [0.0, 2.0e6, -4.5e6] {
                let g = tone_through(fs, f);
                assert!(g.abs() < 0.3, "{fs}: {f} Hz gain {g:.2} dB");
            }
            // A tone that would alias onto the band edge must be suppressed.
            if fs > 22_000_000 {
                let g = tone_through(fs, 11.5e6);
                assert!(g < -40.0, "{fs}: alias rejection only {g:.1} dB");
            }
        }
    }

    #[test]
    fn mixing_moves_a_tone_to_dc() {
        let fs = 50_000_000u32;
        let r = Resampler::new(fs).unwrap();
        let f = 6.0e6;
        let x: Vec<C16> = (0..20_000)
            .map(|i| {
                let a = 2.0 * std::f64::consts::PI * f * i as f64 / fs as f64;
                C16 { re: (a.cos() * 8000.0) as i16, im: (a.sin() * 8000.0) as i16 }
            })
            .collect();
        let step = crate::math::hz_to_step(-f, fs as f64);
        let mut out = vec![C16::default(); 3000];
        r.run(&x, 1000, step, C16::default(), &mut out);
        // At DC the output is a constant phasor.
        let a = out[500];
        for o in &out[500..2900] {
            assert!((o.re as i32 - a.re as i32).abs() < 200 && (o.im as i32 - a.im as i32).abs() < 200);
        }
    }
}
