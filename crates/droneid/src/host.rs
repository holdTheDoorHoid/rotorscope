//! Host conveniences (feature `std`): reading raw IQ files and decoding a
//! whole recording in overlapping chunks. Not part of the decode path.

extern crate std;

use std::vec::Vec;

use crate::math::C16;
use crate::rx::{Burst, MAX_WINDOWS, Receiver, Span, SyncError, find_bursts};

/// Raw IQ sample formats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// Interleaved little-endian `f32` I, Q (GNU Radio `complex64`, "cf32").
    Cf32,
    /// Interleaved signed 8-bit I, Q (HackRF, "ci8").
    Ci8,
    /// Interleaved little-endian `i16` I, Q (USRP sc16, "ci16").
    Ci16,
}

impl Format {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "cf32" | "complex64" | "fc32" => Format::Cf32,
            "ci8" | "cs8" | "int8" => Format::Ci8,
            "ci16" | "cs16" | "sc16" | "int16" => Format::Ci16,
            _ => return None,
        })
    }
}

/// Convert raw bytes to 16-bit samples. Floats are scaled so the largest
/// component lands at 16000 (headroom for the fixed-point chain); 8-bit
/// samples are shifted up by 7; 16-bit samples pass through.
pub fn to_c16(bytes: &[u8], fmt: Format) -> Vec<C16> {
    match fmt {
        Format::Cf32 => {
            let f: Vec<f32> = bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
            let peak = f.iter().fold(0f32, |m, v| m.max(v.abs())).max(1e-30);
            let g = 16000.0 / peak;
            f.as_chunks::<2>().0.iter().map(|p| C16 { re: (p[0] * g).round() as i16, im: (p[1] * g).round() as i16 }).collect()
        }
        Format::Ci8 => bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| C16 { re: ((p[0] as i8 as i16) << 7), im: ((p[1] as i8 as i16) << 7) })
            .collect(),
        Format::Ci16 => bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| C16 { re: i16::from_le_bytes([p[0], p[1]]), im: i16::from_le_bytes([p[2], p[3]]) })
            .collect(),
    }
}

/// Raw float samples (for experiments that resample or quantise).
pub fn to_f32_pairs(bytes: &[u8]) -> Vec<(f32, f32)> {
    bytes
        .as_chunks::<8>()
        .0
        .iter()
        .map(|b| (f32::from_le_bytes([b[0], b[1], b[2], b[3]]), f32::from_le_bytes([b[4], b[5], b[6], b[7]])))
        .collect()
}

/// One result from [`decode_recording`]: the span's offset into the whole
/// recording plus what the receiver made of it.
pub struct Found {
    /// Absolute input sample index of the chunk the span is relative to.
    pub chunk_start: usize,
    pub result: Result<Burst, (Span, SyncError)>,
}

/// Find and decode every burst in a recording of any length, in chunks of
/// [`MAX_WINDOWS`] energy windows overlapping by 1.5 ms, so each burst is
/// seen whole in exactly one chunk.
pub fn decode_recording(rx: &mut Receiver, x: &[C16], mut out: impl FnMut(Found)) {
    let fs = rx.fs_in() as usize;
    let w = fs / 62_500;
    let chunk = w * MAX_WINDOWS;
    let overlap = fs * 3 / 2000;
    let step = chunk - overlap;
    let mut start = 0;
    loop {
        let end = (start + chunk).min(x.len());
        let part = &x[start..end];
        let last = end == x.len();
        let mut spans = [Span::default(); 32];
        let n = find_bursts(part, rx.fs_in(), &mut spans);
        for &s in &spans[..n] {
            // Owned by this chunk unless the next chunk starts before it.
            if !last && s.start >= step + w {
                continue;
            }
            let result = rx.decode_span(part, s, &mut |_| {}).map_err(|e| (s, e));
            out(Found { chunk_start: start, result });
        }
        if last {
            break;
        }
        start += step;
    }
}

/// Band-limited resampling of float IQ (windowed sinc, `half` taps each
/// side at the lower of the two rates, low-pass at `cutoff_hz`). Slow but
/// accurate; for simulating other capture rates from a recording.
pub fn resample_f32(x: &[(f32, f32)], fs_in: f64, fs_out: f64, cutoff_hz: f64) -> Vec<(f32, f32)> {
    use std::f64::consts::PI;
    let n_out = (x.len() as f64 * fs_out / fs_in) as usize;
    let fc = cutoff_hz / fs_in; // cycles per input sample
    // Kernel support in input samples: 16 cycles of the cut-off.
    let half = (16.0 / (2.0 * fc)).ceil() as i64;
    let mut out = Vec::with_capacity(n_out);
    for n in 0..n_out {
        let t = n as f64 * fs_in / fs_out;
        let c = t.floor() as i64;
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for m in (c - half + 1).max(0)..=(c + half).min(x.len() as i64 - 1) {
            let d = t - m as f64;
            let s = if d.abs() < 1e-12 { 2.0 * fc } else { (2.0 * PI * fc * d).sin() / (PI * d) };
            let w = 0.5 + 0.5 * (PI * d / half as f64).cos();
            let g = s * w;
            re += x[m as usize].0 as f64 * g;
            im += x[m as usize].1 as f64 * g;
        }
        out.push((re as f32, im as f32));
    }
    out
}
