//! The 56-bit ME field of an extended squitter (DF17, DF18; DO-260B /
//! ICAO Doc 9871 version 2 formats), by type code.

use libm::{atan2, sqrt};

use crate::alt::{self, Altitude};
use crate::cpr::{Cpr, Kind as CprKind};
use crate::frame::get_bits;

/// ME bits `start..start+n`, numbered from 1 as the standards do.
fn me(m: &[u8; 7], first: usize, n: usize) -> u32 {
    get_bits(m, first - 1, n)
}

/// The 6-bit character set of aircraft identification (also BDS 2,0).
/// `None` for the codes that are not assigned.
pub fn ais_char(c: u8) -> Option<u8> {
    match c {
        1..=26 => Some(b'A' + c - 1),
        32 => Some(b' '),
        48..=57 => Some(b'0' + c - 48),
        _ => None,
    }
}

/// Eight 6-bit characters starting at ME bit `first`; trailing spaces
/// trimmed. `None` if any character is unassigned.
pub fn callsign(m: &[u8; 7], first: usize) -> Option<adsb::Callsign> {
    let mut s = [b' '; 8];
    for (i, slot) in s.iter_mut().enumerate() {
        *slot = ais_char(me(m, first + 6 * i, 6) as u8)?;
    }
    let text = core::str::from_utf8(&s).ok()?;
    adsb::Callsign::new(text.trim_end())
}

/// Emitter category in the ADSBee's numbering (what `adsb::Kind::of`
/// takes): set A = type code 4 is 0-7, B (3) 8-15, C (2) 16-23, D (1) 24-31.
pub fn category_number(tc: u8, ca: u8) -> i8 {
    ((4 - tc.clamp(1, 4)) * 8 + (ca & 7)) as i8
}

/// Decoded extended squitter content.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Es {
    /// TC 1-4.
    Identification { tc: u8, category: u8, callsign: adsb::Callsign },
    /// TC 5-8.
    SurfacePosition {
        tc: u8,
        /// Ground speed, knots (`None`: not available).
        speed_kt: Option<f32>,
        /// Ground track, degrees (`None`: not valid).
        track_deg: Option<f32>,
        cpr: Cpr,
    },
    /// TC 9-18 (barometric altitude) and 20-22 (GNSS height).
    AirbornePosition {
        tc: u8,
        /// Surveillance status: 0 none, 1 permanent alert, 2 temporary
        /// alert, 3 SPI.
        ss: u8,
        altitude: Altitude,
        /// True for TC 20-22: `altitude` is GNSS height (HAE), not pressure.
        gnss: bool,
        /// ME bit 8: NIC supplement B in version 2 (the single-antenna
        /// flag in versions 0-1). With `tc` it gives the NIC
        /// (`adsb::nicnac::nic_of_type_code`).
        nic_b: bool,
        cpr: Cpr,
    },
    /// TC 19.
    Velocity(Velocity),
    /// TC 28 subtype 1.
    EmergencyStatus { emergency: u8, squawk: u16 },
    /// TC 28 subtype 2: a TCAS resolution advisory broadcast (not decoded).
    TcasRa,
    /// TC 29: target state and status (not decoded beyond the type).
    TargetState,
    /// TC 31.
    OperationalStatus(OperationalStatus),
    /// TC 0 (no position), 23-27 and 30 (reserved / test), or a subtype
    /// this decoder does not know.
    Other { tc: u8, subtype: u8 },
}

/// TC 19, all four subtypes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Velocity {
    pub subtype: u8,
    /// Navigation accuracy category for velocity.
    pub nacv: u8,
    /// Subtypes 1-2: ground speed (knots) and track (degrees true).
    pub ground_speed_kt: Option<f32>,
    pub track_deg: Option<f32>,
    /// Subtypes 3-4: heading (degrees, magnetic or true per the
    /// operational status HRD bit) and airspeed.
    pub heading_deg: Option<f32>,
    pub airspeed_kt: Option<u16>,
    /// `true` true airspeed, `false` indicated airspeed.
    pub airspeed_true: bool,
    /// Vertical rate, ft/min, up positive.
    pub vertical_rate_fpm: Option<i32>,
    /// `true`: the vertical rate is barometric; `false`: GNSS.
    pub vertical_rate_baro: bool,
    /// GNSS height minus barometric altitude, feet.
    pub gnss_minus_baro_ft: Option<i32>,
}

/// TC 31.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationalStatus {
    /// 0 airborne, 1 surface.
    pub subtype: u8,
    /// ADS-B version: 0 (DO-260), 1 (DO-260A), 2 (DO-260B).
    pub version: u8,
    pub nic_a: bool,
    pub nacp: u8,
    /// Geometric vertical accuracy (airborne only).
    pub gva: u8,
    pub sil: u8,
    /// Airborne: NIC baro. Surface: track angle / heading bit.
    pub nic_baro: bool,
    /// Heading reference: `false` true north, `true` magnetic north.
    pub hrd_magnetic: bool,
    pub sil_per_sample: bool,
    /// Surface: the length/width code (0-15).
    pub lw_code: Option<u8>,
}

/// Ground speed from the surface "movement" field, knots: piecewise
/// linear between the DO-260B breakpoints (code 2 = 0.125 kt, 9 = 1, 13 = 2,
/// 39 = 15, 94 = 70, 109 = 100, 124 = 175 or more).
pub fn movement_kt(mov: u8) -> Option<f32> {
    const CODES: [u8; 7] = [2, 9, 13, 39, 94, 109, 124];
    const KTS: [f32; 7] = [0.125, 1.0, 2.0, 15.0, 70.0, 100.0, 175.0];
    match mov {
        0 | 125.. => None,
        1 => Some(0.0),
        124 => Some(175.0),
        _ => {
            let i = CODES.iter().position(|&c| c > mov)?;
            let step = (KTS[i] - KTS[i - 1]) / f32::from(CODES[i] - CODES[i - 1]);
            Some(KTS[i - 1] + f32::from(mov - CODES[i - 1]) * step)
        }
    }
}

/// Decode an ME field.
pub fn decode(m: &[u8; 7]) -> Es {
    let tc = me(m, 1, 5) as u8;
    let st = me(m, 6, 3) as u8;
    match tc {
        1..=4 => match callsign(m, 9) {
            Some(cs) => Es::Identification { tc, category: st, callsign: cs },
            None => Es::Other { tc, subtype: st },
        },
        5..=8 => {
            let mov = me(m, 6, 7) as u8;
            let trk_ok = me(m, 13, 1) == 1;
            let trk = me(m, 14, 7);
            Es::SurfacePosition {
                tc,
                speed_kt: movement_kt(mov),
                track_deg: trk_ok.then(|| trk as f32 * 360.0 / 128.0),
                cpr: cpr_at(m),
            }
        }
        9..=18 | 20..=22 => {
            let ss = me(m, 6, 2) as u8;
            let field = me(m, 9, 12) as u16;
            let gnss = tc >= 20;
            // TC 20-22 carry GNSS height in the same 12-bit, 25 ft layout
            // (as dump1090-fa / readsb read it; see docs in lib.rs).
            let altitude = alt::ac12(field);
            let nic_b = me(m, 8, 1) == 1;
            Es::AirbornePosition { tc, ss, altitude, gnss, nic_b, cpr: cpr_at(m) }
        }
        19 => match velocity(m, st) {
            Some(v) => Es::Velocity(v),
            None => Es::Other { tc, subtype: st },
        },
        28 => match st {
            1 => Es::EmergencyStatus {
                emergency: me(m, 9, 3) as u8,
                squawk: alt::squawk(me(m, 12, 13) as u16),
            },
            2 => Es::TcasRa,
            _ => Es::Other { tc, subtype: st },
        },
        29 => Es::TargetState,
        31 if st <= 1 => Es::OperationalStatus(OperationalStatus {
            subtype: st,
            version: me(m, 41, 3) as u8,
            nic_a: me(m, 44, 1) == 1,
            nacp: me(m, 45, 4) as u8,
            gva: if st == 0 { me(m, 49, 2) as u8 } else { 0 },
            sil: me(m, 51, 2) as u8,
            nic_baro: me(m, 53, 1) == 1,
            hrd_magnetic: me(m, 54, 1) == 1,
            sil_per_sample: me(m, 55, 1) == 1,
            lw_code: (st == 1).then(|| me(m, 21, 4) as u8),
        }),
        _ => Es::Other { tc, subtype: st },
    }
}

fn cpr_at(m: &[u8; 7]) -> Cpr {
    Cpr { odd: me(m, 22, 1) == 1, lat: me(m, 23, 17), lon: me(m, 40, 17) }
}

/// Airborne or surface, for the CPR decode of a position message.
pub fn cpr_kind(es: &Es) -> Option<CprKind> {
    match es {
        Es::SurfacePosition { .. } => Some(CprKind::Surface),
        Es::AirbornePosition { .. } => Some(CprKind::Airborne),
        _ => None,
    }
}

fn velocity(m: &[u8; 7], st: u8) -> Option<Velocity> {
    if !(1..=4).contains(&st) {
        return None;
    }
    let nacv = me(m, 11, 3) as u8;
    let vr_baro = me(m, 36, 1) == 1;
    let vr_down = me(m, 37, 1) == 1;
    let vr = me(m, 38, 9) as i32;
    let vertical_rate_fpm = (vr != 0).then(|| {
        let v = (vr - 1) * 64;
        if vr_down { -v } else { v }
    });
    let diff_neg = me(m, 49, 1) == 1;
    let diff = me(m, 50, 7) as i32;
    let gnss_minus_baro_ft = (diff != 0).then(|| {
        let v = (diff - 1) * 25;
        if diff_neg { -v } else { v }
    });
    let mut v = Velocity {
        subtype: st,
        nacv,
        ground_speed_kt: None,
        track_deg: None,
        heading_deg: None,
        airspeed_kt: None,
        airspeed_true: false,
        vertical_rate_fpm,
        vertical_rate_baro: vr_baro,
        gnss_minus_baro_ft,
    };
    let mult: i32 = if st == 2 || st == 4 { 4 } else { 1 };
    if st <= 2 {
        let west = me(m, 14, 1) == 1;
        let vew = me(m, 15, 10) as i32;
        let south = me(m, 25, 1) == 1;
        let vns = me(m, 26, 10) as i32;
        if vew != 0 && vns != 0 {
            let ew = f64::from((vew - 1) * mult) * if west { -1.0 } else { 1.0 };
            let ns = f64::from((vns - 1) * mult) * if south { -1.0 } else { 1.0 };
            v.ground_speed_kt = Some(sqrt(ew * ew + ns * ns) as f32);
            let mut trk = atan2(ew, ns).to_degrees();
            if trk < 0.0 {
                trk += 360.0;
            }
            v.track_deg = Some(trk as f32);
        }
    } else {
        let hdg_ok = me(m, 14, 1) == 1;
        let hdg = me(m, 15, 10);
        v.heading_deg = hdg_ok.then(|| hdg as f32 * 360.0 / 1024.0);
        v.airspeed_true = me(m, 25, 1) == 1;
        let asp = me(m, 26, 10) as i32;
        v.airspeed_kt = (asp != 0).then(|| ((asp - 1) * mult) as u16);
    }
    Some(v)
}
