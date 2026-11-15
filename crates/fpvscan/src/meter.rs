//! Per-dwell level meter over the trigger's probe powers, and the noise
//! floor across a sweep.
//!
//! A probe is `rawcap`'s sum of [`rawcap::iq::POWER`]-style per-byte powers
//! over 256 bytes (6.4 µs every 51.2 µs), so a 60 ms dwell gives about 1170
//! probes. The meter keeps a histogram in half-dB steps, from which it reads
//! percentiles, the duty cycle above a threshold and the level of the loud
//! part, with no per-probe storage.
//!
//! The noise floor is not measurable on a channel with a continuous
//! transmitter (a video transmitter never pauses), so it is tracked across the
//! sweep: the quietest channel's 10th percentile, rising only slowly.

use crate::db_x10;

/// Histogram base (tenths of a dB) and step. The quietest possible probe (256
/// bytes of the lowest power, 2 each) is 27.1 dB; the loudest (450 each)
/// 50.6 dB.
const BASE_X10: i32 = 270;
const STEP_X10: i32 = 5;
const BINS: usize = 50;

#[derive(Clone, Debug)]
pub struct Meter {
    hist: [u16; BINS],
    n: u32,
}

impl Default for Meter {
    fn default() -> Self {
        Self::new()
    }
}

impl Meter {
    pub const fn new() -> Self {
        Meter { hist: [0; BINS], n: 0 }
    }

    pub fn reset(&mut self) {
        *self = Meter::new();
    }

    pub fn count(&self) -> u32 {
        self.n
    }

    /// One probe power sum.
    pub fn feed(&mut self, power: u32) {
        let b = ((db_x10(power.max(1) as u64) - BASE_X10) / STEP_X10).clamp(0, BINS as i32 - 1) as usize;
        self.hist[b] = self.hist[b].saturating_add(1);
        self.n += 1;
    }

    fn bin_db(b: usize) -> i32 {
        BASE_X10 + STEP_X10 * b as i32 + STEP_X10 / 2
    }

    /// Level (tenths of a dB, probe units) below which `pct` percent of the
    /// probes fall.
    pub fn percentile_x10(&self, pct: u32) -> i32 {
        if self.n == 0 {
            return 0;
        }
        let want = (self.n * pct).div_ceil(100).max(1);
        let mut acc = 0;
        for (b, &h) in self.hist.iter().enumerate() {
            acc += h as u32;
            if acc >= want {
                return Self::bin_db(b);
            }
        }
        Self::bin_db(BINS - 1)
    }

    /// Percent of probes at or above `level_x10`.
    pub fn duty_pct(&self, level_x10: i32) -> u8 {
        if self.n == 0 {
            return 0;
        }
        let first = ((level_x10 - BASE_X10) / STEP_X10).clamp(0, BINS as i32) as usize;
        let above: u32 = self.hist[first.min(BINS)..].iter().map(|&h| h as u32).sum();
        (above * 100 / self.n) as u8
    }

    /// What one dwell amounted to, against the floor.
    pub fn dwell(&self, floor_x10: i32) -> crate::Dwell {
        // "On": 6 dB over the floor, as the trigger's rise threshold.
        let duty = self.duty_pct(floor_x10 + 60);
        // The loud part's level: the 90th percentile (a bursty channel's
        // bursts, a continuous one's whole dwell).
        let snr = (self.percentile_x10(90) - floor_x10) / 10;
        crate::Dwell { snr_db: snr.clamp(-128, 127) as i8, duty_pct: duty }
    }
}

/// The noise floor across a sweep, tenths of a dB in probe units.
#[derive(Clone, Copy, Debug, Default)]
pub struct FloorTracker {
    floor_x10: Option<i32>,
}

/// How fast the floor may rise per dwell (tenths of a dB): about 1 dB per
/// sweep of 31 channels, so a quiet channel pulls it straight down again.
const RISE_X10: i32 = 1;

impl FloorTracker {
    pub const fn new() -> Self {
        FloorTracker { floor_x10: None }
    }

    /// Fold in one dwell's meter; returns the floor to judge it against.
    pub fn update(&mut self, m: &Meter) -> i32 {
        let p10 = m.percentile_x10(10);
        let f = match self.floor_x10 {
            None => p10,
            Some(f) if p10 < f => p10,
            Some(f) => f + RISE_X10.min(p10 - f),
        };
        self.floor_x10 = Some(f);
        f
    }

    pub fn get(&self) -> Option<i32> {
        self.floor_x10
    }

    pub fn reset(&mut self) {
        self.floor_x10 = None;
    }
}
