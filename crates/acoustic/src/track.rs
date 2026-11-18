//! Following one pitch through time and deciding "heard".

use crate::comb::{Comb, MAX_CANDS};
use crate::{Config, FrameOut, SizeClass, FRAMES_PER_S};

/// What the detector believes right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// The background is still settling after power-up.
    Warmup,
    /// Nothing rotor-like.
    Quiet,
    /// A rotor-like sound for about a second; not yet believed.
    Candidate,
    /// A rotor-like sound for most of the last three seconds.
    Heard,
}

/// Frames a candidate's pitch may move and still be the same sound (6%).
const FOLLOW: f32 = 0.06;
/// Frames looked at for the decision (about 3 s) and for "candidate" (1 s).
const LONG: u32 = 3 * FRAMES_PER_S;
const SHORT: u32 = FRAMES_PER_S;
/// Hits in [`LONG`] to start and to keep [`State::Heard`].
const HEARD_ON: u32 = LONG * 70 / 100;
const HEARD_OFF: u32 = LONG * 30 / 100;
/// Hits in [`SHORT`] for [`State::Candidate`].
const CAND_ON: u32 = SHORT / 2;
/// Frames without a hit before the track is dropped (1.5 s).
const LOSE_AFTER: u16 = (FRAMES_PER_S * 3 / 2) as u16;
/// Frames a different pitch must persist before it replaces the track.
const TAKEOVER: u8 = 6;
/// Pitches remembered for the wander figure (about 4 s).
const WANDER_N: usize = 128;

#[derive(Clone, Copy)]
struct Track {
    f0: f32,
    /// Bit 0: the latest frame was a hit.
    hist: u128,
    age: u32,
    miss_run: u16,
    score: f32,
    harmonics: u8,
    heard: bool,
    /// Frames in a row with next to no pitch movement.
    steady_run: u32,
}

pub struct Tracker {
    holding: bool,
    track: Option<Track>,
    rival_f0: f32,
    rival_run: u8,
    rival_score: f32,
    pitches: [f32; WANDER_N],
    pitch_n: usize,
    pitch_at: usize,
}

impl Tracker {
    pub const fn new() -> Self {
        Self {
            holding: false,
            track: None,
            rival_f0: 0.0,
            rival_run: 0,
            rival_score: 0.0,
            pitches: [0.0; WANDER_N],
            pitch_n: 0,
            pitch_at: 0,
        }
    }

    /// A rotor-like sound is being followed: the background must not
    /// learn it.
    pub fn holding(&self) -> bool {
        self.holding
    }

    fn remember(&mut self, f0: f32) {
        self.pitches[self.pitch_at] = f0;
        self.pitch_at = (self.pitch_at + 1) % WANDER_N;
        self.pitch_n = (self.pitch_n + 1).min(WANDER_N);
    }

    fn wander_pct(&self) -> f32 {
        if self.pitch_n < 8 {
            return 0.0;
        }
        let v = &self.pitches[..self.pitch_n];
        let mean = v.iter().sum::<f32>() / v.len() as f32;
        let var = v.iter().map(|x| (x - mean) * (x - mean)).sum::<f32>() / v.len() as f32;
        100.0 * libm::sqrtf(var) / mean
    }

    fn start(&mut self, c: &Comb, hits: u8) {
        self.track = Some(Track {
            f0: c.f0,
            hist: (1u128 << hits) - 1,
            age: u32::from(hits),
            miss_run: 0,
            score: c.score,
            harmonics: c.matched,
            heard: false,
            steady_run: 0,
        });
        self.pitch_n = 0;
        self.pitch_at = 0;
        self.remember(c.f0);
        self.rival_run = 0;
    }

    pub fn step(
        &mut self,
        cfg: &Config,
        cands: &[Comb; MAX_CANDS],
        level_dbfs: f32,
        warm: bool,
    ) -> FrameOut {
        let good = |c: &&Comb| c.matched > 0 && c.score >= cfg.hit_score;
        let best = cands.iter().find(good).copied();
        let mut hit = false;
        let mut neighbours = 0u8;

        if let Some(mut t) = self.track {
            let was = t.f0;
            let near = |c: &&Comb| libm::fabsf(c.f0 - was) <= FOLLOW * was;
            let follow = cands.iter().filter(good).find(near).copied();
            t.age = t.age.saturating_add(1);
            t.hist <<= 1;
            match follow {
                Some(c) => {
                    hit = true;
                    t.hist |= 1;
                    t.miss_run = 0;
                    t.f0 += 0.4 * (c.f0 - t.f0);
                    t.score += 0.2 * (c.score - t.score);
                    t.harmonics = c.matched;
                    self.remember(t.f0);
                }
                None => t.miss_run += 1,
            }
            // Something else, on a different pitch, frame after frame?
            let other = cands.iter().filter(good).find(|c| !near(c)).copied();
            match other {
                Some(c) if libm::fabsf(c.f0 - self.rival_f0) <= FOLLOW * c.f0 => {
                    self.rival_run = self.rival_run.saturating_add(1);
                    self.rival_f0 = c.f0;
                    self.rival_score = c.score;
                }
                Some(c) => {
                    self.rival_run = 1;
                    self.rival_f0 = c.f0;
                    self.rival_score = c.score;
                }
                None => self.rival_run = 0,
            }
            self.track = Some(t);
            if self.rival_run >= TAKEOVER && !t.heard && t.miss_run >= u16::from(TAKEOVER) {
                let c = Comb { f0: self.rival_f0, score: self.rival_score, matched: 3, mask: 0 };
                self.start(&c, TAKEOVER);
            } else if t.miss_run > LOSE_AFTER {
                self.track = None;
                self.rival_run = 0;
            }
        } else if let Some(c) = best {
            self.start(&c, 1);
            hit = true;
        }

        let wander = self.wander_pct();
        let settled = self.pitch_n >= WANDER_N / 2;
        let Some(t) = self.track.as_mut() else {
            self.holding = false;
            return FrameOut {
                state: if warm { State::Quiet } else { State::Warmup },
                hit: false,
                f0_hz: 0.0,
                score: best.map_or(0.0, |c| c.score),
                harmonics: 0,
                level_dbfs,
                confidence: 0,
                class: None,
                neighbours: 0,
                wander_pct: 0.0,
                steady: false,
            };
        };

        for c in cands.iter().filter(|c| c.matched > 0 && c.score >= 0.6 * t.score) {
            let off = libm::fabsf(c.f0 - t.f0) / t.f0;
            if (0.03..=0.25).contains(&off) {
                neighbours += 1;
            }
        }

        let long = (t.hist & ((1u128 << LONG) - 1)).count_ones();
        let short = (t.hist & ((1u128 << SHORT) - 1)).count_ones();
        // A pitch that does not move at all is a machine on the mains or a
        // fan, not a rotor holding a drone up.
        if settled && wander < cfg.steady_pct {
            t.steady_run = t.steady_run.saturating_add(1);
        } else {
            t.steady_run = 0;
        }
        let steady = t.steady_run >= (cfg.steady_s * FRAMES_PER_S as f32) as u32;
        t.heard = warm
            && !steady
            && if t.heard { long >= HEARD_OFF } else { t.age >= LONG && long >= HEARD_ON };
        let state = if !warm {
            State::Warmup
        } else if t.heard {
            State::Heard
        } else if short >= CAND_ON && !steady {
            State::Candidate
        } else {
            State::Quiet
        };
        let clear = (t.score / 60.0).clamp(0.0, 1.0);
        let confidence = (100.0 * long as f32 / LONG as f32 * clear) as u8;
        let (f0, harmonics, score) = (t.f0, t.harmonics, t.score);
        self.holding = !steady && (t.heard || short >= CAND_ON);
        FrameOut {
            state,
            hit,
            f0_hz: f0,
            score: if hit { score } else { 0.0 },
            harmonics,
            level_dbfs,
            confidence,
            class: Some(if f0 < cfg.class_split_hz {
                SizeClass::LargeProps
            } else {
                SizeClass::SmallProps
            }),
            neighbours,
            wander_pct: wander,
            steady,
        }
    }
}
