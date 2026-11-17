//! The C5 FPV job on one LO, one dwell at a time: the recording's 20 MHz
//! slice at 40 MS/s (float, from [`crate::dsp::Channel`]) is scaled to ADC
//! codes, given the C5's receiver noise, quantised by the real `rawcap`
//! clip window, probed by the real trigger and level meter, and the frozen
//! capture half-banded by the real `rawcap` filter, exactly as
//! `firmware/node-sdr/src/radio.rs` (`probe`, `poll`, `fpv_take`,
//! `fpv_judge`) and `radio/dji.rs` (`dji_judge`) do.

use droneid::math::{C16, remove_dc};
use rawcap::dsp::{HB_REACH, decimate_nibbles};
use rawcap::iq::{POWER, Window, pack, unpack};
use rawcap::trigger::{Config, Trigger};
use serde_json::{Value, json};

use crate::dsp::{Rng, halfband};

/// Receiver noise at gain 83, ADC codes RMS per axis (BRINGUP-RESULTS.md;
/// the djilink tests use the same).
pub const NOISE: f32 = 48.0;
/// Firmware geometry.
pub const CAPTURE_LEN: usize = 16_384;
pub const WINDOW_BYTES: usize = 2 * CAPTURE_LEN + 2 * HB_REACH;
const PRE_BYTES: usize = 4096;
const BLOCK: usize = 2048;
const PROBE: usize = 256;

/// One dwell's 40 MS/s float slice of one LO, already scaled to ADC codes.
pub struct DwellIn<'a> {
    pub re: &'a [f32],
    pub im: &'a [f32],
}

/// Quantise to PARL_IO bytes with the C5's noise.
pub fn quantise(d: &DwellIn, rng: &mut Rng) -> Vec<u8> {
    let w = Window::CLIP;
    let tab = noise_table();
    let mut k = (rng.next() as usize) % tab.len();
    let step = 1;
    d.re.iter()
        .zip(d.im)
        .map(|(&i, &q)| {
            let (ni, nq) = tab[k];
            k += step;
            if k >= tab.len() {
                k -= tab.len();
            }
            let i = (i + NOISE * ni).floor().clamp(-512.0, 511.0) as i16;
            let q = (q + NOISE * nq).floor().clamp(-512.0, 511.0) as i16;
            pack(w.quantise(i), w.quantise(q))
        })
        .collect()
}

/// Gaussian pairs, read sequentially from a random start per dwell (the
/// C5's noise needs to be white and Gaussian over a capture, not
/// unrepeatable; the table is 105 ms long at 40 MS/s).
fn noise_table() -> &'static [(f32, f32)] {
    static T: std::sync::OnceLock<Vec<(f32, f32)>> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let mut r = Rng(0xC5C5);
        (0..4_194_301).map(|_| r.g2()).collect()
    })
}

/// The probes of a dwell: (power, power without the probe's DC), as `probe`.
pub fn probes(bytes: &[u8]) -> Vec<(u32, u32)> {
    bytes
        .chunks_exact(BLOCK)
        .map(|b| {
            let mut p = 0u32;
            let (mut si, mut sq) = (0i32, 0i32);
            for &x in &b[..PROBE] {
                p += POWER[x as usize] as u32;
                let (i, q) = unpack(x);
                si += 2 * i as i32 + 1;
                sq += 2 * q as i32 + 1;
            }
            let dc = ((si * si + sq * sq) as u32) / PROBE as u32;
            (p, p.saturating_sub(dc).max(PROBE as u32))
        })
        .collect()
}

/// The noise floor a sweep would learn (quietest channels: pure receiver
/// noise), probe units x10 dB, as `FloorTracker::update` on a quiet dwell.
pub fn noise_floor(seed: u64) -> i32 {
    let n = 2_400_000;
    let z = vec![0f32; n];
    let mut rng = Rng(seed);
    let b = quantise(&DwellIn { re: &z, im: &z }, &mut rng);
    let mut m = fpvscan::Meter::new();
    for (_, p) in probes(&b) {
        m.feed(p);
    }
    let mut f = fpvscan::FloorTracker::new();
    f.update(&m)
}

fn c16_of(bytes: &[u8], start: usize) -> Vec<C16> {
    let mut cap = vec![C16::default(); CAPTURE_LEN];
    decimate_nibbles(|k| bytes[start + k], &mut cap);
    remove_dc(&mut cap);
    cap
}

/// The clean path: the same slice without the C5's noise or quantisation,
/// half-banded in float, in the decimator's output units (one 64-code step
/// of the clip window = 2048), DC removed.
fn clean_of(d: &DwellIn, start: usize) -> Vec<C16> {
    let n = WINDOW_BYTES;
    let h = halfband(&d.re[start..start + n], &d.im[start..start + n]);
    let mut out: Vec<C16> = h
        .iter()
        .take(CAPTURE_LEN)
        .map(|v| C16 { re: (v[0] * 32.0).round().clamp(-32768.0, 32767.0) as i16, im: (v[1] * 32.0).round().clamp(-32768.0, 32767.0) as i16 })
        .collect();
    remove_dc(&mut out);
    out
}

pub fn features_json(f: &djilink::Features) -> Value {
    let c = &f.cp;
    let fold = |x: &djilink::cp::Fold| json!([x.main, x.ctl, x.metric, x.thr, x.cp_samples, x.aligned as u8, x.detected as u8]);
    json!({
        "n15": fold(&c.normal15), "e15": fold(&c.extended15), "n30": fold(&c.normal30),
        "w": [f.spec.w3_khz, f.spec.w10_khz, f.spec.w20_khz], "edges": [f.spec.lo_khz, f.spec.hi_khz],
        "c": f.spec.centre_khz, "clip": [f.spec.clip_lo as u8, f.spec.clip_hi as u8], "snr": f.spec.snr_x10, "seg": f.spec.segments,
        "on": f.bursts.on_pct(), "cont": f.bursts.continuous as u8, "contrast": f.bursts.contrast_x10,
        "b": f.bursts.iter().map(|b| json!([b.len_us(), b.complete() as u8, b.centre_khz, b.width_khz])).collect::<Vec<_>>(),
    })
}

fn tl_json(t: &djilink::TimelineStats) -> Value {
    json!([t.probes, t.duty_pct, t.bursts, t.burst_us, t.period_us, t.longest_us])
}

pub struct Lo {
    pub lo_mhz: u16,
    /// The sweep channel this LO serves (carrier at lo + 1), MHz.
    pub ch_mhz: u16,
    pub rng: Rng,
    pub fpv: Box<fpvscan::Scratch>,
    pub dji: Box<djilink::Scratch>,
}

impl Lo {
    pub fn new(lo_mhz: u16, ch_mhz: u16, seed: u64) -> Lo {
        Lo { lo_mhz, ch_mhz, rng: Rng(seed), fpv: Box::new(fpvscan::Scratch::new()), dji: Box::new(djilink::Scratch::new()) }
    }

    /// One 60 ms dwell. Returns the firmware's dwell line and, every
    /// `stride`-th capture-length window of the dwell, a window line (C5
    /// path and clean path features and verdicts).
    pub fn dwell(&mut self, d: &DwellIn, floor: i32, t_ms: f64, stride: usize) -> (Value, Vec<Value>, Vec<u8>) {
        let bytes = quantise(d, &mut self.rng);
        let (line, wins) = self.judge(&bytes, Some(d), floor, t_ms, stride);
        (line, wins, bytes)
    }

    /// The firmware's dwell logic on quantised bytes (`clean`: the float
    /// slice they came from, for the clean-path window statistics).
    pub fn judge(&mut self, bytes: &[u8], clean: Option<&DwellIn>, floor: i32, t_ms: f64, stride: usize) -> (Value, Vec<Value>) {
        let pr = probes(bytes);
        let mut meter = fpvscan::Meter::new();
        let mut tl = djilink::Timeline::new();
        let mut trig = Trigger::new(Config { gate: false, ..Config::default() });
        let mut rise = None;
        for (k, &(p, pac)) in pr.iter().enumerate() {
            meter.feed(pac);
            tl.feed(pac);
            let r = trig.feed((k * BLOCK) as u64, p);
            if rise.is_none() {
                rise = r;
            }
        }
        // Capture: at the first rise (minus the pre-trigger), or the newest
        // window when nothing rose.
        let cap_start = match rise {
            Some(r) => (r.pos as usize).saturating_sub(PRE_BYTES).min(bytes.len() - WINDOW_BYTES),
            None => bytes.len() - WINDOW_BYTES,
        };
        let dw = meter.dwell(floor);
        let tls = tl.stats(floor);
        let loud = tl.loud_x10(floor);
        let bursty = loud >= fpvscan::classify::STRONG_DB as i32 * 10;
        let analysed = dw.snr_db >= fpvscan::classify::STRONG_DB || bursty;
        let x = c16_of(bytes, cap_start);
        let (a, extra) = if analysed {
            let a = fpvscan::analyse(&x, &mut self.fpv);
            let e = fpvscan::band::extra(&x, &self.fpv, &a, self.ch_mhz);
            (Some(a), e)
        } else {
            (None, fpvscan::band::Extra::default())
        };
        let kind = fpvscan::band::classify(&dw, a.as_ref(), &extra, self.ch_mhz);
        let ctx = djilink::Context { lo_khz: self.lo_mhz as u32 * 1000, timeline: Some(tls) };
        let mut dji = Value::Null;
        if analysed && !matches!(kind, fpvscan::Kind::AnalogVideo | fpvscan::Kind::Hdzero) {
            let f = djilink::features(&x, &mut self.dji);
            let v = djilink::classify(&f, &ctx);
            let centre = djilink::classify::centre_khz(&f, &ctx);
            // As firmware/node-sdr radio/dji.rs: every controller-shaped
            // burst feeds the hop tracker, whatever the verdict.
            let hops: Vec<u32> = djilink::classify::controller_bursts(&f).filter_map(|b| b.centre_khz.map(|c| (ctx.lo_khz as i64 + c as i64) as u32)).collect();
            dji = json!({"v": v.map(|v| v.name()).unwrap_or("none"), "centre_khz": centre, "hops": hops, "f": features_json(&f)});
        }
        let line = json!({
            "t": "dwell", "t_ms": t_ms, "lo": self.lo_mhz, "ch": self.ch_mhz,
            "snr_db": dw.snr_db, "duty": dw.duty_pct, "loud_x10": loud, "floor": floor,
            "rise": rise.map(|r| r.pos), "kind": kind.name(),
            "fpv": a.map(|a| json!({"cp": [a.cp_wifi, a.cp_half, a.cp_lte], "cp_thr": fpvscan::classify::cp_threshold(&a), "line": a.line, "line_hz": a.line_hz, "width_khz": a.width_khz, "flat_x10": a.flat_x10, "centroid_khz": a.centroid_khz})),
            "tl": tl_json(&tls), "dji": dji,
        });
        // Every window of the dwell (stride), both paths, for statistics.
        let mut wins = vec![];
        if stride > 0 {
            let n = (bytes.len() - 2 * HB_REACH) / (2 * CAPTURE_LEN);
            for w in (0..n).step_by(stride) {
                let s = w * 2 * CAPTURE_LEN;
                let x = c16_of(bytes, s);
                let f = djilink::features(&x, &mut self.dji);
                let v = djilink::classify(&f, &ctx);
                let mut w = json!({
                    "t": "win", "t_ms": t_ms + s as f64 / 40e3, "lo": self.lo_mhz,
                    "v": v.map(|v| v.name()).unwrap_or("none"), "f": features_json(&f),
                });
                if let Some(d) = clean {
                    let xc = clean_of(d, s);
                    let fc = djilink::features(&xc, &mut self.dji);
                    let vc = djilink::classify(&fc, &djilink::Context { timeline: None, ..ctx });
                    w["vc"] = json!(vc.map(|v| v.name()).unwrap_or("none"));
                    w["fc"] = features_json(&fc);
                }
                wins.push(w);
            }
        }
        (line, wins)
    }
}
