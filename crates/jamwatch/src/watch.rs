//! Per-hop baselines, per-dwell verdicts, and what to tell the hub.
//!
//! **Baselines.** Every hop (keyed by its frequency) learns its own noise
//! floor: the dwell's 10th-percentile probe level. It follows a lower value
//! at once and a higher one only at [`LEARN_UP_X100_PER_MIN`], and not at all
//! while the hop looks raised ([`FREEZE_X10`]): a jammer that appears is
//! never learnt into the baseline, and one that stays for an hour is still
//! reported. A learnt floor is never trusted above the lowest of
//!
//! - the receiver's expected floor for the band plus [`CEIL_X10`]
//!   ([`REF_FLOOR_X10`], from the bring-up noise measurement: **not yet
//!   checked against a real sweep's floors**, see docs/JAMMING.md);
//! - the band's quietest hop plus [`SPREAD_X10`];
//! - the other band's quietest hop, corrected by the expected difference
//!   between the bands, plus [`XBAND_MARGIN_X10`];
//!
//! so a node that boots into a jammed band still sees the rise (against the
//! receiver's own noise, or against the band that is not jammed).
//!
//! **Verdicts** ([`Watch::assess`], then [`Watch::commit`] once the caller
//! has run the link check [`Assess::needs_link_check`] asks for) and
//! **reports** ([`Watch::due`]): see the crate docs and docs/JAMMING.md.

use core::ops::RangeInclusive;

use heapless::Vec;

use crate::spectrum::{self, Features};
use crate::{Band, ProbeStats, range_left_pct};

/// Expected 10th-percentile probe level of the receiver's own noise (tenths
/// of a dB, probe units) at the default gain and clip window, `[2.4, 5.8]`
/// GHz: bring-up measured 64-84 codes RMS at 2414 MHz and 43-51 at 5756
/// MHz (fpvscan::band); quantised as the 4-bit window does, 256 bytes give
/// 33.5-35.7 and 30.4-31.7 dB. UNVERIFIED as absolute floors.
pub const REF_FLOOR_X10: [i32; 2] = [345, 310];
/// A learnt floor is never trusted more than this above [`REF_FLOOR_X10`].
pub const CEIL_X10: i32 = 80;
/// ...nor more than this above the band's quietest hop.
pub const SPREAD_X10: i32 = 60;
/// Expected 2.4 GHz floor minus the 5.8 GHz floor (same gain index), and
/// the margin allowed on top when one band's floor bounds the other's.
pub const XBAND_X10: i32 = 35;
pub const XBAND_MARGIN_X10: i32 = 40;
/// Floor learning: up at most this fast (hundredths of a dB per minute),
/// and not at all while the dwell's floor is this far up.
pub const LEARN_UP_X100_PER_MIN: u64 = 50;
pub const FREEZE_X10: i32 = 60;

/// A hop is *raised* (barrage-like) when its floor is up by this much and
/// [`OCC_PCT`] of the passband is occupied, with no link structure.
pub const RAISE_X10: i32 = 80;
pub const OCC_PCT: u8 = 75;
/// Broadband: at least this many raised hops in a band spanning at least
/// this much, and at least half the band's recent hops raised; or at least
/// [`MIN_RAISED_BOTH`] in each band with half of each raised.
pub const MIN_RAISED: usize = 3;
pub const SPAN_MIN_KHZ: u32 = 50_000;
pub const MIN_RAISED_BOTH: usize = 2;
/// Clipping on this share of samples (ppm) counts as a floor rise of at
/// least [`CLIP_RISE_X10`] (the 4-bit window measures little more), if
/// the dwell was loud throughout ([`CONTINUOUS_PCT`]).
pub const CLIP_PPM: u32 = 10_000;
pub const CLIP_RISE_X10: i32 = 200;
/// ...when at least this percent of the dwell's probes were loud.
pub const CONTINUOUS_PCT: u8 = 90;
/// Blinded: the median floor rise of a band's recent hops (links excluded).
pub const BLIND_X10: i32 = 120;
/// Probe level counted as loud: this far over the floor.
pub const LOUD_X10: i32 = 120;
/// Tone: its hop's floor must be up at least this much.
pub const TONE_RISE_X10: i32 = 60;
/// Pulsed: loud on this percent of probes, in at least this many runs, the
/// loud frames' spectrum this occupied.
pub const PULSE_DUTY_PCT: RangeInclusive<u8> = 2..=90;
pub const PULSE_MIN_RUNS: u32 = 3;
pub const PULSE_OCC_PCT: u8 = 60;
/// Microwave-oven gate (2.4 GHz, 2400-2500 MHz): on half of each mains
/// cycle (8.3 or 10 ms), so loud 20-80 % of the time in runs of 2-14 ms.
pub const OVEN_MHZ: RangeInclusive<u32> = 2400..=2500;
pub const OVEN_DUTY_PCT: RangeInclusive<u8> = 20..=80;
pub const OVEN_ON_US: RangeInclusive<u32> = 2_000..=14_000;
/// Tone, sweep and pulsed reports need this many supporting dwells.
pub const MIN_SUPPORT: usize = 2;
/// Fewer probes than this: no verdict (a short follower visit).
pub const MIN_PROBES: u32 = 100;
/// Each kind is reported at most once per this.
pub const EMIT_MS: u64 = 1000;
pub const MAX_HOPS: usize = 48;

const fn bi(b: Band) -> usize {
    match b {
        Band::G24 => 0,
        Band::G58 => 1,
    }
}

const fn other(b: Band) -> Band {
    match b {
        Band::G24 => Band::G58,
        Band::G58 => Band::G24,
    }
}

const BANDS: [Band; 2] = [Band::G24, Band::G58];

/// What jamwatch reports (`link::ThreatKind`'s jamming kinds).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Broadband,
    Tone,
    Sweep,
    Pulsed,
    Blinded,
}

impl Kind {
    pub const ALL: [Kind; 5] = [Kind::Broadband, Kind::Tone, Kind::Sweep, Kind::Pulsed, Kind::Blinded];

    pub const fn threat(self) -> link::ThreatKind {
        match self {
            Kind::Broadband => link::ThreatKind::JamBroadband,
            Kind::Tone => link::ThreatKind::JamTone,
            Kind::Sweep => link::ThreatKind::JamSweep,
            Kind::Pulsed => link::ThreatKind::JamPulsed,
            Kind::Blinded => link::ThreatKind::Blinded,
        }
    }

    pub fn name(self) -> &'static str {
        self.threat().name()
    }
}

/// One dwell, measured but not yet judged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Assess {
    /// The hop (100 kHz units) and the LO it was received with (MHz).
    pub freq: u32,
    pub lo_mhz: u16,
    pub band: Band,
    pub probes: u32,
    pub p10_x10: i32,
    pub p50_x10: i32,
    pub p99_x10: i32,
    /// The floor judged against, and whether the hop had none learnt yet.
    pub floor_x10: i32,
    pub first: bool,
    /// Percentiles over the floor: 10th (the floor's rise), 50th, 99th.
    pub rise_x10: i32,
    pub rise50_x10: i32,
    pub loud_rise_x10: i32,
    /// The rise, or at least [`CLIP_RISE_X10`] when the front end clips.
    pub eff_rise_x10: i32,
    pub loud_pct: u8,
    pub runs: u32,
    pub on_us: u32,
    pub spec: Option<Features>,
    /// Candidate findings before the link check.
    pub wide: bool,
    pub pulsed: bool,
    pub oven: bool,
}

impl Assess {
    /// Whether the verdict depends on knowing if this is a structured link
    /// (an `fpvscan` cyclic-prefix / line-rate check of the capture; for a
    /// pulsed candidate, of a capture holding a loud frame).
    pub fn needs_link_check(&self) -> bool {
        self.wide
            || self.pulsed
            || self.eff_rise_x10 >= BLIND_X10
            || self.spec.is_some_and(|s| s.tone.is_some() || s.sweep.is_some())
    }

    pub fn clip_ppm(&self) -> u32 {
        self.spec.map_or(0, |s| s.clip_ppm)
    }
}

/// One dwell's verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HopVerdict {
    pub freq: u32,
    pub lo_mhz: u16,
    pub band: Band,
    pub t_ms: u64,
    pub floor_x10: i32,
    pub eff_rise_x10: i32,
    pub rise50_x10: i32,
    pub loud_rise_x10: i32,
    pub loud_pct: u8,
    pub clip_ppm: u32,
    /// A structured link was recognised (its rise is not counted as jamming
    /// or blindness).
    pub link: bool,
    /// Barrage-like: floor up, passband full, no link.
    pub raised: bool,
    pub pulsed: bool,
    pub oven: bool,
    /// A tone's absolute frequency (100 kHz units).
    pub tone: Option<u32>,
    /// A sweep: rate MHz/s and the centre it was seen at (100 kHz units).
    pub sweep: Option<(u32, u32)>,
}

impl HopVerdict {
    /// Anything worth a console line.
    pub fn notable(&self) -> bool {
        self.raised
            || self.pulsed
            || self.oven
            || self.tone.is_some()
            || self.sweep.is_some()
            || self.eff_rise_x10 >= BLIND_X10
            || self.clip_ppm >= CLIP_PPM
    }

    fn blind_counts(&self) -> bool {
        !self.link
    }
}

#[derive(Clone, Copy, Debug)]
struct Hop {
    freq: u32,
    /// Learnt floor, hundredths of a dB.
    floor_x100: i32,
    t_learn: u64,
    v: Option<HopVerdict>,
}

/// What to send: one jamming finding over a band.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Report {
    pub kind: Kind,
    pub band: Band,
    /// Centre (100 kHz units) and width (kHz) of what is affected.
    pub freq: u32,
    pub width_khz: u32,
    /// dB over the learnt floor.
    pub level_db: i8,
    /// Dwells supporting it, and the time they span (ms).
    pub count: u32,
    pub window_ms: u32,
    /// Broadband: percent of the band's recent hops raised. Tone: distinct
    /// tone frequencies (1 MHz apart). Sweep: rate, MHz/s. Pulsed: percent
    /// of probes loud. Blinded: percent of detection range left.
    pub detail: u32,
}

impl Report {
    pub fn to_sighting(&self, uptime_us: u64) -> link::ThreatSighting {
        link::ThreatSighting {
            kind: self.kind.threat(),
            channel: 0,
            freq: self.freq,
            width_khz: self.width_khz,
            level: self.level_db,
            count: self.count,
            window_ms: self.window_ms,
            source: [0; 6],
            target: [0; 6],
            detail: self.detail,
            ssid: heapless::String::new(),
            uptime_us,
        }
    }
}

pub struct Watch {
    hops: Vec<Hop, MAX_HOPS>,
    stale_ms: u64,
    /// Last report per kind and band, and per kind (ms; None = never).
    last: [[Option<u64>; 2]; 5],
    last_kind: [Option<u64>; 5],
    /// Counters for the console.
    pub dwells: u32,
    pub reports: u32,
}

impl Default for Watch {
    fn default() -> Self {
        Self::new()
    }
}

fn median(v: &mut [i32]) -> i32 {
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    v[v.len() / 2]
}

impl Watch {
    pub const fn new() -> Self {
        Watch { hops: Vec::new(), stale_ms: 10_000, last: [[None; 2]; 5], last_kind: [None; 5], dwells: 0, reports: 0 }
    }

    /// Forget everything (role change: the probe levels mean something
    /// different).
    pub fn reset(&mut self) {
        *self = Watch { stale_ms: self.stale_ms, ..Watch::new() };
    }

    /// A new plan taking `cycle_ms` per round: verdicts older than two
    /// rounds (and 2 s, at least 5 s) no longer count. Keeps the learnt
    /// floors (they belong to frequencies, not plans) and drops verdicts.
    pub fn set_cycle_ms(&mut self, cycle_ms: u32) {
        self.stale_ms = (2 * cycle_ms as u64 + 2000).max(5000);
        for h in self.hops.iter_mut() {
            h.v = None;
        }
    }

    pub fn stale_ms(&self) -> u64 {
        self.stale_ms
    }

    fn find(&self, freq: u32) -> Option<usize> {
        self.hops.iter().position(|h| h.freq == freq)
    }

    fn band_min(&self, b: Band) -> Option<i32> {
        self.hops.iter().filter(|h| Band::of(mhz(h.freq)) == b).map(|h| h.floor_x100 / 10).min()
    }

    /// The highest floor (tenths of a dB) a hop of band `b` is trusted with.
    fn bound(&self, b: Band) -> i32 {
        let mut c = REF_FLOOR_X10[bi(b)] + CEIL_X10;
        if let Some(m) = self.band_min(b) {
            c = c.min(m + SPREAD_X10);
        }
        if let Some(m) = self.band_min(other(b)) {
            let diff = if b == Band::G24 { XBAND_X10 } else { -XBAND_X10 };
            c = c.min(m + diff + XBAND_MARGIN_X10);
        }
        c
    }

    /// The floor a dwell on `freq` is judged against (tenths of a dB, probe
    /// units); `None` before the hop's first dwell.
    pub fn floor_x10(&self, freq: u32) -> Option<i32> {
        let h = &self.hops[self.find(freq)?];
        Some((h.floor_x100 / 10).min(self.bound(Band::of(mhz(freq)))))
    }

    /// The loud level for a dwell on `freq` ([`ProbeStats::begin`]).
    pub fn loud_x10(&self, freq: u32) -> i32 {
        let b = Band::of(mhz(freq));
        self.floor_x10(freq).unwrap_or(REF_FLOOR_X10[bi(b)]).min(self.bound(b)) + LOUD_X10
    }

    /// Measure one dwell on hop `freq` (100 kHz units), received with the
    /// LO at `lo_mhz`. `spec`: the spectrogram of the newest window, if one
    /// was taken. `None` when the dwell had too few probes to judge.
    pub fn assess(&self, freq: u32, lo_mhz: u16, p: &ProbeStats, spec: Option<&Features>) -> Option<Assess> {
        if p.count() < MIN_PROBES {
            return None;
        }
        let band = Band::of(mhz(freq));
        let p10 = p.percentile_x10(10);
        let p50 = p.percentile_x10(50);
        let p99 = p.percentile_x10(99);
        let (floor, first) = match self.floor_x10(freq) {
            Some(f) => (f, false),
            None => (p10.min(self.bound(band)), true),
        };
        let rise = p10 - floor;
        let clip = spec.map_or(0, |s| s.clip_ppm);
        let (loud_pct, runs, on_us) = (p.loud_pct(), p.runs(), p.mean_on_us());
        // Clipping in the ring stands for a floor rise only when the dwell
        // was loud throughout (a pulse in the ring is not the floor).
        let eff = if clip >= CLIP_PPM && loud_pct >= CONTINUOUS_PCT { rise.max(CLIP_RISE_X10) } else { rise };
        let occ = spec.map_or(0, |s| s.occupancy_pct);
        let oven = band == Band::G24
            && OVEN_MHZ.contains(&(lo_mhz as u32))
            && OVEN_DUTY_PCT.contains(&loud_pct)
            && OVEN_ON_US.contains(&on_us)
            && runs >= 2;
        let wide = eff >= RAISE_X10 && occ >= OCC_PCT;
        let pulsed = !wide
            && rise < RAISE_X10
            && PULSE_DUTY_PCT.contains(&loud_pct)
            && runs >= PULSE_MIN_RUNS
            && !oven
            && spec.is_some_and(|s| s.pulse_occupancy().is_some_and(|o| o >= PULSE_OCC_PCT));
        Some(Assess {
            freq,
            lo_mhz,
            band,
            probes: p.count(),
            p10_x10: p10,
            p50_x10: p50,
            p99_x10: p99,
            floor_x10: floor,
            first,
            rise_x10: rise,
            rise50_x10: p50 - floor,
            loud_rise_x10: p99 - floor,
            eff_rise_x10: eff,
            loud_pct,
            runs,
            on_us,
            spec: spec.copied(),
            wide,
            pulsed,
            oven,
        })
    }

    /// Judge a dwell. `link`: `Some(true)` if a structured link was
    /// recognised in a capture of it, `Some(false)` if checked and none,
    /// `None` if not checked (a pulsed candidate then does not count; a wide
    /// one does: fail loud). Learns the hop's floor.
    pub fn commit(&mut self, a: &Assess, link: Option<bool>, now_ms: u64) -> HopVerdict {
        let is_link = link == Some(true);
        let tone = a
            .spec
            .and_then(|s| s.tone)
            .filter(|_| !is_link && !a.oven && a.rise_x10 >= TONE_RISE_X10)
            .map(|t| (a.lo_mhz as i32 * 10 + t.offset_khz / 100) as u32);
        let sweep = a
            .spec
            .and_then(|s| s.sweep)
            .filter(|_| !is_link && !a.oven)
            .map(|s| (s.rate_mhz_s, (a.lo_mhz as i32 * 10 + s.centre_khz / 100) as u32));
        let v = HopVerdict {
            freq: a.freq,
            lo_mhz: a.lo_mhz,
            band: a.band,
            t_ms: now_ms,
            floor_x10: a.floor_x10,
            eff_rise_x10: a.eff_rise_x10,
            rise50_x10: a.rise50_x10,
            loud_rise_x10: a.loud_rise_x10,
            loud_pct: a.loud_pct,
            clip_ppm: a.clip_ppm(),
            link: is_link,
            raised: a.wide && !is_link,
            pulsed: a.pulsed && link == Some(false),
            oven: a.oven,
            tone,
            sweep,
        };
        self.dwells = self.dwells.wrapping_add(1);
        self.learn(a, &v, now_ms);
        v
    }

    fn learn(&mut self, a: &Assess, v: &HopVerdict, now_ms: u64) {
        let p10 = a.p10_x10 * 10;
        if let Some(i) = self.find(a.freq) {
            let calm = a.eff_rise_x10 < FREEZE_X10 && !v.raised && !v.pulsed;
            let h = &mut self.hops[i];
            if p10 < h.floor_x100 {
                h.floor_x100 = p10;
            } else if calm {
                let up = (now_ms.saturating_sub(h.t_learn) * LEARN_UP_X100_PER_MIN / 60_000) as i32;
                h.floor_x100 += up.min(p10 - h.floor_x100);
            }
            h.t_learn = now_ms;
            h.v = Some(*v);
            return;
        }
        let h = Hop { freq: a.freq, floor_x100: a.floor_x10 * 10, t_learn: now_ms, v: Some(*v) };
        if self.hops.is_full() {
            // Drop the hop judged longest ago (a plan change, a follower
            // channel not visited for a while).
            if let Some((i, _)) = self.hops.iter().enumerate().min_by_key(|(_, h)| h.v.map_or(0, |v| v.t_ms)) {
                self.hops.swap_remove(i);
            }
        }
        let _ = self.hops.push(h);
    }

    /// The recent verdicts of band `b`.
    fn fresh(&self, b: Band, now_ms: u64) -> impl Iterator<Item = &HopVerdict> {
        let stale = self.stale_ms;
        self.hops.iter().filter_map(move |h| h.v.as_ref()).filter(move |v| v.band == b && v.t_ms + stale >= now_ms)
    }

    fn raised(&self, b: Band, now_ms: u64) -> (usize, usize, u32) {
        let (mut n, mut r) = (0, 0);
        let (mut lo, mut hi) = (u32::MAX, 0);
        for v in self.fresh(b, now_ms) {
            n += 1;
            if v.raised {
                r += 1;
                lo = lo.min(v.freq);
                hi = hi.max(v.freq);
            }
        }
        (n, r, if r > 0 { (hi - lo) * 100 } else { 0 })
    }

    /// Whether band `b` is under barrage jamming now.
    pub fn broadband(&self, b: Band, now_ms: u64) -> bool {
        let (n, r, span) = self.raised(b, now_ms);
        let half = r > 0 && 2 * r >= n;
        if half && r >= MIN_RAISED && span >= SPAN_MIN_KHZ {
            return true;
        }
        let (n2, r2, _) = self.raised(other(b), now_ms);
        half && r >= MIN_RAISED_BOTH && r2 >= MIN_RAISED_BOTH && 2 * r2 >= n2
    }

    /// Band `b`'s finding of `kind`, if any, now.
    pub fn finding(&self, kind: Kind, b: Band, now_ms: u64) -> Option<Report> {
        let mut rises: Vec<i32, MAX_HOPS> = Vec::new();
        let (mut lo, mut hi, mut t0, mut n, mut all) = (u32::MAX, 0u32, u64::MAX, 0u32, 0u32);
        let mut detail: Vec<i32, MAX_HOPS> = Vec::new();
        let mut last_t = 0u64;
        let mut last_f = 0u32;
        for v in self.fresh(b, now_ms) {
            all += 1;
            let (hit, level, d, f) = match kind {
                Kind::Broadband => (v.raised, v.eff_rise_x10, 0, v.freq),
                Kind::Tone => (v.tone.is_some(), v.eff_rise_x10, v.tone.unwrap_or(0) as i32, v.tone.unwrap_or(v.freq)),
                Kind::Sweep => {
                    let (r, c) = v.sweep.unwrap_or((0, v.freq));
                    (v.sweep.is_some(), v.loud_rise_x10, r as i32, c)
                }
                Kind::Pulsed => (v.pulsed, v.loud_rise_x10, v.loud_pct as i32, v.freq),
                Kind::Blinded => (v.blind_counts(), v.eff_rise_x10, 0, v.freq),
            };
            if !hit {
                continue;
            }
            n += 1;
            let _ = rises.push(level);
            let _ = detail.push(d);
            lo = lo.min(f);
            hi = hi.max(f);
            t0 = t0.min(v.t_ms);
            if v.t_ms >= last_t {
                last_t = v.t_ms;
                last_f = f;
            }
        }
        let ok = match kind {
            Kind::Broadband => self.broadband(b, now_ms),
            Kind::Tone | Kind::Sweep | Kind::Pulsed => n as usize >= MIN_SUPPORT,
            Kind::Blinded => n >= 2 && median(&mut rises.clone()) >= BLIND_X10,
        };
        if !ok {
            return None;
        }
        let level = median(&mut rises);
        let (freq, width_khz, detail, count) = match kind {
            Kind::Broadband => ((lo + hi) / 2, (hi - lo) * 100 + spectrum::PASS_KHZ, n * 100 / all.max(1), n),
            Kind::Tone => {
                // Distinct tones, 1 MHz apart.
                let d = &mut detail[..];
                d.sort_unstable();
                let distinct = d.windows(2).filter(|w| w[1] - w[0] >= 10).count() as u32 + 1;
                (last_f, 3 * spectrum::BIN_KHZ as u32, distinct, n)
            }
            Kind::Sweep | Kind::Pulsed => {
                ((lo + hi) / 2, (hi - lo) * 100 + spectrum::PASS_KHZ, median(&mut detail).max(0) as u32, n)
            }
            Kind::Blinded => {
                let blind = rises.iter().filter(|&&r| r >= BLIND_X10).count() as u32;
                ((lo + hi) / 2, (hi - lo) * 100 + spectrum::PASS_KHZ, range_left_pct(level) as u32, blind)
            }
        };
        Some(Report {
            kind,
            band: b,
            freq,
            width_khz,
            level_db: (level / 10).clamp(-128, 127) as i8,
            count,
            window_ms: now_ms.saturating_sub(t0).min(u32::MAX as u64) as u32,
            detail,
        })
    }

    /// Reports due now: each kind at most once per [`EMIT_MS`]; a kind
    /// active on both bands alternates between them.
    pub fn due(&mut self, now_ms: u64, out: &mut Vec<Report, 5>) {
        for (k, kind) in Kind::ALL.iter().enumerate() {
            if self.last_kind[k].is_some_and(|t| now_ms < t + EMIT_MS) {
                continue;
            }
            let mut pick: Option<Report> = None;
            for b in BANDS {
                if let Some(r) = self.finding(*kind, b, now_ms) {
                    let older = |x: &Report| self.last[k][bi(x.band)].unwrap_or(0);
                    if pick.as_ref().is_none_or(|p| self.last[k][bi(b)].unwrap_or(0) < older(p)) {
                        pick = Some(r);
                    }
                }
            }
            if let Some(r) = pick {
                self.last[k][bi(r.band)] = Some(now_ms);
                self.last_kind[k] = Some(now_ms);
                self.reports = self.reports.wrapping_add(1);
                let _ = out.push(r);
            }
        }
    }

    /// Learnt floors and latest verdicts, for the console: `(freq,
    /// floor_x10 as judged, latest verdict)`.
    pub fn hops(&self) -> impl Iterator<Item = (u32, i32, Option<&HopVerdict>)> {
        self.hops.iter().map(|h| (h.freq, self.floor_x10(h.freq).unwrap_or(h.floor_x100 / 10), h.v.as_ref()))
    }
}

/// A hop's frequency (100 kHz units) in whole MHz.
const fn mhz(freq: u32) -> u16 {
    ((freq + 5) / 10) as u16
}
