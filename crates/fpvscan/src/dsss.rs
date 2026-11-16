//! 802.11b DSSS (1 and 2 Mb/s Barker) recogniser, for 2.4 GHz.
//!
//! On 2.4 GHz a lot of Wi-Fi is not OFDM: beacons and probe traffic are
//! often sent at 1 Mb/s DSSS, and every 802.11b frame (CCK included) starts
//! with a 96 or 192 µs preamble and header spread by the 11-chip Barker
//! code, one symbol per microsecond. DSSS has no cyclic prefix, so the OFDM
//! tests in [`crate::analyse`] do not see it; this one does.
//!
//! At 20 MS/s a symbol is exactly 20 samples. The capture is filtered with
//! the Barker code sampled at 20 MS/s and `|output|^2` is folded by sample
//! index modulo 20: Barker-spread symbols pile their energy into one or two
//! of the 20 phases, everything else (noise, OFDM, FM video, Bluetooth,
//! carriers) spreads evenly. The metric is how far the loudest phase stands
//! above the mean, x1000. Clock error is no issue: 25 ppm is 0.2 samples
//! over the [`MAX_IN`] samples used.
//!
//! The firmware freezes its window about 100 µs before the trigger's rise,
//! so a DSSS frame's preamble is near the start of the capture: only the
//! first [`MAX_IN`] samples are used (about 3 ms of C5 time).

use droneid::math::C16;

/// Samples used (410 µs).
pub const MAX_IN: usize = 8192;
/// Samples per 1 µs Barker symbol at 20 MS/s.
pub const SYM: usize = 20;
/// The 802.11 Barker sequence.
pub const BARKER: [i8; 11] = [1, -1, 1, 1, -1, 1, 1, 1, -1, -1, -1];

/// Accept DSSS above this (x1000). Noise and dense OFDM stay near 100
/// (twenty phases of ~400 symbols each); sparse OFDM Wi-Fi, whose energy
/// sits in a few frames, reaches about 260; 802.11b frames, 1 Mb/s or CCK,
/// give 1400 and more (tests, synthetic).
pub const DSSS_MIN: i16 = 600;

const fn template() -> [i8; SYM] {
    let mut t = [0i8; SYM];
    let mut k = 0;
    while k < SYM {
        // Chip k * 11 / 20 of the symbol.
        t[k] = BARKER[k * 11 / SYM];
        k += 1;
    }
    t
}
const TMPL: [i8; SYM] = template();

/// The Barker fold metric of `x` (x1000): `(loudest phase - mean) / mean`.
pub fn barker(x: &[C16]) -> i16 {
    let x = &x[..x.len().min(MAX_IN)];
    if x.len() < 64 * SYM {
        return 0;
    }
    let mut fold = [0u64; SYM];
    for n in 0..x.len() - SYM {
        let (mut re, mut im) = (0i32, 0i32);
        for (k, &t) in TMPL.iter().enumerate() {
            let v = x[n + k];
            if t > 0 {
                re += v.re as i32;
                im += v.im as i32;
            } else {
                re -= v.re as i32;
                im -= v.im as i32;
            }
        }
        let p = (re as i64 * re as i64 + im as i64 * im as i64) as u64;
        fold[n % SYM] = fold[n % SYM].saturating_add(p >> 8);
    }
    let sum: u64 = fold.iter().sum();
    let peak = *fold.iter().max().unwrap_or(&0);
    if sum == 0 {
        return 0;
    }
    let mean = sum / SYM as u64;
    let m = (peak - mean).saturating_mul(1000) / mean.max(1);
    m.min(i16::MAX as u64) as i16
}
