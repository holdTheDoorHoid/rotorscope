//! The whole Path B chain on real DroneID bursts, as the C5 will run it:
//! 40 MS/s 4-bit capture bytes -> probe energy trigger -> frozen window ->
//! half-band to 20 MS/s -> DC removal -> `droneid::rx::Receiver`.
//!
//! Uses RUB-SysSec's public recordings (fetch with
//! `tools/fetch-droneid-samples.sh`; set `DRONEID_TESTDATA` to their
//! directory). Skips with a message when absent. Never prints identities.

use droneid::host::{Format, decode_recording, resample_f32, to_c16, to_f32_pairs};
use droneid::math::{C16, remove_dc};
use droneid::rx::Receiver;
use rawcap::dsp::{HB_REACH, decimate_nibbles};
use rawcap::iq::{Window, pack, power_sum};
use rawcap::trigger::{Config, Trigger};

fn load(name: &str) -> Option<Vec<u8>> {
    let dir = std::env::var("DRONEID_TESTDATA")
        .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/droneid").to_string());
    let path = std::path::Path::new(&dir).join(name);
    match std::fs::read(&path) {
        Ok(b) => Some(b),
        Err(_) => {
            eprintln!("SKIPPED: {} not found; run tools/fetch-droneid-samples.sh", path.display());
            None
        }
    }
}

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
        (-2.0 * a.ln()).sqrt() * (2.0 * std::f64::consts::PI * b).cos()
    }
}

/// Every CRC-valid burst of a recording, moved to +0.5 MHz (where a C5 tuned
/// to the whole MHz below a DroneID channel sees it), band-limited to the
/// analog low-pass (about +-10 MHz) and resampled to 40 MS/s, with 50 µs
/// margins: (samples, block).
type Bursts = Vec<(Vec<(f32, f32)>, [u8; 173])>;

fn bursts_at_40(name: &str) -> Option<Bursts> {
    let raw = load(name)?;
    let f = to_f32_pairs(&raw);
    let x = to_c16(&raw, Format::Cf32);
    let mut rx = Box::new(Receiver::new(50_000_000).unwrap());
    let mut out = vec![];
    decode_recording(&mut rx, &x, |r| {
        if let Ok(b) = r.result
            && b.crc24_ok
        {
            let a = (r.chunk_start + b.span.start).saturating_sub(2500);
            let e = (r.chunk_start + b.span.end + 2500).min(f.len());
            let shift = 0.5e6 - b.cfo_hz;
            let mixed: Vec<(f32, f32)> = f[a..e]
                .iter()
                .enumerate()
                .map(|(n, &(i, q))| {
                    let ph = 2.0 * std::f64::consts::PI * shift * n as f64 / 50e6;
                    let (s, c) = ph.sin_cos();
                    ((i as f64 * c - q as f64 * s) as f32, (i as f64 * s + q as f64 * c) as f32)
                })
                .collect();
            out.push((resample_f32(&mixed, 50e6, 40e6, 9.5e6), b.block));
        }
    });
    Some(out)
}

/// Build a capture stream: noise, each burst at `snr` above it, 3 ms apart.
/// Quantised by `w` with noise at `noise_codes` RMS per axis.
fn stream(bursts: &Bursts, w: Window, noise_codes: f64, burst_rms: f64) -> Vec<u8> {
    let mut rng = Rng(42);
    let gap = 120_000; // 3 ms
    let mut out = Vec::new();
    let q = |v: (f64, f64), rng: &mut Rng| -> u8 {
        let i = (v.0 + noise_codes * rng.g()).floor().clamp(-512.0, 511.0) as i16;
        let qq = (v.1 + noise_codes * rng.g()).floor().clamp(-512.0, 511.0) as i16;
        pack(w.quantise(i), w.quantise(qq))
    };
    for (b, _) in bursts {
        for _ in 0..gap {
            out.push(q((0.0, 0.0), &mut rng));
        }
        let p = (b.iter().map(|&(a, c)| (a as f64).powi(2) + (c as f64).powi(2)).sum::<f64>() / b.len() as f64).sqrt();
        let g = burst_rms / p;
        for &(a, c) in b {
            out.push(q((a as f64 * g, c as f64 * g), &mut rng));
        }
    }
    for _ in 0..gap {
        out.push(q((0.0, 0.0), &mut rng));
    }
    out
}

/// Run the firmware's loop over a byte stream; return CRC-valid blocks.
fn run(bytes: &[u8]) -> (Vec<[u8; 173]>, u32) {
    let cfg = Config::default();
    let mut trig = Trigger::new(cfg);
    let mut rx = Box::new(Receiver::new(20_000_000).unwrap());
    let mut cap = vec![C16::default(); 16_384];
    let need = 2 * cap.len() + 2 * HB_REACH;
    let pre = 4096u64;
    let mut found = vec![];
    let mut pos = 0u64;
    while pos + cfg.block as u64 <= bytes.len() as u64 {
        let p = pos as usize;
        let power = power_sum(&bytes[p..p + cfg.probe as usize]);
        if let Some(r) = trig.feed(pos, power) {
            let start = r.pos.saturating_sub(pre) as usize;
            if start + need > bytes.len() {
                break;
            }
            decimate_nibbles(|k| bytes[start + k], &mut cap);
            remove_dc(&mut cap);
            rx.decode_all(&cap, &mut |res| {
                if let Ok(b) = res
                    && b.crc24_ok
                {
                    found.push(b.block);
                }
            });
            // The ring restarts after the window; the trigger keeps its floor.
            pos = (start + need) as u64 / cfg.block as u64 * cfg.block as u64 + cfg.block as u64;
            continue;
        }
        pos += cfg.block as u64;
    }
    (found, trig.rises)
}

#[test]
fn path_b_chain_decodes_real_bursts() {
    let mut all = vec![];
    for name in ["mavic_air_2", "mini2_sm"] {
        let Some(b) = bursts_at_40(name) else { return };
        all.extend(b);
    }
    assert!(all.len() >= 5, "in-band reference bursts: {}", all.len());
    // Clip window, noise at one 4-bit step (64 codes), bursts ~10 dB above:
    // the operating point the decoder research recommends.
    for (w, noise, rms) in [(Window::CLIP, 64.0, 200.0), (Window::FOLD, 16.0, 50.0)] {
        let s = stream(&all, w, noise, rms);
        let (got, rises) = run(&s);
        let want: Vec<_> = all.iter().map(|b| b.1).collect();
        // No rise without a burst, and every burst found and decoded.
        assert_eq!(rises as usize, want.len(), "{}: rises", w.name());
        assert_eq!(got.len(), want.len(), "{}: decoded {} of {}", w.name(), got.len(), want.len());
        for g in &got {
            assert!(want.contains(g), "{}: a decoded block is not one of the bursts", w.name());
        }
    }
}
