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
//! **One message never moves the clock far** (WP-12). A sync within [`SHIFT_MS`] of the running
//! estimate is taken at once (radio-hop and hub jitter). One further off is *not* adopted on its
//! own: it becomes a candidate, and only [`CONFIRM`] such syncs in a row, all within
//! [`AGREE_MS`] of each other (in the node's own uptime), move the clock there (and restart
//! the rate estimate). A replayed or time-shifted `TimeSync`, or one stepped hub second,
//! therefore does nothing; an attacker must deliver three consistent ones in a row (which the
//! link's age check, `airlink::v3`, bounds). The very first sync after boot is taken as is:
//! there is nothing to compare it with. While candidates disagree with the estimate the old
//! estimate runs on, and after [`STALE_US`] without an adopted sync the time is unknown, so
//! the node parks rather than hops on a doubtful clock.
//!
//! What the node does **not** do: compare its clock with the slot timing of the FLARM/OGN
//! packets it hears. A clock off by a whole second puts it on another second's channel and
//! (FLARM) another second's key, so it hears nothing at all to compare; a clock off by under
//! 200 ms still hears packets, and their arrival inside the 400-800 / 800-1200 ms slots would
//! show it, but that needs the radio's packet timestamp plumbed through and is left as a
//! later improvement (docs/AERO-NODE.md).
//!
//! The arrival time includes the link delay (UART: well under a
//! millisecond; ESP-NOW through the gateway: a few ms). The slots are 400 ms
//! long, so neither matters for hopping; the hub knows its own path if it
//! wants to correct the `utc_ms` reported with each frame.

/// No sync for this long: time unknown.
pub const STALE_US: u64 = 10_000_000;
/// A sync further than this (ms) from the running estimate is not adopted on its own. The
/// hub's own clock follows GPS within 150 ms (no PPS) and the link adds a few ms; the slots
/// leave a 200 ms silent gap. 100 ms: honest hub jitter is taken, a shift big enough to put
/// the node into the wrong slot is not.
pub const SHIFT_MS: u64 = 100;
/// ... it takes this many such syncs in a row ...
pub const CONFIRM: u8 = 3;
/// ... each within this (ms) of the first (UTC minus node uptime), to move the clock.
pub const AGREE_MS: i64 = 50;
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
    /// Syncs received, estimate restarts after a confirmed shift, and syncs not adopted
    /// (further than [`SHIFT_MS`] from the estimate and not, or not yet, confirmed).
    pub syncs: u32,
    pub jumps: u32,
    pub refused: u32,
    /// A shifted candidate: its offset (UTC ms minus uptime ms) and how many in a row.
    cand: Option<(i64, u8)>,
    /// The estimate's error at the latest sync (estimate minus sync), ms.
    pub last_error_ms: i64,
}

impl UtcClock {
    pub const fn new() -> Self {
        Self { last: None, anchor: None, rate_ppb: 0, pps: false, syncs: 0, jumps: 0, refused: 0, cand: None, last_error_ms: 0 }
    }

    /// A `TimeSync { utc_ms, pps }` arrived at node uptime `up_us`.
    pub fn sync(&mut self, up_us: u64, utc_ms: u64, pps: bool) {
        let s = Sample { up_us, utc_ms };
        self.syncs = self.syncs.wrapping_add(1);
        if let Some(pred) = self.extrapolate(up_us) {
            let err = pred as i64 - utc_ms as i64;
            self.last_error_ms = err;
            if err.unsigned_abs() > SHIFT_MS {
                let off = utc_ms as i64 - (up_us / 1000) as i64;
                let n = match self.cand {
                    Some((c, n)) if (off - c).abs() <= AGREE_MS => n + 1,
                    _ => 1,
                };
                if n < CONFIRM {
                    let first = if n > 1 { self.cand.map_or(off, |(c, _)| c) } else { off };
                    self.cand = Some((first, n));
                    self.refused = self.refused.wrapping_add(1);
                    return;
                }
                self.jumps = self.jumps.wrapping_add(1);
                self.anchor = None;
                self.rate_ppb = 0;
            }
        }
        self.cand = None;
        self.pps = pps;
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

    /// A lasting shift (the hub's clock really moved, e.g. an operator accepted a new GPS
    /// time): three consistent syncs in a row move the node there and restart the estimate.
    #[test]
    fn consistent_series_is_adopted() {
        let mut c = UtcClock::new();
        for s in 0..30 {
            c.sync(s * 1_000_000, UTC0 + s * 1000, false);
        }
        for s in 30..32 {
            c.sync(s * 1_000_000, UTC0 + 3_600_000 + s * 1000, true);
            assert_eq!(c.now_ms(s * 1_000_000 + 500_000), Some(UTC0 + s * 1000 + 500), "not yet");
        }
        c.sync(32_000_000, UTC0 + 3_632_000, true);
        assert_eq!(c.jumps, 1);
        assert_eq!(c.refused, 2);
        assert_eq!(c.now_ms(32_500_000), Some(UTC0 + 3_632_500));
        assert!(c.last_error_ms < -3_000_000);
        assert!(c.pps());
    }

    /// One shifted TimeSync (a replay, or a hub second stepped by a spoofer): 1 s and 0.5 s
    /// either way, and 150 ms. Not adopted; the estimate runs on. Before WP-12 every sync
    /// under 2 s was taken at once.
    #[test]
    fn one_shifted_sync_is_not_adopted() {
        for shift in [1_000i64, -1_000, 500, -500, 150] {
            let mut c = UtcClock::new();
            for s in 0..30u64 {
                c.sync(s * 1_000_000, UTC0 + s * 1000, false);
            }
            c.sync(30_000_000, (UTC0 as i64 + 30_000 + shift) as u64, true);
            assert_eq!(c.now_ms(30_000_000), Some(UTC0 + 30_000), "{shift}");
            assert!(!c.pps(), "nothing of it taken");
            assert_eq!((c.jumps, c.refused), (0, 1));
            // Honest syncs resume: no harm done, the candidate is forgotten.
            for s in 31..40u64 {
                c.sync(s * 1_000_000, UTC0 + s * 1000, false);
            }
            assert_eq!(c.now_ms(40_000_000), Some(UTC0 + 40_000));
            // Two shifted, one honest, two shifted: never three in a row.
            for (i, sh) in [shift, shift, 0, shift, shift].into_iter().enumerate() {
                let s = 41 + i as u64;
                c.sync(s * 1_000_000, (UTC0 as i64 + s as i64 * 1000 + sh) as u64, false);
            }
            assert_eq!(c.jumps, 0, "{shift}");
            // Three shifted ones that disagree with each other (a replayed mix) do not
            // confirm either.
            c.sync(49_000_000, UTC0 + 49_000, false);
            for (i, sh) in [shift, shift + 300, shift - 300].into_iter().enumerate() {
                let s = 50 + i as u64;
                c.sync(s * 1_000_000, (UTC0 as i64 + s as i64 * 1000 + sh) as u64, false);
            }
            assert_eq!(c.jumps, 0, "{shift}");
        }
    }

    /// Honest syncs with the hub's jitter (no PPS: its UTC is re-anchored on every message,
    /// +-40 ms, so up to 80 ms sync to sync) and the radio's (0-8 ms) never trip the rule,
    /// over an hour, with the node crystal 40 ppm off.
    #[test]
    fn honest_jitter_never_refused() {
        let mut c = UtcClock::new();
        let mut x: u32 = 777;
        for s in 0..3600u64 {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let hub_jitter = i64::from(x >> 16) % 81 - 40;
            let up = 1_000_000 + s * 999_960 + u64::from(x >> 8) % 8000;
            c.sync(up, (UTC0 as i64 + s as i64 * 1000 + hub_jitter) as u64, false);
        }
        assert_eq!((c.refused, c.jumps), (0, 0));
    }

    #[test]
    fn absurd_rate_is_clamped() {
        let mut c = UtcClock::new();
        c.sync(0, UTC0, false);
        // UTC claims 20.05 s passed in 20 s of uptime: 50 ms off the estimate, under the
        // shift limit, so it is taken as a rate (2500 ppm) and clamped.
        c.sync(20_000_000, UTC0 + 20_050, false);
        assert_eq!(c.rate_ppm(), 500.0);
    }
}
