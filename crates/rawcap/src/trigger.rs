//! The cheap energy trigger on Path B's ring, and burst measurements on a
//! frozen window.
//!
//! The trigger never looks at every sample: it reads one short *probe* per
//! *block* of the stream (by default 256 of every 2048 bytes: 6.4 µs every
//! 51.2 µs), so a 0.57-0.64 ms DroneID burst covers about a dozen probes
//! while the CPU reads one byte in eight. The noise floor follows the probes
//! with a fast-down, slow-up average, so a burst barely moves it and a
//! quieter band is learnt within a few blocks.
//!
//! A *rise* is `min_hot` consecutive probes above `floor * ratio`. The trigger
//! then follows the hot run (bridging up to `bridge` cold probes) until it
//! ends, and only reports it if its probe-resolution length could be a
//! DroneID burst (`min_us..=max_us`, give or take one block). Wi-Fi frames
//! (80-300 µs) and long transmissions are dropped right here, without
//! stopping the ring: on 2026-11-12, at gain 83 in a busy 5 GHz band, every
//! frame used to freeze the ring for a full-rate look, 84 a second, and the
//! loop fell behind (42 blocks/s skipped). A reported rise is then measured
//! on the frozen window at full rate ([`extent`]) and gated again exactly.
//!
//! The ZC check that confirms "DJI" is Phase 10c; this module only finds
//! energy.

/// Probe/block geometry and thresholds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// Stream bytes per block (one probe per block).
    pub block: u32,
    /// Bytes read per probe, from the start of each block.
    pub probe: u32,
    /// Rise threshold as a power ratio over the floor, x256 (1024 = 6 dB).
    pub ratio_q8: u32,
    /// Consecutive hot probes for a rise.
    pub min_hot: u8,
    /// Floor follows a lower probe with weight 1/2^down...
    pub down_shift: u8,
    /// ...and a higher (non-hot) one with weight 1/2^up.
    pub up_shift: u8,
    /// Shortest and longest burst accepted for decoding (µs, measured on the
    /// frozen window).
    pub min_us: u32,
    pub max_us: u32,
    /// Block duration in ns (2048 bytes at 40 MB/s = 51.2 µs), for the
    /// probe-resolution length gate.
    pub block_ns: u32,
    /// Cold probes tolerated inside one hot run.
    pub bridge: u8,
    /// Gate on the run's length before reporting (false: report every rise
    /// as soon as it has `min_hot` hot probes, the old behaviour).
    pub gate: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            block: 2048,
            probe: 256,
            ratio_q8: 1024,
            min_hot: 2,
            down_shift: 3,
            up_shift: 9,
            // DroneID: 8 symbols ~572 µs, 9 symbols ~643 µs (RESEARCH.md).
            min_us: 450,
            max_us: 800,
            block_ns: 51_200,
            bridge: 1,
            gate: true,
        }
    }
}

/// A rise: where the first hot probe's block starts, and the levels seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rise {
    pub pos: u64,
    /// Probe power sum of the hot probe and the floor it beat.
    pub power: u32,
    pub floor: u32,
    /// Blocks from the first to the last hot probe, inclusive (0 when
    /// reported ungated, before the run ended).
    pub probes: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct Trigger {
    pub cfg: Config,
    /// Floor in probe-sum units x 256 (0 = not yet learnt).
    floor_q8: u64,
    /// Hot probes in the current run, the last one's position, and cold
    /// probes since it.
    run_hot: u32,
    last_hot: u64,
    cold: u8,
    first_hot: u64,
    first_power: u32,
    first_floor: u32,
    /// Probes fed, rises found (runs reaching `min_hot`), and rises whose
    /// length ruled them out before any full-rate look.
    pub probes: u32,
    pub rises: u32,
    pub gated: u32,
}

impl Trigger {
    pub const fn new(cfg: Config) -> Self {
        Trigger {
            cfg,
            floor_q8: 0,
            run_hot: 0,
            last_hot: 0,
            cold: 0,
            first_hot: 0,
            first_power: 0,
            first_floor: 0,
            probes: 0,
            rises: 0,
            gated: 0,
        }
    }

    /// Current floor, probe-sum units.
    pub fn floor(&self) -> u32 {
        (self.floor_q8 >> 8) as u32
    }

    /// Forget the floor (after a retune or gain change).
    pub fn reset(&mut self) {
        self.floor_q8 = 0;
        self.run_hot = 0;
        self.cold = 0;
    }

    /// Could a hot run spanning `probes` blocks be a burst of `min_us..=max_us`?
    /// The true length is within one block either side of the probe span.
    pub fn length_ok(&self, probes: u32) -> bool {
        let block_us = self.cfg.block_ns.div_ceil(1000);
        let span_us = probes.saturating_mul(self.cfg.block_ns) / 1000;
        span_us + block_us >= self.cfg.min_us && span_us <= self.cfg.max_us + block_us
    }

    /// The current run ended: report it if it rose and its length fits.
    fn end_run(&mut self) -> Option<Rise> {
        let rose = self.run_hot >= self.cfg.min_hot as u32;
        let probes = ((self.last_hot - self.first_hot) / self.cfg.block.max(1) as u64) as u32 + 1;
        self.run_hot = 0;
        self.cold = 0;
        if !rose || !self.cfg.gate {
            return None;
        }
        if self.length_ok(probes) {
            Some(Rise { pos: self.first_hot, power: self.first_power, floor: self.first_floor, probes })
        } else {
            self.gated = self.gated.wrapping_add(1);
            None
        }
    }

    /// Feed one probe: `pos` is the stream position of its block, `power`
    /// its [`crate::iq::power_sum`].
    pub fn feed(&mut self, pos: u64, power: u32) -> Option<Rise> {
        self.probes = self.probes.wrapping_add(1);
        let p = (power as u64) << 8;
        if self.floor_q8 == 0 {
            self.floor_q8 = p.max(1);
            return None;
        }
        let hot = p > (self.floor_q8 >> 8) * self.cfg.ratio_q8 as u64;
        if hot {
            if self.run_hot == 0 {
                self.first_hot = pos;
                self.first_power = power;
                self.first_floor = self.floor();
            }
            self.run_hot += 1;
            self.last_hot = pos;
            self.cold = 0;
            if self.run_hot == self.cfg.min_hot as u32 {
                self.rises = self.rises.wrapping_add(1);
                if !self.cfg.gate {
                    return Some(Rise { pos: self.first_hot, power: self.first_power, floor: self.first_floor, probes: 0 });
                }
            }
            return None;
        }
        let mut out = None;
        if self.run_hot > 0 {
            self.cold += 1;
            if self.cold > self.cfg.bridge {
                out = self.end_run();
            }
        }
        if p < self.floor_q8 {
            self.floor_q8 -= (self.floor_q8 - p) >> self.cfg.down_shift;
        } else {
            self.floor_q8 += (p - self.floor_q8) >> self.cfg.up_shift;
        }
        self.floor_q8 = self.floor_q8.max(1);
        out
    }
}

/// The longest run of consecutive segments whose power (each `seg` samples
/// long, `powers` in the same units) is above `threshold`: `(first segment,
/// segments)`.
pub fn extent(powers: impl Iterator<Item = u32>, threshold: u32) -> (usize, usize) {
    let (mut best, mut best_at) = (0, 0);
    let (mut run, mut run_at) = (0, 0);
    for (k, p) in powers.enumerate() {
        if p > threshold {
            if run == 0 {
                run_at = k;
            }
            run += 1;
            if run > best {
                best = run;
                best_at = run_at;
            }
        } else {
            run = 0;
        }
    }
    (best_at, best)
}

/// `10 log10(x)` in tenths of a dB, integer only (about +-0.05 dB).
pub fn db10_x10(x: u64) -> i32 {
    if x == 0 {
        return -1000;
    }
    // log2(x) = e + log2(m), m in [1, 2) from the top 16 bits below the MSB.
    let e = 63 - x.leading_zeros() as i32;
    let m = if e >= 16 { (x >> (e - 16)) as u32 } else { (x << (16 - e)) as u32 } & 0xffff;
    // log2(1 + f) ~ f * (1.3466 - 0.3466 f) for f in [0, 1): max error 0.005.
    let f = m as i64; // Q16
    let l2_q16 = (f * (88_253 - ((22_717 * f) >> 16))) >> 16;
    let log2_q16 = ((e as i64) << 16) + l2_q16;
    // 10 log10(2) = 3.0103 -> x10 = 30.103; Q16: 30.103 * 65536 = 1_972_831.
    ((log2_q16 * 30_103 + (1 << 15) * 1000) / (1000 << 16)) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(seed: &mut u64) -> u32 {
        *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        // 1000 +- 12 %.
        880 + ((*seed >> 33) % 240) as u32
    }

    #[test]
    fn learns_floor_and_rises_once_per_burst() {
        let mut t = Trigger::new(Config::default());
        let mut s = 1;
        let mut rises = vec![];
        for k in 0..2000u64 {
            let pos = k * 2048;
            // A 12-probe burst 10 dB up at probe 1000, another at 1500.
            let burst = (1000..1012).contains(&k) || (1500..1512).contains(&k);
            let p = if burst { 10 * noise(&mut s) } else { noise(&mut s) };
            if let Some(r) = t.feed(pos, p) {
                rises.push(r);
            }
        }
        assert_eq!(rises.len(), 2);
        assert_eq!(rises[0].pos, 1000 * 2048);
        assert_eq!(rises[1].pos, 1500 * 2048);
        assert!((850..1150).contains(&rises[0].floor), "floor {}", rises[0].floor);
        assert_eq!(t.rises, 2);
    }

    /// Bursts of a given probe length at 10 dB over noise; what comes out.
    fn run_lengths(cfg: Config, lens: &[u64]) -> (Vec<Rise>, Trigger) {
        let mut t = Trigger::new(cfg);
        let mut s = 7;
        let mut out = vec![];
        let mut k = 0u64;
        for _ in 0..300 {
            t.feed(k * 2048, noise(&mut s));
            k += 1;
        }
        for &n in lens {
            for _ in 0..n {
                if let Some(r) = t.feed(k * 2048, 10 * noise(&mut s)) {
                    out.push(r);
                }
                k += 1;
            }
            for _ in 0..40 {
                if let Some(r) = t.feed(k * 2048, noise(&mut s)) {
                    out.push(r);
                }
                k += 1;
            }
        }
        (out, t)
    }

    #[test]
    fn length_gate_keeps_droneid_drops_wifi() {
        // 51.2 µs blocks: Wi-Fi frames of 2, 4, 6 probes (100-300 µs), DroneID
        // 11 and 12 probes (~570 / ~640 µs), a long 40-probe transmission.
        let (rises, t) = run_lengths(Config::default(), &[2, 4, 6, 11, 12, 40]);
        let spans: Vec<u32> = rises.iter().map(|r| r.probes).collect();
        assert_eq!(spans, vec![11, 12]);
        assert_eq!(t.rises, 6);
        assert_eq!(t.gated, 4);
    }

    #[test]
    fn one_cold_probe_inside_a_burst_is_bridged() {
        let mut t = Trigger::new(Config::default());
        let mut s = 9;
        for k in 0..300u64 {
            t.feed(k * 2048, noise(&mut s));
        }
        let mut got = vec![];
        for k in 300..360u64 {
            let hot = (300..312).contains(&k) && k != 306;
            if let Some(r) = t.feed(k * 2048, if hot { 10 * noise(&mut s) } else { noise(&mut s) }) {
                got.push(r);
            }
        }
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].pos, 300 * 2048);
        assert_eq!(got[0].probes, 12);
    }

    #[test]
    fn ungated_reports_at_min_hot() {
        let cfg = Config { gate: false, ..Config::default() };
        let (rises, t) = run_lengths(cfg, &[2, 4, 40]);
        assert_eq!(rises.len(), 3);
        assert!(rises.iter().all(|r| r.probes == 0));
        assert_eq!(t.gated, 0);
    }

    #[test]
    fn single_hot_probe_is_ignored() {
        let mut t = Trigger::new(Config::default());
        let mut s = 2;
        for k in 0..500u64 {
            let p = if k == 300 { 50 * noise(&mut s) } else { noise(&mut s) };
            assert!(t.feed(k * 2048, p).is_none());
        }
    }

    #[test]
    fn floor_tracks_a_level_change() {
        let mut t = Trigger::new(Config::default());
        for k in 0..100u64 {
            t.feed(k, 10_000);
        }
        // Quieter band: the floor comes down fast.
        for k in 0..50u64 {
            t.feed(k, 1000);
        }
        assert!(t.floor() < 1100, "{}", t.floor());
        // Slightly louder (not hot): it creeps up.
        for k in 0..3000u64 {
            t.feed(k, 2000);
        }
        assert!(t.floor() > 1800, "{}", t.floor());
    }

    #[test]
    fn extent_finds_longest_run() {
        let p = [1, 9, 9, 1, 9, 9, 9, 9, 1, 9];
        assert_eq!(extent(p.into_iter(), 5), (4, 4));
        assert_eq!(extent([1u32, 1].into_iter(), 5), (0, 0));
    }

    #[test]
    fn db10_is_close() {
        for (x, want) in [(1u64, 0), (2, 30), (10, 100), (100, 200), (1000, 300), (12345, 409), (1 << 40, 1204)] {
            let got = db10_x10(x);
            assert!((got - want).abs() <= 1, "{x}: {got} vs {want}");
        }
        for x in 1..5000u64 {
            let exact = (10.0 * (x as f64).log10() * 10.0).round() as i32;
            assert!((db10_x10(x) - exact).abs() <= 1, "{x}");
        }
    }
}
