//! DJI link fingerprints on the FPV job (PLAN.md Phase 11b, `crates/djilink`,
//! docs/DJI-LINKS.md). Receive-only, presence only: the capture is measured,
//! never demodulated.
//!
//! After `fpvscan` has judged a loud channel, the same frozen capture goes
//! through `djilink` unless `fpvscan` already recognised analog video or
//! HDZero. A DJI verdict replaces `fpvscan`'s report for that dwell:
//!
//! - DJI video / O4 video / FPV air unit: reported at once, at the measured
//!   centre when the whole signal was inside the capture;
//! - a controller-shaped burst: fed to the hop tracker; reported (at the
//!   burst's centre) only once bursts have appeared on two centres within
//!   3 s, and rate-limited as one emitter rather than per channel;
//! - a DroneID burst or other 15 kHz OFDM: the existing LTE-style kind.

use super::{Line, Radio, capture, now_us};
use core::fmt::Write as _;
use djilink::Verdict;
use link::RfKind;

/// `djilink`'s per-dwell state on the radio.
pub(super) struct Dji {
    scratch: Option<&'static mut djilink::Scratch>,
    pub timeline: djilink::Timeline,
    hops: djilink::HopTracker,
}

impl Dji {
    pub const fn new() -> Self {
        Dji { scratch: None, timeline: djilink::Timeline::new(), hops: djilink::HopTracker::new() }
    }
}

/// What `djilink` made of one dwell.
pub(super) struct Judged {
    pub verdict: Option<Verdict>,
    /// The report replacing `fpvscan`'s: kind, centre (100 kHz units),
    /// width (kHz), and whether it is rate-limited as one hopping emitter.
    pub report: Option<(RfKind, u32, u16, bool)>,
    f: djilink::Features,
    tl: djilink::TimelineStats,
    us: u32,
}

impl Judged {
    /// Diagnostic fields for the `fpv` line.
    pub fn log(&self, line: &mut Line) {
        let f = &self.f;
        let c = &f.cp.normal15;
        let _ = write!(
            line,
            r#","dji":"{}","dji_cp":[{},{},{}],"dji_ext":{},"dji_w":[{},{},{}],"dji_c":{},"dji_clip":{},"dji_bursts":{},"dji_on":{},"dji_long_us":{},"tl":[{},{},{},{}],"dji_us":{}"#,
            self.verdict.map(|v| v.name()).unwrap_or("none"),
            c.metric,
            c.thr,
            c.cp_samples,
            f.cp.extended15.metric - f.cp.extended15.thr,
            f.spec.w3_khz,
            f.spec.w10_khz,
            f.spec.w20_khz,
            f.spec.centre_khz,
            f.spec.clipped(),
            f.bursts.found,
            f.bursts.on_pct(),
            f.bursts.longest_us(),
            self.tl.bursts,
            self.tl.burst_us,
            self.tl.period_us,
            self.tl.duty_pct,
            self.us
        );
    }
}

impl Radio {
    pub fn set_dji_scratch(&mut self, s: &'static mut djilink::Scratch) {
        self.dji.scratch = Some(s);
    }

    /// One dwell's DJI check, after `fpvscan` said `kind` (`analysed`: the
    /// capture bank holds this dwell's loud window). Resets the timeline
    /// for the next dwell.
    pub(super) fn dji_judge(&mut self, kind: fpvscan::Kind, floor_x10: i32, analysed: bool) -> Option<Judged> {
        let tl = self.dji.timeline.stats(floor_x10);
        self.dji.timeline.reset();
        // `Quiet` with an analysis: the dwell was bursty-loud (see
        // `fpv_judge`), which `fpvscan` does not judge; this crate does.
        if !analysed || matches!(kind, fpvscan::Kind::AnalogVideo | fpvscan::Kind::Hdzero) {
            return None;
        }
        let sc = self.dji.scratch.as_deref_mut()?;
        let t0 = now_us();
        let f = djilink::features(&capture()[..self.c16_len], sc);
        let us = (now_us() - t0) as u32;
        let lo_khz = self.s.mhz as u32 * 1000;
        let ctx = djilink::Context { lo_khz, timeline: Some(tl) };
        let verdict = djilink::classify(&f, &ctx);
        let centre = djilink::classify::centre_khz(&f, &ctx);
        let freq = |k: Option<u32>, fallback: u32| k.map(|k| (k + 50) / 100).unwrap_or(fallback);
        let width = f.spec.w10_khz.min(u16::MAX as u32) as u16;
        let cur = self.cur_freq;
        let report = match verdict {
            Some(Verdict::Video) => Some((RfKind::DjiVideo, freq(centre, cur), width, false)),
            Some(Verdict::O4Video) => Some((RfKind::DjiO4Video, freq(centre, cur), width, false)),
            Some(Verdict::FpvAirUnit) => Some((RfKind::DjiFpvAirUnit, freq(centre, cur), width, false)),
            Some(Verdict::ControllerBurst) => {
                let now = (now_us() / 1000) as u32;
                let (mut confirmed, mut last, mut w) = (false, None, width);
                for b in f.bursts.iter() {
                    if let Some(c) = b.centre_khz {
                        let abs = (lo_khz as i64 + c as i64).max(0) as u32;
                        confirmed |= self.dji.hops.burst(abs, now);
                        last = Some(abs);
                        w = b.width_khz.min(u16::MAX as u32) as u16;
                    }
                }
                confirmed.then(|| (RfKind::DjiController, freq(last, cur), w, true))
            }
            Some(Verdict::DroneIdBurst | Verdict::Ofdm15) => Some((RfKind::LteStyle, freq(centre, cur), width, false)),
            Some(Verdict::Ofdm30) | None => None,
        };
        Some(Judged { verdict, report, f, tl, us })
    }
}
