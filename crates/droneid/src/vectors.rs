//! **Test-only.** The O4 ("v3") forward PHY, stage by stage, checked against
//! the SHA-256 digests published in EdwardBlair/dji-droneid-spec `RF_PHY.md`
//! (commit 55bb886, "PHY implementation verification task"), then fed through
//! our receiver.
//!
//! The spec repository has no licence, so nothing here is copied from it:
//! the digests and the serialisation rules (bits packed MSB first; complex
//! values as interleaved little-endian `f32`; frequency symbols symbol-major
//! in FFT-shifted order) are facts used as expected results. The forward chain
//! is our own (`turbo::encode`, `ratematch`, `scramble`, `zc` and the f64
//! modulator in `synth`). Rotorscope never transmits: this only builds
//! sample arrays in memory for tests.

use crate::crc::crc24a;
use crate::payload::BLOCK;
use crate::ratematch::rate_match;
use crate::rx::Layout;
use crate::scramble;
use crate::turbo::encode;
use crate::zc::{NCAR, carrier};
use crate::{D, E, K};
use sha2::{Digest, Sha256};

const NFFT: usize = 1024;
type Cx = (f64, f64);

/// Published digests, in pipeline order.
pub const DIGESTS: [(&str, &str); 7] = [
    ("input transport", "73b1f1000c7677ebdcef2a2a25e27b06d9c163209add77a16f0e2b70e56d5c52"),
    ("transport + CRC-24A", "20c3bd6bfc7522ce2eecdc77589acb63419846e031df19a62861dbf272ff93cd"),
    ("turbo output", "12bbea41da53fbffa35038d3243d6e6b56e53fe595a3f88364f2f100a1fc6760"),
    ("rate-matched", "2a1cdb2ca6304982af40f019da93fd0170843171feb9aeb1def1025f8a6f9f06"),
    ("scrambled", "8a70e028f3fe6f2bda8cea2ae6b3ec239d41619a390a8fe633b3f2f119c4355a"),
    ("frequency symbols", "3b1867eb0d8b8b9400cbca4cb2e91aa0e3359b182f6c2a637f9840d25ab91301"),
    ("time-domain packet", "70578e11d9ddbc8ce1e27e0b3fe74de8fcaa445e32e5aca1e673cf0cf7bd88ad"),
];

/// The verification input: byte `i` has value `i`.
pub fn test_input() -> [u8; BLOCK] {
    core::array::from_fn(|i| i as u8)
}

/// Every intermediate stage of the forward PHY.
pub struct Stages {
    pub block: [u8; K / 8],
    /// 4236 bits: (systematic, parity 1, parity 2) for each of the 1412
    /// positions of the three LTE `d` streams. For the 1408 data positions
    /// that is "x, z, z'" per input bit; the last four triples are the 12 tail
    /// bits in 36.212 §5.1.3.2.2 order.
    pub turbo: Vec<u8>,
    pub rate_matched: [u8; E],
    pub scrambled: [u8; E],
    /// 8 x 1024, FFT-shifted (bin 512 is DC).
    pub freq: Vec<Cx>,
    /// 8784 samples at 15.36 MS/s, `ifft` scaled by 1/N.
    pub time: Vec<Cx>,
}

/// The data and reference symbols of the 8-symbol layout, in FFT-shifted bins.
pub fn freq_symbols(scrambled: &[u8; E]) -> Vec<Cx> {
    let layout = Layout::Eight;
    let (ra, rb) = layout.refs();
    let data = layout.data();
    let r = core::f64::consts::FRAC_1_SQRT_2;
    let mut f = vec![(0.0, 0.0); 8 * NFFT];
    for s in 0..8 {
        for m in 0..NCAR {
            let k = carrier(m);
            let v = if s == ra {
                super::synth::zc_f64(crate::zc::ROOT_A, k)
            } else if s == rb {
                super::synth::zc_f64(crate::zc::ROOT_B, k)
            } else {
                let j = data.iter().position(|&d| d == s).unwrap();
                let b0 = scrambled[(j * NCAR + m) * 2];
                let b1 = scrambled[(j * NCAR + m) * 2 + 1];
                ((1.0 - 2.0 * b0 as f64) * r, (1.0 - 2.0 * b1 as f64) * r)
            };
            f[s * NFFT + (k + 512) as usize] = v;
        }
    }
    f
}

pub fn stages(input: &[u8; BLOCK]) -> Stages {
    let mut block = [0u8; K / 8];
    block[..BLOCK].copy_from_slice(input);
    let c = crc24a(input);
    block[BLOCK..].copy_from_slice(&[(c >> 16) as u8, (c >> 8) as u8, c as u8]);
    let bits: Vec<u8> = (0..K).map(|i| (block[i / 8] >> (7 - i % 8)) & 1).collect();
    let d = encode(&bits);
    let turbo: Vec<u8> = (0..D).flat_map(|i| [d[0][i], d[1][i], d[2][i]]).collect();
    let rate_matched = rate_match(&d);
    let mut scrambled = rate_matched;
    for (n, b) in scrambled.iter_mut().enumerate() {
        *b ^= scramble::bit(n);
    }
    let freq = freq_symbols(&scrambled);
    let us = Layout::Eight.useful_starts();
    let mut time = vec![(0.0, 0.0); Layout::Eight.len()];
    for s in 0..8 {
        // Un-shift, inverse FFT, scale 1/N.
        let mut x: Vec<Cx> = (0..NFFT).map(|b| freq[s * NFFT + (b + 512) % NFFT]).collect();
        super::synth::ifft(&mut x);
        for v in x.iter_mut() {
            *v = (v.0 / NFFT as f64, v.1 / NFFT as f64);
        }
        let u = us[s];
        let cp = if s == 0 { u } else { u - us[s - 1] - NFFT };
        for (i, o) in time[u - cp..u + NFFT].iter_mut().enumerate() {
            *o = x[(i + NFFT - cp) % NFFT];
        }
    }
    Stages { block, turbo, rate_matched, scrambled, freq, time }
}

pub fn pack_bits(bits: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; bits.len().div_ceil(8)];
    for (i, &b) in bits.iter().enumerate() {
        out[i / 8] |= b << (7 - i % 8);
    }
    out
}

fn f32_bytes(v: &[Cx]) -> Vec<u8> {
    v.iter().flat_map(|&(a, b)| [(a as f32).to_le_bytes(), (b as f32).to_le_bytes()]).flatten().collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// SHA-256 of each stage, in [`DIGESTS`] order.
pub fn digests(st: &Stages, input: &[u8; BLOCK]) -> [String; 7] {
    let h = |b: &[u8]| hex(&Sha256::digest(b));
    [
        h(input),
        h(&st.block),
        h(&pack_bits(&st.turbo)),
        h(&pack_bits(&st.rate_matched)),
        h(&pack_bits(&st.scrambled)),
        h(&f32_bytes(&st.freq)),
        h(&f32_bytes(&st.time)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn published_side_checks() {
        let st = stages(&test_input());
        assert_eq!(&st.block[BLOCK..], &[0xb6, 0xa0, 0x38]);
        assert_eq!(st.turbo.len(), 4236);
        let first = |b: &[u8]| b[..32].iter().map(|&x| (b'0' + x) as char).collect::<String>();
        assert_eq!(first(&st.rate_matched), "00110011001100110011001100110011");
        assert_eq!(first(&st.scrambled), "11100001110100101110110101001010");
        assert_eq!(st.time.len(), 8784);
    }

    /// The five bit-level stages (input, CRC-24A, turbo, rate matching,
    /// scrambling) reproduce the published digests exactly: our transmit-side
    /// coding chain, and therefore the receive-side inverse the decoder runs,
    /// is the published one bit for bit.
    #[test]
    fn coding_stages_match_published_digests() {
        let input = test_input();
        let st = stages(&input);
        let got = digests(&st, &input);
        for ((name, want), got) in DIGESTS.iter().zip(got.iter()).take(5) {
            assert_eq!(got, want, "stage '{name}' differs");
        }
    }

    /// The two floating-point stages do **not** reproduce the published
    /// digests, although every value follows the spec text (QPSK
    /// `((1-2bI)+j(1-2bQ))/sqrt 2`, the R600/R147 formulas, FFT-shifted bins
    /// 212..511 and 513..812, IFFT with 1/N). Tried and ruled out
    /// (2026-11-16): both index forms of the references, every rounding
    /// order of their phase, f32 phase arithmetic, the +-0.0 signs of the
    /// four exact-zero imaginary parts, flipping the last f32 bit of the 18
    /// reference values nearest a rounding boundary, natural vs shifted
    /// carrier order for data and references, root swap, conjugation, I/Q
    /// swap, inverted QPSK, ten scale factors, and the IFFT scaled by 1, 1/N
    /// or 1/sqrt N. The spec publishes no generator for these stages, so the
    /// serialisation or scaling the digest was made with is unknown. Since a
    /// difference of one f32 rounding anywhere changes a digest, this says
    /// nothing about the waveform being wrong; the receiver tests below
    /// decode it. Kept (ignored) so the gap is visible and checkable.
    #[test]
    #[ignore = "float stages: published digests not reproduced, see doc comment"]
    fn float_stages_match_published_digests() {
        let input = test_input();
        let st = stages(&input);
        let got = digests(&st, &input);
        for ((name, want), got) in DIGESTS.iter().zip(got.iter()).skip(5) {
            assert_eq!(got, want, "stage '{name}' differs");
        }
    }

    /// Our float modulator agrees with the spec's own reference formulas
    /// (written in its `n` form) to f64 precision, and the time-domain
    /// packet has the published extent and cyclic prefixes.
    #[test]
    fn modulator_follows_the_spec_formulas() {
        let st = stages(&test_input());
        let pi = core::f64::consts::PI;
        for m in 0..NCAR {
            let n = if m < 300 { m as f64 } else { m as f64 - 600.0 };
            let bin = 212 + m + (m >= 300) as usize;
            let a600 = pi * n * (n + 1.0) / 601.0;
            let a147 = -pi * 147.0 * n * (n + 1.0) / 601.0;
            let r = st.freq[2 * NFFT + bin];
            assert!((r.0 - a600.cos()).abs() < 1e-9 && (r.1 - a600.sin()).abs() < 1e-9, "R600 m {m}");
            let r = st.freq[4 * NFFT + bin];
            assert!((r.0 - a147.cos()).abs() < 1e-9 && (r.1 - a147.sin()).abs() < 1e-9, "R147 m {m}");
        }
        for s in 0..8 {
            for b in (0..212).chain([512]).chain(813..NFFT) {
                assert_eq!(st.freq[s * NFFT + b], (0.0, 0.0), "symbol {s} bin {b} must be empty");
            }
        }
        // Each CP equals the end of its symbol.
        let us = Layout::Eight.useful_starts();
        for (s, &u) in us.iter().enumerate() {
            let cp = if s == 0 { u } else { u - us[s - 1] - NFFT };
            assert_eq!(cp, [80, 72, 72, 72, 72, 72, 72, 80][s]);
            for i in 0..cp {
                let (a, b) = (st.time[u - cp + i], st.time[u + NFFT - cp + i]);
                assert!((a.0 - b.0).abs() < 1e-12 && (a.1 - b.1).abs() < 1e-12);
            }
        }
    }
}

#[cfg(test)]
mod receive {
    //! The published O4 waveform through our receiver, as the C5 would see
    //! it: resampled to the decoder's 20 MS/s and, in the coarse cases,
    //! quantised to the 4-bit Path B window.
    use super::*;
    use crate::math::C16;
    use crate::payload::{EncKind, Record};
    use crate::rx::{Burst, Path, Receiver};
    use crate::synth::{Capture, capture, o4_payload};
    use crate::testutil::Rng;

    fn decode(x: &[C16], fs: u32) -> Vec<Burst> {
        let mut rx = Box::new(Receiver::new(fs).unwrap());
        let mut v = vec![];
        rx.decode_all(x, &mut |r| {
            if let Ok(b) = r {
                v.push(b);
            }
        });
        v
    }

    /// The C5's Path B: 4-bit two's-complement I/Q (clip window), shifted
    /// back to full scale.
    fn four_bit(x: &[C16]) -> Vec<C16> {
        let f = |v: i16| ((v as i32 + 2048).div_euclid(4096).clamp(-8, 7) << 12) as i16;
        x.iter().map(|c| C16 { re: f(c.re), im: f(c.im) }).collect()
    }

    #[test]
    fn verification_input_decodes_to_itself() {
        // The spec's own test input (not a valid logical object) through the
        // 8-symbol path: the transport block comes back bit for bit.
        let mut rng = Rng::new(31);
        let p = test_input();
        let cap = Capture { cfo_hz: 120e3, ..Capture::clean(20_000_000) };
        let (x, _) = capture(&p, Layout::Eight, &cap, &mut rng);
        let got = decode(&x, 20_000_000);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].layout, Layout::Eight);
        assert!(got[0].crc24_ok);
        assert_eq!(got[0].block, p);
    }

    #[test]
    fn o4_records_decode_across_snr_and_cfo() {
        let mut rng = Rng::new(32);
        let fs = 20_000_000;
        let kinds = [EncKind::Cryp, EncKind::Infp10, EncKind::Infp11];
        let mut n = 0;
        for (snr, four) in [(30.0, false), (20.0, true), (12.0, false), (12.0, true), (6.0, false)] {
            for cfo in [-310e3, -47e3, 0.0, 15e3 * 7.5, 500e3, 1.25e6] {
                let kind = kinds[n % 3];
                n += 1;
                let key = [0xd0, 0x4e, n as u8, 0x01];
                let p = o4_payload(kind, key, &mut rng);
                let peak = if four { 7.0 * 4096.0 } else { 12000.0 };
                let cap = Capture { snr_db: snr, cfo_hz: cfo, lead: 1500.0 + rng.f64() * 3000.0, peak, ..Capture::clean(fs) };
                let (x, _) = capture(&p, Layout::Eight, &cap, &mut rng);
                let x = if four { four_bit(&x) } else { x };
                let got = decode(&x, fs);
                assert_eq!(got.len(), 1, "{snr} dB {cfo} Hz 4-bit {four}");
                let b = &got[0];
                assert!(b.crc24_ok, "{snr} dB {cfo} Hz 4-bit {four}: snr est {}", b.snr_db);
                assert_eq!(b.block, p);
                assert!((b.cfo_hz - cfo).abs() < 300.0, "cfo {} vs {cfo}", b.cfo_hz);
                if snr >= 20.0 {
                    assert_eq!(b.path, Some(Path::Systematic));
                }
                let Some(Record::Encrypted(e)) = b.parsed.as_ref().map(|p| &p.record) else { panic!("{:?}", b.parsed) };
                assert_eq!((e.kind, e.key_id), (kind, key));
                assert!(e.valid(), "{:?}", e.checks);
            }
        }
    }

    #[test]
    fn o4_low_snr_needs_the_turbo_decoder() {
        let mut rng = Rng::new(33);
        let fs = 20_000_000;
        let mut turbo = 0;
        for _ in 0..4 {
            let p = o4_payload(EncKind::Infp10, [1, 2, 3, 4], &mut rng);
            let cap = Capture { snr_db: 2.0, cfo_hz: 210e3, ..Capture::clean(fs) };
            let (x, _) = capture(&p, Layout::Eight, &cap, &mut rng);
            if let Some(b) = decode(&x, fs).first()
                && b.crc24_ok
            {
                assert_eq!(b.block, p);
                assert_eq!(b.path, Some(Path::Turbo));
                turbo += 1;
            }
        }
        assert!(turbo >= 3, "turbo decoded {turbo}/4 at 2 dB");
    }
}
