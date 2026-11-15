//! Compact Position Reporting (ICAO Doc 9871, DO-260B Appendix A): the 17-bit
//! latitude/longitude pairs of ADS-B position messages.
//!
//! A single message only places an aircraft within one zone (6 degrees of
//! latitude airborne, 1.5 on the surface). Two ways to remove the ambiguity:
//! - **global**: an even and an odd message close together in time;
//! - **local**: one message plus a reference point known to be within half a
//!   zone (about 180 NM airborne, 45 NM on the surface): the receiver's
//!   own position or the aircraft's last known position.

use libm::floor;

/// Number of latitude zones between the equator and a pole.
pub const NZ: f64 = 15.0;
const SCALE: f64 = 131_072.0; // 2^17

/// Latitudes (degrees) at which the number of longitude zones drops:
/// below `NL_EDGES[0]` NL is 59, below `NL_EDGES[i]` it is 59 - i, and at
/// or above 87 degrees it is 1. From
/// `acos(sqrt((1 - cos(pi/(2 NZ))) / (1 - cos(2 pi / NL))))`; the test
/// `nl_table_matches_formula` recomputes them.
pub const NL_EDGES: [f64; 58] = [
    10.470471300, 14.828174369, 18.186263571, 21.029394926, 23.545044866, 25.829247071,
    27.938987101, 29.911356857, 31.772097077, 33.539934363, 35.228995978, 36.850251076,
    38.412418924, 39.922566843, 41.386518323, 42.809140122, 44.194549514, 45.546267227,
    46.867332525, 48.160391281, 49.427764393, 50.671501656, 51.893424692, 53.095161528,
    54.278174723, 55.443784445, 56.593187562, 57.727473539, 58.847637761, 59.954592767,
    61.049177742, 62.132166592, 63.204274794, 64.266165226, 65.318453097, 66.361710084,
    67.396467741, 68.423220221, 69.442426311, 70.454510750, 71.459864730, 72.458845447,
    73.451774417, 74.438934157, 75.420562567, 76.396843908, 77.367894613, 78.333740829,
    79.294282255, 80.249232133, 81.198013493, 82.139569805, 83.071994447, 83.991735630,
    84.891661907, 85.755416209, 86.535369975, 87.000000000,
];

/// Number of longitude zones at latitude `lat` (1..=59).
pub fn nl(lat: f64) -> u32 {
    let a = if lat < 0.0 { -lat } else { lat };
    for (i, &edge) in NL_EDGES.iter().enumerate() {
        if a < edge {
            return 59 - i as u32;
        }
    }
    1
}

/// Positive modulo.
fn modulo(a: f64, b: f64) -> f64 {
    let r = a - b * floor(a / b);
    if r < 0.0 { r + b } else { r }
}

/// One CPR-encoded position as sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cpr {
    /// Odd (F = 1) or even format.
    pub odd: bool,
    pub lat: u32,
    pub lon: u32,
}

impl Cpr {
    fn lat_f(&self) -> f64 {
        f64::from(self.lat) / SCALE
    }
    fn lon_f(&self) -> f64 {
        f64::from(self.lon) / SCALE
    }
}

/// Airborne or surface encoding: the surface one packs four times the
/// resolution into a quarter of the zone size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Airborne,
    Surface,
}

impl Kind {
    fn span(self) -> f64 {
        match self {
            Kind::Airborne => 360.0,
            Kind::Surface => 90.0,
        }
    }
}

/// Why a decode gave no position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CprError {
    /// The even and odd messages straddle a longitude-zone boundary (the
    /// aircraft crossed one between them): wait for the next pair.
    ZoneMismatch,
    /// Latitude out of range: a corrupt or mismatched pair.
    BadLatitude,
    /// Surface global decode needs a reference to choose a quadrant.
    NeedReference,
}

/// Global decode from an even and an odd message. `odd_newer` says which
/// arrived last; the position is the newer one's. `reference` (lat, lon)
/// is only used, and is required, for [`Kind::Surface`], where the result is
/// one of four quadrants (and two hemispheres) and the one nearest the
/// reference is taken.
///
/// The caller must make sure the pair is close in time (the
/// [`crate::table`] uses 10 s airborne, 50 s on the surface).
pub fn global(
    even: Cpr,
    odd: Cpr,
    odd_newer: bool,
    kind: Kind,
    reference: Option<(f64, f64)>,
) -> Result<(f64, f64), CprError> {
    let span = kind.span();
    let dlat0 = span / (4.0 * NZ);
    let dlat1 = span / (4.0 * NZ - 1.0);
    let (lat0, lat1) = (even.lat_f(), odd.lat_f());
    let (lon0, lon1) = (even.lon_f(), odd.lon_f());

    let j = floor(59.0 * lat0 - 60.0 * lat1 + 0.5);
    let mut rlat0 = dlat0 * (modulo(j, 60.0) + lat0);
    let mut rlat1 = dlat1 * (modulo(j, 59.0) + lat1);

    match kind {
        Kind::Airborne => {
            if rlat0 >= 270.0 {
                rlat0 -= 360.0;
            }
            if rlat1 >= 270.0 {
                rlat1 -= 360.0;
            }
        }
        Kind::Surface => {
            // Northern-hemisphere answer first; the southern one is 90
            // degrees below. Pick by the reference.
            let (rl, _) = reference.ok_or(CprError::NeedReference)?;
            if rl < 0.0 {
                rlat0 -= 90.0;
                rlat1 -= 90.0;
            }
        }
    }
    if !(-90.0..=90.0).contains(&rlat0) || !(-90.0..=90.0).contains(&rlat1) {
        return Err(CprError::BadLatitude);
    }
    let nl0 = nl(rlat0);
    if nl0 != nl(rlat1) {
        return Err(CprError::ZoneMismatch);
    }
    let (lat, ni, lon_cpr) = if odd_newer {
        (rlat1, (nl0 as i64 - 1).max(1), lon1)
    } else {
        (rlat0, (nl0 as i64).max(1), lon0)
    };
    let nl_f = f64::from(nl0);
    let m = floor(lon0 * (nl_f - 1.0) - lon1 * nl_f + 0.5);
    let ni_f = ni as f64;
    let mut lon = (span / ni_f) * (modulo(m, ni_f) + lon_cpr);
    match kind {
        Kind::Airborne => {
            if lon >= 180.0 {
                lon -= 360.0;
            }
        }
        Kind::Surface => {
            // Four candidates 90 degrees apart: take the one nearest the
            // reference longitude.
            let (_, rlon) = reference.ok_or(CprError::NeedReference)?;
            let mut best = lon;
            let mut best_d = f64::MAX;
            for k in 0..4 {
                let mut c = lon + 90.0 * f64::from(k);
                c = modulo(c + 180.0, 360.0) - 180.0;
                let d = angle_diff(c, rlon);
                if d < best_d {
                    best_d = d;
                    best = c;
                }
            }
            lon = best;
        }
    }
    Ok((lat, lon))
}

fn angle_diff(a: f64, b: f64) -> f64 {
    let d = modulo(a - b + 180.0, 360.0) - 180.0;
    if d < 0.0 { -d } else { d }
}

/// Local decode of one message against a reference position that is
/// within half a zone of the aircraft (180 NM airborne, 45 NM surface).
/// Always gives an answer; whether the reference was close enough is the
/// caller's judgement.
pub fn local(msg: Cpr, kind: Kind, reference: (f64, f64)) -> (f64, f64) {
    let span = kind.span();
    let (rlat, rlon) = reference;
    let i = if msg.odd { 1.0 } else { 0.0 };
    let dlat = span / (4.0 * NZ - i);
    let lat_cpr = msg.lat_f();
    let j = floor(rlat / dlat) + floor(0.5 + modulo(rlat, dlat) / dlat - lat_cpr);
    let lat = dlat * (j + lat_cpr);
    let ni = (f64::from(nl(lat)) - i).max(1.0);
    let dlon = span / ni;
    let lon_cpr = msg.lon_f();
    let m = floor(rlon / dlon) + floor(0.5 + modulo(rlon, dlon) / dlon - lon_cpr);
    let mut lon = dlon * (m + lon_cpr);
    if lon >= 180.0 {
        lon -= 360.0;
    }
    if lon < -180.0 {
        lon += 360.0;
    }
    (lat, lon)
}

/// CPR-encode a position (tests and test-frame builders; DO-260B A.1.7.3).
pub fn encode(lat: f64, lon: f64, odd: bool, kind: Kind) -> Cpr {
    let span = kind.span();
    let i = if odd { 1.0 } else { 0.0 };
    let dlat = span / (4.0 * NZ - i);
    let yz = floor(SCALE * modulo(lat, dlat) / dlat + 0.5);
    let rlat = dlat * (yz / SCALE + floor(lat / dlat));
    let ni = (f64::from(nl(rlat)) - i).max(1.0);
    let dlon = span / ni;
    let xz = floor(SCALE * modulo(lon, dlon) / dlon + 0.5);
    Cpr {
        odd,
        lat: (yz as u32) & 0x1FFFF,
        lon: (xz as u32) & 0x1FFFF,
    }
}
