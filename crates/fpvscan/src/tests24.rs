//! 2.4 GHz: synthetic signals through the same 4-bit capture path as
//! `tests.rs` (40 MS/s complex, receiver noise, the C5's clip window,
//! half-band to 20 MS/s, DC removed), analysed and classified with
//! [`crate::band::classify`]. Signals: analog FM video (PAL and NTSC, with
//! and without the 6.5 MHz audio subcarrier, under Wi-Fi), OFDM Wi-Fi on
//! channels 1, 6 and 11 as seen from the video channels next to them,
//! 802.11b DSSS (1 Mb/s Barker and 11 Mb/s CCK frames), Bluetooth GFSK
//! hopping, microwave ovens (mains-gated and inverter) and noise. All of
//! it is synthetic: nothing here has been checked against a real 2.4 GHz
//! video transmitter.

use std::f64::consts::PI;

use droneid::math::{C16, remove_dc};
use rawcap::dsp::{HB_REACH, decimate_nibbles};
use rawcap::iq::{Window, pack};

use crate::analyse::{Analysis, Scratch, analyse};
use crate::band::{self, Band, Extra, Floors};
use crate::classify::{Dwell, Kind, LINE_MIN};
use crate::{channels, channels24, dsss};

const FS40: f64 = 40e6;
const OUT: usize = 16_384;
const IN: usize = 2 * OUT + 2 * HB_REACH;
/// Receiver noise on 2.4 GHz at gain 83, codes RMS per axis
/// (BRINGUP-RESULTS.md, 2414 MHz: 64 and 84 on two boards).
const NOISE: f64 = 72.0;

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
    fn bit(&mut self) -> f64 {
        if self.u() < 0.5 { 1.0 } else { -1.0 }
    }
}

fn capture(sig: &[(f64, f64)], seed: u64) -> Vec<C16> {
    let mut rng = Rng(seed);
    let w = Window::CLIP;
    let bytes: Vec<u8> = sig
        .iter()
        .map(|&(i, q)| {
            let i = (i + NOISE * rng.g()).floor().clamp(-512.0, 511.0) as i16;
            let q = (q + NOISE * rng.g()).floor().clamp(-512.0, 511.0) as i16;
            pack(w.quantise(i), w.quantise(q))
        })
        .collect();
    let mut out = vec![C16::default(); OUT];
    decimate_nibbles(|k| bytes[k], &mut out);
    remove_dc(&mut out);
    out
}

/// Analysis and 2.4 GHz features of `sig` as heard on `mhz`.
fn run(sig: &[(f64, f64)], seed: u64, mhz: u16) -> (Analysis, Extra) {
    let cap = capture(sig, seed);
    let mut s = Box::new(Scratch::new());
    let a = analyse(&cap, &mut s);
    let e = band::extra(&cap, &s, &a, mhz);
    (a, e)
}

fn add(a: &mut Sig, b: &Sig) {
    for (x, y) in a.iter_mut().zip(b) {
        x.0 += y.0;
        x.1 += y.1;
    }
}

/// Shift by `f` Hz.
fn shift(sig: &mut Sig, f: f64) {
    for (n, v) in sig.iter_mut().enumerate() {
        let (s, c) = (2.0 * PI * f * n as f64 / FS40).sin_cos();
        *v = (v.0 * c - v.1 * s, v.0 * s + v.1 * c);
    }
}

// ------------------------------------------------------------ signals --

fn composite(line: usize, t_us: f64, line_us: f64, pic: &dyn Fn(usize, f64) -> f64) -> f64 {
    let s = line_us / 64.0;
    if t_us < 4.7 * s {
        -0.3
    } else if t_us < 10.4 * s || t_us >= 62.35 * s {
        0.0
    } else {
        pic(line, (t_us - 10.4 * s) / (51.95 * s)).clamp(0.0, 0.7)
    }
}

/// FM video at `f0` Hz from the LO, `dev` Hz per unit of composite level,
/// optionally with a 6.5 MHz FM audio subcarrier (`sub` Hz of deviation of
/// the main carrier, its own tone FM-ing it by 50 kHz).
fn fm_video(amp: f64, f0: f64, dev: f64, line_us: f64, pic: &dyn Fn(usize, f64) -> f64, start_us: f64, sub: f64) -> Sig {
    let (mut ph, mut ph_sub) = (0.0f64, 0.0f64);
    (0..IN)
        .map(|n| {
            let t = start_us + n as f64 / FS40 * 1e6;
            let line = (t / line_us) as usize;
            let v = composite(line, t - line as f64 * line_us, line_us, pic);
            ph_sub += 2.0 * PI * (6.5e6 + 50e3 * (2.0 * PI * 1e3 * t * 1e-6).sin()) / FS40;
            ph += 2.0 * PI * (f0 + dev * v + sub * ph_sub.cos()) / FS40;
            (amp * ph.cos(), amp * ph.sin())
        })
        .collect()
}

type Picture = Box<dyn Fn(usize, f64) -> f64>;

fn pictures() -> Vec<(&'static str, Picture)> {
    vec![
        ("grey", Box::new(|_, _| 0.35)),
        ("bars", Box::new(|_, x| (7.0 - (x * 8.0).floor()) * 0.1)),
        ("random", Box::new(|l, x| {
            let mut r = Rng((l as u64) << 20 | (x * 1000.0) as u64);
            0.7 * r.u()
        })),
    ]
}

/// One OFDM symbol (802.11a/g/n 20 MHz: 52 carriers 312.5 kHz apart, 0.8 µs
/// cyclic prefix).
fn ofdm_symbol(out: &mut Sig, rng: &mut Rng, amp: f64) {
    let ks: Vec<i32> = (-26..=26).filter(|&k| k != 0).collect();
    let d: Vec<(f64, f64)> = ks.iter().map(|_| (rng.bit() * std::f64::consts::FRAC_1_SQRT_2, rng.bit() * std::f64::consts::FRAC_1_SQRT_2)).collect();
    let norm = amp / (ks.len() as f64).sqrt();
    for s in 0..32 + 128 {
        let t = (s as f64 - 32.0) / FS40;
        let (mut re, mut im) = (0.0, 0.0);
        for (k, &(a, b)) in ks.iter().zip(&d) {
            let (sn, cs) = (2.0 * PI * *k as f64 * 312_500.0 * t).sin_cos();
            re += a * cs - b * sn;
            im += a * sn + b * cs;
        }
        out.push((re * norm, im * norm));
    }
}

/// Bursty OFDM Wi-Fi, frames of 8-60 symbols, gaps in `gap_us`, at `f` Hz.
fn wifi_ofdm(amp: f64, seed: u64, gap_us: (f64, f64), f: f64) -> Sig {
    let mut rng = Rng(seed);
    let mut out = vec![(0.0, 0.0); (rng.u() * gap_us.1 * 40.0) as usize];
    while out.len() < IN {
        for _ in 0..8 + (rng.u() * 52.0) as usize {
            ofdm_symbol(&mut out, &mut rng, amp);
        }
        out.resize(out.len() + ((gap_us.0 + rng.u() * (gap_us.1 - gap_us.0)) * 40.0) as usize, (0.0, 0.0));
    }
    out.truncate(IN);
    shift(&mut out, f);
    out
}

/// 802.11b DSSS frames at `f` Hz: a long preamble and header (192 µs of
/// DBPSK Barker), then either more Barker symbols (1 Mb/s) or 11 Mchip/s
/// random QPSK chips standing in for CCK, `body_us` long; gaps in `gap_us`.
fn wifi_dsss(amp: f64, seed: u64, gap_us: (f64, f64), body_us: f64, cck: bool, f: f64) -> Sig {
    let mut rng = Rng(seed);
    let mut out: Sig = vec![(0.0, 0.0); (rng.u() * gap_us.1 * 40.0) as usize];
    let chip = |out: &mut Sig, t0: f64, n: usize, val: (f64, f64)| {
        // Chip n of 11 per µs, held for its share of 40 MS/s samples.
        let end = t0 + (n + 1) as f64 * FS40 / 11e6;
        while (out.len() as f64) < end {
            out.push((amp * val.0, amp * val.1));
        }
    };
    while out.len() < IN {
        let t0 = out.len() as f64;
        let (mut sym, mut phase) = (0usize, 1.0f64);
        for _ in 0..192 {
            phase *= rng.bit();
            for (k, &b) in dsss::BARKER.iter().enumerate() {
                chip(&mut out, t0, sym * 11 + k, (phase * b as f64, 0.0));
            }
            sym += 1;
        }
        for _ in 0..body_us as usize {
            phase *= rng.bit();
            for (k, &b) in dsss::BARKER.iter().enumerate() {
                let v = if cck { (rng.bit() * std::f64::consts::FRAC_1_SQRT_2, rng.bit() * std::f64::consts::FRAC_1_SQRT_2) } else { (phase * b as f64, 0.0) };
                chip(&mut out, t0, sym * 11 + k, v);
            }
            sym += 1;
        }
        out.resize(out.len() + ((gap_us.0 + rng.u() * (gap_us.1 - gap_us.0)) * 40.0) as usize, (0.0, 0.0));
    }
    out.truncate(IN);
    shift(&mut out, f);
    out
}

/// Bluetooth BR GFSK (1 Msym/s, h = 0.32, Gaussian BT 0.5 by a 3-tap
/// smoothing of the symbol stream), `devices` piconets each hopping every
/// 625 µs slot over 2402-2480 MHz, 366 µs packets; heard at LO `lo` MHz.
fn bluetooth(amp: f64, seed: u64, devices: usize, lo: f64) -> Sig {
    let mut rng = Rng(seed);
    let mut out = vec![(0.0, 0.0); IN];
    for _ in 0..devices {
        let start = rng.u() * 625.0;
        let mut ph = 0.0f64;
        let mut slot = usize::MAX;
        let (mut f_off, mut on) = (0.0, false);
        let mut bits = [0.0f64; 3];
        let mut bit_k = usize::MAX;
        for (n, v) in out.iter_mut().enumerate() {
            let t_us = n as f64 / 40.0 + start;
            let s = (t_us / 625.0) as usize;
            if s != slot {
                slot = s;
                f_off = (2402.0 + (rng.u() * 79.0).floor() - lo) * 1e6;
                on = rng.u() < 0.6;
            }
            let in_slot = t_us - s as f64 * 625.0;
            if !on || in_slot > 366.0 || f_off.abs() > 12e6 {
                continue;
            }
            let k = in_slot as usize;
            if k != bit_k {
                bit_k = k;
                bits = [bits[1], bits[2], rng.bit()];
            }
            let m = 0.25 * bits[0] + 0.5 * bits[1] + 0.25 * bits[2];
            ph += 2.0 * PI * (f_off + 160e3 * m) / FS40;
            v.0 += amp * ph.cos();
            v.1 += amp * ph.sin();
        }
    }
    out
}

/// A magnetron at `f` Hz from the LO. Mains-gated (`inverter` false): on for
/// half of each 16.7 ms mains cycle, its frequency sweeping 8 MHz over the
/// half-cycle; `phase_ms` is where in the cycle the capture starts.
/// Inverter (`Some(ripple Hz)`): always on, frequency rippled +-3 MHz by
/// the switching supply (typically 20-45 kHz; 31 kHz is twice the line
/// rate, 15.6 kHz the line rate itself, the worst cases). Both with about 1 MHz of random FM.
fn oven(amp: f64, seed: u64, f: f64, inverter: Option<f64>, phase_ms: f64) -> Sig {
    let mut rng = Rng(seed);
    let (mut ph, mut jit) = (0.0f64, 0.0f64);
    (0..IN)
        .map(|n| {
            let t = phase_ms * 1e-3 + n as f64 / FS40;
            jit = 0.995 * jit + 0.1 * rng.g();
            let (on, fi) = if let Some(rip) = inverter {
                (true, f + 3e6 * (2.0 * PI * rip * t).sin())
            } else {
                let c = (t * 60.0).fract();
                (c < 0.5, f - 4e6 + 8e6 * (PI * c * 2.0).sin())
            };
            ph += 2.0 * PI * (fi + 1e6 * jit) / FS40;
            if on { (amp * ph.cos(), amp * ph.sin()) } else { (0.0, 0.0) }
        })
        .collect()
}

fn cw(amp: f64, f0: f64) -> Sig {
    (0..IN)
        .map(|n| {
            let ph = 2.0 * PI * f0 * n as f64 / FS40;
            (amp * ph.cos(), amp * ph.sin())
        })
        .collect()
}

const LOUD: Dwell = Dwell { snr_db: 20, duty_pct: 100 };
const DUTIES: [u8; 4] = [30, 70, 95, 100];

// --------------------------------------------------------------- tests --

#[test]
fn channel_table_24() {
    let all = channels24::all();
    println!("2.4 GHz channels: {all:?}");
    assert_eq!(all.len(), 15);
    assert!(all.windows(2).all(|w| w[0] < w[1]));
    assert!(all.iter().all(|&f| channels24::reachable(f) && band::reachable(f)));
    let outside: Vec<u16> = all.iter().copied().filter(|f| !channels24::ISM_MHZ.contains(f)).collect();
    assert_eq!(outside, [2370, 2390, 2490, 2510]);
    assert_eq!(channels24::label(2410), Some((b'A', 1)));
    assert_eq!(channels24::label(2450), Some((b'A', 3)));
    assert_eq!(channels24::label(2468), Some((b'B', 4)));
    assert_eq!(channels24::label(2473), Some((b'B', 8)));
    assert_eq!(band::label(2414).unwrap().to_string(), "2.4B1");
    assert_eq!(band::label(5880).unwrap().to_string(), "F8");
    assert_eq!(band::lo_for(2414), (2413, 1000));
    assert_eq!(band::lo_for(2370), (2369, 1000));
    // The reach the receiver was measured at.
    assert!(band::reachable(2201) && band::reachable(2651) && !band::reachable(2700) && !band::reachable(2100));
    // Combined sweep: every 2.4 GHz channel, then the 5.8 GHz sweep unchanged.
    let sw = band::sweep();
    assert_eq!(sw.len(), 15 + channels::sweep().len());
    assert_eq!(&sw[..15], all);
    assert_eq!(&sw[15..], channels::sweep());
    assert!(sw.windows(2).all(|w| w[0] < w[1]));
    assert_eq!(band::sweep_of(Band::G24), all);
    // 5.8 GHz through `band` is the 5.8 GHz module.
    for &f in channels::all() {
        assert_eq!(band::reachable(f), channels::reachable(f), "{f}");
        if channels::reachable(f) {
            assert_eq!(band::lo_for(f), channels::lo_for(f), "{f}");
        }
        let l = band::label(f).unwrap();
        assert_eq!((l.letter, l.ch), channels::label(f).unwrap());
    }
    // The hub's 2.4 GHz table (crates/coverage FPV_CHANNELS_24) is this one.
    let hub: [u32; 15] = [23700, 23900, 24100, 24110, 24140, 24300, 24320, 24330, 24500, 24530, 24680, 24700, 24730, 24900, 25100];
    let ours: Vec<u32> = all.iter().map(|&f| f as u32 * 10).collect();
    assert_eq!(ours, hub);
}

#[test]
fn band_classify_is_unchanged_on_58() {
    // On 5.8 GHz the band dispatcher is the 5.8 GHz classifier, whatever
    // the DSSS metric says.
    let sigs: Vec<Sig> = vec![
        fm_video(350.0, 1e6, 6e6, 64.0, &|_, _| 0.35, 0.0, 0.0),
        wifi_ofdm(350.0, 3, (20.0, 80.0), 0.0),
        cw(350.0, 1.3e6),
        vec![(0.0, 0.0); IN],
    ];
    for (i, s) in sigs.iter().enumerate() {
        let (a, _) = run(s, i as u64, 5800);
        for d in [LOUD, Dwell { snr_db: 20, duty_pct: 40 }, Dwell { snr_db: 3, duty_pct: 5 }] {
            for mhz in [5658, 5745, 5800] {
                for e in [Extra::default(), Extra { dsss: 30_000, sub_line: 1000, half_r: -1000 }] {
                    assert_eq!(band::classify(&d, Some(&a), &e, mhz), crate::classify(&d, Some(&a), mhz));
                }
                assert_eq!(band::classify(&d, None, &Extra::default(), mhz), crate::classify(&d, None, mhz));
            }
        }
    }
}

#[test]
fn analog_video_24() {
    for (std, line_us, hz) in [("PAL", 64.0, 15_625.0), ("NTSC", 1e6 / 15_734.264, 15_734.264)] {
        for (name, pic) in pictures() {
            for (dev, sub) in [(3e6, 0.0), (6e6, 0.0), (3e6, 600e3)] {
                for (amp, seed, mhz) in [(350.0, 1u64, 2414u16), (150.0, 2, 2370), (150.0, 3, 2450)] {
                    let sig = fm_video(amp, 1e6, dev, line_us, &*pic, 7.0 * seed as f64, sub);
                    let (a, e) = run(&sig, seed, mhz);
                    let k = band::classify(&LOUD, Some(&a), &e, mhz);
                    println!("{std} {name} dev {dev} sub {sub} amp {amp} @ {mhz}: line {} @ {} Hz, cp {} dsss {} sub {} half {} -> {k:?}",
                        a.line, a.line_hz, a.cp_wifi, e.dsss, e.sub_line, e.half_r);
                    assert_eq!(k, Kind::AnalogVideo, "{std} {name} dev {dev} sub {sub}: {a:?}");
                    assert!((a.line_hz as f64 - hz).abs() < 250.0, "{std} {name}: {} Hz", a.line_hz);
                    assert!(e.dsss < dsss::DSSS_MIN / 2 && e.sub_line < LINE_MIN / 2, "{e:?}");
                }
            }
        }
    }
}

#[test]
fn analog_video_under_wifi() {
    // A camera on 2414 with Wi-Fi channel 1 (2412, -1 MHz from the LO,
    // overlapping) busy at a third of its level, and with Bluetooth.
    let pics = pictures();
    for seed in 0..4u64 {
        let mut sig = fm_video(350.0, 1e6, 3e6, 64.0, &*pics[seed as usize % 3].1, 3.0 * seed as f64, 0.0);
        add(&mut sig, &wifi_ofdm(110.0, seed + 40, (20.0, 80.0), -2e6));
        add(&mut sig, &bluetooth(150.0, seed + 50, 3, 2413.0));
        let (a, e) = run(&sig, seed + 60, 2414);
        let k = band::classify(&LOUD, Some(&a), &e, 2414);
        println!("video + Wi-Fi + BT seed {seed}: line {} @ {} cp {} dsss {} sub {} half {} -> {k:?}", a.line, a.line_hz, a.cp_wifi, e.dsss, e.sub_line, e.half_r);
        assert_eq!(k, Kind::AnalogVideo, "{a:?}");
    }
}

#[test]
fn wifi_ofdm_channels_1_6_11_are_never_video() {
    // (video channel, Wi-Fi offset from its LO): ch 1 seen from 2414,
    // ch 6 from 2432 (+6 MHz, partly outside the capture), ch 11 from 2468,
    // ch 6 from 2433 and 2430.
    let cases = [(2414u16, -1e6), (2432, 6e6), (2468, -5e6), (2433, 5e6), (2430, 8e6)];
    let mut worst_line = i16::MIN;
    let mut worst_x = i16::MIN;
    for (i, &(mhz, f)) in cases.iter().enumerate() {
        for seed in 0..4u64 {
            for gap in [(100.0, 400.0), (20.0, 80.0), (10.0, 20.0)] {
                let sig = wifi_ofdm(350.0, seed * 13 + i as u64, gap, f);
                if sig.iter().filter(|v| v.0 != 0.0).count() < 4000 {
                    continue;
                }
                let (a, e) = run(&sig, seed + 100, mhz);
                worst_line = worst_line.max(a.line);
                worst_x = worst_x.max(e.dsss);
                                for duty in DUTIES {
                    let k = band::classify(&Dwell { snr_db: 20, duty_pct: duty }, Some(&a), &e, mhz);
                    assert!(!k.reported(), "{mhz} {f} seed {seed} gap {gap:?} duty {duty}: {k:?} {a:?}");
                }
                assert_eq!(band::classify(&Dwell { snr_db: 20, duty_pct: 40 }, Some(&a), &e, mhz), Kind::Wifi, "{mhz} {f}: {a:?}");
            }
        }
    }
    println!("OFDM Wi-Fi: largest line metric {worst_line}, largest dsss {worst_x}");
    assert!(worst_line < LINE_MIN / 2);
    // Sparse traffic puts its energy in a few frames, so its fold is
    // noisier than dense traffic's: up to about 260 (DSSS gives 1400+).
    assert!(worst_x < dsss::DSSS_MIN, "{worst_x}");
}

#[test]
fn wifi_dsss_is_wifi() {
    let mut least = i16::MAX;
    for seed in 0..4u64 {
        for (cck, body) in [(false, 400.0), (true, 900.0), (true, 150.0)] {
            for (mhz, f) in [(2414u16, -1e6), (2432, 5e6), (2468, -5e6)] {
                let sig = wifi_dsss(300.0, seed * 5 + 1, (50.0, 300.0), body, cck, f);
                // The firmware's window starts ~100 µs before the rise: put
                // a frame start there, as the trigger would.
                let lead = sig.iter().position(|v| v.0 != 0.0).unwrap_or(0);
                let mut sig = sig[lead..].to_vec();
                sig.splice(0..0, std::iter::repeat_n((0.0, 0.0), 4000));
                sig.resize(IN, (0.0, 0.0));
                let (a, e) = run(&sig, seed + 7, mhz);
                least = least.min(e.dsss);
                for duty in DUTIES {
                    let k = band::classify(&Dwell { snr_db: 20, duty_pct: duty }, Some(&a), &e, mhz);
                    println!("dsss cck {cck} body {body} @ {mhz} duty {duty}: dsss {} sub {} half {} line {} cp {} -> {k:?}", e.dsss, e.sub_line, e.half_r, a.line, a.cp_wifi);
                    assert_eq!(k, Kind::Wifi, "{a:?} dsss {} sub {} half {}", e.dsss, e.sub_line, e.half_r);
                }
                assert!(a.line < LINE_MIN / 2, "{a:?}");
            }
        }
    }
    println!("smallest DSSS metric on 802.11b: {least}");
}

#[test]
fn bluetooth_hopping_is_never_reported() {
    let mut worst_line = i16::MIN;
    for seed in 0..6u64 {
        for devices in [1, 3, 8] {
            let lo = [2413.0, 2431.0, 2449.0][seed as usize % 3];
            let mhz = lo as u16 + 1;
            let (a, e) = run(&bluetooth(300.0, seed, devices, lo), seed + 20, mhz);
            worst_line = worst_line.max(a.line);
            for duty in DUTIES {
                let k = band::classify(&Dwell { snr_db: 20, duty_pct: duty }, Some(&a), &e, mhz);
                assert!(!k.reported(), "BT seed {seed} x{devices} duty {duty}: {k:?} {a:?} dsss {} sub {} half {}", e.dsss, e.sub_line, e.half_r);
            }
            assert!(e.dsss < dsss::DSSS_MIN / 2, "{e:?}");
        }
    }
    println!("Bluetooth: largest line metric {worst_line}");
    assert!(worst_line < LINE_MIN, "{worst_line}");
}

#[test]
fn microwave_ovens_are_never_reported() {
    for seed in 0..4u64 {
        for (inverter, phase_ms) in [(None, 1.0), (None, 7.9), (Some(31e3), 0.0), (Some(15.6e3), 0.0), (Some(47e3), 0.0), (Some(25e3), 0.0)] {
            for f in [-3e6, 2e6] {
                let sig = oven(350.0, seed, f, inverter, phase_ms + seed as f64);
                if sig.iter().filter(|v| v.0 != 0.0).count() < 4000 {
                    continue;
                }
                let (a, e) = run(&sig, seed + 30, 2450);
                println!("oven inverter {inverter:?} phase {phase_ms}: line {} @ {} width {} cp {} dsss {} sub {} half {}", a.line, a.line_hz, a.width_khz, a.cp_wifi, e.dsss, e.sub_line, e.half_r);
                for duty in DUTIES {
                    let k = band::classify(&Dwell { snr_db: 25, duty_pct: duty }, Some(&a), &e, 2450);
                    assert!(!k.reported(), "oven {inverter:?} duty {duty}: {k:?} {a:?}");
                }
            }
        }
    }
}

#[test]
fn noise_and_carriers_24() {
    for seed in 0..4u64 {
        let (a, e) = run(&vec![(0.0, 0.0); IN], seed, 2430);
        println!("noise: line {} cp {} dsss {} sub {} half {}", a.line, a.cp_wifi, e.dsss, e.sub_line, e.half_r);
        assert!(e.dsss < dsss::DSSS_MIN / 2, "{e:?}");
        assert_eq!(band::classify(&Dwell { snr_db: 20, duty_pct: 30 }, Some(&a), &e, 2430), Kind::Energy);
        assert_eq!(band::classify(&Dwell { snr_db: 2, duty_pct: 10 }, Some(&a), &e, 2430), Kind::Quiet);
    }
    // A continuous carrier: inside the oven ISM band it is not reported,
    // outside it (2370, 2390, 2510) it is, as on 5.8 GHz.
    let (a, e) = run(&cw(350.0, 1.3e6), 4, 2450);
    assert_eq!(band::classify(&LOUD, Some(&a), &e, 2450), Kind::Energy);
    assert_eq!(band::classify(&LOUD, Some(&a), &e, 2490), Kind::Energy);
    for mhz in [2370, 2390, 2510] {
        assert_eq!(band::classify(&LOUD, Some(&a), &e, mhz), Kind::FpvCarrier);
        assert_eq!(band::classify(&LOUD, None, &Extra::default(), mhz), Kind::FpvCarrier);
    }
    assert_eq!(band::classify(&LOUD, None, &Extra::default(), 2414), Kind::Energy);
}

#[test]
fn floors_are_per_band() {
    use crate::meter::Meter;
    let mut rng = Rng(9);
    let probe = |rng: &mut Rng, p: f64| (256.0 * p * (1.0 + 0.06 * rng.g())) as u32;
    let mut floors = Floors::new();
    let mut quiet58 = Meter::new();
    let mut quiet24 = Meter::new();
    for _ in 0..1170 {
        quiet58.feed(probe(&mut rng, 6.0));
        // 2.4 GHz noise ~4 dB higher at the same gain index.
        quiet24.feed(probe(&mut rng, 15.0));
    }
    let f58 = floors.update(5745, &quiet58);
    let f24 = floors.update(2370, &quiet24);
    assert!(f24 - f58 >= 30, "{f58} {f24}");
    // A quiet 2.4 GHz channel judged against its own floor is quiet...
    assert!(quiet24.dwell(f24).snr_db.abs() <= 1);
    // ...but against a shared floor it would read 4 dB loud.
    assert!(quiet24.dwell(f58).snr_db >= 3);
    assert_eq!(floors.get(Band::G58), Some(f58));
    assert_eq!(floors.get(Band::G24), Some(f24));
}

#[test]
fn smooth_ripple_at_the_line_rate_is_not_video_on_24() {
    // The worst case for a line-rate test: FM by a clean sinusoid at the
    // line rate itself (or near it). The 5.8 GHz classifier calls it video
    // (printed, not asserted: unchanged behaviour); on 2.4 GHz, where
    // switching supplies are common, its anti-periodic shape rejects it.
    for (seed, rip) in [(1u64, 15_625.0), (2, 15_734.0), (3, 15_400.0)] {
        let sig: Sig = {
            let mut ph = 0.0f64;
            (0..IN)
                .map(|n| {
                    let t = n as f64 / FS40;
                    ph += 2.0 * PI * (1e6 + 3e6 * (2.0 * PI * rip * t).sin()) / FS40;
                    (350.0 * ph.cos(), 350.0 * ph.sin())
                })
                .collect()
        };
        let (a, e) = run(&sig, seed, 2450);
        let k24 = band::classify(&LOUD, Some(&a), &e, 2450);
        println!("ripple {rip} Hz: line {} @ {} sub {} half {} -> 2.4 {k24:?}, 5.8 would say {:?}",
            a.line, a.line_hz, e.sub_line, e.half_r, crate::classify(&LOUD, Some(&a), 5745));
        assert_ne!(k24, Kind::AnalogVideo, "{a:?} {e:?}");
        assert!(!k24.reported());
    }
}
