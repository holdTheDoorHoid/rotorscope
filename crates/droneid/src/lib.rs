//! DJI OcuSync "DroneID" receiver, `no_std` and fixed point.
//!
//! Every OcuSync DJI aircraft sends a short LTE-like OFDM burst (600 QPSK
//! carriers at 15 kHz, 1024-point FFT at 15.36 MS/s, two Zadoff-Chu
//! reference symbols, LTE turbo code) carrying its serial number and the
//! positions of the aircraft, its home point and the pilot. This crate turns
//! raw complex baseband into those fields. See [`rx`] for the stages.
//!
//! ```no_run
//! # use droneid::{math::C16, rx::Receiver};
//! # let samples: &[C16] = &[];
//! let mut rx = Receiver::new(20_000_000).unwrap(); // capture rate, S/s
//! rx.decode_all(samples, &mut |r| {
//!     if let Ok(burst) = r && burst.crc24_ok {
//!         // burst.parsed: the record; burst.path: which decoder got it
//!     }
//! });
//! ```
//!
//! # What it handles
//! - Capture rates 15.36, 20, 30.72, 40 and 50 MS/s (any rate whose ratio to
//!   15.36 MHz reduces to `L/M` with `L <= 192`), bursts anywhere in the
//!   captured band, both burst layouts (9 symbols, and the 8-symbol layout
//!   of Mavic Pro / Mavic 2 and, per its published spec, O4).
//! - Two decode paths: (a) hard decisions on the systematic bits plus
//!   CRC-24A (what the NDSS receiver does; nearly free), and (b) a
//!   max-log-MAP turbo decoder with CRC early stopping, run when (a) fails.
//!   On the public recordings (a) always suffices; on synthetic bursts (b)
//!   gains about 8 dB.
//! - The inner record: OcuSync 2 flight information ([`payload::FlightInfo`]),
//!   a second plaintext record type (selector 0x11), and O4's encrypted
//!   `CRYP` / `INFP` records, of which only the cleartext key ID and header
//!   are reported. **No decryption and no key handling, ever** (PLAN.md,
//!   Phase 10). The O4 parsing is built from the published layout and is
//!   untested on real signals.
//!
//! # Numerics
//! Written for an ESP32-C5 (RV32IMAC, 240 MHz, no FPU, 384 KB RAM):
//! - Samples are `i16` complex. Every per-sample loop (NCO mixing,
//!   resampling, cyclic-prefix correlation, FFT, equalisation, turbo
//!   decoding) is integer: `i16 x i16 -> i32` products, `i64` only for long
//!   correlation sums.
//! - FFT: an in-house 1024-point radix-2 transform on `i16` with block
//!   floating point ([`fft`]); about 13 bits of working precision at any
//!   input level, so 4-bit and 16-bit captures share one path.
//! - Resampling: polyphase FIR, 24 Q14 taps per branch ([`resample`]).
//! - Angles are 32-bit binary angles (wrap for free); `atan2` is CORDIC;
//!   sines come from a 1024-entry Q15 table built at compile time.
//! - Soft values are `i8` from the demapper and `i16` in the turbo decoder
//!   (positive means bit 0).
//! - `f64` appears only once per burst or per configuration: the coarse and
//!   fractional frequency estimates, the SNR in dB, interpolation fractions,
//!   and the filter design (soft-float on the C5, microseconds to
//!   milliseconds, never per sample). Tables are `const`-evaluated.
//! - No heap. [`rx::Receiver`] owns all working memory, about 125 KiB (52 KiB
//!   burst buffer, 37 KiB turbo, 9 KiB filter taps, the rest small); the
//!   deepest call adds about 30 KiB of stack. Put it in a `static` on a chip.
//!
//! # Sources
//! Protocol facts come from the NDSS 2023 paper and its receiver
//! (RUB-SysSec/DroneSecurity, read for facts only), proto17/dji_droneid's
//! notes, anarkiwi/samples2djidroneid's field table, and
//! EdwardBlair/dji-droneid-spec (O4 records, PHY constants, the scrambler
//! example used as a test vector). Coding facts are 3GPP TS 36.211/36.212. No
//! third-party code is included.
//!
//! # Privacy
//! A decoded burst carries a real serial number and often a UUID tied to the
//! pilot's app. Tests here check their format only; the `droneid-file` tool
//! has `--redact` for sharing output.
#![cfg_attr(not(any(test, feature = "std")), no_std)]

pub mod crc;
pub mod fft;
#[cfg(feature = "std")]
pub mod host;
pub mod math;
pub mod ratematch;
pub mod payload;
pub mod resample;
pub mod rx;
pub mod zc;
pub mod scramble;
#[cfg(test)]
mod synth;
pub mod turbo;
#[cfg(test)]
mod testutil;

/// Transport block size with CRC-24A, in bits (173 bytes + 3).
pub const K: usize = 1408;
/// Bits per turbo output stream, including 4 tail bits.
pub const D: usize = K + 4;
/// Rate-matched bits per burst: 6 data symbols x 600 carriers x 2.
pub const E: usize = 7200;
