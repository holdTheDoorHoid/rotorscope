//! Averaged spectrum over a capture's bursts, at 78 kHz resolution.
//!
//! Sixteen 1024-point FFTs (19.5 kHz bins) are averaged into 256 groups of
//! four bins. Only FFT blocks that lie mostly inside bursts are used, so a
//! 0.5 ms controller burst is not diluted by the noise around it. The two
//! groups at DC (receiver residual) are replaced by their neighbours' mean.
//!
//! From the plateau level (the eighth-strongest group, or an eighth of the
//! strongest if that is higher, so a carrier does not read as wide):
//!
//! - occupied width at -3, -10 and -20 dB: groups at or above the level;
//! - band edges at -6 dB: the outermost groups at or above it, refined by
//!   interpolating in dB between the crossing groups, giving the centre to
//!   tens of kHz on a clean signal;
//! - whether an edge ran into the capture's usable band (+-9.4 MHz, where
//!   the half-band filter rolls off): then the signal is wider than we see
//!   and the centre is unknown.

use droneid::fft::{N as NFFT, fft};
use droneid::math::C16;

use crate::Scratch;
use crate::burst::{BLOCK, Bursts};

pub const GROUP_BINS: usize = 4;
pub const GROUPS: usize = NFFT / GROUP_BINS;
/// Groups inside the half-band filter's passband (+-9.375 MHz).
pub const USE: core::ops::Range<usize> = 8..GROUPS - 8;
/// The groups at DC.
const DC: [usize; 2] = [GROUPS / 2 - 1, GROUPS / 2];
/// Width of a group, Hz.
pub const GROUP_HZ: u32 = crate::FS / GROUPS as u32;

/// Frequency (Hz, relative to the LO) of group position `pos_q8` (group
/// index in Q8, 256 = one group; the group's centre is at its index).
pub const fn group_hz(pos_q8: i64) -> i64 {
    // Bin of group g's centre: 4g + 1.5; bin 512 is DC; 19,531.25 Hz a bin.
    (pos_q8 * GROUP_BINS as i64 + 384 - (NFFT as i64 / 2) * 256) * 1_953_125 / 25_600
}

/// Powers of one transform's bins, grouped (index 0 = -10 MHz), unscaled.
pub fn group_fft<'a>(f: &[C16; NFFT], psd: &'a mut [u64; GROUPS]) -> &'a [u64; GROUPS] {
    *psd = [0; GROUPS];
    for (k, v) in f.iter().enumerate() {
        let shifted = (k + NFFT / 2) % NFFT;
        psd[shifted / GROUP_BINS] += v.norm2() as u64;
    }
    psd
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Spectrum {
    /// FFT blocks averaged.
    pub segments: u8,
    /// Occupied widths (kHz) at -3, -10, -20 dB under the plateau.
    pub w3_khz: u32,
    pub w10_khz: u32,
    pub w20_khz: u32,
    /// Band edges at -6 dB and their mid point, relative to the LO (kHz).
    pub lo_khz: i32,
    pub hi_khz: i32,
    pub centre_khz: i32,
    /// An edge ran into the usable band's limit.
    pub clip_lo: bool,
    pub clip_hi: bool,
    /// Plateau over the median group (tenths of a dB): how far the signal
    /// stands out of the capture's noise.
    pub snr_x10: i16,
}

impl Spectrum {
    pub const fn clipped(&self) -> bool {
        self.clip_lo || self.clip_hi
    }
    /// -6 dB width (kHz).
    pub const fn w6_khz(&self) -> u32 {
        (self.hi_khz - self.lo_khz).unsigned_abs()
    }
}

/// Whether FFT block `seg` (1024 samples) is mostly inside bursts.
fn mostly_on(b: &Bursts, seg: usize) -> bool {
    let per = NFFT / BLOCK;
    let on = (seg * per..(seg + 1) * per).filter(|&k| b.on(k)).count();
    on * 4 >= per * 3
}

pub fn measure(x: &[C16], bursts: &Bursts, s: &mut Scratch) -> Spectrum {
    let mut out = Spectrum::default();
    let blocks = x.len() / NFFT;
    let any_on = (0..blocks).any(|g| mostly_on(bursts, g));
    s.psd = [0; GROUPS];
    for g in 0..blocks {
        if any_on && !mostly_on(bursts, g) {
            continue;
        }
        s.fft.copy_from_slice(&x[g * NFFT..(g + 1) * NFFT]);
        let exp = fft(&mut s.fft, false);
        for (k, v) in s.fft.iter().enumerate() {
            let p = v.norm2() as u64;
            let p = if exp >= 0 { p << (2 * exp).min(30) } else { p >> (-2 * exp).min(63) };
            let shifted = (k + NFFT / 2) % NFFT;
            s.psd[shifted / GROUP_BINS] = s.psd[shifted / GROUP_BINS].saturating_add(p);
        }
        out.segments += 1;
    }
    if out.segments == 0 {
        return out;
    }
    let fill = (s.psd[DC[0] - 1] / 2).saturating_add(s.psd[DC[1] + 1] / 2);
    for g in DC {
        s.psd[g] = fill;
    }
    let psd = &s.psd;

    // Plateau and median, from a sorted copy of the usable groups (in the
    // envelope scratch's spare: two u32 halves would lose range, so sort a
    // u64 copy on the stack; 240 x 8 bytes).
    let mut sorted = [0u64; GROUPS - 16];
    sorted.copy_from_slice(&psd[USE]);
    sorted.sort_unstable();
    let n = sorted.len();
    let max = sorted[n - 1];
    let plateau = sorted[n - 8].max(max / 8).max(1);
    let median = sorted[n / 2].max(1);
    out.snr_x10 = (crate::db_x10(plateau) - crate::db_x10(median)).clamp(-32768, 32767) as i16;

    let count = |thr: u64| USE.filter(|&g| psd[g] >= thr).count() as u32 * GROUP_HZ / 1000;
    out.w3_khz = count(plateau / 2);
    out.w10_khz = count(plateau / 10);
    out.w20_khz = count(plateau / 100);

    // Edges at -6 dB, interpolated in dB between the crossing groups.
    let thr = plateau / 4;
    let thr_db = crate::db_x10(thr);
    let first = USE.clone().find(|&g| psd[g] >= thr);
    let last = USE.clone().rev().find(|&g| psd[g] >= thr);
    let (Some(first), Some(last)) = (first, last) else { return out };
    let edge = |inside: usize, outside: usize| -> i64 {
        // Position (Q8 groups) where the level crosses thr between them.
        let (di, do_) = (crate::db_x10(psd[inside]), crate::db_x10(psd[outside].max(1)));
        let frac_q8 = if di > do_ { ((thr_db - do_).clamp(0, di - do_) as i64 * 256) / (di - do_) as i64 } else { 128 };
        let (a, b) = (outside as i64 * 256, inside as i64 * 256);
        a + (b - a) * frac_q8 / 256
    };
    out.clip_lo = first <= USE.start;
    out.clip_hi = last >= USE.end - 1;
    let lo = if out.clip_lo { first as i64 * 256 - 128 } else { edge(first, first - 1) };
    let hi = if out.clip_hi { last as i64 * 256 + 128 } else { edge(last, last + 1) };
    out.lo_khz = (group_hz(lo) / 1000) as i32;
    out.hi_khz = (group_hz(hi) / 1000) as i32;
    out.centre_khz = ((group_hz(lo) + group_hz(hi)) / 2000) as i32;
    out
}
