//! Turning a frozen Path B window into decoder input.
//!
//! The decoder needs one contiguous `&[C16]`. A whole DroneID burst plus
//! margin is about 0.8 ms; at 40 MS/s that is 32,768 samples, 128 KiB as
//! `C16`, more than the chip can spare next to the receiver. Halving the rate
//! first (the receiver accepts 20 MS/s) makes it 64 KiB, exactly the free
//! half of the capture bank (`firmware/node-sdr`). The burst is about 9 MHz
//! wide and sits within +-5 MHz of the LO (the LO is tuned to the whole MHz
//! below a DroneID centre, which puts the burst at +0.5 MHz), so a short
//! half-band filter is enough: the analog low-pass is already set to about
//! 20 MHz full width, so little lies beyond +-10 MHz to fold in.

use droneid::math::C16;

use crate::iq::{nibble, unpack};

/// Half-band taps (Q9) at odd offsets 1, 3, 5 from the centre; the centre
/// tap is 256 (one half). DC gain 512/512.
const HB: [i32; 3] = [150, -25, 3];

/// Input samples needed to the right of the last output's centre.
pub const HB_REACH: usize = 5;

/// Decimate nibble bytes 2:1 into `out`: `out[m]` is centred on input
/// `2m + HB_REACH`, so `src` must provide `2 * out.len() + 2 * HB_REACH`
/// bytes (`src(k)` returns byte `k`). Output is in cell-centre units
/// (`(2n+1) << 10` at DC, see [`crate::iq::level`]).
pub fn decimate_nibbles(src: impl Fn(usize) -> u8, out: &mut [C16]) {
    // Work in 2n+1 units so the truncation bias vanishes; scale at the end.
    let v = |k: usize| -> (i32, i32) {
        let (i, q) = unpack(src(k));
        (2 * i as i32 + 1, 2 * q as i32 + 1)
    };
    for (m, o) in out.iter_mut().enumerate() {
        let c = 2 * m + HB_REACH;
        let (ci, cq) = v(c);
        let (mut ai, mut aq) = (256 * ci, 256 * cq);
        for (t, &h) in HB.iter().enumerate() {
            let d = 2 * t + 1;
            let (li, lq) = v(c - d);
            let (ri, rq) = v(c + d);
            ai += h * (li + ri);
            aq += h * (lq + rq);
        }
        // a / 512 is in (2n+1) units; (2n+1) << 10 = a << 1.
        *o = C16 { re: sat(ai << 1), im: sat(aq << 1) };
    }
}

/// Undecimated conversion (for dumps and tests): every byte as a sample.
pub fn nibbles_to_c16(src: impl Fn(usize) -> u8, out: &mut [C16]) {
    for (k, o) in out.iter_mut().enumerate() {
        let b = src(k);
        *o = C16 { re: crate::iq::level(nibble(b >> 4)), im: crate::iq::level(nibble(b)) };
    }
}

fn sat(v: i32) -> i16 {
    v.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iq::{Window, pack};

    /// A complex tone at `f` (Hz, at 40 MS/s), amplitude `a` ADC codes,
    /// quantised through `w`.
    fn tone(f: f64, a: f64, n: usize, w: Window) -> Vec<u8> {
        (0..n)
            .map(|k| {
                let ph = 2.0 * core::f64::consts::PI * f * k as f64 / 40e6;
                let i = (a * ph.cos()).floor() as i16;
                let q = (a * ph.sin()).floor() as i16;
                pack(w.quantise(i), w.quantise(q))
            })
            .collect()
    }

    fn power(x: &[C16]) -> f64 {
        x.iter().map(|c| (c.re as f64).powi(2) + (c.im as f64).powi(2)).sum::<f64>() / x.len() as f64
    }

    fn decim(b: &[u8]) -> Vec<C16> {
        let n = (b.len() - 2 * HB_REACH) / 2;
        let mut out = vec![C16::default(); n];
        decimate_nibbles(|k| b[k], &mut out);
        out
    }

    #[test]
    fn passband_kept_stopband_cut() {
        let a = 400.0; // codes; clip window step is 64
        let pass = power(&decim(&tone(1.0e6, a, 4096, Window::CLIP)));
        let edge = power(&decim(&tone(5.0e6, a, 4096, Window::CLIP)));
        let stop = power(&decim(&tone(15.0e6, a, 4096, Window::CLIP)));
        let db = |x: f64| 10.0 * (x / pass).log10();
        assert!(db(edge).abs() < 0.5, "5 MHz: {:.2} dB", db(edge));
        assert!(db(stop) < -20.0, "15 MHz alias: {:.1} dB", db(stop));
    }

    #[test]
    fn dc_and_scale() {
        // A constant nibble pair stays constant, at its cell centre.
        let b = vec![pack(3, -2); 64];
        for c in decim(&b) {
            assert_eq!((c.re, c.im), (7 << 10, -3 << 10));
        }
        // Full scale does not wrap.
        let b = vec![pack(-8, 7); 64];
        for c in decim(&b) {
            assert_eq!((c.re, c.im), (-15 << 10, 15 << 10));
        }
        // Worst-case overshoot pattern saturates instead of wrapping.
        let b: Vec<u8> = (0..64).map(|k| if (k / 2) % 2 == 0 { pack(7, 7) } else { pack(-8, -8) }).collect();
        for c in decim(&b) {
            assert!(c.re.unsigned_abs() <= 32767);
        }
    }

    #[test]
    fn tone_frequency_survives_decimation() {
        // +2 MHz at 40 MS/s is +2 MHz at 20 MS/s: phase step 2*pi*2/20 per sample.
        let y = decim(&tone(2.0e6, 300.0, 2048, Window::CLIP));
        let mut acc = (0.0f64, 0.0f64);
        for w in y.windows(2) {
            let (a, b) = (w[1], w[0]);
            // a * conj(b)
            acc.0 += a.re as f64 * b.re as f64 + a.im as f64 * b.im as f64;
            acc.1 += a.im as f64 * b.re as f64 - a.re as f64 * b.im as f64;
        }
        let f = acc.1.atan2(acc.0) / (2.0 * core::f64::consts::PI) * 20e6;
        assert!((f - 2.0e6).abs() < 20e3, "{f}");
    }

    #[test]
    fn undecimated_matches_levels() {
        let b = [pack(1, -1), pack(-8, 7)];
        let mut out = [C16::default(); 2];
        nibbles_to_c16(|k| b[k], &mut out);
        assert_eq!((out[0].re, out[0].im), (3 << 10, -1 << 10));
        assert_eq!((out[1].re, out[1].im), (-15 << 10, 15 << 10));
    }
}
