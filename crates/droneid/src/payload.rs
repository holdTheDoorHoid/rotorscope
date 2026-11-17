//! The 173-byte transport block: envelope, inner CRC-16 and record.
//!
//! **Plaintext (OcuSync 2, O3 where unencrypted).** The block starts with a
//! logical-object envelope: a length byte `L`, `L` bytes of record, then
//! DJI's CRC-16 over the length byte and record, little-endian. The flight
//! information record (selector `0x10`, `L = 88`) follows the same field order
//! as the Wi-Fi DroneID beacon decoded by `rid::dji`, two bytes later, with a
//! GPS time and the pilot's position in place of pitch and roll. Layout facts
//! from the NDSS 2023 paper (RUB-SysSec/DroneSecurity), Kismet's DroneID
//! notes as used by anarkiwi/samples2djidroneid (Apache-2.0) and the
//! Department 13 white paper; no code from any of them.
//!
//! | Offset | Size | Field |
//! |---|---|---|
//! | 0 | 1 | envelope length `L` (88) |
//! | 1 | 1 | selector `0x10` |
//! | 2 | 1 | version |
//! | 3 | 2 | sequence |
//! | 5 | 2 | state flags |
//! | 7 | 16 | serial (ASCII, NUL padded) |
//! | 23 | 4 | drone longitude, radians x 1e7 |
//! | 27 | 4 | drone latitude |
//! | 31 | 2 | altitude |
//! | 33 | 2 | height |
//! | 35 | 6 | velocity north, east, up |
//! | 41 | 2 | yaw, hundredths of a degree |
//! | 43 | 8 | GPS time, ms since the Unix epoch |
//! | 51 | 4 | pilot (app) latitude |
//! | 55 | 4 | pilot (app) longitude |
//! | 59 | 4 | home longitude |
//! | 63 | 4 | home latitude |
//! | 67 | 1 | product type |
//! | 68 | 1 | UUID length |
//! | 69 | 20 | UUID |
//! | 89 | 2 | CRC-16 |
//!
//! **O4 (encrypted).** The record is a `CRYP` (selector 0x13, 163 bytes) or
//! `INFP` (0x10, 128 bytes; 0x11, 160 bytes) core inside the same logical
//! envelope: `[length][core][7 zero bytes][CRC-16 LE]`, length = core + 7,
//! then zeros to the end of the 173-byte slot (SPEC.md "Logical-object
//! envelope", "O4 PHY transport block"). A record counts only when every
//! check in [`O4Checks`] passes ([`Encrypted::valid`]): boundary, CRC-16,
//! selector and marker, exact core length, the length field, zero padding
//! and zero tail. Only the cleartext header is read: record type, the 4-byte
//! key ID that links one session's packets, the IV and the declared length.
//! The ciphertext is left in the block, reachable through
//! [`Encrypted::ciphertext`] for a later layer; this crate never decrypts.
//! The older "V1" 0x55 envelope and bare cores are still recognised for
//! logs, but never count as valid O4 records (the O4 PHY carries a logical
//! object). Layout facts from EdwardBlair/dji-droneid-spec. **Untested on
//! real signals**: no O4 recording was available.

use crate::crc::{dji_crc8, dji_crc16};

/// Transport block payload length (without CRC-24A).
pub const BLOCK: usize = 173;

/// Envelope the record was found in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Envelope {
    /// Length byte, record, CRC-16.
    Logical { len: u8 },
    /// 0x55-framed transport envelope (O4 spec "V1").
    V1 { len: u16 },
    /// A bare O4 core with no envelope CRC.
    Direct,
}

/// Decoded content of a transport block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parsed {
    pub envelope: Envelope,
    /// Inner CRC-16 status; `None` where the envelope has none.
    pub crc16_ok: Option<bool>,
    pub record: Record,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Record {
    FlightInfo(FlightInfo),
    Encrypted(Encrypted),
    /// Selector 0x11, 141 bytes, seen in plaintext from a Mavic Air 2. It
    /// begins with the serial number; the rest (three non-zero bytes, then
    /// zeros in the one sample seen) is undocumented. Its length matches the
    /// plaintext of O4's encrypted INFP-11 record.
    Info11 { serial: [u8; 16], len: u16 },
    /// Something else, or too short.
    Unknown { selector: u8, len: u16 },
}

/// OcuSync 2 flight information (selector 0x10, 88 bytes). Raw wire values;
/// the methods convert.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlightInfo {
    pub version: u8,
    pub seq: u16,
    /// See the `STATE_*` constants (meanings as in `rid::dji`; believed).
    pub state: u16,
    pub serial: [u8; 16],
    /// Radians x 1e7.
    pub lat_raw: i32,
    pub lon_raw: i32,
    /// Altitude, raw units (see [`FlightInfo::altitude_m`]).
    pub altitude_raw: i16,
    /// Height above takeoff, raw units.
    pub height_raw: i16,
    /// Velocities, believed cm/s.
    pub v_north: i16,
    pub v_east: i16,
    pub v_up: i16,
    /// Hundredths of a degree.
    pub yaw_raw: i16,
    /// Milliseconds since the Unix epoch (GPS-derived).
    pub gps_time_ms: u64,
    pub app_lat_raw: i32,
    pub app_lon_raw: i32,
    pub home_lat_raw: i32,
    pub home_lon_raw: i32,
    pub product_type: u8,
    pub uuid_len: u8,
    pub uuid: [u8; 20],
}

/// Converts radians x 1e7 to degrees.
pub fn rad_e7_to_deg(v: i32) -> f64 {
    v as f64 * (180.0 / core::f64::consts::PI / 1e7)
}

impl FlightInfo {
    pub const STATE_SERIAL_VALID: u16 = 0x0001;
    pub const STATE_PRIVACY_DISABLED: u16 = 0x0002;
    pub const STATE_HOMEPOINT_SET: u16 = 0x0004;
    pub const STATE_UUID_SET: u16 = 0x0008;
    pub const STATE_MOTOR_ON: u16 = 0x0010;
    pub const STATE_IN_AIR: u16 = 0x0020;
    pub const STATE_GPS_VALID: u16 = 0x0040;
    pub const STATE_ALTITUDE_VALID: u16 = 0x0080;
    pub const STATE_HEIGHT_VALID: u16 = 0x0100;
    pub const STATE_HORIZONTAL_VELOCITY_VALID: u16 = 0x0200;
    pub const STATE_VERTICAL_VELOCITY_VALID: u16 = 0x0400;

    pub fn lat(&self) -> f64 {
        rad_e7_to_deg(self.lat_raw)
    }
    pub fn lon(&self) -> f64 {
        rad_e7_to_deg(self.lon_raw)
    }
    pub fn app_lat(&self) -> f64 {
        rad_e7_to_deg(self.app_lat_raw)
    }
    pub fn app_lon(&self) -> f64 {
        rad_e7_to_deg(self.app_lon_raw)
    }
    pub fn home_lat(&self) -> f64 {
        rad_e7_to_deg(self.home_lat_raw)
    }
    pub fn home_lon(&self) -> f64 {
        rad_e7_to_deg(self.home_lon_raw)
    }
    /// Altitude in metres, assuming the raw value is feet (the NDSS
    /// receiver's reading, which reproduces the paper's published values).
    pub fn altitude_m(&self) -> f64 {
        self.altitude_raw as f64 / FEET_PER_M
    }
    pub fn height_m(&self) -> f64 {
        self.height_raw as f64 / FEET_PER_M
    }
    pub fn yaw_deg(&self) -> f64 {
        self.yaw_raw as f64 / 100.0
    }
    /// Serial as text, trimmed of NULs, when it is printable ASCII.
    pub fn serial_str(&self) -> Option<&str> {
        ascii(&self.serial)
    }
    pub fn uuid_str(&self) -> Option<&str> {
        ascii(&self.uuid[..(self.uuid_len as usize).min(20)])
    }
    pub fn product_name(&self) -> Option<&'static str> {
        product_name(self.product_type)
    }
}

const FEET_PER_M: f64 = 3.281;

fn ascii(b: &[u8]) -> Option<&str> {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    let s = core::str::from_utf8(&b[..end]).ok()?;
    s.bytes().all(|c| (0x20..0x7F).contains(&c)).then_some(s)
}

/// O4 encrypted record kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncKind {
    /// Session-key record (selector 0x13).
    Cryp,
    /// Telemetry record, 109-byte ciphertext (selector 0x10).
    Infp10,
    /// Telemetry record, 141-byte ciphertext (selector 0x11).
    Infp11,
}

impl EncKind {
    /// Core length the selector implies.
    pub fn core_len(self) -> usize {
        match self {
            EncKind::Cryp => 163,
            EncKind::Infp10 => 128,
            EncKind::Infp11 => 160,
        }
    }
    /// Core offset of the encrypted bytes.
    pub fn ciphertext_at(self) -> usize {
        match self {
            EncKind::Cryp => 131,
            EncKind::Infp10 | EncKind::Infp11 => 19,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            EncKind::Cryp => "CRYP",
            EncKind::Infp10 => "INFP-10",
            EncKind::Infp11 => "INFP-11",
        }
    }
}

/// Cleartext header of an O4 record. Nothing here is secret: the key ID is
/// a truncated hash of the session key (`SHA-256(K)[0..4]` per the spec)
/// that only links the records of one session, and collisions between
/// sessions are possible. The ciphertext stays in the transport block
/// ([`Encrypted::core`], [`Encrypted::ciphertext`]); this crate only checks
/// its length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Encrypted {
    pub kind: EncKind,
    pub key_id: [u8; 4],
    /// CRYP: the AES-CTR IV prefix at core offset 121. INFP: at offset 9.
    pub iv: [u8; 8],
    /// The record's own length field (ciphertext or plaintext length).
    pub declared_len: u16,
    /// Whether that field holds the value the spec gives for this kind.
    pub layout_ok: bool,
    /// Where the core starts in the 173-byte block.
    pub core_at: u8,
    /// The spec's structural checks, all of which must pass before a record
    /// counts ([`Encrypted::valid`]).
    pub checks: O4Checks,
}

/// The receiver-side checks of the O4 spec (RF_PHY.md "Passive
/// demodulation" step 8, SPEC.md "Logical-object envelope" and "O4 PHY
/// transport block"), applied after CRC-24A has passed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct O4Checks {
    /// A logical-object envelope (length byte, payload, CRC-16) whose
    /// declared end, `length + 3`, lies inside the 173-byte slot.
    pub boundary: bool,
    /// Its CRC-16 (init 0x3692, reflected 0x8408) matches.
    pub crc16: bool,
    /// Selector and ASCII marker agree (0x13 `CRYP`, 0x10/0x11 `INFP`).
    pub marker: bool,
    /// Exact core length: the payload is one whole core of the selector's
    /// length plus the seven padding bytes.
    pub core_len: bool,
    /// The record's length field holds the spec's value (32, 109, 141).
    pub declared_len: bool,
    /// The seven padding bytes after the core are zero.
    pub padding_zero: bool,
    /// Every slot byte after the logical object is zero.
    pub tail_zero: bool,
}

impl O4Checks {
    pub fn valid(&self) -> bool {
        self.boundary && self.crc16 && self.marker && self.core_len && self.declared_len && self.padding_zero && self.tail_zero
    }
    /// The first failing check, for logs.
    pub fn first_failure(&self) -> Option<&'static str> {
        [
            (self.boundary, "boundary"),
            (self.crc16, "crc16"),
            (self.marker, "marker"),
            (self.core_len, "core_len"),
            (self.declared_len, "declared_len"),
            (self.padding_zero, "padding"),
            (self.tail_zero, "tail"),
        ]
        .into_iter()
        .find(|(ok, _)| !ok)
        .map(|(_, n)| n)
    }
}

/// Seven zero bytes follow every O4 core inside the logical object (SPEC.md).
pub const O4_PADDING: usize = 7;

impl Encrypted {
    /// Passed every structural check of the spec. Only valid records may
    /// update a session, a track or the channel follower.
    pub fn valid(&self) -> bool {
        self.checks.valid()
    }
    /// The whole core (selector to end of ciphertext) inside `block`.
    pub fn core<'a>(&self, block: &'a [u8; BLOCK]) -> &'a [u8] {
        let a = self.core_at as usize;
        &block[a..(a + self.kind.core_len()).min(BLOCK)]
    }
    /// The encrypted bytes of the core: CRYP's 32-byte auxiliary ciphertext
    /// (core offset 131) or INFP's 109 / 141 bytes (offset 19). Returned
    /// untouched; this crate never decrypts.
    pub fn ciphertext<'a>(&self, block: &'a [u8; BLOCK]) -> &'a [u8] {
        let c = self.core(block);
        &c[self.kind.ciphertext_at().min(c.len())..]
    }
    /// The complete logical object (`length + 3` bytes from block offset 0)
    /// when the envelope is a logical one.
    pub fn logical_object<'a>(&self, block: &'a [u8; BLOCK]) -> Option<&'a [u8]> {
        (self.checks.boundary).then(|| &block[..block[0] as usize + 3])
    }
}

fn classify(rec: &[u8]) -> Record {
    let selector = rec.first().copied().unwrap_or(0);
    let len = rec.len() as u16;
    if rec.len() >= 19 {
        let magic = &rec[1..5];
        let kind = match (selector, magic) {
            (0x13, b"CRYP") => Some(EncKind::Cryp),
            (0x10, b"INFP") => Some(EncKind::Infp10),
            (0x11, b"INFP") => Some(EncKind::Infp11),
            _ => None,
        };
        if let Some(kind) = kind {
            if rec.len() < kind.core_len() {
                return Record::Unknown { selector, len };
            }
            let mut key_id = [0; 4];
            key_id.copy_from_slice(&rec[5..9]);
            let mut iv = [0; 8];
            let (iv_at, len_at, want) = match kind {
                EncKind::Cryp => (121, 129, 32),
                EncKind::Infp10 => (9, 17, 109),
                EncKind::Infp11 => (9, 17, 141),
            };
            iv.copy_from_slice(&rec[iv_at..iv_at + 8]);
            let declared_len = u16::from_le_bytes([rec[len_at], rec[len_at + 1]]);
            let layout_ok = declared_len == want;
            let checks = O4Checks { marker: true, declared_len: layout_ok, ..O4Checks::default() };
            return Record::Encrypted(Encrypted { kind, key_id, iv, declared_len, layout_ok, core_at: 0, checks });
        }
    }
    if selector == 0x10 && rec.len() >= 88 {
        return Record::FlightInfo(flight_info(rec));
    }
    if selector == 0x11 && rec.len() >= 17 {
        let mut serial = [0; 16];
        serial.copy_from_slice(&rec[1..17]);
        return Record::Info11 { serial, len };
    }
    Record::Unknown { selector, len }
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn le32(b: &[u8], at: usize) -> i32 {
    i32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// `r` starts at the selector (block offset 1).
fn flight_info(r: &[u8]) -> FlightInfo {
    let mut serial = [0; 16];
    serial.copy_from_slice(&r[6..22]);
    let mut uuid = [0; 20];
    uuid.copy_from_slice(&r[68..88]);
    let mut t = [0; 8];
    t.copy_from_slice(&r[42..50]);
    FlightInfo {
        version: r[1],
        seq: le16(r, 2),
        state: le16(r, 4),
        serial,
        lon_raw: le32(r, 22),
        lat_raw: le32(r, 26),
        altitude_raw: le16(r, 30) as i16,
        height_raw: le16(r, 32) as i16,
        v_north: le16(r, 34) as i16,
        v_east: le16(r, 36) as i16,
        v_up: le16(r, 38) as i16,
        yaw_raw: le16(r, 40) as i16,
        gps_time_ms: u64::from_le_bytes(t),
        app_lat_raw: le32(r, 50),
        app_lon_raw: le32(r, 54),
        home_lon_raw: le32(r, 58),
        home_lat_raw: le32(r, 62),
        product_type: r[66],
        uuid_len: r[67],
        uuid,
    }
}

/// Parse a decoded transport block (CRC-24A already checked by the caller).
pub fn parse(b: &[u8; BLOCK]) -> Parsed {
    // V1 transport envelope: 0x55, 10-bit length, CRC-8 header.
    if b[0] == 0x55 {
        let len = b[1] as usize | ((b[2] as usize & 3) << 8);
        if (13..=BLOCK).contains(&len) && dji_crc8(&b[..3]) == b[3] {
            let ok = dji_crc16(&b[..len - 2]) == le16(b, len - 2);
            if b[9..11] == [0x03, 0xda] {
                let mut record = classify(&b[11..len - 2]);
                if let Record::Encrypted(e) = &mut record {
                    // Recognised, but not the logical object the O4 PHY
                    // carries: never valid (checks.boundary stays false).
                    e.core_at = 11;
                    e.checks.crc16 = ok;
                }
                return Parsed { envelope: Envelope::V1 { len: len as u16 }, crc16_ok: Some(ok), record };
            }
        }
    }
    // Logical envelope.
    let l = b[0] as usize;
    if l >= 1 && l + 3 <= BLOCK {
        let ok = dji_crc16(&b[..l + 1]) == le16(b, l + 1);
        let mut record = classify(&b[1..l + 1]);
        if let Record::Encrypted(e) = &mut record {
            let n = e.kind.core_len();
            e.core_at = 1;
            e.checks.boundary = true;
            e.checks.crc16 = ok;
            e.checks.core_len = l == n + O4_PADDING;
            e.checks.padding_zero = b[1 + n..1 + l].iter().all(|&v| v == 0);
            e.checks.tail_zero = b[l + 3..].iter().all(|&v| v == 0);
        }
        if ok || !matches!(record, Record::Unknown { .. }) {
            return Parsed { envelope: Envelope::Logical { len: l as u8 }, crc16_ok: Some(ok), record };
        }
    }
    // A bare core.
    if let rec @ Record::Encrypted(_) = classify(b) {
        return Parsed { envelope: Envelope::Direct, crc16_ok: None, record: rec };
    }
    Parsed {
        envelope: Envelope::Logical { len: b[0] },
        crc16_ok: Some(false),
        record: Record::Unknown { selector: b[1], len: b[0] as u16 },
    }
}

/// DJI product type codes, from public tables (Department 13 white paper,
/// Kismet, the NDSS receiver). Facts only.
pub fn product_name(code: u8) -> Option<&'static str> {
    Some(match code {
        1 => "Inspire 1",
        2 | 3 => "Phantom 3 Series",
        4 => "Phantom 3 Std",
        5 => "M100",
        6 => "ACEONE",
        7 => "WKM",
        8 => "NAZA",
        9 => "A2",
        10 => "A3",
        11 => "Phantom 4",
        12 => "MG1",
        14 => "M600",
        15 => "Phantom 3 4K",
        16 => "Mavic Pro",
        17 => "Inspire 2",
        18 => "Phantom 4 Pro",
        20 => "N2",
        21 => "Spark",
        23 => "M600 Pro",
        24 => "Mavic Air",
        25 => "M200",
        26 => "Phantom 4 Series",
        27 => "Phantom 4 Adv",
        28 => "M210",
        30 => "M210 RTK",
        31 => "A3 AG",
        32 => "MG2",
        34 => "MG1A",
        35 => "Phantom 4 RTK",
        36 => "Phantom 4 Pro V2.0",
        38 => "MG1P",
        40 => "MG1P RTK",
        41 => "Mavic 2",
        44 => "M200 V2 Series",
        51 => "Mavic 2 Enterprise",
        53 => "Mavic Mini",
        58 => "Mavic Air 2",
        59 => "P4M",
        60 => "M300 RTK",
        61 => "DJI FPV",
        63 => "Mini 2",
        64 => "AGRAS T10",
        65 => "AGRAS T30",
        66 => "Air 2S",
        67 => "M30",
        68 => "Mavic 3",
        69 => "Mavic 2 Enterprise Advanced",
        70 => "Mini SE",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic flight-info block with made-up values (not from any
    /// recording).
    pub(crate) fn sample_block() -> [u8; BLOCK] {
        let mut b = [0u8; BLOCK];
        b[0] = 88;
        b[1] = 0x10;
        b[2] = 2;
        b[3..5].copy_from_slice(&1234u16.to_le_bytes());
        b[5..7].copy_from_slice(&0x1ff3u16.to_le_bytes());
        b[7..23].copy_from_slice(b"TESTSERIAL000001");
        let e7 = |deg: f64| (deg.to_radians() * 1e7).round() as i32;
        b[23..27].copy_from_slice(&e7(-75.16).to_le_bytes());
        b[27..31].copy_from_slice(&e7(39.95).to_le_bytes());
        b[31..33].copy_from_slice(&134i16.to_le_bytes());
        b[33..35].copy_from_slice(&12i16.to_le_bytes());
        b[35..37].copy_from_slice(&(-5i16).to_le_bytes());
        b[37..39].copy_from_slice(&7i16.to_le_bytes());
        b[39..41].copy_from_slice(&0i16.to_le_bytes());
        b[41..43].copy_from_slice(&16900i16.to_le_bytes());
        b[43..51].copy_from_slice(&1_700_000_000_123u64.to_le_bytes());
        b[51..55].copy_from_slice(&e7(39.951).to_le_bytes());
        b[55..59].copy_from_slice(&e7(-75.161).to_le_bytes());
        b[59..63].copy_from_slice(&e7(-75.162).to_le_bytes());
        b[63..67].copy_from_slice(&e7(39.952).to_le_bytes());
        b[67] = 63;
        b[68] = 6;
        b[69..75].copy_from_slice(b"ABCDEF");
        let c = dji_crc16(&b[..89]);
        b[89..91].copy_from_slice(&c.to_le_bytes());
        b
    }

    #[test]
    fn flight_info_fields() {
        let b = sample_block();
        let p = parse(&b);
        assert_eq!(p.envelope, Envelope::Logical { len: 88 });
        assert_eq!(p.crc16_ok, Some(true));
        let Record::FlightInfo(f) = p.record else { panic!("{p:?}") };
        assert_eq!((f.version, f.seq, f.state), (2, 1234, 0x1ff3));
        assert_eq!(f.serial_str(), Some("TESTSERIAL000001"));
        assert!((f.lat() - 39.95).abs() < 1e-5 && (f.lon() + 75.16).abs() < 1e-5);
        assert!((f.app_lat() - 39.951).abs() < 1e-5 && (f.app_lon() + 75.161).abs() < 1e-5);
        assert!((f.home_lat() - 39.952).abs() < 1e-5 && (f.home_lon() + 75.162).abs() < 1e-5);
        assert!((f.altitude_m() - 40.84).abs() < 0.01 && (f.height_m() - 3.657).abs() < 0.01);
        assert_eq!((f.v_north, f.v_east, f.v_up), (-5, 7, 0));
        assert!((f.yaw_deg() - 169.0).abs() < 1e-9);
        assert_eq!(f.gps_time_ms, 1_700_000_000_123);
        assert_eq!(f.product_name(), Some("Mini 2"));
        assert_eq!(f.uuid_str(), Some("ABCDEF"));
    }

    #[test]
    fn info11_record() {
        let mut b = [0u8; BLOCK];
        b[0] = 141;
        b[1] = 0x11;
        b[2..12].copy_from_slice(b"TESTSERIAL");
        let c = dji_crc16(&b[..142]);
        b[142..144].copy_from_slice(&c.to_le_bytes());
        let p = parse(&b);
        assert_eq!(p.crc16_ok, Some(true));
        let Record::Info11 { serial, len } = p.record else { panic!("{p:?}") };
        assert_eq!((&serial[..10], len), (&b"TESTSERIAL"[..], 141));
    }

    #[test]
    fn crc16_failure_is_reported_not_hidden() {
        let mut b = sample_block();
        b[40] ^= 1;
        let p = parse(&b);
        assert_eq!(p.crc16_ok, Some(false));
        assert!(matches!(p.record, Record::FlightInfo(_)));
    }

    fn o4_core(kind: EncKind) -> Vec<u8> {
        let mut c = vec![0u8; kind.core_len()];
        let (sel, magic, iv_at, len_at, len) = match kind {
            EncKind::Cryp => (0x13, b"CRYP", 121, 129, 32u16),
            EncKind::Infp10 => (0x10, b"INFP", 9, 17, 109),
            EncKind::Infp11 => (0x11, b"INFP", 9, 17, 141),
        };
        c[0] = sel;
        c[1..5].copy_from_slice(magic);
        c[5..9].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
        c[iv_at..iv_at + 8].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        c[len_at..len_at + 2].copy_from_slice(&len.to_le_bytes());
        for (i, v) in c.iter_mut().enumerate().skip(len_at + 2) {
            *v = (i * 37) as u8; // stand-in ciphertext
        }
        c
    }

    #[test]
    fn o4_records_in_logical_envelope() {
        // Built from the published layout only; untested on real signals.
        for (kind, pad) in [(EncKind::Cryp, 7), (EncKind::Infp10, 0), (EncKind::Infp11, 7)] {
            let core = o4_core(kind);
            let l = core.len() + pad;
            let mut b = [0u8; BLOCK];
            b[0] = l as u8;
            b[1..1 + core.len()].copy_from_slice(&core);
            let c = dji_crc16(&b[..l + 1]);
            b[l + 1..l + 3].copy_from_slice(&c.to_le_bytes());
            let p = parse(&b);
            assert_eq!(p.crc16_ok, Some(true), "{kind:?}");
            let Record::Encrypted(e) = p.record else { panic!("{kind:?}: {p:?}") };
            assert_eq!(e.kind, kind);
            assert_eq!(e.key_id, [0xde, 0xad, 0xbe, 0xef]);
            assert_eq!(e.iv, [1, 2, 3, 4, 5, 6, 7, 8]);
            assert!(e.layout_ok);
        }
    }

    #[test]
    fn o4_record_in_v1_envelope_and_bare() {
        let core = o4_core(EncKind::Infp10);
        let len = 11 + core.len() + 2;
        let mut b = [0u8; BLOCK];
        b[0] = 0x55;
        b[1] = len as u8;
        b[2] = (len >> 8) as u8 & 3;
        b[3] = dji_crc8(&b[..3]);
        b[9] = 0x03;
        b[10] = 0xda;
        b[11..11 + core.len()].copy_from_slice(&core);
        let c = dji_crc16(&b[..len - 2]);
        b[len - 2..len].copy_from_slice(&c.to_le_bytes());
        let p = parse(&b);
        assert_eq!(p.envelope, Envelope::V1 { len: len as u16 });
        assert_eq!(p.crc16_ok, Some(true));
        assert!(matches!(p.record, Record::Encrypted(Encrypted { kind: EncKind::Infp10, .. })));

        let core = o4_core(EncKind::Infp11);
        let mut b = [0u8; BLOCK];
        b[..core.len()].copy_from_slice(&core);
        let p = parse(&b);
        assert_eq!(p.envelope, Envelope::Direct);
        assert!(matches!(p.record, Record::Encrypted(Encrypted { kind: EncKind::Infp11, key_id: [0xde, 0xad, 0xbe, 0xef], .. })));
    }

    fn o4_block(kind: EncKind) -> [u8; BLOCK] {
        let core = o4_core(kind);
        let l = core.len() + O4_PADDING;
        let mut b = [0u8; BLOCK];
        b[0] = l as u8;
        b[1..1 + core.len()].copy_from_slice(&core);
        let c = dji_crc16(&b[..l + 1]);
        b[l + 1..l + 3].copy_from_slice(&c.to_le_bytes());
        b
    }

    fn reseal(b: &mut [u8; BLOCK]) {
        let l = b[0] as usize;
        let c = dji_crc16(&b[..l + 1]);
        b[l + 1..l + 3].copy_from_slice(&c.to_le_bytes());
    }

    fn enc(b: &[u8; BLOCK]) -> Encrypted {
        match parse(b).record {
            Record::Encrypted(e) => e,
            r => panic!("{r:?}"),
        }
    }

    #[test]
    fn o4_spec_layout_is_valid_and_exposes_the_raw_core() {
        for kind in [EncKind::Cryp, EncKind::Infp10, EncKind::Infp11] {
            let b = o4_block(kind);
            let e = enc(&b);
            assert!(e.valid(), "{kind:?} {:?}", e.checks);
            assert_eq!(e.checks.first_failure(), None);
            assert_eq!(e.core(&b), &o4_core(kind)[..]);
            assert_eq!(e.ciphertext(&b).len(), match kind {
                EncKind::Cryp => 32,
                EncKind::Infp10 => 109,
                EncKind::Infp11 => 141,
            });
            assert_eq!(e.logical_object(&b).unwrap().len(), kind.core_len() + O4_PADDING + 3);
        }
        // CRYP fills the slot exactly: 1 + 163 + 7 + 2 = 173.
        assert_eq!(EncKind::Cryp.core_len() + O4_PADDING + 3, BLOCK);
    }

    #[test]
    fn o4_every_check_can_fail() {
        let kind = EncKind::Infp10;
        // CRC-16.
        let mut b = o4_block(kind);
        b[40] ^= 1;
        assert_eq!(enc(&b).checks.first_failure(), Some("crc16"));
        // Non-zero padding inside the object (CRC fixed up).
        let mut b = o4_block(kind);
        b[1 + kind.core_len() + 3] = 0x5a;
        reseal(&mut b);
        assert_eq!(enc(&b).checks.first_failure(), Some("padding"));
        // Stale bytes after the object.
        let mut b = o4_block(kind);
        b[BLOCK - 1] = 1;
        assert_eq!(enc(&b).checks.first_failure(), Some("tail"));
        // Wrong length field.
        let mut b = o4_block(kind);
        b[1 + 17] = 108;
        reseal(&mut b);
        assert_eq!(enc(&b).checks.first_failure(), Some("declared_len"));
        // Object two bytes short of core + padding.
        let mut b = o4_block(kind);
        b[0] -= 2;
        reseal(&mut b);
        assert_eq!(enc(&b).checks.first_failure(), Some("core_len"));
        // An INFP marker on the CRYP selector is not O4.
        let mut b = o4_block(kind);
        b[1] = 0x13;
        reseal(&mut b);
        assert!(!matches!(parse(&b).record, Record::Encrypted(_)));
        // A V1-framed core is recognised but never valid.
        let core = o4_core(kind);
        let len = 11 + core.len() + 2;
        let mut b = [0u8; BLOCK];
        b[0] = 0x55;
        b[1] = len as u8;
        b[3] = dji_crc8(&b[..3]);
        b[9..11].copy_from_slice(&[0x03, 0xda]);
        b[11..11 + core.len()].copy_from_slice(&core);
        let c = dji_crc16(&b[..len - 2]);
        b[len - 2..len].copy_from_slice(&c.to_le_bytes());
        assert_eq!(enc(&b).checks.first_failure(), Some("boundary"));
    }

    #[test]
    fn legacy_plaintext_unchanged_next_to_o4() {
        // Selector 0x10 without the INFP marker stays OcuSync 2 flight info.
        let p = parse(&sample_block());
        assert!(matches!(p.record, Record::FlightInfo(_)));
        assert_eq!(p.crc16_ok, Some(true));
    }
}
