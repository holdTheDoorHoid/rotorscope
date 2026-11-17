//! Energy maps and hop-pattern classification.
//!
//! Two inputs, both plain RSSI (no demodulation, nothing decoded):
//!
//! 1. **Sweeps** ([`EnergyMap`]): `(freq, rssi, time)` samples from
//!    [`crate::scan::energy_plan`] steps, summarised per window (a few
//!    seconds) into an [`EnergyReport`]: which bins are busy, whether the
//!    busy bins form a hop set (how wide, what channel spacing), and how
//!    alike their signal strengths are.
//! 2. **Dwells** ([`BurstTimer`]): fast RSSI samples on one frequency,
//!    turned into burst lengths and a packet interval.
//!
//! [`classify`] matches both against [`crate::signatures::TABLE`].
//!
//! # Not reading meters and gateways as drones
//!
//! 902-928 MHz in Philadelphia is busy: AMI electricity / gas / water meter
//! networks (themselves frequency hoppers), LoRaWAN gateways and sensors,
//! and assorted ISM devices. Four things keep them out:
//!
//! - **A noise floor per bin** (fast down, slow up), so a hit is "this many
//!   dB above what this bin usually sees", not an absolute level.
//! - **A background occupancy per bin**, learned over minutes (rises with a
//!   time constant of ~64 windows, falls with ~8). A meter mesh that is
//!   always there becomes background; only occupancy *above* background
//!   counts. Bins that are busy most of the time (steady carriers, a
//!   gateway's downlink channel) are reported as `steady` and never join a
//!   hop set. Cost: a control link that stays on air for many minutes fades
//!   into the background too; the packet recognisers do not have that
//!   problem, and the hub keeps the earlier detection.
//! - **One emitter, one signal level.** A drone link's hops all come from
//!   the same transmitter, so their levels agree within a few dB. A meter
//!   mesh is hundreds of transmitters at different distances: the spread of
//!   levels across the busy channels is wide. A hop set with a spread above
//!   [`MAX_HOPPER_SPREAD_DB`] is reported as background, not as a hopper.
//! - **Timing.** Control links send on a fixed packet interval (ELRS
//!   4 ms ... 40 ms, AFHDS 3.85 ms); a dwell on one of their channels shows
//!   bursts at whole multiples of it. Meters and sensors send when they
//!   have something to say. [`BurstTimer`] scores periodicity, and
//!   [`classify`] only names a candidate with high confidence when the
//!   timing fits.

use crate::signatures::{Signature, Source, TABLE};
use crate::{Region, System};

/// Max level spread (dB) among a hop set's channels for it to count as one
/// emitter.
pub const MAX_HOPPER_SPREAD_DB: i16 = 8;
/// A sample this far above the bin's floor is a hit.
pub const HIT_MARGIN_DB: i16 = 10;
/// Max steady emitters / hop channels kept per report.
pub const MAX_STEADY: usize = 8;

/// Per-bin state.
#[derive(Debug, Clone, Copy, Default)]
struct Bin {
    /// Noise floor, dBm x 4.
    floor_q2: i32,
    init: bool,
    /// Background occupancy, 0..=65535 (fraction of samples that are hits).
    bg_occ: u32,
    samples: u16,
    hits: u16,
    /// Sum of (rssi - floor) over hits, dB.
    excess_sum: i32,
    rssi_sum: i32,
}

/// A frequency-binned energy map.
#[derive(Debug, Clone)]
pub struct EnergyMap<const N: usize> {
    pub start_hz: u32,
    pub bin_hz: u32,
    bins: [Bin; N],
    windows: u32,
}

/// A persistent emitter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Steady {
    pub freq_hz: u32,
    pub rssi_dbm: i16,
}

/// A set of busy channels that looks like one hopping transmitter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct HopSet {
    pub low_hz: u32,
    pub high_hz: u32,
    /// Distinct channels seen busy in this window.
    pub channels_seen: u16,
    /// Channel spacing that fits the busy channels, when one does.
    pub spacing_hz: Option<u32>,
    /// Width of one channel's energy (contiguous busy bins).
    pub occupied_bw_hz: u32,
    pub rssi_dbm: i16,
    /// Max - min level (above floor) across channels, dB.
    pub spread_db: i16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Activity {
    Quiet,
    /// Only steady emitters.
    Steady,
    /// Hopping energy that does not look like one transmitter (meters).
    Background,
    /// One hopping transmitter.
    Hopper(HopSet),
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct EnergyReport {
    pub activity: Activity,
    pub steady: heapless::Vec<Steady, MAX_STEADY>,
    /// Windows seen so far (the background needs ~20 to settle).
    pub windows: u32,
}

/// Candidate channel spacings tried when fitting a grid, largest first.
const SPACINGS: [u32; 9] = [
    1_000_000, 600_000, 525_000, 500_000, 333_000, 300_000, 250_000, 200_000, 100_000,
];

impl<const N: usize> EnergyMap<N> {
    pub const fn new(start_hz: u32, bin_hz: u32) -> Self {
        Self {
            start_hz,
            bin_hz,
            bins: [Bin {
                floor_q2: 0,
                init: false,
                bg_occ: 0,
                samples: 0,
                hits: 0,
                excess_sum: 0,
                rssi_sum: 0,
            }; N],
            windows: 0,
        }
    }

    /// A map covering `region`'s band in `bin_hz` bins (bins past `N` are
    /// ignored).
    pub fn for_region(region: Region, bin_hz: u32) -> Self {
        Self::new(crate::scan::band(region).0, bin_hz)
    }

    fn bin_of(&self, freq_hz: u32) -> Option<usize> {
        let i = (freq_hz.checked_sub(self.start_hz)? / self.bin_hz) as usize;
        (i < N).then_some(i)
    }

    fn centre(&self, i: usize) -> u32 {
        self.start_hz + i as u32 * self.bin_hz + self.bin_hz / 2
    }

    /// Add one RSSI sample.
    pub fn add(&mut self, freq_hz: u32, rssi_dbm: i16) {
        let Some(i) = self.bin_of(freq_hz) else {
            return;
        };
        let b = &mut self.bins[i];
        let r4 = rssi_dbm as i32 * 4;
        if !b.init {
            b.floor_q2 = r4;
            b.init = true;
        }
        let excess = (r4 - b.floor_q2) / 4;
        b.samples = b.samples.saturating_add(1);
        if excess >= HIT_MARGIN_DB as i32 {
            b.hits = b.hits.saturating_add(1);
            b.excess_sum += excess;
            b.rssi_sum += rssi_dbm as i32;
        } else if r4 < b.floor_q2 {
            // Fast down.
            b.floor_q2 -= (b.floor_q2 - r4 + 1) / 2;
        } else {
            // Slow up (only from non-hit samples).
            b.floor_q2 += (r4 - b.floor_q2 + 7) / 8;
        }
    }

    /// Close the current window: summarise, update the background, reset.
    pub fn end_window(&mut self) -> EnergyReport {
        self.windows += 1;
        let mut steady = heapless::Vec::new();
        // Per-bin excess occupancy over background.
        let mut busy = [false; N];
        let mut level = [0i16; N];
        let mut rssi = [0i16; N];
        for (i, b) in self.bins.iter().enumerate() {
            if b.samples == 0 {
                continue;
            }
            let occ = (b.hits as u32 * 65535) / b.samples as u32;
            if b.hits > 0 {
                level[i] = (b.excess_sum / b.hits as i32) as i16;
                rssi[i] = (b.rssi_sum / b.hits as i32) as i16;
            }
            if occ > 52_000 && b.bg_occ > 32_000 {
                let _ = steady.push(Steady {
                    freq_hz: self.centre(i),
                    rssi_dbm: rssi[i],
                });
                continue;
            }
            if occ > 52_000 {
                // Busy now but not yet background: still steady-looking.
                let _ = steady.push(Steady {
                    freq_hz: self.centre(i),
                    rssi_dbm: rssi[i],
                });
                continue;
            }
            // Excess over background: at least one hit and above the
            // background by a margin (background ~0 for an empty bin).
            busy[i] = b.hits > 0 && occ > 2 * b.bg_occ + 500;
        }
        // A bin busy since power-up starts with its floor at the signal:
        // keep every floor within 6 dB of the band's median floor.
        let mut floors: heapless::Vec<i32, N> = self
            .bins
            .iter()
            .filter(|b| b.init)
            .map(|b| b.floor_q2)
            .collect();
        if !floors.is_empty() {
            floors.sort_unstable();
            let cap = floors[floors.len() / 2] + 6 * 4;
            for b in self.bins.iter_mut() {
                b.floor_q2 = b.floor_q2.min(cap);
            }
        }
        // Update background and reset counters.
        for b in self.bins.iter_mut() {
            if b.samples > 0 {
                let occ = (b.hits as u32 * 65535) / b.samples as u32;
                b.bg_occ = if occ > b.bg_occ {
                    b.bg_occ + (occ - b.bg_occ) / 64
                } else {
                    b.bg_occ - (b.bg_occ - occ) / 8
                };
            }
            b.samples = 0;
            b.hits = 0;
            b.excess_sum = 0;
            b.rssi_sum = 0;
        }
        // Channels: local level peaks among busy bins (adjacent channels can
        // touch when the spacing is close to the signal width, so runs of
        // busy bins are split at their level peaks). Width: run length /
        // peaks in the run.
        let mut chans: heapless::Vec<(u32, i16, i16, u32), 128> = heapless::Vec::new(); // (centre, level, rssi, width)
        let mut i = 0;
        while i < N {
            if !busy[i] {
                i += 1;
                continue;
            }
            let start = i;
            while i < N && busy[i] {
                i += 1;
            }
            let run = start..i;
            let first_peak = chans.len();
            for k in run.clone() {
                let left = if k > start { level[k - 1] } else { i16::MIN };
                let right = if k + 1 < i { level[k + 1] } else { i16::MIN };
                if level[k] > left && level[k] >= right {
                    let _ = chans.push((self.centre(k), level[k], rssi[k], 0));
                }
            }
            let peaks = (chans.len() - first_peak).max(1) as u32;
            let width = run.len() as u32 * self.bin_hz / peaks;
            for c in chans[first_peak..].iter_mut() {
                c.3 = width;
            }
        }
        let activity = if chans.len() >= 4 {
            let low = chans.iter().map(|c| c.0).min().unwrap_or(0);
            let high = chans.iter().map(|c| c.0).max().unwrap_or(0);
            let lmin = chans.iter().map(|c| c.1).min().unwrap_or(0);
            let lmax = chans.iter().map(|c| c.1).max().unwrap_or(0);
            let mut widths: heapless::Vec<u32, 128> = chans.iter().map(|c| c.3).collect();
            widths.sort_unstable();
            let spread = lmax - lmin;
            let set = HopSet {
                low_hz: low,
                high_hz: high,
                channels_seen: chans.len() as u16,
                spacing_hz: fit_spacing(chans.iter().map(|c| c.0), self.bin_hz),
                occupied_bw_hz: widths[widths.len() / 2],
                rssi_dbm: (chans.iter().map(|c| c.2 as i32).sum::<i32>() / chans.len() as i32)
                    as i16,
                spread_db: spread,
            };
            if spread <= MAX_HOPPER_SPREAD_DB {
                Activity::Hopper(set)
            } else {
                Activity::Background
            }
        } else if !chans.is_empty() {
            Activity::Background
        } else if !steady.is_empty() {
            Activity::Steady
        } else {
            Activity::Quiet
        };
        EnergyReport {
            activity,
            steady,
            windows: self.windows,
        }
    }
}

/// The largest candidate spacing on whose grid (best offset) at least 90%
/// of the channel centres fall, within half a bin.
fn fit_spacing(centres: impl Iterator<Item = u32> + Clone, bin_hz: u32) -> Option<u32> {
    let n = centres.clone().count();
    if n < 3 {
        return None;
    }
    let tol = bin_hz / 2 + 1;
    for &s in SPACINGS.iter().filter(|&&s| s >= 2 * bin_hz) {
        for anchor in centres.clone() {
            let on = centres
                .clone()
                .filter(|&c| {
                    let d = c.abs_diff(anchor) % s;
                    d <= tol || s - d <= tol
                })
                .count();
            if on * 10 >= n * 9 {
                return Some(s);
            }
        }
    }
    None
}

/// Burst timing on one frequency (a dwell with fast RSSI reads).
#[derive(Debug, Clone)]
pub struct BurstTimer<const M: usize> {
    floor_q2: i32,
    init: bool,
    start: Option<u64>,
    /// (start µs, length µs).
    bursts: heapless::Vec<(u64, u32), M>,
}

impl<const M: usize> Default for BurstTimer<M> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct BurstSummary {
    pub bursts: u16,
    pub median_len_us: u32,
    /// Packet interval estimate: the shortest gap between burst starts that
    /// the other gaps are whole multiples of.
    pub interval_us: Option<u32>,
    /// % of gaps that are whole multiples (within 3%) of `interval_us`.
    pub periodic_pct: u8,
}

impl<const M: usize> BurstTimer<M> {
    pub const fn new() -> Self {
        Self {
            floor_q2: 0,
            init: false,
            start: None,
            bursts: heapless::Vec::new(),
        }
    }

    pub fn add(&mut self, rssi_dbm: i16, t_us: u64) {
        let r4 = rssi_dbm as i32 * 4;
        if !self.init {
            self.floor_q2 = r4;
            self.init = true;
        }
        let hit = (r4 - self.floor_q2) / 4 >= HIT_MARGIN_DB as i32;
        match (hit, self.start) {
            (true, None) => self.start = Some(t_us),
            (false, Some(s)) => {
                self.start = None;
                if self.bursts.is_full() {
                    self.bursts.remove(0);
                }
                let _ = self.bursts.push((s, (t_us - s) as u32));
            }
            _ => {}
        }
        if !hit {
            if r4 < self.floor_q2 {
                self.floor_q2 -= (self.floor_q2 - r4 + 1) / 2;
            } else {
                self.floor_q2 += (r4 - self.floor_q2 + 7) / 8;
            }
        }
    }

    pub fn summary(&self) -> Option<BurstSummary> {
        let n = self.bursts.len();
        if n < 3 {
            return None;
        }
        let mut lens: heapless::Vec<u32, M> = self.bursts.iter().map(|b| b.1).collect();
        lens.sort_unstable();
        let gaps = || {
            self.bursts
                .windows(2)
                .map(|w| (w[1].0 - w[0].0) as u32)
                .filter(|&g| g > 200)
        };
        let mut best: Option<(u32, u8)> = None;
        // Candidate bases: each gap, and its halves/thirds (a hopper may
        // never show two consecutive packets on one channel).
        for g in gaps() {
            for div in 1..=4u32 {
                let base = g / div;
                if base < 500 {
                    continue;
                }
                let total = gaps().count() as u32;
                let fit = gaps()
                    .filter(|&x| {
                        let k = (x + base / 2) / base;
                        // Tolerance relative to the base, not the gap: a
                        // long gap is "a multiple" of anything otherwise.
                        k >= 1 && x.abs_diff(k * base) * 100 <= 3 * base
                    })
                    .count() as u32;
                let pct = (fit * 100 / total.max(1)) as u8;
                // Prefer the largest base that explains >= 90%, else the
                // best fit.
                let better = match best {
                    None => true,
                    Some((b, p)) => (pct >= 90 && (p < 90 || base > b)) || (p < 90 && pct > p),
                };
                if better {
                    best = Some((base, pct));
                }
            }
        }
        Some(BurstSummary {
            bursts: n as u16,
            median_len_us: lens[lens.len() / 2],
            interval_us: best.map(|b| b.0),
            periodic_pct: best.map_or(0, |b| b.1),
        })
    }
}

/// A candidate system with a score (0-100).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Candidate {
    pub system: System,
    pub label: &'static str,
    pub score: u8,
    pub source: Source,
}

/// Match a hop set (and, if available, a dwell's burst timing) against the
/// signature table. Returns up to 6 candidates, best first. A score of 70+
/// means band, grid and timing all fit; 40-69 means band and either grid or
/// timing; below that, band only.
pub fn classify(hop: &HopSet, dwell: Option<&BurstSummary>) -> heapless::Vec<Candidate, 6> {
    let mut out: heapless::Vec<Candidate, 16> = heapless::Vec::new();
    for s in TABLE.iter() {
        if let Some(score) = score(s, hop, dwell) {
            let _ = out.push(Candidate {
                system: s.system,
                label: s.label,
                score,
                source: s.source,
            });
        }
    }
    out.sort_unstable_by_key(|c| core::cmp::Reverse(c.score));
    out.iter().take(6).copied().collect()
}

fn score(s: &Signature, hop: &HopSet, dwell: Option<&BurstSummary>) -> Option<u8> {
    // Band: the hop set must lie (mostly) inside the signature's band.
    let margin = 1_000_000;
    if hop.low_hz + margin < s.band_hz.0 || hop.high_hz > s.band_hz.1 + margin {
        return None;
    }
    let mut score = 20u32;
    if let (Some(a), Some(b)) = (hop.spacing_hz, s.spacing_hz) {
        if a.abs_diff(b) * 10 <= b {
            score += 30;
        } else {
            score = score.saturating_sub(15);
        }
    }
    if let Some(n) = s.channels
        && hop.channels_seen > n + 2
    {
        return None;
    }
    if let Some(d) = dwell {
        if let Some(iv) = d.interval_us
            && d.periodic_pct >= 70
        {
            if s.intervals_us
                .iter()
                .any(|&x| x.abs_diff(iv) * 100 <= 3 * x)
            {
                score += 35;
            } else if !s.intervals_us.is_empty() {
                score = score.saturating_sub(10);
            }
        } else if s.intervals_us.is_empty() {
            // Irregular timing fits systems with irregular timing (SiK).
            score += 15;
        }
        if (s.burst_us.0..=s.burst_us.1).contains(&d.median_len_us) {
            score += 15;
        } else {
            score = score.saturating_sub(10);
        }
    }
    if matches!(s.source, Source::Estimate) {
        score = score.min(50);
    }
    Some(score.min(100) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u32 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (self.0 >> 33) as u32
        }
    }

    const BIN: u32 = 200_000;

    /// Sweep the FCC band `sweeps` times; `emit(freq, t)` returns extra dBm
    /// above a -110 dBm floor (or None).
    fn sweep(
        map: &mut EnergyMap<130>,
        sweeps: u32,
        t0: u64,
        rng: &mut Lcg,
        mut emit: impl FnMut(u32, u64, &mut Lcg) -> Option<i16>,
    ) {
        let mut t = t0;
        for _ in 0..sweeps {
            for step in crate::scan::energy_plan(Region::Fcc915, BIN) {
                let f = step.config.freq_hz;
                let noise = -110 + (rng.next() % 3) as i16;
                let r = emit(f, t, rng).map_or(noise, |e| noise.max(-110 + e));
                map.add(f, r);
                t += 2_000;
            }
        }
    }

    /// ELRS-like hopper on the FCC915 grid: 500 kHz wide, ~same level.
    fn elrs(f: u32, t: u64, rng: &mut Lcg) -> Option<i16> {
        let iv = 4_000u64;
        let n = t / iv;
        let ch = ((n / 4).wrapping_mul(2654435761) % 40) as u32;
        let c = 903_500_000 + ch * 600_000;
        // On air 80% of the interval.
        let d = f.abs_diff(c);
        let lvl = if d <= 100_000 { 40 } else { 33 };
        (d <= 250_000 && t % iv < 3_200).then_some(lvl + (rng.next() % 3) as i16)
    }

    /// Meter-mesh-like: random channels on a 250 kHz grid, random levels,
    /// short random bursts.
    fn meters(f: u32, _t: u64, rng: &mut Lcg) -> Option<i16> {
        let on = rng.next() % 100 < 6;
        let lvl = 15 + (rng.next() % 40) as i16;
        (on && (f / 1000) % 250 < 200).then_some(lvl)
    }

    #[test]
    fn hopper_found_with_grid_and_steady_carrier_kept_apart() {
        let mut map = EnergyMap::<130>::for_region(Region::Fcc915, BIN);
        let mut rng = Lcg(1);
        // A steady carrier at 911.1 MHz throughout.
        let steady = |f: u32| (f.abs_diff(911_100_000) < 100_000).then_some(30);
        for w in 0..3 {
            sweep(&mut map, 10, w * 10_000_000, &mut rng, |f, _, _| steady(f));
            let _ = map.end_window();
        }
        sweep(&mut map, 40, 50_000_000, &mut rng, |f, t, r| {
            steady(f).or_else(|| elrs(f, t, r))
        });
        let rep = map.end_window();
        assert!(
            rep.steady
                .iter()
                .any(|s| s.freq_hz.abs_diff(911_100_000) < 200_000)
        );
        let Activity::Hopper(h) = rep.activity else {
            panic!("{:?}", rep.activity)
        };
        assert_eq!(h.spacing_hz, Some(600_000));
        assert!(h.channels_seen >= 10);
        let c = classify(&h, None);
        assert!(c.iter().any(|c| c.system == System::ExpressLrs));
    }

    #[test]
    fn meter_mesh_is_background_not_hopper() {
        let mut map = EnergyMap::<130>::for_region(Region::Fcc915, BIN);
        let mut rng = Lcg(7);
        for w in 0..30 {
            sweep(&mut map, 10, w * 10_000_000, &mut rng, meters);
            let rep = map.end_window();
            assert!(
                !matches!(rep.activity, Activity::Hopper(_)),
                "window {w}: {:?}",
                rep.activity
            );
        }
    }

    #[test]
    fn burst_timer_finds_interval_and_rejects_random() {
        let mut bt = BurstTimer::<64>::new();
        // ELRS 250 Hz parked on one channel: 4 packets (4 ms apart, 3.2 ms
        // long) every 40 hops.
        let mut t = 0u64;
        for _ in 0..20_000 {
            let n = t / 4_000;
            let on = (n % 160) < 4 && t % 4_000 < 3_200;
            bt.add(if on { -60 } else { -110 }, t);
            t += 50;
        }
        let s = bt.summary().unwrap();
        assert_eq!(s.interval_us, Some(4_000));
        assert!(s.periodic_pct >= 90);
        assert!((3_000..3_400).contains(&s.median_len_us));
        let hop = HopSet {
            low_hz: 903_500_000,
            high_hz: 926_900_000,
            channels_seen: 30,
            spacing_hz: Some(600_000),
            occupied_bw_hz: 600_000,
            rssi_dbm: -60,
            spread_db: 3,
        };
        let c = classify(&hop, Some(&s));
        assert_eq!(c[0].system, System::ExpressLrs);
        assert!(c[0].score >= 70);

        // Random bursts (meter-like).
        let mut bt = BurstTimer::<64>::new();
        let mut rng = Lcg(3);
        let mut t = 0u64;
        while t < 30_000_000 {
            let gap = 20_000 + (rng.next() % 900_000) as u64;
            let len = 2_000 + (rng.next() % 30_000) as u64;
            for k in 0..(gap / 100) {
                bt.add(-110, t + k * 100);
            }
            t += gap;
            for k in 0..(len / 100) {
                bt.add(-70, t + k * 100);
            }
            t += len;
        }
        let s = bt.summary().unwrap();
        assert!(s.periodic_pct < 70, "{s:?}");
    }
}
