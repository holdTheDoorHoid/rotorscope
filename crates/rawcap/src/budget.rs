//! How much of the capture loop the DroneID decoder may take.
//!
//! On node-sdr the ring is stopped and nothing is probed while a window is
//! decoded, and a full decode of a DroneID-shaped burst holds the loop about
//! 150 ms (BRINGUP-RESULTS.md, `decode_ms` 144-158). Any 450-800 µs energy
//! run passes the trigger's length gate, so one short pulse every ~150 ms
//! used to keep the node blind nearly all the time. The decoder's own cheap
//! gates (`droneid::rx::Config::cfo_window`, `turbo_gate`, and its CP and
//! Zadoff-Chu checks) make most such pulses cheap; this budget bounds the
//! rest, which nothing before decoding can tell from a real burst:
//!
//! - at most [`Config::per_dwell`] *full* decodes (a burst that passed the
//!   decoder's sync checks and was demodulated) per dwell, and
//!   [`Config::per_sec`] in any second;
//! - a credit that earns [`Config::busy_permille`] of wall time and pays for
//!   every decode's measured time, cheap ones included, holding at most
//!   [`Config::credit_us`]: over any stretch `T` the decoder holds the loop
//!   at most `busy_permille * T + credit_us / (1 - busy_permille)`.
//!
//! A window the budget refuses is counted ([`Budget::skipped`]) and dropped
//! undecoded; the trigger keeps running. Under a flood of DroneID-shaped
//! bursts real ones are then decoded first-come within the budget: the
//! budget trades decode coverage for capture time, it cannot tell bursts
//! apart. The per-dwell pressure ([`Budget::dwell`]) goes to the jamming
//! watch (`jamwatch::ProbeStats::decode_pressure`).
//!
//! Pure logic, explicit time in µs (the node passes its clock, the bench's
//! `hil` scenarios a simulated one).

/// Budget settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// Full decodes per dwell. A 700 ms DroneID dwell holds one burst per
    /// drone on that channel (one every ~640 ms).
    pub per_dwell: u8,
    /// Full decodes in any one second (at most [`MAX_PER_SEC`]).
    pub per_sec: u8,
    /// Share of wall time the decoder may hold the loop, per mille.
    pub busy_permille: u32,
    /// Decode time the credit may save up, µs (about three full decodes).
    pub credit_us: u32,
}

/// Largest `per_sec` (the window of recent full decodes kept).
pub const MAX_PER_SEC: usize = 8;

impl Default for Config {
    fn default() -> Self {
        Config { per_dwell: 3, per_sec: 4, busy_permille: 400, credit_us: 450_000 }
    }
}

/// One dwell's decoder pressure, for the jamming watch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pressure {
    /// Windows refused by the budget.
    pub skipped: u32,
    /// Decodes run, and how many of them were full.
    pub decodes: u32,
    pub full: u32,
    /// Bursts that decoded with a valid CRC.
    pub valid: u32,
    /// Time the decoder held the loop, and the dwell's length so far, µs.
    pub busy_us: u32,
    pub span_us: u32,
}

impl Pressure {
    /// Decoder time as a percentage of the dwell.
    pub fn busy_pct(&self) -> u8 {
        if self.span_us == 0 { 0 } else { (self.busy_us as u64 * 100 / self.span_us as u64).min(100) as u8 }
    }
}

#[derive(Clone, Debug)]
pub struct Budget {
    pub cfg: Config,
    credit_us: i64,
    last_us: u64,
    /// Start times of the most recent full decodes (a ring).
    recent: [u64; MAX_PER_SEC],
    recent_at: usize,
    dwell_start_us: u64,
    dwell: Pressure,
    /// Since boot: decodes run, full ones, windows refused, decoder time.
    pub decodes: u32,
    pub full: u32,
    pub skipped: u32,
    pub busy_us: u64,
}

impl Budget {
    pub const fn new(cfg: Config) -> Self {
        Budget {
            cfg,
            credit_us: cfg.credit_us as i64,
            last_us: 0,
            recent: [0; MAX_PER_SEC],
            recent_at: 0,
            dwell_start_us: 0,
            dwell: Pressure { skipped: 0, decodes: 0, full: 0, valid: 0, busy_us: 0, span_us: 0 },
            decodes: 0,
            full: 0,
            skipped: 0,
            busy_us: 0,
        }
    }

    fn credit_at(&self, t: u64) -> i64 {
        let earned = (t.saturating_sub(self.last_us) * self.cfg.busy_permille as u64 / 1000) as i64;
        (self.credit_us + earned).min(self.cfg.credit_us as i64)
    }

    fn full_in_last_second(&self, now_us: u64) -> usize {
        self.recent.iter().filter(|&&t| t != 0 && now_us.saturating_sub(t) < 1_000_000).count()
    }

    /// A new dwell (retune) starts at `now_us`.
    pub fn begin_dwell(&mut self, now_us: u64) {
        self.dwell_start_us = now_us;
        self.dwell = Pressure::default();
    }

    /// May a window be decoded now? A refusal is counted (the caller drops
    /// the window).
    pub fn allow(&mut self, now_us: u64) -> bool {
        let per_sec = (self.cfg.per_sec as usize).min(MAX_PER_SEC);
        let ok = self.credit_at(now_us) > 0
            && self.dwell.full < self.cfg.per_dwell as u32
            && self.full_in_last_second(now_us) < per_sec;
        if !ok {
            self.skipped = self.skipped.wrapping_add(1);
            self.dwell.skipped += 1;
        }
        ok
    }

    /// A decode ran from `start_us` to `end_us`; `full`: it demodulated at
    /// least one burst; `valid`: bursts with a valid CRC.
    pub fn done(&mut self, start_us: u64, end_us: u64, full: bool, valid: u32) {
        let busy = end_us.saturating_sub(start_us);
        self.credit_us = self.credit_at(end_us) - busy as i64;
        self.last_us = end_us;
        self.decodes = self.decodes.wrapping_add(1);
        self.busy_us += busy;
        self.dwell.decodes += 1;
        self.dwell.busy_us = self.dwell.busy_us.saturating_add(busy as u32);
        self.dwell.valid += valid;
        if full {
            self.full = self.full.wrapping_add(1);
            self.dwell.full += 1;
            // Never 0 (the ring's "empty" marker).
            self.recent[self.recent_at] = start_us.max(1);
            self.recent_at = (self.recent_at + 1) % MAX_PER_SEC;
        }
    }

    /// The current dwell's pressure as of `now_us`.
    pub fn dwell(&self, now_us: u64) -> Pressure {
        Pressure { span_us: now_us.saturating_sub(self.dwell_start_us).min(u32::MAX as u64) as u32, ..self.dwell }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A train of DroneID-shaped pulses every `gap_us`, each costing a full
    /// `decode_us` decode if taken; returns (decoder share per mille,
    /// pulses decoded, pulses skipped, pulses that arrived while decoding).
    fn train(b: &mut Budget, gap_us: u64, decode_us: u64, secs: u64, dwell_us: u64) -> (u64, u32, u32, u32) {
        let t0 = 5_000_000u64;
        let end = t0 + secs * 1_000_000;
        let (mut busy_until, mut busy, mut decoded, mut skipped, mut blind) = (0u64, 0u64, 0, 0, 0);
        let mut next_dwell = t0;
        let mut t = t0;
        while t < end {
            while next_dwell <= t {
                b.begin_dwell(next_dwell);
                next_dwell += dwell_us;
            }
            if t < busy_until {
                blind += 1;
            } else if b.allow(t) {
                b.done(t, t + decode_us, true, 0);
                busy_until = t + decode_us;
                busy += decode_us;
                decoded += 1;
            } else {
                skipped += 1;
            }
            t += gap_us;
        }
        (busy * 1000 / (end - t0), decoded, skipped, blind)
    }

    #[test]
    fn a_pulse_train_cannot_hold_the_loop() {
        // One DroneID-shaped pulse every 150 ms against a 150 ms decode:
        // without a budget the loop decodes every one and is never free
        // (1000 per mille). With it, the decoder holds at most its share.
        let mut b = Budget::new(Config::default());
        let (share, decoded, skipped, _) = train(&mut b, 150_000, 150_000, 60, 700_000);
        assert!(share <= 400 + 20, "decoder held {share} per mille");
        assert!(share >= 300, "{share}");
        assert!(decoded > 100 && skipped > 150, "{decoded} {skipped}");
        assert_eq!(b.skipped, skipped);
        // Faster pulses (every 20 ms; still one 450-800 µs pulse): the same.
        let mut b = Budget::new(Config::default());
        let (share, _, skipped, _) = train(&mut b, 20_000, 150_000, 60, 700_000);
        assert!(share <= 420 && skipped > 1000, "{share} {skipped}");
    }

    #[test]
    fn caps_per_dwell_and_per_second() {
        // Short full decodes (10 ms) back to back: the counts bind first.
        let mut b = Budget::new(Config::default());
        b.begin_dwell(0);
        let mut n = 0;
        for k in 0..20u64 {
            let t = 1_000_000 + k * 20_000;
            if b.allow(t) {
                b.done(t, t + 10_000, true, 0);
                n += 1;
            }
        }
        assert_eq!(n, 3, "per dwell");
        // A new dwell: one more until four fell in the last second.
        b.begin_dwell(1_500_000);
        assert!(b.allow(1_500_000));
        b.done(1_500_000, 1_510_000, true, 0);
        assert!(!b.allow(1_520_000), "four in the last second");
        b.begin_dwell(2_100_000);
        assert!(b.allow(2_100_000), "a second later");
        let p = b.dwell(2_100_000);
        assert_eq!((p.skipped, p.full), (0, 0));
    }

    #[test]
    fn cheap_rejections_do_not_use_up_the_budget() {
        // Busy 5 GHz Wi-Fi: ~8 DroneID-length frames a second that the
        // decoder's sync checks reject in a few ms each. None is refused,
        // and a real burst in the same dwell is still decoded.
        let mut b = Budget::new(Config::default());
        let mut t = 1_000_000u64;
        for k in 0..800 {
            if k % 6 == 0 {
                b.begin_dwell(t);
            }
            assert!(b.allow(t), "frame {k}");
            b.done(t, t + 4_000, false, 0);
            t += 125_000;
        }
        assert!(b.allow(t));
        b.done(t, t + 150_000, true, 1);
        assert_eq!((b.skipped, b.full), (0, 1));
        assert_eq!(b.dwell(t + 150_000).valid, 1);
    }

    #[test]
    fn after_a_quiet_spell_the_credit_covers_a_few_full_decodes() {
        let mut b = Budget::new(Config::default());
        b.begin_dwell(10_000_000);
        let mut t = 10_000_000u64;
        let mut n = 0;
        while b.allow(t) {
            b.done(t, t + 150_000, n < 2, 0);
            t += 150_000;
            n += 1;
        }
        // 450 ms of credit and 40 % earned meanwhile: five back to back (a
        // decode may start on any credit left and overdraw it).
        assert_eq!(n, 5);
        let p = b.dwell(t);
        assert_eq!((p.skipped, p.decodes, p.busy_pct()), (1, 5, 100));
        // Then it waits for the credit to come back (~150 ms at 40 %).
        assert!(!b.allow(t + 100_000) && b.allow(t + 200_000));
    }
}
