//! Analog FPV / RF scanner DSP for the raw-capture ESP32-C5 (PLAN.md Phase 7
//! option 2 and Phase 11b): detection only, never video.
//!
//! The firmware (`firmware/node-sdr`, role FPV) steps its receiver across the
//! analog FPV channels it can reach ([`channels`]). On each channel it
//!
//! 1. measures the level of every trigger probe against the noise floor
//!    ([`meter`]): how loud the channel is and what fraction of the dwell it
//!    was on (its duty cycle);
//! 2. on a loud channel, analyses one frozen capture of 20 MS/s complex
//!    samples ([`analyse`]): FM-demodulates it and looks for the 15.6/15.7 kHz
//!    line rate of analog video, measures the cyclic-prefix autocorrelation at
//!    the Wi-Fi and LTE-style (15 kHz) symbol lengths, and the occupied width
//!    and flatness of the spectrum;
//! 3. turns both into one [`Kind`] ([`classify`]).
//!
//! Wi-Fi is the thing a naive energy scanner gets wrong on 5.8 GHz. Here it
//! can only become [`Kind::AnalogVideo`] by showing a line-rate periodicity
//! in its FM-demodulated signal, which OFDM does not have, and it is
//! recognised positively by its 3.2 µs (or 12.8 µs, 802.11ax) cyclic prefix.
//!
//! **2.4 GHz** ([`channels24`], [`band`], [`dsss`]): the same scanner walks
//! the 2.4 GHz analog video plans too. There the band is far busier (Wi-Fi
//! channels 1-13, Bluetooth hopping, microwave ovens), so it gets its own
//! noise floor, an 802.11b DSSS recogniser, and inside the ISM band only a
//! confirmed line rate (or a recognised OFDM family) is reported. The
//! firmware uses [`band`], which on 5.8 GHz is exactly the 5.8 GHz path.
//!
//! Everything per sample is integer (`i16` samples, `i32` products, `i64`
//! sums). [`Scratch`] holds all working memory (about 26 KiB): put it in a
//! `static` or PSRAM on the chip.
#![cfg_attr(not(test), no_std)]

pub mod analyse;
pub mod band;
pub mod channels;
pub mod channels24;
pub mod classify;
pub mod dsss;
pub mod meter;

pub use analyse::{Analysis, Scratch, analyse};
pub use classify::{Dwell, Kind, classify};
pub use meter::{FloorTracker, Meter};

/// Capture rate the analysis expects (the firmware's decimated Path B).
pub const FS: u32 = 20_000_000;

/// `log2(v)` in Q8 (256 = one octave), `v > 0`; 0 for `v == 0`. Max error
/// about 0.005 octave (a quadratic correction of the mantissa).
pub fn log2_q8(v: u64) -> i32 {
    if v == 0 {
        return 0;
    }
    let e = 63 - v.leading_zeros() as i32;
    // Next 8 bits after the leading one.
    let f = if e >= 8 { ((v >> (e - 8)) & 0xff) as i32 } else { ((v << (8 - e)) & 0xff) as i32 };
    // log2(1 + f) ~ f + 0.343 f (1 - f), in Q8.
    e * 256 + f + ((f * (256 - f) * 88) >> 16)
}

/// `10 log10(v)` in tenths of a dB.
pub fn db_x10(v: u64) -> i32 {
    // 10 log10(2) / 256 * 65536 = 7706.4
    (log2_q8(v) * 7706) >> 16
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests24;
