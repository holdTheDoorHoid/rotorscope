//! Reed-Solomon over GF(256) as UAT uses it (DO-282B; parameters as in
//! dump978's `fec.c`): field polynomial x^8+x^7+x^2+x+1 (0x187), primitive
//! element alpha = x (2), generator roots alpha^120 .. alpha^(120+nroots-1).
//! The codes are RS(255, 255-nroots) shortened to the frame length; the
//! first byte of a frame is the highest-degree coefficient and the parity
//! bytes come last.
//!
//! The decoder is written fresh (Berlekamp-Massey, Chien search, Forney),
//! not ported. It is stricter than Phil Karn's `decode_rs_char` that
//! dump978 uses in one way: an error located in the shortened (padding)
//! part of the code cannot be real, so it rejects the frame instead of
//! ignoring that error. It also re-checks the syndromes after correcting.

/// Field polynomial (including the x^8 term).
pub const GF_POLY: u16 = 0x187;
/// First consecutive root of the generator, as a power of alpha.
pub const FCR: usize = 120;
/// Most parity bytes any UAT code uses (the ground uplink's RS(92,72)).
pub const MAX_ROOTS: usize = 20;

/// alpha^i for i in 0..510 (doubled so products need no reduction).
static EXP: [u8; 512] = make_exp();
/// log_alpha(x) for x in 1..=255 (LOG[0] unused).
static LOG: [u8; 256] = make_log();

const fn make_exp() -> [u8; 512] {
    let mut t = [0u8; 512];
    let mut x: u16 = 1;
    let mut i = 0;
    while i < 255 {
        t[i] = x as u8;
        t[i + 255] = x as u8;
        x <<= 1;
        if x & 0x100 != 0 {
            x ^= GF_POLY;
        }
        i += 1;
    }
    t[510] = t[0];
    t[511] = t[1];
    t
}

const fn make_log() -> [u8; 256] {
    let exp = make_exp();
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 255 {
        t[exp[i] as usize] = i as u8;
        i += 1;
    }
    t
}

/// Product in GF(256).
#[inline]
pub fn mul(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        0
    } else {
        EXP[usize::from(LOG[usize::from(a)]) + usize::from(LOG[usize::from(b)])]
    }
}

/// Quotient in GF(256); `b` must not be 0.
#[inline]
fn div(a: u8, b: u8) -> u8 {
    if a == 0 {
        0
    } else {
        EXP[usize::from(LOG[usize::from(a)]) + 255 - usize::from(LOG[usize::from(b)])]
    }
}

/// alpha^e for any e (reduced mod 255).
#[inline]
pub fn pow_alpha(e: usize) -> u8 {
    EXP[e % 255]
}

/// A shortened Reed-Solomon code: `n` bytes, of which the last `nroots`
/// are parity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Code {
    pub n: usize,
    pub nroots: usize,
}

/// UAT basic ADS-B: 18 data + 12 parity bytes, corrects up to 6.
pub const BASIC: Code = Code { n: 30, nroots: 12 };
/// UAT long ADS-B: 34 data + 14 parity bytes, corrects up to 7.
pub const LONG: Code = Code { n: 48, nroots: 14 };

impl Code {
    /// Data bytes.
    pub const fn k(&self) -> usize {
        self.n - self.nroots
    }

    /// Most byte errors the code can correct.
    pub const fn t(&self) -> usize {
        self.nroots / 2
    }

    /// The syndromes S_i = r(alpha^(FCR+i)), first byte highest degree.
    fn syndromes(&self, r: &[u8], s: &mut [u8; MAX_ROOTS]) -> bool {
        let mut any = false;
        for (i, si) in s.iter_mut().enumerate().take(self.nroots) {
            let root = pow_alpha(FCR + i);
            let mut acc = 0u8;
            for &b in &r[..self.n] {
                acc = mul(acc, root) ^ b;
            }
            *si = acc;
            any |= acc != 0;
        }
        any
    }

    /// Correct `word` (exactly `n` bytes) in place. Returns the number of
    /// bytes corrected, or `None` if the errors are beyond what the code
    /// can correct (or are detectably inconsistent); then `word` is left
    /// untouched.
    pub fn correct(&self, word: &mut [u8]) -> Option<usize> {
        if word.len() != self.n || self.nroots > MAX_ROOTS || self.n > 255 {
            return None;
        }
        let mut s = [0u8; MAX_ROOTS];
        if !self.syndromes(word, &mut s) {
            return Some(0);
        }
        let nr = self.nroots;

        // Berlekamp-Massey: the error locator Lambda(x), ascending powers.
        let mut lambda = [0u8; MAX_ROOTS + 1];
        let mut prev = [0u8; MAX_ROOTS + 1];
        lambda[0] = 1;
        prev[0] = 1;
        let mut l = 0usize;
        let mut shift = 1usize;
        let mut prev_d = 1u8;
        for r in 0..nr {
            let mut d = s[r];
            for i in 1..=l {
                d ^= mul(lambda[i], s[r - i]);
            }
            if d == 0 {
                shift += 1;
                continue;
            }
            let coef = div(d, prev_d);
            let saved = lambda;
            for i in 0..(MAX_ROOTS + 1).saturating_sub(shift) {
                lambda[i + shift] ^= mul(coef, prev[i]);
            }
            if 2 * l <= r {
                l = r + 1 - l;
                prev = saved;
                prev_d = d;
                shift = 1;
            } else {
                shift += 1;
            }
        }
        let deg = (0..=MAX_ROOTS).rev().find(|&i| lambda[i] != 0).unwrap_or(0);
        if deg != l || l == 0 || l > self.t() {
            return None;
        }

        // Omega(x) = S(x) Lambda(x) mod x^nroots.
        let mut omega = [0u8; MAX_ROOTS];
        for (i, o) in omega.iter_mut().enumerate().take(nr) {
            let mut acc = 0u8;
            for j in 0..=i.min(deg) {
                acc ^= mul(lambda[j], s[i - j]);
            }
            *o = acc;
        }

        // Chien search over the real (unshortened-away) positions only, and
        // Forney for each root. Position j holds the coefficient of degree
        // p = n-1-j; its locator is X = alpha^p.
        let mut fixed = [0u8; 255];
        fixed[..self.n].copy_from_slice(word);
        let mut found = 0usize;
        // X^(1-FCR) = alpha^(p * (1-FCR) mod 255).
        let one_minus_fcr = (1 + 255 - FCR % 255) % 255;
        for (j, byte) in fixed.iter_mut().enumerate().take(self.n) {
            let p = self.n - 1 - j;
            let xinv_log = (255 - p % 255) % 255;
            // Lambda(X^-1)
            let mut v = 0u8;
            for (i, &c) in lambda.iter().enumerate().take(deg + 1) {
                v ^= mul(c, pow_alpha(xinv_log * i));
            }
            if v != 0 {
                continue;
            }
            found += 1;
            let mut num = 0u8;
            for (i, &c) in omega.iter().enumerate().take(nr) {
                num ^= mul(c, pow_alpha(xinv_log * i));
            }
            // Lambda'(x) in characteristic 2: odd terms only.
            let mut den = 0u8;
            let mut i = 1;
            while i <= deg {
                den ^= mul(lambda[i], pow_alpha(xinv_log * (i - 1)));
                i += 2;
            }
            if den == 0 {
                return None;
            }
            let e = mul(pow_alpha(p * one_minus_fcr), div(num, den));
            if e == 0 {
                return None;
            }
            *byte ^= e;
        }
        // Every root must be a real byte position: a root in the padding (or
        // a locator that does not split) means more errors than we can fix.
        if found != deg {
            return None;
        }
        if self.syndromes(&fixed, &mut s) {
            return None;
        }
        word.copy_from_slice(&fixed[..self.n]);
        Some(found)
    }
}
