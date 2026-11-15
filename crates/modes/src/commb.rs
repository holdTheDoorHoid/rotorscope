//! Comm-B (DF20/DF21) MB fields: which BDS register a reply carries, by
//! inference, and its content for the common ones.
//!
//! A Comm-B reply does not say which register it holds: the ground
//! interrogator asked for one and knows. A passive receiver has to guess
//! from the content. Each register has reserved bits that must be zero,
//! status bits that must be set when a value is present (and the value zero
//! when not), and physical ranges. A register is only reported when exactly
//! one of the known layouts fits; when several fit (mostly all-zero or
//! sparse fields) the reply is [`Bds::Ambiguous`], and the table can break
//! the tie with what ADS-B already says about the aircraft
//! ([`Hint`]). Layouts per ICAO Doc 9871 (Mode S specific services),
//! appendix A tables A-2-16, A-2-32, A-2-64, A-2-80, A-2-96.

use crate::es::callsign;
use crate::frame::get_bits;

/// MB bit `first` (1-based) for `n` bits.
fn mb(m: &[u8; 7], first: usize, n: usize) -> u32 {
    get_bits(m, first - 1, n)
}

/// Two's-complement value of an `n`-bit magnitude with a separate sign bit
/// (the sign is the MSB of an n+1-bit two's-complement number).
fn twos(sign: bool, mag: u32, n: u32) -> i32 {
    if sign { mag as i32 - (1i32 << n) } else { mag as i32 }
}

/// Inferred register content.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Bds {
    /// 1,0: data link capability report (content not decoded).
    DataLinkCapability,
    /// 2,0: aircraft identification.
    Identification(adsb::Callsign),
    /// 4,0: selected vertical intention.
    VerticalIntent(Bds40),
    /// 5,0: track and turn report.
    TrackTurn(Bds50),
    /// 6,0: heading and speed report.
    HeadingSpeed(Bds60),
    /// Several layouts fit (bit mask: 1 = 1,0, 2 = 2,0, 4 = 4,0, 8 = 5,0,
    /// 16 = 6,0).
    Ambiguous(u8),
    /// None of the known layouts fits (other registers, or all zeros).
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bds40 {
    /// MCP/FCU selected altitude, feet.
    pub mcp_alt_ft: Option<i32>,
    /// FMS selected altitude, feet.
    pub fms_alt_ft: Option<i32>,
    /// Barometric pressure setting, hPa.
    pub baro_hpa: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bds50 {
    pub roll_deg: Option<f32>,
    pub track_deg: Option<f32>,
    pub ground_speed_kt: Option<u16>,
    pub track_rate_dps: Option<f32>,
    pub tas_kt: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bds60 {
    pub heading_deg: Option<f32>,
    pub ias_kt: Option<u16>,
    pub mach: Option<f32>,
    pub baro_rate_fpm: Option<i32>,
    pub inertial_rate_fpm: Option<i32>,
}

/// What is already known about the aircraft, to break ties.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Hint {
    pub ground_speed_kt: Option<f32>,
    pub track_deg: Option<f32>,
    pub callsign: Option<adsb::Callsign>,
}

/// A status bit and its value field: when the status is clear the value
/// must be all zeros. Returns `Err` for a violation, `Ok(None)` when not
/// present, `Ok(Some(v))` otherwise.
fn status_field(m: &[u8; 7], status: usize, first: usize, n: usize) -> Result<Option<u32>, ()> {
    let s = mb(m, status, 1) == 1;
    let v = mb(m, first, n);
    match (s, v) {
        (false, 0) => Ok(None),
        (false, _) => Err(()),
        (true, v) => Ok(Some(v)),
    }
}

fn is_bds10(m: &[u8; 7]) -> bool {
    // First byte 0x10, bits 10-14 reserved (zero).
    mb(m, 1, 8) == 0x10 && mb(m, 10, 5) == 0
}

fn bds20(m: &[u8; 7]) -> Option<adsb::Callsign> {
    if mb(m, 1, 8) != 0x20 {
        return None;
    }
    let cs = callsign(m, 9)?;
    (!cs.is_empty()).then_some(cs)
}

fn bds40(m: &[u8; 7]) -> Option<Bds40> {
    // Reserved: 40-47 and 52-53.
    if mb(m, 40, 8) != 0 || mb(m, 52, 2) != 0 {
        return None;
    }
    let mcp = status_field(m, 1, 2, 12).ok()?;
    let fms = status_field(m, 14, 15, 12).ok()?;
    let baro = status_field(m, 27, 28, 12).ok()?;
    if mcp.is_none() && fms.is_none() && baro.is_none() {
        return None;
    }
    let mcp_alt_ft = mcp.map(|v| v as i32 * 16);
    let fms_alt_ft = fms.map(|v| v as i32 * 16);
    let baro_hpa = baro.map(|v| v as f32 * 0.1 + 800.0);
    let alt_ok = |a: Option<i32>| a.is_none_or(|a| (0..=50_000).contains(&a));
    if !alt_ok(mcp_alt_ft) || !alt_ok(fms_alt_ft) {
        return None;
    }
    if baro_hpa.is_some_and(|p| !(900.0..=1100.0).contains(&p)) {
        return None;
    }
    Some(Bds40 { mcp_alt_ft, fms_alt_ft, baro_hpa })
}

fn bds50(m: &[u8; 7]) -> Option<Bds50> {
    let roll = status_field(m, 1, 2, 10).ok()?;
    let trk = status_field(m, 12, 13, 11).ok()?;
    let gs = status_field(m, 24, 25, 10).ok()?;
    let rate = status_field(m, 35, 36, 10).ok()?;
    let tas = status_field(m, 46, 47, 10).ok()?;
    if roll.is_none() && trk.is_none() && gs.is_none() && tas.is_none() {
        return None;
    }
    let roll_deg = roll.map(|v| twos(v & 0x200 != 0, v & 0x1FF, 9) as f32 * 45.0 / 256.0);
    let track_deg = trk.map(|v| {
        let t = twos(v & 0x400 != 0, v & 0x3FF, 10) as f32 * 90.0 / 512.0;
        if t < 0.0 { t + 360.0 } else { t }
    });
    let ground_speed_kt = gs.map(|v| (v * 2) as u16);
    let track_rate_dps = rate.map(|v| twos(v & 0x200 != 0, v & 0x1FF, 9) as f32 * 8.0 / 256.0);
    let tas_kt = tas.map(|v| (v * 2) as u16);
    if roll_deg.is_some_and(|r| r.abs() > 50.0) {
        return None;
    }
    if ground_speed_kt.is_some_and(|g| g > 700) || tas_kt.is_some_and(|t| t > 600) {
        return None;
    }
    if let (Some(g), Some(t)) = (ground_speed_kt, tas_kt) {
        // Wind rarely exceeds 200 kt.
        if (i32::from(g) - i32::from(t)).abs() > 200 {
            return None;
        }
    }
    if track_rate_dps.is_some_and(|r| r.abs() > 10.0) {
        return None;
    }
    Some(Bds50 { roll_deg, track_deg, ground_speed_kt, track_rate_dps, tas_kt })
}

fn bds60(m: &[u8; 7]) -> Option<Bds60> {
    let hdg = status_field(m, 1, 2, 11).ok()?;
    let ias = status_field(m, 13, 14, 10).ok()?;
    let mach = status_field(m, 24, 25, 10).ok()?;
    let baro = status_field(m, 35, 36, 10).ok()?;
    let ins = status_field(m, 46, 47, 10).ok()?;
    if hdg.is_none() && ias.is_none() && mach.is_none() {
        return None;
    }
    let heading_deg = hdg.map(|v| {
        let h = twos(v & 0x400 != 0, v & 0x3FF, 10) as f32 * 90.0 / 512.0;
        if h < 0.0 { h + 360.0 } else { h }
    });
    let ias_kt = ias.map(|v| v as u16);
    let mach_v = mach.map(|v| v as f32 * 2.048 / 512.0);
    let rate = |v: u32| twos(v & 0x200 != 0, v & 0x1FF, 9) * 32;
    let baro_rate_fpm = baro.map(rate);
    let inertial_rate_fpm = ins.map(rate);
    if ias_kt.is_some_and(|i| i == 0 || i > 500) || mach_v.is_some_and(|m| m <= 0.0 || m > 1.0) {
        return None;
    }
    if baro_rate_fpm.is_some_and(|r| r.abs() > 6000) || inertial_rate_fpm.is_some_and(|r| r.abs() > 6000) {
        return None;
    }
    if let (Some(i), Some(mc)) = (ias_kt, mach_v) {
        // IAS and Mach agree within a broad envelope: Mach is roughly
        // IAS/600 at low level and IAS/250 at FL400.
        let i = f32::from(i);
        if !(i / 700.0..=i / 200.0).contains(&mc) {
            return None;
        }
    }
    Some(Bds60 { heading_deg, ias_kt, mach: mach_v, baro_rate_fpm, inertial_rate_fpm })
}

/// Infer the register in an MB field.
pub fn infer(m: &[u8; 7], hint: &Hint) -> Bds {
    if m.iter().all(|&b| b == 0) {
        return Bds::Unknown;
    }
    let c10 = is_bds10(m);
    let c20 = bds20(m);
    let c40 = bds40(m);
    let mut c50 = bds50(m);
    let mut c60 = bds60(m);
    // Tie-break 5,0 / 6,0 with known ADS-B speed and track: a 5,0 whose
    // ground speed or track disagrees with ADS-B is not a 5,0.
    if let (Some(b), Some(gs)) = (c50, hint.ground_speed_kt)
        && b.ground_speed_kt.is_some_and(|g| (f32::from(g) - gs).abs() > 30.0) {
            c50 = None;
        }
    if let (Some(b), Some(trk)) = (c50, hint.track_deg)
        && b.track_deg.is_some_and(|t| angle_diff(t, trk) > 20.0) {
            c50 = None;
        }
    if let (Some(b), Some(gs)) = (c60, hint.ground_speed_kt) {
        // IAS is below ground speed at altitude, but never far above it.
        if b.ias_kt.is_some_and(|i| f32::from(i) > gs + 100.0) {
            c60 = None;
        }
    }
    let mask = u8::from(c10)
        | u8::from(c20.is_some()) << 1
        | u8::from(c40.is_some()) << 2
        | u8::from(c50.is_some()) << 3
        | u8::from(c60.is_some()) << 4;
    match mask {
        0 => Bds::Unknown,
        1 => Bds::DataLinkCapability,
        2 => c20.map_or(Bds::Unknown, Bds::Identification),
        4 => c40.map_or(Bds::Unknown, Bds::VerticalIntent),
        8 => c50.map_or(Bds::Unknown, Bds::TrackTurn),
        16 => c60.map_or(Bds::Unknown, Bds::HeadingSpeed),
        m => Bds::Ambiguous(m),
    }
}

fn angle_diff(a: f32, b: f32) -> f32 {
    let mut d = (a - b) % 360.0;
    if d < -180.0 {
        d += 360.0;
    }
    if d > 180.0 {
        d -= 360.0;
    }
    d.abs()
}

