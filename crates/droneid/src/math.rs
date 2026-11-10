//! Integer complex types, a const-built unit-circle table, an NCO, CORDIC
//! `atan2` and the few scalar helpers the receiver needs.
//!
//! Angles are *binary angles*: a `u32` where 2^32 is one full turn, so phase
//! accumulators wrap for free and need no floating point. Everything here
//! that runs per sample is integer-only; the `f64` functions are `const fn`
//! and run at compile time (tables) or once per configuration (filter
//! design), where soft-float on a chip without an FPU is affordable.

/// A complex sample with 16-bit parts. The input, resampled and FFT data use it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct C16 {
    pub re: i16,
    pub im: i16,
}

/// A complex value with 32-bit parts (products, channel estimates).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct C32 {
    pub re: i32,
    pub im: i32,
}

/// A complex accumulator with 64-bit parts (long correlation sums).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct C64 {
    pub re: i64,
    pub im: i64,
}

impl C16 {
    pub const fn new(re: i16, im: i16) -> Self {
        Self { re, im }
    }
    pub fn to32(self) -> C32 {
        C32 { re: self.re as i32, im: self.im as i32 }
    }
    /// `|z|^2`, exact.
    pub fn norm2(self) -> u32 {
        let (r, i) = (self.re as i32, self.im as i32);
        (r * r) as u32 + (i * i) as u32
    }
    /// `self * conj(other)`, exact in 32 bits... except for (-32768)^2 * 2,
    /// which callers never produce, so the sum wraps rather than panics.
    pub fn mul_conj(self, o: C16) -> C32 {
        let (a, b, c, d) = (self.re as i32, self.im as i32, o.re as i32, o.im as i32);
        C32 { re: (a * c).wrapping_add(b * d), im: (b * c).wrapping_sub(a * d) }
    }
    /// `self * w` where `w` is Q15, rounded back to 16 bits (saturating).
    pub fn mul_q15(self, w: C16) -> C16 {
        let (a, b, c, d) = (self.re as i32, self.im as i32, w.re as i32, w.im as i32);
        let re = (a * c - b * d + (1 << 14)) >> 15;
        let im = (a * d + b * c + (1 << 14)) >> 15;
        C16 { re: sat16(re), im: sat16(im) }
    }
}

/// Subtract the mean of `x` from every sample (saturating) and return the
/// mean that was removed.
///
/// Raw captures that truncate (the ESP32-C5's bit-sliced debug bus and its
/// dump engine both floor) carry a half-LSB bias plus the receiver's own DC
/// (LO leakage). The decoder already removes DC per burst span; doing it
/// once over the whole capture as well keeps the energy detector's windows
/// honest and makes dumped captures look clean.
pub fn remove_dc(x: &mut [C16]) -> C16 {
    if x.is_empty() {
        return C16::default();
    }
    let (mut sr, mut si) = (0i64, 0i64);
    for v in x.iter() {
        sr += v.re as i64;
        si += v.im as i64;
    }
    let n = x.len() as i64;
    // Round half away from zero.
    let mr = (sr + sr.signum() * (n / 2)) / n;
    let mi = (si + si.signum() * (n / 2)) / n;
    for v in x.iter_mut() {
        v.re = sat16(v.re as i32 - mr as i32);
        v.im = sat16(v.im as i32 - mi as i32);
    }
    C16 { re: mr as i16, im: mi as i16 }
}

impl C32 {
    pub const ZERO: C32 = C32 { re: 0, im: 0 };
    /// Wrapping sum.
    pub fn wadd(self, o: C32) -> C32 {
        C32 { re: self.re.wrapping_add(o.re), im: self.im.wrapping_add(o.im) }
    }
    pub fn wsub(self, o: C32) -> C32 {
        C32 { re: self.re.wrapping_sub(o.re), im: self.im.wrapping_sub(o.im) }
    }
    /// `self * w` where `w` is a Q15 unit-circle value; 64-bit intermediate.
    pub fn rot_q15(self, w: C16) -> C32 {
        let (a, b, c, d) = (self.re as i64, self.im as i64, w.re as i64, w.im as i64);
        C32 { re: ((a * c - b * d + (1 << 14)) >> 15) as i32, im: ((a * d + b * c + (1 << 14)) >> 15) as i32 }
    }
    /// `self * conj(o)` in 64 bits.
    pub fn mul_conj64(self, o: C32) -> C64 {
        let (a, b, c, d) = (self.re as i64, self.im as i64, o.re as i64, o.im as i64);
        C64 { re: a * c + b * d, im: b * c - a * d }
    }
    pub fn norm2(self) -> u64 {
        let (r, i) = (self.re as i64, self.im as i64);
        (r * r + i * i) as u64
    }
    /// Arithmetic shift of both parts; negative `s` shifts left.
    pub fn shift(self, s: i32) -> C32 {
        if s >= 0 {
            let s = s.min(31) as u32;
            C32 { re: self.re >> s, im: self.im >> s }
        } else {
            let s = (-s).min(31) as u32;
            C32 { re: self.re << s, im: self.im << s }
        }
    }
}

impl C64 {
    pub const ZERO: C64 = C64 { re: 0, im: 0 };
    pub fn wadd(self, o: C64) -> C64 {
        C64 { re: self.re.wrapping_add(o.re), im: self.im.wrapping_add(o.im) }
    }
    pub fn wsub(self, o: C64) -> C64 {
        C64 { re: self.re.wrapping_sub(o.re), im: self.im.wrapping_sub(o.im) }
    }
    /// Approximate magnitude (exact `isqrt` of the squared magnitude, after
    /// scaling into range).
    pub fn abs(self) -> u64 {
        let (mut r, mut i) = (self.re.unsigned_abs(), self.im.unsigned_abs());
        let mut sh = 0;
        while r > (1 << 31) || i > (1 << 31) {
            r >>= 1;
            i >>= 1;
            sh += 1;
        }
        isqrt64(r * r + i * i) << sh
    }
    /// Phase as a binary angle.
    pub fn angle(self) -> u32 {
        atan2_64(self.im, self.re)
    }
}

pub fn sat16(v: i32) -> i16 {
    v.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

/// Integer square root of a `u64`.
pub fn isqrt64(v: u64) -> u64 {
    if v < 2 {
        return v;
    }
    // Newton from a power-of-two upper bound; converges in a handful of steps.
    let mut x = 1u64 << (64 - v.leading_zeros()).div_ceil(2);
    loop {
        let y = (x + v / x) / 2;
        if y >= x {
            return x;
        }
        x = y;
    }
}

// ---------------------------------------------------------------------------
// Compile-time trigonometry (f64, const fn).

pub const PI: f64 = core::f64::consts::PI;

/// `(sin x, cos x)` by range reduction and Taylor series; accurate to ~1e-15
/// on the reduced range. Usable in `const` contexts and at runtime.
pub const fn sin_cos(x: f64) -> (f64, f64) {
    // Reduce to [-pi, pi].
    let two_pi = 2.0 * PI;
    let k = x / two_pi;
    let kr = if k >= 0.0 { (k + 0.5) as i64 } else { (k - 0.5) as i64 };
    let mut r = x - kr as f64 * two_pi;
    // Fold to [-pi/2, pi/2]: sin(pi - r) = sin r, cos(pi - r) = -cos r.
    let mut cos_sign = 1.0;
    if r > PI / 2.0 {
        r = PI - r;
        cos_sign = -1.0;
    } else if r < -PI / 2.0 {
        r = -PI - r;
        cos_sign = -1.0;
    }
    let r2 = r * r;
    let mut s = r;
    let mut c = 1.0;
    let mut ts = r;
    let mut tc = 1.0;
    let mut n = 1;
    while n < 14 {
        ts = -ts * r2 / ((2 * n) as f64 * (2 * n + 1) as f64);
        tc = -tc * r2 / ((2 * n - 1) as f64 * (2 * n) as f64);
        s += ts;
        c += tc;
        n += 1;
    }
    (s, cos_sign * c)
}

pub const fn round_i32(v: f64) -> i32 {
    if v >= 0.0 { (v + 0.5) as i32 } else { (v - 0.5) as i32 }
}

const fn q15(v: f64) -> i16 {
    let r = round_i32(v * 32768.0);
    if r > 32767 {
        32767
    } else if r < -32768 {
        -32768
    } else {
        r as i16
    }
}

/// Number of entries in [`UNIT`].
pub const UNIT_N: usize = 1024;

/// `UNIT[k] = exp(j 2 pi k / 1024)` in Q15. Serves as FFT twiddles, NCO and
/// rotation lookup (4 KiB).
pub static UNIT: [C16; UNIT_N] = {
    let mut t = [C16 { re: 0, im: 0 }; UNIT_N];
    let mut k = 0;
    while k < UNIT_N {
        let (s, c) = sin_cos(2.0 * PI * k as f64 / UNIT_N as f64);
        t[k] = C16 { re: q15(c), im: q15(s) };
        k += 1;
    }
    t
};

/// `exp(j * angle)` for a binary angle, nearest table entry (0.35 degree
/// resolution, about -50 dB phase noise; ample for QPSK).
#[inline]
pub fn cis(angle: u32) -> C16 {
    UNIT[(angle.wrapping_add(1 << 21) >> 22) as usize]
}

/// Numerically controlled oscillator: a phase accumulator and table lookup.
#[derive(Clone, Copy, Debug, Default)]
pub struct Nco {
    pub phase: u32,
    pub step: u32,
}

impl Nco {
    /// An oscillator at `freq_hz` for sample rate `fs_hz` (negative freq is a
    /// clockwise rotation). Computed once, in f64.
    pub fn new(freq_hz: f64, fs_hz: f64) -> Self {
        Self { phase: 0, step: hz_to_step(freq_hz, fs_hz) }
    }
    #[inline]
    pub fn tick(&mut self) -> C16 {
        let v = cis(self.phase);
        self.phase = self.phase.wrapping_add(self.step);
        v
    }
}

/// Binary-angle phase step per sample for a frequency.
pub fn hz_to_step(freq_hz: f64, fs_hz: f64) -> u32 {
    let turns = freq_hz / fs_hz; // cycles per sample
    let frac = turns - floor(turns);
    (frac * 4294967296.0) as u64 as u32
}

/// Binary angle to radians in (-pi, pi].
pub fn angle_rad(a: u32) -> f64 {
    a as i32 as f64 * (PI / 2147483648.0)
}

pub const fn floor(v: f64) -> f64 {
    let t = v as i64 as f64;
    if t > v { t - 1.0 } else { t }
}

// ---------------------------------------------------------------------------
// CORDIC atan2 (integer).

const CORDIC_N: usize = 24;

const fn atan_series(x: f64) -> f64 {
    // x <= 1/2 here; 40 terms is plenty.
    let x2 = x * x;
    let mut term = x;
    let mut sum = x;
    let mut n = 1;
    while n < 40 {
        term = -term * x2;
        sum += term / (2 * n + 1) as f64;
        n += 1;
    }
    sum
}

/// `atan(2^-i)` as binary angles.
static CORDIC_ATAN: [u32; CORDIC_N] = {
    let mut t = [0u32; CORDIC_N];
    let mut i = 0;
    while i < CORDIC_N {
        let a = if i == 0 { PI / 4.0 } else { atan_series(1.0 / (1u64 << i) as f64) };
        t[i] = (a / (2.0 * PI) * 4294967296.0 + 0.5) as u64 as u32;
        i += 1;
    }
    t
};

/// `atan2(y, x)` as a binary angle, by 24 CORDIC iterations. Accuracy is a
/// few units in 2^-24 of a turn for inputs that are not tiny.
pub fn atan2_64(y: i64, x: i64) -> u32 {
    if x == 0 && y == 0 {
        return 0;
    }
    // Scale into a range where the CORDIC gain (1.65) cannot overflow i64
    // and enough bits remain for precision.
    let (mut x, mut y) = (x, y);
    while x.unsigned_abs() > (1 << 40) || y.unsigned_abs() > (1 << 40) {
        x >>= 1;
        y >>= 1;
    }
    while x.unsigned_abs() < (1 << 30) && y.unsigned_abs() < (1 << 30) {
        x <<= 1;
        y <<= 1;
    }
    // Rotate into the right half-plane.
    let mut z: u32 = 0;
    if x < 0 {
        // Add or subtract a half turn.
        x = -x;
        y = -y;
        z = 0x8000_0000;
    }
    for (i, &a) in CORDIC_ATAN.iter().enumerate() {
        let (xs, ys) = (x >> i, y >> i);
        if y > 0 {
            x += ys;
            y -= xs;
            z = z.wrapping_add(a);
        } else {
            x -= ys;
            y += xs;
            z = z.wrapping_sub(a);
        }
    }
    z
}

/// Ten times the base-10 log of a positive ratio, in f32. Once-per-burst use
/// (SNR reporting); no `libm` needed.
pub fn db10(ratio: f64) -> f32 {
    if ratio <= 0.0 {
        return -200.0;
    }
    // log2 via exponent extraction, then a series for the mantissa.
    let bits = ratio.to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i64 - 1023;
    let m = f64::from_bits((bits & !(0x7ff << 52)) | (1023 << 52)); // [1, 2)
    // ln(m) = 2 atanh((m-1)/(m+1))
    let t = (m - 1.0) / (m + 1.0);
    let t2 = t * t;
    let mut term = t;
    let mut s = 0.0;
    let mut n = 0;
    while n < 12 {
        s += term / (2 * n + 1) as f64;
        term *= t2;
        n += 1;
    }
    let ln = 2.0 * s + exp as f64 * core::f64::consts::LN_2;
    (ln * (10.0 / core::f64::consts::LN_10)) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_table_matches_std() {
        for k in (0..UNIT_N).step_by(7) {
            let a = 2.0 * std::f64::consts::PI * k as f64 / UNIT_N as f64;
            assert!((UNIT[k].re as f64 - a.cos() * 32768.0).abs() <= 1.0);
            assert!((UNIT[k].im as f64 - a.sin() * 32768.0).abs() <= 1.0);
        }
    }

    #[test]
    fn atan2_matches_std() {
        let pts = [(1, 0), (0, 1), (-1, 0), (0, -1), (3, 4), (-5, 2), (-7, -9), (1000, -1), (123456789, 987654321)];
        for &(x, y) in &pts {
            let want = (y as f64).atan2(x as f64);
            let got = angle_rad(atan2_64(y, x));
            let mut d = got - want;
            if d > std::f64::consts::PI {
                d -= 2.0 * std::f64::consts::PI;
            }
            if d < -std::f64::consts::PI {
                d += 2.0 * std::f64::consts::PI;
            }
            assert!(d.abs() < 1e-6, "atan2({y},{x}) = {got} want {want}");
        }
    }

    #[test]
    fn isqrt_and_db() {
        for v in [0u64, 1, 2, 3, 4, 15, 16, 17, 1 << 40, u64::MAX] {
            let r = isqrt64(v);
            assert!(r * r <= v || r == 0);
            assert!((r + 1).checked_mul(r + 1).is_none_or(|s| s > v));
        }
        assert!((db10(100.0) - 20.0).abs() < 1e-4);
        assert!((db10(0.5) + 3.0103).abs() < 1e-3);
    }
}
