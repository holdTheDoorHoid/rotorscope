//! Synthetic signals through the real 4-bit path: 40 MS/s complex baseband
//! plus receiver noise, quantised by the clip window exactly as the C5's
//! debug bus does (`rawcap::iq`), half-banded to 20 MS/s (`rawcap::dsp`), DC
//! removed, analysed and classified.

use std::f64::consts::PI;

use droneid::math::{C16, remove_dc};
use rawcap::dsp::{HB_REACH, decimate_nibbles};
use rawcap::iq::{Window, pack};

use crate::analyse::{Analysis, Scratch, analyse};
use crate::channels;
use crate::classify::{Dwell, Kind, LINE_MIN, classify, cp_threshold};
use crate::meter::{FloorTracker, Meter};

const FS40: f64 = 40e6;
const OUT: usize = 16_384;
const IN: usize = 2 * OUT + 2 * HB_REACH;
/// Receiver noise at gain 83, codes RMS per axis (BRINGUP-RESULTS.md: 43-51).
const NOISE: f64 = 48.0;

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

/// 4-bit capture bytes of `sig` (codes) plus receiver noise.
fn quantise(sig: &[(f64, f64)], seed: u64) -> Vec<u8> {
    let mut rng = Rng(seed);
    let w = Window::CLIP;
    sig.iter()
        .map(|&(i, q)| {
            let i = (i + NOISE * rng.g()).floor().clamp(-512.0, 511.0) as i16;
            let q = (q + NOISE * rng.g()).floor().clamp(-512.0, 511.0) as i16;
            pack(w.quantise(i), w.quantise(q))
        })
        .collect()
}

/// The firmware's capture: half-band to 20 MS/s, DC removed.
fn capture(bytes: &[u8]) -> Vec<C16> {
    assert!(bytes.len() >= IN);
    let mut out = vec![C16::default(); OUT];
    decimate_nibbles(|k| bytes[k], &mut out);
    remove_dc(&mut out);
    out
}

fn run(sig: &[(f64, f64)], seed: u64) -> Analysis {
    let cap = capture(&quantise(sig, seed));
    let mut s = Box::new(Scratch::new());
    analyse(&cap, &mut s)
}

// ------------------------------------------------------------ signals --

/// Composite video level at `t_us` into a line of `line_us`: sync -0.3,
/// blanking 0, picture 0..0.7 from `pic(line, x)` with x in 0..1.
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

/// FM video at `f0` Hz from the LO, `dev` Hz per unit of composite level.
fn fm_video(amp: f64, f0: f64, dev: f64, line_us: f64, pic: &dyn Fn(usize, f64) -> f64, start_us: f64) -> Vec<(f64, f64)> {
    let mut ph = 0.0f64;
    (0..IN)
        .map(|n| {
            let t = start_us + n as f64 / FS40 * 1e6;
            let line = (t / line_us) as usize;
            let v = composite(line, t - line as f64 * line_us, line_us, pic);
            ph += 2.0 * PI * (f0 + dev * v) / FS40;
            (amp * ph.cos(), amp * ph.sin())
        })
        .collect()
}

/// One OFDM symbol by direct synthesis: carriers `ks` (multiples of
/// `spacing` Hz) with random QPSK, `ncp` samples of cyclic prefix and a
/// useful part of `fs / spacing` samples (fractional is fine).
fn ofdm_symbol(out: &mut Vec<(f64, f64)>, rng: &mut Rng, ks: &[i32], spacing: f64, ncp: usize, amp: f64) {
    let nu = (FS40 / spacing).round() as usize;
    let d: Vec<(f64, f64)> = ks.iter().map(|_| rng.qpsk()).collect();
    let norm = amp / (ks.len() as f64).sqrt();
    for s in 0..ncp + nu {
        let t = (s as f64 - ncp as f64) / FS40;
        let (mut re, mut im) = (0.0, 0.0);
        for (k, &(a, b)) in ks.iter().zip(&d) {
            let (sn, cs) = (2.0 * PI * *k as f64 * spacing * t).sin_cos();
            re += a * cs - b * sn;
            im += a * sn + b * cs;
        }
        out.push((re * norm, im * norm));
    }
}

/// Bursty Wi-Fi: frames of 8..60 symbols, `gap_us` range of silence between.
/// `he`: 802.11ax data symbols (12.8 µs + 0.8 µs) instead of 802.11a.
fn wifi(amp: f64, seed: u64, gap_us: (f64, f64), he: bool) -> Vec<(f64, f64)> {
    let mut rng = Rng(seed);
    let (ks, spacing): (Vec<i32>, f64) = if he {
        ((-122..=122).filter(|k: &i32| k.abs() > 2).collect(), 78_125.0)
    } else {
        ((-26..=26).filter(|&k| k != 0).collect(), 312_500.0)
    };
    let mut out = vec![];
    // Start somewhere inside a gap or a frame.
    let lead = (rng.u() * gap_us.1 * 40.0) as usize;
    out.resize(lead, (0.0, 0.0));
    while out.len() < IN {
        let nsym = 8 + (rng.u() * 52.0) as usize;
        for _ in 0..nsym {
            ofdm_symbol(&mut out, &mut rng, &ks, spacing, 32, amp);
            if out.len() >= IN {
                break;
            }
        }
        let gap = gap_us.0 + rng.u() * (gap_us.1 - gap_us.0);
        out.resize(out.len() + (gap * 40.0) as usize, (0.0, 0.0));
    }
    out.truncate(IN);
    out
}

/// Continuous LTE-style OFDM: 15 kHz, `ncar` carriers, 4.7 µs CP.
fn lte(amp: f64, seed: u64, ncar: i32) -> Vec<(f64, f64)> {
    let mut rng = Rng(seed);
    let ks: Vec<i32> = (-ncar / 2..=ncar / 2).filter(|&k| k != 0).collect();
    let mut out = vec![];
    let skip = (rng.u() * 2800.0) as usize;
    while out.len() < IN + skip {
        ofdm_symbol(&mut out, &mut rng, &ks, 15_000.0, 188, amp);
    }
    out.drain(..skip);
    out.truncate(IN);
    out
}

/// HDZero-like: continuous, flat, 17 MHz wide, no cyclic prefix.
fn flat_wide(amp: f64, seed: u64) -> Vec<(f64, f64)> {
    let mut rng = Rng(seed);
    let ks: Vec<i32> = (-85..=85).collect();
    let mut out = vec![];
    while out.len() < IN {
        ofdm_symbol(&mut out, &mut rng, &ks, 100_000.0, 0, amp);
    }
    out.truncate(IN);
    out
}

fn cw(amp: f64, f0: f64) -> Vec<(f64, f64)> {
    (0..IN).map(|n| {
        let ph = 2.0 * PI * f0 * n as f64 / FS40;
        (amp * ph.cos(), amp * ph.sin())
    }).collect()
}

fn silence() -> Vec<(f64, f64)> {
    vec![(0.0, 0.0); IN]
}

const LOUD: Dwell = Dwell { snr_db: 20, duty_pct: 100 };

/// Pictures: flat grey, colour-bar-like steps, a vertical gradient, fixed
/// fine detail, and fresh random detail on every line (only the sync
/// pulses repeat).
fn pictures() -> Vec<(&'static str, Box<dyn Fn(usize, f64) -> f64>)> {
    vec![
        ("grey", Box::new(|_, _| 0.35)),
        ("bars", Box::new(|_, x| (7.0 - (x * 8.0).floor()) * 0.1)),
        ("gradient", Box::new(|l, x| 0.1 + 0.5 * ((l % 300) as f64 / 300.0) + 0.05 * x)),
        ("detail", Box::new(|_, x| 0.35 + 0.3 * ((x * 97.0).sin() * (x * 31.0).cos()))),
        ("random", Box::new(|l, x| {
            let mut r = Rng((l as u64) << 20 | (x * 1000.0) as u64);
            0.7 * r.u()
        })),
    ]
}

// --------------------------------------------------------------- tests --

#[test]
fn analog_video_is_found_with_its_line_rate() {
    for (std, line_us, hz) in [("PAL", 64.0, 15_625.0), ("NTSC", 1e6 / 15_734.264, 15_734.264)] {
        for (name, pic) in pictures() {
            for (amp, seed) in [(350.0, 1u64), (150.0, 2)] {
                for f0 in [1e6, 5e6] {
                    let sig = fm_video(amp, f0, 6e6, line_us, &*pic, 13.0 * seed as f64);
                    let a = run(&sig, seed);
                    let k = classify(&LOUD, Some(&a), 5800);
                    println!("{std} {name} amp {amp} f0 {f0}: line {} @ {} Hz, cp_wifi {} lte {} width {} flat {} -> {k:?}",
                        a.line, a.line_hz, a.cp_wifi, a.cp_lte, a.width_khz, a.flat_x10);
                    assert_eq!(k, Kind::AnalogVideo, "{std} {name} amp {amp} f0 {f0}: {a:?}");
                    assert!((a.line_hz as f64 - hz).abs() < 250.0, "{std} {name}: {} Hz", a.line_hz);
                }
            }
        }
    }
}

#[test]
fn wifi_is_never_video() {
    let mut worst_line = i16::MIN;
    for seed in 0..12u64 {
        for he in [false, true] {
            // Light traffic, busy traffic, back to back.
            for gap in [(100.0, 400.0), (20.0, 80.0), (10.0, 20.0)] {
                for amp in [350.0, 150.0] {
                    let sig = wifi(amp, seed * 7 + 1, gap, he);
                    let a = run(&sig, seed + 100);
                    let active = sig.iter().filter(|v| v.0 != 0.0 || v.1 != 0.0).count();
                    if active < 4000 {
                        continue; // the capture happened to fall in a gap
                    }
                    worst_line = worst_line.max(a.line);
                    for duty in [30, 70, 95] {
                        let k = classify(&Dwell { snr_db: 20, duty_pct: duty }, Some(&a), 5745);
                        assert_ne!(k, Kind::AnalogVideo, "seed {seed} he {he} gap {gap:?}: {a:?}");
                    }
                    let k = classify(&Dwell { snr_db: 20, duty_pct: 40 }, Some(&a), 5745);
                    assert_eq!(k, Kind::Wifi, "seed {seed} he {he} gap {gap:?} amp {amp}: {a:?}");
                }
            }
        }
    }
    println!("largest line metric on Wi-Fi: {worst_line}");
    assert!(worst_line < LINE_MIN / 2, "{worst_line}");
}

#[test]
fn lte_style_ofdm() {
    for seed in 0..3u64 {
        for (amp, ncar) in [(350.0, 600), (150.0, 300), (350.0, 72)] {
            let a = run(&lte(amp, seed, ncar), seed + 7);
            let k = classify(&LOUD, Some(&a), 5745);
            println!("lte {ncar} amp {amp}: {a:?} cp threshold {} -> {k:?}", cp_threshold(&a));
            if ncar >= 300 {
                assert_eq!(k, Kind::LteStyle, "{a:?}");
            } else {
                // 1 MHz wide: too few independent samples in 0.8 ms to call
                // its cyclic prefix; it must at least not be Wi-Fi or video.
                assert!(matches!(k, Kind::LteStyle | Kind::FpvCarrier), "{a:?}: {k:?}");
            }
            assert!(a.line < LINE_MIN / 2);
        }
    }
}

#[test]
fn noise_triggers_nothing() {
    for seed in 0..6u64 {
        let a = run(&silence(), seed);
        println!("noise: {a:?} cp threshold {}", cp_threshold(&a));
        for v in [a.line, a.cp_wifi, a.cp_half, a.cp_lte] {
            assert!(v.abs() < 30, "{a:?}");
        }
        // Even if the level meter were fooled, the features say nothing.
        assert_eq!(classify(&Dwell { snr_db: 20, duty_pct: 30 }, Some(&a), 5800), Kind::Energy);
        assert_eq!(classify(&Dwell { snr_db: 2, duty_pct: 10 }, Some(&a), 5800), Kind::Quiet);
    }
}

#[test]
fn hdzero_and_carriers() {
    let a = run(&flat_wide(350.0, 3), 3);
    println!("flat wide: {a:?}");
    assert!(a.width_khz >= 15_000 && a.flat_x10 <= 20, "{a:?}");
    assert_eq!(classify(&LOUD, Some(&a), 5658), Kind::Hdzero, "R1 is on the grid");
    assert_eq!(classify(&LOUD, Some(&a), 5665), Kind::FpvCarrier, "E3 is not");
    assert_eq!(classify(&Dwell { snr_db: 20, duty_pct: 40 }, Some(&a), 5658), Kind::Energy, "bursty");

    let a = run(&cw(350.0, 1.3e6), 4);
    println!("cw: {a:?}");
    assert!(a.width_khz < 2_000, "{a:?}");
    assert!(a.line.abs() < 50 && a.cp_wifi.abs() < 50 && a.cp_lte.abs() < 50, "{a:?}");
    assert_eq!(classify(&LOUD, Some(&a), 5800), Kind::FpvCarrier);
    assert_eq!(classify(&LOUD, None, 5800), Kind::FpvCarrier);
    assert_eq!(classify(&Dwell { snr_db: 20, duty_pct: 50 }, None, 5800), Kind::Energy);
}

#[test]
fn video_spectrum_is_not_flat_and_wide() {
    // Analog video must not pass for HDZero even if its line rate were missed.
    let pics = pictures();
    let sig = fm_video(350.0, 1e6, 6e6, 64.0, &*pics[1].1, 0.0);
    let a = run(&sig, 9);
    println!("video spectrum: {a:?}");
    assert!(a.flat_x10 > crate::classify::HDZ_FLAT_X10 || a.width_khz < crate::classify::HDZ_WIDTH_KHZ, "{a:?}");
}

#[test]
fn channel_table() {
    assert_eq!(channels::all().len(), 47);
    let sweep = channels::sweep();
    assert!(sweep.windows(2).all(|w| w[0] < w[1]));
    let skipped: Vec<u16> = channels::skipped().collect();
    println!("sweep {} channels, skipped {}: {skipped:?}", sweep.len(), skipped.len());
    assert_eq!(sweep.len() + skipped.len(), 47);
    assert!(skipped.iter().all(|&f| f > channels::REACH_MAX_MHZ));
    assert_eq!(sweep.first(), Some(&5362));
    assert_eq!(sweep.last(), Some(&5809));
    assert_eq!(channels::lo_for(5745), (5744, 1000));
    assert_eq!(channels::lo_for(5809), (5800, 9000));
    assert_eq!(channels::label(5880), Some((b'F', 8)));
    assert_eq!(channels::label(5658), Some((b'R', 1)));
    assert!(channels::hdzero_grid(5658) && channels::hdzero_grid(5800) && !channels::hdzero_grid(5665));
    // The hub's table (crates/coverage FPV_CHANNELS, 100 kHz units) is this one.
    let hub: [u32; 47] = [
        53620, 53990, 54360, 54730, 55100, 55470, 55840, 56210, 56450, 56580, 56650, 56850, 56950, 57050, 57250,
        57320, 57330, 57400, 57450, 57520, 57600, 57650, 57690, 57710, 57800, 57850, 57900, 58000, 58050, 58060,
        58090, 58200, 58250, 58280, 58400, 58430, 58450, 58470, 58600, 58650, 58660, 58800, 58850, 59050, 59170,
        59250, 59450,
    ];
    let ours: Vec<u32> = channels::all().iter().map(|&f| f as u32 * 10).collect();
    assert_eq!(ours, hub);
}

#[test]
fn meter_and_floor() {
    let mut rng = Rng(5);
    // Noise probes: 256 bytes of noise at about one step: POWER ~ 6 each.
    let noise = |rng: &mut Rng| (256.0 * 6.0 * (1.0 + 0.06 * rng.g())) as u32;
    let mut floor = FloorTracker::new();
    // A quiet channel.
    let mut m = Meter::new();
    for _ in 0..1170 {
        m.feed(noise(&mut rng));
    }
    let f = floor.update(&m);
    let d = m.dwell(f);
    assert!(d.snr_db.abs() <= 1 && d.duty_pct <= 1, "{d:?}");
    // A continuous transmitter 15 dB up.
    let mut m = Meter::new();
    for _ in 0..1170 {
        m.feed(noise(&mut rng) * 32);
    }
    let f = floor.update(&m);
    let d = m.dwell(f);
    assert!((14..=16).contains(&d.snr_db) && d.duty_pct >= 99, "{d:?} floor {f}");
    // Bursty: 30% on, 20 dB up.
    let mut m = Meter::new();
    for k in 0..1170 {
        m.feed(if k % 10 < 3 { noise(&mut rng) * 100 } else { noise(&mut rng) });
    }
    let d = m.dwell(floor.update(&m));
    assert!((28..=32).contains(&d.duty_pct) && d.snr_db >= 18, "{d:?}");
    assert_eq!(classify(&d, None, 5745), Kind::Energy);
}

#[test]
fn log_helpers() {
    for v in [1u64, 2, 3, 10, 1000, 123_456, 1 << 40, u64::MAX / 3] {
        let want = (v as f64).log2() * 256.0;
        assert!((crate::log2_q8(v) as f64 - want).abs() < 3.0, "{v}: {} vs {want}", crate::log2_q8(v));
        let db = 10.0 * (v as f64).log10() * 10.0;
        assert!((crate::db_x10(v) as f64 - db).abs() < 2.0, "{v}: {}", crate::db_x10(v));
    }
}

#[test]
fn angle_approximation() {
    for k in 0..720 {
        let th = k as f64 * PI / 360.0 - PI + 0.001;
        let (re, im) = ((1000.0 * th.cos()) as i32, (1000.0 * th.sin()) as i32);
        let want = (im as f64).atan2(re as f64) / (2.0 * PI) * 65536.0;
        let got = crate::analyse::angle16(re, im) as f64;
        let err = (got - want + 32768.0).rem_euclid(65536.0) - 32768.0;
        assert!(err.abs() < 80.0, "{th}: {got} vs {want}");
    }
}

