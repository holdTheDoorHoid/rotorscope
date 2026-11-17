//! Full-band survey of a recording at its own rate, independent of the C5
//! model: a spectrogram (1024-point FFTs, 4 averaged per cell, bins in
//! pairs), cells over the noise floor joined into time-frequency tracks,
//! one CSV row per track (a burst: start, length, centre, width, level).
//! For the first bursts of each width class, the autocorrelation over
//! 1-100 µs of lag (by FFT) is accumulated, to find the real symbol length
//! (the cyclic prefix's lag) without assuming one.

use std::io::Write;

use crate::dsp::{Cf, Fft, lowpass};

pub const NFFT: usize = 1024;
const AVG: usize = 4;
const PAIR: usize = 2;
pub const GROUPS: usize = NFFT / PAIR;
/// Hot: this far over the floor (dB) in an 8-dof cell.
const HOT_DB: f32 = 8.0;
/// Lag accumulators: up to 100 µs.
pub const LAG_MAX_US: f64 = 100.0;

#[derive(Clone, Debug)]
struct Track {
    t0: usize,
    last: usize,
    lo: usize,
    hi: usize,
    /// Sum over rows of span widths (groups), rows.
    wsum: usize,
    rows: usize,
    /// Power-weighted centroid accumulators (linear over floor).
    pc: f64,
    p: f64,
    peak_db: f32,
    analysed: bool,
    /// Lag features of the analysed part: best lag (us), |r|^2 there, at
    /// 33.33 us, at 66.67 us, and the median over 2-80 us.
    lagf: Option<(f64, f64, f64, f64, f64)>,
    /// The last row's span.
    cur: (usize, usize),
}

pub struct LagAcc {
    pub fs: f64,
    pub n: usize,
    pub bursts: usize,
    pub acc: Vec<f64>,
}

impl LagAcc {
    fn new(fs: f64) -> LagAcc {
        let n = (LAG_MAX_US * 1e-6 * fs) as usize + 1;
        LagAcc { fs, n, bursts: 0, acc: vec![0.0; n] }
    }
}

pub struct Survey {
    fs: f64,
    fc_hz: f64,
    fft: Fft,
    win: Vec<f32>,
    /// Samples of the current and previous block, and the absolute index of
    /// `buf[0]`.
    buf: Vec<Cf>,
    buf0: usize,
    /// Cell row counter (absolute) and FFT frame phase.
    row: usize,
    pending: Vec<Cf>,
    acc: Vec<f32>,
    nacc: usize,
    floor: Vec<f32>,
    rows_blk: Vec<Vec<f32>>,
    tracks: Vec<Track>,
    out: std::io::BufWriter<std::fs::File>,
    pub narrow: LagAcc,
    pub wide: LagAcc,
    pub psd: Vec<f64>,
    pub psd_rows: usize,
    pub bursts: usize,
    max_lag_per_block: usize,
    lag_done_blk: usize,
}

fn db(v: f32) -> f32 {
    10.0 * v.max(1e-30).log10()
}

impl Survey {
    pub fn new(fs: f64, fc_hz: f64, path: &std::path::Path) -> Survey {
        let mut out = std::io::BufWriter::new(std::fs::File::create(path).expect("survey csv"));
        writeln!(out, "t_ms,dur_us,f_mhz,w_mhz,wmax_mhz,lo_mhz,hi_mhz,peak_db,rows,lag_us,r_best,r33,r67,r_med").unwrap();
        let win = (0..NFFT).map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / NFFT as f64).cos()) as f32).collect();
        Survey {
            fs,
            fc_hz,
            fft: Fft::new(NFFT),
            win,
            buf: vec![],
            buf0: 0,
            row: 0,
            pending: vec![],
            acc: vec![0.0; GROUPS],
            nacc: 0,
            floor: vec![],
            rows_blk: vec![],
            tracks: vec![],
            out,
            narrow: LagAcc::new(5e6),
            wide: LagAcc::new(25e6),
            psd: vec![0.0; GROUPS],
            psd_rows: 0,
            bursts: 0,
            max_lag_per_block: 6,
            lag_done_blk: 0,
        }
    }

    fn row_us(&self) -> f64 {
        (NFFT * AVG) as f64 / self.fs * 1e6
    }

    fn group_mhz(&self, g: f64) -> f64 {
        (self.fc_hz + (g + 0.5 - GROUPS as f64 / 2.0) * PAIR as f64 * self.fs / NFFT as f64) / 1e6
    }

    /// One block of samples (any length; the caller's dwell).
    pub fn push(&mut self, x: &[Cf]) {
        // Keep the previous block for burst analysis.
        let keep_from = self.buf.len().saturating_sub(x.len().max(1));
        if keep_from > 0 {
            self.buf.drain(..keep_from);
            self.buf0 += keep_from;
        }
        self.buf.extend_from_slice(x);
        self.lag_done_blk = 0;
        // Spectrogram rows of this block.
        self.pending.extend_from_slice(x);
        let mut rows: Vec<Vec<f32>> = vec![];
        let mut fr = vec![[0f32; 2]; NFFT];
        let mut k = 0;
        while k + NFFT <= self.pending.len() {
            for i in 0..NFFT {
                let s = self.pending[k + i];
                fr[i] = [s[0] * self.win[i], s[1] * self.win[i]];
            }
            self.fft.run(&mut fr, false);
            for g in 0..GROUPS {
                // FFT order to ascending frequency.
                let mut p = 0.0;
                for j in 0..PAIR {
                    let b = (g * PAIR + j + NFFT / 2) % NFFT;
                    p += fr[b][0] * fr[b][0] + fr[b][1] * fr[b][1];
                }
                self.acc[g] += p;
            }
            self.nacc += 1;
            if self.nacc == AVG {
                rows.push(std::mem::replace(&mut self.acc, vec![0.0; GROUPS]));
                self.nacc = 0;
            }
            k += NFFT;
        }
        self.pending.drain(..k);
        // Floor of this block: per group p10, capped at the median of them.
        if !rows.is_empty() {
            let mut p10 = vec![0f32; GROUPS];
            let mut col = vec![0f32; rows.len()];
            for g in 0..GROUPS {
                for (r, row) in rows.iter().enumerate() {
                    col[r] = row[g];
                }
                col.sort_by(|a, b| a.partial_cmp(b).unwrap());
                p10[g] = col[col.len() / 10];
            }
            let mut s = p10.clone();
            s.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let med = s[GROUPS / 2];
            self.floor = p10.iter().map(|&v| v.min(med)).collect();
        }
        for row in std::mem::take(&mut rows) {
            for g in 0..GROUPS {
                self.psd[g] += row[g] as f64;
            }
            self.psd_rows += 1;
            self.row_in(&row);
        }
        let _ = &self.rows_blk;
    }

    fn row_in(&mut self, row: &[f32]) {
        let r = self.row;
        self.row += 1;
        let thr: Vec<f32> = self.floor.iter().map(|f| f * 10f32.powf(HOT_DB / 10.0)).collect();
        // Spans: hot groups, bridging one cold group; at least 2 groups.
        let mut spans = vec![];
        let mut g = 0;
        while g < GROUPS {
            if row[g] <= thr[g] {
                g += 1;
                continue;
            }
            let s = g;
            let mut e = g;
            g += 1;
            while g < GROUPS && (row[g] > thr[g] || (g + 1 < GROUPS && row[g + 1] > thr[g + 1])) {
                if row[g] > thr[g] {
                    e = g;
                }
                g += 1;
            }
            if e - s + 1 >= 2 {
                spans.push((s, e));
            }
        }
        // Attach spans to tracks active in the last two rows.
        for &(s, e) in &spans {
            let hit = self.tracks.iter().position(|t| t.last + 2 >= r && s <= t.cur.1 + 1 && e + 1 >= t.cur.0);
            let mut pc = 0.0;
            let mut p = 0.0;
            let mut peak = f32::MIN;
            for g in s..=e {
                let over = (row[g] / self.floor[g]) as f64 - 1.0;
                if over > 0.0 {
                    pc += over * g as f64;
                    p += over;
                }
                peak = peak.max(db(row[g] / self.floor[g]));
            }
            match hit {
                Some(i) => {
                    let t = &mut self.tracks[i];
                    if t.last == r {
                        // A second span in the same row: widen this row's span.
                        t.wsum += e + 1 - s;
                        t.cur = (t.cur.0.min(s), t.cur.1.max(e));
                    } else {
                        t.wsum += e + 1 - s;
                        t.rows += 1;
                        t.cur = (s, e);
                    }
                    t.last = r;
                    t.lo = t.lo.min(s);
                    t.hi = t.hi.max(e);
                    t.pc += pc;
                    t.p += p;
                    t.peak_db = t.peak_db.max(peak);
                }
                None => self.tracks.push(Track { t0: r, last: r, lo: s, hi: e, wsum: e + 1 - s, rows: 1, pc, p, peak_db: peak, analysed: false, lagf: None, cur: (s, e) }),
            }
        }
        // Analyse young tracks at 2 ms; close stale ones.
        let row_us = self.row_us();
        let mut i = 0;
        while i < self.tracks.len() {
            let t = &self.tracks[i];
            let age_rows = r + 1 - t.t0;
            if !t.analysed && (age_rows as f64 * row_us >= 2000.0) {
                let t = self.tracks[i].clone();
                self.tracks[i].lagf = self.lag(&t, r + 1);
                self.tracks[i].analysed = true;
            }
            if self.tracks[i].last + 2 < r {
                let t = self.tracks.remove(i);
                self.close(t);
            } else {
                i += 1;
            }
        }
    }

    fn close(&mut self, mut t: Track) {
        if t.rows < 2 {
            return;
        }
        if !t.analysed {
            let r = t.last + 1;
            t.lagf = self.lag(&t, r);
        }
        let row_us = self.row_us();
        let dur = (t.last + 1 - t.t0) as f64 * row_us;
        let gw = PAIR as f64 * self.fs / NFFT as f64 / 1e6;
        let w = t.wsum as f64 / t.rows as f64 * gw;
        let c = if t.p > 0.0 { t.pc / t.p } else { (t.lo + t.hi) as f64 / 2.0 };
        self.bursts += 1;
        let lf = match t.lagf {
            Some((l, b, a33, a67, med)) => format!("{l:.2},{b:.5},{a33:.5},{a67:.5},{med:.5}"),
            None => ",,,,".to_string(),
        };
        writeln!(
            self.out,
            "{:.3},{:.1},{:.4},{:.3},{:.3},{:.3},{:.3},{:.1},{},{}",
            t.t0 as f64 * row_us / 1e3,
            dur,
            self.group_mhz(c),
            w,
            (t.hi + 1 - t.lo) as f64 * gw,
            self.group_mhz(t.lo as f64) - gw / 2.0,
            self.group_mhz(t.hi as f64) + gw / 2.0,
            t.peak_db,
            t.rows,
            lf
        )
        .unwrap();
    }

    /// Accumulate the autocorrelation of up to 2 ms of a track, mixed to its
    /// centre and filtered to its width.
    fn lag(&mut self, t: &Track, end_row: usize) -> Option<(f64, f64, f64, f64, f64)> {
        let gw = PAIR as f64 * self.fs / NFFT as f64;
        let w = t.wsum as f64 / t.rows as f64 * gw;
        let row_n = NFFT * AVG;
        let narrow = w < 4e6;
        let max_s = if narrow { 2e-3 } else { 1e-3 };
        let (s0, s1) = (t.t0 * row_n, (end_row * row_n).min(t.t0 * row_n + (max_s * self.fs) as usize));
        if s0 < self.buf0 || s1 > self.buf0 + self.buf.len() || s1 <= s0 {
            return None;
        }
        let dur_us = (s1 - s0) as f64 / self.fs * 1e6;
        if dur_us < 100.0 || w < 300e3 {
            return None;
        }
        // Long wide tracks (video) are many; analyse a few per block.
        let long_wide = !narrow && (w >= 11e6 || (end_row - t.t0) as f64 * self.row_us() > 900.0);
        if long_wide {
            if self.lag_done_blk >= self.max_lag_per_block {
                return None;
            }
            self.lag_done_blk += 1;
        }
        let c = if t.p > 0.0 { t.pc / t.p } else { (t.lo + t.hi) as f64 / 2.0 };
        let f_off = (c + 0.5 - GROUPS as f64 / 2.0) * gw;
        let (dec, acc) = if narrow { ((self.fs / 5e6) as usize, &mut self.narrow) } else { ((self.fs / 25e6) as usize, &mut self.wide) };
        let fc = (w / 2.0 + if narrow { 0.2e6 } else { 0.5e6 }).min(acc.fs / 2.0 * 0.9);
        let h = lowpass(8 * dec + 1, fc, self.fs, 1.0);
        let x = &self.buf[s0 - self.buf0..s1 - self.buf0];
        let a = -2.0 * std::f64::consts::PI * f_off / self.fs;
        let (sa, ca) = a.sin_cos();
        let (mut pr, mut pi) = (1.0f64, 0.0f64);
        let mixed: Vec<[f64; 2]> = x
            .iter()
            .map(|v| {
                let (xr, xi) = (v[0] as f64, v[1] as f64);
                let o = [xr * pr - xi * pi, xr * pi + xi * pr];
                let t = pr * ca - pi * sa;
                pi = pr * sa + pi * ca;
                pr = t;
                o
            })
            .collect();
        let mut y: Vec<Cf> = vec![];
        let mut n = h.len();
        while n < x.len() {
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for (k, &hk) in h.iter().enumerate() {
                let v = mixed[n - k];
                re += hk * v[0];
                im += hk * v[1];
            }
            y.push([re as f32, im as f32]);
            n += dec;
        }
        if y.len() < 64 {
            return None;
        }
        let m = (y.len() + acc.n).next_power_of_two();
        let fft = Fft::new(m);
        let mut z = vec![[0f32; 2]; m];
        z[..y.len()].copy_from_slice(&y);
        fft.run(&mut z, false);
        for v in z.iter_mut() {
            *v = [v[0] * v[0] + v[1] * v[1], 0.0];
        }
        fft.run(&mut z, true);
        let r0 = z[0][0].max(1e-30) as f64;
        let nl = acc.n.min(y.len());
        let r2: Vec<f64> = (0..nl)
            .map(|l| {
                let (re, im) = (z[l][0] as f64 / r0, z[l][1] as f64 / r0);
                re * re + im * im
            })
            .collect();
        // Class accumulators: complete narrow bursts and wide ones.
        if narrow || w >= 7e6 {
            for (l, v) in r2.iter().enumerate() {
                acc.acc[l] += v;
            }
            acc.bursts += 1;
        }
        let fs_d = acc.fs;
        let li = |us: f64| (us * 1e-6 * fs_d).round() as usize;
        let near = |us: f64| {
            let c = li(us);
            (c.saturating_sub(1)..=(c + 1).min(nl.saturating_sub(1))).map(|k| r2.get(k).copied().unwrap_or(0.0)).fold(0.0, f64::max)
        };
        let lo = li(2.0).max(1);
        let hi = li(80.0).min(nl);
        let (mut bl, mut bv) = (0usize, 0.0f64);
        for (l, &v) in r2.iter().enumerate().take(hi).skip(lo) {
            if v > bv {
                bv = v;
                bl = l;
            }
        }
        let mut m: Vec<f64> = r2[lo..hi.max(lo + 1).min(nl)].to_vec();
        m.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let med = m.get(m.len() / 2).copied().unwrap_or(0.0);
        Some((bl as f64 / fs_d * 1e6, bv, near(100.0 / 3.0), near(200.0 / 3.0), med))
    }

    pub fn finish(&mut self) {
        let ts = std::mem::take(&mut self.tracks);
        for t in ts {
            self.close(t);
        }
        self.out.flush().unwrap();
    }

    /// The mean spectrum (dB over the median group), as (MHz, dB) per group.
    pub fn psd_db(&self) -> Vec<(f64, f32)> {
        let mut s: Vec<f64> = self.psd.clone();
        s.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let med = s[GROUPS / 2].max(1e-30);
        self.psd.iter().enumerate().map(|(g, &p)| (self.group_mhz(g as f64), db((p / med) as f32))).collect()
    }
}
