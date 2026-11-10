//! Path A samples: the dump engine's 32-bit words.
//!
//! ```text
//!  31     28 27          20 19           10 9             0
//! | unused  | gain index    | I (10-bit)    | Q (10-bit)    |
//! ```
//!
//! Field positions: ESPARGOS esp-sdr `docs/how-it-works.md` ("What a
//! captured word looks like", gain index in 27:20) and esp-sdr-zephyr
//! `include/esp_sdr/esp_sdr_rx.h`. The two disagree on which 10-bit field is
//! I: esp-sdr's diagram puts I low; esp-sdr-zephyr says "Receive words carry
//! Q in the low field: with I low, a tone below the LO showed up above it
//! (checked by detuning the receiver against a known carrier)", and C5VRX's
//! debug-bus lanes (Q from DIAG 0..9) agree with it. We follow the measured
//! claim (Q low) and keep a swap switch for the bring-up tone test
//! (BRINGUP.md): a swapped capture mirrors the spectrum, and DroneID then
//! fails to synchronise.

use droneid::math::C16;

/// Sign-extend the low 10 bits.
#[inline(always)]
pub const fn sext10(v: u32) -> i16 {
    (((v << 22) as i32) >> 22) as i16
}

/// Q field, -512..=511.
#[inline(always)]
pub const fn q(w: u32) -> i16 {
    sext10(w)
}

/// I field, -512..=511.
#[inline(always)]
pub const fn i(w: u32) -> i16 {
    sext10(w >> 10)
}

/// Gain index the receiver had for this sample.
#[inline(always)]
pub const fn gain(w: u32) -> u8 {
    (w >> 20) as u8
}

/// A 10-bit code at the centre of its cell, scaled to 16 bits:
/// `(2c + 1) << 5`, so -512..=511 maps to -32736..=32736.
#[inline(always)]
pub const fn level(c: i16) -> i16 {
    ((2 * c) + 1) << 5
}

/// One word as a decoder sample. `swap_iq` takes I from the low field.
#[inline(always)]
pub const fn to_c16(w: u32, swap_iq: bool) -> C16 {
    let (a, b) = (i(w), q(w));
    if swap_iq { C16 { re: level(b), im: level(a) } } else { C16 { re: level(a), im: level(b) } }
}

/// Level statistics of a snapshot, for choosing gain and window.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stats {
    pub n: u32,
    /// Mean of each axis (ADC codes): DC offset.
    pub dc_i: i16,
    pub dc_q: i16,
    /// RMS about the mean, codes x 10.
    pub rms_i_x10: u32,
    pub rms_q_x10: u32,
    /// Largest |code - DC| on either axis.
    pub peak: u16,
    /// Samples at either ADC rail (-512 or 511) on either axis.
    pub railed: u32,
    /// Samples whose |code| reaches 128 / 256 (where the fold / fold5
    /// windows wrap).
    pub over128: u32,
    pub over256: u32,
    /// Gain index of the first and last words (they should match a forced gain).
    pub gain_first: u8,
    pub gain_last: u8,
}

/// Integer square root (floor).
pub fn isqrt(v: u64) -> u64 {
    if v < 2 {
        return v;
    }
    let mut x = 1u64 << ((64 - v.leading_zeros()).div_ceil(2));
    loop {
        let y = (x + v / x) / 2;
        if y >= x {
            return x;
        }
        x = y;
    }
}

pub fn stats(words: &[u32]) -> Stats {
    let mut s = Stats { n: words.len() as u32, ..Stats::default() };
    if words.is_empty() {
        return s;
    }
    let (mut si, mut sq, mut si2, mut sq2) = (0i64, 0i64, 0i64, 0i64);
    for &w in words {
        let (a, b) = (i(w) as i64, q(w) as i64);
        si += a;
        sq += b;
        si2 += a * a;
        sq2 += b * b;
        let rail = |c: i64| c == -512 || c == 511;
        if rail(a) || rail(b) {
            s.railed += 1;
        }
        let m = a.abs().max(b.abs());
        if m >= 128 {
            s.over128 += 1;
        }
        if m >= 256 {
            s.over256 += 1;
        }
    }
    let n = words.len() as i64;
    s.dc_i = (si / n) as i16;
    s.dc_q = (sq / n) as i16;
    // Variance x 100 = (sum x^2 - (sum x)^2 / n) * 100 / n.
    let var100 = |s2: i64, s1: i64| ((s2 - s1 * s1 / n).max(0) as u64) * 100 / n as u64;
    s.rms_i_x10 = isqrt(var100(si2, si)) as u32;
    s.rms_q_x10 = isqrt(var100(sq2, sq)) as u32;
    for &w in words {
        let d = (i(w) - s.dc_i).unsigned_abs().max((q(w) - s.dc_q).unsigned_abs());
        s.peak = s.peak.max(d);
    }
    s.gain_first = gain(words[0]);
    s.gain_last = gain(words[words.len() - 1]);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(i: i16, q: i16, g: u8) -> u32 {
        ((g as u32) << 20) | (((i as u32) & 0x3ff) << 10) | ((q as u32) & 0x3ff)
    }

    #[test]
    fn fields() {
        let w = word(-512, 511, 77);
        assert_eq!(i(w), -512);
        assert_eq!(q(w), 511);
        assert_eq!(gain(w), 77);
        let w = word(3, -1, 0);
        assert_eq!((i(w), q(w)), (3, -1));
    }

    #[test]
    fn levels_fit_and_centre() {
        assert_eq!(level(511), 32736);
        assert_eq!(level(-512), -32736);
        assert_eq!(level(0), 32);
        assert_eq!(level(-1), -32);
        let c = to_c16(word(5, -7, 0), false);
        assert_eq!((c.re, c.im), (level(5), level(-7)));
        let c = to_c16(word(5, -7, 0), true);
        assert_eq!((c.re, c.im), (level(-7), level(5)));
    }

    #[test]
    fn stats_of_a_square_wave() {
        // I alternates +-100 around DC 10, Q constant 0, gain 60.
        let v: Vec<u32> = (0..1000).map(|k| word(if k % 2 == 0 { 110 } else { -90 }, 0, 60)).collect();
        let s = stats(&v);
        assert_eq!(s.dc_i, 10);
        assert_eq!(s.dc_q, 0);
        assert_eq!(s.rms_i_x10, 1000);
        assert_eq!(s.rms_q_x10, 0);
        assert_eq!(s.peak, 100);
        assert_eq!(s.over128, 0);
        assert_eq!((s.gain_first, s.gain_last), (60, 60));
        let v = [word(511, 0, 1), word(-300, 0, 2)];
        let s = stats(&v);
        assert_eq!(s.railed, 1);
        assert_eq!(s.over256, 2);
    }

    #[test]
    fn isqrt_exact() {
        for v in [0u64, 1, 2, 3, 4, 15, 16, 17, 99, 100, 1 << 40, u32::MAX as u64] {
            let r = isqrt(v);
            assert!(r * r <= v && (r + 1) * (r + 1) > v, "{v}");
        }
    }
}
