//! Path B samples: one PARL_IO byte per complex sample, a 4-bit window of
//! the 10-bit ADC code for I (high nibble) and Q (low nibble).
//!
//! # Which debug-bus bits
//!
//! `MODEM_DIAG[9:0]` carries Q and `MODEM_DIAG[19:10]` carries I, both 10-bit
//! two's complement, the same words the dump engine writes (esp-sdr-zephyr
//! `doc/hardware-quirks.md`, "Gapless receive through the modem diagnostic
//! bus"; C5VRX `main/rf.c` routes Q from DIAG 4,5,6,9 and I from 14,15,16,19,
//! "bit-exact against the RF dump"). PARL_IO data line `k` takes one of them:
//! lines 0..3 the Q nibble (LSB first, sign last), lines 4..7 the I nibble.
//! With LSB-first packing that puts Q in bits 3:0 and I in bits 7:4 of each
//! byte (both sources agree).
//!
//! # The windows
//!
//! A window keeps the ADC's sign bit (bit 9) as the nibble's sign and three
//! magnitude bits `lsb+2..lsb`:
//!
//! - `lsb = 6` (**clip**): bits 9,8,7,6, the top four bits. A plain
//!   arithmetic shift, `code >> 6`; it only saturates where the ADC itself
//!   does. The decoder research (RESEARCH.md, "Findings from building the
//!   decoder") found this window decodes from 2 dB to beyond 54 dB
//!   per-carrier SNR, so it is the default.
//! - `lsb = 4` (**fold**, C5VRX's "ultrafine" lane): bits 9,6,5,4. Sixteen
//!   codes per step, 12 dB finer, but magnitudes past 127 codes fold back
//!   modulo 8 steps while the sign stays right (C5VRX `test/adc_fold.h`:
//!   "+288 codes reads +32"). Needs gain held within about 16 dB.
//! - `lsb = 5`: in between (C5VRX's "fine" lane).
//!
//! In every window the nibble reads as 4-bit two's complement, so unpacking
//! never depends on the window; only the meaning of one step does.

use droneid::math::C16;

/// A 4-bit window of the 10-bit ADC code: the sign bit plus three bits from
/// `lsb` up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    lsb: u8,
}

impl Window {
    /// Top four bits; saturates only at the ADC's own rails. Default.
    pub const CLIP: Window = Window { lsb: 6 };
    /// C5VRX's "fine" lane, bits 9,7,6,5.
    pub const FOLD5: Window = Window { lsb: 5 };
    /// C5VRX's "ultrafine" lane, bits 9,6,5,4.
    pub const FOLD: Window = Window { lsb: 4 };

    pub const fn new(lsb: u8) -> Option<Window> {
        if lsb >= 4 && lsb <= 6 { Some(Window { lsb }) } else { None }
    }

    pub const fn lsb(self) -> u8 {
        self.lsb
    }

    pub fn parse(s: &str) -> Option<Window> {
        Some(match s {
            "clip" | "6" => Self::CLIP,
            "fold5" | "fine" | "5" => Self::FOLD5,
            "fold" | "ultrafine" | "4" => Self::FOLD,
            _ => return None,
        })
    }

    pub const fn name(self) -> &'static str {
        match self.lsb {
            6 => "clip",
            5 => "fold5",
            _ => "fold",
        }
    }

    /// The `MODEM_DIAG` bit that PARL_IO data line `k` (0..8) must carry.
    /// Add `MODEM_DIAG0_IDX` (106) for the GPIO-matrix output signal.
    pub const fn diag_lanes(self) -> [u8; 8] {
        let l = self.lsb;
        [l, l + 1, l + 2, 9, 10 + l, 11 + l, 12 + l, 19]
    }

    /// The nibble this window reads for a 10-bit ADC code (the model of the
    /// hardware, for tests and for converting Path A captures).
    pub const fn quantise(self, code: i16) -> i8 {
        let c = if code > 511 {
            511
        } else if code < -512 {
            -512
        } else {
            code
        };
        let mag = ((((c as u16) & 0x3ff) >> self.lsb) & 7) as i8;
        if c < 0 { mag - 8 } else { mag }
    }

    /// Largest ADC magnitude this window shows without folding (codes).
    /// `CLIP` never folds; this is then the ADC's full scale.
    pub const fn span(self) -> i16 {
        8 << self.lsb
    }
}

/// A 4-bit two's complement nibble (`0..16`) as `-8..=7`.
#[inline(always)]
pub const fn nibble(n: u8) -> i8 {
    (((n & 15) << 4) as i8) >> 4
}

/// `(i, q)` of one PARL_IO byte.
#[inline(always)]
pub const fn unpack(b: u8) -> (i8, i8) {
    (nibble(b >> 4), nibble(b))
}

/// The byte PARL_IO would capture for `(i, q)` nibbles (tests, synthesis).
pub const fn pack(i: i8, q: i8) -> u8 {
    (((i as u8) & 15) << 4) | ((q as u8) & 15)
}

/// A nibble as a 16-bit level at the centre of its quantiser cell:
/// `(2n + 1) << 10`, so `-8..=7` maps to `-15360..=15360`. Taking the cell
/// centre removes the half-step bias that bit slicing (a floor) leaves;
/// the headroom keeps later DC removal and filtering from saturating.
#[inline(always)]
pub const fn level(n: i8) -> i16 {
    ((2 * n as i16) + 1) << 10
}

/// One byte as a decoder sample (cell centres, no DC removal).
#[inline(always)]
pub const fn to_c16(b: u8) -> C16 {
    let (i, q) = unpack(b);
    C16 { re: level(i), im: level(q) }
}

/// Instantaneous power of one byte in half-step units:
/// `(2i+1)^2 + (2q+1)^2`, 2..=450. The energy trigger sums these.
pub const POWER: [u16; 256] = {
    let mut t = [0u16; 256];
    let mut b = 0;
    while b < 256 {
        let (i, q) = unpack(b as u8);
        let (a, c) = (2 * i as i32 + 1, 2 * q as i32 + 1);
        t[b] = (a * a + c * c) as u16;
        b += 1;
    }
    t
};

/// Sum of [`POWER`] over `bytes`.
#[inline]
pub fn power_sum(bytes: &[u8]) -> u32 {
    let mut s = 0u32;
    for &b in bytes {
        s += POWER[b as usize] as u32;
    }
    s
}

/// [`POWER`] of a silent input: both axes at the cell just above or below
/// zero. Mean power near this means the window sees no noise at all (gain
/// far too low for the window).
pub const POWER_MIN: u16 = 2;

/// Full-scale sine on both axes in [`POWER`] units (`2 * 15^2 / 2`).
pub const POWER_FULL_SCALE: u32 = 225;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nibble_is_twos_complement() {
        assert_eq!(nibble(0), 0);
        assert_eq!(nibble(7), 7);
        assert_eq!(nibble(8), -8);
        assert_eq!(nibble(15), -1);
        for i in -8..8i8 {
            for q in -8..8i8 {
                assert_eq!(unpack(pack(i, q)), (i, q));
            }
        }
    }

    #[test]
    fn q_low_nibble_i_high_nibble() {
        // Q on data lines 0..3 (bits 3:0), I on lines 4..7 (bits 7:4).
        assert_eq!(unpack(0x1f), (1, -1));
        assert_eq!(unpack(0xf1), (-1, 1));
    }

    #[test]
    fn clip_window_is_arithmetic_shift() {
        for code in -512..512i16 {
            assert_eq!(Window::CLIP.quantise(code) as i16, code >> 6, "code {code}");
        }
        // ...and saturates with the ADC.
        assert_eq!(Window::CLIP.quantise(2000), 7);
        assert_eq!(Window::CLIP.quantise(-2000), -8);
    }

    #[test]
    fn fold_window_matches_c5vrx_model() {
        // C5VRX test/adc_fold.h: sign from bit 9, magnitude bits 6..4 modulo 8.
        let fold = |code: i16| -> i8 {
            let mag = (((code as u16 & 0x3ff) >> 4) & 7) as i8;
            if code < 0 { mag - 8 } else { mag }
        };
        for code in -512..512i16 {
            assert_eq!(Window::FOLD.quantise(code), fold(code), "code {code}");
        }
        // Inside +-127 codes it is a plain shift by 4...
        for code in -128..128i16 {
            assert_eq!(Window::FOLD.quantise(code) as i16, code >> 4);
        }
        // ...beyond, it folds: "+288 codes reads +32" means 2 steps (32 codes).
        assert_eq!(Window::FOLD.quantise(288), 2);
        assert_eq!(Window::FOLD.span(), 128);
    }

    #[test]
    fn lanes_carry_sign_last() {
        assert_eq!(Window::CLIP.diag_lanes(), [6, 7, 8, 9, 16, 17, 18, 19]);
        // C5VRX main/rf.c, ultrafine: Q on DIAG[4,5,6,9], I on DIAG[14,15,16,19].
        assert_eq!(Window::FOLD.diag_lanes(), [4, 5, 6, 9, 14, 15, 16, 19]);
        assert_eq!(Window::FOLD5.diag_lanes(), [5, 6, 7, 9, 15, 16, 17, 19]);
        assert!(Window::new(3).is_none() && Window::new(7).is_none());
        for w in [Window::CLIP, Window::FOLD5, Window::FOLD] {
            assert_eq!(Window::parse(w.name()), Some(w));
        }
    }

    #[test]
    fn levels_are_cell_centres() {
        assert_eq!(level(0), 1024);
        assert_eq!(level(-1), -1024);
        assert_eq!(level(7), 15360);
        assert_eq!(level(-8), -15360);
        // Symmetric: a zero-mean input that floors stays zero-mean.
        let s: i32 = (-8..8).map(|n| level(n) as i32).sum();
        assert_eq!(s, 0);
    }

    #[test]
    fn power_table() {
        assert_eq!(POWER[pack(0, 0) as usize], 2);
        assert_eq!(POWER[pack(-1, -1) as usize], 2);
        assert_eq!(POWER[pack(-8, -8) as usize], 450);
        assert_eq!(POWER[pack(7, -8) as usize], 450);
        assert_eq!(power_sum(&[pack(1, 0), pack(0, 1)]), 10 + 10);
    }
}
