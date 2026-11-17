//! Synthetic scenes through the real 4-bit path: 40 MS/s complex baseband in
//! ADC codes plus receiver noise, quantised by the clip window as the C5's
//! debug bus does (`rawcap::iq`). Each dwell yields the trigger probes (256
//! of every 2048 samples) and the newest part of the ring, exactly what the
//! firmware has at the end of a dwell; the link check is `fpvscan` on the
//! half-banded capture, as in the firmware.

use std::f64::consts::PI;

use droneid::math::{C16, remove_dc};
use rawcap::dsp::{HB_REACH, decimate_nibbles};
use rawcap::iq::{POWER, Window, pack, unpack};

use crate::spectrum::{self, NFFT};
use crate::watch::{self, Kind, Report};
use crate::*;

const FS: f64 = 40e6;
/// Receiver noise, codes RMS per axis (bring-up: 43-51 at 5756 MHz, 64-84
/// at 2414 MHz).
const N58: f64 = 48.0;
const N24: f64 = 74.0;
const RING: usize = 16_384;
const BLOCK: u64 = 2048;
const PROBE: u64 = 256;

#[derive(Clone)]
struct Rng(u64);
impl Rng {
    fn u(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
    }
    fn g(&mut self) -> f64 {
        let (a, b) = (self.u().max(1e-300), self.u());
        (-2.0 * a.ln()).sqrt() * (2.0 * PI * b).cos()
    }
}

fn hash(a: u64, b: u64) -> u64 {
    let mut r = Rng(a.wrapping_mul(0x1000_0000_01b3) ^ b);
    r.u();
    r.0
}

/// Analog low-pass (about +-10 MHz): what a component at `f_hz` from the LO
/// keeps of its amplitude.
fn lpf(f_hz: f64) -> f64 {
    let f = f_hz.abs() / 1e6;
    if f <= 9.0 {
        1.0
    } else if f >= 12.0 {
        0.0
    } else {
        (12.0 - f) / 3.0
    }
}

/// Complex amplitude for a component `db` above the receiver noise power
/// (both axes) of the band.
fn amp(db: f64, noise: f64) -> f64 {
    (2.0 * noise * noise * 10f64.powf(db / 10.0)).sqrt()
}

#[derive(Clone)]
enum Comp {
    /// White noise `db` over the receiver noise on hops whose LO is in
    /// `mhz`; optionally gated on `on` s of every `period` s.
    Noise { db: f64, mhz: (u16, u16), gate: Option<(f64, f64)> },
    Tone { mhz: f64, db: f64 },
    /// Sawtooth sweep `lo`..`hi` MHz at `rate` MHz/s.
    Chirp { lo: f64, hi: f64, rate: f64, db: f64 },
    /// 20 MHz Wi-Fi-numerology OFDM (3.2 µs + 0.8 µs CP): a drone video
    /// link when continuous, Wi-Fi traffic when gated.
    Ofdm { mhz: f64, db: f64, gate: Option<(f64, f64)>, seed: u64 },
    /// Flat, wide (17 MHz), continuous, no cyclic prefix: HDZero-like.
    FlatWide { mhz: f64, db: f64 },
    /// Analog FM video (PAL line), a vertical gradient picture plus a ramp
    /// across each line: the FM carrier moves steadily across every line,
    /// a sweep-like shape a sweep detector must not call a jammer.
    FmVideo { mhz: f64, db: f64 },
    /// A magnetron oven: on half of each 60 Hz cycle; a wandering carrier,
    /// or (`wide`) 15 MHz of noise.
    Oven { mhz: f64, db: f64, wide: bool },
}

struct Scene {
    comps: Vec<Comp>,
    rng: Rng,
    /// OFDM symbol cache per component: (symbol index, 160 samples).
    cache: Vec<(i64, Vec<(f64, f64)>)>,
    /// FM state: last sample index and phase.
    fm: (u64, f64),
}

/// One 128-point inverse DFT (f64) of Wi-Fi's carriers with QPSK from
/// `seed`, with its 32-sample cyclic prefix.
fn ofdm_symbol(seed: u64) -> Vec<(f64, f64)> {
    let ks: Vec<i32> = (-26..=26).filter(|&k| k != 0).collect();
    let body = idft(seed, &ks);
    body[96..].iter().chain(body.iter()).copied().collect()
}

/// Band-limited noise-like signal seen `off_hz` from the LO: `half`
/// carriers either side (312.5 kHz apart), random QPSK every 3.2 µs
/// symbol, no cyclic prefix, each carrier through the analog low-pass.
fn flat_symbol(seed: u64, half: i32, off_hz: f64) -> Vec<(f64, f64)> {
    let mut r = Rng(seed);
    let ks: Vec<(f64, f64, f64)> = (-half..=half)
        .map(|k| {
            let f = off_hz + k as f64 * 312_500.0;
            (f, lpf(f) * if r.u() < 0.5 { 1.0 } else { -1.0 }, lpf(f) * if r.u() < 0.5 { 1.0 } else { -1.0 })
        })
        .collect();
    let norm = 1.0 / (2.0 * ks.len() as f64).sqrt();
    (0..128)
        .map(|n| {
            let (mut re, mut im) = (0.0, 0.0);
            for &(f, a, b) in &ks {
                let (sn, cs) = (2.0 * PI * f * n as f64 / FS).sin_cos();
                re += a * cs - b * sn;
                im += a * sn + b * cs;
            }
            (re * norm, im * norm)
        })
        .collect()
}

fn idft(seed: u64, ks: &[i32]) -> Vec<(f64, f64)> {
    let mut r = Rng(seed);
    let d: Vec<(f64, f64)> = ks.iter().map(|_| (if r.u() < 0.5 { 1.0 } else { -1.0 }, if r.u() < 0.5 { 1.0 } else { -1.0 })).collect();
    let norm = 1.0 / (2.0 * ks.len() as f64).sqrt();
    (0..128)
        .map(|n| {
            let (mut re, mut im) = (0.0, 0.0);
            for (k, &(a, b)) in ks.iter().zip(&d) {
                let (s, c) = (2.0 * PI * *k as f64 * n as f64 / 128.0).sin_cos();
                re += a * c - b * s;
                im += a * s + b * c;
            }
            (re * norm, im * norm)
        })
        .collect()
}

impl Scene {
    fn new(comps: Vec<Comp>, seed: u64) -> Self {
        let n = comps.len();
        Scene { comps, rng: Rng(seed), cache: vec![(i64::MIN, vec![]); n], fm: (0, 0.0) }
    }

    fn quiet() -> Self {
        Scene::new(vec![], 1)
    }

    /// Sample `n` (absolute, 40 MS/s) as received with the LO at `lo` MHz,
    /// in ADC codes, without receiver noise.
    fn sample(&mut self, n: u64, lo: u16, noise: f64) -> (f64, f64) {
        let t = n as f64 / FS;
        let lo_hz = lo as f64 * 1e6;
        let (mut re, mut im) = (0.0, 0.0);
        let rot = |f_hz: f64, ph0: f64| -> (f64, f64) {
            let ph = 2.0 * PI * (f_hz - lo_hz) * t + ph0;
            (ph.cos(), ph.sin())
        };
        for c in 0..self.comps.len() {
            match self.comps[c].clone() {
                Comp::Noise { db, mhz, gate } => {
                    if lo < mhz.0 || lo > mhz.1 || gate.is_some_and(|(p, on)| t % p >= on) {
                        continue;
                    }
                    let a = amp(db, noise) / 2f64.sqrt();
                    re += a * self.rng.g();
                    im += a * self.rng.g();
                }
                Comp::Tone { mhz, db } => {
                    let g = lpf(mhz * 1e6 - lo_hz) * amp(db, noise);
                    let (c, s) = rot(mhz * 1e6, 0.3);
                    re += g * c;
                    im += g * s;
                }
                Comp::Chirp { lo: f0, hi: f1, rate, db } => {
                    let period = (f1 - f0) / rate;
                    let tt = t % period;
                    let f_bb = (f0 + rate * tt) * 1e6 - lo_hz;
                    let g = lpf(f_bb) * amp(db, noise);
                    // Phase of the sawtooth from the start of its period.
                    let ph = 2.0 * PI * ((f0 * 1e6 - lo_hz) * tt + 0.5 * rate * 1e6 * tt * tt);
                    re += g * ph.cos();
                    im += g * ph.sin();
                }
                Comp::Ofdm { mhz, db, gate, seed } => {
                    if gate.is_some_and(|(p, on)| t % p >= on) {
                        continue;
                    }
                    let g = lpf(mhz * 1e6 - lo_hz) * amp(db, noise);
                    let sym = (n / 160) as i64;
                    if self.cache[c].0 != sym {
                        self.cache[c] = (sym, ofdm_symbol(hash(seed, sym as u64)));
                    }
                    let (a, b) = self.cache[c].1[(n % 160) as usize];
                    let (cs, sn) = rot(mhz * 1e6, 0.0);
                    re += g * (a * cs - b * sn);
                    im += g * (a * sn + b * cs);
                }
                Comp::FlatWide { mhz, db } => {
                    let (a, b) = self.flat(c, n, 27, mhz * 1e6 - lo_hz);
                    let g = amp(db, noise);
                    re += g * a;
                    im += g * b;
                }
                Comp::FmVideo { mhz, db } => {
                    // Instantaneous frequency: carrier + 8 MHz per unit of
                    // composite (sync -0.3, picture 0..0.7 ramping across
                    // the line). Phase integrated sample by sample.
                    let inst = |n: u64| -> f64 {
                        let t_us = n as f64 / 40.0;
                        let x = t_us % 64.0;
                        let line = (t_us / 64.0) as u64;
                        let v = if x < 4.7 {
                            -0.3
                        } else if x < 10.4 {
                            0.0
                        } else {
                            0.1 + 0.3 * ((line % 300) as f64 / 300.0) + 0.3 * (x - 10.4) / 53.6
                        };
                        mhz * 1e6 - lo_hz + 8e6 * v
                    };
                    let (mut k, mut ph) = self.fm;
                    if n < k || n - k > 1_000_000 {
                        // Going back (the ring after the probes) or far
                        // ahead: a phase jump there is harmless.
                        k = n;
                    }
                    while k < n {
                        ph += 2.0 * PI * inst(k) / FS;
                        k += 1;
                    }
                    self.fm = (k, ph % (2.0 * PI));
                    let g = lpf(inst(n)) * amp(db, noise);
                    re += g * ph.cos();
                    im += g * ph.sin();
                }
                Comp::Oven { mhz, db, wide } => {
                    let cyc = 1.0 / 60.0;
                    if t % cyc >= cyc / 2.0 {
                        continue;
                    }
                    let f = mhz - lo as f64;
                    if wide {
                        let (a, b) = self.flat(c, n, 24, f * 1e6);
                        let g = amp(db, noise);
                        re += g * a;
                        im += g * b;
                    } else {
                        // The magnetron's frequency wanders with the anode
                        // voltage over the half cycle.
                        let wander = 2.0e6 * (2.0 * PI * t / cyc).sin();
                        let fhz = mhz * 1e6 + wander;
                        let g = lpf(fhz - lo_hz) * amp(db, noise);
                        let ph = 2.0 * PI * ((mhz * 1e6 - lo_hz) * t - 2.0e6 * cyc / (2.0 * PI) * (2.0 * PI * t / cyc).cos());
                        re += g * ph.cos();
                        im += g * ph.sin();
                    }
                }
            }
        }
        (re, im)
    }

    /// Sample `n` of component `c`'s flat signal (`half` carriers a side),
    /// unit power, carriers beyond the low-pass dropped.
    fn flat(&mut self, c: usize, n: u64, half: i32, off_hz: f64) -> (f64, f64) {
        // Symbol and offset (one LO per dwell) key the cache.
        let sym = (n / 128) as i64 ^ ((off_hz as i64) << 40);
        if self.cache[c].0 != sym {
            self.cache[c] = (sym, flat_symbol(hash(c as u64 + 99, n / 128), half, off_hz));
        }
        self.cache[c].1[(n % 128) as usize]
    }

    /// The 4-bit byte for sample `n` with receiver noise.
    fn byte(&mut self, n: u64, lo: u16) -> u8 {
        let noise = if lo < 4000 { N24 } else { N58 };
        let (re, im) = self.sample(n, lo, noise);
        let w = Window::CLIP;
        let i = (re + noise * self.rng.g()).floor().clamp(-512.0, 511.0) as i16;
        let q = (im + noise * self.rng.g()).floor().clamp(-512.0, 511.0) as i16;
        pack(w.quantise(i), w.quantise(q))
    }
}

fn v(b: u8) -> (i8, i8) {
    let (i, q) = unpack(b);
    (2 * i + 1, 2 * q + 1)
}

/// The ring's newest `len` bytes of a scene at `lo`, ending at sample `end`.
fn ring(scene: &mut Scene, lo: u16, end: u64, len: usize) -> Vec<u8> {
    (end - len as u64..end).map(|n| scene.byte(n, lo)).collect()
}

fn spec_of(bytes: &[u8]) -> Features {
    let mut s = Box::new(Scratch::new());
    analyse(|k| v(bytes[k]), bytes.len(), 15, &mut s).unwrap()
}

/// `fpvscan`'s verdict on 4096 half-banded samples of `bytes` from `at`.
fn link_check(bytes: &[u8], at: usize, lo: u16) -> bool {
    const N: usize = 4096;
    let need = 2 * N + 2 * HB_REACH;
    let at = at.saturating_sub(N / 2).min(bytes.len() - need);
    let mut cap = vec![C16::default(); N];
    decimate_nibbles(|k| bytes[at + k], &mut cap);
    remove_dc(&mut cap);
    let mut sc = Box::new(fpvscan::Scratch::new());
    let a = fpvscan::analyse(&cap, &mut sc);
    let e = fpvscan::band::extra(&cap, &sc, &a, lo + 1);
    link_from_analysis(&a, e.dsss)
}

struct Sim {
    w: Watch,
    t_ms: u64,
    probes: usize,
    reports: Vec<Report>,
    times: Vec<(u64, Kind)>,
    verdicts: Vec<HopVerdict>,
    sc: Box<Scratch>,
}

impl Sim {
    fn new(probes: usize) -> Self {
        Sim { w: Watch::new(), t_ms: 1000, probes, reports: vec![], times: vec![], verdicts: vec![], sc: Box::new(Scratch::new()) }
    }

    /// One dwell of `dwell_ms` on hop `freq` (LO `lo`), as the firmware does
    /// it: probes, spectrogram, assess, link check if asked, commit, due.
    fn dwell(&mut self, scene: &mut Scene, freq: u32, lo: u16, dwell_ms: u64) -> HopVerdict {
        let start = self.t_ms * 40_000;
        let mut ps = ProbeStats::new(51_200);
        ps.begin(self.w.loud_x10(freq));
        for p in 0..self.probes as u64 {
            let s = start + p * BLOCK;
            let pw: u32 = (s..s + PROBE).map(|n| POWER[scene.byte(n, lo) as usize] as u32).sum();
            ps.feed(pw);
        }
        let end = start + self.probes as u64 * BLOCK;
        let bytes = ring(scene, lo, end, RING);
        let f = analyse(|k| v(bytes[k]), bytes.len(), 15, &mut self.sc).unwrap();
        self.t_ms += dwell_ms;
        let a = self.w.assess(freq, lo, &ps, Some(&f)).unwrap();
        let link = a.needs_link_check().then(|| {
            let at = if a.pulsed { f.pulse_at(bytes.len())? } else { bytes.len() };
            Some(link_check(&bytes, at, lo))
        });
        let vd = self.w.commit(&a, link.flatten(), self.t_ms);
        let mut out = heapless::Vec::new();
        self.w.due(self.t_ms, &mut out);
        self.times.extend(out.iter().map(|r| (self.t_ms, r.kind)));
        self.reports.extend(out);
        self.verdicts.push(vd);
        vd
    }

    fn cycle(&mut self, scene: &mut Scene, plan: &[u32], dwell_ms: u64) {
        for &f in plan {
            self.dwell(scene, f, (f / 10) as u16, dwell_ms);
        }
    }

    fn of(&self, k: Kind) -> Vec<&Report> {
        self.reports.iter().filter(|r| r.kind == k).collect()
    }

    fn clear(&mut self) {
        self.reports.clear();
        self.verdicts.clear();
    }
}

/// The node's built-in DroneID plan (firmware/node-sdr `DRONEID_PLAN`).
const DRONEID: [u32; 13] = [57465, 57565, 57665, 57765, 57865, 57965, 58065, 58165, 24145, 24295, 24445, 24595, 23995];

/// A sim that has learnt a quiet environment for `cycles` rounds.
fn learnt(cycles: usize, probes: usize) -> Sim {
    let mut sim = Sim::new(probes);
    sim.w.set_cycle_ms(13 * 700);
    let mut q = Scene::quiet();
    for _ in 0..cycles {
        sim.cycle(&mut q, &DRONEID, 700);
    }
    sim.clear();
    sim
}

fn barrage(db: f64) -> Comp {
    Comp::Noise { db, mhz: (0, 9999), gate: None }
}

// ------------------------------------------------------------- pieces --

#[test]
fn fft_matches_a_dft() {
    let mut r = Rng(7);
    let x: Vec<(i8, i8)> = (0..NFFT).map(|_| ((r.u() * 31.0 - 15.0) as i8, (r.u() * 31.0 - 15.0) as i8)).collect();
    // One frame through the module's path: a pure tone at bin 5 comes out
    // at offset 5, and the spectrum of noise matches a float DFT's shape.
    let tone: Vec<u8> = (0..spectrum::MIN_LEN)
        .map(|n| {
            let ph = 2.0 * PI * 5.0 * n as f64 / NFFT as f64;
            pack(((6.0 * ph.cos()).round() as i8).clamp(-8, 7), ((6.0 * ph.sin()).round() as i8).clamp(-8, 7))
        })
        .collect();
    let mut s = Box::new(Scratch::new());
    let f = analyse(|k| v(tone[k]), tone.len(), 15, &mut s).unwrap();
    let t = f.tone.expect("tone");
    assert_eq!(t.offset_khz, 5 * spectrum::BIN_KHZ);
    assert!(f.sweep.is_none());
    // A float DFT of x, Hann-windowed, against the module's averaged bins
    // for a ring that repeats x.
    let rep: Vec<(i8, i8)> = (0..spectrum::MIN_LEN).map(|n| x[n % NFFT]).collect();
    analyse(|k| rep[k], rep.len(), 15, &mut s).unwrap();
    for k in 0..NFFT {
        let (mut re, mut im) = (0.0, 0.0);
        for (n, &(a, b)) in x.iter().enumerate() {
            let w = 0.5 * (1.0 - (2.0 * PI * n as f64 / NFFT as f64).cos());
            let (sn, cs) = (-2.0 * PI * (k * n) as f64 / NFFT as f64).sin_cos();
            re += w * (a as f64 * cs - b as f64 * sn);
            im += w * (a as f64 * sn + b as f64 * cs);
        }
        // Module scale: x * 2^8 per sample, squared, times 128 frames.
        let want = (re * re + im * im) * 65536.0 * 128.0;
        let o = if k < 32 { k as i32 } else { k as i32 - 64 };
        let got = s.avg(o) as f64;
        assert!((got - want).abs() <= want * 0.02 + 2e7, "bin {k}: {got} vs {want}");
    }
}

#[test]
fn range_left_table() {
    for x10 in 0..=400 {
        let want = 100.0 * 10f64.powf(-(x10 as f64) / 200.0);
        let got = range_left_pct(x10) as f64;
        assert!((got - want).abs() <= 1.0, "{x10}: {got} vs {want:.2}");
    }
    assert_eq!(range_left_pct(-50), 100);
    assert_eq!(range_left_pct(600), 1);
}

#[test]
fn probe_runs_and_duty() {
    let mut p = ProbeStats::new(51_200);
    p.begin(400);
    // 40 probes quiet, 10 loud, repeated 4 times.
    for _ in 0..4 {
        for _ in 0..40 {
            p.feed(1300);
        }
        for _ in 0..10 {
            p.feed(30_000);
        }
    }
    assert_eq!(p.runs(), 4);
    assert_eq!(p.loud_pct(), 20);
    assert_eq!(p.mean_on_us(), 512);
    assert!((p.percentile_x10(10) - 311).abs() <= 5, "{}", p.percentile_x10(10));
}

#[test]
fn receiver_noise_matches_the_expected_floor() {
    // The REF_FLOOR constants are what the simulated receiver noise gives.
    for (lo, want) in [(5756u16, watch::REF_FLOOR_X10[1]), (2414, watch::REF_FLOOR_X10[0])] {
        let mut s = Scene::quiet();
        let mut ps = ProbeStats::new(51_200);
        ps.begin(999);
        for p in 0..400u64 {
            ps.feed((p * BLOCK..p * BLOCK + PROBE).map(|n| POWER[s.byte(n, lo) as usize] as u32).sum());
        }
        let p10 = ps.percentile_x10(10);
        assert!((p10 - want).abs() <= 15, "LO {lo}: p10 {p10} vs {want}");
    }
}

// ----------------------------------------------------------- spectrum --

#[test]
fn spectrum_of_receiver_noise_is_flat_and_featureless() {
    let mut s = Scene::quiet();
    let f = spec_of(&ring(&mut s, 5756, 1 << 20, RING));
    assert!(f.occupancy_pct >= 90, "{f:?}");
    assert!(f.tone.is_none() && f.sweep.is_none(), "{f:?}");
    assert_eq!(f.clip_ppm, 0);
    assert!(f.carrier_pct <= 10, "{f:?}");
}

#[test]
fn spectrum_of_a_barrage_jammer_is_full_and_clips() {
    let mut s = Scene::new(vec![barrage(18.0)], 3);
    let f = spec_of(&ring(&mut s, 5756, 1 << 20, RING));
    assert!(f.occupancy_pct >= 90, "{f:?}");
    assert!(f.clip_ppm >= watch::CLIP_PPM, "{f:?}");
    assert!(f.tone.is_none() && f.sweep.is_none(), "{f:?}");
}

#[test]
fn spectrum_finds_a_tone() {
    for (off, db) in [(3.0, 20.0), (-5.6, 15.0), (7.2, 25.0)] {
        let mut s = Scene::new(vec![Comp::Tone { mhz: 5756.0 + off, db }], 5);
        let f = spec_of(&ring(&mut s, 5756, 1 << 20, RING));
        let t = f.tone.unwrap_or_else(|| panic!("no tone at {off}: {f:?}"));
        assert!((t.offset_khz - (off * 1000.0) as i32).abs() <= spectrum::BIN_KHZ, "{t:?}");
        assert!(f.sweep.is_none(), "{f:?}");
        assert!(f.occupancy_pct < 50, "{f:?}");
    }
}

#[test]
fn spectrum_finds_sweeps_and_their_rate() {
    // MHz/s: slow (spread frames), medium (both), fast (contiguous frames).
    for (rate, seed) in [(2.0e4, 1), (5.0e4, 2), (3.0e5, 3), (1.0e6, 4)] {
        let mut hits = 0;
        for k in 0..6u64 {
            let mut s = Scene::new(vec![Comp::Chirp { lo: 5700.0, hi: 5820.0, rate, db: 20.0 }], seed * 10 + k);
            // When the carrier crosses the passband (LO 5756 MHz), at
            // different points of its crossing.
            let t_in = (5748.0 - 5700.0) / rate;
            let end = (t_in * FS) as u64 + RING as u64 + k * 997;
            let f = spec_of(&ring(&mut s, 5756, end, RING));
            if let Some(sw) = f.sweep {
                hits += 1;
                let err = (sw.rate_mhz_s as f64 - rate).abs() / rate;
                assert!(err < 0.3, "rate {rate}: measured {sw:?}");
                assert!(sw.rising);
                assert!(f.tone.is_none());
            }
        }
        assert!(hits >= 2, "rate {rate}: only {hits} of 6 windows saw the sweep");
    }
}

#[test]
fn a_narrow_signal_does_not_fill_the_passband() {
    let mut s = Scene::new(vec![Comp::FlatWide { mhz: 5756.0 + 9.0, db: 15.0 }], 9);
    let f = spec_of(&ring(&mut s, 5756, 1 << 20, RING));
    assert!(f.occupancy_pct < watch::OCC_PCT, "{f:?}");
}

#[test]
fn link_check_tells_ofdm_from_noise() {
    let mut s = Scene::new(vec![Comp::Ofdm { mhz: 5757.0, db: 15.0, gate: None, seed: 1 }], 1);
    let b = ring(&mut s, 5756, 1 << 20, RING);
    assert!(link_check(&b, b.len(), 5756));
    // A drone right next to the node: clipped, the prefix survives.
    let mut s = Scene::new(vec![Comp::Ofdm { mhz: 5757.0, db: 30.0, gate: None, seed: 2 }], 2);
    let b = ring(&mut s, 5756, 1 << 20, RING);
    assert!(spec_of(&b).clip_ppm >= watch::CLIP_PPM);
    assert!(link_check(&b, b.len(), 5756));
    // Noise of the same power, and a jammer drowning the link.
    for comps in [vec![barrage(15.0)], vec![barrage(20.0), Comp::Ofdm { mhz: 5757.0, db: 5.0, gate: None, seed: 3 }]] {
        let mut s = Scene::new(comps, 3);
        let b = ring(&mut s, 5756, 1 << 20, RING);
        assert!(!link_check(&b, b.len(), 5756));
    }
}

// -------------------------------------------------------------- watch --

#[test]
fn quiet_bands_report_nothing() {
    let mut sim = learnt(1, 200);
    let mut q = Scene::quiet();
    for _ in 0..3 {
        sim.cycle(&mut q, &DRONEID, 700);
    }
    assert!(sim.reports.is_empty(), "{:?}", sim.reports);
    assert!(sim.verdicts.iter().all(|v| !v.notable()), "{:?}", sim.verdicts.iter().find(|v| v.notable()));
}

#[test]
fn barrage_on_both_bands_is_broadband_and_blinding() {
    let mut sim = learnt(1, 200);
    let mut j = Scene::new(vec![barrage(15.0)], 11);
    for _ in 0..2 {
        sim.cycle(&mut j, &DRONEID, 700);
    }
    let bb = sim.of(Kind::Broadband);
    assert!(bb.iter().any(|r| r.band == Band::G58) && bb.iter().any(|r| r.band == Band::G24), "{:?}", sim.reports);
    let r = bb.iter().find(|r| r.band == Band::G58).unwrap();
    assert!((57465..=58165).contains(&r.freq) && r.width_khz >= 50_000 && r.level_db >= 8, "{r:?}");
    let bl = sim.of(Kind::Blinded);
    assert!(!bl.is_empty(), "{:?}", sim.reports);
    assert!(bl.iter().all(|r| r.detail <= 25 && r.level_db >= 12), "{bl:?}");
    // Detected within the first round of hops: the first raised hops count.
    let first = sim.reports.iter().find(|r| r.kind == Kind::Broadband).unwrap();
    assert!(first.window_ms <= 13 * 700, "{first:?}");
    // No false tones, sweeps or pulses.
    assert!(sim.of(Kind::Tone).is_empty() && sim.of(Kind::Sweep).is_empty() && sim.of(Kind::Pulsed).is_empty(), "{:?}", sim.reports);
}

#[test]
fn reports_are_rate_limited_per_kind() {
    let mut sim = learnt(1, 150);
    let mut j = Scene::new(vec![barrage(15.0)], 12);
    // Dwells of 100 ms: ten verdicts a second, still one report per kind per second.
    for _ in 0..3 {
        sim.cycle(&mut j, &DRONEID, 100);
    }
    assert!(!sim.times.is_empty());
    for k in Kind::ALL {
        let ts: Vec<u64> = sim.times.iter().filter(|t| t.1 == k).map(|t| t.0).collect();
        assert!(ts.windows(2).all(|w| w[1] - w[0] >= watch::EMIT_MS), "{k:?}: {ts:?}");
    }
}

#[test]
fn booting_into_a_jammed_band_still_reports() {
    // No quiet learning first: the jammer is there from the first dwell.
    let mut sim = Sim::new(200);
    sim.w.set_cycle_ms(13 * 700);
    let mut j = Scene::new(vec![barrage(15.0)], 13);
    for _ in 0..2 {
        sim.cycle(&mut j, &DRONEID, 700);
    }
    assert!(!sim.of(Kind::Broadband).is_empty(), "{:?}", sim.reports);
    assert!(!sim.of(Kind::Blinded).is_empty(), "{:?}", sim.reports);
}

#[test]
fn booting_into_a_jammed_24_band_compares_against_58() {
    // Only 2.4 GHz jammed, from boot, by less than the absolute ceiling
    // would show on its own: the clean 5.8 GHz band bounds its floor.
    let mut sim = Sim::new(200);
    sim.w.set_cycle_ms(13 * 700);
    let mut j = Scene::new(vec![Comp::Noise { db: 15.0, mhz: (2000, 3000), gate: None }], 14);
    for _ in 0..3 {
        sim.cycle(&mut j, &DRONEID, 700);
    }
    assert!(sim.of(Kind::Broadband).iter().any(|r| r.band == Band::G24), "{:?}", sim.reports);
    assert!(sim.of(Kind::Broadband).iter().all(|r| r.band == Band::G24), "{:?}", sim.reports);
}

#[test]
fn a_long_jam_is_not_learnt_into_the_baseline() {
    let mut sim = learnt(1, 150);
    // Dwells 30 s apart: 6 rounds of 13 hops is 39 minutes of jamming.
    sim.w.set_cycle_ms(13 * 30_000);
    let mut j = Scene::new(vec![barrage(15.0)], 15);
    for _ in 0..5 {
        sim.cycle(&mut j, &DRONEID, 30_000);
    }
    sim.clear();
    sim.cycle(&mut j, &DRONEID, 30_000);
    assert!(sim.reports.iter().any(|r| r.kind == Kind::Broadband), "the jammer was learnt into the baseline");
}

#[test]
fn a_drone_video_link_is_not_jamming() {
    // A continuous 20 MHz OFDM link with a cyclic prefix, loud (and a
    // second one right next to the node, clipping) on 5.8 GHz.
    let mut sim = learnt(1, 200);
    let mut s = Scene::new(
        vec![
            Comp::Ofdm { mhz: 5776.5, db: 18.0, gate: None, seed: 21 },
            Comp::Ofdm { mhz: 5806.5, db: 32.0, gate: None, seed: 22 },
        ],
        21,
    );
    for _ in 0..2 {
        sim.cycle(&mut s, &DRONEID, 700);
    }
    assert!(sim.reports.is_empty(), "{:?}", sim.reports);
    let near: Vec<_> = sim.verdicts.iter().filter(|v| v.freq == 57765 || v.freq == 58065).collect();
    assert!(near.iter().all(|v| v.link && !v.raised), "{near:?}");
}

#[test]
fn a_drone_link_next_to_a_barrage_is_still_jamming() {
    let mut sim = learnt(1, 200);
    let mut s = Scene::new(vec![barrage(15.0), Comp::Ofdm { mhz: 5776.5, db: 30.0, gate: None, seed: 23 }], 23);
    for _ in 0..2 {
        sim.cycle(&mut s, &DRONEID, 700);
    }
    assert!(sim.of(Kind::Broadband).iter().any(|r| r.band == Band::G58), "{:?}", sim.reports);
}

#[test]
fn busy_wifi_is_not_jamming() {
    // Bursty OFDM on 2.4 GHz channels 1/6/11 and 5.8 GHz 149/157, near and
    // loud, half the time on air.
    let mut comps = vec![];
    for (k, mhz) in [2412.0, 2437.0, 2462.0, 5745.0, 5785.0].into_iter().enumerate() {
        comps.push(Comp::Ofdm { mhz, db: 22.0, gate: Some((0.0013 + 0.0002 * k as f64, 0.0006)), seed: 30 + k as u64 });
    }
    let mut sim = learnt(1, 400);
    let mut s = Scene::new(comps, 30);
    for _ in 0..3 {
        sim.cycle(&mut s, &DRONEID, 700);
    }
    let bad: Vec<_> = sim.reports.iter().filter(|r| r.kind != Kind::Blinded).collect();
    assert!(bad.is_empty(), "{bad:?}");
    assert!(sim.of(Kind::Blinded).is_empty(), "{:?}", sim.reports);
}

#[test]
fn one_wide_flat_transmitter_is_not_broadband() {
    // HDZero-like (flat, no prefix) on one channel: raises a hop or two,
    // never a band.
    let mut sim = learnt(1, 200);
    let mut s = Scene::new(vec![Comp::FlatWide { mhz: 5776.5, db: 18.0 }], 40);
    for _ in 0..3 {
        sim.cycle(&mut s, &DRONEID, 700);
    }
    assert!(sim.of(Kind::Broadband).is_empty(), "{:?}", sim.reports);
    assert!(sim.of(Kind::Blinded).is_empty(), "{:?}", sim.reports);
}

#[test]
fn a_cw_tone_is_reported_as_a_tone() {
    let mut sim = learnt(1, 200);
    let mut s = Scene::new(vec![Comp::Tone { mhz: 5779.0, db: 22.0 }], 50);
    for _ in 0..2 {
        sim.cycle(&mut s, &DRONEID, 700);
    }
    let t = sim.of(Kind::Tone);
    assert!(!t.is_empty(), "{:?} / {:?}", sim.reports, sim.verdicts.iter().filter(|v| v.notable()).collect::<Vec<_>>());
    assert!(t.iter().all(|r| r.band == Band::G58 && r.freq.abs_diff(57790) <= 10), "{t:?}");
    assert!(sim.of(Kind::Broadband).is_empty(), "{:?}", sim.reports);
}

#[test]
fn a_sweeping_jammer_is_reported_with_its_rate() {
    let rate = 1.0e5;
    let mut sim = learnt(1, 200);
    let mut s = Scene::new(vec![Comp::Chirp { lo: 5730.0, hi: 5830.0, rate, db: 22.0 }], 60);
    for _ in 0..3 {
        sim.cycle(&mut s, &DRONEID, 700);
    }
    let sw = sim.of(Kind::Sweep);
    assert!(!sw.is_empty(), "{:?}", sim.reports);
    for r in &sw {
        let err = (r.detail as f64 - rate).abs() / rate;
        assert!(err < 0.3, "{r:?}");
    }
    assert!(sim.of(Kind::Broadband).is_empty(), "{:?}", sim.reports);
}

#[test]
fn pulsed_broadband_interference_is_reported() {
    // 1 ms of +22 dB noise every 4.1 ms on 5.8 GHz.
    let mut sim = learnt(1, 400);
    let mut s = Scene::new(vec![Comp::Noise { db: 22.0, mhz: (5000, 6000), gate: Some((0.0041, 0.001)) }], 70);
    for _ in 0..3 {
        sim.cycle(&mut s, &DRONEID, 700);
    }
    let p = sim.of(Kind::Pulsed);
    assert!(!p.is_empty(), "{:?} / {:?}", sim.reports, sim.verdicts.iter().filter(|v| v.notable()).collect::<Vec<_>>());
    assert!(p.iter().all(|r| r.band == Band::G58 && (15..=40).contains(&r.detail)), "{p:?}");
    assert!(sim.of(Kind::Broadband).is_empty(), "{:?}", sim.reports);
}

#[test]
fn microwave_ovens_are_not_jamming() {
    for wide in [false, true] {
        let mut sim = learnt(1, 1200);
        let mut s = Scene::new(vec![Comp::Oven { mhz: 2452.0, db: 25.0, wide }], 80);
        for _ in 0..2 {
            sim.cycle(&mut s, &DRONEID, 700);
        }
        assert!(sim.reports.is_empty(), "wide {wide}: {:?}", sim.reports);
        let seen = sim.verdicts.iter().filter(|v| v.oven).count();
        assert!(seen >= 1, "wide {wide}: the oven gate never fired");
    }
}

#[test]
fn reports_map_to_link_frames() {
    let r = Report { kind: Kind::Blinded, band: Band::G58, freq: 57815, width_khz: 87_500, level_db: 15, count: 8, window_ms: 9100, detail: 18 };
    let s = r.to_sighting(123);
    assert_eq!(s.kind, link::ThreatKind::Blinded);
    assert!(s.kind.is_jamming());
    let mut buf = [0u8; link::MAX_FRAME];
    let wire = link::encode(&link::Frame::Threat(s.clone()), &mut buf).unwrap().to_vec();
    let mut dec = link::Decoder::new();
    let mut ok = false;
    dec.feed(&wire, |f| ok = f.as_ref().is_ok_and(|g| *g == link::Frame::Threat(s.clone())));
    assert!(ok);
    for k in Kind::ALL {
        assert!(k.threat().is_jamming());
    }
}

#[test]
fn analog_video_is_not_a_sweep() {
    let mut sim = learnt(1, 200);
    let mut s = Scene::new(vec![Comp::FmVideo { mhz: 5777.0, db: 18.0 }], 90);
    for _ in 0..3 {
        sim.cycle(&mut s, &DRONEID, 700);
    }
    let bad: Vec<_> = sim.reports.iter().filter(|r| r.kind != Kind::Blinded).collect();
    assert!(bad.is_empty(), "{bad:?}");
}

#[test]
fn noise_never_looks_like_a_tone_or_sweep() {
    // Receiver noise and barrages of many levels, many windows each.
    let mut sc = Box::new(Scratch::new());
    for (k, db) in [None, Some(3.0), Some(8.0), Some(14.0), Some(20.0), Some(30.0)].into_iter().enumerate() {
        let mut s = Scene::new(db.map(barrage).into_iter().collect(), 100 + k as u64);
        for w in 0..30u64 {
            let b = ring(&mut s, 5756, (w + 1) * 100_000, RING);
            let f = analyse(|k| v(b[k]), b.len(), 15, &mut sc).unwrap();
            assert!(f.tone.is_none() && f.sweep.is_none(), "{db:?}: {f:?}");
            assert!(f.occupancy_pct >= 85, "{db:?}: {f:?}");
        }
    }
}
