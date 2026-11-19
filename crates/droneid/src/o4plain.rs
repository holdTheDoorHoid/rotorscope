//! Parsers for O4 **plaintext**: what a CRYP auxiliary field or an INFP
//! record contains once a separate decryption layer (not part of this
//! crate: `crates/djicrypt`, which works only with a key the owner supplies
//! at run time) has turned its ciphertext back into bytes.
//!
//! This module does no cryptography and never sees a key: it takes the
//! decrypted bytes and checks and reads them, exactly like
//! [`crate::payload`] does for OcuSync 2's plaintext record. It is the plug-in
//! point between the two: [`crate::payload::Encrypted::ciphertext`] gives
//! the bytes to decrypt, these functions read the result.
//!
//! Layouts and checks are facts from EdwardBlair/dji-droneid-spec `SPEC.md`
//! ("CRYP auxiliary plaintext", "INFP-10 plaintext", "INFP-11 plaintext"),
//! written from the tables, no code copied. Note the units differ from
//! OcuSync 2: INFP-10 altitude is metres and height decimetres.

use crate::payload::rad_e7_to_deg;

/// Why decrypted bytes were not accepted. A wrong key gives random bytes,
/// which these checks reject.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reject {
    Length,
    Selector,
    Marker,
    Version,
    TextLength,
    Coordinates,
    Heading,
    /// GPS time and aircraft position all zero.
    Uninitialised,
}

/// The decrypted 32-byte CRYP auxiliary field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CrypAux {
    pub serial: [u8; 16],
    pub lon_raw: i32,
    pub lat_raw: i32,
    /// Bit 0: position valid. Bit 1: "position producer marker".
    pub flags: u8,
}

impl CrypAux {
    pub const LEN: usize = 32;
    pub fn position_valid(&self) -> bool {
        self.flags & 1 != 0
    }
    pub fn serial_str(&self) -> Option<&str> {
        serial_text(&self.serial)
    }
}

/// Decrypted INFP-10 telemetry (109 bytes). Raw wire values; methods convert.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Infp10 {
    pub version: u8,
    pub seq: u16,
    /// Bit 4 motors on, bit 5 in air, bit 6 position valid, ... (SPEC.md
    /// "INFP-10 state flags"); see the `STATE_*` constants.
    pub state: u16,
    pub serial: [u8; 16],
    pub lon_raw: i32,
    pub lat_raw: i32,
    /// Metres (not feet as in OcuSync 2).
    pub altitude_m: i16,
    /// Decimetres above takeoff.
    pub height_dm: i16,
    /// cm/s.
    pub v_north: i16,
    pub v_east: i16,
    pub v_up: i16,
    /// Centidegrees.
    pub heading_cdeg: i16,
    pub gps_time_ms: u64,
    pub app_lat_raw: i32,
    pub app_lon_raw: i32,
    /// Both 1,000,000,000 when no home point is set.
    pub home_lon_raw: i32,
    pub home_lat_raw: i32,
    pub product_type: u8,
    pub identity_len: u8,
    pub identity: [u8; 20],
    /// The pilot's DJI account ID (personal data: hide by default).
    pub account_len: u8,
    pub account: [u8; 20],
}

/// The "no home point" sentinel pair.
pub const HOME_UNSET: i32 = 1_000_000_000;

impl Infp10 {
    pub const LEN: usize = 109;
    pub const STATE_SERIAL_VALID: u16 = 1 << 1;
    pub const STATE_MOTORS_ON: u16 = 1 << 4;
    pub const STATE_IN_AIR: u16 = 1 << 5;
    pub const STATE_POSITION_VALID: u16 = 1 << 6;
    pub const STATE_ALTITUDE_VALID: u16 = 1 << 7;
    pub const STATE_HEIGHT_VALID: u16 = 1 << 8;
    pub const STATE_HVEL_VALID: u16 = 1 << 9;
    pub const STATE_VVEL_VALID: u16 = 1 << 10;
    pub const STATE_YAW_VALID: u16 = 1 << 11;
    pub const STATE_APP_POSITION_VALID: u16 = 1 << 12;

    pub fn has(&self, bit: u16) -> bool {
        self.state & bit != 0
    }
    pub fn lat(&self) -> f64 {
        rad_e7_to_deg(self.lat_raw)
    }
    pub fn lon(&self) -> f64 {
        rad_e7_to_deg(self.lon_raw)
    }
    pub fn height_m(&self) -> f64 {
        self.height_dm as f64 / 10.0
    }
    pub fn heading_deg(&self) -> f64 {
        self.heading_cdeg as f64 / 100.0
    }
    pub fn home_set(&self) -> bool {
        !(self.home_lon_raw == HOME_UNSET && self.home_lat_raw == HOME_UNSET)
    }
    pub fn serial_str(&self) -> Option<&str> {
        serial_text(&self.serial)
    }
    pub fn identity_str(&self) -> Option<&str> {
        core::str::from_utf8(&self.identity[..(self.identity_len as usize).min(20)]).ok()
    }
    pub fn account_str(&self) -> Option<&str> {
        core::str::from_utf8(&self.account[..(self.account_len as usize).min(20)]).ok()
    }
}

/// Decrypted INFP-11 (141 bytes): the serial and a flight-purpose text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Infp11 {
    pub serial: [u8; 16],
    pub purpose_len: u8,
    pub purpose: [u8; 120],
}

impl Infp11 {
    pub const LEN: usize = 141;
    pub fn serial_str(&self) -> Option<&str> {
        serial_text(&self.serial)
    }
    /// The declared prefix, when it is UTF-8.
    pub fn purpose_str(&self) -> Option<&str> {
        core::str::from_utf8(&self.purpose[..(self.purpose_len as usize).min(120)]).ok()
    }
}

/// Bytes before the first NUL, when that is non-empty ASCII alphanumeric
/// (the spec's rule; anything else is "unavailable", not an error).
pub fn serial_text(b: &[u8]) -> Option<&str> {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    let t = &b[..end];
    (!t.is_empty() && t.iter().all(u8::is_ascii_alphanumeric)).then(|| core::str::from_utf8(t).ok()).flatten()
}

fn le16(b: &[u8], at: usize) -> i16 {
    i16::from_le_bytes([b[at], b[at + 1]])
}
fn le32(b: &[u8], at: usize) -> i32 {
    i32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}
fn lat_ok(raw: i32) -> bool {
    rad_e7_to_deg(raw).abs() <= 90.0
}
fn lon_ok(raw: i32) -> bool {
    rad_e7_to_deg(raw).abs() <= 180.0
}

pub fn parse_cryp_aux(p: &[u8]) -> Result<CrypAux, Reject> {
    if p.len() != CrypAux::LEN {
        return Err(Reject::Length);
    }
    let mut serial = [0; 16];
    serial.copy_from_slice(&p[..16]);
    let a = CrypAux { serial, lon_raw: le32(p, 16), lat_raw: le32(p, 20), flags: p[24] };
    if a.position_valid() && !(lon_ok(a.lon_raw) && lat_ok(a.lat_raw)) {
        return Err(Reject::Coordinates);
    }
    Ok(a)
}

pub fn parse_infp10(p: &[u8]) -> Result<Infp10, Reject> {
    if p.len() != Infp10::LEN {
        return Err(Reject::Length);
    }
    if p[0] != 0x10 {
        return Err(Reject::Selector);
    }
    if !matches!(p[1], 2 | 3) {
        return Err(Reject::Version);
    }
    if p[67] > 20 || p[88] > 20 {
        return Err(Reject::TextLength);
    }
    let mut serial = [0; 16];
    serial.copy_from_slice(&p[6..22]);
    let mut identity = [0; 20];
    identity.copy_from_slice(&p[68..88]);
    let mut account = [0; 20];
    account.copy_from_slice(&p[89..109]);
    let mut t = [0; 8];
    t.copy_from_slice(&p[42..50]);
    let r = Infp10 {
        version: p[1],
        seq: le16(p, 2) as u16,
        state: le16(p, 4) as u16,
        serial,
        lon_raw: le32(p, 22),
        lat_raw: le32(p, 26),
        altitude_m: le16(p, 30),
        height_dm: le16(p, 32),
        v_north: le16(p, 34),
        v_east: le16(p, 36),
        v_up: le16(p, 38),
        heading_cdeg: le16(p, 40),
        gps_time_ms: u64::from_le_bytes(t),
        app_lat_raw: le32(p, 50),
        app_lon_raw: le32(p, 54),
        home_lon_raw: le32(p, 58),
        home_lat_raw: le32(p, 62),
        product_type: p[66],
        identity_len: p[67],
        identity,
        account_len: p[88],
        account,
    };
    if !(lon_ok(r.lon_raw) && lat_ok(r.lat_raw) && lon_ok(r.app_lon_raw) && lat_ok(r.app_lat_raw)) {
        return Err(Reject::Coordinates);
    }
    if r.home_set() && !(lon_ok(r.home_lon_raw) && lat_ok(r.home_lat_raw)) {
        return Err(Reject::Coordinates);
    }
    if r.heading_cdeg.unsigned_abs() > 18_000 {
        return Err(Reject::Heading);
    }
    if r.gps_time_ms == 0 && r.lon_raw == 0 && r.lat_raw == 0 {
        return Err(Reject::Uninitialised);
    }
    Ok(r)
}

pub fn parse_infp11(p: &[u8]) -> Result<Infp11, Reject> {
    if p.len() != Infp11::LEN {
        return Err(Reject::Length);
    }
    if p[0] != 0x11 {
        return Err(Reject::Selector);
    }
    if p[17..20] != [0xcc, 0xaa, 0x03] {
        return Err(Reject::Marker);
    }
    if p[20] > 120 {
        return Err(Reject::TextLength);
    }
    let mut serial = [0; 16];
    serial.copy_from_slice(&p[1..17]);
    let mut purpose = [0; 120];
    purpose.copy_from_slice(&p[21..141]);
    Ok(Infp11 { serial, purpose_len: p[20], purpose })
}

/// Test-only builders of plaintexts (the inverse of the parsers), for
/// round-trip tests here and in a future decryption layer.
#[cfg(any(test, feature = "std"))]
pub mod build {
    use super::*;

    pub fn infp10(r: &Infp10) -> [u8; Infp10::LEN] {
        let mut p = [0u8; Infp10::LEN];
        p[0] = 0x10;
        p[1] = r.version;
        p[2..4].copy_from_slice(&r.seq.to_le_bytes());
        p[4..6].copy_from_slice(&r.state.to_le_bytes());
        p[6..22].copy_from_slice(&r.serial);
        p[22..26].copy_from_slice(&r.lon_raw.to_le_bytes());
        p[26..30].copy_from_slice(&r.lat_raw.to_le_bytes());
        p[30..32].copy_from_slice(&r.altitude_m.to_le_bytes());
        p[32..34].copy_from_slice(&r.height_dm.to_le_bytes());
        p[34..36].copy_from_slice(&r.v_north.to_le_bytes());
        p[36..38].copy_from_slice(&r.v_east.to_le_bytes());
        p[38..40].copy_from_slice(&r.v_up.to_le_bytes());
        p[40..42].copy_from_slice(&r.heading_cdeg.to_le_bytes());
        p[42..50].copy_from_slice(&r.gps_time_ms.to_le_bytes());
        p[50..54].copy_from_slice(&r.app_lat_raw.to_le_bytes());
        p[54..58].copy_from_slice(&r.app_lon_raw.to_le_bytes());
        p[58..62].copy_from_slice(&r.home_lon_raw.to_le_bytes());
        p[62..66].copy_from_slice(&r.home_lat_raw.to_le_bytes());
        p[66] = r.product_type;
        p[67] = r.identity_len;
        p[68..88].copy_from_slice(&r.identity);
        p[88] = r.account_len;
        p[89..109].copy_from_slice(&r.account);
        p
    }

    pub fn infp11(serial: &[u8], purpose: &str) -> [u8; Infp11::LEN] {
        let mut p = [0u8; Infp11::LEN];
        p[0] = 0x11;
        p[1..1 + serial.len()].copy_from_slice(serial);
        p[17..20].copy_from_slice(&[0xcc, 0xaa, 0x03]);
        p[20] = purpose.len() as u8;
        p[21..21 + purpose.len()].copy_from_slice(purpose.as_bytes());
        p
    }

    pub fn cryp_aux(serial: &[u8], lat_raw: i32, lon_raw: i32, flags: u8) -> [u8; CrypAux::LEN] {
        let mut p = [0u8; CrypAux::LEN];
        p[..serial.len()].copy_from_slice(serial);
        p[16..20].copy_from_slice(&lon_raw.to_le_bytes());
        p[20..24].copy_from_slice(&lat_raw.to_le_bytes());
        p[24] = flags;
        p
    }

    /// A made-up flight over Philadelphia (no real drone).
    pub fn sample_infp10() -> Infp10 {
        let e7 = |deg: f64| (deg.to_radians() * 1e7).round() as i32;
        let mut serial = [0u8; 16];
        serial[..14].copy_from_slice(b"SYNTHO4SERIAL1");
        Infp10 {
            version: 3,
            seq: 77,
            state: Infp10::STATE_SERIAL_VALID | Infp10::STATE_MOTORS_ON | Infp10::STATE_IN_AIR | Infp10::STATE_POSITION_VALID | Infp10::STATE_ALTITUDE_VALID | Infp10::STATE_HEIGHT_VALID,
            serial,
            lon_raw: e7(-75.1652),
            lat_raw: e7(39.9526),
            altitude_m: 61,
            height_dm: 452,
            v_north: 120,
            v_east: -340,
            v_up: 15,
            heading_cdeg: -9000,
            gps_time_ms: 1_763_300_000_000,
            app_lat_raw: e7(39.9530),
            app_lon_raw: e7(-75.1660),
            home_lon_raw: e7(-75.1661),
            home_lat_raw: e7(39.9531),
            product_type: 93,
            identity_len: 4,
            identity: *b"ID01\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0",
            account_len: 6,
            account: *b"ACCT01\0\0\0\0\0\0\0\0\0\0\0\0\0\0",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infp10_round_trip_and_checks() {
        let r = build::sample_infp10();
        let p = build::infp10(&r);
        assert_eq!(parse_infp10(&p), Ok(r));
        let got = parse_infp10(&p).unwrap();
        assert!((got.lat() - 39.9526).abs() < 1e-5 && (got.lon() + 75.1652).abs() < 1e-5);
        assert!((got.height_m() - 45.2).abs() < 1e-9 && (got.heading_deg() + 90.0).abs() < 1e-9);
        assert!(got.has(Infp10::STATE_IN_AIR) && got.home_set());
        assert_eq!(got.serial_str(), Some("SYNTHO4SERIAL1"));
        let bad = |f: &dyn Fn(&mut [u8; 109]), want| {
            let mut q = p;
            f(&mut q);
            assert_eq!(parse_infp10(&q), Err(want));
        };
        bad(&|q| q[0] = 0x11, Reject::Selector);
        bad(&|q| q[1] = 4, Reject::Version);
        bad(&|q| q[67] = 21, Reject::TextLength);
        bad(&|q| q[26..30].copy_from_slice(&i32::MAX.to_le_bytes()), Reject::Coordinates);
        bad(&|q| q[40..42].copy_from_slice(&18_001i16.to_le_bytes()), Reject::Heading);
        bad(&|q| { q[22..30].fill(0); q[42..50].fill(0) }, Reject::Uninitialised);
        assert_eq!(parse_infp10(&p[..108]), Err(Reject::Length));
        // The home sentinel is accepted.
        let mut q = p;
        q[58..62].copy_from_slice(&HOME_UNSET.to_le_bytes());
        q[62..66].copy_from_slice(&HOME_UNSET.to_le_bytes());
        assert!(!parse_infp10(&q).unwrap().home_set());
    }

    #[test]
    fn infp11_and_cryp_aux() {
        let p = build::infp11(b"SYNTHO4SERIAL1", "Survey");
        let r = parse_infp11(&p).unwrap();
        assert_eq!((r.serial_str(), r.purpose_str()), (Some("SYNTHO4SERIAL1"), Some("Survey")));
        let mut q = p;
        q[19] = 2;
        assert_eq!(parse_infp11(&q), Err(Reject::Marker));
        let a = parse_cryp_aux(&build::cryp_aux(b"SN1", 7_000_000, -13_000_000, 1)).unwrap();
        assert!(a.position_valid());
        assert_eq!(a.serial_str(), Some("SN1"));
        assert_eq!(parse_cryp_aux(&build::cryp_aux(b"SN1", i32::MAX, 0, 1)), Err(Reject::Coordinates));
        // Invalid coordinates with the valid bit clear are fine.
        assert!(parse_cryp_aux(&build::cryp_aux(b"SN1", i32::MAX, 0, 0)).is_ok());
        assert_eq!(serial_text(b"AB-1\0"), None);
    }
}
