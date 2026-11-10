//! Burst detection, synchronisation, OFDM demodulation and decoding.
//!
//! Stages, in order (each is integer-only per sample; once-per-burst values
//! such as the frequency estimate use `f64` scalars):
//!
//! 1. [`find_bursts`]: 16 µs window energies at the input rate; a threshold
//!    a few standard deviations above a low percentile (the noise floor);
//!    runs lasting a DroneID burst become [`Span`]s.
//! 2. Coarse frequency: averaged 1024-point power spectra over the span at
//!    the input rate; the 9 MHz window holding the most power gives the
//!    burst centre to about one FFT bin.
//! 3. Remove the span's DC offset, mix by the coarse offset and resample
//!    the span (plus 40 µs each side) to 15.36 MS/s.
//! 4. Cyclic-prefix sync: running correlations of every CP with the end of
//!    its symbol, summed over all symbols for each candidate start, give the
//!    burst timing and the fractional frequency offset (within +-7.5 kHz).
//!    Comparing the 9-symbol and 8-symbol hypotheses, plus the energy where
//!    the 9-symbol layout's extra first symbol would be, picks the layout.
//! 5. Zadoff-Chu: root 600 confirms the burst (a differential correlation
//!    that does not care about whole-carrier offsets). Whole-carrier offset
//!    is then the shift at which the root-600 and root-147 channel estimates
//!    agree, which no other shift can fake because the two roots turn a
//!    carrier offset into very different time shifts. Root 600's phase slope
//!    across carriers then gives fine timing.
//! 6. Channel: least-squares estimates from both reference symbols; one
//!    linear phase slope across carriers (timing) estimated from both and
//!    removed, then each one's common phase; the two are averaged and
//!    smoothed across carriers into a channel shape. For each data symbol the
//!    common phase is interpolated (or extrapolated) in time between the two
//!    references, which tracks residual frequency offset. There are no
//!    pilots, so nothing else tracks phase.
//! 7. Per data symbol: FFT, `y * conj(h)` (the max-log LLR up to scale, with
//!    channel-strength weighting for free), scaled to `i8`.
//! 8. Descramble, de-rate-match, then decode: (a) hard decisions on the
//!    combined systematic soft values + CRC-24A; (b) if that fails, the turbo
//!    decoder. Then the inner CRC-16 and the record.

use crate::fft::{self, N as NFFT, fft};
use crate::math::{C16, C32, C64, Nco, angle_rad, cis, db10, hz_to_step, isqrt64};
use crate::payload::{self, BLOCK, Parsed};
use crate::ratematch::derate;
use crate::resample::{FS_OUT, RateError, Resampler};
use crate::turbo::{Turbo, crc_ok};
use crate::zc::{NCAR, ROOT_A, ROOT_B, carrier, zc};
use crate::{D, E, K, scramble};

/// Subcarrier spacing.
pub const SCS_HZ: f64 = 15_000.0;
/// Resampled burst buffer (about 867 µs at 15.36 MS/s).
pub const BUF_LEN: usize = 13_312;
/// Most energy windows [`find_bursts`] looks at in one call (16.4 ms).
pub const MAX_WINDOWS: usize = 1024;
/// CP samples correlated per symbol (the shorter CP length).
const CP: usize = 72;
/// How far into the cyclic prefix FFT windows start, after fine timing.
const BACKOFF: i32 = 8;
/// Back-off used before fine timing is known.
const BACKOFF_COARSE: i32 = 16;
/// Whole-carrier offsets searched each side of the coarse estimate.
const DMAX: i32 = 12;

/// The two burst layouts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// 9 symbols, 643 µs: CP 80, 72 x 7, 80. References at 3 and 5; symbol 0
    /// carries no turbo data. OcuSync 2 (Mini 2, Air 2 and others).
    Nine,
    /// 8 symbols, 572 µs: CP 80, 72 x 6, 80. References at 2 and 4.
    /// "Legacy" (Mavic Pro, Mavic 2) and, per the O4 spec, O4.
    Eight,
}

const U9: [usize; 9] = [80, 1176, 2272, 3368, 4464, 5560, 6656, 7752, 8856];
const U8: [usize; 8] = [80, 1176, 2272, 3368, 4464, 5560, 6656, 7760];

impl Layout {
    /// Offsets of each symbol's FFT window from the start of the burst
    /// (first CP sample).
    pub fn useful_starts(self) -> &'static [usize] {
        match self {
            Layout::Nine => &U9,
            Layout::Eight => &U8,
        }
    }
    /// Burst length in samples at 15.36 MS/s.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(self) -> usize {
        self.useful_starts().last().unwrap() + NFFT
    }
    /// Symbol indices of the root-600 and root-147 references.
    pub fn refs(self) -> (usize, usize) {
        match self {
            Layout::Nine => (3, 5),
            Layout::Eight => (2, 4),
        }
    }
    /// The six data symbols, in transmission order.
    pub fn data(self) -> [usize; 6] {
        match self {
            Layout::Nine => [1, 2, 4, 6, 7, 8],
            Layout::Eight => [0, 1, 3, 5, 6, 7],
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Layout::Nine => "9-symbol",
            Layout::Eight => "8-symbol",
        }
    }
}

/// A stretch of input that looks like a burst, in input samples.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

/// Find burst-length energy runs in `x` (sample rate `fs_in`). Looks at most
/// [`MAX_WINDOWS`] 16 µs windows; callers split longer input into
/// overlapping chunks. Returns how many spans were written.
pub fn find_bursts(x: &[C16], fs_in: u32, out: &mut [Span]) -> usize {
    let w = (fs_in / 62_500) as usize;
    if w == 0 {
        return 0;
    }
    let nw = (x.len() / w).min(MAX_WINDOWS);
    if nw < 8 {
        return 0;
    }
    let mut pw = [0u32; MAX_WINDOWS];
    for (i, p) in pw.iter_mut().enumerate().take(nw) {
        let s: u64 = x[i * w..(i + 1) * w].iter().map(|c| c.norm2() as u64).sum();
        *p = (s / w as u64) as u32;
    }
    // Noise floor: a low percentile, which stays on the noise whether bursts
    // fill 1% of the input (a long recording) or 90% (back-to-back captures).
    let mut sorted = [0u32; MAX_WINDOWS];
    sorted[..nw].copy_from_slice(&pw[..nw]);
    let (_, lo, _) = sorted[..nw].select_nth_unstable(nw * 3 / 100);
    let lo = (*lo).max(1) as u64;
    // A window's power estimate scatters by about 1/sqrt(W) of the mean, so
    // 6/sqrt(W) above the floor is far outside what noise does for 30
    // windows in a row, yet only ~1 dB at 50 MS/s.
    let stat = lo + lo * 6000 / (isqrt64(w as u64 * 1_000_000).max(1));
    // Coarsely quantised captures (4-bit) can read as zero between bursts,
    // so also never sit more than 15 dB under the loudest window.
    let peak = pw[..nw].iter().copied().max().unwrap_or(0) as u64;
    let thr = stat.max(peak / 32).min(u32::MAX as u64) as u32;
    // Burst durations, in windows: 480..780 µs.
    let (min_w, max_w) = (30, 48);
    let mut count = 0;
    let mut i = 0;
    while i < nw && count < out.len() {
        if pw[i] <= thr {
            i += 1;
            continue;
        }
        let a = i;
        let mut b = i;
        // Extend, bridging single quiet windows.
        while b < nw && (pw[b] > thr || (b + 1 < nw && pw[b + 1] > thr && b > a)) {
            b += 1;
        }
        let len = b - a;
        if (min_w..=max_w).contains(&len) && a > 0 && b < nw {
            out[count] = Span { start: a * w, end: b * w };
            count += 1;
        }
        i = b;
    }
    count
}

/// Decoder settings.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Full turbo iterations at most; 0 disables path (b).
    pub max_iterations: u8,
    /// Try path (a), systematic bits only, first. Turning it off forces
    /// every burst through the turbo decoder (for testing path (b)).
    pub try_systematic: bool,
    /// Channel smoothing half-width across carriers.
    pub smooth: u8,
    /// Minimum root-600 correlation (x1000) to accept a burst.
    pub zc_threshold: u16,
    /// Minimum normalised CP correlation (x1000) to accept a burst.
    pub cp_threshold: u16,
    /// 1024-point spectra averaged for the coarse frequency estimate.
    pub spectrum_blocks: u8,
    /// Burst centre frequency relative to the capture centre, when known
    /// (a C5 parked on a DroneID channel knows it to within its crystal
    /// error); skips the spectral search. Must be within about +-100 kHz.
    pub cfo_hint_hz: Option<i32>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_iterations: 8,
            try_systematic: true,
            smooth: 6,
            zc_threshold: 150,
            cp_threshold: 100,
            spectrum_blocks: 8,
            cfo_hint_hz: None,
        }
    }
}

/// Which decode path produced the block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Path {
    /// (a) systematic bits only.
    Systematic,
    /// (b) the turbo decoder.
    Turbo,
}

/// Why a span was not demodulated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncError {
    Rate(RateError),
    /// Span too short or too close to the end of the input.
    TooShort,
    /// No cyclic-prefix structure.
    NoCp,
    /// No root-600 reference symbol.
    NoZc,
}

/// Measurements and outcome for one burst.
#[derive(Clone, Debug)]
pub struct Burst {
    pub span: Span,
    pub layout: Layout,
    /// Burst start in input samples (first CP sample), from fine timing.
    pub start: f64,
    /// Total frequency offset of the burst centre from the input's centre.
    pub cfo_hz: f64,
    /// Signal-to-noise ratio per carrier from the reference symbols, dB.
    pub snr_db: f32,
    /// Normalised CP correlation x1000.
    pub cp_metric: u16,
    /// Root-600 / root-147 agreement metrics x1000.
    pub zc_metric: u16,
    pub zc_pair_metric: u16,
    /// Mean |LLR| of the channel soft values (out of 127).
    pub mean_llr: u8,
    pub path: Option<Path>,
    pub crc24_ok: bool,
    /// Turbo half-iterations used (0 if the turbo decoder did not run).
    pub half_iterations: u8,
    /// Systematic hard decisions that disagree with the final block (a
    /// raw channel bit-error count; meaningful when `crc24_ok`).
    pub systematic_errors: u16,
    /// The 173 decoded bytes (best effort when CRC fails).
    pub block: [u8; BLOCK],
    pub parsed: Option<Parsed>,
}

/// Points at which [`Receiver::decode_span`] reports progress (for timing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    CoarseCfo,
    Resample,
    CpSync,
    ZcSync,
    Demod,
    Derate,
    Systematic,
    Turbo,
}

/// The whole receive chain and its working memory (about 125 KiB; put it
/// in a `static` on a microcontroller).
pub struct Receiver {
    rs: Resampler,
    pub config: Config,
    buf: [C16; BUF_LEN],
    n: usize,
    fftbuf: [C16; NFFT],
    chan: [C32; NCAR],
    llr: [i8; E],
    soft: [[i16; D]; 3],
    turbo: Turbo,
}

impl Receiver {
    pub fn new(fs_in: u32) -> Result<Self, RateError> {
        Ok(Self {
            rs: Resampler::new(fs_in)?,
            config: Config::default(),
            buf: [C16::default(); BUF_LEN],
            n: 0,
            fftbuf: [C16::default(); NFFT],
            chan: [C32::ZERO; NCAR],
            llr: [0; E],
            soft: [[0; D]; 3],
            turbo: Turbo::new(),
        })
    }

    /// Build a receiver directly in `slot`, never on the stack.
    ///
    /// [`Receiver::new`] returns about 125 KiB by value, which a chip's stack
    /// cannot hold. This zeroes `slot` (every buffer starts at zero, exactly
    /// as `new` leaves it) and then writes the two fields that are not all
    /// zero: the resampler (about 9 KiB, built on the stack once) and the
    /// configuration. `slot` may be a `static`, a PSRAM region or anything
    /// else the caller owns.
    pub fn init_in_place(
        slot: &mut core::mem::MaybeUninit<Receiver>,
        fs_in: u32,
    ) -> Result<&mut Receiver, RateError> {
        let rs = Resampler::new(fs_in)?;
        let p = slot.as_mut_ptr();
        // SAFETY: apart from `rs` and `config`, every field is an integer, an
        // array of integers or integer complex types, or a `Turbo` made only
        // of integer arrays, so all-zero bytes are a valid value of each.
        // `rs` and `config` are written with `write` (no drop of the zeroed
        // bytes) before the reference is created.
        unsafe {
            core::ptr::write_bytes(p, 0, 1);
            core::ptr::addr_of_mut!((*p).rs).write(rs);
            core::ptr::addr_of_mut!((*p).config).write(Config::default());
            Ok(&mut *p)
        }
    }

    pub fn fs_in(&self) -> u32 {
        self.rs.fs_in
    }

    /// The descrambled channel soft values of the last burst (diagnostics).
    pub fn llr(&self) -> &[i8; E] {
        &self.llr
    }

    /// Find and decode every burst in `x`.
    pub fn decode_all(&mut self, x: &[C16], out: &mut dyn FnMut(Result<Burst, (Span, SyncError)>)) {
        let mut spans = [Span::default(); 24];
        let n = find_bursts(x, self.rs.fs_in, &mut spans);
        for &s in &spans[..n] {
            out(self.decode_span(x, s, &mut |_| {}).map_err(|e| (s, e)));
        }
    }

    /// Demodulate and decode the burst in `span` of `x`. `mark` is called
    /// after each stage.
    pub fn decode_span(&mut self, x: &[C16], span: Span, mark: &mut dyn FnMut(Stage)) -> Result<Burst, SyncError> {
        let fs_in = self.rs.fs_in as f64;
        if span.end <= span.start + 2048 || span.end > x.len() {
            return Err(SyncError::TooShort);
        }
        // 2. Coarse frequency.
        let f_coarse = match self.config.cfo_hint_hz {
            Some(h) => h as f64,
            None => coarse_cfo(x, span, self.rs.fs_in, self.config.spectrum_blocks, &mut self.fftbuf),
        };
        mark(Stage::CoarseCfo);

        // 3. Mix and resample.
        let margin = self.rs.fs_in as usize / 25_000;
        let a = span.start.saturating_sub(margin);
        let b = (span.end + margin).min(x.len());
        let want = (((b - a) as u64 * self.rs.l as u64) / self.rs.m as u64) as usize;
        let want = want.min(BUF_LEN);
        // DC offset (LO leakage, or the half-LSB bias of truncating bit
        // slicing) would otherwise sit inside the band as a strong tone.
        let dc = {
            let (mut re, mut im) = (0i64, 0i64);
            for v in &x[a..b] {
                re += v.re as i64;
                im += v.im as i64;
            }
            let n = (b - a).max(1) as i64;
            C16 { re: (re / n) as i16, im: (im / n) as i16 }
        };
        self.n = self.rs.run(x, a, hz_to_step(-f_coarse, fs_in), dc, &mut self.buf[..want]);
        normalise(&mut self.buf[..self.n]);
        mark(Stage::Resample);

        // 4. Cyclic-prefix timing, layout, fractional frequency.
        let cp = cp_sync(&self.buf[..self.n]).ok_or(SyncError::NoCp)?;
        if cp.metric < self.config.cp_threshold {
            return Err(SyncError::NoCp);
        }
        let layout = cp.layout;
        let f_frac = angle_rad(cp.angle) / (2.0 * core::f64::consts::PI) * SCS_HZ;
        rotate(&mut self.buf[..self.n], hz_to_step(-f_frac, FS_OUT as f64));
        mark(Stage::CpSync);

        // 5. Reference symbols: presence, whole-carrier offset, fine timing.
        let (ia, ib) = layout.refs();
        let us = layout.useful_starts();
        let mut t = cp.t;
        let win = |t: i32, s: usize, back: i32| t + us[s] as i32 - back;
        fft_at(&self.buf[..self.n], win(t, ia, BACKOFF_COARSE), &mut self.fftbuf);
        let mut ya = [C16::default(); NFFT];
        ya.copy_from_slice(&self.fftbuf);
        let zc_metric = zc_presence(&ya);
        if zc_metric < self.config.zc_threshold {
            return Err(SyncError::NoZc);
        }
        fft_at(&self.buf[..self.n], win(t, ib, BACKOFF_COARSE), &mut self.fftbuf);
        let (d, zc_pair_metric) = whole_carrier_offset(&ya, &self.fftbuf);
        let f_int = d as f64 * SCS_HZ;
        if d != 0 {
            rotate(&mut self.buf[..self.n], hz_to_step(-f_int, FS_OUT as f64));
        }
        // Fine timing from the root-600 phase slope.
        fft_at(&self.buf[..self.n], win(t, ia, BACKOFF_COARSE), &mut self.fftbuf);
        let tau = delay_from_slope(&self.fftbuf, ROOT_A);
        t += crate::math::round_i32(tau) - BACKOFF_COARSE;
        mark(Stage::ZcSync);

        // 6. Channel estimate from both references.
        let wa = win(t, ia, BACKOFF);
        let wb = win(t, ib, BACKOFF);
        let ea = fft_at(&self.buf[..self.n], wa, &mut self.fftbuf);
        let ha = ls_estimate(&self.fftbuf, ROOT_A);
        let eb = fft_at(&self.buf[..self.n], wb, &mut self.fftbuf);
        let hb = ls_estimate(&self.fftbuf, ROOT_B);
        let e_ref = ea.max(eb);
        let model = channel_model(&ha, ea - e_ref, &hb, eb - e_ref, self.config.smooth, &mut self.chan);

        // 7. Data symbols.
        let mut sum_abs = 0u64;
        for (j, &s) in layout.data().iter().enumerate() {
            let w = win(t, s, BACKOFF);
            let e = fft_at(&self.buf[..self.n], w, &mut self.fftbuf);
            let frac = (w - wa) as i64 * 65536 / (wb - wa) as i64;
            let phase = model.phase_a.wrapping_add(((model.dphase as i64 * frac) >> 16) as u32);
            sum_abs += demap(
                &self.fftbuf,
                e - e_ref,
                &self.chan,
                phase,
                model.slope,
                model.llr_recip,
                model.llr_shift,
                &mut self.llr[j * 2 * NCAR..(j + 1) * 2 * NCAR],
            );
        }
        let mean_llr = (sum_abs / E as u64).min(255) as u8;
        mark(Stage::Demod);

        // 8. Descramble, de-rate-match, decode.
        scramble::descramble(&mut self.llr);
        derate(&self.llr, &mut self.soft);
        mark(Stage::Derate);
        let mut full = [0u8; K / 8];
        for i in 0..K {
            if self.soft[0][i] < 0 {
                full[i / 8] |= 0x80 >> (i % 8);
            }
        }
        let systematic_hard = full;
        let mut path = None;
        let mut half_iterations = 0;
        if self.config.try_systematic && crc_ok(&full) {
            path = Some(Path::Systematic);
        }
        mark(Stage::Systematic);
        if path.is_none() && self.config.max_iterations > 0 {
            let r = self.turbo.decode(&self.soft, self.config.max_iterations, &mut full);
            half_iterations = r.half_iterations;
            if r.crc_ok {
                path = Some(Path::Turbo);
            }
            mark(Stage::Turbo);
        }
        let systematic_errors =
            systematic_hard.iter().zip(&full).map(|(a, b)| (a ^ b).count_ones() as u16).sum::<u16>();
        let mut block = [0u8; BLOCK];
        block.copy_from_slice(&full[..BLOCK]);
        let crc24_ok = path.is_some();
        let parsed = Some(payload::parse(&block));

        let in_per_out = self.rs.m as f64 / self.rs.l as f64;
        Ok(Burst {
            span,
            layout,
            start: a as f64 + (t as f64) * in_per_out - self.rs.delay_in(),
            cfo_hz: f_coarse + f_frac + f_int,
            snr_db: model.snr_db,
            cp_metric: cp.metric,
            zc_metric,
            zc_pair_metric,
            mean_llr,
            path,
            crc24_ok,
            half_iterations,
            systematic_errors,
            block,
            parsed,
        })
    }
}

/// Scale the buffer so its RMS sits near 2048 (peaks of an OFDM burst
/// rarely exceed 4x RMS), by a power of two.
fn normalise(buf: &mut [C16]) {
    if buf.is_empty() {
        return;
    }
    let p: u64 = buf.iter().map(|c| c.norm2() as u64).sum::<u64>() / buf.len() as u64;
    let target = 2048u64 * 2048 * 2;
    let mut sh = 0i32;
    let mut q = p.max(1);
    while q * 4 <= target && sh < 12 {
        q *= 4;
        sh += 1;
    }
    while q > target * 4 && sh > -12 {
        q /= 4;
        sh -= 1;
    }
    if sh == 0 {
        return;
    }
    for c in buf.iter_mut() {
        let (r, i) = (c.re as i32, c.im as i32);
        let (r, i) = if sh > 0 { (r << sh, i << sh) } else { (r >> -sh, i >> -sh) };
        *c = C16 { re: crate::math::sat16(r), im: crate::math::sat16(i) };
    }
}

/// Multiply the buffer by `exp(j step n)` in place.
fn rotate(buf: &mut [C16], step: u32) {
    let mut nco = Nco { phase: 0, step };
    for c in buf.iter_mut() {
        *c = c.mul_q15(nco.tick());
    }
}

/// FFT of `buf[start..start + 1024]` (zeros outside) into `out`; returns the
/// block exponent.
fn fft_at(buf: &[C16], start: i32, out: &mut [C16; NFFT]) -> i32 {
    for (i, o) in out.iter_mut().enumerate() {
        let j = start + i as i32;
        *o = if j >= 0 && (j as usize) < buf.len() { buf[j as usize] } else { C16::default() };
    }
    fft(out, false)
}

/// Stage 2: centre frequency of the strongest 9 MHz-wide band during the
/// span, from averaged power spectra at the input rate.
fn coarse_cfo(x: &[C16], span: Span, fs_in: u32, blocks: u8, fftbuf: &mut [C16; NFFT]) -> f64 {
    let len = span.end - span.start;
    let nblk = (len / NFFT).clamp(1, blocks.max(1) as usize);
    let stride = if nblk > 1 { (len - NFFT) / (nblk - 1) } else { 0 };
    let mut p = [0u64; NFFT];
    let mut e_acc = i32::MIN;
    for blk in 0..nblk {
        let s = span.start + blk * stride;
        for (i, o) in fftbuf.iter_mut().enumerate() {
            *o = x.get(s + i).copied().unwrap_or_default();
        }
        let e = fft(fftbuf, false);
        if e > e_acc {
            if e_acc != i32::MIN {
                let sh = (2 * (e - e_acc)).min(63) as u32;
                for v in p.iter_mut() {
                    *v >>= sh;
                }
            }
            e_acc = e;
        }
        let sh = (2 * (e_acc - e)).min(63) as u32;
        for (v, c) in p.iter_mut().zip(fftbuf.iter()) {
            *v += (c.norm2() as u64) >> sh;
        }
    }
    // Remove a DC spike (LO leakage) by interpolation.
    p[0] = (p[1] + p[NFFT - 1]) / 2;
    let bin_hz = fs_in as f64 / NFFT as f64;
    let wb = ((9.0e6 / bin_hz) + 0.5) as usize;
    let wb = wb.clamp(1, NFFT - 1);
    // Circular sliding sum; window for centre c covers c - wb/2 .. c - wb/2 + wb.
    let half = wb / 2;
    let mut s: u64 = (0..wb).map(|j| p[(j + NFFT - half) % NFFT]).sum();
    let mut sums = [0u64; NFFT];
    for (c, out) in sums.iter_mut().enumerate() {
        *out = s;
        let leave = (c + NFFT - half) % NFFT;
        let enter = (c + NFFT - half + wb) % NFFT;
        s = s - p[leave] + p[enter];
    }
    let (best, &smax) = sums.iter().enumerate().max_by_key(|&(_, v)| *v).unwrap();
    // Centre of the near-flat top around the maximum.
    let near = smax - smax / 64;
    let mut lo = 0i32;
    while lo > -(NFFT as i32 / 2) && sums[(best as i32 + lo - 1).rem_euclid(NFFT as i32) as usize] >= near {
        lo -= 1;
    }
    let mut hi = 0i32;
    while hi < NFFT as i32 / 2 && sums[(best as i32 + hi + 1).rem_euclid(NFFT as i32) as usize] >= near {
        hi += 1;
    }
    let c = best as f64 + (lo + hi) as f64 / 2.0;
    let c = if c >= NFFT as f64 / 2.0 { c - NFFT as f64 } else { c };
    c * bin_hz
}

struct CpResult {
    /// Burst start (first CP sample) in the 15.36 MS/s buffer.
    t: i32,
    layout: Layout,
    angle: u32,
    metric: u16,
}

/// Cheap magnitude: max + 3/8 min (within 7%), used only to rank.
fn mag_approx(c: C64) -> u64 {
    let (a, b) = (c.re.unsigned_abs(), c.im.unsigned_abs());
    let (mx, mn) = if a > b { (a, b) } else { (b, a) };
    mx + ((mn * 3) >> 3)
}

/// Stage 4: CP correlation summed over every symbol of a 9-symbol burst
/// starting at `t`; the last eight symbols are also an 8-symbol burst
/// starting at `t + 1096`.
fn cp_sync(r: &[C16]) -> Option<CpResult> {
    let n = r.len() as i32;
    let prod = |i: i32| -> C64 {
        if i < 0 || i + NFFT as i32 >= n {
            return C64::ZERO;
        }
        let p = r[(i + NFFT as i32) as usize].mul_conj(r[i as usize]);
        C64 { re: p.re as i64, im: p.im as i64 }
    };
    let pow = |i: i32| -> u64 {
        if i < 0 || i + NFFT as i32 >= n {
            return 0;
        }
        (r[i as usize].norm2() as u64 + r[(i + NFFT as i32) as usize].norm2() as u64) / 2
    };
    let t_min = -(U9[1] as i32 - U9[0] as i32);
    let t_max = n - Layout::Nine.len() as i32;
    if t_max < t_min {
        return None;
    }
    let mut sums = [C64::ZERO; 9];
    for s in 0..9 {
        let a = t_min + U9[s] as i32 - CP as i32;
        for m in 0..CP as i32 {
            sums[s] = sums[s].wadd(prod(a + m));
        }
    }
    let mut best9 = (0u64, t_min);
    let mut best8 = (0u64, t_min);
    let mut t = t_min;
    loop {
        let m8 = sums[1..].iter().fold(C64::ZERO, |a, &b| a.wadd(b));
        let m9 = m8.wadd(sums[0]);
        let (a9, a8) = (mag_approx(m9), mag_approx(m8));
        if a9 > best9.0 {
            best9 = (a9, t);
        }
        if a8 > best8.0 {
            best8 = (a8, t + (U9[1] - U9[0]) as i32);
        }
        if t == t_max {
            break;
        }
        for s in 0..9 {
            let a = t + U9[s] as i32 - CP as i32;
            sums[s] = sums[s].wadd(prod(a + CP as i32)).wsub(prod(a));
        }
        t += 1;
    }
    // Layout. The CP maxima alone are ambiguous (eight of nine symbols of a
    // 9-symbol burst form a perfect 8-symbol burst), so check each candidate
    // against the burst's energy envelope: every symbol must be loud and a
    // guard just before and just after must be quiet.
    let shift = (U9[1] - U9[0]) as i32;
    let cands = [
        (Layout::Nine, best9.1),
        (Layout::Eight, best8.1),
        (Layout::Nine, best8.1 - shift),
        (Layout::Eight, best9.1 + shift),
    ];
    let mean_pow = |a: i32, b: i32| -> Option<u64> {
        (a >= 0 && b <= n && b > a)
            .then(|| r[a as usize..b as usize].iter().map(|c| c.norm2() as u64).sum::<u64>() / (b - a) as u64)
    };
    const GUARD: i32 = 400;
    let score = |layout: Layout, t: i32| -> (u64, u64) {
        let mut min_sym = u64::MAX;
        for &u in layout.useful_starts() {
            let p = mean_pow(t + u as i32, t + u as i32 + NFFT as i32).unwrap_or(0);
            min_sym = min_sym.min(p);
        }
        let end = t + layout.len() as i32;
        let g = [mean_pow(t - GUARD, t), mean_pow(end, end + GUARD)];
        let guard = g.iter().flatten().copied().max().unwrap_or(0).max(1);
        // Ratio x 256, and the CP magnitude as a tie-break.
        (min_sym.saturating_mul(256) / guard, 0)
    };
    let mut best = (cands[0], 0u64);
    for &c in &cands {
        let (sc, _) = score(c.0, c.1);
        if sc > best.1 {
            best = (c, sc);
        }
    }
    let (layout, t) = best.0;
    // Exact metric at the chosen timing.
    let mut acc = C64::ZERO;
    let mut p = 0u64;
    for &u in layout.useful_starts() {
        for k in 0..CP as i32 {
            acc = acc.wadd(prod(t + u as i32 - CP as i32 + k));
            p += pow(t + u as i32 - CP as i32 + k);
        }
    }
    let metric = (acc.abs() * 1000 / p.max(1)).min(1000) as u16;
    Some(CpResult { t, layout, angle: acc.angle(), metric })
}

/// Root-600 presence: differential correlation of adjacent carriers against
/// the reference, normalised (x1000). Whole-carrier offsets only rotate it.
fn zc_presence(y: &[C16; NFFT]) -> u16 {
    let mut acc = C64::ZERO;
    let mut norm = 0u64;
    for m in 0..NCAR - 1 {
        let (k0, k1) = (carrier(m), carrier(m + 1));
        if k1 - k0 != 1 {
            continue;
        }
        let (a, b) = (y[fft::bin(k0)], y[fft::bin(k1)]);
        let d = a.mul_conj(b);
        // conj(z(k0)) z(k1), Q15.
        let r = zc(ROOT_A, k1).mul_conj(zc(ROOT_A, k0)).shift(15);
        let p = C64 {
            re: d.re as i64 * r.re as i64 - d.im as i64 * r.im as i64,
            im: d.re as i64 * r.im as i64 + d.im as i64 * r.re as i64,
        };
        acc = acc.wadd(p);
        norm += (a.norm2() as u64 + b.norm2() as u64) / 2;
    }
    ((acc.abs() >> 15) * 1000 / norm.max(1)).min(1000) as u16
}

/// Whole-carrier frequency offset: the shift `d` at which the channel seen
/// through root 600 and through root 147 agree. Returns `(d, metric x1000)`.
fn whole_carrier_offset(ya: &[C16; NFFT], yb: &[C16; NFFT]) -> (i32, u16) {
    // conj(zA) zB per carrier, Q15, computed once.
    let mut refp = [C32::ZERO; NCAR];
    for (m, r) in refp.iter_mut().enumerate() {
        let k = carrier(m);
        *r = zc(ROOT_B, k).mul_conj(zc(ROOT_A, k)).shift(15);
    }
    let mut best = (0i32, 0u64, 1u64);
    for d in -DMAX..=DMAX {
        let mut acc = C64::ZERO;
        let mut norm = 0u64;
        for (m, r) in refp.iter().enumerate() {
            let k = carrier(m);
            let (a, b) = (ya[fft::bin(k + d)], yb[fft::bin(k + d)]);
            let ab = a.mul_conj(b);
            acc = acc.wadd(C64 {
                re: ab.re as i64 * r.re as i64 - ab.im as i64 * r.im as i64,
                im: ab.re as i64 * r.im as i64 + ab.im as i64 * r.re as i64,
            });
            norm += (a.norm2() as u64 + b.norm2() as u64) / 2;
        }
        let v = acc.abs() >> 15;
        // Compare v / norm across d without division.
        if (v as u128) * (best.2 as u128) > (best.1 as u128) * (norm.max(1) as u128) {
            best = (d, v, norm.max(1));
        }
    }
    (best.0, (best.1 * 1000 / best.2).min(1000) as u16)
}

/// Delay (samples, may be fractional) of the reference symbol inside the
/// FFT window, from the phase slope of `y * conj(z)` across carriers.
fn delay_from_slope(y: &[C16; NFFT], root: u32) -> f64 {
    let mut acc = C64::ZERO;
    let mut prev: Option<(i32, C32)> = None;
    for m in 0..NCAR {
        let k = carrier(m);
        let h = y[fft::bin(k)].mul_q15(C16 { re: zc(root, k).re, im: -zc(root, k).im }).to32();
        if let Some((kp, hp)) = prev
            && k - kp == 1
        {
            // h[k-1] * conj(h[k]) has angle +2 pi tau / N.
            acc = acc.wadd(hp.mul_conj64(h));
        }
        prev = Some((k, h));
    }
    angle_rad(acc.angle()) * NFFT as f64 / (2.0 * core::f64::consts::PI)
}

/// `y * conj(z)` on every carrier, as 32-bit values.
fn ls_estimate(y: &[C16; NFFT], root: u32) -> [C32; NCAR] {
    let mut h = [C32::ZERO; NCAR];
    for (m, hm) in h.iter_mut().enumerate() {
        let k = carrier(m);
        let z = zc(root, k);
        *hm = y[fft::bin(k)].mul_q15(C16 { re: z.re, im: z.im.saturating_neg() }).to32();
    }
    h
}

struct ChannelModel {
    /// Common phase at reference A.
    phase_a: u32,
    /// Its change from reference A to reference B (residual frequency
    /// offset and drift), interpolated over time.
    dphase: i32,
    /// Phase slope across carriers (binary angle per carrier).
    slope: u32,
    llr_recip: i64,
    llr_shift: u32,
    snr_db: f32,
}

/// Phase slope per carrier of two estimates together: angle of the sum of
/// h[k+1] conj(h[k]).
fn slope_of(h1: &[C32; NCAR], h2: &[C32; NCAR]) -> u32 {
    let mut acc = C64::ZERO;
    for h in [h1, h2] {
        for m in 0..NCAR - 1 {
            if carrier(m + 1) - carrier(m) == 1 {
                acc = acc.wadd(h[m + 1].mul_conj64(h[m]));
            }
        }
    }
    acc.angle()
}

/// Remove a phase slope (per carrier) and return the common phase left.
fn flatten(h: &mut [C32; NCAR], slope: u32) -> u32 {
    let mut acc = C64::ZERO;
    for (m, v) in h.iter_mut().enumerate() {
        let a = slope.wrapping_mul(carrier(m) as u32).wrapping_neg();
        *v = v.rot_q15(cis(a));
        acc = acc.wadd(C64 { re: v.re as i64, im: v.im as i64 });
    }
    acc.angle()
}

/// Stage 6. `ha`, `hb` are least-squares estimates with block exponents
/// relative to the common reference. Leaves the smoothed, de-rotated
/// channel shape in `chan` and returns the time-varying phase model.
fn channel_model(ha: &[C32; NCAR], sa: i32, hb: &[C32; NCAR], sb: i32, smooth: u8, chan: &mut [C32; NCAR]) -> ChannelModel {
    let mut a = [C32::ZERO; NCAR];
    let mut b = [C32::ZERO; NCAR];
    for m in 0..NCAR {
        a[m] = ha[m].shift(-sa);
        b[m] = hb[m].shift(-sb);
    }
    // One slope for both references: the timing is common to them, and a
    // per-symbol estimate is too noisy (its error grows with carrier index).
    // Whatever residual ramp remains is common to both and stays in the
    // averaged channel shape below.
    let slope = slope_of(&a, &b);
    let pa = flatten(&mut a, slope);
    let pb = flatten(&mut b, slope);
    for v in a.iter_mut() {
        *v = v.rot_q15(cis(pa.wrapping_neg()));
    }
    for v in b.iter_mut() {
        *v = v.rot_q15(cis(pb.wrapping_neg()));
    }
    // Noise from the difference of the two references; signal from the mean.
    let (mut noise, mut sig) = (0u64, 0u64);
    for m in 0..NCAR {
        noise += a[m].wsub(b[m]).norm2() / 2;
        let s = a[m].wadd(b[m]);
        sig += s.norm2() / 4;
        chan[m] = C32 { re: s.re / 2, im: s.im / 2 };
    }
    // E|a - b|^2 / 2 is one estimate's noise power; |(a + b) / 2|^2 carries
    // half of it on top of the signal.
    let noise_per = noise as f64 / NCAR as f64;
    let sig_per = sig as f64 / NCAR as f64 - noise_per / 2.0;
    let snr_db = db10(sig_per.max(1e-9) / noise_per.max(1e-9));
    // Smooth across carriers (moving average, window shrinks at the edges).
    let w = smooth as usize;
    if w > 0 {
        let mut tmp = [C32::ZERO; NCAR];
        let mut acc = (0i64, 0i64);
        let mut lo = 0usize;
        let mut hi = 0usize; // exclusive
        for (m, t) in tmp.iter_mut().enumerate() {
            let want_hi = (m + w + 1).min(NCAR);
            let want_lo = m.saturating_sub(w);
            while hi < want_hi {
                acc.0 += chan[hi].re as i64;
                acc.1 += chan[hi].im as i64;
                hi += 1;
            }
            while lo < want_lo {
                acc.0 -= chan[lo].re as i64;
                acc.1 -= chan[lo].im as i64;
                lo += 1;
            }
            let n = (hi - lo) as i64;
            *t = C32 { re: (acc.0 / n) as i32, im: (acc.1 / n) as i32 };
        }
        *chan = tmp;
    }
    // LLR scale: a clean QPSK point at the reference power maps to about 32.
    let g2: u64 = chan.iter().map(|c| c.norm2()).sum::<u64>() / NCAR as u64;
    let mut sh = 0u32;
    while (g2 >> sh) >= (1 << 15) {
        sh += 1;
    }
    let g2s = (g2 >> sh).max(1) as i64;
    let llr_recip = (32i64 << 30) / g2s;
    ChannelModel {
        phase_a: pa,
        dphase: pb.wrapping_sub(pa) as i32,
        slope,
        llr_recip,
        llr_shift: sh,
        snr_db,
    }
}

/// Stage 7 for one symbol: equalise against `chan` rotated by `phase +
/// slope * k`, write 1200 soft values (I then Q per carrier, carriers from
/// -300 up). Returns the sum of |LLR|.
#[allow(clippy::too_many_arguments)]
fn demap(y: &[C16; NFFT], exp: i32, chan: &[C32; NCAR], phase: u32, slope: u32, recip: i64, shift: u32, out: &mut [i8]) -> u64 {
    let mut sum = 0u64;
    for m in 0..NCAR {
        let k = carrier(m);
        let h = chan[m].rot_q15(cis(phase.wrapping_add(slope.wrapping_mul(k as u32))));
        let v = y[fft::bin(k)].to32().shift(-exp);
        let z = v.mul_conj64(h);
        let conv = |x: i64| -> i8 {
            let x = (x >> shift).clamp(-(1 << 24), 1 << 24);
            ((x * recip) >> 30).clamp(-127, 127) as i8
        };
        let (li, lq) = (conv(z.re), conv(z.im));
        sum += li.unsigned_abs() as u64 + lq.unsigned_abs() as u64;
        out[2 * m] = li;
        out[2 * m + 1] = lq;
    }
    sum
}

#[cfg(test)]
mod size {
    #[test]
    fn receiver_memory_is_as_documented() {
        let n = core::mem::size_of::<super::Receiver>();
        assert!((120 * 1024..132 * 1024).contains(&n), "Receiver is {n} bytes");
    }
}
