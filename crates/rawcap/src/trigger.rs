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
//! A *rise* is `min_hot` consecutive probes above `floor * ratio`. The
//! firmware then waits until the stream has run a full capture window past
//! the rise, freezes the ring, and measures the burst on the frozen window at
//! full rate ([`extent`]) before handing it to the decoder. Duration gating
//! there (DroneID: 0.57-0.64 ms) rejects most Wi-Fi frames without a decode.
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
}

#[derive(Clone, Copy, Debug)]
pub struct Trigger {
    pub cfg: Config,
    /// Floor in probe-sum units x 256 (0 = not yet learnt).
    floor_q8: u64,
    hot: u8,
    first_hot: u64,
    first_power: u32,
    /// Probes fed and rises found.
    pub probes: u32,
    pub rises: u32,
}

impl Trigger {
    pub const fn new(cfg: Config) -> Self {
        Trigger { cfg, floor_q8: 0, hot: 0, first_hot: 0, first_power: 0, probes: 0, rises: 0 }
    }

    /// Current floor, probe-sum units.
    pub fn floor(&self) -> u32 {
        (self.floor_q8 >> 8) as u32
    }

    /// Forget the floor (after a retune or gain change).
    pub fn reset(&mut self) {
        self.floor_q8 = 0;
        self.hot = 0;
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
            if self.hot == 0 {
                self.first_hot = pos;
                self.first_power = power;
            }
            self.hot = self.hot.saturating_add(1);
            if self.hot == self.cfg.min_hot {
                self.rises = self.rises.wrapping_add(1);
                return Some(Rise { pos: self.first_hot, power: self.first_power, floor: self.floor() });
            }
            return None;
        }
        self.hot = 0;
        if p < self.floor_q8 {
            self.floor_q8 -= (self.floor_q8 - p) >> self.cfg.down_shift;
        } else {
            self.floor_q8 += (p - self.floor_q8) >> self.cfg.up_shift;
        }
        self.floor_q8 = self.floor_q8.max(1);
        None
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
