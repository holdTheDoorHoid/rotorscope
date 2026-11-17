//! UAT ADS-B payload fields (DO-282B), after error correction.
//!
//! Field positions and scalings follow dump978's `uat_decode.c`
//! (Oliver Jowett, GPL-2.0-or-later, as carried in SoftRF's
//! `libraries/dump978`), ported to Rust: the header (`uat_decode_hdr`), the
//! state vector (`uat_decode_sv`), the mode status (`uat_decode_ms`) and the
//! auxiliary state vector (`uat_decode_auxsv`), with the same payload-type
//! table (`uat_decode_adsb_mdb`).

use adsb::Callsign;

/// Which altitude a value is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AltKind {
    /// Pressure altitude (29.92 inHg).
    Baro,
    /// Geometric (GNSS) altitude.
    Geo,
}

/// What a direction value means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    /// Track over ground.
    Track,
    MagneticHeading,
    TrueHeading,
}

/// Air/ground state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AirGround {
    Subsonic,
    Supersonic,
    Ground,
    Reserved,
}

/// The state vector (every ADS-B payload type has one).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StateVector {
    /// Navigation integrity category (0 = unknown).
    pub nic: u8,
    /// Latitude, longitude in degrees.
    pub position: Option<(f64, f64)>,
    /// Altitude in feet and which kind.
    pub altitude: Option<(i32, AltKind)>,
    pub air_ground: AirGround,
    /// North (+) / south (-) velocity, knots (airborne only).
    pub ns_kt: Option<i32>,
    /// East (+) / west (-) velocity, knots (airborne only).
    pub ew_kt: Option<i32>,
    /// Direction 0..<360 degrees and what it means.
    pub track: Option<(f32, TrackKind)>,
    /// Ground speed, knots.
    pub speed_kt: Option<i32>,
    /// Vertical rate, ft/min, up positive, and its source.
    pub vrate: Option<(i32, AltKind)>,
    pub utc_coupled: bool,
    /// TIS-B site (address qualifiers 2 and 3 only), else 0.
    pub tisb_site_id: u8,
}

/// The mode status element (payload types 1 and 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeStatus {
    /// Emitter category 0..39 (same numbering as `adsb::Kind::of`).
    pub emitter_category: u8,
    /// Flight ID, when the field holds one.
    pub callsign: Callsign,
    /// Mode 3/A code, when the call-sign field holds a squawk instead.
    pub squawk: Option<u16>,
    /// Emergency/priority status: 0 none, 1 general, 2 medical, 3 minimum
    /// fuel, 4 no communications, 5 unlawful interference, 6 downed.
    pub emergency: u8,
    pub uat_version: u8,
    pub sil: u8,
    pub nac_p: u8,
    pub nac_v: u8,
    pub nic_baro: u8,
    pub ident_active: bool,
}

/// The decoded payload of one ADS-B frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Payload {
    /// Payload type code 0..31.
    pub mdb_type: u8,
    /// 0 ICAO via ADS-B, 1 reserved/national (self-assigned), 2 ICAO via
    /// TIS-B, 3 TIS-B track file, 4 vehicle, 5 fixed beacon.
    pub address_qualifier: u8,
    pub address: u32,
    pub sv: Option<StateVector>,
    pub ms: Option<ModeStatus>,
    /// Secondary altitude from the auxiliary state vector (the other kind
    /// from the state vector's).
    pub aux_altitude: Option<(i32, AltKind)>,
}

const BASE40: &[u8; 40] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ  ..";

/// UAT 12-bit altitude code: 0 = unknown, else (n-1)*25 - 1000 ft.
fn altitude(raw: u32) -> Option<i32> {
    (raw != 0).then(|| (raw as i32 - 1) * 25 - 1000)
}

fn sv(f: &[u8]) -> StateVector {
    let nic = f[11] & 15;
    let raw_lat = (u32::from(f[4]) << 15) | (u32::from(f[5]) << 7) | (u32::from(f[6]) >> 1);
    let raw_lon = ((u32::from(f[6]) & 1) << 23)
        | (u32::from(f[7]) << 15)
        | (u32::from(f[8]) << 7)
        | (u32::from(f[9]) >> 1);
    let position = (nic != 0 || raw_lat != 0 || raw_lon != 0).then(|| {
        let mut lat = f64::from(raw_lat) * 360.0 / 16_777_216.0;
        if lat > 90.0 {
            lat -= 180.0;
        }
        let mut lon = f64::from(raw_lon) * 360.0 / 16_777_216.0;
        if lon > 180.0 {
            lon -= 360.0;
        }
        (lat, lon)
    });
    let raw_alt = (u32::from(f[10]) << 4) | (u32::from(f[11]) >> 4);
    let alt_kind = if f[9] & 1 != 0 {
        AltKind::Geo
    } else {
        AltKind::Baro
    };
    let altitude = altitude(raw_alt).map(|a| (a, alt_kind));

    let air_ground = match f[12] >> 6 {
        0 => AirGround::Subsonic,
        1 => AirGround::Supersonic,
        2 => AirGround::Ground,
        _ => AirGround::Reserved,
    };
    let mut out = StateVector {
        nic,
        position,
        altitude,
        air_ground,
        ns_kt: None,
        ew_kt: None,
        track: None,
        speed_kt: None,
        vrate: None,
        utc_coupled: false,
        tisb_site_id: 0,
    };
    let a = (i32::from(f[12] & 0x1f) << 6) | (i32::from(f[13]) >> 2);
    let b = (i32::from(f[13] & 0x03) << 9) | (i32::from(f[14]) << 1) | (i32::from(f[15]) >> 7);
    match air_ground {
        AirGround::Subsonic | AirGround::Supersonic => {
            let scale = if air_ground == AirGround::Supersonic {
                4
            } else {
                1
            };
            let vel = |raw: i32| {
                (raw & 0x3ff != 0).then(|| {
                    let v = ((raw & 0x3ff) - 1) * scale;
                    if raw & 0x400 != 0 { -v } else { v }
                })
            };
            out.ns_kt = vel(a);
            out.ew_kt = vel(b);
            if let (Some(ns), Some(ew)) = (out.ns_kt, out.ew_kt) {
                if ns != 0 || ew != 0 {
                    let deg = libm::atan2f(ew as f32, ns as f32).to_degrees();
                    out.track = Some((if deg < 0.0 { deg + 360.0 } else { deg }, TrackKind::Track));
                }
                let sp = libm::sqrt(f64::from(ns * ns + ew * ew));
                out.speed_kt = Some(libm::round(sp) as i32);
            }
            let raw_v = (i32::from(f[15] & 0x7f) << 4) | (i32::from(f[16]) >> 4);
            if raw_v & 0x1ff != 0 {
                let kind = if raw_v & 0x400 != 0 {
                    AltKind::Baro
                } else {
                    AltKind::Geo
                };
                let v = ((raw_v & 0x1ff) - 1) * 64;
                out.vrate = Some((if raw_v & 0x200 != 0 { -v } else { v }, kind));
            }
        }
        AirGround::Ground => {
            if a != 0 {
                out.speed_kt = Some((a & 0x3ff) - 1);
            }
            let kind = match (b & 0x600) >> 9 {
                1 => Some(TrackKind::Track),
                2 => Some(TrackKind::MagneticHeading),
                3 => Some(TrackKind::TrueHeading),
                _ => None,
            };
            out.track = kind.map(|k| ((b & 0x1ff) as f32 * 360.0 / 512.0, k));
        }
        AirGround::Reserved => {}
    }
    let aq = f[0] & 7;
    if aq == 2 || aq == 3 {
        out.tisb_site_id = f[16] & 0x0f;
    } else {
        out.utc_coupled = f[16] & 0x08 != 0;
    }
    out
}

fn ms(f: &[u8]) -> ModeStatus {
    let w = |i: usize| (u16::from(f[i]) << 8) | u16::from(f[i + 1]);
    let (v0, v1, v2) = (w(17), w(19), w(21));
    let ch = |v: u16| BASE40[usize::from(v % 40)];
    let chars = [
        ch(v0 / 40),
        ch(v0),
        ch(v1 / 1600),
        ch(v1 / 40),
        ch(v1),
        ch(v2 / 1600),
        ch(v2 / 40),
        ch(v2),
    ];
    let text = core::str::from_utf8(&chars).unwrap_or("");
    // Trailing spaces trimmed (dump978); '.' is a reserved code, treat as
    // nothing usable.
    let text = text.trim_end_matches(' ');
    let csid = f[26] & 0x02 != 0;
    let mut callsign = Callsign::default();
    let mut squawk = None;
    if !text.is_empty() && !text.contains('.') {
        if csid {
            callsign = Callsign::new(text).unwrap_or_default();
        } else {
            // CSID = 0: the field carries the Mode 3/A code, four octal digits.
            let t = text.trim();
            if t.len() == 4 && t.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
                squawk = u16::from_str_radix(t, 8).ok();
            }
        }
    }
    ModeStatus {
        emitter_category: ((v0 / 1600) % 40) as u8,
        callsign,
        squawk,
        emergency: (f[23] >> 5) & 7,
        uat_version: (f[23] >> 2) & 7,
        sil: f[23] & 3,
        nac_p: f[25] >> 4,
        nac_v: (f[25] >> 1) & 7,
        nic_baro: f[25] & 1,
        ident_active: f[26] & 0x10 != 0,
    }
}

fn aux_altitude(f: &[u8]) -> Option<(i32, AltKind)> {
    let raw = (u32::from(f[29]) << 4) | (u32::from(f[30]) >> 4);
    // The secondary altitude is the other kind from the primary one.
    let kind = if f[9] & 1 != 0 {
        AltKind::Baro
    } else {
        AltKind::Geo
    };
    altitude(raw).map(|a| (a, kind))
}

/// Decode the data bytes of a corrected frame (18 for basic, 34 for long).
/// Payload types that need a long frame are only decoded from one.
pub fn decode(data: &[u8]) -> Option<Payload> {
    if data.len() < 18 {
        return None;
    }
    let mdb_type = data[0] >> 3;
    let long = data.len() >= 34;
    let mut p = Payload {
        mdb_type,
        address_qualifier: data[0] & 7,
        address: (u32::from(data[1]) << 16) | (u32::from(data[2]) << 8) | u32::from(data[3]),
        sv: None,
        ms: None,
        aux_altitude: None,
    };
    match mdb_type {
        // HDR + SV (type 0 is the basic frame; 4, 7-10 are long frames whose
        // other elements are reserved or not decoded here).
        0 | 4 | 7..=10 => p.sv = Some(sv(data)),
        1 if long => {
            p.sv = Some(sv(data));
            p.ms = Some(ms(data));
            p.aux_altitude = aux_altitude(data);
        }
        2 | 5 | 6 if long => {
            p.sv = Some(sv(data));
            p.aux_altitude = aux_altitude(data);
        }
        3 if long => {
            p.sv = Some(sv(data));
            p.ms = Some(ms(data));
        }
        _ => {}
    }
    Some(p)
}
