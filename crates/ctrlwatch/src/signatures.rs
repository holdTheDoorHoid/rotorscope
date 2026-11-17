//! Energy and timing signatures: what each system looks like to a receiver
//! that cannot (or may not) demodulate it. The classifier in
//! [`crate::energy`] matches measurements against this table.
//!
//! Honesty about sources: rows marked [`Source::Firmware`] come from the
//! open firmware (exact). [`Source::PublicRe`] rows come from public
//! reverse engineering (DIY-Multiprotocol-TX-Module and similar) and are
//! believed right but were not checked against hardware here.
//! [`Source::VendorDocs`] rows use the vendor's advertised packet rates;
//! their channel plans are not public. [`Source::Estimate`] rows are
//! educated guesses and only good for a candidate list.
//!
//! What can be told apart by energy alone, roughly:
//! - **ELRS 2.4 FLRC/FSK** (1000/500 Hz, ~0.4-0.7 ms bursts on a 1 MHz grid
//!   of 80 channels) is distinctive; nothing else common hops that fast on
//!   that grid.
//! - **ELRS 900** (600 kHz grid, 40 channels, 4-40 ms packet interval) vs
//!   **mLRS 900** (600 kHz grid offset by 0.9 MHz, 43 channels, long 91-byte
//!   bursts at 19-50 Hz): burst length separates them.
//! - **SiK** (250 kHz grid in 915-928 MHz, irregular TDM bursts) is
//!   separable from both by grid and band.
//! - **Crossfire vs ELRS 900 vs FrSky R9**: overlapping bands, rates in the
//!   same range, closed channel plans: candidates only.
//! - **Ghost vs Tracer vs ELRS 2.4 LoRa**: likewise candidates only.
//! - **AFHDS 2A** (3.85 ms) and **FrSky D16** (9 ms) have well-known
//!   periods but share 2.4 GHz with Wi-Fi and Bluetooth: timing on a dwell is
//!   needed, not just occupancy.

use crate::System;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Source {
    Firmware,
    PublicRe,
    VendorDocs,
    Estimate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Signature {
    pub system: System,
    pub label: &'static str,
    /// Band the channels lie in, Hz.
    pub band_hz: (u32, u32),
    pub spacing_hz: Option<u32>,
    pub channels: Option<u16>,
    /// Packet intervals the system uses, µs (any one of them).
    pub intervals_us: &'static [u32],
    /// Burst (time on air) range, µs.
    pub burst_us: (u32, u32),
    pub source: Source,
}

#[allow(clippy::too_many_arguments)]
const fn sig(
    system: System,
    label: &'static str,
    band_hz: (u32, u32),
    spacing_hz: Option<u32>,
    channels: Option<u16>,
    intervals_us: &'static [u32],
    burst_us: (u32, u32),
    source: Source,
) -> Signature {
    Signature {
        system,
        label,
        band_hz,
        spacing_hz,
        channels,
        intervals_us,
        burst_us,
        source,
    }
}

pub const TABLE: [Signature; 16] = [
    sig(
        System::ExpressLrs,
        "ELRS 900 LoRa (FCC915)",
        (903_250_000, 927_150_000),
        Some(600_000),
        Some(40),
        &[4_000, 5_000, 6_666, 10_000, 20_000, 40_000],
        (3_000, 30_000),
        Source::Firmware,
    ),
    sig(
        System::ExpressLrs,
        "ELRS 868 LoRa (EU868)",
        (863_000_000, 869_800_000),
        Some(525_000),
        Some(13),
        &[4_000, 5_000, 6_666, 10_000, 20_000, 40_000],
        (3_000, 30_000),
        Source::Firmware,
    ),
    sig(
        System::ExpressLrs,
        "ELRS 900 K1000 FSK",
        (903_250_000, 927_150_000),
        Some(600_000),
        Some(40),
        &[1_000],
        (500, 800),
        Source::Firmware,
    ),
    sig(
        System::ExpressLrs,
        "ELRS 2.4 LoRa",
        (2_400_000_000, 2_480_000_000),
        Some(1_000_000),
        Some(80),
        &[2_000, 3_003, 4_000, 6_666, 10_000, 20_000],
        (1_400, 11_000),
        Source::Firmware,
    ),
    sig(
        System::ExpressLrs,
        "ELRS 2.4 FLRC/FSK (F1000/F500/D500/D250/K1000)",
        (2_400_000_000, 2_480_000_000),
        Some(1_000_000),
        Some(80),
        &[1_000, 2_000],
        (350, 750),
        Source::Firmware,
    ),
    sig(
        System::Mlrs,
        "mLRS 915",
        (902_100_000, 927_900_000),
        Some(600_000),
        Some(43),
        &[20_000, 32_000, 53_000],
        (7_000, 24_000),
        Source::Firmware,
    ),
    sig(
        System::Mlrs,
        "mLRS 868",
        (863_000_000, 868_300_000),
        Some(525_000),
        Some(10),
        &[20_000, 32_000, 53_000],
        (7_000, 24_000),
        Source::Firmware,
    ),
    sig(
        System::Mlrs,
        "mLRS 2.4",
        (2_400_500_000, 2_480_500_000),
        Some(1_000_000),
        Some(80),
        &[9_000, 20_000, 32_000, 53_000],
        (2_300, 24_000),
        Source::Firmware,
    ),
    sig(
        System::Sik,
        "SiK/RFD900 915",
        (915_000_000, 928_000_000),
        Some(250_000),
        Some(50),
        &[],
        (500, 45_000),
        Source::Firmware,
    ),
    sig(
        System::OpenLrsNg,
        "OpenLRSng 433",
        (433_000_000, 438_000_000),
        Some(50_000),
        None,
        &[],
        (2_000, 40_000),
        Source::Firmware,
    ),
    sig(
        System::Crossfire,
        "TBS Crossfire 900",
        (902_000_000, 928_000_000),
        None,
        None,
        &[6_667, 20_000],
        (1_000, 15_000),
        Source::VendorDocs,
    ),
    sig(
        System::FrskyR9,
        "FrSky R9 / ACCESS 900",
        (902_000_000, 928_000_000),
        None,
        None,
        &[9_000, 18_000],
        (1_000, 9_000),
        Source::Estimate,
    ),
    sig(
        System::Tracer,
        "TBS Tracer 2.4",
        (2_400_000_000, 2_483_500_000),
        None,
        None,
        &[4_000],
        (200, 3_000),
        Source::Estimate,
    ),
    sig(
        System::Ghost,
        "ImmersionRC Ghost 2.4",
        (2_400_000_000, 2_483_500_000),
        None,
        None,
        &[2_000, 4_000],
        (200, 3_000),
        Source::Estimate,
    ),
    sig(
        System::FrskyX,
        "FrSky D16 / ACCESS 2.4",
        (2_400_000_000, 2_483_500_000),
        None,
        Some(47),
        &[9_000],
        (500, 2_000),
        Source::PublicRe,
    ),
    sig(
        System::FlyskyAfhds2a,
        "FlySky AFHDS 2A",
        (2_400_000_000, 2_483_500_000),
        None,
        Some(16),
        &[3_850],
        (300, 1_500),
        Source::PublicRe,
    ),
];
