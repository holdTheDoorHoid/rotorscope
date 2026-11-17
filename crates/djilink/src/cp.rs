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
//! detectable at all. The free search (all windows, [`K_FREE`]) runs too and
//! wins when only it detects: real O4 bursts (RFUAV Mini 4 Pro, 2.4 GHz)
//! open with one to three symbols of other lengths (gaps of 33, 44 and
//! 47 µs before the regular 35.7 µs grid), so the predicted window misses
//! the prefixes of the rest of the burst.
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

    /// 30 kHz OFDM with a normal prefix, better explained by it than by
    /// either 15 kHz fold: DJI O4's numerology (measured on the RFUAV
    /// Mini 4 Pro recordings: useful symbol 33.33 µs, period 35.7 µs, video
    /// and controller uplink both; docs/DJI-LINKS.md).
    pub fn o4_numerology(&self) -> bool {
        self.normal30.detected && self.normal30.metric >= self.normal15.metric.max(self.extended15.metric)
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
    // Predicted prefix start from the first burst whose start we saw: its
    // first prefix begins there.
    let align = b.first_with_head().filter(|_| !b.continuous).map(|h| {
        let p = (h.start_sample() as u64 * 256 % nm.period_q8 as u64) as usize;
        p / (256 * nm.bin)
    });

    let pairs = fold(x, nm.lag, nm, b, &mut s.fold);
    // Two candidates: the window the burst start predicts (lower
    // threshold), and the best anywhere (higher). Real O4 bursts (RFUAV
    // Mini 4 Pro) begin with one to three symbols of other lengths, so the
    // prediction can miss the prefix grid of the rest of the burst.
    let cand_a = align.map(|c| best(&s.fold, nb, nm.window, (c + nb - ALIGN_BINS)..(c + nb + ALIGN_BINS + 1)));
    let cand_f = best(&s.fold, nb, nm.window, 0..nb);
    // Prefix length: bins around a window at least half as coherent.
    let cp_len = |fold: &[FoldBin], main: i32, at: usize| -> u16 {
        let single = |k: usize| window(fold, nb, k % nb, 1);
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
        (len * nm.bin) as u16
    };
    let cp_a = cand_a.map(|(m, at)| cp_len(&s.fold, m, at)).unwrap_or(0);
    let cp_f = cp_len(&s.fold, cand_f.0, cand_f.1);

    // Control: the same windows at lags either side, averaged.
    let at_a = cand_a.map(|c| c.1).unwrap_or(0);
    fold(x, nm.lag - CTL_OFF, nm, b, &mut s.fold);
    let (lo_a, lo_f) = (window(&s.fold, nb, at_a, nm.window), window(&s.fold, nb, cand_f.1, nm.window));
    fold(x, nm.lag + CTL_OFF, nm, b, &mut s.fold);
    let ctl_a = (lo_a + window(&s.fold, nb, at_a, nm.window)) / 2;
    let ctl_f = (lo_f + window(&s.fold, nb, cand_f.1, nm.window)) / 2;

    // Degrees of freedom of one window: the pairs it sums, thinned by the
    // signal's bandwidth.
    let per_window = pairs as u64 * (nm.window * nm.bin) as u64 * 256 / nm.period_q8 as u64;
    let bw = spec.w10_khz.clamp(MIN_DOF_KHZ, crate::FS / 1000) as u64;
    let dof = (per_window * bw / (crate::FS as u64 / 1000)).max(1);
    let thr = |k: u64| ((k * 100 * 100 / isqrt64(dof * 10_000).max(1)) as i32).max(FLOOR);

    let free = Fold { main: cand_f.0, ctl: ctl_f, metric: cand_f.0 - ctl_f, thr: thr(K_FREE), cp_samples: cp_f, aligned: false, detected: false };
    let mut out = match cand_a {
        Some((m, _)) => {
            let a = Fold { main: m, ctl: ctl_a, metric: m - ctl_a, thr: thr(K_ALIGNED), cp_samples: cp_a, aligned: true, detected: false };
            // The aligned window unless only the free search detects.
            if a.metric < a.thr && free.metric >= free.thr { free } else { a }
        }
        None => free,
    };
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
