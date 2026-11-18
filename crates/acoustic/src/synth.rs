//! Made-up sounds for tests and tools: rotors, hum, speech-like bursts and
//! noise. Nothing here is a model of a real drone; real recordings are
//! fetched by `tools/fetch-acoustic-samples.sh`.

use crate::SAMPLE_RATE;

const FS: f32 = SAMPLE_RATE as f32;
const TAU: f32 = core::f32::consts::TAU;

/// Small repeatable random source.
pub struct Rng(pub u64);

impl Rng {
    pub fn unit(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Roughly normal, variance 1.
    pub fn gauss(&mut self) -> f32 {
        let mut s = 0.0;
        for _ in 0..12 {
            s += self.unit();
        }
        s - 6.0
    }

    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }
}

/// A sound being built, in full-scale units (1.0 = clipping).
pub struct Sound {
    pub samples: Vec<f32>,
}

impl Sound {
    pub fn silence(seconds: f32) -> Self {
        Self { samples: vec![0.0; (seconds * FS) as usize] }
    }

    /// White noise of the given RMS over the whole sound.
    pub fn noise(mut self, rms: f32, seed: u64) -> Self {
        let mut r = Rng(seed | 1);
        for s in &mut self.samples {
            *s += rms * r.gauss();
        }
        self
    }

    /// Noise that is louder at low frequencies (wind and traffic rumble).
    pub fn rumble(mut self, rms: f32, seed: u64) -> Self {
        let mut r = Rng(seed | 1);
        let mut y = 0.0f32;
        for s in &mut self.samples {
            y += 0.02 * (r.gauss() - y);
            *s += rms * 7.0 * y;
        }
        self
    }

    /// A ladder of tones from `from` to `to` seconds. `pitch(t)` is the
    /// fundamental at time `t`; multiple `h` has amplitude `amp / h^rolloff`.
    pub fn ladder(
        mut self,
        from: f32,
        to: f32,
        amp: f32,
        harmonics: u32,
        rolloff: f32,
        pitch: impl Fn(f32) -> f32,
    ) -> Self {
        let mut phase = 0.0f32;
        let (a, b) = ((from * FS) as usize, ((to * FS) as usize).min(self.samples.len()));
        for i in a..b {
            let t = i as f32 / FS;
            let f = pitch(t);
            phase = (phase + TAU * f / FS) % TAU;
            // Fade in and out over 50 ms so the ends do not click.
            let edge = ((i - a).min(b - i) as f32 / (0.05 * FS)).min(1.0);
            let mut v = 0.0;
            for h in 1..=harmonics {
                if f * h as f32 > 0.45 * FS {
                    break;
                }
                v += libm::sinf(phase * h as f32 + h as f32 * 1.3) / libm::powf(h as f32, rolloff);
            }
            self.samples[i] += amp * edge * v;
        }
        self
    }

    /// Four rotors near `f0`, each a little off and each drifting slowly, as
    /// a hovering quadcopter's do.
    pub fn quad(mut self, from: f32, to: f32, amp: f32, f0: f32, seed: u64) -> Self {
        let mut r = Rng(seed | 1);
        for _ in 0..4 {
            let off = r.range(-0.04, 0.04);
            let (p1, p2) = (r.range(0.0, TAU), r.range(0.0, TAU));
            let (w1, w2) = (r.range(0.2, 0.5), r.range(0.9, 1.7));
            self = self.ladder(from, to, amp, 14, 1.0, move |t| {
                f0 * (1.0 + off + 0.008 * libm::sinf(TAU * w1 * t + p1) + 0.004 * libm::sinf(TAU * w2 * t + p2))
            });
        }
        self
    }

    /// Voiced bursts with a pitch that jumps from one to the next, with
    /// gaps: the rhythm of speech without being speech.
    pub fn speechlike(mut self, from: f32, to: f32, amp: f32, seed: u64) -> Self {
        let mut r = Rng(seed | 1);
        let mut t = from;
        while t < to {
            let len = r.range(0.12, 0.35);
            let f = r.range(95.0, 240.0);
            let bend = r.range(-0.2, 0.2);
            let end = (t + len).min(to);
            let t0 = t;
            self = self.ladder(t, end, amp, 20, 1.2, move |x| f * (1.0 + bend * (x - t0) / len));
            t = end + r.range(0.05, 0.3);
        }
        self
    }

    pub fn into_pcm(self) -> Vec<i16> {
        self.samples.iter().map(|v| (v * 32768.0).clamp(-32768.0, 32767.0) as i16).collect()
    }
}
