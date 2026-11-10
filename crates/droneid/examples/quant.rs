//! The ESP32-C5 capture experiment: can DroneID still be decoded from the
//! C5's two raw-sample paths?
//!
//! - continuous 40 MS/s, 4-bit I + 4-bit Q (debug bus -> PARL_IO), modelled
//!   two ways: top-4-bits (saturating) and a 4-bit window of a wider word
//!   that wraps around (folding);
//! - 822 µs snapshots at 20 MS/s, 10-bit I/Q (the dump engine).
//!
//! Every burst of both public recordings is moved to +0.5 MHz (where a C5
//! tuned to the whole MHz below a DroneID channel sees it), band-limited and
//! resampled to the capture rate, scaled against a fixed thermal noise floor
//! of 1 LSB RMS per component, quantised, and decoded. Sweeping the burst's
//! per-carrier SNR maps the window where each path decodes: the bottom is
//! sensitivity, the top is where clipping or wrapping takes over.
//!
//! Also prints host timings per decode stage.
//!
//!     cargo run --release -p droneid --example quant [testdata/droneid]

use std::time::Instant;

use droneid::host::{Format, decode_recording, resample_f32, to_c16, to_f32_pairs};
use droneid::math::C16;
use droneid::rx::{Path, Receiver, Span, Stage};

type Cx = (f32, f32);

/// A burst cut from a recording, re-centred, at the capture rate.
struct Snip {
    x: Vec<Cx>,
    /// Burst mean power (per complex sample).
    p_burst: f64,
    block: [u8; 173],
}

#[derive(Clone, Copy, PartialEq)]
enum Quant {
    Float,
    Clip4,
    Fold4,
    Bits10,
}

struct Scenario {
    name: &'static str,
    fs: f64,
    cutoff: f64,
    q: Quant,
    /// Capture window around the burst start: (before, total), seconds.
    window: (f64, f64),
}

fn mix(x: &[Cx], f: f64, fs: f64) -> Vec<Cx> {
    x.iter()
        .enumerate()
        .map(|(n, &(a, b))| {
            let ph = 2.0 * std::f64::consts::PI * f * n as f64 / fs;
            let (s, c) = ph.sin_cos();
            ((a as f64 * c - b as f64 * s) as f32, (a as f64 * s + b as f64 * c) as f32)
        })
        .collect()
}

/// splitmix + Box-Muller, seeded.
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
        let (u1, u2) = (self.u().max(1e-300), self.u());
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }
}

/// Scale `s` to the per-carrier SNR against 1-LSB-RMS noise, add the noise,
/// quantise, and widen to 16 bits.
fn capture(s: &Snip, fs: f64, snr_db: f64, q: Quant, rng: &mut Rng) -> Vec<C16> {
    let sigma = 1.0; // LSB per component
    let n_tot = 2.0 * sigma * sigma;
    let p_want = 10f64.powf(snr_db / 10.0) * 9.0e6 * n_tot / fs;
    let a = (p_want / s.p_burst).sqrt();
    s.x.iter()
        .map(|&(i, qd)| {
            let v = (i as f64 * a + sigma * rng.g(), qd as f64 * a + sigma * rng.g());
            let f = |v: f64| -> i16 {
                match q {
                    Quant::Float => match std::env::var("QV").as_deref() {
                        Ok("floor1") => ((v.floor()) * 256.0).clamp(-32768.0, 32767.0) as i16,
                        Ok("round1") => ((v.round()) * 256.0).clamp(-32768.0, 32767.0) as i16,
                        Ok("round4") => ((v.round().clamp(-8.0, 7.0)) as i16) << 12,
                        _ => (v * 256.0).round().clamp(-32768.0, 32767.0) as i16,
                    },
                    // Bit slicing truncates (floor), so all three keep its
                    // half-LSB bias.
                    Quant::Clip4 => ((v.floor().clamp(-8.0, 7.0)) as i16) << 12,
                    Quant::Fold4 => ((((v.floor() as i64 + 8).rem_euclid(16)) - 8) as i16) << 12,
                    Quant::Bits10 => ((v.floor().clamp(-512.0, 511.0)) as i16) << 6,
                }
            };
            C16 { re: f(v.0), im: f(v.1) }
        })
        .collect()
}

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| "testdata/droneid".into());
    let scenarios = [
        Scenario { name: "40 MS/s float (reference)", fs: 40e6, cutoff: 19e6, q: Quant::Float, window: (150e-6, 1000e-6) },
        Scenario { name: "40 MS/s 4-bit, top bits (clip)", fs: 40e6, cutoff: 19e6, q: Quant::Clip4, window: (150e-6, 1000e-6) },
        Scenario { name: "40 MS/s 4-bit, window (wrap)", fs: 40e6, cutoff: 19e6, q: Quant::Fold4, window: (150e-6, 1000e-6) },
        Scenario { name: "20 MS/s 10-bit, 822 us snapshot", fs: 20e6, cutoff: 9.5e6, q: Quant::Bits10, window: (100e-6, 819e-6) },
    ];
    // Reference decode at 50 MS/s to find each burst and its true content.
    let mut refs = vec![]; // (float samples, start, cfo, block)
    for name in ["mavic_air_2", "mini2_sm"] {
        let raw = match std::fs::read(format!("{dir}/{name}")) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("{dir}/{name}: {e}; run tools/fetch-droneid-samples.sh");
                return;
            }
        };
        let f = to_f32_pairs(&raw);
        let x = to_c16(&raw, Format::Cf32);
        let mut rx = Box::new(Receiver::new(50_000_000).unwrap());
        decode_recording(&mut rx, &x, |found| {
            if let Ok(b) = found.result
                && b.crc24_ok
            {
                refs.push((name, f.clone(), found.chunk_start as f64 + b.start, b.cfo_hz, b.block));
            }
        });
    }
    println!("{} reference bursts (CRC-valid at 50 MS/s)\n", refs.len());

    let snrs: Vec<f64> = (-2..=27).map(|i| i as f64 * 2.0).collect();
    for sc in &scenarios {
        let fs = sc.fs;
        // Cut, re-centre at +0.5 MHz, resample.
        let snips: Vec<Snip> = refs
            .iter()
            .map(|(_, f, start, cfo, block)| {
                let a = (start - sc.window.0 * 50e6 - 200.0).max(0.0) as usize;
                let b = ((start + (sc.window.1 - sc.window.0) * 50e6) as usize + 200).min(f.len());
                let seg = mix(&f[a..b], -(cfo - 0.5e6), 50e6);
                let y = resample_f32(&seg, 50e6, fs, sc.cutoff);
                let skip = (200.0 * fs / 50e6) as usize;
                let len = (sc.window.1 * fs) as usize;
                let y: Vec<Cx> = y[skip..(skip + len).min(y.len())].to_vec();
                // Burst power over its body.
                let b0 = (sc.window.0 * fs) as usize;
                let b1 = b0 + (600e-6 * fs) as usize;
                let p = y[b0..b1].iter().map(|v| (v.0 as f64).powi(2) + (v.1 as f64).powi(2)).sum::<f64>()
                    / (b1 - b0) as f64;
                Snip { x: y, p_burst: p, block: *block }
            })
            .collect();
        let mut rx = Box::new(Receiver::new(fs as u32).unwrap());
        let mut rng = Rng(42);
        println!("== {}", sc.name);
        println!(" SNR/carrier  decoded  systematic  turbo  wrong  genie-timing  mean SNR est");
        let mut lo_sys = None;
        let mut lo_any = None;
        let mut hi_sys = None;
        let mut hi_any = None;
        for &snr in &snrs {
            let (mut sys, mut tur, mut bad, mut est, mut genie) = (0, 0, 0, 0.0, 0);
            for s in &snips {
                let x = capture(s, fs, snr, sc.q, &mut rng);
                let mut got = None;
                rx.decode_all(&x, &mut |r| {
                    if let Ok(b) = r
                        && b.crc24_ok
                    {
                        got = Some(b);
                    }
                });
                if got.is_none() {
                    // Missed by the energy trigger or sync? Try with the true
                    // burst position, to separate detection from decoding.
                    let b0 = (sc.window.0 * fs) as usize;
                    let span = Span { start: b0, end: b0 + (643e-6 * fs) as usize };
                    if let Ok(b) = rx.decode_span(&x, span, &mut |_| {})
                        && b.crc24_ok
                        && b.block == s.block
                    {
                        genie += 1;
                    }
                }
                if let Some(b) = got {
                    est += b.snr_db as f64;
                    if b.block != s.block {
                        bad += 1;
                    } else if b.path == Some(Path::Systematic) {
                        sys += 1;
                    } else {
                        tur += 1;
                    }
                }
            }
            let n = snips.len();
            let ok = sys + tur;
            println!(
                " {snr:6.0} dB   {ok:3}/{n}   {sys:5}      {tur:5}  {bad:4}   {genie:6}        {}",
                if ok > 0 { format!("{:.1}", est / ok as f64) } else { "-".into() }
            );
            if sys * 2 >= n {
                lo_sys.get_or_insert(snr);
                hi_sys = Some(snr);
            }
            if ok * 2 >= n {
                lo_any.get_or_insert(snr);
                hi_any = Some(snr);
            }
        }
        let f = |v: Option<f64>| v.map_or("-".to_string(), |v| format!("{v:.0}"));
        println!(
            " >= half decode: systematic {}..{} dB, any path {}..{} dB (per-carrier SNR vs 1 LSB noise)\n",
            f(lo_sys),
            f(hi_sys),
            f(lo_any),
            f(hi_any)
        );
    }

    // Stage timings on the host.
    println!("== host timings per stage (release build, mean of 50 runs)");
    for (fs, q) in [(50e6, Quant::Float), (40e6, Quant::Clip4), (20e6, Quant::Bits10)] {
        let (_, f, start, cfo, _) = &refs[1];
        let a = (*start - 150e-6 * 50e6) as usize;
        let seg = mix(&f[a..a + 50_000], -(cfo - 0.5e6), 50e6);
        let y = resample_f32(&seg, 50e6, fs, 0.45 * fs);
        let snip = Snip {
            p_burst: y.iter().map(|v| (v.0 as f64).powi(2) + (v.1 as f64).powi(2)).sum::<f64>() / y.len() as f64,
            x: y,
            block: [0; 173],
        };
        let x = capture(&snip, fs, 25.0, q, &mut Rng(1));
        let mut rx = Box::new(Receiver::new(fs as u32).unwrap());
        let mut spans = [Span::default(); 4];
        let n = droneid::rx::find_bursts(&x, fs as u32, &mut spans);
        assert!(n >= 1);
        let reps = 50;
        let mut acc = [0f64; 9];
        let t0 = Instant::now();
        for _ in 0..reps {
            droneid::rx::find_bursts(&x, fs as u32, &mut spans);
        }
        let detect = t0.elapsed().as_secs_f64() / reps as f64;
        let force_turbo = |rx: &mut Receiver, sys: bool, acc: &mut [f64; 9]| {
            rx.config.try_systematic = sys;
            let mut last = Instant::now();
            rx.decode_span(&x, spans[0], &mut |st| {
                let now = Instant::now();
                let i = match st {
                    Stage::CoarseCfo => 0,
                    Stage::Resample => 1,
                    Stage::CpSync => 2,
                    Stage::ZcSync => 3,
                    Stage::Demod => 4,
                    Stage::Derate => 5,
                    Stage::Systematic => 6,
                    Stage::Turbo => 7,
                };
                acc[i] += (now - last).as_secs_f64();
                last = now;
            })
            .unwrap();
        };
        for _ in 0..reps {
            force_turbo(&mut rx, true, &mut acc);
        }
        let us = |v: f64| v / reps as f64 * 1e6;
        println!(
            " {:>4.1} MS/s: detect {:.0} us | coarse CFO {:.0} | resample {:.0} | CP sync {:.0} | ZC sync {:.0} | demod {:.0} | derate {:.0} | systematic {:.0} us",
            fs / 1e6,
            detect * 1e6,
            us(acc[0]),
            us(acc[1]),
            us(acc[2]),
            us(acc[3]),
            us(acc[4]),
            us(acc[5]),
            us(acc[6]),
        );
    }
    // Turbo worst case: 8 full iterations on soft values that never pass CRC.
    let mut t = Box::new(droneid::turbo::Turbo::new());
    let mut soft = [[0i16; droneid::D]; 3];
    let mut r = Rng(9);
    for s in soft.iter_mut() {
        for v in s.iter_mut() {
            *v = (r.g() * 20.0) as i16;
        }
    }
    let mut out = [0u8; droneid::K / 8];
    let t0 = Instant::now();
    let reps = 20;
    let mut halves = 0;
    for _ in 0..reps {
        halves = t.decode(&soft, 8, &mut out).half_iterations;
    }
    let dt = t0.elapsed().as_secs_f64() / reps as f64;
    println!(" turbo: {:.0} us for {halves} half-iterations ({:.0} us each)", dt * 1e6, dt * 1e6 / halves as f64);
}
