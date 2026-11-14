//! One channel's verdict from its dwell level and its capture's features.

use crate::analyse::Analysis;
use crate::channels;

/// What a dwell amounted to: the loud part's level over the floor and the
/// fraction of the dwell that was on (6 dB over the floor). See
/// [`crate::Meter::dwell`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Dwell {
    pub snr_db: i8,
    pub duty_pct: u8,
}

/// The scanner's verdict. `link::RfKind` has every kind but `Quiet` and
/// `Wifi`, which the firmware never reports to the hub (Wi-Fi is not a
/// drone signal, and calling every access point a sighting would bury the
/// real ones).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Nothing over the floor worth a look.
    Quiet,
    /// FM video: the line rate was found.
    AnalogVideo,
    /// Continuous, loud, on an FPV channel, nothing else recognised (an
    /// analog transmitter whose line rate did not show, or a carrier).
    FpvCarrier,
    /// Continuous, wide and flat, on HDZero's grid.
    Hdzero,
    /// 15 kHz OFDM (DJI, Walksnail, Autel, Herelink, and DroneID itself).
    LteStyle,
    /// Wi-Fi at half clock (6.4 µs symbols).
    WifiNarrow,
    /// Ordinary Wi-Fi (3.2 or 12.8 µs symbols).
    Wifi,
    /// Loud, bursty, unrecognised.
    Energy,
}

impl Kind {
    pub const fn name(self) -> &'static str {
        match self {
            Kind::Quiet => "quiet",
            Kind::AnalogVideo => "analog_video",
            Kind::FpvCarrier => "fpv_carrier",
            Kind::Hdzero => "hdzero",
            Kind::LteStyle => "lte_style",
            Kind::WifiNarrow => "wifi_narrow",
            Kind::Wifi => "wifi",
            Kind::Energy => "energy",
        }
    }

    /// Whether the firmware sends this kind to the hub.
    pub const fn reported(self) -> bool {
        !matches!(self, Kind::Quiet | Kind::Wifi)
    }
}

/// Level over the floor worth analysing, dB.
pub const STRONG_DB: i8 = 8;
/// Duty (percent) that counts as continuous: an analog transmitter never pauses.
pub const CONTINUOUS_PCT: u8 = 90;
/// Feature thresholds (x1000), tuned on the synthetic signals in the tests:
/// noise sits within about +-20 of zero on all of them.
pub const LINE_MIN: i16 = 150;
/// Cyclic-prefix metrics must beat this many standard deviations of their
/// own noise. The deviation depends on how many independent samples the
/// capture holds, which for a narrow signal is far fewer than its length:
/// about `n * width / fs`, so a 1 MHz signal needs a metric four times
/// larger than a 16 MHz one ([`cp_threshold`]).
pub const CP_SIGMAS: u32 = 3;
/// Never accept a cyclic-prefix metric below this (x1000).
pub const CP_FLOOR: i16 = 20;
/// HDZero: at least this wide (it is 17 or 27 MHz; our capture is 20 MHz)
/// and this flat (tenths of a dB, arithmetic over geometric mean).
pub const HDZ_WIDTH_KHZ: u16 = 14_000;
pub const HDZ_FLAT_X10: u16 = 25;
/// Line rate accepted as video (PAL 15,625 Hz, NTSC 15,734 Hz, a margin
/// for the estimate).
pub const LINE_HZ: core::ops::RangeInclusive<u16> = 15_200..=16_200;

/// The cyclic-prefix threshold (x1000) for this capture: [`CP_SIGMAS`]
/// standard deviations of the metric on noise of the capture's bandwidth.
pub fn cp_threshold(a: &Analysis) -> i16 {
    let n_eff = (a.n as u64 * a.width_khz.max(500) as u64 / (crate::FS as u64 / 1000)).max(1);
    let sigma_x1000 = 1000 * 1000 / isqrt(n_eff * 1000 * 1000).max(1);
    ((CP_SIGMAS as u64 * sigma_x1000).min(1000) as i16).max(CP_FLOOR)
}

fn isqrt(v: u64) -> u64 {
    droneid::math::isqrt64(v)
}

/// `mhz`: the channel centre (for the HDZero grid). `a`: the analysis of a
/// capture taken while the channel was loud, or `None` if there was none.
pub fn classify(d: &Dwell, a: Option<&Analysis>, mhz: u16) -> Kind {
    if d.snr_db < STRONG_DB || d.duty_pct == 0 {
        return Kind::Quiet;
    }
    let continuous = d.duty_pct >= CONTINUOUS_PCT;
    let Some(a) = a else {
        return if continuous { Kind::FpvCarrier } else { Kind::Energy };
    };
    if a.line >= LINE_MIN && LINE_HZ.contains(&a.line_hz) {
        return Kind::AnalogVideo;
    }
    let cp = cp_threshold(a);
    if a.cp_lte >= cp {
        return Kind::LteStyle;
    }
    if a.cp_wifi >= cp {
        return Kind::Wifi;
    }
    if a.cp_half >= cp {
        return Kind::WifiNarrow;
    }
    if continuous && a.width_khz >= HDZ_WIDTH_KHZ && a.flat_x10 <= HDZ_FLAT_X10 && channels::hdzero_grid(mhz) {
        return Kind::Hdzero;
    }
    if continuous { Kind::FpvCarrier } else { Kind::Energy }
}
