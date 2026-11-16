//! Cyclic-prefix structure, folded over the symbol period.
//!
//! An OFDM symbol's prefix is a copy of its last `Ncp` samples, so
//! `p[n] = x[n] conj(x[n+L])` (`L` the useful length) has the same phase
//! for every `n` inside a prefix, and a random one elsewhere. `p` is summed
//! into bins by `n mod T` (`T = L + Ncp`, the symbol period, kept in Q8 so a
//! non-integer period does not drift), so the prefixes of every symbol in
//! the capture land in the same few bins and add coherently. The metric is
//! the best window of prefix length: `|sum p| / sum (|x[n]|^2 + |x[n+L]|^2)/2`,
//! x1000, which is about 1000 on a clean prefix and about `1000/sqrt(dof)`
//! on anything else.
//!
//! Two guards against what is not OFDM:
//!
//! - the same window at two **control lags** `L -+ 37` (1.85 µs off): a
//!   carrier, FM video blanking or any slowly varying narrow signal
//!   correlates there just as well, a prefix does not; the metric is main
//!   minus the controls' mean;
//! - a **threshold scaled by the degrees of freedom**: a narrow signal of
//!   bandwidth `B` has only about `B / fs` independent samples per sample,
//!   so its random correlations are larger; `dof = prefix pairs summed x
//!   min(1, B/fs)`, threshold `K / sqrt(dof)`.
//!
//! When the capture saw a burst begin, its symbol timing is known to a block
//! (1.6 µs), and only windows within [`ALIGN_BINS`] of the predicted prefix
//! are tried: fewer tries, so a lower threshold ([`K_ALIGNED`] instead of
//! [`K_FREE`]). That is what makes a 1 MHz, 0.45 ms controller burst
//! detectable at all.
//!
//! Numerologies tried ([`NORMAL15`], [`EXTENDED15`], [`NORMAL30`]): LTE's
//! 15 kHz with normal prefix (4.69 µs; DJI's 144/2048 at 30.72 MS/s) and
//! with extended prefix (16.7 µs), and 30 kHz with normal prefix, a hedge in
//! case O4's wide modes changed numerology (not documented anywhere; 5G
//! NR-U uses it too, so it is never called DJI).

use droneid::math::{C16, isqrt64};

use crate::Scratch;
use crate::burst::{BLOCK, Bursts};
use crate::spectrum::Spectrum;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FoldBin {
    re: i64,
    im: i64,
    pw: u64,
}

impl FoldBin {
    pub const ZERO: FoldBin = FoldBin { re: 0, im: 0, pw: 0 };
}

pub const MAX_BINS: usize = 112;

/// One OFDM numerology at 20 MS/s.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Numerology {
    /// Useful symbol length (the correlation lag), samples.
    pub lag: usize,
    /// Symbol period in Q8 samples (useful + prefix, averaged over a slot).
    pub period_q8: u32,
    /// Samples per fold bin.
    pub bin: usize,
    /// Bins in the summing window (a little shorter than the prefix, so a
    /// misaligned window still lies inside it).
    pub window: usize,
}

impl Numerology {
    pub const fn bins(&self) -> usize {
        (self.period_q8 as usize).div_ceil(256 * self.bin)
    }
    pub const fn period(&self) -> usize {
        self.period_q8 as usize / 256
    }
}

/// 15 kHz, normal prefix: useful 66.67 µs (1333.3 samples); a 0.5 ms slot
/// of seven symbols is 10,000 samples, so the mean period is 1428.57.
/// Prefix 93.75 samples (104.2 on a slot's first symbol); window 80.
pub const NORMAL15: Numerology = Numerology { lag: 1333, period_q8: 365_714, bin: 16, window: 5 };
/// 15 kHz, extended prefix: period 83.33 µs (1666.7), prefix 333.3; window 288.
pub const EXTENDED15: Numerology = Numerology { lag: 1333, period_q8: 426_667, bin: 16, window: 18 };
/// 30 kHz, normal prefix: useful 33.33 µs (666.7), period 714.29, prefix 46.9; window 40.
pub const NORMAL30: Numerology = Numerology { lag: 667, period_q8: 182_857, bin: 8, window: 5 };

/// Control lag offset (samples).
pub const CTL_OFF: usize = 37;
/// Threshold constants, x10: the metric must reach `K / 10 * 1000 / sqrt(dof)`.
pub const K_FREE: u64 = 50;
pub const K_ALIGNED: u64 = 25;
/// Never accept a metric below this (x1000).
pub const FLOOR: i32 = 200;
/// Search half-width around the predicted prefix, bins.
pub const ALIGN_BINS: usize = 3;
/// Narrowest bandwidth credited in the dof (kHz): below it, the width
/// measurement itself is unreliable.
pub const MIN_DOF_KHZ: u32 = 400;

/// The fold for one numerology.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Fold {
    /// Best window's coherence at the lag, and the same window's at the
    /// control lags (mean), x1000.
    pub main: i32,
    pub ctl: i32,
    /// `main - ctl`.
    pub metric: i32,
    /// The threshold it had to beat (x1000).
    pub thr: i32,
    /// Prefix length estimate (samples): contiguous bins around the best
    /// window still at least half as coherent.
    pub cp_samples: u16,
    /// The search was limited to the timing predicted from a burst start.
    pub aligned: bool,
    pub detected: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CpReport {
    pub normal15: Fold,
    pub extended15: Fold,
    pub normal30: Fold,
}

impl CpReport {
    /// 15 kHz OFDM with a normal prefix (DJI's numerology), and better
    /// explained by it than by an extended prefix.
    pub fn dji_numerology(&self) -> bool {
        self.normal15.detected && self.normal15.metric >= self.extended15.metric
    }
}

/// Sum `x[n] conj(x[n+lag])` into `bins` by `n mod period`; only pairs whose
/// both samples lie in on-blocks. Returns the pairs summed.
fn fold(x: &[C16], lag: usize, nm: &Numerology, b: &Bursts, bins: &mut [FoldBin]) -> usize {
    let nb = nm.bins();
    for v in bins[..nb].iter_mut() {
        *v = FoldBin::ZERO;
    }
    if x.len() <= lag {
        return 0;
    }
    let bin_q8 = (nm.bin * 256) as u32;
    let mut phase: u32 = 0;
    let mut pairs = 0;
    for n in 0..x.len() - lag {
        if b.on(n / BLOCK) && b.on((n + lag) / BLOCK) {
            let (a, c) = (x[n], x[n + lag]);
            let (ar, ai, cr, ci) = (a.re as i64, a.im as i64, c.re as i64, c.im as i64);
            let k = (phase / bin_q8) as usize;
            let f = &mut bins[k.min(nb - 1)];
            f.re += ar * cr + ai * ci;
            f.im += ai * cr - ar * ci;
            f.pw += ((ar * ar + ai * ai + cr * cr + ci * ci) / 2) as u64;
            pairs += 1;
        }
        phase += 256;
        if phase >= nm.period_q8 {
            phase -= nm.period_q8;
        }
    }
    pairs
}

/// Coherence (x1000) of the window starting at bin `start`.
fn window(bins: &[FoldBin], nb: usize, start: usize, w: usize) -> i32 {
    let (mut re, mut im, mut pw) = (0i64, 0i64, 0u64);
    for k in 0..w {
        let f = bins[(start + k) % nb];
        re += f.re;
        im += f.im;
        pw += f.pw;
    }
    let mag = droneid::math::C64 { re, im }.abs();
    (mag.saturating_mul(1000).checked_div(pw).unwrap_or(0)).min(1000) as i32
}

/// Best window over `starts`; returns (coherence, start).
fn best(bins: &[FoldBin], nb: usize, w: usize, starts: impl Iterator<Item = usize>) -> (i32, usize) {
    let mut out = (0, 0);
    for s in starts {
        let c = window(bins, nb, s % nb, w);
        if c > out.0 {
            out = (c, s % nb);
        }
    }
    out
}

fn one(x: &[C16], nm: &Numerology, b: &Bursts, spec: &Spectrum, s: &mut Scratch) -> Fold {
    let nb = nm.bins();
    let mut out = Fold::default();
    // Predicted prefix start from the first burst whose start we saw: its
    // first prefix begins there.
    let align = b.first_with_head().filter(|_| !b.continuous).map(|h| {
        let p = (h.start_sample() as u64 * 256 % nm.period_q8 as u64) as usize;
        p / (256 * nm.bin)
    });
    let starts = |c: Option<usize>| -> (usize, usize) {
        match c {
            Some(c) => (c + nb - ALIGN_BINS, 2 * ALIGN_BINS + 1),
            None => (0, nb),
        }
    };
    let (s0, cnt) = starts(align);
    out.aligned = align.is_some();

    let pairs = fold(x, nm.lag, nm, b, &mut s.fold);
    let (main, at) = best(&s.fold, nb, nm.window, s0..s0 + cnt);
    // Prefix length: bins around the best window at least half as coherent.
    let single = |k: usize| window(&s.fold, nb, k % nb, 1);
    let half = main / 2;
    let mut len = nm.window;
    let mut k = at + nb - 1;
    while len < nb && single(k) >= half {
        len += 1;
        k = k + nb - 1;
    }
    let mut k = at + nm.window;
    while len < nb && single(k) >= half {
        len += 1;
        k += 1;
    }
    out.cp_samples = (len * nm.bin) as u16;

    // Control: the same window at lags either side, averaged.
    fold(x, nm.lag - CTL_OFF, nm, b, &mut s.fold);
    let ctl_lo = window(&s.fold, nb, at, nm.window);
    fold(x, nm.lag + CTL_OFF, nm, b, &mut s.fold);
    let ctl = (ctl_lo + window(&s.fold, nb, at, nm.window)) / 2;
    out.main = main;
    out.ctl = ctl;
    out.metric = main - ctl;

    // Degrees of freedom of one window: the pairs it sums, thinned by the
    // signal's bandwidth.
    let per_window = pairs as u64 * (nm.window * nm.bin) as u64 * 256 / nm.period_q8 as u64;
    let bw = spec.w10_khz.clamp(MIN_DOF_KHZ, crate::FS / 1000) as u64;
    let dof = (per_window * bw / (crate::FS as u64 / 1000)).max(1);
    let k = if out.aligned { K_ALIGNED } else { K_FREE };
    out.thr = ((k * 100 * 100 / isqrt64(dof * 10_000).max(1)) as i32).max(FLOOR);
    out.detected = out.metric >= out.thr;
    out
}

pub fn measure(x: &[C16], b: &Bursts, spec: &Spectrum, s: &mut Scratch) -> CpReport {
    CpReport {
        normal15: one(x, &NORMAL15, b, spec, s),
        extended15: one(x, &EXTENDED15, b, spec, s),
        normal30: one(x, &NORMAL30, b, spec, s),
    }
}
