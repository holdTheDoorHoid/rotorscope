//! **Test-only** synthetic DroneID burst generator (`#[cfg(test)]`; never
//! compiled into a library build, let alone firmware: Rotorscope never
//! transmits DroneID). It runs the transmit chain in `f64` (CRC-24A, turbo
//! encoding, rate matching, scrambling, QPSK, Zadoff-Chu references, IFFT,
//! cyclic prefixes), then a channel (multipath, frequency offset, timing
//! offset, white noise), resamples to the capture rate and quantises.

use crate::crc::crc24a;
use crate::payload::BLOCK;
use crate::ratematch::rate_match;
use crate::rx::Layout;
use crate::scramble;
use crate::testutil::Rng;
use crate::turbo::encode;
use crate::zc::{NCAR, ROOT_A, ROOT_B, carrier};
use crate::{E, K};

type Cx = (f64, f64);
const PI: f64 = std::f64::consts::PI;
const NFFT: usize = 1024;

fn cmul(a: Cx, b: Cx) -> Cx {
    (a.0 * b.0 - a.1 * b.1, a.0 * b.1 + a.1 * b.0)
}

/// In-place radix-2 inverse FFT (no 1/N).
fn ifft(x: &mut [Cx]) {
    let n = x.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            x.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let a = 2.0 * PI / len as f64;
        for s in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let w = ((a * k as f64).cos(), (a * k as f64).sin());
                let u = x[s + k];
                let v = cmul(x[s + k + len / 2], w);
                x[s + k] = (u.0 + v.0, u.1 + v.1);
                x[s + k + len / 2] = (u.0 - v.0, u.1 - v.1);
            }
        }
        len <<= 1;
    }
}

fn zc_f64(u: u32, k: i32) -> Cx {
    let i = (k + 300) as f64;
    let a = -PI * u as f64 * i * (i + 1.0) / 601.0;
    (a.cos(), a.sin())
}

/// The 7200 coded, scrambled bits for a payload.
pub fn coded_bits(payload: &[u8; BLOCK]) -> [u8; E] {
    let mut block = [0u8; K / 8];
    block[..BLOCK].copy_from_slice(payload);
    let c = crc24a(payload);
    block[BLOCK..].copy_from_slice(&[(c >> 16) as u8, (c >> 8) as u8, c as u8]);
    let bits: Vec<u8> = (0..K).map(|i| (block[i / 8] >> (7 - i % 8)) & 1).collect();
    let mut e = rate_match(&encode(&bits));
    for (n, b) in e.iter_mut().enumerate() {
        *b ^= scramble::bit(n);
    }
    e
}

/// One burst at 15.36 MS/s, unit average power per occupied carrier.
pub fn burst_1536(payload: &[u8; BLOCK], layout: Layout, rng: &mut Rng) -> Vec<Cx> {
    let bits = coded_bits(payload);
    let (ra, rb) = layout.refs();
    let data = layout.data();
    let us = layout.useful_starts();
    let mut out = vec![(0.0, 0.0); layout.len()];
    let r = std::f64::consts::FRAC_1_SQRT_2;
    for (s, &u) in us.iter().enumerate() {
        let mut f = vec![(0.0, 0.0); NFFT];
        for m in 0..NCAR {
            let k = carrier(m);
            let v = if s == ra {
                zc_f64(ROOT_A, k)
            } else if s == rb {
                zc_f64(ROOT_B, k)
            } else if let Some(j) = data.iter().position(|&d| d == s) {
                let b0 = bits[(j * NCAR + m) * 2];
                let b1 = bits[(j * NCAR + m) * 2 + 1];
                ((1.0 - 2.0 * b0 as f64) * r, (1.0 - 2.0 * b1 as f64) * r)
            } else {
                // The 9-symbol layout's first symbol: content not modelled.
                (if rng.u8() & 1 == 0 { r } else { -r }, if rng.u8() & 1 == 0 { r } else { -r })
            };
            f[k.rem_euclid(NFFT as i32) as usize] = v;
        }
        ifft(&mut f);
        let cp = if s == 0 { u } else { u - us[s - 1] - NFFT };
        for (i, o) in out[u - cp..u + NFFT].iter_mut().enumerate() {
            *o = f[(i + NFFT - cp) % NFFT];
        }
    }
    let scale = 1.0 / (NFFT as f64).sqrt();
    out.iter().map(|&(a, b)| (a * scale, b * scale)).collect()
}

/// Channel and capture settings.
#[derive(Clone, Copy, Debug)]
pub struct Capture {
    pub fs: u32,
    /// Per-carrier SNR (Es/N0 at the FFT output), dB.
    pub snr_db: f64,
    pub cfo_hz: f64,
    /// Leading samples (at `fs`) before the burst, plus a fraction.
    pub lead: f64,
    /// Second path: (relative amplitude, delay in 15.36 MS/s samples).
    pub echo: Option<(f64, f64)>,
    /// Peak amplitude after scaling to `i16`.
    pub peak: f64,
}

impl Capture {
    pub fn clean(fs: u32) -> Self {
        Self { fs, snr_db: 30.0, cfo_hz: 0.0, lead: 3000.0, echo: None, peak: 12000.0 }
    }
}

/// Windowed-sinc interpolation of `x` (at 15.36 MS/s) at time `t` (in input
/// samples).
fn interp(x: &[Cx], t: f64) -> Cx {
    const HALF: i64 = 24;
    let c = t.floor() as i64;
    let mut acc = (0.0, 0.0);
    // Low-pass at 7.3 MHz of 15.36 (the burst is +-4.5 MHz).
    let fc = 7.3e6 / 15.36e6;
    for m in c - HALF + 1..=c + HALF {
        if m < 0 || m as usize >= x.len() {
            continue;
        }
        let d = t - m as f64;
        let s = if d.abs() < 1e-12 { 2.0 * fc } else { (2.0 * PI * fc * d).sin() / (PI * d) };
        let w = 0.5 + 0.5 * (PI * d / HALF as f64).cos();
        let g = s * w;
        acc.0 += x[m as usize].0 * g;
        acc.1 += x[m as usize].1 * g;
    }
    acc
}

/// A complete capture holding one burst; returns the samples and the burst
/// start (first CP sample) in capture samples.
pub fn capture(payload: &[u8; BLOCK], layout: Layout, cap: &Capture, rng: &mut Rng) -> (Vec<crate::math::C16>, f64) {
    let mut b = burst_1536(payload, layout, rng);
    if let Some((amp, delay)) = cap.echo {
        let src = b.clone();
        let d = delay.floor() as usize;
        for (i, o) in b.iter_mut().enumerate() {
            if i > d {
                let v = interp(&src, i as f64 - delay);
                o.0 += amp * v.0;
                o.1 += amp * v.1;
            }
        }
    }
    let ratio = 15.36e6 / cap.fs as f64;
    let tail = 0.4e-3 * cap.fs as f64;
    let total = (cap.lead + b.len() as f64 / ratio + tail) as usize;
    let p_sig: f64 = b.iter().map(|v| v.0 * v.0 + v.1 * v.1).sum::<f64>() / b.len() as f64;
    // Es/N0 per carrier = P_sig * fs / (sigma^2 * 9 MHz).
    let snr = 10f64.powf(cap.snr_db / 10.0);
    let sigma2 = p_sig * cap.fs as f64 / (9.0e6 * snr);
    let sd = (sigma2 / 2.0).sqrt();
    let mut y: Vec<Cx> = Vec::with_capacity(total);
    for n in 0..total {
        let t_in = (n as f64 - cap.lead) * ratio;
        let mut v = if t_in > -30.0 && t_in < b.len() as f64 + 30.0 { interp(&b, t_in) } else { (0.0, 0.0) };
        let a = 2.0 * PI * cap.cfo_hz * n as f64 / cap.fs as f64;
        v = cmul(v, (a.cos(), a.sin()));
        v.0 += sd * rng.gauss();
        v.1 += sd * rng.gauss();
        y.push(v);
    }
    let peak = y.iter().fold(0f64, |m, v| m.max(v.0.abs()).max(v.1.abs()));
    let g = cap.peak / peak;
    let q = y
        .iter()
        .map(|v| crate::math::C16 { re: (v.0 * g).round() as i16, im: (v.1 * g).round() as i16 })
        .collect();
    (q, cap.lead)
}

/// A random flight-information payload with a valid inner CRC.
pub fn random_payload(rng: &mut Rng) -> [u8; BLOCK] {
    let mut b = [0u8; BLOCK];
    b[0] = 88;
    b[1] = 0x10;
    b[2] = 2;
    for v in b[3..89].iter_mut() {
        *v = rng.u8();
    }
    b[7..23].copy_from_slice(b"SYNTHETIC0000001");
    b[67] = 63;
    b[68] = 6;
    let c = crate::crc::dji_crc16(&b[..89]);
    b[89..91].copy_from_slice(&c.to_le_bytes());
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload::Record;
    use crate::rx::{Burst, Path, Receiver, Span, find_bursts};

    const RATES: [u32; 5] = [50_000_000, 40_000_000, 30_720_000, 20_000_000, 15_360_000];

    fn decode(x: &[crate::math::C16], fs: u32, max_iter: u8) -> Vec<Burst> {
        let mut rx = Box::new(Receiver::new(fs).unwrap());
        rx.config.max_iterations = max_iter;
        let mut v = vec![];
        rx.decode_all(x, &mut |r| {
            if let Ok(b) = r {
                v.push(b);
            }
        });
        v
    }

    fn decode_known(x: &[crate::math::C16], fs: u32, start: f64, layout: Layout) -> Burst {
        let mut rx = Box::new(Receiver::new(fs).unwrap());
        let len = layout.len() as f64 * fs as f64 / 15.36e6;
        let span = Span { start: start as usize, end: (start + len) as usize };
        rx.decode_span(x, span, &mut |_| {}).expect("sync")
    }

    #[test]
    fn every_rate_and_layout_clean() {
        let mut rng = Rng::new(1);
        for fs in RATES {
            for layout in [Layout::Nine, Layout::Eight] {
                let p = random_payload(&mut rng);
                let cap = Capture { cfo_hz: 1.234e6 * if fs > 16e6 as u32 { 1.0 } else { 0.5 }, lead: 2345.6, ..Capture::clean(fs) };
                let (x, start) = capture(&p, layout, &cap, &mut rng);
                let got = decode(&x, fs, 8);
                assert_eq!(got.len(), 1, "{fs} {layout:?}: {} bursts", got.len());
                let b = &got[0];
                assert_eq!(b.layout, layout, "{fs}");
                assert!(b.crc24_ok, "{fs} {layout:?}: CRC failed, snr {}", b.snr_db);
                assert_eq!(b.path, Some(Path::Systematic));
                assert_eq!(b.block, p);
                assert_eq!(b.systematic_errors, 0);
                assert!((b.cfo_hz - cap.cfo_hz).abs() < 200.0, "{fs}: cfo {} vs {}", b.cfo_hz, cap.cfo_hz);
                assert!((b.start - start).abs() < 2.0 * fs as f64 / 15.36e6, "{fs}: start {} vs {start}", b.start);
                assert!(b.snr_db > 22.0, "{fs}: snr {}", b.snr_db);
                let parsed = b.parsed.as_ref().unwrap();
                assert_eq!(parsed.crc16_ok, Some(true));
                assert!(matches!(&parsed.record, Record::FlightInfo(f) if &f.serial == b"SYNTHETIC0000001"));
            }
        }
    }

    #[test]
    fn frequency_offsets_and_timing() {
        let mut rng = Rng::new(2);
        // Whole-carrier, fractional and large offsets, both signs, with
        // random timing; at 50 MS/s offsets up to +-15 MHz fit.
        let cases = [
            (50_000_000, 15.0e6),
            (50_000_000, -12.47e6),
            (40_000_000, 7_507.0),
            (40_000_000, -60_000.0),
            (20_000_000, 501_234.0),
            (20_000_000, -4.9e6),
            (15_360_000, 2.0e6),
            (30_720_000, -9.999e6),
        ];
        for (fs, cfo) in cases {
            for layout in [Layout::Nine, Layout::Eight] {
                let p = random_payload(&mut rng);
                let lead = 1000.0 + rng.f64() * 5000.0;
                let cap = Capture { cfo_hz: cfo, lead, snr_db: 20.0, ..Capture::clean(fs) };
                let (x, _) = capture(&p, layout, &cap, &mut rng);
                let got = decode(&x, fs, 8);
                assert_eq!(got.len(), 1, "{fs} {cfo} {layout:?}");
                assert!(got[0].crc24_ok, "{fs} {cfo} {layout:?}: snr {}", got[0].snr_db);
                assert_eq!(got[0].block, p);
                assert!((got[0].cfo_hz - cfo).abs() < 300.0, "{fs}: {} vs {cfo}", got[0].cfo_hz);
            }
        }
    }

    #[test]
    fn multipath() {
        let mut rng = Rng::new(3);
        for (amp, delay) in [(0.5, 7.3), (0.7, 30.0), (0.3, 55.0)] {
            for layout in [Layout::Nine, Layout::Eight] {
                let p = random_payload(&mut rng);
                let cap = Capture { echo: Some((amp, delay)), snr_db: 25.0, cfo_hz: -333_333.0, ..Capture::clean(40_000_000) };
                let (x, _) = capture(&p, layout, &cap, &mut rng);
                let got = decode(&x, 40_000_000, 8);
                assert_eq!(got.len(), 1);
                assert!(got[0].crc24_ok, "echo {amp} at {delay}: snr {}", got[0].snr_db);
                assert_eq!(got[0].block, p);
            }
        }
    }

    #[test]
    fn snr_sweep_uses_the_right_path() {
        let mut rng = Rng::new(4);
        let fs = 20_000_000;
        for layout in [Layout::Nine, Layout::Eight] {
            // High SNR: the cheap path is enough.
            let p = random_payload(&mut rng);
            let cap = Capture { snr_db: 15.0, cfo_hz: 250e3, ..Capture::clean(fs) };
            let (x, start) = capture(&p, layout, &cap, &mut rng);
            let b = decode_known(&x, fs, start, layout);
            assert_eq!((b.path, b.block), (Some(Path::Systematic), p), "15 dB");
            assert!((b.snr_db as f64 - 15.0).abs() < 3.0, "SNR estimate {} at 15 dB", b.snr_db);
            // Low SNR: only the turbo decoder gets it.
            let mut turbo_seen = 0;
            for _ in 0..4 {
                let p = random_payload(&mut rng);
                let cap = Capture { snr_db: 2.0, cfo_hz: -250e3, ..Capture::clean(fs) };
                let (x, start) = capture(&p, layout, &cap, &mut rng);
                let b = decode_known(&x, fs, start, layout);
                if b.path == Some(Path::Turbo) {
                    assert_eq!(b.block, p);
                    turbo_seen += 1;
                }
                assert_ne!(b.path, Some(Path::Systematic), "2 dB should defeat the systematic path");
            }
            assert!(turbo_seen >= 3, "turbo decoded only {turbo_seen}/4 at 2 dB");
            // Hopeless: nothing passes, nothing false.
            let p = random_payload(&mut rng);
            let cap = Capture { snr_db: -8.0, ..Capture::clean(fs) };
            let (x, start) = capture(&p, layout, &cap, &mut rng);
            let mut rx = Box::new(Receiver::new(fs).unwrap());
            let len = layout.len() as f64 * fs as f64 / 15.36e6;
            let span = Span { start: start as usize, end: (start + len) as usize };
            if let Ok(b) = rx.decode_span(&x, span, &mut |_| {}) {
                assert!(!b.crc24_ok);
            }
        }
    }

    #[test]
    fn noise_alone_finds_nothing() {
        let mut rng = Rng::new(5);
        for fs in [50_000_000u32, 20_000_000] {
            let x: Vec<_> = (0..fs as usize / 250)
                .map(|_| crate::math::C16 { re: (rng.gauss() * 900.0) as i16, im: (rng.gauss() * 900.0) as i16 })
                .collect();
            let mut spans = [Span::default(); 8];
            assert_eq!(find_bursts(&x, fs, &mut spans), 0);
        }
    }

    #[test]
    fn coarse_four_bit_capture_still_decodes() {
        // 4-bit I/Q at 40 MS/s (the C5's continuous path), modelled as clipping
        // to -8..7 then shifted to full scale.
        let mut rng = Rng::new(6);
        let fs = 40_000_000;
        let p = random_payload(&mut rng);
        let cap = Capture { snr_db: 25.0, cfo_hz: 500e3, peak: 7.0 * 4096.0, ..Capture::clean(fs) };
        let (x, _) = capture(&p, Layout::Nine, &cap, &mut rng);
        let q: Vec<_> = x
            .iter()
            .map(|c| {
                let f = |v: i16| ((v as i32 + 2048).div_euclid(4096).clamp(-8, 7) << 12) as i16;
                crate::math::C16 { re: f(c.re), im: f(c.im) }
            })
            .collect();
        let got = decode(&q, fs, 8);
        assert_eq!(got.len(), 1);
        assert!(got[0].crc24_ok);
        assert_eq!(got[0].block, p);
    }

    #[test]
    fn receiver_built_in_place_decodes_like_new() {
        // The chip builds its receiver in a zeroed PSRAM slot with
        // `init_in_place`; it must behave exactly like `Receiver::new`.
        let mut rng = Rng::new(7);
        let fs = 20_000_000;
        let p = random_payload(&mut rng);
        let cap = Capture { snr_db: 20.0, cfo_hz: 500e3, ..Capture::clean(fs) };
        let (x, _) = capture(&p, Layout::Nine, &cap, &mut rng);
        // Dirty the slot first: init_in_place must not depend on it being zero.
        let mut slot: Box<core::mem::MaybeUninit<Receiver>> = Box::new_uninit();
        unsafe { core::ptr::write_bytes(slot.as_mut_ptr().cast::<u8>(), 0xa5, core::mem::size_of::<Receiver>()) };
        let rx = Receiver::init_in_place(&mut slot, fs).unwrap();
        assert_eq!(rx.fs_in(), fs);
        let mut got = vec![];
        rx.decode_all(&x, &mut |r| {
            if let Ok(b) = r {
                got.push(b);
            }
        });
        let want = decode(&x, fs, crate::rx::Config::default().max_iterations);
        assert_eq!(got.len(), 1);
        assert_eq!(want.len(), 1);
        assert!(got[0].crc24_ok);
        assert_eq!(got[0].block, want[0].block);
        assert_eq!(got[0].block, p);
    }

    #[test]
    fn dc_bias_removed_then_decodes() {
        // A truncating 4-bit capture with a large DC step on top: remove_dc
        // brings the mean back to zero and the burst still decodes.
        let mut rng = Rng::new(8);
        let fs = 40_000_000;
        let p = random_payload(&mut rng);
        let cap = Capture { snr_db: 25.0, cfo_hz: 500e3, peak: 6.0 * 4096.0, ..Capture::clean(fs) };
        let (x, _) = capture(&p, Layout::Nine, &cap, &mut rng);
        let mut q: Vec<_> = x
            .iter()
            .map(|c| {
                // floor() like the hardware, plus one LSB of DC on I.
                let f = |v: i16, dc: i32| (((v as i32).div_euclid(4096) + dc).clamp(-8, 7) << 12) as i16;
                crate::math::C16 { re: f(c.re, 1), im: f(c.im, 0) }
            })
            .collect();
        let dc = crate::math::remove_dc(&mut q);
        // floor() costs -0.5 LSB on both axes; I also carries +1 LSB.
        assert!((dc.re - 2048).abs() < 600 && (dc.im + 2048).abs() < 600, "removed {dc:?}");
        let (sr, si) = q.iter().fold((0i64, 0i64), |(a, b), v| (a + v.re as i64, b + v.im as i64));
        assert!(sr.abs() < q.len() as i64 && si.abs() < q.len() as i64);
        let got = decode(&q, fs, 8);
        assert_eq!(got.len(), 1);
        assert!(got[0].crc24_ok);
        assert_eq!(got[0].block, p);
    }
}
