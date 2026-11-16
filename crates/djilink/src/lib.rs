//! Receive-only fingerprints of DJI's radio links from one raw capture of
//! the ESP32-C5 (PLAN.md Phase 11b; numbers and sources in
//! `docs/DJI-LINKS.md`).
//!
//! **Presence only.** Nothing here demodulates a payload, touches a key or
//! transmits. It measures the *shape* of a signal and names the family:
//!
//! - **DJI video downlink** (OcuSync 2/3, O3, O4; drones and FPV air units):
//!   LTE-like OFDM with 15 kHz subcarriers and a 4.7 µs cyclic prefix,
//!   ~9 MHz (10 MHz mode) or ~18 MHz (20 MHz mode) wide or wider, sent in
//!   ~1.07 ms packets of 15 symbols. On 5.170-5.250 GHz only O4 products
//!   transmit video ([`plan::O4_BAND_KHZ`]), so there it is "DJI O4 video".
//! - **DJI controller uplink**: the same numerology, but only ~1.1 MHz
//!   (OcuSync 1/2, "1.4 MHz-CA") or ~2.2 MHz (O3, "3 MHz") wide, in short
//!   bursts of 6-7 symbols (~0.43-0.5 ms) that hop on a 2 MHz raster. One
//!   burst makes a candidate; [`hop::HopTracker`] confirms it when bursts
//!   turn up on different centres.
//! - **DJI FPV (V1 / Vista) air units**: video on DJI's integer-MHz FPV
//!   channels (5660, 5695, 5735, 5770, 5805, 5839, 5878, 5914 MHz) instead
//!   of the half-MHz raster OcuSync drones use ([`plan`]).
//!
//! What it measures, per capture ([`features`]):
//!
//! 1. **Bursts** ([`burst`]): a 1.6 µs envelope, split on and off, giving
//!    each burst's length, whether the capture cut it, and (for the longer
//!    ones) its own centre frequency.
//! 2. **Spectrum** ([`spectrum`]): sixteen 1024-point FFTs over the bursts,
//!    in 256 groups of 78 kHz: occupied width at -3/-10/-20 dB and the band
//!    edges at -6 dB, interpolated, hence the centre to tens of kHz.
//! 3. **Cyclic prefix** ([`cp`]): `x[n] conj(x[n+L])` folded at the symbol
//!    period, so the prefixes of every symbol add up coherently; for LTE
//!    normal CP, LTE extended CP and (as a hedge) 30 kHz numerology. Unlike
//!    one correlation over the whole capture (`fpvscan`'s `cp_lte`) this
//!    finds the prefix of a 0.5 ms, 1 MHz controller burst too.
//!
//! [`classify::classify`] turns them into a [`classify::Verdict`].
//!
//! Integer per-sample work (`i16` samples, `i64` sums); all working memory is
//! in [`Scratch`] (12.6 KiB) plus about 2 KiB of stack; no allocation. The per-dwell
//! [`timeline::Timeline`] (1.5 KiB) and [`hop::HopTracker`] (under 100 B)
//! live in the caller's state.
#![cfg_attr(not(test), no_std)]

pub mod burst;
pub mod classify;
pub mod cp;
pub mod hop;
pub mod plan;
pub mod spectrum;
pub mod timeline;

use droneid::fft::N as NFFT;
use droneid::math::C16;

pub use classify::{Context, Verdict, classify};
pub use hop::HopTracker;
pub use timeline::{Timeline, TimelineStats};

/// Capture rate the analysis expects (the C5 firmware's decimated Path B).
pub const FS: u32 = 20_000_000;
/// Most input samples used (0.82 ms at 20 MS/s, the firmware's capture).
pub const MAX_IN: usize = 16_384;

/// All working memory of one analysis (12.6 KiB). Reused; nothing in it
/// outlives a call except what [`Features`] copies out.
pub struct Scratch {
    pub(crate) fft: [C16; NFFT],
    pub(crate) psd: [u64; spectrum::GROUPS],
    pub(crate) env: [u32; burst::MAX_BLOCKS],
    pub(crate) tmp: [u32; burst::MAX_BLOCKS],
    pub(crate) fold: [cp::FoldBin; cp::MAX_BINS],
}

impl Default for Scratch {
    fn default() -> Self {
        Self::new()
    }
}

impl Scratch {
    pub const fn new() -> Self {
        Scratch {
            fft: [C16 { re: 0, im: 0 }; NFFT],
            psd: [0; spectrum::GROUPS],
            env: [0; burst::MAX_BLOCKS],
            tmp: [0; burst::MAX_BLOCKS],
            fold: [cp::FoldBin::ZERO; cp::MAX_BINS],
        }
    }

    /// The averaged spectrum of the last analysis: 256 groups of 78.125 kHz
    /// from -10 MHz up (diagnostics).
    pub fn psd(&self) -> &[u64; spectrum::GROUPS] {
        &self.psd
    }
}

/// Everything measured on one capture.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Features {
    pub n: usize,
    pub bursts: burst::Bursts,
    pub spec: spectrum::Spectrum,
    pub cp: cp::CpReport,
}

/// Measure one capture (20 MS/s complex, DC removed; at most [`MAX_IN`]
/// samples are used). Costs about 22 FFTs of 1024 points and three passes
/// of complex multiply-accumulate over the capture.
pub fn features(x: &[C16], s: &mut Scratch) -> Features {
    let x = &x[..x.len().min(MAX_IN)];
    let mut f = Features { n: x.len(), ..Features::default() };
    if x.len() < 4 * NFFT {
        return f;
    }
    f.bursts = burst::find(x, s);
    burst::centres(x, &mut f.bursts, s);
    f.spec = spectrum::measure(x, &f.bursts, s);
    f.cp = cp::measure(x, &f.bursts, &f.spec, s);
    f
}

/// `log2(v)` in Q8 (256 = one octave), `v > 0`; 0 for `v == 0`. Max error
/// about 0.005 octave (the same approximation as `fpvscan`).
pub fn log2_q8(v: u64) -> i32 {
    if v == 0 {
        return 0;
    }
    let e = 63 - v.leading_zeros() as i32;
    let f = if e >= 8 { ((v >> (e - 8)) & 0xff) as i32 } else { ((v << (8 - e)) & 0xff) as i32 };
    e * 256 + f + ((f * (256 - f) * 88) >> 16)
}

/// `10 log10(v)` in tenths of a dB.
pub fn db_x10(v: u64) -> i32 {
    (log2_q8(v) * 7706) >> 16
}

#[cfg(test)]
mod tests;
