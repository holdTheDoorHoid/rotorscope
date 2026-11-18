//! Scoring a ladder of tones: does this pitch have peaks at its multiples?

use crate::Config;

/// Peaks kept per frame.
pub const MAX_PEAKS: usize = 48;
/// Candidates handed to the pitch tracker per frame.
pub const MAX_CANDS: usize = 4;
/// Multiples of the pitch looked at.
const MAX_HARMONICS: usize = 12;
/// A peak's score is capped here, dB, so one very loud tone cannot stand in
/// for a ladder.
const CAP_DB: f32 = 25.0;
/// Cost of each missing multiple among the first eight, dB.
const MISS_DB: f32 = 2.0;

/// One spectral peak.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Peak {
    pub hz: f32,
    /// Height above the background, dB.
    pub db: f32,
}

/// One pitch and how well the frame's peaks fit its ladder.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Comb {
    pub f0: f32,
    pub score: f32,
    /// Multiples with a peak.
    pub matched: u8,
    /// Bit `h-1` set: multiple `h` had a peak.
    pub mask: u16,
}

impl Comb {
    pub const NONE: Self = Self { f0: 0.0, score: 0.0, matched: 0, mask: 0 };
}

/// Fit the ladder of `f0` to `peaks` (frequency order). `tight` narrows the
/// window once the pitch has been refined.
fn fit(cfg: &Config, f0: f32, peaks: &[Peak], tight: bool) -> (Comb, f32) {
    let top = ((cfg.f_top / f0) as usize).min(MAX_HARMONICS);
    let (mut score, mut matched, mut mask) = (0.0f32, 0u8, 0u16);
    // Least squares of peak = h * f0, weighted by peak height.
    let (mut num, mut den) = (0.0f32, 0.0f32);
    let mut start = 0;
    for h in 1..=top {
        let want = f0 * h as f32;
        let tol = if tight { 5.0 + 0.004 * want } else { 6.0 + 0.012 * want }.min(0.4 * f0);
        while start < peaks.len() && peaks[start].hz < want - tol {
            start += 1;
        }
        let mut best: Option<Peak> = None;
        let mut i = start;
        while i < peaks.len() && peaks[i].hz <= want + tol {
            if best.is_none_or(|b| peaks[i].db > b.db) {
                best = Some(peaks[i]);
            }
            i += 1;
        }
        match best {
            Some(p) => {
                let w = p.db.min(CAP_DB);
                score += w;
                matched += 1;
                mask |= 1 << (h - 1);
                num += w * h as f32 * p.hz;
                den += w * (h * h) as f32;
            }
            None if h <= 8 => score -= MISS_DB,
            None => {}
        }
    }
    let refined = if den > 0.0 { num / den } else { f0 };
    (Comb { f0, score, matched, mask }, refined)
}

/// A real ladder has neighbours: at least two multiples next to each other,
/// one of them the first or second. A pitch one octave too low matches only
/// every other rung and fails this.
fn plausible(c: &Comb) -> bool {
    c.matched >= 3 && c.mask & 0b11 != 0 && c.mask & (c.mask >> 1) != 0
}

/// The best few distinct pitches in this frame, best first.
pub fn candidates(cfg: &Config, peaks: &[Peak]) -> [Comb; MAX_CANDS] {
    let mut out = [Comb::NONE; MAX_CANDS];
    for p in peaks {
        // The peak may be the pitch itself or one of its first multiples.
        for d in 1..=4u8 {
            let f0 = p.hz / f32::from(d);
            if f0 < cfg.f0_min || f0 > cfg.f0_max {
                continue;
            }
            let (_, refined) = fit(cfg, f0, peaks, false);
            if refined < cfg.f0_min || refined > cfg.f0_max {
                continue;
            }
            let (mut c, again) = fit(cfg, refined, peaks, true);
            c.f0 = again;
            if !plausible(&c) || c.score <= 0.0 {
                continue;
            }
            insert(&mut out, c);
        }
    }
    out
}

/// Keep `out` sorted by score with pitches at least 3% apart.
fn insert(out: &mut [Comb; MAX_CANDS], c: Comb) {
    for i in 0..MAX_CANDS {
        if out[i].matched > 0 && libm::fabsf(out[i].f0 - c.f0) < 0.03 * c.f0 {
            if c.score <= out[i].score {
                return;
            }
            // Better fit of the same pitch: drop the old one, insert anew.
            out.copy_within(i + 1.., i);
            out[MAX_CANDS - 1] = Comb::NONE;
            break;
        }
    }
    for i in 0..MAX_CANDS {
        if c.score > out[i].score {
            out.copy_within(i..MAX_CANDS - 1, i + 1);
            out[i] = c;
            return;
        }
    }
}
