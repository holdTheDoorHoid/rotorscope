//! Hearing a drone: a rotor-sound detector for one microphone.
//!
//! A rotor makes a tone at its blade-pass rate (blades x turns per second)
//! and at every whole multiple of it. This crate looks for that ladder of
//! tones, follows its pitch from one moment to the next, and says "heard"
//! only when it has been there for most of a few seconds. PLAN.md Phase 15.
//!
//! **Input.** 16 kHz, one channel, 16-bit, [`HOP`] samples (32 ms) per call
//! to [`Detector::process`].
//!
//! **Steps per call.**
//! 1. Spectrum of the last [`FFT_N`] samples (128 ms, 7.8 Hz per bin),
//!    averaged over about a quarter of a second. Averaging steadies the
//!    noise, so a much fainter tone can be told from it.
//! 2. Background level per bin, two ways, the higher wins:
//!    - *across frequency*: the lower quartile of the bins around it, so
//!      wind and traffic rumble set the level and tones do not;
//!    - *across time*: a slow average of the bin itself (about a minute),
//!      so a hum sinks into the background. This average stands still
//!      while a rotor-like sound is being followed.
//! 3. Peaks well above the background.
//! 4. For each plausible pitch (70-1600 Hz), how many multiples of it have
//!    a peak ([`comb`]). The best ladder is this frame's candidate.
//! 5. A pitch track: the candidate must continue the pitch heard so far.
//!    Speech and music jump; a rotor glides.
//! 6. [`State::Heard`] once the track has been hit in most frames of about
//!    three seconds; it clears when the hits fall away.
//! 7. A pitch that does not move at all for ten seconds is a machine on the
//!    mains or a fan ([`FrameOut::steady`]): it is not reported, and the
//!    background is allowed to learn it. A rotor holding a drone in the air
//!    is corrected many times a second and never sits that still.
//!
//! **What it cannot tell apart.** Anything else that makes a steady ladder
//! of tones whose pitch drifts a little: a mower, a chainsaw, a motorbike
//! holding its revs, a sung note. The hub treats sound alone as a weaker
//! claim than a radio detection for that reason.
//!
//! No heap, no `std`; one [`Detector`] is about 40 KB and is meant to live
//! in a `static` or in PSRAM.
#![cfg_attr(not(any(test, feature = "synth")), no_std)]
#![cfg_attr(feature = "synth", allow(clippy::excessive_precision))]

mod comb;
mod fft;
mod track;
pub mod wav;

#[cfg(any(test, feature = "synth"))]
pub mod synth;
#[cfg(test)]
mod tests;

pub use comb::{Comb, Peak, MAX_PEAKS};
pub use track::State;

/// Samples per second expected by [`Detector::process`].
pub const SAMPLE_RATE: u32 = 16_000;
/// Samples per spectrum.
pub const FFT_N: usize = 2048;
/// New samples per call (32 ms).
pub const HOP: usize = 512;
/// Spectrum bins used (0 .. Nyquist).
pub const BINS: usize = FFT_N / 2;
/// Width of one bin, Hz.
pub const BIN_HZ: f32 = SAMPLE_RATE as f32 / FFT_N as f32;
/// Calls per second.
pub const FRAMES_PER_S: u32 = SAMPLE_RATE / HOP as u32;

/// Spacing of the points where the across-frequency background is measured.
const FLOOR_STEP: usize = 16;
const FLOOR_POINTS: usize = BINS / FLOOR_STEP + 1;
/// Bins either side of a point that feed its background estimate; every
/// [`FLOOR_SKIP`]th one is used (neighbouring bins say nearly the same
/// thing, and sorting them all cost 4 ms a frame on the hub).
const FLOOR_HALF: usize = 32;
const FLOOR_SKIP: usize = 2;
/// The lower quartile of the averaged noise spectrum sits at this fraction
/// of its mean. Measured on white noise with [`SMOOTH`] (test
/// `floor_estimate_is_unbiased`); it is 0.29 for an unaveraged spectrum.
const QUARTILE_OF_MEAN: f32 = 0.70;
/// Weight of the newest frame in the averaged spectrum.
const SMOOTH: f32 = 0.22;

/// Rough size of what is making the sound, from its pitch alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeClass {
    /// Low pitch: large, slow propellers (camera drones; also most engines).
    LargeProps,
    /// High pitch: small, fast propellers (FPV and small drones).
    SmallProps,
}

/// Tuning. The defaults were set on the recordings listed in
/// docs/ACOUSTIC.md; a site should not need to change them.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// Lowest and highest blade-pass pitch searched, Hz.
    pub f0_min: f32,
    pub f0_max: f32,
    /// Highest frequency used, Hz.
    pub f_top: f32,
    /// A peak must stand this far above the background, dB.
    pub peak_db: f32,
    /// A frame counts as a hit when its ladder scores at least this.
    pub hit_score: f32,
    /// Pitch below this is [`SizeClass::LargeProps`].
    pub class_split_hz: f32,
    /// Time constant of the across-time background, seconds.
    pub absorb_s: f32,
    /// Frames at the start during which nothing is reported while the
    /// background settles.
    pub warmup_frames: u32,
    /// A pitch that moves less than this (percent, over four seconds) for
    /// [`Self::steady_s`] seconds is a hum.
    pub steady_pct: f32,
    pub steady_s: f32,
}

impl Config {
    pub const DEFAULT: Self = Self {
        f0_min: 70.0,
        f0_max: 1600.0,
        f_top: 7400.0,
        peak_db: 4.5,
        hit_score: 30.0,
        class_split_hz: 350.0,
        absorb_s: 60.0,
        warmup_frames: 5 * FRAMES_PER_S,
        steady_pct: 0.2,
        steady_s: 10.0,
    };
}

impl Default for Config {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// What one call to [`Detector::process`] found.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameOut {
    pub state: State,
    /// This frame alone showed a rotor-like ladder on the tracked pitch.
    pub hit: bool,
    /// Tracked blade-pass pitch, Hz (0 with no track).
    pub f0_hz: f32,
    /// Ladder score of this frame's best candidate (0 if none).
    pub score: f32,
    /// Multiples of the pitch that had a peak in this frame.
    pub harmonics: u8,
    /// Loudness of the last 32 ms, dB relative to full scale.
    pub level_dbfs: f32,
    /// 0-100: share of recent frames hit, scaled by how clear the ladder is.
    pub confidence: u8,
    /// Size class of the tracked pitch.
    pub class: Option<SizeClass>,
    /// Other ladders at nearby pitches in this frame (several rotors turning
    /// at slightly different speeds), 0-3.
    pub neighbours: u8,
    /// How much the tracked pitch has moved over the last four seconds,
    /// percent of the pitch (standard deviation).
    pub wander_pct: f32,
    /// The pitch has not moved for [`Config::steady_s`]: a hum, not a rotor.
    /// Never [`State::Heard`] while this is set.
    pub steady: bool,
}

/// The detector. See the crate documentation.
pub struct Detector {
    pub cfg: Config,
    table: fft::Table,
    /// The last [`FFT_N`] samples, oldest first.
    hist: [i16; FFT_N],
    re: [f32; FFT_N],
    im: [f32; FFT_N],
    power: [f32; BINS],
    /// `power` averaged over the last few frames; everything after the
    /// transform works on this.
    avg: [f32; BINS],
    /// Across-time background per bin.
    slow: [f32; BINS],
    /// Across-frequency background at every [`FLOOR_STEP`]th bin.
    local: [f32; FLOOR_POINTS],
    frames: u32,
    track: track::Tracker,
    ready: bool,
}

impl Detector {
    /// All zero, usable in a `static`. Tables are filled on first use.
    pub const fn zeroed() -> Self {
        Self {
            cfg: Config::DEFAULT,
            table: fft::Table::zeroed(),
            hist: [0; FFT_N],
            re: [0.0; FFT_N],
            im: [0.0; FFT_N],
            power: [0.0; BINS],
            avg: [0.0; BINS],
            slow: [0.0; BINS],
            local: [0.0; FLOOR_POINTS],
            frames: 0,
            track: track::Tracker::new(),
            ready: false,
        }
    }

    /// Build a detector in place, for memory that is not a `static` (the hub
    /// keeps it in PSRAM): nothing this size crosses the stack.
    pub fn init_in_place(slot: &mut core::mem::MaybeUninit<Self>) -> &mut Self {
        let p = slot.as_mut_ptr();
        // SAFETY: `p` is valid for one `Self`. All-zero bytes are a valid
        // value for every field except `cfg` and `track`, which are written
        // before the reference is made (the number arrays, the counter and
        // the `ready` flag are plain numbers and a bool).
        unsafe {
            core::ptr::write_bytes(p, 0, 1);
            (&raw mut (*p).cfg).write(Config::DEFAULT);
            (&raw mut (*p).track).write(track::Tracker::new());
            &mut *p
        }
    }

    /// Forget everything heard (not the tuning).
    pub fn reset(&mut self) {
        self.hist = [0; FFT_N];
        self.slow = [0.0; BINS];
        self.avg = [0.0; BINS];
        self.frames = 0;
        self.track = track::Tracker::new();
    }

    /// Frames processed since the start or the last [`Self::reset`].
    pub fn frames(&self) -> u32 {
        self.frames
    }

    /// Power spectrum of the latest frame (bin `k` is `k * BIN_HZ`).
    pub fn spectrum(&self) -> &[f32; BINS] {
        &self.power
    }

    /// The spectrum averaged over the last few frames.
    pub fn averaged(&self) -> &[f32; BINS] {
        &self.avg
    }

    /// Background level under bin `k` in the latest frame.
    pub fn background(&self, k: usize) -> f32 {
        let j = k / FLOOR_STEP;
        let frac = (k % FLOOR_STEP) as f32 / FLOOR_STEP as f32;
        let a = self.local[j];
        let b = self.local[(j + 1).min(FLOOR_POINTS - 1)];
        let across = a + (b - a) * frac;
        across.max(self.slow[k])
    }

    /// Take the next [`HOP`] samples.
    pub fn process(&mut self, hop: &[i16; HOP]) -> FrameOut {
        self.process_timed(hop, &mut || 0).0
    }

    /// [`Self::process`], also returning how long each stage took by the
    /// caller's clock (window, transform, averaging, background, peaks,
    /// ladder and tracking), for measuring on the chip.
    pub fn process_timed(&mut self, hop: &[i16; HOP], now: &mut dyn FnMut() -> u32) -> (FrameOut, [u32; 6]) {
        let mut t = [0u32; 7];
        t[0] = now();
        if !self.ready {
            self.table.fill();
            self.ready = true;
        }
        self.hist.copy_within(HOP.., 0);
        self.hist[FFT_N - HOP..].copy_from_slice(hop);

        let mut sum_sq = 0.0f32;
        for &s in hop {
            let v = f32::from(s) * (1.0 / 32768.0);
            sum_sq += v * v;
        }
        let level_dbfs = 10.0 * libm::log10f(sum_sq / HOP as f32 + 1e-12);

        for n in 0..FFT_N {
            self.re[n] = f32::from(self.hist[n]) * (1.0 / 32768.0) * self.table.hann(n);
            self.im[n] = 0.0;
        }
        t[1] = now();
        fft::forward(&self.table, &mut self.re, &mut self.im);
        t[2] = now();
        let w = if self.frames == 0 { 1.0 } else { SMOOTH };
        for k in 0..BINS {
            self.power[k] = self.re[k] * self.re[k] + self.im[k] * self.im[k] + 1e-20;
            self.avg[k] += w * (self.power[k] - self.avg[k]);
        }
        t[3] = now();
        self.local_floor();
        self.slow_floor();
        self.frames = self.frames.saturating_add(1);
        t[4] = now();
        let (peaks, n) = self.peaks();
        t[5] = now();
        let cands = comb::candidates(&self.cfg, &peaks[..n]);
        let warm = self.frames > self.cfg.warmup_frames;
        let out = self.track.step(&self.cfg, &cands, level_dbfs, warm);
        t[6] = now();
        let mut d = [0u32; 6];
        for i in 0..6 {
            d[i] = t[i + 1].wrapping_sub(t[i]);
        }
        (out, d)
    }

    /// Lower quartile of the bins around each floor point, scaled to the
    /// mean it implies for noise.
    fn local_floor(&mut self) {
        let mut buf = [0.0f32; 2 * FLOOR_HALF / FLOOR_SKIP + 1];
        for j in 0..FLOOR_POINTS {
            let c = j * FLOOR_STEP;
            let lo = c.saturating_sub(FLOOR_HALF).max(2);
            let hi = (c + FLOOR_HALF).min(BINS - 1);
            let mut n = 0;
            for k in (lo..=hi).step_by(FLOOR_SKIP) {
                buf[n] = self.avg[k];
                n += 1;
            }
            let w = &mut buf[..n];
            // Small window: insertion sort.
            for i in 1..w.len() {
                let v = w[i];
                let mut p = i;
                while p > 0 && w[p - 1] > v {
                    w[p] = w[p - 1];
                    p -= 1;
                }
                w[p] = v;
            }
            self.local[j] = w[w.len() / 4] / QUARTILE_OF_MEAN;
        }
    }

    /// Slow per-bin average. Fast for the first seconds so a hum that is
    /// already there at power-up is absorbed before anything is reported;
    /// it falls ten times faster than it rises, so a passing loud sound
    /// does not leave a deaf spot behind.
    fn slow_floor(&mut self) {
        // While a rotor-like sound is followed the background stands still,
        // or a hovering drone would be learned as background in seconds.
        if self.track.holding() {
            return;
        }
        let rise = if self.frames < self.cfg.warmup_frames {
            1.0 / (0.5 * FRAMES_PER_S as f32)
        } else {
            1.0 / (self.cfg.absorb_s * FRAMES_PER_S as f32)
        };
        let fall = (rise * 10.0).min(0.5);
        for k in 0..BINS {
            let d = self.power[k] - self.slow[k];
            self.slow[k] += d * if d > 0.0 { rise } else { fall };
        }
    }

    /// Local maxima standing [`Config::peak_db`] above the background; the
    /// strongest [`MAX_PEAKS`], in frequency order.
    fn peaks(&self) -> ([Peak; MAX_PEAKS], usize) {
        let mut out = [Peak { hz: 0.0, db: 0.0 }; MAX_PEAKS];
        let mut n = 0;
        let ratio = libm::powf(10.0, self.cfg.peak_db / 10.0);
        let k_lo = ((self.cfg.f0_min * 0.9 / BIN_HZ) as usize).max(3);
        let k_hi = ((self.cfg.f_top / BIN_HZ) as usize).min(BINS - 2);
        for k in k_lo..=k_hi {
            let p = self.avg[k];
            if p <= self.avg[k - 1] || p < self.avg[k + 1] {
                continue;
            }
            let over = p / self.background(k);
            if over < ratio {
                continue;
            }
            // Peak position between bins: a parabola through the log power.
            let (a, b, c) = (
                libm::logf(self.avg[k - 1]),
                libm::logf(p),
                libm::logf(self.avg[k + 1]),
            );
            let den = a - 2.0 * b + c;
            let off = if den < -1e-6 { (0.5 * (a - c) / den).clamp(-0.5, 0.5) } else { 0.0 };
            let peak = Peak { hz: (k as f32 + off) * BIN_HZ, db: 10.0 * libm::log10f(over) };
            if n < MAX_PEAKS {
                out[n] = peak;
                n += 1;
            } else {
                let mut weakest = 0;
                for i in 1..MAX_PEAKS {
                    if out[i].db < out[weakest].db {
                        weakest = i;
                    }
                }
                if peak.db > out[weakest].db {
                    // Keep frequency order: close the gap, append.
                    out.copy_within(weakest + 1.., weakest);
                    out[MAX_PEAKS - 1] = peak;
                }
            }
        }
        (out, n)
    }
}
