//! Float DSP for the host tool: an FFT, a streaming mixer + rational
//! polyphase resampler, the C5's half-band in float, and a Gaussian source.

use std::f64::consts::PI;

/// Complex sample as `[re, im]`.
pub type Cf = [f32; 2];

// ------------------------------------------------------------------ FFT --

pub struct Fft {
    n: usize,
    tw: Vec<Cf>,
    rev: Vec<u32>,
}

impl Fft {
    pub fn new(n: usize) -> Fft {
        assert!(n.is_power_of_two());
        let bits = n.trailing_zeros();
        let rev = (0..n as u32).map(|i| if bits == 0 { 0 } else { i.reverse_bits() >> (32 - bits) }).collect();
        let tw = (0..n / 2)
            .map(|k| {
                let a = -2.0 * PI * k as f64 / n as f64;
                [a.cos() as f32, a.sin() as f32]
            })
            .collect();
        Fft { n, tw, rev }
    }

    /// In place; `inverse` conjugates the twiddles (no 1/n scaling).
    pub fn run(&self, a: &mut [Cf], inverse: bool) {
        let n = self.n;
        assert_eq!(a.len(), n);
        for i in 0..n {
            let j = self.rev[i] as usize;
            if i < j {
                a.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            let step = n / len;
            for i in (0..n).step_by(len) {
                for k in 0..len / 2 {
                    let w = self.tw[k * step];
                    let wi = if inverse { -w[1] } else { w[1] };
                    let u = a[i + k];
                    let v = a[i + k + len / 2];
                    let t = [v[0] * w[0] - v[1] * wi, v[0] * wi + v[1] * w[0]];
                    a[i + k] = [u[0] + t[0], u[1] + t[1]];
                    a[i + k + len / 2] = [u[0] - t[0], u[1] - t[1]];
                }
            }
            len <<= 1;
        }
    }
}

// --------------------------------------------------------- filter design --

/// Blackman-windowed sinc low-pass, `n` taps, cut-off `fc` (Hz, -6 dB) at
/// rate `fs`, DC gain `gain`.
pub fn lowpass(n: usize, fc: f64, fs: f64, gain: f64) -> Vec<f64> {
    let m = (n - 1) as f64 / 2.0;
    let mut h: Vec<f64> = (0..n)
        .map(|i| {
            let t = i as f64 - m;
            let s = if t.abs() < 1e-12 { 2.0 * fc / fs } else { (2.0 * PI * fc / fs * t).sin() / (PI * t) };
            let w = 0.42 - 0.5 * (2.0 * PI * i as f64 / (n - 1) as f64).cos() + 0.08 * (4.0 * PI * i as f64 / (n - 1) as f64).cos();
            s * w
        })
        .collect();
    let sum: f64 = h.iter().sum();
    for v in h.iter_mut() {
        *v *= gain / sum;
    }
    h
}

// ------------------------------------------------- mixer + resampler --

/// Streaming "shift by `-f_off`, low-pass, resample by `up/down`" from
/// interleaved complex input. Input and output are split into re/im arrays
/// so the dot products vectorise.
pub struct Channel {
    up: usize,
    down: usize,
    /// Per phase, taps reversed so they align with ascending input.
    phases: Vec<Vec<f32>>,
    k: usize,
    /// Mixed input history (re, im), oldest first.
    hr: Vec<f32>,
    hi: Vec<f32>,
    /// Absolute input index of `hr[0]`.
    h0: u64,
    /// Next output index (absolute).
    next_out: u64,
    /// Mixer phasor and its per-sample step.
    ph: [f64; 2],
    step: [f64; 2],
}

impl Channel {
    /// `f_off`: frequency (Hz, relative to the input's centre) moved to DC.
    /// `fc`: low-pass cut-off (Hz). `taps_per_phase`: filter length / up.
    pub fn new(fs_in: f64, f_off: f64, up: usize, down: usize, fc: f64, taps_per_phase: usize) -> Channel {
        let n = taps_per_phase * up;
        let h = lowpass(n, fc, fs_in * up as f64, up as f64);
        let mut phases = vec![vec![0f32; taps_per_phase]; up];
        // y[m] (at up*fs) = sum_j h[j] u[m - j]; u nonzero at multiples of up.
        // For phase p = m mod up: taps h[p + k*up] multiply x[(m-p)/up - k].
        for (p, ph) in phases.iter_mut().enumerate() {
            for k in 0..taps_per_phase {
                // ascending input order: index k' = taps-1-k
                ph[taps_per_phase - 1 - k] = h[p + k * up] as f32;
            }
        }
        let a = -2.0 * PI * f_off / fs_in;
        Channel {
            up,
            down,
            phases,
            k: taps_per_phase,
            hr: vec![0.0; taps_per_phase],
            hi: vec![0.0; taps_per_phase],
            h0: 0u64.wrapping_sub(taps_per_phase as u64),
            next_out: 0,
            ph: [1.0, 0.0],
            step: [a.cos(), a.sin()],
        }
    }

    /// Feed interleaved input; append outputs to `or`, `oi`.
    pub fn push(&mut self, x: &[Cf], or: &mut Vec<f32>, oi: &mut Vec<f32>) {
        // Mix into the history.
        let (mut pr, mut pi) = (self.ph[0], self.ph[1]);
        let (sr, si) = (self.step[0], self.step[1]);
        self.hr.reserve(x.len());
        self.hi.reserve(x.len());
        for (i, s) in x.iter().enumerate() {
            let (a, b) = (s[0] as f64, s[1] as f64);
            self.hr.push((a * pr - b * pi) as f32);
            self.hi.push((a * pi + b * pr) as f32);
            let t = pr * sr - pi * si;
            pi = pr * si + pi * sr;
            pr = t;
            if i & 1023 == 1023 {
                let g = 1.0 / (pr * pr + pi * pi).sqrt();
                pr *= g;
                pi *= g;
            }
        }
        let g = 1.0 / (pr * pr + pi * pi).sqrt();
        self.ph = [pr * g, pi * g];
        let end = self.h0.wrapping_add(self.hr.len() as u64); // exclusive absolute
        loop {
            let m = self.next_out * self.down as u64;
            let base = m / self.up as u64; // newest input used
            let p = (m % self.up as u64) as usize;
            if base >= end {
                break;
            }
            // Inputs base-k+1 ..= base, i.e. history index from.
            let from = (base + 1).wrapping_sub(self.k as u64).wrapping_sub(self.h0) as usize;
            let taps = &self.phases[p];
            let xr = &self.hr[from..from + self.k];
            let xi = &self.hi[from..from + self.k];
            or.push(dot(taps, xr));
            oi.push(dot(taps, xi));
            self.next_out += 1;
        }
        // Keep the last k inputs.
        let keep = self.k;
        let drop = self.hr.len() - keep;
        self.hr.drain(..drop);
        self.hi.drain(..drop);
        self.h0 = self.h0.wrapping_add(drop as u64);
    }
}

#[inline]
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0f32; 8];
    let ca = a.chunks_exact(8);
    let cb = b.chunks_exact(8);
    let (ra, rb) = (ca.remainder(), cb.remainder());
    for (x, y) in ca.zip(cb) {
        for j in 0..8 {
            acc[j] += x[j] * y[j];
        }
    }
    let mut s: f32 = acc.iter().sum();
    for (x, y) in ra.iter().zip(rb) {
        s += x * y;
    }
    s
}

// ---------------------------------------------------- C5 half-band --

/// The C5's 2:1 half-band (rawcap::dsp taps 256 + 150/-25/3 at odd offsets,
/// /512) in float: `out[m]` centred on input `2m + 5`.
pub fn halfband(xr: &[f32], xi: &[f32]) -> Vec<Cf> {
    const H: [f32; 3] = [150.0 / 512.0, -25.0 / 512.0, 3.0 / 512.0];
    let n = (xr.len().saturating_sub(10)) / 2;
    (0..n)
        .map(|m| {
            let c = 2 * m + 5;
            let mut r = 0.5 * xr[c];
            let mut i = 0.5 * xi[c];
            for (t, &h) in H.iter().enumerate() {
                let d = 2 * t + 1;
                r += h * (xr[c - d] + xr[c + d]);
                i += h * (xi[c - d] + xi[c + d]);
            }
            [r, i]
        })
        .collect()
}

// ---------------------------------------------------------- noise --

pub struct Rng(pub u64);

impl Rng {
    #[inline]
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    #[inline]
    pub fn u(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    /// Two independent standard normals.
    #[inline]
    pub fn g2(&mut self) -> (f32, f32) {
        let a = self.u().max(1e-300);
        let b = self.u();
        let r = (-2.0 * a.ln()).sqrt();
        let (s, c) = (2.0 * PI * b).sin_cos();
        ((r * c) as f32, (r * s) as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_moves_a_tone_to_dc_and_keeps_its_level() {
        // A tone at +7 MHz in 100 MS/s, channel centred there, out at 40 MS/s.
        let fs = 100e6;
        let x: Vec<Cf> = (0..100_000)
            .map(|n| {
                let a = 2.0 * PI * 7.3e6 * n as f64 / fs;
                [a.cos() as f32, a.sin() as f32]
            })
            .collect();
        let mut ch = Channel::new(fs, 7e6, 2, 5, 13e6, 64);
        let (mut r, mut i) = (vec![], vec![]);
        for part in x.chunks(7777) {
            ch.push(part, &mut r, &mut i);
        }
        assert!((r.len() as i64 - 40_000).abs() <= 2, "{}", r.len());
        // Steady state power ~1 and frequency +0.3 MHz at 40 MS/s.
        let p: f32 = r[1000..].iter().zip(&i[1000..]).map(|(a, b)| a * a + b * b).sum::<f32>() / (r.len() - 1000) as f32;
        assert!((p - 1.0).abs() < 0.05, "power {p}");
        let mut ph = 0.0;
        for k in 1000..2000 {
            let (a, b) = (r[k] as f64, i[k] as f64);
            let (c, d) = (r[k + 1] as f64, i[k + 1] as f64);
            ph += (b * c - a * d).atan2(a * c + b * d) * -1.0;
        }
        let f = ph / 1000.0 / (2.0 * PI) * 40e6;
        assert!((f - 0.3e6).abs() < 5e3, "freq {f}");
        // A tone 30 MHz away is gone.
        let y: Vec<Cf> = (0..100_000)
            .map(|n| {
                let a = 2.0 * PI * 37e6 * n as f64 / fs;
                [a.cos() as f32, a.sin() as f32]
            })
            .collect();
        let mut ch = Channel::new(fs, 7e6, 2, 5, 13e6, 64);
        let (mut r, mut i) = (vec![], vec![]);
        ch.push(&y, &mut r, &mut i);
        let p: f32 = r[1000..].iter().zip(&i[1000..]).map(|(a, b)| a * a + b * b).sum::<f32>() / (r.len() - 1000) as f32;
        assert!(p < 1e-4, "stop {p}");
    }

    #[test]
    fn fft_roundtrip() {
        let f = Fft::new(64);
        let mut a: Vec<Cf> = (0..64).map(|k| [k as f32, -(k as f32) * 0.5]).collect();
        let b = a.clone();
        f.run(&mut a, false);
        f.run(&mut a, true);
        for (x, y) in a.iter().zip(&b) {
            assert!((x[0] / 64.0 - y[0]).abs() < 1e-3 && (x[1] / 64.0 - y[1]).abs() < 1e-3);
        }
    }
}
