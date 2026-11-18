//! In-place radix-2 FFT, single precision, with its table kept by the caller.

use crate::FFT_N;

const HALF: usize = FFT_N / 2;

/// cos and sin of 2*pi*k/N for k < N/2.
pub struct Table {
    pub cos: [f32; HALF],
    pub sin: [f32; HALF],
}

impl Table {
    pub const fn zeroed() -> Self {
        Self { cos: [0.0; HALF], sin: [0.0; HALF] }
    }

    pub fn fill(&mut self) {
        for k in 0..HALF {
            let a = 2.0 * core::f32::consts::PI * k as f32 / FFT_N as f32;
            self.cos[k] = libm::cosf(a);
            self.sin[k] = libm::sinf(a);
        }
    }

    /// Hann window value at sample `n`, from the cosine table.
    #[inline]
    pub fn hann(&self, n: usize) -> f32 {
        let c = if n < HALF { self.cos[n] } else { -self.cos[n - HALF] };
        0.5 - 0.5 * c
    }
}

/// Forward transform of `re` + i*`im`, in place.
pub fn forward(t: &Table, re: &mut [f32; FFT_N], im: &mut [f32; FFT_N]) {
    let bits = FFT_N.trailing_zeros();
    for i in 0..FFT_N {
        let j = i.reverse_bits() >> (usize::BITS - bits);
        if j > i {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= FFT_N {
        let step = FFT_N / len;
        let half = len / 2;
        for (rc, ic) in re.chunks_exact_mut(len).zip(im.chunks_exact_mut(len)) {
            let (ra, rb) = rc.split_at_mut(half);
            let (ia, ib) = ic.split_at_mut(half);
            let tw = t.cos.iter().step_by(step).zip(t.sin.iter().step_by(step));
            for ((((ra, rb), ia), ib), (&c, &s)) in ra.iter_mut().zip(rb).zip(ia).zip(ib).zip(tw) {
                // Multiply by cos - i*sin.
                let xr = *rb * c + *ib * s;
                let xi = *ib * c - *rb * s;
                *rb = *ra - xr;
                *ib = *ia - xi;
                *ra += xr;
                *ia += xi;
            }
        }
        len <<= 1;
    }
}
