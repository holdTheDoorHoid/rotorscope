//! Features of one frozen capture (20 MS/s complex, DC removed).
//!
//! - **Cyclic-prefix autocorrelation** `|sum x[n] x*[n+L]| / power` at the
//!   useful-symbol lengths of the OFDM families we care about, minus the same
//!   at two nearby lags (a carrier, or the constant-frequency blanking of FM
//!   video, correlates at every lag; a cyclic prefix only at its own):
//!   64 samples = 3.2 µs (802.11a/n/ac, also a 40 MHz channel seen through our
//!   20 MHz), 256 = 12.8 µs (802.11ax data symbols), 128 = 6.4 µs (Wi-Fi at
//!   half clock, Skydio's narrow modes), 1333 = 66.7 µs (LTE-style 15 kHz
//!   carriers: DJI, Walksnail, Autel, Herelink).
//! - **Line rate**: FM discriminator (phase step per sample), summed to
//!   2.5 MS/s, mean removed, normalised autocorrelation over the lags of a
//!   60-68 µs line (14.7-16.7 kHz). Analog video repeats its sync pulse and
//!   most of its picture every line (64 µs PAL, 63.56 µs NTSC), so the
//!   autocorrelation peaks there; the metric is that peak minus the
//!   correlation 6-10 µs either side, which a sync pulse (4.7 µs) has left
//!   but slowly varying picture content has not.
//! - **Spectrum**: sixteen 1024-point FFTs averaged and grouped into 64
//!   groups of 312.5 kHz: occupied width (groups within 10 dB of the
//!   loudest), flatness (arithmetic over geometric mean of those groups,
//!   0 dB = perfectly flat) and the power centroid.

use droneid::fft::{N as NFFT, fft};
use droneid::math::C16;

use crate::db_x10;

/// Most input samples used.
pub const MAX_IN: usize = 16_384;
/// Discriminator samples summed per output (20 -> 2.5 MS/s).
pub const DECIM: usize = 8;
const MAX_D: usize = MAX_IN / DECIM;
/// Rate of the demodulated signal.
pub const FS_D: u32 = crate::FS / DECIM as u32;
/// Line-period search, in 2.5 MS/s samples: 60..=68 µs.
pub const LINE_MIN: usize = 150;
pub const LINE_MAX: usize = 170;
/// Control lags: this far either side of the peak (6.4..=9.6 µs).
const LINE_OFF: core::ops::RangeInclusive<usize> = 16..=24;
const R_LO: usize = LINE_MIN - 24;
const R_HI: usize = LINE_MAX + 24;

const GROUP: usize = 16;
const GROUPS: usize = NFFT / GROUP;
/// Groups used for width/flatness: the half-band filter and analog low-pass
/// roll off above about +-9.4 MHz.
const G_USE: core::ops::Range<usize> = 2..GROUPS - 2;
const GROUP_KHZ: u32 = crate::FS / 1000 / GROUPS as u32;

/// All working memory (about 26 KiB).
pub struct Scratch {
    d: [i32; MAX_D],
    r: [i32; R_HI + 1],
    psd: [u64; GROUPS],
    fft: [C16; NFFT],
}

impl Default for Scratch {
    fn default() -> Self {
        Self::new()
    }
}

impl Scratch {
    pub const fn new() -> Self {
        Scratch { d: [0; MAX_D], r: [0; R_HI + 1], psd: [0; GROUPS], fft: [C16 { re: 0, im: 0 }; NFFT] }
    }

    /// Averaged power spectrum of the last analysis, 64 groups of 312.5 kHz
    /// from -10 MHz up (diagnostics).
    pub fn psd(&self) -> &[u64; GROUPS] {
        &self.psd
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Analysis {
    pub n: usize,
    /// Mean `|x|^2` per sample, in the input's units.
    pub power: u64,
    /// Cyclic-prefix metrics x1000 (correlation at the lag minus the larger
    /// of two control lags): 3.2 µs / 12.8 µs Wi-Fi (the larger), 6.4 µs
    /// half-clock Wi-Fi, 66.7 µs LTE-style.
    pub cp_wifi: i16,
    pub cp_half: i16,
    pub cp_lte: i16,
    /// Line-rate metric x1000 (peak minus control), and the peak's rate.
    pub line: i16,
    pub line_hz: u16,
    /// Occupied width (kHz), flatness (tenths of a dB, 0 = flat) and power
    /// centroid relative to the LO (kHz).
    pub width_khz: u16,
    pub flat_x10: u16,
    pub centroid_khz: i32,
}

/// `|sum x[n] conj(x[n+lag])|` and the two powers, x1000-normalised.
fn cp_corr(x: &[C16], lag: usize) -> i32 {
    if x.len() <= lag + 64 {
        return 0;
    }
    let (mut re, mut im, mut p0, mut p1) = (0i64, 0i64, 0u64, 0u64);
    for n in 0..x.len() - lag {
        let (a, b) = (x[n], x[n + lag]);
        let (ar, ai, br, bi) = (a.re as i32, a.im as i32, b.re as i32, b.im as i32);
        // Products of 4-bit-derived samples stay far below 2^31 (|x| < 2^15,
        // and a sum of two products saturates only at the very rails).
        re += ar.wrapping_mul(br).wrapping_add(ai.wrapping_mul(bi)) as i64;
        im += ai.wrapping_mul(br).wrapping_sub(ar.wrapping_mul(bi)) as i64;
        p0 += (ar * ar) as u64 + (ai * ai) as u64;
        p1 += (br * br) as u64 + (bi * bi) as u64;
    }
    let mag = droneid::math::C64 { re, im }.abs();
    let den = droneid::math::isqrt64(p0) * droneid::math::isqrt64(p1);
    mag.saturating_mul(1000).checked_div(den).unwrap_or(0) as i32
}

/// CP metric at `lag` against controls `lag +- off`.
fn cp_metric(x: &[C16], lag: usize, off: usize) -> i16 {
    let c = cp_corr(x, lag);
    let ctl = cp_corr(x, lag - off).max(cp_corr(x, lag + off));
    (c - ctl).clamp(-1000, 1000) as i16
}

/// Phase of `z` as a fraction of a turn, 65536 = 2 pi, in -32768..32768.
/// About 0.3 degrees worst-case error (atan approximated per octant).
pub fn angle16(re: i32, im: i32) -> i32 {
    if re == 0 && im == 0 {
        return 0;
    }
    let (ax, ay) = (re.unsigned_abs() as i64, im.unsigned_abs() as i64);
    // First-octant angle of min/max, in units of 1/65536 turn:
    // atan(z) ~ z (pi/4 + 0.273 (1 - z)) for 0 <= z <= 1, as a turn fraction.
    let (num, den, swap) = if ay <= ax { (ay, ax, false) } else { (ax, ay, true) };
    let z = (num << 16) / den; // Q16
    // pi/4 rad = 8192 units; 0.273 rad = 2847 units.
    let a = (z * (8192 + ((2847 * (65536 - z)) >> 16))) >> 16;
    let a = if swap { 16384 - a } else { a };
    let a = match (re >= 0, im >= 0) {
        (true, true) => a,
        (false, true) => 32768 - a,
        (false, false) => a - 32768,
        (true, false) => -a,
    };
    a as i32
}

/// Line-rate metric over `x`; fills `s.d` and `s.r`.
fn line_rate(x: &[C16], s: &mut Scratch) -> (i16, u16) {
    let m = ((x.len() - 1) / DECIM).min(MAX_D);
    if m < R_HI + 64 {
        return (0, 0);
    }
    // Discriminator: phase step x[n] * conj(x[n-1]), summed over DECIM.
    let mut sum: i64 = 0;
    for (k, d) in s.d[..m].iter_mut().enumerate() {
        let mut acc = 0i32;
        for n in k * DECIM + 1..=k * DECIM + DECIM {
            let (a, b) = (x[n], x[n - 1]);
            let (ar, ai, br, bi) = (a.re as i32, a.im as i32, b.re as i32, b.im as i32);
            let re = (ar >> 2) * (br >> 2) + (ai >> 2) * (bi >> 2);
            let im = (ai >> 2) * (br >> 2) - (ar >> 2) * (bi >> 2);
            acc += angle16(re, im);
        }
        // DECIM x 32768 = 2^18 at most; >> 4 keeps products inside i32.
        *d = acc >> 4;
        sum += *d as i64;
    }
    let mean = (sum / m as i64) as i32;
    let mut e0: i64 = 0;
    for d in s.d[..m].iter_mut() {
        *d -= mean;
        e0 += (*d as i64) * (*d as i64);
    }
    if e0 == 0 {
        return (0, 0);
    }
    for lag in R_LO..=R_HI {
        let mut c: i64 = 0;
        for k in 0..m - lag {
            c += (s.d[k] * s.d[k + lag]) as i64;
        }
        // Unbiased: scale by m / (m - lag).
        let r = c * 1000 * m as i64 / ((m - lag) as i64 * e0);
        s.r[lag] = r.clamp(-1000, 1000) as i32;
    }
    let (mut best, mut at) = (i32::MIN, LINE_MIN);
    for lag in LINE_MIN..=LINE_MAX {
        if s.r[lag] > best {
            best = s.r[lag];
            at = lag;
        }
    }
    let mut ctl = i32::MIN;
    for off in LINE_OFF {
        ctl = ctl.max(s.r[at - off]).max(s.r[at + off]);
    }
    // Parabolic interpolation of the peak position, in 1/256 sample.
    let (a, b, c) = (s.r[at - 1] as i64, s.r[at] as i64, s.r[at + 1] as i64);
    let den = a - 2 * b + c;
    let frac_q8 = if den < 0 { (128 * (a - c) / den).clamp(-128, 128) } else { 0 };
    let lag_q8 = at as i64 * 256 + frac_q8;
    let hz = FS_D as i64 * 256 / lag_q8;
    ((best - ctl).clamp(-1000, 1000) as i16, hz.clamp(0, u16::MAX as i64) as u16)
}

/// Averaged, grouped power spectrum into `s.psd` (index 0 = -10 MHz).
fn spectrum(x: &[C16], s: &mut Scratch) -> bool {
    s.psd = [0; GROUPS];
    let blocks = x.len() / NFFT;
    if blocks == 0 {
        return false;
    }
    for b in 0..blocks {
        s.fft.copy_from_slice(&x[b * NFFT..(b + 1) * NFFT]);
        let exp = fft(&mut s.fft, false);
        for (k, v) in s.fft.iter().enumerate() {
            let p = v.norm2() as u64;
            let p = if exp >= 0 { p << (2 * exp).min(30) } else { p >> (-2 * exp).min(63) };
            // FFT bin k is frequency k (k < N/2) or k - N: shift so 0 = -fs/2.
            let shifted = (k + NFFT / 2) % NFFT;
            s.psd[shifted / GROUP] = s.psd[shifted / GROUP].saturating_add(p);
        }
    }
    true
}

pub fn analyse(x: &[C16], s: &mut Scratch) -> Analysis {
    let x = &x[..x.len().min(MAX_IN)];
    let mut a = Analysis { n: x.len(), ..Analysis::default() };
    if x.len() < 2048 {
        return a;
    }
    a.power = x.iter().map(|v| v.norm2() as u64).sum::<u64>() / x.len() as u64;
    a.cp_wifi = cp_metric(x, 64, 7).max(cp_metric(x, 256, 20));
    a.cp_half = cp_metric(x, 128, 12);
    a.cp_lte = cp_metric(x, 1333, 40);
    (a.line, a.line_hz) = line_rate(x, s);
    if spectrum(x, s) {
        let peak = s.psd[G_USE].iter().copied().max().unwrap_or(0);
        let thr = peak / 10;
        let (mut n, mut sum, mut sum_db, mut wsum, mut wf) = (0u32, 0u64, 0i64, 0u64, 0i64);
        for g in G_USE {
            let p = s.psd[g];
            let f_khz = (g as i64 * GROUP_KHZ as i64) - 10_000 + GROUP_KHZ as i64 / 2;
            wsum += p >> 16;
            wf += (p >> 16) as i64 * f_khz;
            if p >= thr && p > 0 {
                n += 1;
                sum += p >> 8;
                sum_db += db_x10(p) as i64;
            }
        }
        a.width_khz = (n * GROUP_KHZ).min(u16::MAX as u32) as u16;
        if n > 0 {
            let arith = db_x10((sum / n as u64).max(1) << 8) as i64;
            a.flat_x10 = (arith - sum_db / n as i64).clamp(0, u16::MAX as i64) as u16;
        }
        a.centroid_khz = if wsum > 0 { (wf / wsum as i64) as i32 } else { 0 };
    }
    a
}
