//! A coarse spectrogram of the newest part of the 40 MS/s ring.
//!
//! 64-point FFTs (625 kHz bins, 1.6 µs a frame, Hann window, integer
//! arithmetic) over two sets of frames:
//!
//! - **A**: [`FRAMES_A`] back-to-back frames at the newest end (102 µs):
//!   fast sweeps (a carrier moving 3.75 MHz or more in 102 µs, up to 5 MHz
//!   a frame: about 3.7e4 to 3.1e6 MHz/s).
//! - **B**: [`FRAMES_B`] frames spread evenly over the rest of the window
//!   (about 22 µs apart in a 1.4 ms window): slow sweeps (about 2.7e3 to
//!   2.3e5 MHz/s) and the time structure of pulses.
//!
//! From the average over all frames: how much of the receiver's passband is
//! occupied ([`Features::occupancy_pct`]: a jammer or a wideband link fills
//! it, a narrow signal does not) and whether one bin holds a persistent tone.
//! From the per-frame peaks: monotonic carrier movement (a sweep). From the
//! loud frames only (12 dB above the median frame): the pulses' occupancy.
//! From the samples: the fraction on the 4-bit rails (overload).
//!
//! The passband is the analog low-pass's (about +-10 MHz, phy `LPF_DEFAULT`)
//! flat part, [`PASS`] bins either side of DC, without the [`DC`] bins
//! (receiver DC residual).

/// FFT size, sample rate and bin width.
pub const NFFT: usize = 64;
pub const FS: u32 = 40_000_000;
pub const BIN_KHZ: i32 = (FS / 1000 / NFFT as u32) as i32;
/// Bins (signed offset from DC) used: `DC < |o| <= PASS`, i.e. 1.25 to
/// 8.75 MHz either side: 26 bins.
pub const PASS: i32 = 14;
pub const DC: i32 = 1;
const NPASS: usize = 2 * (PASS - DC) as usize;
/// Width the passband covers, kHz (what a hop "sees").
pub const PASS_KHZ: u32 = (2 * PASS + 1) as u32 * BIN_KHZ as u32;

pub const FRAMES_A: usize = 64;
pub const FRAMES_B: usize = 64;
pub const FRAMES: usize = FRAMES_A + FRAMES_B;
/// Shortest window [`analyse`] accepts.
pub const MIN_LEN: usize = 2 * FRAMES_A * NFFT;

/// A frame is a "carrier" frame when its strongest passband bin is this far
/// (tenths of a dB) above the frame's median passband bin.
pub const CARRIER_PTM_X10: i32 = 100;
/// Loud frames: this far above the 10th-percentile frame power (tenths of
/// a dB).
pub const LOUD_FRAME_X10: i32 = 120;
/// Occupied bin: no more than this below the passband's mean bin.
pub const OCC_X10: i32 = 60;
/// Tone: concentration (percent of passband power within +-2 bins of the
/// peak), peak over median (tenths of a dB), persistence (percent of frames
/// whose own peak is within +-1 bin).
pub const TONE_CONC_PCT: u8 = 50;
pub const TONE_PTM_X10: i32 = 150;
pub const TONE_PERSIST_PCT: u8 = 70;
/// Sweep: a monotonic run of carrier frames spanning at least this many
/// bins, with at least this many frames, steps of at most `SWEEP_MAX_STEP`
/// bins (one bin back allowed for jitter), and a straight-line fit whose
/// RMS residual is at most `SWEEP_RESID_Q8` / 256 bins.
pub const SWEEP_MIN_BINS: i32 = 6;
pub const SWEEP_MIN_FRAMES: usize = 5;
pub const SWEEP_MAX_STEP: i32 = 8;
pub const SWEEP_RESID_Q8: i32 = 384;

/// sin(2 pi k / 64) for k = 0..=16, Q15.
const SIN: [i32; 17] = [
    0, 3212, 6393, 9512, 12539, 15446, 18204, 20787, 23170, 25329, 27245, 28898, 30273, 31356, 32137, 32609, 32767,
];

/// (cos, sin) of 2 pi m / 64, Q15, m in 0..64.
fn cs(m: usize) -> (i32, i32) {
    let m = m & 63;
    let q = m / 16;
    let r = m % 16;
    let (c, s) = (SIN[16 - r], SIN[r]);
    match q {
        0 => (c, s),
        1 => (-s, c),
        2 => (-c, -s),
        _ => (s, -c),
    }
}

/// Hann window, Q15.
fn hann(n: usize) -> i32 {
    (32767 - cs(n).0) / 2
}

/// In-place forward FFT, 64 points.
fn fft64(re: &mut [i32; NFFT], im: &mut [i32; NFFT]) {
    for i in 0..NFFT {
        let j = ((i as u8).reverse_bits() >> 2) as usize;
        if j > i {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= NFFT {
        let half = len / 2;
        let step = NFFT / len;
        let mut start = 0;
        while start < NFFT {
            for k in 0..half {
                let (c, s) = cs(k * step);
                let (a, b) = (start + k, start + k + half);
                let (xr, xi) = (re[b] as i64, im[b] as i64);
                // (xr + j xi)(c - j s)
                let tr = ((xr * c as i64 + xi * s as i64) >> 15) as i32;
                let ti = ((xi * c as i64 - xr * s as i64) >> 15) as i32;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
            start += len;
        }
        len *= 2;
    }
}

/// Signed bin offset of FFT output `k`.
const fn offset(k: usize) -> i32 {
    if k < NFFT / 2 { k as i32 } else { k as i32 - NFFT as i32 }
}

/// FFT output index of signed offset `o`.
const fn index(o: i32) -> usize {
    (if o < 0 { o + NFFT as i32 } else { o }) as usize
}

/// The passband offsets in frequency order.
const PASS_OFFSETS: [i32; NPASS] = {
    let mut t = [0i32; NPASS];
    let mut n = 0;
    let mut o = -PASS;
    while o <= PASS {
        if o < -DC || o > DC {
            t[n] = o;
            n += 1;
        }
        o += 1;
    }
    t
};

/// Working memory (about 2.7 KiB).
pub struct Scratch {
    avg: [u64; NFFT],
    loud: [u64; NFFT],
    fpow: [u64; FRAMES],
    peak: [i8; FRAMES],
    carrier: [bool; FRAMES],
    re: [i32; NFFT],
    im: [i32; NFFT],
    pw: [u64; NFFT],
    tmp: [u64; NPASS],
}

impl Default for Scratch {
    fn default() -> Self {
        Self::new()
    }
}

impl Scratch {
    pub const fn new() -> Self {
        Scratch {
            avg: [0; NFFT],
            loud: [0; NFFT],
            fpow: [0; FRAMES],
            peak: [0; FRAMES],
            carrier: [false; FRAMES],
            re: [0; NFFT],
            im: [0; NFFT],
            pw: [0; NFFT],
            tmp: [0; NPASS],
        }
    }

    /// The averaged spectrum of the last analysis, by signed bin offset
    /// (diagnostics, tests).
    pub fn avg(&self, o: i32) -> u64 {
        self.avg[index(o)]
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tone {
    /// Offset from the LO, kHz.
    pub offset_khz: i32,
    pub conc_pct: u8,
    pub ptm_x10: i32,
    pub persist_pct: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sweep {
    /// MHz per second (magnitude).
    pub rate_mhz_s: u32,
    pub rising: bool,
    /// Span the carrier was seen to cover, kHz, and its centre from the LO.
    pub span_khz: u32,
    pub centre_khz: i32,
    pub frames: u16,
    /// Monotonic runs found that qualified (a sawtooth repeats).
    pub runs: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Features {
    pub frames: u16,
    /// Percent of passband bins within [`OCC_X10`] of the 75th-percentile
    /// bin of the averaged spectrum.
    pub occupancy_pct: u8,
    /// Frames at least [`LOUD_FRAME_X10`] above the quiet frames, and the
    /// occupancy of their own averaged spectrum (0 when none).
    pub loud_frames: u16,
    pub loud_occupancy_pct: u8,
    /// Sample index (into the analysed window) of the loudest frame.
    pub loudest_at: usize,
    /// Samples with either axis at full scale, parts per million.
    pub clip_ppm: u32,
    /// Carrier frames (see [`CARRIER_PTM_X10`]), percent.
    pub carrier_pct: u8,
    pub tone: Option<Tone>,
    pub sweep: Option<Sweep>,
}

impl Features {
    /// Occupancy of the loud part: the loud frames' own spectrum, or, when
    /// the whole window sat inside one pulse (no quiet frames to stand out
    /// from, but the samples clip), the whole window's. `None`: the window
    /// caught no pulse.
    pub fn pulse_occupancy(&self) -> Option<u8> {
        if self.loud_frames > 0 {
            Some(self.loud_occupancy_pct)
        } else if self.clip_ppm >= crate::watch::CLIP_PPM {
            Some(self.occupancy_pct)
        } else {
            None
        }
    }

    /// Where a link check of a pulse should look (sample index), if the
    /// window caught one.
    pub fn pulse_at(&self, len: usize) -> Option<usize> {
        if self.loud_frames > 0 {
            Some(self.loudest_at)
        } else {
            self.pulse_occupancy().map(|_| len)
        }
    }
}

/// Percent of passband bins of `spec` no more than [`OCC_X10`] below the
/// passband's mean bin. Flat noise (a barrage, the receiver's own) fills
/// it; a narrow signal pulls the mean up and leaves most bins far below.
fn occupancy(spec: &[u64; NFFT]) -> u8 {
    let tot: u64 = PASS_OFFSETS.iter().map(|&o| spec[index(o)]).sum();
    if tot == 0 {
        return 0;
    }
    // Within 6 dB of the mean: v * 4 * NPASS >= tot (OCC_X10 = 60).
    const _: () = assert!(OCC_X10 == 60);
    let n = PASS_OFFSETS.iter().filter(|&&o| spec[index(o)] * 4 * NPASS as u64 >= tot).count();
    (n * 100 / NPASS) as u8
}

/// Median passband bin of `pw`.
fn median_pass(pw: &[u64; NFFT], tmp: &mut [u64; NPASS]) -> u64 {
    for (t, &o) in tmp.iter_mut().zip(PASS_OFFSETS.iter()) {
        *t = pw[index(o)];
    }
    let k = NPASS / 2;
    tmp.select_nth_unstable(k);
    tmp[k]
}

/// `10 log10(a / b)` in tenths of a dB.
fn ratio_x10(a: u64, b: u64) -> i32 {
    fpvscan::db_x10(a.max(1)) - fpvscan::db_x10(b.max(1))
}

/// Spectrogram features of `len` samples. `get(k)` returns sample `k` as
/// integers whose full scale is `full_scale` (the 4-bit ring as `2n + 1`:
/// -15..=15, full scale 15). Returns `None` for a window shorter than
/// [`MIN_LEN`].
pub fn analyse(get: impl Fn(usize) -> (i8, i8), len: usize, full_scale: i8, s: &mut Scratch) -> Option<Features> {
    if len < MIN_LEN {
        return None;
    }
    s.avg = [0; NFFT];
    s.loud = [0; NFFT];
    let a_start = len - FRAMES_A * NFFT;
    let b_span = a_start - NFFT;
    let b_step = b_span / (FRAMES_B - 1);
    let start = |f: usize| if f < FRAMES_B { f * b_step } else { a_start + (f - FRAMES_B) * NFFT };
    let fs = full_scale as i32;
    let mut clipped = 0u32;
    // Pass 1: spectra, frame powers, peaks.
    for f in 0..FRAMES {
        let at = start(f);
        for n in 0..NFFT {
            let (i, q) = get(at + n);
            let (i, q) = (i as i32, q as i32);
            clipped += (i.abs() >= fs || q.abs() >= fs) as u32;
            let w = hann(n);
            // |v| <= 128 * 32767 >> 7: the 64-point sum stays far inside i32.
            s.re[n] = (i * w) >> 7;
            s.im[n] = (q * w) >> 7;
        }
        fft64(&mut s.re, &mut s.im);
        let mut tot = 0u64;
        let (mut pk, mut pko) = (0u64, 0i32);
        for k in 0..NFFT {
            let p = (s.re[k] as i64 * s.re[k] as i64 + s.im[k] as i64 * s.im[k] as i64) as u64;
            s.pw[k] = p;
            s.avg[k] += p;
            let o = offset(k);
            if (DC < o.abs()) && o.abs() <= PASS {
                tot += p;
                if p > pk {
                    pk = p;
                    pko = o;
                }
            }
        }
        s.fpow[f] = tot;
        s.peak[f] = pko as i8;
        let med = median_pass(&s.pw, &mut s.tmp);
        s.carrier[f] = ratio_x10(pk, med) >= CARRIER_PTM_X10;
    }
    // Loud frames: a second FFT pass over just those (cheap: usually none).
    let mut fp = s.fpow;
    // Against the quiet frames (10th percentile), so that a pulse longer
    // than half the window still stands out.
    fp.select_nth_unstable(FRAMES / 10);
    let med_frame = fp[FRAMES / 10];
    let (mut loud_frames, mut loudest, mut loudest_p) = (0u16, 0usize, 0u64);
    for f in 0..FRAMES {
        if s.fpow[f] > loudest_p {
            loudest_p = s.fpow[f];
            loudest = start(f);
        }
        if ratio_x10(s.fpow[f], med_frame) >= LOUD_FRAME_X10 {
            loud_frames += 1;
            let at = start(f);
            for n in 0..NFFT {
                let (i, q) = get(at + n);
                let w = hann(n);
                s.re[n] = (i as i32 * w) >> 7;
                s.im[n] = (q as i32 * w) >> 7;
            }
            fft64(&mut s.re, &mut s.im);
            for k in 0..NFFT {
                s.loud[k] += (s.re[k] as i64 * s.re[k] as i64 + s.im[k] as i64 * s.im[k] as i64) as u64;
            }
        }
    }
    let occupancy_pct = occupancy(&s.avg);
    let loud_occupancy_pct = if loud_frames > 0 { occupancy(&s.loud) } else { 0 };
    let carrier_pct = (s.carrier.iter().filter(|&&c| c).count() * 100 / FRAMES) as u8;

    // Tone: the averaged spectrum's passband peak.
    let tone = {
        let (mut pk, mut pko) = (0u64, 0i32);
        let mut tot = 0u64;
        for &o in &PASS_OFFSETS {
            let v = s.avg[index(o)];
            tot += v;
            if v > pk {
                pk = v;
                pko = o;
            }
        }
        let near: u64 = PASS_OFFSETS.iter().filter(|&&o| (o - pko).abs() <= 2).map(|&o| s.avg[index(o)]).sum();
        let conc = (near * 100).checked_div(tot).unwrap_or(0) as u8;
        let ptm = ratio_x10(pk, median_pass(&s.avg, &mut s.tmp));
        let persist = (s.peak.iter().filter(|&&p| (p as i32 - pko).abs() <= 1).count() * 100 / FRAMES) as u8;
        (conc >= TONE_CONC_PCT && ptm >= TONE_PTM_X10 && persist >= TONE_PERSIST_PCT).then_some(Tone {
            offset_khz: pko * BIN_KHZ,
            conc_pct: conc,
            ptm_x10: ptm,
            persist_pct: persist,
        })
    };

    // Sweep: the best monotonic run in either set of frames.
    let sb = sweep_in(&s.peak[..FRAMES_B], &s.carrier[..FRAMES_B], b_step);
    let sa = sweep_in(&s.peak[FRAMES_B..], &s.carrier[FRAMES_B..], NFFT);
    let runs = sa.map_or(0, |x| x.runs).saturating_add(sb.map_or(0, |x| x.runs));
    let sweep = match (sa, sb) {
        (Some(a), Some(b)) => Some(if a.span_khz >= b.span_khz { a } else { b }),
        (a, b) => a.or(b),
    }
    .map(|x| Sweep { runs, ..x });

    Some(Features {
        frames: FRAMES as u16,
        occupancy_pct,
        loud_frames,
        loud_occupancy_pct,
        loudest_at: loudest,
        clip_ppm: (clipped as u64 * 1_000_000 / (FRAMES * NFFT) as u64) as u32,
        carrier_pct,
        tone,
        sweep,
    })
}

/// The longest qualifying monotonic run of carrier frames in `peak`
/// (frames `spacing` samples apart), with the count of qualifying runs.
fn sweep_in(peak: &[i8], carrier: &[bool], spacing: usize) -> Option<Sweep> {
    let n = peak.len();
    let mut best: Option<Sweep> = None;
    let mut runs = 0u8;
    let mut k = 0;
    while k < n {
        if !carrier[k] {
            k += 1;
            continue;
        }
        // Grow a run from k.
        let s0 = k;
        let mut dir = 0i32;
        let mut e = k + 1;
        while e < n && carrier[e] {
            let d = peak[e] as i32 - peak[e - 1] as i32;
            if d.abs() > SWEEP_MAX_STEP {
                break;
            }
            if dir == 0 {
                if d != 0 {
                    dir = d.signum();
                }
            } else if d * dir < -1 {
                break;
            }
            e += 1;
        }
        if let Some(sw) = fit(&peak[s0..e], spacing) {
            runs = runs.saturating_add(1);
            if best.is_none_or(|b| sw.span_khz > b.span_khz) {
                best = Some(sw);
            }
        }
        // A sawtooth's wrap starts the next run at the frame that broke this one.
        k = if e > s0 + 1 { e } else { s0 + 1 };
    }
    best.map(|b| Sweep { runs, ..b })
}

/// Straight-line fit of a run of peak positions; a [`Sweep`] if it moves
/// far and straight enough.
fn fit(p: &[i8], spacing: usize) -> Option<Sweep> {
    let m = p.len();
    if m < SWEEP_MIN_FRAMES || spacing == 0 {
        return None;
    }
    let (lo, hi) = (*p.iter().min()? as i32, *p.iter().max()? as i32);
    if hi - lo < SWEEP_MIN_BINS {
        return None;
    }
    // Least squares in Q8 integers: x = frame index, y = bin.
    let mi = m as i64;
    let sx: i64 = (0..mi).sum();
    let sy: i64 = p.iter().map(|&v| v as i64).sum();
    let sxx: i64 = (0..mi).map(|x| x * x).sum();
    let sxy: i64 = p.iter().enumerate().map(|(x, &y)| x as i64 * y as i64).sum();
    let den = mi * sxx - sx * sx;
    if den == 0 {
        return None;
    }
    let slope_q8 = (mi * sxy - sx * sy) * 256 / den;
    let icpt_q8 = (sy * 256 - slope_q8 * sx) / mi;
    let rss: i64 = p
        .iter()
        .enumerate()
        .map(|(x, &y)| {
            let r = y as i64 * 256 - (icpt_q8 + slope_q8 * x as i64);
            r * r
        })
        .sum();
    // RMS residual <= SWEEP_RESID_Q8 / 256 bins.
    if rss > (SWEEP_RESID_Q8 as i64 * SWEEP_RESID_Q8 as i64) * mi {
        return None;
    }
    // The fit must agree with the span: a straight run, not a jump.
    if slope_q8.abs() * (mi - 1) < (SWEEP_MIN_BINS as i64 - 1) * 256 {
        return None;
    }
    // bins/frame -> MHz/s: slope * BIN_KHZ kHz per (spacing / FS) s.
    let rate = slope_q8.unsigned_abs() * BIN_KHZ as u64 * FS as u64 / 256 / spacing as u64 / 1000;
    Some(Sweep {
        rate_mhz_s: rate as u32,
        rising: slope_q8 > 0,
        span_khz: ((hi - lo) * BIN_KHZ) as u32,
        centre_khz: (hi + lo) * BIN_KHZ / 2,
        frames: m as u16,
        runs: 1,
    })
}
