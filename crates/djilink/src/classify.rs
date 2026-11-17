//! From one capture's features (and its dwell's timing) to a verdict.
//!
//! Every DJI verdict needs a DJI numerology first: 15 kHz OFDM with a normal
//! cyclic prefix (OcuSync 2/3, O3, DroneID:
//! [`crate::cp::CpReport::dji_numerology`]) or 30 kHz OFDM with a normal
//! prefix (O4: measured on the RFUAV Mini 4 Pro recordings, video and
//! controller uplink both; [`crate::cp::CpReport::o4_numerology`]). Wi-Fi
//! (3.2 µs symbols), analog FM video and carriers have neither, so they can
//! never become a DJI kind here. After that, shape decides:
//!
//! | Verdict | Numerology | Width (-10 dB) | Timing | Where |
//! |---|---|---|---|---|
//! | [`Verdict::ControllerBurst`] | 15 kHz | 0.6-3.0 MHz | a whole 0.2-0.8 ms burst, mostly off | anywhere |
//! | [`Verdict::ControllerBurst`] | 30 kHz | 2.0-8.7 MHz | a whole 0.2-0.8 ms burst, mostly off | anywhere |
//! | [`Verdict::DroneIdBurst`] | 15 kHz | 8-10.5 MHz (and >= 5 at -3 dB) | one whole 0.45-0.8 ms burst, dwell nearly silent | anywhere |
//! | [`Verdict::O4Video`] | 30 kHz | >= 7 MHz and >= 5.5 at -3 dB, or a burst >= 8.8 MHz | anything else | anywhere |
//! | [`Verdict::O4Video`] | 15 kHz | >= 7 MHz (and >= 5 at -3 dB) | anything else | 5.170-5.250 GHz |
//! | [`Verdict::Video`] | 15 kHz | >= 7 MHz | | centre on the x.5 MHz raster, or unknown (clipped, or wider than 12 MHz) |
//! | [`Verdict::FpvAirUnit`] | 15 kHz | 7-12 MHz | | a whole-MHz DJI FPV channel, off the x.5 raster (unconfirmed: the one DJI FPV system recorded, the FPV Combo, sat on 5770.5) |
//! | [`Verdict::Ofdm15`] | 15 kHz | anything else (a measured centre on neither raster: e.g. Autel), or extended prefix | | |
//!
//! [`Verdict::Ofdm30`] is 30 kHz OFDM of no DJI shape.

use crate::Features;
use crate::plan;
use crate::timeline::TimelineStats;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// DJI-style video downlink, generation not determined (an OcuSync
    /// drone, an O3/O4 air unit; Autel's LTE-based links look the same).
    Video,
    /// O4 video: 30 kHz OFDM of video width anywhere (measured, RFUAV
    /// Mini 4 Pro), or the 15 kHz shape on 5.170-5.250 GHz, which only O4
    /// products list for video (Autel EVO Lite/Max also file the band).
    O4Video,
    /// The same on a whole-MHz DJI FPV (V1/Vista) channel. Walksnail is
    /// said to share the channel plan and an LTE-style chip; unproven.
    FpvAirUnit,
    /// One narrow uplink-shaped burst (15 kHz OcuSync / O3, or 30 kHz O4);
    /// [`crate::HopTracker`] confirms the controller once bursts appear on
    /// different centres.
    ControllerBurst,
    /// A DroneID broadcast burst (the `droneid` receiver decodes those).
    DroneIdBurst,
    /// 15 kHz OFDM that fits none of the above (another width, or an
    /// extended prefix): the LTE-style family.
    Ofdm15,
    /// 30 kHz OFDM of no DJI shape.
    Ofdm30,
}

impl Verdict {
    pub const fn name(self) -> &'static str {
        match self {
            Verdict::Video => "dji_video",
            Verdict::O4Video => "dji_o4_video",
            Verdict::FpvAirUnit => "dji_fpv_air_unit",
            Verdict::ControllerBurst => "dji_controller_burst",
            Verdict::DroneIdBurst => "droneid_burst",
            Verdict::Ofdm15 => "ofdm15",
            Verdict::Ofdm30 => "ofdm30",
        }
    }
}

/// Where and how the capture was taken.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Context {
    /// The receiver's LO, kHz (absolute).
    pub lo_khz: u32,
    /// The dwell's burst timing, if the caller kept a timeline.
    pub timeline: Option<TimelineStats>,
}

/// Controller uplink width (kHz, -10 dB): OcuSync 1/2's 73 subcarriers
/// (1.1 MHz, filed 99% OBW 1.17-1.20 MHz) to O3's 145 (2.2 MHz, filed
/// 2.27-2.36 MHz), with margin.
pub const CTRL_WIDTH_KHZ: core::ops::RangeInclusive<u32> = 600..=3_000;
/// O4 controller uplink width (kHz, -10 dB), 30 kHz numerology: measured
/// 4.4 MHz on whole bursts in the C5 model (3.0 where the capture's edge
/// cuts one), 5.2 MHz mean span in the full-rate survey (RFUAV Mini 4 Pro,
/// both video modes): about 145 subcarriers at 30 kHz, O3's count at twice
/// the spacing.
/// A strong uplink burst in the 40 MHz video mode (RFUAV Avata 2, 6.3-6.7 MHz
/// mean span in the survey) reads up to 8.5 MHz at -10 dB in the C5 model.
pub const O4_CTRL_WIDTH_KHZ: core::ops::RangeInclusive<u32> = 2_000..=8_700;
/// O4 video, a burst's own -10 dB width: the 10 MHz mode reads 8.98-9.14 MHz
/// (Mini 4 Pro) and wider; an uplink burst at most 8.5 (above).
pub const O4_VIDEO_BURST_MIN_KHZ: u32 = 8_800;
/// O4 video, the capture's -3 dB width: 10 MHz mode video 6.1 MHz and up
/// (10th percentile, Avata 2; 8.3 median on the Mini 4 Pro), uplink-only
/// captures 2.3-5.0 MHz (Avata 2, 40 MHz mode).
pub const O4_VIDEO_MIN_W3_KHZ: u32 = 5_500;
/// Controller burst length (µs), a whole burst inside the capture: 6-7
/// symbols of 71.4 µs = 0.43-0.5 ms (OcuSync / O3, ICTC 2025); O4 measured
/// 0.40-0.45 ms (RFUAV Mini 4 Pro). The first guess allowed 1.3 ms; the
/// RFUAV EVO Nano's narrow 15 kHz bursts (Autel, ~1.7 MHz) are 1.31 ms, so
/// the ceiling is now 0.8 ms and cut bursts no longer count.
pub const CTRL_BURST_US: core::ops::RangeInclusive<u32> = 200..=800;
/// A controller is off most of the time in any one 20 MHz window.
pub const CTRL_MAX_ON_PCT: u8 = 95;
/// DroneID: 600 subcarriers (9.0 MHz), 8 or 9 symbols (0.57/0.64 ms).
pub const DRONEID_WIDTH_KHZ: core::ops::RangeInclusive<u32> = 8_000..=10_500;
pub const DRONEID_BURST_US: core::ops::RangeInclusive<u32> = 450..=800;
/// A dwell with DroneID alone is nearly silent (one burst per ~600 ms).
pub const DRONEID_MAX_DUTY: u8 = 5;
/// Video: the 10 MHz mode is 9.0-9.4 MHz (filed), 20 MHz 18 MHz.
pub const VIDEO_MIN_WIDTH_KHZ: u32 = 7_000;
/// ...and wide at -3 dB too: a weak narrow signal barely over the noise
/// reads wide at -10 dB (the noise is within 10 dB of it), not at -3 dB.
pub const WIDE_MIN_W3_KHZ: u32 = 5_000;

/// Widest signal (kHz, -10 dB) whose measured centre the raster tests use.
pub const RASTER_MAX_W10_KHZ: u32 = 12_000;

/// The absolute centre (kHz) of the capture's signal, if it was all inside
/// the capture.
pub fn centre_khz(f: &Features, c: &Context) -> Option<u32> {
    if f.spec.clipped() || f.spec.segments == 0 {
        return None;
    }
    Some((c.lo_khz as i64 + f.spec.centre_khz as i64).max(0) as u32)
}

/// Same channel (kHz) for [`same_channel_continues`]: centres measured near
/// the capture's edge are off by up to ~400 kHz (the EVO Nano at -8.6 MHz);
/// DJI's hops are at least 1 MHz apart.
pub const SAME_CHANNEL_KHZ: u32 = 500;

/// Whether another burst of the capture on `x`'s channel runs on past a
/// controller burst's length (cut by the capture, or longer than
/// [`CTRL_BURST_US`]): then `x` is part of a longer transmission, not a hop.
/// The RFUAV EVO Nano (Autel) sends a 0.33 ms burst and, 0.08 ms later on
/// the same centre, a 1.31 ms 15 kHz one; a capture that starts at the
/// first holds it whole and the second cut.
pub fn same_channel_continues(f: &Features, x: &crate::burst::Burst) -> bool {
    let Some(cx) = x.centre_khz else { return false };
    f.bursts.iter().any(|y| {
        !core::ptr::eq(x, y)
            && y.centre_khz.is_some_and(|cy| cy.abs_diff(cx) <= SAME_CHANNEL_KHZ)
            && (!y.complete() || y.len_us() > *CTRL_BURST_US.end())
    })
}

/// The bursts of a capture shaped like a controller uplink hop (the
/// numerology's uplink width, a whole 0.2-0.8 ms burst, own centre
/// measured, not continued on its channel), whatever the capture's verdict:
/// the caller feeds their centres to [`crate::HopTracker`]. Empty unless the
/// capture has a DJI numerology.
pub fn controller_bursts(f: &Features) -> impl Iterator<Item = &crate::burst::Burst> {
    let o4 = f.cp.o4_numerology();
    let any = o4 || f.cp.dji_numerology();
    let width = if o4 { O4_CTRL_WIDTH_KHZ } else { CTRL_WIDTH_KHZ };
    f.bursts.iter().filter(move |x| {
        any && x.centre_khz.is_some()
            && x.complete()
            && CTRL_BURST_US.contains(&x.len_us())
            && width.contains(&x.width_khz)
            && !same_channel_continues(f, x)
    })
}

pub fn classify(f: &Features, c: &Context) -> Option<Verdict> {
    let cp = &f.cp;
    let o4 = cp.o4_numerology();
    if !o4 && !cp.dji_numerology() {
        return if cp.normal15.detected || cp.extended15.detected {
            Some(Verdict::Ofdm15)
        } else {
            cp.normal30.detected.then_some(Verdict::Ofdm30)
        };
    }
    let w = f.spec.w10_khz;
    let b = &f.bursts;
    let ctrl_width = if o4 { O4_CTRL_WIDTH_KHZ } else { CTRL_WIDTH_KHZ };

    // Controller: a short burst of uplink width. The burst's own width
    // counts too, because two hops in one capture widen the averaged
    // spectrum. Not when the capture also holds a video-width burst: O4's
    // uplink and video share a 2.5 ms frame (uplink, then video 0.49 ms
    // later), so one capture often has both; video wins, and the uplink
    // burst still reaches the hop tracker ([`controller_bursts`]).
    let video_min = if o4 { O4_VIDEO_BURST_MIN_KHZ } else { VIDEO_MIN_WIDTH_KHZ };
    let video_burst = !b.continuous && b.iter().any(|x| x.width_khz >= video_min && x.len_us() >= 100);
    if !video_burst && !b.continuous && b.on_pct() <= CTRL_MAX_ON_PCT && c.timeline.is_none_or(|t| t.duty_pct <= 60) {
        let fits = b.iter().any(|x| {
            x.complete()
                && CTRL_BURST_US.contains(&x.len_us())
                && !same_channel_continues(f, x)
                && (ctrl_width.contains(&x.width_khz) || (x.width_khz == 0 && ctrl_width.contains(&w)))
        });
        let narrow = ctrl_width.contains(&w) || w <= 2 * ctrl_width.end();
        if fits && narrow {
            return Some(Verdict::ControllerBurst);
        }
    }

    let wide = f.spec.w3_khz >= WIDE_MIN_W3_KHZ;
    if o4 {
        // A video burst sharing the capture with a longer uplink burst
        // leaves the averaged spectrum narrow: its own width counts.
        let wide_o4 = w >= VIDEO_MIN_WIDTH_KHZ && f.spec.w3_khz >= O4_VIDEO_MIN_W3_KHZ;
        return Some(if wide_o4 || video_burst { Verdict::O4Video } else { Verdict::Ofdm30 });
    }
    if DRONEID_WIDTH_KHZ.contains(&w) && wide && !b.continuous {
        let whole: usize = b.iter().filter(|x| x.complete() && DRONEID_BURST_US.contains(&x.len_us())).count();
        let others = b.iter().filter(|x| x.len_us() > *DRONEID_BURST_US.end()).count();
        let quiet = c.timeline.is_none_or(|t| t.duty_pct <= DRONEID_MAX_DUTY && t.longest_us <= 1_000);
        if whole == 1 && others == 0 && quiet {
            return Some(Verdict::DroneIdBurst);
        }
    }

    if w >= VIDEO_MIN_WIDTH_KHZ && wide {
        // Band first: even a clipped (wider than 20 MHz) signal is in it if
        // the LO is.
        let in_band = match centre_khz(f, c) {
            Some(k) => plan::in_o4_band(k),
            None => plan::in_o4_band(c.lo_khz),
        };
        if in_band {
            return Some(Verdict::O4Video);
        }
        // The raster tests need a centre to tens of kHz: only the 10 MHz
        // modes give one. An 18 MHz signal reaches the half-band's roll-off
        // at both edges of the capture, and its centre came out 0.1-2.1 MHz
        // low (RFUAV FPV Combo, 20 MHz mode, true 5770.50 at full rate).
        if let Some(k) = centre_khz(f, c)
            && f.spec.w10_khz <= RASTER_MAX_W10_KHZ
        {
            if plan::on_half_raster(k) {
                return Some(Verdict::Video);
            }
            if plan::fpv_v1_channel(k).is_some() {
                return Some(Verdict::FpvAirUnit);
            }
            // Measured centre on neither DJI raster: LTE-style, not DJI.
            // The RFUAV EVO Nano's (Autel) 15 kHz, ~10 MHz bursts sit on
            // 5733.1, 5742.0, 5751.3, 5760.1, 5769.0, 5777.8, 5786.8,
            // 5796.0, 5805.0 and 5813.6 MHz; all but the last two are off
            // both rasters.
            return Some(Verdict::Ofdm15);
        }
        return Some(Verdict::Video);
    }
    Some(Verdict::Ofdm15)
}
