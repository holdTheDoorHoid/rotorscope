//! Seeded randomness for tests (no external crates).

/// SplitMix64 plus a Box-Muller Gaussian.
pub struct Rng(u64, Option<f64>);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed, None)
    }
    pub fn u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    pub fn u8(&mut self) -> u8 {
        self.u64() as u8
    }
    /// Uniform in [0, 1).
    pub fn f64(&mut self) -> f64 {
        (self.u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    /// Standard normal.
    pub fn gauss(&mut self) -> f64 {
        if let Some(v) = self.1.take() {
            return v;
        }
        let u1 = self.f64().max(1e-300);
        let u2 = self.f64();
        let r = (-2.0 * u1.ln()).sqrt();
        let t = 2.0 * std::f64::consts::PI * u2;
        self.1 = Some(r * t.sin());
        r * t.cos()
    }
}
