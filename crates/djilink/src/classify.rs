//! From one capture's features (and its dwell's timing) to a verdict.
//!
//! Every DJI verdict needs DJI's numerology first: 15 kHz OFDM with a normal
//! cyclic prefix ([`crate::cp::CpReport::dji_numerology`]). Wi-Fi (3.2 µs
//! symbols), analog FM video and carriers never have it, so they can never
//! become a DJI kind here. After that, shape decides:
//!
//! | Verdict | Width (-10 dB) | Timing | Where |
//! |---|---|---|---|
//! | [`Verdict::ControllerBurst`] | 0.6-3.0 MHz | a 0.2-1.3 ms burst, mostly off | anywhere |
//! | [`Verdict::DroneIdBurst`] | 8-10.5 MHz (and >= 5 at -3 dB) | one whole 0.45-0.8 ms burst, dwell nearly silent | anywhere |
//! | [`Verdict::O4Video`] | >= 7 MHz (and >= 5 at -3 dB) | anything else | 5.170-5.250 GHz |
//! | [`Verdict::FpvAirUnit`] | >= 7 MHz | | a whole-MHz DJI FPV channel, off the x.5 raster |
//! | [`Verdict::Video`] | >= 7 MHz | | elsewhere |
//! | [`Verdict::Ofdm15`] | anything else, or extended prefix | | |
//!
//! [`Verdict::Ofdm30`] is 30 kHz OFDM (not DJI by any public record).

use crate::Features;
use crate::plan;
use crate::timeline::TimelineStats;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// DJI-style video downlink, generation not determined (an OcuSync
    /// drone, an O3/O4 air unit; Autel's LTE-based links look the same).
    Video,
    /// The same on 5.170-5.250 GHz, which only O4 products use for video
    /// (Autel EVO Lite/Max also file the band).
    O4Video,
    /// The same on a whole-MHz DJI FPV (V1/Vista) channel. Walksnail is
    /// said to share the channel plan and an LTE-style chip; unproven.
    FpvAirUnit,
    /// One narrow uplink-shaped burst; [`crate::HopTracker`] confirms the
    /// controller once bursts appear on different centres.
    ControllerBurst,
    /// A DroneID broadcast burst (the `droneid` receiver decodes those).
    DroneIdBurst,
    /// 15 kHz OFDM that fits none of the above (another width, or an
    /// extended prefix): the LTE-style family.
    Ofdm15,
    /// 30 kHz OFDM.
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
/// Controller burst length (µs): 6-7 symbols of 71.4 µs = 0.43-0.5 ms.
pub const CTRL_BURST_US: core::ops::RangeInclusive<u32> = 200..=1_300;
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

/// The absolute centre (kHz) of the capture's signal, if it was all inside
/// the capture.
pub fn centre_khz(f: &Features, c: &Context) -> Option<u32> {
    if f.spec.clipped() || f.spec.segments == 0 {
        return None;
    }
    Some((c.lo_khz as i64 + f.spec.centre_khz as i64).max(0) as u32)
}

pub fn classify(f: &Features, c: &Context) -> Option<Verdict> {
    let cp = &f.cp;
    if !cp.normal15.detected && !cp.extended15.detected {
        return cp.normal30.detected.then_some(Verdict::Ofdm30);
    }
    if !cp.dji_numerology() {
        return Some(Verdict::Ofdm15);
    }
    let w = f.spec.w10_khz;
    let b = &f.bursts;

    // Controller: a short burst of uplink width. The burst's own width
    // counts too, because two hops in one capture widen the averaged
    // spectrum.
    if !b.continuous && b.on_pct() <= CTRL_MAX_ON_PCT && c.timeline.is_none_or(|t| t.duty_pct <= 60) {
        let fits = b.iter().any(|x| {
            CTRL_BURST_US.contains(&x.len_us())
                && (CTRL_WIDTH_KHZ.contains(&x.width_khz) || (x.width_khz == 0 && CTRL_WIDTH_KHZ.contains(&w)))
        });
        let narrow = CTRL_WIDTH_KHZ.contains(&w) || w <= 2 * CTRL_WIDTH_KHZ.end();
        if fits && narrow {
            return Some(Verdict::ControllerBurst);
        }
    }

    let wide = f.spec.w3_khz >= WIDE_MIN_W3_KHZ;
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
        if let Some(k) = centre_khz(f, c)
            && plan::fpv_v1_channel(k).is_some()
            && !plan::on_half_raster(k)
        {
            return Some(Verdict::FpvAirUnit);
        }
        return Some(Verdict::Video);
    }
    Some(Verdict::Ofdm15)
}
