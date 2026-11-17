//! UTC on a node without GPS: the hub sends `Command::TimeSync` about once
//! a second; the node takes each message's arrival as "now is `utc_ms`".
//!
//! Between messages it counts its own uptime, corrected for the rate
//! difference between its crystal and the hub's clock (measured over a long
//! baseline, so the few milliseconds of jitter a radio hop adds average
//! out). If no message has arrived for [`STALE_US`] the time is unknown:
//! hopping by a stale clock would put the receiver on the wrong channel
//! with confidence, which is worse than parking on one.
//!
//! The arrival time includes the link delay (UART: well under a
//! millisecond; ESP-NOW through the gateway: a few ms). The slots are 400 ms
//! long, so neither matters for hopping; the hub knows its own path if it
//! wants to correct the `utc_ms` reported with each frame.

/// No sync for this long: time unknown.
pub const STALE_US: u64 = 10_000_000;
/// A sync further than this from the running estimate restarts the
/// estimate (the hub's clock jumped, e.g. its first GPS fix).
pub const JUMP_MS: u64 = 2_000;
/// Rate is measured only over at least this much time...
pub const MIN_BASELINE_US: u64 = 20_000_000;
/// ...and the reference sample is renewed after this long, so a slowly
/// changing rate (temperature) is followed.
pub const MAX_BASELINE_US: u64 = 600_000_000;
/// Crystals are within ~±50 ppm; anything beyond this is a measurement gone
/// wrong and is clamped.
pub const MAX_RATE_PPB: i64 = 500_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Sample {
    up_us: u64,
    utc_ms: u64,
}

#[derive(Debug, Clone, Default)]
pub struct UtcClock {
    last: Option<Sample>,
    anchor: Option<Sample>,
    /// How much faster UTC runs than our uptime, parts per billion.
    rate_ppb: i64,
    pps: bool,
    /// Syncs taken, and estimate restarts after a jump.
    pub syncs: u32,
    pub jumps: u32,
    /// The estimate's error at the latest sync (estimate minus sync), ms.
    pub last_error_ms: i64,
}

impl UtcClock {
    pub const fn new() -> Self {
        Self { last: None, anchor: None, rate_ppb: 0, pps: false, syncs: 0, jumps: 0, last_error_ms: 0 }
    }

    /// A `TimeSync { utc_ms, pps }` arrived at node uptime `up_us`.
    pub fn sync(&mut self, up_us: u64, utc_ms: u64, pps: bool) {
        let s = Sample { up_us, utc_ms };
        self.syncs = self.syncs.wrapping_add(1);
        self.pps = pps;
        if let Some(pred) = self.extrapolate(up_us) {
            let err = pred as i64 - utc_ms as i64;
            self.last_error_ms = err;
            if err.unsigned_abs() > JUMP_MS {
                self.jumps = self.jumps.wrapping_add(1);
                self.anchor = None;
                self.rate_ppb = 0;
            }
        }
        match self.anchor {
            None => self.anchor = Some(s),
            Some(a) if up_us <= a.up_us || utc_ms < a.utc_ms => {
                // Uptime went backwards (cannot on one boot) or UTC did:
                // start over.
                self.anchor = Some(s);
                self.rate_ppb = 0;
            }
            Some(a) => {
                let dup = (up_us - a.up_us) as i128;
                if dup as u64 >= MIN_BASELINE_US {
                    let dutc_us = (utc_ms - a.utc_ms) as i128 * 1000;
                    let ppb = (dutc_us - dup) * 1_000_000_000 / dup;
                    self.rate_ppb = (ppb as i64).clamp(-MAX_RATE_PPB, MAX_RATE_PPB);
                }
                if dup as u64 >= MAX_BASELINE_US {
                    self.anchor = Some(s);
                }
            }
        }
        self.last = Some(s);
    }

    /// The estimate from the latest sync, however old.
    fn extrapolate(&self, up_us: u64) -> Option<u64> {
        let l = self.last?;
        let dt = up_us.checked_sub(l.up_us)? as i128;
        let corrected_us = dt + dt * self.rate_ppb as i128 / 1_000_000_000;
        Some((l.utc_ms as i128 + corrected_us / 1000).max(0) as u64)
    }

    /// UTC (Unix ms) at node uptime `up_us`, or `None` when unknown (no
    /// sync yet, or none for [`STALE_US`]).
    pub fn now_ms(&self, up_us: u64) -> Option<u64> {
        let l = self.last?;
        if up_us.saturating_sub(l.up_us) > STALE_US {
            return None;
        }
        self.extrapolate(up_us)
    }

    pub fn known(&self, up_us: u64) -> bool {
        self.now_ms(up_us).is_some()
    }

    /// The hub said its UTC was anchored to a GPS PPS edge (latest sync).
    pub fn pps(&self) -> bool {
        self.pps
    }

    /// Measured rate difference, parts per million (positive: UTC runs
    /// faster than our uptime).
    pub fn rate_ppm(&self) -> f32 {
        self.rate_ppb as f32 / 1000.0
    }

    /// Microseconds since the latest sync.
    pub fn age_us(&self, up_us: u64) -> Option<u64> {
        Some(up_us.saturating_sub(self.last?.up_us))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UTC0: u64 = 1_763_337_600_000;

    #[test]
    fn unknown_until_synced_then_stale() {
        let mut c = UtcClock::new();
        assert_eq!(c.now_ms(5), None);
        c.sync(1_000_000, UTC0, true);
        assert_eq!(c.now_ms(1_000_000), Some(UTC0));
        assert_eq!(c.now_ms(1_250_000), Some(UTC0 + 250));
        assert_eq!(c.now_ms(1_000_000 + STALE_US), Some(UTC0 + 10_000));
        assert_eq!(c.now_ms(1_000_001 + STALE_US), None);
        assert!(c.pps());
    }

    /// A node crystal 40 ppm slow: after a minute of syncs the rate is
    /// measured, and a 9 s gap between syncs is bridged to within 1 ms.
    #[test]
    fn tracks_drift() {
        let mut c = UtcClock::new();
        // Uptime runs 40 ppm slow: 1 s of UTC = 999_960 us of uptime.
        let up = |utc_s: u64| 5_000_000 + utc_s * 999_960;
        for s in 0..=60 {
            c.sync(up(s), UTC0 + s * 1000, true);
        }
        assert!((c.rate_ppm() - 40.0).abs() < 0.5, "{}", c.rate_ppm());
        let est = c.now_ms(up(69)).unwrap();
        assert!(est.abs_diff(UTC0 + 69_000) <= 1, "{est}");
        // Without drift correction it would be 69 s * 40 ppm = 2.8 ms off at
        // 69 s; the check above would still pass, so check a long gap too.
        let mut c2 = c.clone();
        c2.last = Some(Sample { up_us: up(60), utc_ms: UTC0 + 60_000 });
        let est = c2.extrapolate(up(60 + 500)).unwrap();
        assert!(est.abs_diff(UTC0 + 560_000) <= 2, "{est}");
    }

    /// Jitter of a few ms on each arrival does not throw the estimate.
    #[test]
    fn jitter_averages_out() {
        let mut c = UtcClock::new();
        let mut x: u32 = 12345;
        for s in 0..120u64 {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let jitter_us = u64::from(x >> 16) % 8000; // 0-8 ms late
            c.sync(1_000_000 + s * 1_000_000 + jitter_us, UTC0 + s * 1000, false);
        }
        assert!(c.rate_ppm().abs() < 200.0, "{}", c.rate_ppm());
        let est = c.now_ms(1_000_000 + 120 * 1_000_000).unwrap();
        assert!(est.abs_diff(UTC0 + 120_000) < 15, "{est}");
    }

    #[test]
    fn jump_restarts() {
        let mut c = UtcClock::new();
        for s in 0..30 {
            c.sync(s * 1_000_000, UTC0 + s * 1000, false);
        }
        // The hub gets GPS and its clock jumps an hour.
        c.sync(30_000_000, UTC0 + 3_630_000, true);
        assert_eq!(c.jumps, 1);
        assert_eq!(c.now_ms(30_500_000), Some(UTC0 + 3_630_500));
        assert!(c.last_error_ms < -3_000_000);
    }

    #[test]
    fn absurd_rate_is_clamped() {
        let mut c = UtcClock::new();
        c.sync(0, UTC0, false);
        // UTC claims 21 s passed in 20 s of uptime: 1 s off the estimate,
        // under the 2 s jump limit, so it is taken as a rate (50000 ppm)
        // and clamped.
        c.sync(20_000_000, UTC0 + 21_000, false);
        assert_eq!(c.rate_ppm(), 500.0);
    }
}
