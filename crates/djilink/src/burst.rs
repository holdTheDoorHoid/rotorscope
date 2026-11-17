//! Bursts inside one capture: a 1.6 µs power envelope split into on and off.
//!
//! The capture's own levels set the split, because the sweep's noise floor
//! is in different units: when the loudest blocks (95th percentile) are at
//! least 6 dB over the quietest (10th), the threshold is a quarter of the
//! way up from the quiet level in dB (at least 3 dB over it); otherwise
//! nothing in the capture was off and it is one continuous burst. Gaps of up to [`MERGE`] blocks are bridged (the
//! envelope of an OFDM burst dips, it does not stop), runs shorter than
//! [`MIN_LEN`] are dropped.

use droneid::fft::{N as NFFT, fft};
use droneid::math::C16;

use crate::Scratch;

/// Samples per envelope block (1.6 µs at 20 MS/s).
pub const BLOCK: usize = 32;
pub const MAX_BLOCKS: usize = crate::MAX_IN / BLOCK;
/// Bursts kept (the rest are counted).
pub const MAX_BURSTS: usize = 8;
/// Off-gaps bridged, blocks (9.6 µs).
pub const MERGE: usize = 6;
/// Shortest burst kept, blocks (4.8 µs).
pub const MIN_LEN: usize = 3;
/// Bursts whose own centre is measured (one FFT each, from their middle).
pub const CENTRES: usize = 4;
/// On/off contrast below which the capture counts as continuous (dB x10).
pub const CONTRAST_X10: i32 = 60;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Burst {
    /// First block and length, in [`BLOCK`]s.
    pub start: u16,
    pub len: u16,
    /// The capture began (ended) inside it, so its length is a lower bound.
    pub head_cut: bool,
    pub tail_cut: bool,
    /// Its own power centroid relative to the LO (kHz), and its own -10 dB
    /// width (kHz, from one FFT so coarse), if measured.
    pub centre_khz: Option<i32>,
    pub width_khz: u32,
}

impl Burst {
    pub const fn len_us(&self) -> u32 {
        blocks_us(self.len as u32)
    }
    pub const fn complete(&self) -> bool {
        !self.head_cut && !self.tail_cut
    }
    /// First sample.
    pub const fn start_sample(&self) -> usize {
        self.start as usize * BLOCK
    }
}

/// Microseconds in `b` blocks.
pub const fn blocks_us(b: u32) -> u32 {
    (b as u64 * BLOCK as u64 * 1_000_000 / crate::FS as u64) as u32
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Bursts {
    /// Blocks in the capture and blocks on.
    pub blocks: u16,
    pub on_blocks: u16,
    /// Bursts found (may exceed `list.len()`).
    pub found: u8,
    pub list: heapless_list::List,
    /// No off part was seen: the capture is one burst.
    pub continuous: bool,
    /// 95th over 10th percentile block power, tenths of a dB.
    pub contrast_x10: i16,
    /// The on/off decision per block (after bridging and dropping).
    pub mask: [u32; MAX_BLOCKS / 32],
}

impl Bursts {
    pub fn on(&self, block: usize) -> bool {
        block < MAX_BLOCKS && self.mask[block / 32] & (1 << (block % 32)) != 0
    }
    pub fn on_pct(&self) -> u8 {
        if self.blocks == 0 { 0 } else { (self.on_blocks as u32 * 100 / self.blocks as u32) as u8 }
    }
    pub fn on_samples(&self) -> usize {
        self.on_blocks as usize * BLOCK
    }
    pub fn iter(&self) -> impl Iterator<Item = &Burst> {
        self.list.as_slice().iter()
    }
    /// Longest burst, µs.
    pub fn longest_us(&self) -> u32 {
        self.iter().map(|b| b.len_us()).max().unwrap_or(0)
    }
    /// The first burst whose start the capture saw (its symbol timing is
    /// known to a block).
    pub fn first_with_head(&self) -> Option<&Burst> {
        self.iter().find(|b| !b.head_cut)
    }
}

/// A tiny fixed-capacity list (no `heapless` dependency for one array).
pub mod heapless_list {
    use super::{Burst, MAX_BURSTS};

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct List {
        items: [Burst; MAX_BURSTS],
        n: u8,
    }

    impl List {
        pub fn push(&mut self, b: Burst) -> bool {
            if (self.n as usize) < MAX_BURSTS {
                self.items[self.n as usize] = b;
                self.n += 1;
                true
            } else {
                false
            }
        }
        pub fn as_slice(&self) -> &[Burst] {
            &self.items[..self.n as usize]
        }
        pub fn as_mut_slice(&mut self) -> &mut [Burst] {
            &mut self.items[..self.n as usize]
        }
        pub fn len(&self) -> usize {
            self.n as usize
        }
        pub fn is_empty(&self) -> bool {
            self.n == 0
        }
    }
}

/// Envelope, threshold, runs. Fills `s.env`.
pub fn find(x: &[C16], s: &mut Scratch) -> Bursts {
    let nb = (x.len() / BLOCK).min(MAX_BLOCKS);
    let mut out = Bursts { blocks: nb as u16, ..Bursts::default() };
    if nb == 0 {
        return out;
    }
    for (b, e) in s.env[..nb].iter_mut().enumerate() {
        let sum: u64 = x[b * BLOCK..(b + 1) * BLOCK].iter().map(|v| v.norm2() as u64).sum();
        *e = (sum / BLOCK as u64).min(u32::MAX as u64) as u32;
    }
    s.tmp[..nb].copy_from_slice(&s.env[..nb]);
    s.tmp[..nb].sort_unstable();
    let p10 = s.tmp[nb / 10].max(1) as u64;
    let p95 = s.tmp[nb * 95 / 100].max(1) as u64;
    let contrast = crate::db_x10(p95) - crate::db_x10(p10);
    out.contrast_x10 = contrast.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
    out.continuous = contrast < CONTRAST_X10;
    // A quarter of the way up in dB (a narrow burst's envelope dips deep),
    // but at least 3 dB over the quiet part.
    let quarter = droneid::math::isqrt64(p10 * droneid::math::isqrt64(p10 * p95));
    let thr = if out.continuous { 0 } else { quarter.max(2 * p10).min(u32::MAX as u64) as u32 };

    // Raw on/off into the mask.
    for b in 0..nb {
        if s.env[b] >= thr {
            out.mask[b / 32] |= 1 << (b % 32);
        }
    }
    // Bridge short gaps between on runs.
    let mut b = 0;
    let mut last_on: Option<usize> = None;
    while b < nb {
        if out.on(b) {
            if let Some(l) = last_on
                && b - l - 1 > 0
                && b - l - 1 <= MERGE
            {
                for g in l + 1..b {
                    out.mask[g / 32] |= 1 << (g % 32);
                }
            }
            last_on = Some(b);
        }
        b += 1;
    }
    // Runs: drop the short ones, record the rest.
    let mut b = 0;
    while b < nb {
        if !out.on(b) {
            b += 1;
            continue;
        }
        let start = b;
        while b < nb && out.on(b) {
            b += 1;
        }
        let len = b - start;
        if len < MIN_LEN {
            for g in start..b {
                out.mask[g / 32] &= !(1 << (g % 32));
            }
            continue;
        }
        out.on_blocks += len as u16;
        out.found = out.found.saturating_add(1);
        out.list.push(Burst {
            start: start as u16,
            len: len as u16,
            // Within a merge gap of either end counts as cut: a narrow
            // burst's envelope dips for a block or two (a 1.7 MHz burst
            // running past the capture's end read as ending 3 µs early).
            head_cut: start <= MERGE,
            tail_cut: b + MERGE >= nb,
            centre_khz: None,
            width_khz: 0,
        });
    }
    out
}

/// Each of the first [`CENTRES`] bursts at least one FFT long: the power
/// centroid and -10 dB width of one 1024-point FFT from its middle, over
/// the 78 kHz groups within 10 dB of its plateau.
pub fn centres(x: &[C16], bursts: &mut Bursts, s: &mut Scratch) {
    for b in bursts.list.as_mut_slice().iter_mut().take(CENTRES) {
        let len = b.len as usize * BLOCK;
        if len < NFFT {
            continue;
        }
        let at = b.start_sample() + (len - NFFT) / 2;
        if at + NFFT > x.len() {
            continue;
        }
        s.fft.copy_from_slice(&x[at..at + NFFT]);
        fft(&mut s.fft, false);
        let g = crate::spectrum::group_fft(&s.fft, &mut s.psd);
        // Plateau: the third-strongest group (one FFT's groups scatter by a
        // few dB; the strongest is an outlier more often than not).
        let mut top = [0u64; 3];
        for k in crate::spectrum::USE {
            let v = g[k];
            if v > top[2] {
                top[2] = v;
                top.sort_unstable_by(|a, b| b.cmp(a));
            }
        }
        let thr = top[2] / 10;
        b.width_khz = crate::spectrum::USE.filter(|&k| g[k] >= thr).count() as u32 * crate::spectrum::GROUP_HZ / 1000;
        let (mut w, mut wf) = (0u64, 0i64);
        for k in crate::spectrum::USE {
            if g[k] >= thr && g[k] > 0 {
                let p = g[k] >> 8;
                w += p;
                wf += p as i64 * crate::spectrum::group_hz(k as i64 * 256) / 1000;
            }
        }
        if w > 0 {
            b.centre_khz = Some((wf / w as i64) as i32);
        }
    }
}
