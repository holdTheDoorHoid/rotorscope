//! The hub's own clock against GPS time (docs/GNSS-INTEGRITY.md, "Time").
//!
//! The hub has a free-running clock of its own (its uptime, from its crystal). GPS time may
//! steer it only by what a real crystal could explain; anything else is a `TimeJump` finding
//! of the integrity monitor ([`crate::integrity::Cause::TimeJump`]) and the GPS time is not
//! adopted.
//!
//! Two observables, each followed on its own ([`Chan`]):
//! - **the message**: the receiver's UTC against the hub time the epoch was read (NMEA or UBX).
//!   Its arrival jitters with the message length and the hub's main loop, so its tolerance is
//!   [`TOL_MSG_US`].
//! - **the PPS edge** (when wired): the whole UTC second against the hub time of the edge,
//!   taken in the interrupt. Sub-millisecond, so its tolerance is [`TOL_PPS_US`].
//!
//! Each channel compares every sample with a reference **offset** (`UTC - hub time`):
//! - Before the hub trusts GPS time, the reference is the last sample that agreed. A sample off
//!   by more than the tolerance fires the check; two disagreeing samples in a row that agree
//!   with each other become the new reference (nothing was trusted yet: the step is reported,
//!   then followed). A single late sample (a main-loop stall) does not move it.
//! - Once the hub trusts GPS time ([`TimeGuard::commit`] on trusted epochs), the reference is a
//!   **disciplined clock**: anchored on the first trusted sample, then steered towards each
//!   trusted sample by at most [`DRIFT_PPM`] of the time since the last steer. It is never
//!   re-anchored (only [`TimeGuard::reset`], the operator's "accept GPS time"), so a step stays
//!   a finding for as long as it lasts, and a slow walk faster than the budget is caught once
//!   it has built up to the tolerance.
//!
//! The allowance on every comparison is the tolerance plus [`DRIFT_PPM`] of the time since the
//! reference was last moved: what the hub's crystal can drift while nothing steers it.
//!
//! **Bounds** (attacker-facing): a step of more than [`TOL_MSG_US`] (150 ms; 5 ms with PPS) is
//! caught in two epochs; a walk at rate `w` (ppm) faster than `DRIFT_PPM + c` (`c` the hub
//! crystal's real error, within about 20 ppm) is caught after `tolerance / (w - DRIFT_PPM - c)`;
//! a walk at or below that is never caught by this check: at most `(20 + c)` ppm, 1.7 s a day
//! with a perfect crystal and 3.5 s a day at the crystal's tolerance.
//!
//! **Floor** ([`TimeGuard::floor_check`]): GPS time before this firmware's build date is
//! impossible, and time before the latest time the hub remembers trusting (kept on its SD card)
//! is untrusted until GPS time has run past it. Neither can see a jump *forward* at boot.

/// Tolerance of the message channel, microseconds. The NMEA/UBX arrival time moves with the
/// length of the epoch's messages (tens of ms at 115200 baud) and the hub's main loop (under
/// 20 ms normally); 150 ms is three times that, and well under the 200 ms silent gap and the
/// 400 ms slots the aero nodes hop by (a 300 ms step is caught).
pub const TOL_MSG_US: i64 = 150_000;
/// Tolerance of the PPS channel, microseconds: the edge is timestamped in its interrupt
/// (microseconds); 5 ms covers an interrupt held off by a critical section.
pub const TOL_PPS_US: i64 = 5_000;
/// What the hub's crystal may drift against true time, parts per million (the ESP32-P4's
/// 40 MHz crystal: +-10 ppm at 25 C, about as much again over temperature and ageing). GPS may
/// steer the hub's clock by at most this rate.
pub const DRIFT_PPM: i64 = 20;
/// The disciplined clock moves towards each trusted sample by this fraction of the difference
/// (and never by more than the drift budget): one sample's jitter barely moves it.
pub const STEER_DIV: i64 = 8;
/// A trusted sample this long (us) after the previous one closes a gap: see [`Chan`]'s commit.
pub const GAP_US: u64 = 2_000_000;
/// Time earlier than the remembered time by more than this (ms) is untrusted.
pub const REMEMBER_SLACK_MS: i64 = 2_000;

/// One channel's reference.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Chan {
    /// `(offset us, hub time us)`: the reference offset and when it was last moved.
    reff: Option<(i64, u64)>,
    /// The reference is the disciplined (trusted) clock.
    disc: bool,
    /// The offset of the last sample that disagreed, if the previous sample did.
    pend: Option<i64>,
}

impl Chan {
    /// Check one sample `(offset us, hub time us)` against tolerance `tol` (us). Returns the
    /// sample's difference from the reference (us) and whether it is beyond the allowance.
    fn check(&mut self, off: i64, at: u64, tol: i64) -> (i64, bool) {
        let Some((r, rat)) = self.reff else {
            self.reff = Some((off, at));
            return (0, false);
        };
        let res = off - r;
        let allowed = tol + allowance(at.saturating_sub(rat));
        if res.abs() <= allowed {
            self.pend = None;
            if !self.disc {
                self.reff = Some((off, at));
            }
            return (res, false);
        }
        if !self.disc
            && let Some(p) = self.pend
            && (off - p).abs() <= tol
        {
            // Nothing trusted yet: the new level held for two samples; follow it.
            self.reff = Some((off, at));
            self.pend = None;
        } else {
            self.pend = Some(off);
        }
        (res, true)
    }

    /// A trusted sample: anchor the disciplined clock, or steer it within the budget.
    fn commit(&mut self, off: i64, at: u64) {
        match self.reff {
            Some((r, rat)) if self.disc => {
                let dt = at.saturating_sub(rat);
                let lim = allowance(dt);
                // Epoch to epoch, a fraction (jitter averages out). After a gap (no trusted
                // epoch for a while), the whole difference up to what the crystal could have
                // drifted meanwhile: the check allowed exactly that much.
                let want = if dt > GAP_US { off - r } else { (off - r) / STEER_DIV };
                let step = want.clamp(-lim, lim);
                self.reff = Some((r + step, at));
            }
            _ => {
                self.reff = Some((off, at));
                self.disc = true;
            }
        }
        self.pend = None;
    }

    /// The disciplined clock is anchored.
    pub fn disciplined(&self) -> bool {
        self.disc
    }

    /// The reference offset (UTC minus hub time), microseconds.
    pub fn offset_us(&self) -> Option<i64> {
        self.reff.map(|(o, _)| o)
    }
}

/// Crystal drift allowed over `dt_us`, microseconds (rounded up, so steering at the budget
/// keeps up with a crystal at the budget).
fn allowance(dt_us: u64) -> i64 {
    (dt_us.min(i64::MAX as u64 / 2) as i64 / 1_000_000).saturating_mul(DRIFT_PPM)
        + ((dt_us % 1_000_000) as i64 * DRIFT_PPM + 999_999) / 1_000_000
}

/// One epoch's time observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    /// The receiver's UTC (Unix ms) for this epoch.
    pub utc_ms: i64,
    /// Hub time the epoch was read, microseconds.
    pub at_us: u64,
    /// The PPS edge of this epoch's second, hub microseconds, if one arrived.
    pub pps_us: Option<u64>,
}

impl Sample {
    fn msg(&self) -> i64 {
        self.utc_ms.saturating_mul(1000) - self.at_us as i64
    }
    /// The PPS edge marks the whole second the message names.
    fn pps(&self) -> Option<(i64, u64)> {
        let edge = self.pps_us?;
        let sec_ms = (self.utc_ms + 500).div_euclid(1000) * 1000;
        Some((sec_ms.saturating_mul(1000) - edge as i64, edge))
    }
}

/// What [`TimeGuard::check`] found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Check {
    /// The largest difference from a reference this epoch, ms (signed).
    pub step_ms: i64,
    /// Beyond the allowance on either channel.
    pub jump: bool,
}

/// The two channels, the floor and the remembered time. About 80 bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TimeGuard {
    /// The message channel.
    pub msg: Chan,
    /// The PPS channel.
    pub pps: Chan,
    floor_ms: i64,
    remembered_ms: Option<i64>,
}

impl TimeGuard {
    /// `floor_ms`: no real time is earlier (this firmware's build date, Unix ms).
    pub const fn new(floor_ms: i64) -> Self {
        TimeGuard { msg: Chan { reff: None, disc: false, pend: None }, pps: Chan { reff: None, disc: false, pend: None }, floor_ms, remembered_ms: None }
    }

    /// The latest time the hub remembers trusting (from its card), Unix ms.
    pub fn set_remembered(&mut self, utc_ms: Option<i64>) {
        self.remembered_ms = utc_ms;
    }

    /// Set the build-date floor, Unix ms.
    pub fn set_floor(&mut self, floor_ms: i64) {
        self.floor_ms = floor_ms;
    }

    /// The remembered time, if any.
    pub fn remembered(&self) -> Option<i64> {
        self.remembered_ms
    }

    /// The build-date floor, Unix ms.
    pub fn floor(&self) -> i64 {
        self.floor_ms
    }

    /// GPS time `utc_ms` is impossible (before the build date) or untrusted (before the
    /// remembered time): how far before, in seconds (negative), else `None`.
    pub fn floor_check(&self, utc_ms: i64) -> Option<i64> {
        let limit = match self.remembered_ms {
            Some(r) => (r - REMEMBER_SLACK_MS).max(self.floor_ms),
            None => self.floor_ms,
        };
        (utc_ms < limit).then(|| (utc_ms - limit).div_euclid(1000).min(-1))
    }

    /// Compare one epoch's sample with both references.
    pub fn check(&mut self, s: &Sample) -> Check {
        let (mut step, mut jump) = self.msg.check(s.msg(), s.at_us, TOL_MSG_US);
        if let Some((off, at)) = s.pps() {
            let (st, j) = self.pps.check(off, at, TOL_PPS_US);
            if j || (!jump && st.abs() > step.abs()) {
                step = st;
            }
            jump |= j;
        }
        Check { step_ms: step / 1000, jump }
    }

    /// A trusted epoch: anchor or steer the disciplined clocks.
    pub fn commit(&mut self, s: &Sample) {
        self.msg.commit(s.msg(), s.at_us);
        if let Some((off, at)) = s.pps() {
            self.pps.commit(off, at);
        }
    }

    /// The operator accepts GPS time as it is now: forget both references and the remembered
    /// time (the floor stays).
    pub fn reset(&mut self) {
        *self = TimeGuard::new(self.floor_ms);
    }

    /// The hub trusts its clock (a disciplined reference exists).
    pub fn disciplined(&self) -> bool {
        self.msg.disc || self.pps.disc
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UTC0: i64 = 1_800_000_000_000;

    /// A deterministic +-k generator.
    struct Rng(u64);
    impl Rng {
        fn pm(&mut self, k: i64) -> i64 {
            self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            ((self.0 >> 33) % (2 * k as u64 + 1)) as i64 - k
        }
    }

    /// A receiver and a hub. The hub's crystal gains `ppm` on true time; the receiver's
    /// time is true time plus `shift_us` (a spoofer); messages arrive 300 ms after the second
    /// with +-`jitter_us`; the PPS edge (if `pps`) marks the receiver's second with +-50 us.
    struct Bench {
        g: TimeGuard,
        rng: Rng,
        t_s: i64,
        ppm: i64,
        jitter_us: i64,
        pps: bool,
        shift_us: i64,
    }

    impl Bench {
        fn new(ppm: i64, jitter_us: i64, pps: bool) -> Self {
            Bench { g: TimeGuard::new(UTC0 - 86_400_000), rng: Rng(7), t_s: 0, ppm, jitter_us, pps, shift_us: 0 }
        }
        /// Hub time (us) at true time `true_us` after the start.
        fn hub(&self, true_us: i64) -> u64 {
            (5_000_000 + true_us + true_us / 1_000_000 * self.ppm + (true_us % 1_000_000) * self.ppm / 1_000_000) as u64
        }
        /// The next epoch's sample, with an extra arrival delay `late_us`.
        fn sample(&mut self, late_us: i64) -> Sample {
            self.t_s += 1;
            // The receiver's second `t_s` happens at true time t_s - shift.
            let edge_true = self.t_s * 1_000_000 - self.shift_us;
            let j = self.rng.pm(self.jitter_us);
            let at_us = self.hub(edge_true + 300_000 + j + late_us);
            let pj = self.rng.pm(50);
            let pps_us = self.pps.then(|| self.hub(edge_true + pj));
            Sample { utc_ms: UTC0 + self.t_s * 1000, at_us, pps_us }
        }
        /// One epoch, trusted (committed) when it passes.
        fn epoch(&mut self, late_us: i64) -> Check {
            let s = self.sample(late_us);
            let c = self.g.check(&s);
            if !c.jump {
                self.g.commit(&s);
            }
            c
        }
        fn run(&mut self, n: u32) -> u32 {
            (0..n).filter(|_| self.epoch(0).jump).count() as u32
        }
    }

    /// An honest receiver for a whole day: +-40 ms arrival jitter, a crystal at the edge of
    /// the budget either way; with and without PPS. Nothing fires.
    #[test]
    fn honest_day_never_fires() {
        for &(ppm, pps) in &[(DRIFT_PPM, false), (-DRIFT_PPM, false), (DRIFT_PPM, true), (-DRIFT_PPM, true), (0, true)] {
            let mut b = Bench::new(ppm, 40_000, pps);
            assert_eq!(b.run(86_400), 0, "ppm {ppm} pps {pps}");
            assert!(b.g.disciplined());
        }
    }

    /// One late epoch (the hub's main loop stalled 381 ms, as at an SD mount) fires for that
    /// epoch only; the monitor needs two in a row.
    #[test]
    fn one_stall_is_one_epoch() {
        let mut b = Bench::new(5, 40_000, false);
        b.run(100);
        assert!(b.epoch(381_000).jump);
        assert_eq!(b.run(100), 0);
    }

    /// Steps of 1 s, 0.5 s and 0.3 s (either way): beyond the message tolerance, fired every
    /// epoch for as long as they last, and the trusted clock does not follow.
    #[test]
    fn steps_fire_and_are_not_adopted() {
        for &step_ms in &[1_000i64, 500, 300, -300, -1_000] {
            for pps in [false, true] {
                let mut b = Bench::new(10, 40_000, pps);
                b.run(600);
                let before = b.g.msg.offset_us().unwrap();
                b.shift_us = step_ms * 1000;
                let fired = b.run(600);
                assert_eq!(fired, 600, "step {step_ms} pps {pps}");
                let after = b.g.msg.offset_us().unwrap();
                assert_eq!(before, after, "not steered while it disagrees");
                let c = b.epoch(0);
                assert!((c.step_ms.abs() - step_ms.abs()).abs() < 100, "{c:?}");
                // The receiver comes back to the truth: quiet again.
                b.shift_us = 0;
                let c = b.epoch(0);
                assert_eq!(b.run(100), 0, "step {step_ms} pps {pps} {c:?} {:?} {:?}", b.g.msg, b.g.pps);
            }
        }
    }

    /// With PPS a 20 ms step (well inside the message tolerance) is caught too.
    #[test]
    fn pps_catches_small_steps() {
        let mut b = Bench::new(10, 40_000, true);
        b.run(300);
        b.shift_us = 20_000;
        assert_eq!(b.run(10), 10);
        let mut b = Bench::new(10, 40_000, false);
        b.run(300);
        b.shift_us = 20_000;
        assert_eq!(b.run(10), 0, "without PPS 20 ms is inside the jitter");
    }

    /// A walk 200 ppm faster than the hub's crystal (0.2 ms a second) is caught once it has
    /// built up past the tolerance: 150 ms / (200 - 20) ppm = 833 s without PPS (plus the
    /// jitter's margin); 5 ms / 180 ppm = 28 s with PPS. A walk within the budget is not:
    /// the attacker's limit is DRIFT_PPM (plus the crystal's own error) per day.
    #[test]
    fn slow_walk_is_caught_within_the_bound() {
        for (pps, bound) in [(false, 1_000u32), (true, 40)] {
            let mut b = Bench::new(0, 40_000, pps);
            b.run(300);
            let mut caught = None;
            for i in 0..2_000u32 {
                b.shift_us += 200;
                if b.epoch(0).jump {
                    caught = Some(i);
                    break;
                }
            }
            let c = caught.expect("caught");
            assert!(c <= bound, "pps {pps}: {c}");
            assert!(c >= bound / 2, "pps {pps}: not before the bound's arithmetic says: {c}");
        }
        // 15 ppm (inside the 20 ppm budget, crystal perfect): never caught. 1.3 s in a day.
        let mut b = Bench::new(0, 40_000, true);
        b.run(300);
        let mut fired = 0;
        for _ in 0..86_400 {
            b.shift_us += 15;
            fired += u32::from(b.epoch(0).jump);
        }
        assert_eq!(fired, 0);
        assert!((b.shift_us - 1_296_000).abs() < 1_000);
    }

    /// Before anything is trusted, a step is reported and then followed (two agreeing
    /// samples); a single odd sample is not followed.
    #[test]
    fn untrusted_reference_follows_a_lasting_step() {
        let mut g = TimeGuard::new(0);
        let s = |t: i64, off_ms: i64| Sample { utc_ms: UTC0 + t * 1000 + off_ms, at_us: (t * 1_000_000) as u64, pps_us: None };
        assert!(!g.check(&s(1, 0)).jump);
        assert!(!g.check(&s(2, 0)).jump);
        assert!(g.check(&s(3, 900)).jump, "odd one");
        assert!(!g.check(&s(4, 0)).jump, "not followed");
        assert!(g.check(&s(5, 3_600_000)).jump);
        assert!(g.check(&s(6, 3_600_000)).jump, "reported twice");
        assert!(!g.check(&s(7, 3_600_000)).jump, "then followed");
        assert!(!g.disciplined());
    }

    #[test]
    fn floor_and_remembered_time() {
        let mut g = TimeGuard::new(UTC0);
        assert_eq!(g.floor_check(UTC0 - 3_000), Some(-3));
        assert_eq!(g.floor_check(UTC0 + 1), None);
        g.set_remembered(Some(UTC0 + 86_400_000));
        assert_eq!(g.floor_check(UTC0 + 1), Some(-86_398), "before the remembered time");
        assert_eq!(g.floor_check(UTC0 + 86_399_000), None, "within the slack");
        g.set_remembered(Some(UTC0 - 86_400_000));
        assert_eq!(g.floor_check(UTC0 - 1_000), Some(-1), "the build date still holds");
        g.reset();
        assert_eq!(g.remembered(), None);
        assert_eq!(g.floor(), UTC0);
    }

    #[test]
    fn allowance_is_the_drift_budget() {
        assert_eq!(allowance(1_000_000), DRIFT_PPM);
        assert_eq!(allowance(3_600_000_000), 3_600 * DRIFT_PPM);
        assert_eq!(allowance(500_000), DRIFT_PPM / 2);
        assert_eq!(allowance(999_000), DRIFT_PPM, "rounded up");
        assert!(allowance(u64::MAX) > 0);
    }
}
