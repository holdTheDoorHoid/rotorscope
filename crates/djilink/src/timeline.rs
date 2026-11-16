//! Burst timing across a whole dwell, from the trigger's probes.
//!
//! The firmware measures one probe (256 bytes, 6.4 µs) every 2048 bytes
//! (51.2 µs) of the 40 MS/s ring; a 60 ms dwell is about 1170 probes. Each is
//! kept as a level in half-dB steps (one byte), so the on/off split can be
//! made at the end of the dwell against the sweep's noise floor, exactly as
//! `fpvscan::Meter` judges duty. From the runs: how many bursts, how long,
//! how often.
//!
//! What it tells apart: a DroneID burst (0.57-0.64 ms, about once per
//! 600 ms: one or no burst in a 60 ms dwell), a controller's hops (~0.5 ms
//! bursts, repeating every few ms while the hop set passes through this
//! 20 MHz window), a video downlink (on for most of the dwell), Wi-Fi
//! (irregular).

/// Probes kept per dwell (78 ms at 51.2 µs).
pub const MAX_PROBES: usize = 1536;
/// Probe spacing, ns.
pub const PROBE_NS: u32 = 51_200;
/// Level code base (tenths of a dB) and step: code = (dB x10 - BASE) / 5.
const BASE_X10: i32 = 200;
const STEP_X10: i32 = 5;
/// "On": this far over the floor (tenths of a dB), as `fpvscan::Meter`.
pub const ON_X10: i32 = 60;
/// Run lengths kept for the medians.
const RUNS: usize = 64;

pub struct Timeline {
    lv: [u8; MAX_PROBES],
    n: u16,
}

impl Default for Timeline {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TimelineStats {
    pub probes: u16,
    /// Percent of probes on.
    pub duty_pct: u8,
    /// Runs of on-probes (bursts), not counting one cut by the dwell's ends
    /// in the medians.
    pub bursts: u16,
    /// Median burst length and median start-to-start interval, µs (0 if
    /// fewer than one / two whole bursts).
    pub burst_us: u32,
    pub period_us: u32,
    /// Longest burst, µs (cut ones included).
    pub longest_us: u32,
}

impl Timeline {
    pub const fn new() -> Self {
        Timeline { lv: [0; MAX_PROBES], n: 0 }
    }

    pub fn reset(&mut self) {
        self.n = 0;
    }

    pub fn count(&self) -> usize {
        self.n as usize
    }

    /// One probe power sum (the same units as `fpvscan::Meter::feed`).
    pub fn feed(&mut self, power: u32) {
        if (self.n as usize) < MAX_PROBES {
            let c = (crate::db_x10(power.max(1) as u64) - BASE_X10) / STEP_X10;
            self.lv[self.n as usize] = c.clamp(0, 255) as u8;
            self.n += 1;
        }
    }

    /// How far (tenths of a dB) the loudest 1% of probes stand over
    /// `floor_x10`: a short-burst transmitter (a controller hopping through
    /// this window for a tenth of the dwell) is loud here while the dwell's
    /// 90th-percentile level, which `fpvscan` judges by, is still the floor.
    pub fn loud_x10(&self, floor_x10: i32) -> i32 {
        let n = self.n as usize;
        if n == 0 {
            return i32::MIN;
        }
        let mut hist = [0u16; 256];
        for &c in &self.lv[..n] {
            hist[c as usize] += 1;
        }
        let want = n.div_ceil(100);
        let mut acc = 0;
        for c in (0..256).rev() {
            acc += hist[c] as usize;
            if acc >= want {
                return BASE_X10 + STEP_X10 * c as i32 - floor_x10;
            }
        }
        i32::MIN
    }

    /// Bursts against `floor_x10` (tenths of a dB, probe units: what
    /// `fpvscan::FloorTracker::update` returns).
    pub fn stats(&self, floor_x10: i32) -> TimelineStats {
        let n = self.n as usize;
        let mut out = TimelineStats { probes: self.n, ..TimelineStats::default() };
        if n == 0 {
            return out;
        }
        let thr = ((floor_x10 + ON_X10 - BASE_X10) / STEP_X10).clamp(0, 255) as u8;
        let on = |k: usize| self.lv[k] >= thr;
        let mut lens = [0u32; RUNS];
        let mut starts = [0u32; RUNS];
        let (mut nl, mut ns, mut on_n) = (0usize, 0usize, 0u32);
        let mut k = 0;
        while k < n {
            if !on(k) {
                k += 1;
                continue;
            }
            let s = k;
            while k < n && on(k) {
                k += 1;
            }
            let len = (k - s) as u32;
            on_n += len;
            out.bursts = out.bursts.saturating_add(1);
            out.longest_us = out.longest_us.max(len * PROBE_NS / 1000);
            let whole = s > 0 && k < n;
            if whole && nl < RUNS {
                lens[nl] = len;
                nl += 1;
            }
            if s > 0 && ns < RUNS {
                starts[ns] = s as u32;
                ns += 1;
            }
        }
        out.duty_pct = (on_n * 100 / n as u32) as u8;
        if nl > 0 {
            let l = &mut lens[..nl];
            l.sort_unstable();
            out.burst_us = l[nl / 2] * PROBE_NS / 1000;
        }
        if ns > 1 {
            let mut gaps = [0u32; RUNS];
            for i in 1..ns {
                gaps[i - 1] = starts[i] - starts[i - 1];
            }
            let g = &mut gaps[..ns - 1];
            g.sort_unstable();
            out.period_us = g[(ns - 1) / 2] * PROBE_NS / 1000;
        }
        out
    }
}
