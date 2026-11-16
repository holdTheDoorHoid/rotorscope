//! One scanner over both bands: 2.4 GHz ([`crate::channels24`]) and
//! 5.8 GHz ([`crate::channels`]). The firmware calls these instead of the
//! per-band modules; on a 5.8 GHz channel every function here gives exactly
//! what the 5.8 GHz module does.
//!
//! What differs on 2.4 GHz ([`classify`]):
//!
//! - **Separate noise floor.** The receiver's gain tables, front end and
//!   noise differ between the bands (bring-up: noise 43-51 codes RMS at
//!   5756 MHz, 64-84 at 2414 MHz, same gain index), so one floor across both
//!   would make every 2.4 GHz channel look a few dB loud. [`Floors`] keeps
//!   one per band. On 2.4 GHz the out-of-ISM channels (2370, 2390, 2510)
//!   give a clean floor even when every Wi-Fi channel is busy.
//! - **Sub-line periodicity is not video.** FM repeating at a half, third
//!   or quarter of a line also repeats every line; an inverter microwave
//!   oven's switching ripple can do that. [`line_shape`] catches it, and a smooth ripple at the line rate itself.
//! - **DSSS is Wi-Fi.** 802.11b's Barker-spread preamble ([`crate::dsss`])
//!   is recognised as [`Kind::Wifi`]: it has no cyclic prefix for the OFDM
//!   tests to see.
//! - **Only confirmed video is reported inside the ISM band.** 2400-2500
//!   MHz is full of loud, unrecognised, sometimes continuous energy that is
//!   not a drone (microwave ovens, inverter ovens are continuous; streaming
//!   Wi-Fi; cordless gadgets). There a continuous signal without a line rate
//!   is [`Kind::Energy`] (not reported), not [`Kind::FpvCarrier`]. Outside
//!   it (2370, 2390, 2510) a continuous carrier is still reported.
//!   Analog video with its line rate, LTE-style OFDM (DJI and Autel video use
//!   2.4 GHz too) and half-clock Wi-Fi are reported on both bands.

use droneid::math::C16;

use crate::analyse::{Analysis, Scratch};
use crate::classify::Kind;
use crate::meter::{FloorTracker, Meter};
use crate::{channels, channels24, classify as c58, dsss};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Band {
    G24,
    G58,
}

impl Band {
    pub const fn of(mhz: u16) -> Band {
        if mhz < 4000 { Band::G24 } else { Band::G58 }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Band::G24 => "2g4",
            Band::G58 => "5g8",
        }
    }
}

/// Microwave ovens' ISM allocation: on 2.4 GHz, continuous unrecognised
/// energy inside it is not reported.
pub const OVEN_ISM_MHZ: core::ops::RangeInclusive<u16> = 2400..=2500;

pub const fn reachable(mhz: u16) -> bool {
    match Band::of(mhz) {
        Band::G24 => channels24::reachable(mhz),
        Band::G58 => channels::reachable(mhz),
    }
}

/// The LO for a carrier at `mhz`, and the carrier's offset from it (kHz).
pub const fn lo_for(mhz: u16) -> (u16, i32) {
    match Band::of(mhz) {
        Band::G24 => channels24::lo_for(mhz),
        Band::G58 => channels::lo_for(mhz),
    }
}

/// A channel's name: band, band letter, channel number. Prints as `F8` on
/// 5.8 GHz and `2.4A1` on 2.4 GHz (the 2.4 GHz plans reuse letters).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Label {
    pub band: Band,
    pub letter: u8,
    pub ch: u8,
}

impl core::fmt::Display for Label {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.band == Band::G24 {
            f.write_str("2.4")?;
        }
        write!(f, "{}{}", self.letter as char, self.ch)
    }
}

pub fn label(mhz: u16) -> Option<Label> {
    let band = Band::of(mhz);
    let l = match band {
        Band::G24 => channels24::label(mhz),
        Band::G58 => channels::label(mhz),
    };
    l.map(|(letter, ch)| Label { band, letter, ch })
}

/// The built-in sweep of both bands, sorted (2.4 GHz first): every
/// 2.4 GHz channel and every reachable 5.8 GHz one.
pub const SWEEP: ([u16; 64], usize) = {
    let mut out = [0u16; 64];
    let mut n = 0;
    let mut i = 0;
    while i < channels24::ALL.1 {
        if channels24::reachable(channels24::ALL.0[i]) {
            out[n] = channels24::ALL.0[i];
            n += 1;
        }
        i += 1;
    }
    let mut i = 0;
    while i < channels::SWEEP.1 {
        out[n] = channels::SWEEP.0[i];
        n += 1;
        i += 1;
    }
    (out, n)
};

pub fn sweep() -> &'static [u16] {
    &SWEEP.0[..SWEEP.1]
}

/// The built-in sweep of one band.
pub fn sweep_of(band: Band) -> &'static [u16] {
    match band {
        Band::G24 => channels24::sweep(),
        Band::G58 => channels::sweep(),
    }
}

/// One noise floor per band.
#[derive(Clone, Copy, Debug, Default)]
pub struct Floors {
    g24: FloorTracker,
    g58: FloorTracker,
}

impl Floors {
    pub const fn new() -> Self {
        Floors { g24: FloorTracker::new(), g58: FloorTracker::new() }
    }

    /// Fold in one dwell on `mhz`; returns that band's floor.
    pub fn update(&mut self, mhz: u16, m: &Meter) -> i32 {
        match Band::of(mhz) {
            Band::G24 => self.g24.update(m),
            Band::G58 => self.g58.update(m),
        }
    }

    pub fn get(&self, band: Band) -> Option<i32> {
        match band {
            Band::G24 => self.g24.get(),
            Band::G58 => self.g58.get(),
        }
    }

    pub fn reset(&mut self) {
        *self = Floors::new();
    }
}

/// The 2.4 GHz-only features of an analysed capture (all zero on 5.8 GHz).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Extra {
    /// [`dsss::barker`], x1000.
    pub dsss: i16,
    /// [`line_shape`]: sub-line periodicity, x1000.
    pub sub_line: i16,
    /// [`line_shape`]: correlation half a line along, x1000.
    pub half_r: i16,
}

/// Compute [`Extra`] for a capture `x` just analysed into `s` as `a`.
pub fn extra(x: &[C16], s: &Scratch, a: &Analysis, mhz: u16) -> Extra {
    if Band::of(mhz) != Band::G24 {
        return Extra::default();
    }
    let (sub_line, half_r) = line_shape(s, a);
    Extra { dsss: dsss::barker(x), sub_line, half_r }
}

/// Two checks that the line-rate periodicity has the shape of video.
///
/// First (`sub_line`): how sharply the FM-demodulated signal also repeats at a half, third or
/// quarter of the line period the analysis found (x1000, the same
/// peak-minus-control measure as the line metric).
///
/// The line metric accepts any FM that repeats every 60-68 µs, and so any
/// FM that repeats at a whole fraction of that: an inverter microwave
/// oven's 20-45 kHz switching ripple at 31 kHz passes it (test
/// `microwave_ovens_are_never_reported`). Video repeats its sync pulse once
/// per line and nothing in a line half or a third of the way along (bar the
/// equalising pulses of a few vertical-interval lines), so its sub-line
/// metric stays near zero.
///
/// Second (`half_r`): the plain correlation half a line along. A smooth
/// FM ripple at the line rate itself (a sinusoid) is anti-periodic there,
/// near -1000; video, a short sync pulse once a line, is near -80 (the
/// sync's duty), and a picture that is dark on one half and bright on the
/// other pulls it to about -500 at worst.
pub fn line_shape(s: &Scratch, a: &Analysis) -> (i16, i16) {
    let d = s.demod(a.n);
    if a.line_hz == 0 || d.len() < 1024 {
        return (0, 0);
    }
    let e0: i64 = d.iter().map(|&v| v as i64 * v as i64).sum();
    if e0 == 0 {
        return (0, 0);
    }
    let m = d.len();
    let r = |lag: usize| -> i64 {
        let mut c: i64 = 0;
        for k in 0..m - lag {
            c += d[k] as i64 * d[k + lag] as i64;
        }
        c * 1000 * m as i64 / ((m - lag) as i64 * e0)
    };
    let line = crate::analyse::FS_D as usize / a.line_hz as usize;
    if !(crate::analyse::LINE_MIN..=crate::analyse::LINE_MAX).contains(&line) {
        return (0, 0);
    }
    let mut worst = i64::MIN;
    for k in 2..=4usize {
        let h = line.div_ceil(k);
        let peak = (h - 2..=h + 2).map(r).max().unwrap_or(0);
        let mut ctl = i64::MIN;
        for off in [16usize, 20, 24] {
            if h > off {
                ctl = ctl.max(r(h - off));
            }
            ctl = ctl.max(r(h + off));
        }
        worst = worst.max(peak - ctl);
    }
    let h = line.div_ceil(2);
    let half = (h - 2..=h + 2).map(r).min().unwrap_or(0);
    (worst.clamp(-1000, 1000) as i16, half.clamp(-1000, 1000) as i16)
}

/// Below this `half_r` the line periodicity is a smooth ripple, not video.
pub const HALF_R_MIN: i16 = -750;

/// The verdict for a dwell on `mhz` of either band. `e`: [`extra`] of the
/// analysed capture (default if none). On 5.8 GHz this is exactly
/// [`crate::classify::classify`].
pub fn classify(d: &crate::Dwell, a: Option<&Analysis>, e: &Extra, mhz: u16) -> Kind {
    let mut k = c58::classify(d, a, mhz);
    if Band::of(mhz) == Band::G58 {
        return k;
    }
    if k == Kind::AnalogVideo && a.is_some_and(|a| e.sub_line > a.line / 2 || e.half_r < HALF_R_MIN) {
        // Periodic, but faster than a line or smooth: not video. Judge it
        // as if the line rate had not shown.
        k = if d.duty_pct >= c58::CONTINUOUS_PCT { Kind::FpvCarrier } else { Kind::Energy };
    }
    match k {
        Kind::Quiet | Kind::AnalogVideo | Kind::LteStyle | Kind::Wifi | Kind::WifiNarrow => k,
        // Continuous or bursty, nothing recognised (HDZero's grid has no
        // 2.4 GHz channels, so `Hdzero` cannot happen here).
        Kind::FpvCarrier | Kind::Hdzero | Kind::Energy => {
            if a.is_some() && e.dsss >= dsss::DSSS_MIN {
                Kind::Wifi
            } else if k == Kind::FpvCarrier && !OVEN_ISM_MHZ.contains(&mhz) {
                Kind::FpvCarrier
            } else {
                Kind::Energy
            }
        }
    }
}
