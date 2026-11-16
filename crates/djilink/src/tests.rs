//! Synthetic signals through the C5's real 4-bit path (as `fpvscan`'s
//! tests): complex baseband at 40 MS/s plus receiver noise, quantised by
//! the clip window (`rawcap::iq`), half-banded to 20 MS/s (`rawcap::dsp`),
//! DC removed, then measured and classified.
//!
//! The OFDM signals are built the way the documented numerologies are
//! defined: an inverse FFT per symbol at the system's own rate (30.72 MS/s
//! with 2048 points for DJI 20 MHz video, 15.36/1024 for 10 MHz video and
//! DroneID, 1.92/128 and 3.84/256 for the uplinks), a cyclic prefix of the
//! documented length, then band-limited interpolation to 40 MS/s. These are
//! models of the published parameters, not recordings: they show the
//! detectors do what they are designed to, not that DJI's real signals
//! match the design (docs/DJI-LINKS.md says which numbers are measured).

use std::f64::consts::PI;

use droneid::math::{C16, remove_dc};
use rawcap::dsp::{HB_REACH, decimate_nibbles};
use rawcap::iq::{Window, pack};

use crate::classify::{Context, Verdict, centre_khz, classify};
use crate::{Features, HopTracker, Scratch, Timeline, features, plan};

const FS40: f64 = 40e6;
const OUT: usize = 16_384;
const IN: usize = 2 * OUT + 2 * HB_REACH;
/// Receiver noise at gain 83, codes RMS per axis (BRINGUP-RESULTS.md).
const NOISE: f64 = 48.0;

type Sig = Vec<(f64, f64)>;

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
    fn qpsk(&mut self) -> (f64, f64) {
        let s = std::f64::consts::FRAC_1_SQRT_2;
        (if self.u() < 0.5 { s } else { -s }, if self.u() < 0.5 { s } else { -s })
    }
}

fn quantise(sig: &[(f64, f64)], seed: u64) -> Vec<u8> {
    let mut rng = Rng(seed ^ 0x5EED);
    let w = Window::CLIP;
    sig.iter()
        .map(|&(i, q)| {
            let i = (i + NOISE * rng.g()).floor().clamp(-512.0, 511.0) as i16;
            let q = (q + NOISE * rng.g()).floor().clamp(-512.0, 511.0) as i16;
            pack(w.quantise(i), w.quantise(q))
        })
        .collect()
}

fn capture(sig: &[(f64, f64)], seed: u64) -> Vec<C16> {
    let bytes = quantise(sig, seed);
    let mut out = vec![C16::default(); OUT];
    decimate_nibbles(|k| bytes[k], &mut out);
    remove_dc(&mut out);
    out
}

fn run(sig: &[(f64, f64)], seed: u64) -> Features {
    let cap = capture(sig, seed);
    let mut s = Box::new(Scratch::new());
    features(&cap, &mut s)
}

// ------------------------------------------------------------- synthesis --

fn fft_inplace(a: &mut [(f64, f64)], inverse: bool) {
    let n = a.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            a.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = 2.0 * PI / len as f64 * if inverse { 1.0 } else { -1.0 };
        let (ws, wc) = ang.sin_cos();
        for i in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0, 0.0);
            for k in 0..len / 2 {
                let (ur, ui) = a[i + k];
                let (vr, vi) = a[i + k + len / 2];
                let (tr, ti) = (vr * cr - vi * ci, vr * ci + vi * cr);
                a[i + k] = (ur + tr, ui + ti);
                a[i + k + len / 2] = (ur - tr, ui - ti);
                let ncr = cr * wc - ci * ws;
                ci = cr * ws + ci * wc;
                cr = ncr;
            }
        }
        len <<= 1;
    }
}

/// An OFDM numerology at its own sample rate.
#[derive(Clone)]
struct Ofdm {
    nfft: usize,
    /// Subcarrier spacing, Hz (fs = nfft * spacing).
    spacing: f64,
    /// Occupied subcarriers k = -half..=half (DC included, as DJI's
    /// constant DC carrier).
    half: i32,
    /// Prefix length per symbol of one packet, samples at fs.
    cps: Vec<usize>,
}

impl Ofdm {
    fn fs(&self) -> f64 {
        self.nfft as f64 * self.spacing
    }
    fn dur_s(&self) -> f64 {
        self.cps.iter().map(|&c| (c + self.nfft) as f64).sum::<f64>() / self.fs()
    }
}

/// DJI video, 20 MHz mode: 2048 points at 30.72 MS/s, 1201 subcarriers,
/// prefix 144 with 160 on symbols 0, 7, 14 of a 15-symbol packet (tmbinc).
fn dji20() -> Ofdm {
    let cps = (0..15).map(|k| if k % 7 == 0 { 160 } else { 144 }).collect();
    Ofdm { nfft: 2048, spacing: 15e3, half: 600, cps }
}
/// DJI video, 10 MHz mode: 1024 at 15.36 MS/s, 601 subcarriers, 72/80.
fn dji10() -> Ofdm {
    let cps = (0..15).map(|k| if k % 7 == 0 { 80 } else { 72 }).collect();
    Ofdm { nfft: 1024, spacing: 15e3, half: 300, cps }
}
/// A wide (40 MHz) mode seen through our 20 MHz: 4096 at 61.44 MS/s, 2401
/// subcarriers, prefix scaled (288/320). O4's real wide numerology is not
/// public; this is the 15 kHz assumption.
fn dji40() -> Ofdm {
    let cps = (0..15).map(|k| if k % 7 == 0 { 320 } else { 288 }).collect();
    Ofdm { nfft: 4096, spacing: 15e3, half: 1200, cps }
}
/// DroneID: 1024 at 15.36 MS/s, 601 subcarriers, 9 symbols 80/72.../80.
fn droneid_burst() -> Ofdm {
    let cps = (0..9).map(|k| if k == 0 || k == 8 { 80 } else { 72 }).collect();
    Ofdm { nfft: 1024, spacing: 15e3, half: 300, cps }
}
/// OcuSync 1/2 uplink ("1.4 MHz-CA"): 128 at 1.92 MS/s, 73 subcarriers,
/// 6 symbols, LTE prefix 10 then 9 (ICTC 2025).
fn uplink_os2() -> Ofdm {
    Ofdm { nfft: 128, spacing: 15e3, half: 36, cps: vec![10, 9, 9, 9, 9, 9] }
}
/// O3 uplink: 256 at 3.84 MS/s, 145 subcarriers, 7 symbols, 20 then 18.
fn uplink_o3() -> Ofdm {
    Ofdm { nfft: 256, spacing: 15e3, half: 72, cps: vec![20, 18, 18, 18, 18, 18, 18] }
}
/// LTE extended prefix, 10 MHz: 1024 at 15.36 MS/s, 256 samples.
fn lte_extended() -> Ofdm {
    Ofdm { nfft: 1024, spacing: 15e3, half: 300, cps: vec![256; 6] }
}
/// 30 kHz numerology, ~18 MHz: 1024 at 30.72 MS/s, 601 subcarriers, 72.
fn nr30() -> Ofdm {
    Ofdm { nfft: 1024, spacing: 30e3, half: 300, cps: vec![72; 14] }
}

/// One packet of `o` at its own rate, unit power.
fn packet(o: &Ofdm, rng: &mut Rng) -> Sig {
    let mut out = vec![];
    let norm = 1.0 / ((2 * o.half + 1) as f64).sqrt();
    for &cp in &o.cps {
        let mut f = vec![(0.0, 0.0); o.nfft];
        for k in -o.half..=o.half {
            let v = if k == 0 { (0.5, 0.5) } else { rng.qpsk() };
            f[k.rem_euclid(o.nfft as i32) as usize] = (v.0 * norm, v.1 * norm);
        }
        fft_inplace(&mut f, true);
        out.extend_from_slice(&f[o.nfft - cp..]);
        out.extend_from_slice(&f);
    }
    out
}

/// Add `x` (sampled at `fs`) into `out` (40 MS/s) starting at `t0_s`,
/// shifted by `f_hz`, scaled by `amp`: windowed-sinc interpolation.
fn add_at(out: &mut Sig, x: &[(f64, f64)], fs: f64, t0_s: f64, f_hz: f64, amp: f64) {
    const HW: i64 = 16;
    let dur = x.len() as f64 / fs;
    let m0 = ((t0_s - HW as f64 / fs) * FS40).floor().max(0.0) as usize;
    let m1 = (((t0_s + dur + HW as f64 / fs) * FS40).ceil() as usize).min(out.len());
    for (m, o) in out.iter_mut().enumerate().take(m1).skip(m0) {
        let t = m as f64 / FS40;
        let p = (t - t0_s) * fs;
        let c = p.floor() as i64;
        let (mut re, mut im) = (0.0, 0.0);
        for i in c - HW + 1..=c + HW {
            if i < 0 || i as usize >= x.len() {
                continue;
            }
            let u = p - i as f64;
            let s = if u.abs() < 1e-12 { 1.0 } else { (PI * u).sin() / (PI * u) };
            let w = 0.5 + 0.5 * (PI * u / HW as f64).cos();
            re += x[i as usize].0 * s * w;
            im += x[i as usize].1 * s * w;
        }
        let (sn, cs) = (2.0 * PI * f_hz * t).sin_cos();
        o.0 += amp * (re * cs - im * sn);
        o.1 += amp * (re * sn + im * cs);
    }
}

fn silence() -> Sig {
    vec![(0.0, 0.0); IN]
}

/// Packets of `o` back to back (gap 0) or separated by `gap_s`, from a
/// random point inside the first.
fn packets(o: &Ofdm, amp: f64, f_hz: f64, gap_s: f64, seed: u64) -> Sig {
    let mut rng = Rng(seed);
    let mut out = silence();
    let step = o.dur_s() + gap_s;
    let mut t = -rng.u() * step;
    while t < IN as f64 / FS40 {
        let p = packet(o, &mut rng);
        add_at(&mut out, &p, o.fs(), t, f_hz, amp);
        t += step;
    }
    out
}

/// Single bursts of `o` at `(t0_s, f_hz)`.
fn bursts(o: &Ofdm, amp: f64, at: &[(f64, f64)], seed: u64) -> Sig {
    let mut rng = Rng(seed);
    let mut out = silence();
    for &(t0, f) in at {
        let p = packet(o, &mut rng);
        add_at(&mut out, &p, o.fs(), t0, f, amp);
    }
    out
}

/// Bursty Wi-Fi (802.11a, or ax data symbols), as `fpvscan`'s tests.
fn wifi(amp: f64, seed: u64, gap_us: (f64, f64), he: bool) -> Sig {
    let mut rng = Rng(seed);
    let (half, nfft, cp) = if he { (122, 256, 16) } else { (26, 64, 16) };
    // 20 MS/s native: 64-point / 3.2 µs or 256-point / 12.8 µs symbols.
    let o = Ofdm { nfft, spacing: 20e6 / nfft as f64, half, cps: vec![cp; 1] };
    let mut out = silence();
    let mut t = rng.u() * gap_us.1 * 1e-6;
    while t < IN as f64 / FS40 {
        let nsym = 8 + (rng.u() * 52.0) as usize;
        let mut frame = vec![];
        for _ in 0..nsym {
            frame.extend(packet(&o, &mut rng));
        }
        add_at(&mut out, &frame, 20e6, t, 0.0, amp);
        t += frame.len() as f64 / 20e6 + (gap_us.0 + rng.u() * (gap_us.1 - gap_us.0)) * 1e-6;
    }
    out
}

/// FM analog video: sync, blanking, a picture from `pic`.
fn fm_video(amp: f64, f0: f64, dev: f64, line_us: f64, pic: &dyn Fn(usize, f64) -> f64) -> Sig {
    let mut ph = 0.0f64;
    (0..IN)
        .map(|n| {
            let t = n as f64 / FS40 * 1e6 + 13.0;
            let line = (t / line_us) as usize;
            let tl = t - line as f64 * line_us;
            let s = line_us / 64.0;
            let v = if tl < 4.7 * s {
                -0.3
            } else if tl < 10.4 * s || tl >= 62.35 * s {
                0.0
            } else {
                pic(line, (tl - 10.4 * s) / (51.95 * s)).clamp(0.0, 0.7)
            };
            ph += 2.0 * PI * (f0 + dev * v) / FS40;
            (amp * ph.cos(), amp * ph.sin())
        })
        .collect()
}

/// GFSK-like narrow hopping bursts (a hobby RC link): 250 kbit/s,
/// +-150 kHz deviation, `burst_us` long, at the given offsets.
fn fsk_bursts(amp: f64, burst_us: f64, at: &[(f64, f64)], seed: u64) -> Sig {
    let mut rng = Rng(seed);
    let mut out = silence();
    for &(t0, f) in at {
        let mut ph = 0.0f64;
        let mut bit = 0.0;
        let n0 = (t0 * FS40) as usize;
        let n = (burst_us * 40.0) as usize;
        for k in 0..n {
            if k % 160 == 0 {
                bit = if rng.u() < 0.5 { -1.0 } else { 1.0 };
            }
            ph += 2.0 * PI * (f + 150e3 * bit) / FS40;
            if let Some(o) = out.get_mut(n0 + k) {
                o.0 += amp * ph.cos();
                o.1 += amp * ph.sin();
            }
        }
    }
    out
}

fn cw(amp: f64, f0: f64) -> Sig {
    (0..IN)
        .map(|n| {
            let ph = 2.0 * PI * f0 * n as f64 / FS40;
            (amp * ph.cos(), amp * ph.sin())
        })
        .collect()
}

/// HDZero-like: continuous, flat, ~17 MHz, no cyclic prefix.
fn flat_wide(amp: f64, seed: u64) -> Sig {
    let o = Ofdm { nfft: 256, spacing: 100e3, half: 85, cps: vec![0; 1] };
    let mut rng = Rng(seed);
    let mut x = vec![];
    while (x.len() as f64) / 25.6e6 < IN as f64 / FS40 + 1e-4 {
        x.extend(packet(&o, &mut rng));
    }
    let mut out = silence();
    add_at(&mut out, &x, o.fs(), 0.0, 0.0, amp);
    out
}

fn ctx(lo_mhz: f64) -> Context {
    Context { lo_khz: (lo_mhz * 1000.0) as u32, timeline: None }
}

fn is_dji(v: Option<Verdict>) -> bool {
    matches!(
        v,
        Some(Verdict::Video | Verdict::O4Video | Verdict::FpvAirUnit | Verdict::ControllerBurst | Verdict::DroneIdBurst)
    )
}

fn show(name: &str, f: &Features) {
    let c = &f.cp;
    println!(
        "{name}: n15 {}-{}={} thr {} cp {} al {} | ext {} thr {} cp {} | n30 {} thr {} | w3 {} w10 {} w20 {} lo {} hi {} c {} clip {}{} snr {} | bursts {} on {}% cont {} longest {} us {:?}",
        c.normal15.main, c.normal15.ctl, c.normal15.metric, c.normal15.thr, c.normal15.cp_samples, c.normal15.aligned,
        c.extended15.metric, c.extended15.thr, c.extended15.cp_samples,
        c.normal30.metric, c.normal30.thr,
        f.spec.w3_khz, f.spec.w10_khz, f.spec.w20_khz, f.spec.lo_khz, f.spec.hi_khz, f.spec.centre_khz,
        f.spec.clip_lo as u8, f.spec.clip_hi as u8, f.spec.snr_x10,
        f.bursts.found, f.bursts.on_pct(), f.bursts.continuous, f.bursts.longest_us(),
        f.bursts.iter().map(|b| (b.len_us(), b.complete(), b.centre_khz, b.width_khz)).collect::<Vec<_>>()
    );
}

// ------------------------------------------------------------------ tests --

#[test]
fn video_20mhz_continuous_and_gapped() {
    for seed in 0..3u64 {
        for (amp, gap) in [(350.0, 0.0), (150.0, 0.0), (350.0, 0.3e-3), (120.0, 0.1e-3)] {
            for f in [0.0, 0.5e6, 1e6] {
                let fe = run(&packets(&dji20(), amp, f, gap, seed), seed + 1);
                show(&format!("dji20 amp {amp} gap {gap} f {f}"), &fe);
                assert!(fe.cp.dji_numerology(), "{fe:?}");
                assert!(fe.spec.w10_khz >= 16_000, "{fe:?}");
                assert_eq!(classify(&fe, &ctx(5735.5)), Some(Verdict::Video));
                // Centre known only when the whole 18 MHz is inside the capture.
                if f == 0.0 && amp >= 150.0 {
                    assert!(!fe.spec.clipped(), "{fe:?}");
                    assert!(fe.spec.centre_khz.abs() < 100, "{fe:?}");
                }
            }
        }
    }
}

#[test]
fn video_10mhz_centre_is_measured() {
    for seed in 0..3u64 {
        for f in [-4.0e6, -0.5e6, 0.5e6, 1.0e6, 3.5e6] {
            for amp in [350.0, 150.0] {
                let fe = run(&packets(&dji10(), amp, f, 0.0, seed), seed + 9);
                show(&format!("dji10 f {f} amp {amp}"), &fe);
                assert!(fe.cp.dji_numerology(), "{fe:?}");
                assert!((8_000..=10_500).contains(&fe.spec.w10_khz), "{fe:?}");
                assert!(!fe.spec.clipped());
                let err = fe.spec.centre_khz - (f / 1000.0) as i32;
                assert!(err.abs() <= 60, "centre off by {err} kHz: {fe:?}");
                assert_eq!(classify(&fe, &ctx(5740.0)), Some(Verdict::Video));
            }
        }
    }
}

#[test]
fn fpv_air_unit_by_whole_mhz_channel() {
    // A 10 MHz-mode link centred on DJI FPV channel 4 (5770.0), LO 5769.
    let fe = run(&packets(&dji10(), 300.0, 1.0e6, 0.0, 4), 5);
    show("v1 ch4", &fe);
    assert_eq!(classify(&fe, &ctx(5769.0)), Some(Verdict::FpvAirUnit));
    // The same signal 1.5 MHz lower is on the OcuSync x.5 raster (5768.5).
    let fe = run(&packets(&dji10(), 300.0, -0.5e6, 0.0, 4), 5);
    assert_eq!(classify(&fe, &ctx(5769.0)), Some(Verdict::Video));
    // A 20 MHz link centred in the capture on channel 3 (5735).
    let fe = run(&packets(&dji20(), 300.0, 0.0, 0.0, 6), 7);
    show("v1 ch3 20 MHz", &fe);
    assert_eq!(classify(&fe, &ctx(5735.0)), Some(Verdict::FpvAirUnit));
    // Clipped (centre unknown): never an air-unit claim.
    let fe = run(&packets(&dji20(), 300.0, 1.0e6, 0.0, 6), 7);
    assert!(fe.spec.clipped());
    assert_eq!(classify(&fe, &ctx(5734.0)), Some(Verdict::Video));
}

#[test]
fn o4_band_video() {
    for (o, f, lo) in [(dji20(), 0.0, 5200.0), (dji10(), 2.0e6, 5230.0), (dji40(), 0.0, 5210.0)] {
        let fe = run(&packets(&o, 300.0, f, 0.0, 11), 12);
        show(&format!("o4 band lo {lo} nfft {}", o.nfft), &fe);
        assert_eq!(classify(&fe, &ctx(lo)), Some(Verdict::O4Video), "{fe:?}");
    }
    // The wide mode on 5.8 GHz fills the capture: plain DJI video.
    let fe = run(&packets(&dji40(), 300.0, 0.0, 0.0, 13), 14);
    assert!(fe.spec.clipped(), "{fe:?}");
    assert_eq!(classify(&fe, &ctx(5794.0)), Some(Verdict::Video));
}

#[test]
fn droneid_burst_is_not_video() {
    for seed in 0..4u64 {
        for (amp, t0, f) in [(300.0, 0.05e-3, 0.5e6), (150.0, 0.12e-3, -1.0e6)] {
            let fe = run(&bursts(&droneid_burst(), amp, &[(t0, f)], seed), seed);
            show(&format!("droneid amp {amp}"), &fe);
            assert!(fe.cp.dji_numerology(), "{fe:?}");
            assert_eq!(classify(&fe, &ctx(5756.0)), Some(Verdict::DroneIdBurst), "{fe:?}");
            // A dwell that is busy all the time is video with a burst in it.
            let busy = Context {
                timeline: Some(crate::TimelineStats { duty_pct: 80, longest_us: 30_000, ..Default::default() }),
                ..ctx(5756.0)
            };
            assert_eq!(classify(&fe, &busy), Some(Verdict::Video));
        }
    }
}

#[test]
fn controller_bursts_os2_and_o3() {
    let mut worst = i32::MAX;
    for seed in 0..6u64 {
        for (name, o) in [("os2", uplink_os2()), ("o3", uplink_o3())] {
            for amp in [300.0, 120.0] {
                for f in [-7.0e6, -2.12e6, 0.88e6, 5.3e6] {
                    let t0 = 0.08e-3 + 0.05e-3 * seed as f64;
                    let fe = run(&bursts(&o, amp, &[(t0, f)], seed), seed + 3);
                    show(&format!("{name} amp {amp} f {f}"), &fe);
                    worst = worst.min(fe.cp.normal15.metric - fe.cp.normal15.thr);
                    assert_eq!(classify(&fe, &ctx(5780.0)), Some(Verdict::ControllerBurst), "{name} {amp} {f}: {fe:?}");
                    let b = fe.bursts.iter().next().unwrap();
                    let c = b.centre_khz.expect("burst centre");
                    assert!((c - (f / 1000.0) as i32).abs() < 150, "burst centre {c} for {f}");
                }
            }
        }
    }
    println!("smallest margin over threshold: {worst}");
}

#[test]
fn controller_hops_inside_one_capture_and_across_dwells() {
    // Two O3 uplink bursts 0.3 ms apart on different channels.
    let sig = bursts(&uplink_o3(), 250.0, &[(0.05e-3, -4.12e6), (0.6e-3, 3.88e6)], 21);
    let fe = run(&sig, 22);
    show("two hops", &fe);
    assert_eq!(classify(&fe, &ctx(5780.0)), Some(Verdict::ControllerBurst));
    let centres: Vec<i32> = fe.bursts.iter().filter_map(|b| b.centre_khz).collect();
    assert_eq!(centres.len(), 2, "{fe:?}");
    let mut h = HopTracker::new();
    let mut confirmed = false;
    for c in &centres {
        confirmed = h.burst((5_780_000 + c) as u32, 1000);
    }
    assert!(confirmed);

    // Across dwells: one centre repeatedly never confirms; a second does.
    let mut h = HopTracker::new();
    for t in 0..5 {
        assert!(!h.burst(5_746_120, t * 60));
    }
    assert!(h.burst(5_750_120, 400));
    // ...but not once the first ones are older than the window.
    let mut h = HopTracker::new();
    h.burst(5_746_120, 0);
    assert!(!h.burst(5_790_120, 10_000));
}

#[test]
fn wifi_is_never_dji() {
    let mut worst = i32::MIN;
    for seed in 0..10u64 {
        for he in [false, true] {
            for gap in [(100.0, 400.0), (20.0, 80.0), (10.0, 20.0)] {
                for amp in [350.0, 150.0] {
                    let fe = run(&wifi(amp, seed * 7 + 1, gap, he), seed + 100);
                    worst = worst.max(fe.cp.normal15.metric - fe.cp.normal15.thr);
                    let v = classify(&fe, &ctx(5745.0));
                    if v.is_some() {
                        show(&format!("wifi he {he} gap {gap:?} amp {amp}"), &fe);
                    }
                    assert!(!is_dji(v), "seed {seed} he {he} {gap:?} {amp}: {v:?}");
                    assert!(!fe.cp.normal15.detected, "{fe:?}");
                }
            }
        }
    }
    println!("Wi-Fi: largest 15 kHz metric over its threshold: {worst}");
}

#[test]
fn analog_video_carriers_and_noise_are_never_dji() {
    let pics: Vec<Box<dyn Fn(usize, f64) -> f64>> = vec![
        Box::new(|_, _| 0.35),
        Box::new(|_, x| (7.0 - (x * 8.0).floor()) * 0.1),
        Box::new(|l, x| {
            let mut r = Rng((l as u64) << 20 | (x * 1000.0) as u64);
            0.7 * r.u()
        }),
    ];
    for (i, pic) in pics.iter().enumerate() {
        for line_us in [64.0, 1e6 / 15_734.264] {
            for (amp, f0) in [(350.0, 1e6), (150.0, -3e6)] {
                let fe = run(&fm_video(amp, f0, 6e6, line_us, &**pic), i as u64);
                let v = classify(&fe, &ctx(5800.0));
                if v.is_some() {
                    show("fm", &fe);
                }
                assert_eq!(v, None, "pic {i} line {line_us} amp {amp}");
            }
        }
    }
    for (name, sig) in [("cw", cw(350.0, 1.3e6)), ("flat wide", flat_wide(300.0, 3)), ("silence", silence())] {
        for seed in 0..4 {
            let fe = run(&sig, seed);
            let v = classify(&fe, &ctx(5800.0));
            if v.is_some() {
                show(name, &fe);
            }
            assert_eq!(v, None, "{name} seed {seed}");
        }
    }
}

#[test]
fn hobby_rc_hopping_is_not_a_dji_controller() {
    let mut worst = i32::MIN;
    for seed in 0..12u64 {
        for (amp, us) in [(300.0, 450.0), (150.0, 1000.0), (300.0, 250.0)] {
            let f = -6e6 + seed as f64 * 1e6;
            let fe = run(&fsk_bursts(amp, us, &[(0.06e-3, f), (0.6e-3, f + 2e6)], seed), seed);
            worst = worst.max(fe.cp.normal15.metric - fe.cp.normal15.thr);
            let v = classify(&fe, &ctx(5780.0));
            if v.is_some() {
                show("fsk", &fe);
            }
            assert!(!is_dji(v), "seed {seed} amp {amp} {us} us: {v:?}");
        }
    }
    println!("FSK: largest 15 kHz metric over its threshold: {worst}");
}

#[test]
fn extended_prefix_and_30khz_are_family_only() {
    for seed in 0..3u64 {
        let fe = run(&packets(&lte_extended(), 300.0, 0.5e6, 0.0, seed), seed);
        show("lte extended", &fe);
        assert!(fe.cp.extended15.detected && fe.cp.extended15.metric > fe.cp.normal15.metric, "{fe:?}");
        assert!(fe.cp.extended15.cp_samples >= 250, "{fe:?}");
        assert_eq!(classify(&fe, &ctx(5745.0)), Some(Verdict::Ofdm15));

        let fe = run(&packets(&nr30(), 300.0, 0.0, 0.0, seed), seed);
        show("nr30", &fe);
        assert!(fe.cp.normal30.detected, "{fe:?}");
        assert!(!fe.cp.normal15.detected, "{fe:?}");
        assert_eq!(classify(&fe, &ctx(5745.0)), Some(Verdict::Ofdm30));

        // DJI's normal prefix reads as about 4.7 µs (94 samples).
        let fe = run(&packets(&dji10(), 300.0, 0.0, 0.0, seed), seed);
        assert!((64..=128).contains(&fe.cp.normal15.cp_samples), "{fe:?}");
    }
}

#[test]
fn frequency_offset_does_not_matter() {
    // The prefix metric uses only the magnitude of the folded sum, so a
    // carrier offset (transmitter or our crystal) changes nothing but the
    // measured centre.
    for cfo in [-250e3, -60e3, 0.0, 90e3, 240e3] {
        let fe = run(&packets(&dji10(), 250.0, 1.0e6 + cfo, 0.0, 3), 4);
        assert!(fe.cp.dji_numerology(), "cfo {cfo}: {fe:?}");
        assert!((fe.spec.centre_khz - (1000.0 + cfo / 1000.0) as i32).abs() <= 60, "cfo {cfo}: {fe:?}");
        let fe = run(&bursts(&uplink_os2(), 250.0, &[(0.1e-3, -3e6 + cfo)], 5), 6);
        assert_eq!(classify(&fe, &ctx(5780.0)), Some(Verdict::ControllerBurst), "cfo {cfo}: {fe:?}");
    }
}

#[test]
fn weak_signals_degrade_to_nothing_not_to_a_wrong_kind() {
    // Below what the detectors can call, the answer must be None or the
    // right kind, never another kind.
    for amp in [80.0, 50.0, 30.0] {
        for seed in 0..3u64 {
            let v = classify(&run(&packets(&dji20(), amp, 0.0, 0.0, seed), seed), &ctx(5735.5));
            assert!(matches!(v, None | Some(Verdict::Video | Verdict::Ofdm15)), "video amp {amp}: {v:?}");
            let v = classify(&run(&bursts(&uplink_os2(), amp, &[(0.1e-3, 2e6)], seed), seed), &ctx(5780.0));
            assert!(matches!(v, None | Some(Verdict::ControllerBurst | Verdict::Ofdm15)), "uplink amp {amp}: {v:?}");
        }
    }
}

#[test]
fn timeline_shapes() {
    // Probe levels: floor 30.0 dB; "on" probes at 45 dB.
    let floor = 300;
    let on = 10u32.pow(4) * 3; // 44.8 dB
    let off = 1000u32; // 30 dB
    let mut t = Timeline::new();
    // DroneID: one 0.6 ms burst (12 probes) in a 60 ms dwell.
    for k in 0..1170 {
        t.feed(if (500..512).contains(&k) { on } else { off });
    }
    let s = t.stats(floor);
    assert_eq!((s.bursts, s.duty_pct), (1, 1));
    assert!((560..=640).contains(&s.burst_us), "{s:?}");

    // Controller: 0.5 ms bursts every ~5 ms.
    t.reset();
    for k in 0..1170 {
        t.feed(if k % 98 < 10 && k > 3 { on } else { off });
    }
    let s = t.stats(floor);
    assert!(s.bursts >= 10 && s.duty_pct <= 12, "{s:?}");
    assert!((480..=540).contains(&s.burst_us) && (4_900..=5_100).contains(&s.period_us), "{s:?}");

    // Video: on all the time.
    t.reset();
    for _ in 0..1170 {
        t.feed(on);
    }
    let s = t.stats(floor);
    assert_eq!(s.duty_pct, 100);
    assert!(s.longest_us > 59_000);
}

#[test]
fn plan_helpers() {
    assert_eq!(plan::fpv_v1_channel(5_770_050), Some(4));
    assert_eq!(plan::fpv_v1_channel(5_768_500), None);
    assert!(plan::on_half_raster(5_768_500) && plan::on_half_raster(5_794_420));
    assert!(!plan::on_half_raster(5_770_000));
    assert!(plan::in_o4_band(5_200_000) && !plan::in_o4_band(5_745_000));
    // Every O3/O4 air-unit manual channel is on the x.5 raster; every V1
    // channel is not.
    for c in plan::O3_AIR_KHZ.iter().chain(&plan::O4_AIR_KHZ).chain(&[plan::O3_AIR_40_KHZ, plan::O4_AIR_WIDE_KHZ]) {
        assert!(plan::on_half_raster(*c) && plan::fpv_v1_channel(*c).is_none(), "{c}");
    }
    for c in plan::FPV_V1_KHZ {
        assert!(!plan::on_half_raster(c));
    }
    // The extra sweep LOs cover the O4 band.
    for m in plan::O4_BAND_SWEEP_MHZ {
        assert!(plan::in_o4_band(m as u32 * 1000));
    }
    let fe = Features::default();
    assert_eq!(centre_khz(&fe, &ctx(5800.0)), None);
}

/// The firmware's order: fpvscan first (analog video, HDZero, Wi-Fi), then
/// this crate on the same capture.
#[test]
fn alongside_fpvscan() {
    use fpvscan::{Dwell, Kind};
    let loud = Dwell { snr_db: 20, duty_pct: 100 };
    let mut sc = Box::new(fpvscan::Scratch::new());
    let mut ds = Box::new(Scratch::new());
    let both = |sig: &Sig, seed: u64, sc: &mut fpvscan::Scratch, ds: &mut Scratch| {
        let cap = capture(sig, seed);
        let a = fpvscan::analyse(&cap, sc);
        (fpvscan::classify(&loud, Some(&a), 5745), classify(&features(&cap, ds), &ctx(5744.0)))
    };
    let (k, v) = both(&packets(&dji20(), 300.0, 1e6, 0.0, 1), 1, &mut sc, &mut ds);
    assert_eq!((k, v), (Kind::LteStyle, Some(Verdict::Video)));
    let (k, v) = both(&wifi(300.0, 3, (20.0, 80.0), false), 2, &mut sc, &mut ds);
    assert_eq!((k, v), (Kind::Wifi, None));
    // A narrow controller burst is beyond fpvscan's whole-capture prefix
    // test; this crate finds it.
    let (k, v) = both(&bursts(&uplink_os2(), 300.0, &[(0.1e-3, 2.88e6)], 3), 3, &mut sc, &mut ds);
    println!("controller: fpvscan says {k:?}");
    assert_eq!(v, Some(Verdict::ControllerBurst));
}

#[test]
fn noise_alone_over_many_captures() {
    let mut worst = i32::MIN;
    for seed in 0..40u64 {
        let fe = run(&silence(), seed * 31 + 7);
        for f in [&fe.cp.normal15, &fe.cp.extended15, &fe.cp.normal30] {
            worst = worst.max(f.metric - f.thr);
        }
        assert_eq!(classify(&fe, &ctx(5800.0)), None, "seed {seed}: {fe:?}");
    }
    println!("noise: largest metric over its threshold: {worst}");
}

/// Not an assertion of a spec: where detection gives out on the synthetic
/// signals (printed for docs/DJI-LINKS.md).
#[test]
fn sensitivity_report() {
    // Signal power over the noise in the 20 MHz capture, dB.
    let snr = |amp: f64| 10.0 * (amp * amp / (NOISE * NOISE)).log10();
    for amp in [120.0, 80.0, 60.0, 45.0, 35.0, 25.0] {
        let mut hits = [0; 3];
        for seed in 0..4u64 {
            let v = classify(&run(&packets(&dji20(), amp, 0.0, 0.0, seed), seed), &ctx(5735.5));
            hits[0] += (v == Some(Verdict::Video)) as u32;
            let v = classify(&run(&bursts(&uplink_os2(), amp, &[(0.1e-3, 2e6)], seed), seed), &ctx(5780.0));
            hits[1] += (v == Some(Verdict::ControllerBurst)) as u32;
            let v = classify(&run(&bursts(&uplink_o3(), amp, &[(0.1e-3, 2e6)], seed), seed), &ctx(5780.0));
            hits[2] += (v == Some(Verdict::ControllerBurst)) as u32;
        }
        println!("amp {amp} ({:+.1} dB total power over capture noise): video {}/4, os2 uplink {}/4, o3 uplink {}/4", snr(amp), hits[0], hits[1], hits[2]);
    }
}

#[test]
fn memory_budget() {
    // docs/DJI-LINKS.md quotes these.
    println!(
        "Scratch {} B, Timeline {} B, HopTracker {} B, Features {} B",
        std::mem::size_of::<Scratch>(),
        std::mem::size_of::<Timeline>(),
        std::mem::size_of::<HopTracker>(),
        std::mem::size_of::<Features>()
    );
    assert!(std::mem::size_of::<Scratch>() <= 13 * 1024);
    assert!(std::mem::size_of::<Timeline>() <= 1600);
    assert!(std::mem::size_of::<HopTracker>() <= 128);
}
