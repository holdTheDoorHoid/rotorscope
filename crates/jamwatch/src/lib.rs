//! Jamming detection for the raw-capture ESP32-C5 (`firmware/node-sdr`):
//! pure logic, no hardware, host-tested (docs/JAMMING.md has the why).
//!
//! The node hops across the 2.4 and 5.8 GHz drone bands. At the end of every
//! dwell it already holds two things this crate turns into a verdict:
//!
//! 1. **Every trigger probe's power** (256 of every 2048 bytes of the 40 MS/s
//!    4-bit stream, one per 51.2 µs): [`probes::ProbeStats`] keeps their
//!    histogram (`fpvscan::Meter`) and the on/off runs above a "loud" level.
//!    The 10th percentile is the dwell's floor; on a hop with only bursty
//!    traffic (Wi-Fi, DroneID, controllers) it is the receiver's own noise,
//!    and only something that never pauses (a jammer, or one continuous
//!    link) can lift it.
//! 2. **The newest ~1.4 ms of the ring**: [`spectrum::analyse`] runs 128
//!    64-point FFTs over it (one contiguous block of 64 frames for fast
//!    sweeps, 64 frames spread over the rest for slow ones) and measures how
//!    much of the receiver's passband is occupied, whether one bin holds a
//!    persistent tone, whether a carrier moves monotonically (a sweep, with
//!    its rate in MHz/s), what the loud frames look like, and how many
//!    samples sit on the 4-bit rails (front-end overload).
//!
//! [`watch::Watch`] keeps a slowly learnt noise floor per hop (robust to
//! booting into a jammed band: it never accepts a floor far above the
//! receiver's expected one, the band's quietest hop or the other band's),
//! judges each dwell, and across hops and bands decides what to report:
//!
//! - **Broadband** (barrage): the floor of most hops of a band raised by
//!   [`watch::RAISE_X10`] or more, flat across the passband, spanning
//!   [`watch::SPAN_MIN_KHZ`] or more, with no OFDM cyclic prefix (checked by
//!   `fpvscan`, see [`link_from_analysis`]). One drone link raises one or two
//!   neighbouring hops and carries a cyclic prefix; a barrage jammer raises
//!   everything and is noise.
//! - **Tone**, **Sweep**, **Pulsed**: from the spectrum and the probe runs.
//! - **Blinded**: whatever the cause, the floor of most hops of a band is up
//!   so far ([`watch::BLIND_X10`]) that a typical link would be buried;
//!   `detail` = estimated percent of free-space detection range left.
//!
//! Units: frequencies in 100 kHz (as `link`), levels in tenths of a dB in
//! probe units (`fpvscan::db_x10` of a probe's power sum), time in ms.
#![cfg_attr(not(test), no_std)]

pub mod probes;
pub mod spectrum;
pub mod watch;

pub use fpvscan::band::Band;
pub use probes::ProbeStats;
pub use spectrum::{Features, Scratch, analyse};
pub use watch::{Assess, HopVerdict, Kind, Report, Watch};

/// Whether an `fpvscan` analysis of a capture shows a structured link rather
/// than noise: an OFDM cyclic prefix (Wi-Fi, half-clock Wi-Fi, LTE-style 15
/// kHz: DJI, Walksnail, Autel...) above its noise-dependent threshold, or an
/// analog video line rate. `dsss`: `fpvscan::band::extra`'s 802.11b Barker
/// metric (2.4 GHz; 0 elsewhere). Jamming noise has none of these, and a
/// link under a strong jammer loses them (the metric scales with the link's
/// share of the power), which is the right answer: it is drowned.
pub fn link_from_analysis(a: &fpvscan::Analysis, dsss: i16) -> bool {
    use fpvscan::classify as c;
    let thr = c::cp_threshold(a);
    a.cp_lte >= thr
        || a.cp_wifi >= thr
        || a.cp_half >= thr
        || (a.line >= c::LINE_MIN && c::LINE_HZ.contains(&a.line_hz))
        || dsss >= fpvscan::dsss::DSSS_MIN
}

/// Percent of free-space detection range left when the noise floor is up
/// `rise_x10` tenths of a dB: `100 * 10^(-rise/20)` (received power falls
/// with the square of distance, so every 6 dB of extra noise halves the
/// range at which a given link still clears the same SNR). 100 at or below
/// 0 dB, 1 at 40 dB and above.
pub fn range_left_pct(rise_x10: i32) -> u8 {
    // 100 * 10^(-d/20) for d = 0..=40 dB, rounded.
    const T: [u8; 41] = [
        100, 89, 79, 71, 63, 56, 50, 45, 40, 35, 32, 28, 25, 22, 20, 18, 16, 14, 13, 11, 10, 9, 8, 7, 6, 6, 5, 4, 4, 4,
        3, 3, 3, 2, 2, 2, 2, 1, 1, 1, 1,
    ];
    if rise_x10 <= 0 {
        return 100;
    }
    let d = (rise_x10 / 10) as usize;
    if d >= 40 {
        return 1;
    }
    // Linear between whole dB.
    let (a, b) = (T[d] as i32, T[d + 1] as i32);
    ((a * 10 + (b - a) * (rise_x10 % 10) + 5) / 10) as u8
}

#[cfg(test)]
mod tests;
