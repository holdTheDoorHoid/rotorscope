//! LTE turbo decoding for K = 1408 (3GPP TS 36.212 §5.1.3.2): two 8-state
//! recursive systematic constituent codes (feedback 1 + D^2 + D^3, parity
//! 1 + D + D^3), each terminated with three tail bits, joined by the QPP
//! interleaver `pi(i) = (f1 i + f2 i^2) mod K` with f1 = 43, f2 = 88
//! (36.212 Table 5.1.3-3, row K = 1408).
//!
//! The decoder is fixed-point max-log-MAP (BCJR with `max` in place of
//! log-sum-exp) with the usual 0.75 extrinsic scaling, which recovers most of
//! the max-log loss. State metrics are `i32` in the arithmetic and stored as
//! `i16`; the input clamps below bound their spread well inside 16 bits.
//! It checks CRC-24A after every half-iteration and stops as soon as it
//! passes.
//!
//! Soft values follow one convention everywhere in this crate: positive
//! means bit 0.
//!
//! Memory: forward metrics 1412 x 8 x 2 bytes plus five K-length `i16`
//! arrays, about 37 KiB in all, held in [`Turbo`] so the caller decides where
//! it lives (a `static` on a microcontroller).

use crate::crc::crc24a;
use crate::{D, K};

pub const F1: usize = 43;
pub const F2: usize = 88;

/// The QPP interleaver, computed at compile time.
pub static QPP: [u16; K] = {
    let mut t = [0u16; K];
    let mut i = 0;
    while i < K {
        t[i] = ((F1 * i + F2 * ((i * i) % K)) % K) as u16;
        i += 1;
    }
    t
};

const STATES: usize = 8;
const NEG: i32 = -(1 << 14);
/// Clamp for channel soft values entering the trellis.
const CH_CLAMP: i32 = 1 << 10;
/// Clamp for extrinsic values passed between decoders.
const EXT_CLAMP: i32 = 1 << 12;

/// One trellis branch: next state and the parity bit, for input 0 and 1.
#[derive(Clone, Copy)]
struct Branch {
    next: [u8; 2],
    parity: [u8; 2],
}

/// State = s1 s2 s3 (s1 most recent) packed as `s1 << 2 | s2 << 1 | s3`.
const TRELLIS: [Branch; STATES] = {
    let mut t = [Branch { next: [0; 2], parity: [0; 2] }; STATES];
    let mut s = 0;
    while s < STATES {
        let (s1, s2, s3) = ((s >> 2) & 1, (s >> 1) & 1, s & 1);
        let mut u = 0;
        while u < 2 {
            let a = u ^ s2 ^ s3;
            t[s].parity[u] = (a ^ s1 ^ s3) as u8;
            t[s].next[u] = ((a << 2) | (s1 << 1) | s2) as u8;
            u += 1;
        }
        s += 1;
    }
    t
};

/// Workspace for the turbo decoder.
pub struct Turbo {
    alpha: [[i16; STATES]; K + 4],
    /// A priori input to decoder 1 (natural order).
    la1: [i16; K],
    /// Extrinsic output of decoder 1 (natural order).
    le1: [i16; K],
    /// Interleaved systematic soft values.
    sys2: [i16; K],
    /// Interleaved a priori / extrinsic for decoder 2.
    la2: [i16; K],
    le2: [i16; K],
}

/// What [`Turbo::decode`] reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TurboResult {
    /// Half-iterations run (2 per full iteration).
    pub half_iterations: u8,
    pub crc_ok: bool,
}

impl Default for Turbo {
    fn default() -> Self {
        Self::new()
    }
}

impl Turbo {
    pub const fn new() -> Self {
        Self {
            alpha: [[0; STATES]; K + 4],
            la1: [0; K],
            le1: [0; K],
            sys2: [0; K],
            la2: [0; K],
            le2: [0; K],
        }
    }

    /// Decode the three de-rate-matched streams (`d[0]` systematic, `d[1]`,
    /// `d[2]` parity, each with its four tail positions) into `out` (176
    /// bytes, MSB first, CRC-24A included), running at most `max_iter` full
    /// iterations.
    #[inline(always)]
    pub fn decode(&mut self, d: &[[i16; D]; 3], max_iter: u8, out: &mut [u8; K / 8]) -> TurboResult {
        let sys = &d[0];
        let p1 = &d[1];
        let p2 = &d[2];
        // Tail soft values (36.212 §5.1.3.2.2 ordering):
        //   d0: x_K  z_K+1  x'_K  z'_K+1
        //   d1: z_K  x_K+2  z'_K  x'_K+2
        //   d2: x_K+1 z_K+2 x'_K+1 z'_K+2
        let tail1_sys = [sys[K], d[2][K], d[1][K + 1]];
        let tail1_par = [p1[K], sys[K + 1], d[2][K + 1]];
        let tail2_sys = [sys[K + 2], d[2][K + 2], d[1][K + 3]];
        let tail2_par = [p1[K + 2], sys[K + 3], d[2][K + 3]];

        for i in 0..K {
            self.sys2[i] = sys[QPP[i] as usize];
        }
        self.la1.fill(0);
        let mut half = 0u8;
        for _ in 0..max_iter {
            // Decoder 1, natural order.
            siso(&mut self.alpha, &sys[..K], &p1[..K], &self.la1, &tail1_sys, &tail1_par, &mut self.le1);
            half += 1;
            if check(&sys[..K], &self.la1, &self.le1, None, out) {
                return TurboResult { half_iterations: half, crc_ok: true };
            }
            for (la2, &p) in self.la2.iter_mut().zip(QPP.iter()) {
                *la2 = self.le1[p as usize];
            }
            // Decoder 2, interleaved order.
            siso(&mut self.alpha, &self.sys2, &p2[..K], &self.la2, &tail2_sys, &tail2_par, &mut self.le2);
            half += 1;
            for (&le2, &p) in self.le2.iter().zip(QPP.iter()) {
                self.la1[p as usize] = le2;
            }
            if check(&self.sys2, &self.la2, &self.le2, Some(&QPP), out) {
                return TurboResult { half_iterations: half, crc_ok: true };
            }
        }
        TurboResult { half_iterations: half, crc_ok: false }
    }
}

/// Hard decisions from `sys + la + le` (in the decoder's own order; `perm`
/// maps back to natural order), packed into `out`; then CRC-24A.
fn check(sys: &[i16], la: &[i16], le: &[i16], perm: Option<&[u16; K]>, out: &mut [u8; K / 8]) -> bool {
    out.fill(0);
    for i in 0..K {
        let l = sys[i] as i32 + la[i] as i32 + le[i] as i32;
        if l < 0 {
            let n = perm.map_or(i, |p| p[i] as usize);
            out[n / 8] |= 0x80 >> (n % 8);
        }
    }
    crc_ok(out)
}

/// CRC-24A over a full 176-byte block, rejecting the all-zero block (which
/// trivially passes and is what an empty input decodes to).
pub fn crc_ok(block: &[u8; K / 8]) -> bool {
    block.iter().any(|&b| b != 0) && crc24a(block) == 0
}

#[inline]
fn clamp(v: i32, c: i32) -> i32 {
    v.clamp(-c, c)
}

/// One max-log-MAP pass over a terminated constituent code. Writes the
/// scaled extrinsic information into `le`.
fn siso(
    alpha: &mut [[i16; STATES]; K + 4],
    sys: &[i16],
    par: &[i16],
    la: &[i16],
    tail_sys: &[i16; 3],
    tail_par: &[i16; 3],
    le: &mut [i16; K],
) {
    // Branch metric for input u, parity z: (u == 0) * (Ls + La) + (z == 0) * Lp.
    let gamma = |k: usize, u: usize, z: u8| -> i32 {
        let (ls, lp, la_k) = if k < K {
            (clamp(sys[k] as i32, CH_CLAMP), clamp(par[k] as i32, CH_CLAMP), la[k] as i32)
        } else {
            (clamp(tail_sys[k - K] as i32, CH_CLAMP), clamp(tail_par[k - K] as i32, CH_CLAMP), 0)
        };
        (if u == 0 { ls + la_k } else { 0 }) + (if z == 0 { lp } else { 0 })
    };

    // Forward recursion over K data steps and 3 tail steps.
    alpha[0] = [0, NEG as i16, NEG as i16, NEG as i16, NEG as i16, NEG as i16, NEG as i16, NEG as i16];
    for k in 0..K + 3 {
        let mut next = [NEG * 2; STATES];
        let a = alpha[k];
        for (s, br) in TRELLIS.iter().enumerate() {
            for u in 0..2 {
                // Tail steps force the input that drives the register to zero.
                if k >= K && u != tail_input(s) {
                    continue;
                }
                let m = a[s] as i32 + gamma(k, u, br.parity[u]);
                let n = br.next[u] as usize;
                if m > next[n] {
                    next[n] = m;
                }
            }
        }
        let top = *next.iter().max().unwrap();
        for (dst, &v) in alpha[k + 1].iter_mut().zip(&next) {
            *dst = (v - top).max(NEG) as i16;
        }
    }

    // Backward recursion with the soft output.
    let mut beta = [NEG; STATES];
    beta[0] = 0;
    for k in (K..K + 3).rev() {
        let mut prev = [NEG * 2; STATES];
        for (s, br) in TRELLIS.iter().enumerate() {
            let u = tail_input(s);
            let m = gamma(k, u, br.parity[u]) + beta[br.next[u] as usize];
            if m > prev[s] {
                prev[s] = m;
            }
        }
        let top = *prev.iter().max().unwrap();
        for (b, p) in beta.iter_mut().zip(prev) {
            *b = (p - top).max(NEG);
        }
    }
    for k in (0..K).rev() {
        let a = alpha[k];
        let mut prev = [NEG * 2; STATES];
        let mut best = [NEG * 4; 2];
        for (s, br) in TRELLIS.iter().enumerate() {
            for u in 0..2 {
                let g = gamma(k, u, br.parity[u]);
                let b = beta[br.next[u] as usize];
                let m = g + b;
                if m > prev[s] {
                    prev[s] = m;
                }
                let full = a[s] as i32 + m;
                if full > best[u] {
                    best[u] = full;
                }
            }
        }
        let top = *prev.iter().max().unwrap();
        for (b, p) in beta.iter_mut().zip(prev) {
            *b = (p - top).max(NEG);
        }
        let lam = best[0] - best[1];
        let ext = lam - clamp(sys[k] as i32, CH_CLAMP) - la[k] as i32;
        le[k] = clamp((ext * 3) >> 2, EXT_CLAMP) as i16;
    }
}

/// The input that drives state `s` towards zero (trellis termination): the
/// feedback bit, so that the register shifts in a zero.
#[inline]
fn tail_input(s: usize) -> usize {
    ((s >> 1) ^ s) & 1
}

/// Test-only LTE turbo encoder: `bits` (K, MSB-first packing not assumed:
/// one bit per byte) to the three streams d0, d1, d2 with tails.
#[cfg(test)]
pub fn encode(bits: &[u8]) -> [[u8; D]; 3] {
    fn rsc(input: impl Iterator<Item = u8>, x: &mut [u8], z: &mut [u8]) -> [(u8, u8); 3] {
        let mut s = 0usize;
        for (k, u) in input.enumerate() {
            let br = TRELLIS[s];
            x[k] = u;
            z[k] = br.parity[u as usize];
            s = br.next[u as usize] as usize;
        }
        let mut tail = [(0, 0); 3];
        for t in tail.iter_mut() {
            let u = tail_input(s);
            let br = TRELLIS[s];
            *t = (u as u8, br.parity[u]);
            s = br.next[u] as usize;
        }
        assert_eq!(s, 0);
        tail
    }
    assert_eq!(bits.len(), K);
    let mut x = [0u8; K];
    let mut z = [0u8; K];
    let mut x2 = [0u8; K];
    let mut z2 = [0u8; K];
    let t1 = rsc(bits.iter().copied(), &mut x, &mut z);
    let t2 = rsc((0..K).map(|i| bits[QPP[i] as usize]), &mut x2, &mut z2);
    let mut d = [[0u8; D]; 3];
    d[0][..K].copy_from_slice(&x);
    d[1][..K].copy_from_slice(&z);
    d[2][..K].copy_from_slice(&z2);
    // (x_K, z_K), (x_K+1, z_K+1), (x_K+2, z_K+2) and the primed ones.
    let [(xk, zk), (xk1, zk1), (xk2, zk2)] = t1;
    let [(yk, wk), (yk1, wk1), (yk2, wk2)] = t2;
    d[0][K..].copy_from_slice(&[xk, zk1, yk, wk1]);
    d[1][K..].copy_from_slice(&[zk, xk2, wk, yk2]);
    d[2][K..].copy_from_slice(&[xk1, zk2, yk1, wk2]);
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ratematch::{derate, rate_match};
    use crate::testutil::Rng;

    #[test]
    fn qpp_is_a_permutation_with_table_parameters() {
        let mut seen = [false; K];
        for &p in QPP.iter() {
            assert!(!seen[p as usize]);
            seen[p as usize] = true;
        }
        // 36.212 Table 5.1.3-3 conditions for a valid QPP: f1 coprime to K,
        // every prime factor of K divides f2. K = 1408 = 2^7 * 11.
        fn gcd(a: usize, b: usize) -> usize {
            if b == 0 { a } else { gcd(b, a % b) }
        }
        assert_eq!(gcd(F1, K), 1);
        assert_eq!(F2 % 2, 0);
        assert_eq!(F2 % 11, 0);
        // The published O4 spec uses the same pair.
        assert_eq!((QPP[0], QPP[1], QPP[2]), (0, (F1 + F2) as u16, ((2 * F1 + 4 * F2) % K) as u16));
    }

    #[test]
    fn encoder_terminates_and_noiseless_decode_is_exact() {
        let mut rng = Rng::new(7);
        let mut bytes = [0u8; K / 8];
        for b in bytes[..173].iter_mut() {
            *b = rng.u8();
        }
        let c = crc24a(&bytes[..173]);
        bytes[173..].copy_from_slice(&[(c >> 16) as u8, (c >> 8) as u8, c as u8]);
        let bits: Vec<u8> = (0..K).map(|i| (bytes[i / 8] >> (7 - i % 8)) & 1).collect();
        let d = encode(&bits);
        let e = rate_match(&d);
        let llr: Vec<i8> = e.iter().map(|&b| if b == 0 { 20 } else { -20 }).collect();
        let mut soft = [[0i16; D]; 3];
        derate(&llr, &mut soft);
        let mut t = Box::new(Turbo::new());
        let mut out = [0u8; K / 8];
        let r = t.decode(&soft, 8, &mut out);
        assert!(r.crc_ok);
        assert_eq!(r.half_iterations, 1);
        assert_eq!(out, bytes);
    }

    /// Bit error rate of the coded chain (turbo + rate matching to 7200)
    /// over BPSK/AWGN at a given Eb/N0, fixed iterations, seeded noise.
    fn ber(ebn0_db: f64, blocks: usize, seed: u64) -> (usize, usize, usize) {
        let mut rng = Rng::new(seed);
        let rate = K as f64 / crate::E as f64;
        let ecn0 = 10f64.powf(ebn0_db / 10.0) * rate;
        let sigma = (1.0 / (2.0 * ecn0)).sqrt();
        let mut t = Box::new(Turbo::new());
        let (mut errs, mut sys_errs, mut bad_blocks) = (0, 0, 0);
        for _ in 0..blocks {
            let mut bytes = [0u8; K / 8];
            for b in bytes[..173].iter_mut() {
                *b = rng.u8();
            }
            let c = crc24a(&bytes[..173]);
            bytes[173..].copy_from_slice(&[(c >> 16) as u8, (c >> 8) as u8, c as u8]);
            let bits: Vec<u8> = (0..K).map(|i| (bytes[i / 8] >> (7 - i % 8)) & 1).collect();
            let e = rate_match(&encode(&bits));
            // LLR = 2y/sigma^2, scaled so the mean |LLR| sits near 24.
            let scale = 24.0 / (2.0 / (sigma * sigma));
            let llr: Vec<i8> = e
                .iter()
                .map(|&b| {
                    let y = (if b == 0 { 1.0 } else { -1.0 }) + sigma * rng.gauss();
                    (2.0 * y / (sigma * sigma) * scale).round().clamp(-127.0, 127.0) as i8
                })
                .collect();
            let mut soft = [[0i16; D]; 3];
            derate(&llr, &mut soft);
            let mut out = [0u8; K / 8];
            for i in 0..K {
                if (soft[0][i] < 0) != (bits[i] == 1) {
                    sys_errs += 1;
                }
            }
            let r = t.decode(&soft, 8, &mut out);
            let e = (0..K).filter(|&i| ((out[i / 8] >> (7 - i % 8)) & 1) != bits[i]).count();
            errs += e;
            if !r.crc_ok {
                bad_blocks += 1;
            }
        }
        (errs, sys_errs, bad_blocks)
    }

    #[test]
    fn ber_waterfall() {
        // Measured (20 blocks, 8 iterations): -1 dB all blocks fail, 0 dB 19
        // of 20 fail, 0.5 dB 2 of 20 fail, 1 dB and up error-free. The
        // systematic bits alone are still ~16% wrong at 1 dB.
        let n = 20;
        let total = n * K;
        let (e, sys, bad) = ber(1.0, n, 1);
        assert_eq!((e, bad), (0, 0), "1 dB must be error-free");
        assert!(sys * 10 > total, "1 dB: systematic-only BER suspiciously low");
        // In the waterfall most blocks already decode.
        let (_, _, bad) = ber(0.5, n, 2);
        assert!(bad * 4 <= n, "0.5 dB: {bad} of {n} blocks failed");
        // Well below threshold it fails, as it must.
        let (e, _, bad) = ber(-1.0, n, 3);
        assert!(bad == n && e * 10 > total, "-1 dB should not decode");
    }
}
