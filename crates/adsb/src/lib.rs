//! Parser for **CSBee**, the text output of the ADSBee 1090 (Pants for Birds,
//! <https://github.com/PantsForBirds/adsbee>, GPL-3.0). `no_std`, no
//! allocator, no dependencies.
//!
//! Turn it on with `AT+PROTOCOL_OUT=COMMS_UART,CSBEE` (the UART header) or
//! `AT+PROTOCOL_OUT=CONSOLE,CSBEE` (the USB console). Once a second the
//! ADSBee writes one line per aircraft in its table, then one statistics
//! line:
//!
//! ```text
//! #A:ICAO,FLAGS,CALL,SQUAWK,ECAT,LAT,LON,BARO_ALT,GNSS_ALT,DIR,SPEED,BARO_VRATE,GNSS_VRATE,NICNAC,ACDIMS,VERSION,SIGS,SIGQ,SFPS,ESFPS,CRC\r\n
//! #U:ICAO,FLAGS,CALL,SQUAWK,ECAT,LAT,LON,BARO_ALT,GNSS_ALT,DIR,SPEED,BARO_VRATE,GNSS_VRATE,UAT_EMERG,NICNAC,ACDIMS,VERSION,SIGS,SIGQ,UATFPS,CRC\r\n
//! #S:VERSION,SDPS,RAW_SFPS,SFPS,RAW_ESFPS,ESFPS,RAW_UATFPS,UATFPS,NUM_AIRCRAFT,TSCAL,UPTIME,CRC\r\n
//! ```
//!
//! The layout follows the `snprintf` calls in the ADSBee's
//! `firmware/common/comms/csbee/csbee_utils.hh` at tag `adsbee_1090-0.9.0`
//! (CSBee protocol version 2), **not** the comment above them, which lists
//! an older order. `#A:` is an aircraft heard on 1090 MHz (Mode S / ADS-B),
//! `#U:` one heard on 978 MHz (UAT, ADSBee 1090U only). Units are the
//! ADSBee's: feet, knots, feet per minute, degrees, dBm.
//!
//! # Checksum
//! The last field is a CRC-16 of every byte from the `#` up to and including
//! the comma before it, printed as 1-4 upper-case hex digits without leading
//! zeros. It is CRC-16/CCITT-FALSE (polynomial 0x1021, initial value 0xFFFF,
//! no reflection, no final XOR) **with its two bytes swapped**, as the
//! ADSBee's `CalculateCRC16` (`firmware/common/utils/buffer_utils.cpp`)
//! returns it. See [`crc16`]. A line whose CRC does not match is refused.
//!
//! # Robustness
//! [`parse`] never panics and refuses anything it cannot fully account for:
//! wrong prefix, wrong field count, a field that does not parse, a bad CRC,
//! or non-ASCII bytes. [`LineSplitter`] turns a raw UART byte stream into
//! lines, resynchronising on `#` (which never appears inside a line) so one
//! lost byte costs one line, not two.
//!
//! No ADSBee code is copied; only the format is followed.
#![no_std]

mod crc;
#[cfg(feature = "encode")]
pub mod encode;
mod split;

#[cfg(test)]
mod tests;

pub use crc::crc16;
pub use split::LineSplitter;

/// The CSBee protocol version this parser follows (first field of `#S:`).
pub const PROTOCOL_VERSION: u16 = 2;
/// Longest line the ADSBee writes, `\r\n` included (`kCSBeeMessageStrMaxLen`).
pub const MAX_LINE: usize = 200;
/// The ADSBee's default COMMS_UART baud rate (8N1).
pub const DEFAULT_BAUD: u32 = 115_200;

/// Why a line was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Not a CSBee line: no `#A:`, `#U:` or `#S:` prefix. On the USB console
    /// this is normal (log messages, AT replies); on the UART it is noise.
    NotCsbee,
    /// Longer than [`MAX_LINE`].
    TooLong,
    /// A byte that is not printable ASCII.
    NotText,
    /// Too many or too few fields: a cut-off line, two lines run together,
    /// or firmware with a different layout.
    FieldCount,
    /// The checksum is not 1-4 hex digits.
    NoCrc,
    /// The checksum does not match the line.
    BadCrc,
    /// Field `n` did not parse (0 = the first field after the `#X:` tag).
    BadField(u8),
}

/// One parsed line.
#[derive(Debug, Clone, PartialEq)]
pub enum Report {
    Aircraft(Aircraft),
    Stats(Stats),
}

/// Which receiver heard an aircraft.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    /// 1090 MHz Mode S / ADS-B (`#A:` lines).
    ModeS,
    /// 978 MHz UAT (`#U:` lines; ADSBee 1090U only).
    Uat,
}

/// Bit numbers in [`Aircraft::flags`]. Bits 0-7 and 23-29 mean the same on
/// both receivers; the others differ (see the ADSBee's
/// `aircraft_dictionary.hh`), so only the shared ones are named here, plus
/// [`flag::MODE_S_MILITARY`].
pub mod flag {
    /// Messages say the aircraft is airborne.
    pub const AIRBORNE: u32 = 0;
    pub const BARO_ALT_VALID: u32 = 1;
    pub const GNSS_ALT_VALID: u32 = 2;
    pub const POSITION_VALID: u32 = 3;
    pub const DIRECTION_VALID: u32 = 4;
    pub const SPEED_VALID: u32 = 5;
    pub const BARO_VRATE_VALID: u32 = 6;
    pub const GNSS_VRATE_VALID: u32 = 7;
    /// Mode S only: at least one military extended squitter was heard.
    pub const MODE_S_MILITARY: u32 = 8;
    /// The bits from here to 29 are set when that value was updated during
    /// the last reporting interval (one second) and cleared after each report.
    pub const UPDATED_BARO_ALT: u32 = 23;
    pub const UPDATED_GNSS_ALT: u32 = 24;
    pub const UPDATED_POSITION: u32 = 25;
    pub const UPDATED_DIRECTION: u32 = 26;
    pub const UPDATED_SPEED: u32 = 27;
    pub const UPDATED_BARO_VRATE: u32 = 28;
    pub const UPDATED_GNSS_VRATE: u32 = 29;
}

/// A callsign (flight ID), up to 8 characters. Empty when the aircraft has
/// not sent one (the ADSBee prints `?`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Callsign {
    bytes: [u8; 8],
    len: u8,
}

impl Callsign {
    /// From text: spaces trimmed, `?` means none. `None` if longer than 8
    /// characters or containing anything but printable ASCII (no comma).
    pub fn new(text: &str) -> Option<Self> {
        let t = text.trim_matches(' ');
        if t == "?" || t.is_empty() {
            return Some(Self::default());
        }
        if t.len() > 8
            || !t
                .bytes()
                .all(|b| (0x20..=0x7e).contains(&b) && b != b',' && b != b'#')
        {
            return None;
        }
        let mut bytes = [0u8; 8];
        bytes[..t.len()].copy_from_slice(t.as_bytes());
        Some(Self {
            bytes,
            len: t.len() as u8,
        })
    }

    pub fn as_str(&self) -> &str {
        // Only printable ASCII is ever stored.
        core::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or("")
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl core::fmt::Debug for Callsign {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}

/// ADS-B emitter category (DO-260B), as the ADSBee numbers it
/// (`ADSBTypes::EmitterCategory`), grouped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Unknown,
    /// < 7 t (most general aviation).
    Light,
    /// 7-136 t.
    Medium,
    /// > 136 t.
    Heavy,
    /// High-vortex large (e.g. B757).
    HighVortex,
    HighPerformance,
    /// Helicopters: medevac, police, news.
    Rotorcraft,
    Glider,
    LighterThanAir,
    Parachutist,
    Ultralight,
    /// An unmanned aircraft that carries ADS-B.
    Uav,
    Space,
    /// Airport emergency or service vehicle on the ground.
    SurfaceVehicle,
    /// Fixed obstacle (tower, cable).
    Obstacle,
}

impl Kind {
    /// From the ADSBee's emitter category number.
    pub const fn of(category: i8) -> Kind {
        match category {
            1 => Kind::Light,
            2 | 3 => Kind::Medium,
            4 => Kind::HighVortex,
            5 => Kind::Heavy,
            6 => Kind::HighPerformance,
            7 => Kind::Rotorcraft,
            9 => Kind::Glider,
            10 => Kind::LighterThanAir,
            11 => Kind::Parachutist,
            12 => Kind::Ultralight,
            14 => Kind::Uav,
            15 => Kind::Space,
            17 | 18 => Kind::SurfaceVehicle,
            19..=21 => Kind::Obstacle,
            _ => Kind::Unknown,
        }
    }

    /// A short English name.
    pub const fn label(self) -> &'static str {
        match self {
            Kind::Unknown => "Aircraft",
            Kind::Light => "Light aircraft",
            Kind::Medium => "Medium aircraft",
            Kind::Heavy => "Heavy aircraft",
            Kind::HighVortex => "Large aircraft",
            Kind::HighPerformance => "High performance",
            Kind::Rotorcraft => "Helicopter",
            Kind::Glider => "Glider",
            Kind::LighterThanAir => "Balloon/airship",
            Kind::Parachutist => "Parachutist",
            Kind::Ultralight => "Ultralight",
            Kind::Uav => "Drone (ADS-B)",
            Kind::Space => "Space vehicle",
            Kind::SurfaceVehicle => "Ground vehicle",
            Kind::Obstacle => "Obstacle",
        }
    }

    /// The JSON / CSV name.
    pub const fn name(self) -> &'static str {
        match self {
            Kind::Unknown => "Unknown",
            Kind::Light => "Light",
            Kind::Medium => "Medium",
            Kind::Heavy => "Heavy",
            Kind::HighVortex => "HighVortex",
            Kind::HighPerformance => "HighPerformance",
            Kind::Rotorcraft => "Rotorcraft",
            Kind::Glider => "Glider",
            Kind::LighterThanAir => "LighterThanAir",
            Kind::Parachutist => "Parachutist",
            Kind::Ultralight => "Ultralight",
            Kind::Uav => "Uav",
            Kind::Space => "Space",
            Kind::SurfaceVehicle => "SurfaceVehicle",
            Kind::Obstacle => "Obstacle",
        }
    }
}

/// One `#A:` or `#U:` line, field for field. Most values are only
/// meaningful when their flag says so: use the accessor methods
/// ([`Aircraft::position`], [`Aircraft::baro_altitude_ft`], ...), which
/// apply the flags, rather than the raw fields.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aircraft {
    pub source: Source,
    /// The ICAO field as printed: the 24-bit address, and for UAT the
    /// address qualifier in bits 24-27 ([`Aircraft::icao24`],
    /// [`Aircraft::address_qualifier`]).
    pub icao: u32,
    pub flags: u32,
    pub callsign: Callsign,
    /// Mode A code as its numeric value (print with `{:04o}`): 7700 is
    /// `0o7700`. `None` until received.
    pub squawk: Option<u16>,
    /// Emitter category number; see [`Kind::of`]. -1 = not received.
    pub category: i8,
    pub lat_deg: f64,
    pub lon_deg: f64,
    /// Pressure altitude (29.92 inHg), feet.
    pub baro_alt_ft: i32,
    /// Geometric altitude (height above the WGS-84 ellipsoid), feet.
    pub gnss_alt_ft: i32,
    /// Track over ground, or heading (flag bit 14 on Mode S, 10 on UAT).
    /// Printed rounded to whole degrees.
    pub direction_deg: f32,
    pub speed_kt: i32,
    pub baro_vrate_fpm: i32,
    pub gnss_vrate_fpm: i32,
    /// UAT emergency/priority status (0 = none). Always 0 for Mode S.
    pub uat_emergency: u8,
    /// Integrity and accuracy (SIL, GVA, NACp, NACv, NICbaro, NIC) packed.
    pub nicnac: u16,
    /// System design assurance, size and GNSS antenna offset packed.
    pub acdims: u32,
    /// ADS-B (or UAT) version the aircraft reports.
    pub version: u8,
    /// Signal strength of the last message, dBm.
    pub signal_dbm: i16,
    /// Mode S: signal over noise, dB. UAT: bits corrected by FEC.
    pub signal_quality: i16,
    /// Mode S: valid short squitters in the last second. UAT: valid frames.
    pub frames_per_s: u16,
    /// Mode S: valid extended squitters in the last second. UAT: 0.
    pub es_frames_per_s: u16,
}

impl Aircraft {
    fn has(&self, bit: u32) -> bool {
        self.flags & (1 << bit) != 0
    }

    /// The 24-bit ICAO (or self-assigned) address.
    pub fn icao24(&self) -> u32 {
        self.icao & 0x00ff_ffff
    }

    /// UAT address qualifier (0 = ADS-B with ICAO address, 1 = self-assigned,
    /// 2/3 = TIS-B, 4 = surface vehicle, 5 = beacon). 0 for Mode S.
    pub fn address_qualifier(&self) -> u8 {
        ((self.icao >> 24) & 0x0f) as u8
    }

    pub fn kind(&self) -> Kind {
        Kind::of(self.category)
    }

    /// Latitude and longitude, when the position flag is set and the
    /// numbers are a real position (not the 0,0 the ADSBee prints before
    /// it has one).
    pub fn position(&self) -> Option<(f64, f64)> {
        let (lat, lon) = (self.lat_deg, self.lon_deg);
        let sane = lat.is_finite()
            && lon.is_finite()
            && (-90.0..=90.0).contains(&lat)
            && (-180.0..=180.0).contains(&lon)
            && !(lat == 0.0 && lon == 0.0);
        (self.has(flag::POSITION_VALID) && sane).then_some((lat, lon))
    }

    /// The position was updated in the last reporting interval, i.e. it is
    /// at most about a second old. Only believed on a line that also heard
    /// the aircraft ([`Self::heard`]): the `UPDATED_*` bits can stay set on
    /// lines that repeat old values.
    pub fn position_updated(&self) -> bool {
        self.heard() && self.has(flag::UPDATED_POSITION)
    }

    pub fn baro_altitude_ft(&self) -> Option<i32> {
        self.has(flag::BARO_ALT_VALID).then_some(self.baro_alt_ft)
    }

    pub fn gnss_altitude_ft(&self) -> Option<i32> {
        self.has(flag::GNSS_ALT_VALID).then_some(self.gnss_alt_ft)
    }

    /// Direction of travel (or heading), 0 to <360 degrees.
    pub fn direction_deg(&self) -> Option<f32> {
        let d = self.direction_deg;
        (self.has(flag::DIRECTION_VALID) && d.is_finite()).then(|| {
            let r = d % 360.0;
            if r < 0.0 { r + 360.0 } else { r }
        })
    }

    /// Ground speed (or airspeed), knots.
    pub fn speed_kt(&self) -> Option<i32> {
        self.has(flag::SPEED_VALID).then_some(self.speed_kt)
    }

    /// Vertical rate, feet per minute, up positive: barometric if known,
    /// else geometric.
    pub fn vertical_rate_fpm(&self) -> Option<i32> {
        if self.has(flag::BARO_VRATE_VALID) {
            Some(self.baro_vrate_fpm)
        } else {
            self.has(flag::GNSS_VRATE_VALID)
                .then_some(self.gnss_vrate_fpm)
        }
    }

    pub fn airborne(&self) -> bool {
        self.has(flag::AIRBORNE)
    }

    /// Mode S only: a military extended squitter was heard.
    pub fn military(&self) -> bool {
        self.source == Source::ModeS && self.has(flag::MODE_S_MILITARY)
    }

    /// The ADSBee heard this aircraft during the last reporting interval.
    /// It keeps listing an aircraft for a while after it goes quiet; such a
    /// line repeats old data and should not count as a fresh sighting.
    ///
    /// Judged by the per-second message counts only. The `UPDATED_*` flag
    /// bits are not trusted for this: on ADSBee 0.9.0 they were seen staying
    /// set on lines that only repeated old values (docs/ADSBEE.md), which
    /// made a quiet aircraft look current.
    pub fn heard(&self) -> bool {
        self.frames_per_s > 0 || self.es_frames_per_s > 0
    }

    /// The emergency a squawk code declares, if any.
    pub fn emergency(&self) -> Option<&'static str> {
        match self.squawk? {
            0o7500 => Some("hijack"),
            0o7600 => Some("radio failure"),
            0o7700 => Some("emergency"),
            _ => None,
        }
    }
}

/// One `#S:` line: the ADSBee's receiver statistics for the last second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    /// CSBee protocol version ([`PROTOCOL_VERSION`] for this parser).
    pub version: u16,
    /// Demodulations attempted per second.
    pub demods_per_s: u16,
    pub raw_squitters_per_s: u16,
    pub squitters_per_s: u16,
    pub raw_extended_squitters_per_s: u16,
    pub extended_squitters_per_s: u16,
    pub raw_uat_per_s: u16,
    pub uat_per_s: u16,
    /// Aircraft in the ADSBee's own table.
    pub aircraft: u16,
    /// Reserved (always 0 on 0.9.0).
    pub tscal: u32,
    pub uptime_s: u32,
}

impl Stats {
    /// Valid frames per second over both receivers.
    pub fn valid_per_s(&self) -> u32 {
        u32::from(self.squitters_per_s)
            + u32::from(self.extended_squitters_per_s)
            + u32::from(self.uat_per_s)
    }
}

/// Fields after the tag, CRC excluded.
const AIRCRAFT_FIELDS: usize = 20;
const STATS_FIELDS: usize = 11;

/// Parse one line. A trailing `\r\n` (or `\n`, or `\r`) is allowed and ignored.
pub fn parse(line: &[u8]) -> Result<Report, Error> {
    let mut line = line;
    while let [rest @ .., b'\r' | b'\n'] = line {
        line = rest;
    }
    if line.len() > MAX_LINE {
        return Err(Error::TooLong);
    }
    let tag = match line {
        [b'#', t @ (b'A' | b'U' | b'S'), b':', ..] => *t,
        _ => return Err(Error::NotCsbee),
    };
    if !line.iter().all(|b| (0x20..=0x7e).contains(b)) {
        return Err(Error::NotText);
    }
    // Printable ASCII, so this cannot fail.
    let text = core::str::from_utf8(line).map_err(|_| Error::NotText)?;
    let comma = text.rfind(',').ok_or(Error::FieldCount)?;
    if comma < 3 {
        return Err(Error::FieldCount);
    }
    let crc_text = &text[comma + 1..];
    if crc_text.is_empty() || crc_text.len() > 4 || !crc_text.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(Error::NoCrc);
    }
    let crc = u16::from_str_radix(crc_text, 16).map_err(|_| Error::NoCrc)?;
    if crc16(&line[..=comma]) != crc {
        return Err(Error::BadCrc);
    }

    let mut f = Fields::new(&text[3..comma]);
    let report = match tag {
        b'S' => Report::Stats(Stats {
            version: f.num()?,
            demods_per_s: f.num()?,
            raw_squitters_per_s: f.num()?,
            squitters_per_s: f.num()?,
            raw_extended_squitters_per_s: f.num()?,
            extended_squitters_per_s: f.num()?,
            raw_uat_per_s: f.num()?,
            uat_per_s: f.num()?,
            aircraft: f.num()?,
            tscal: f.num()?,
            uptime_s: f.num()?,
        }),
        _ => {
            let source = if tag == b'A' {
                Source::ModeS
            } else {
                Source::Uat
            };
            let icao = f.hex()?;
            if icao > 0x0fff_ffff {
                return Err(f.bad());
            }
            let flags = f.hex()?;
            let callsign = f.callsign()?;
            let squawk = f.squawk()?;
            let category = f.num()?;
            let lat_deg = f.num()?;
            let lon_deg = f.num()?;
            let baro_alt_ft = f.num()?;
            let gnss_alt_ft = f.num()?;
            let direction_deg = f.num()?;
            let speed_kt = f.num()?;
            let baro_vrate_fpm = f.num()?;
            let gnss_vrate_fpm = f.num()?;
            let uat_emergency = if source == Source::Uat { f.num()? } else { 0 };
            let nicnac = f.hex()?;
            let acdims = f.hex()?;
            let version = f.num()?;
            let signal_dbm = f.num()?;
            let signal_quality = f.num()?;
            let frames_per_s = f.num()?;
            let es_frames_per_s = if source == Source::ModeS { f.num()? } else { 0 };
            Report::Aircraft(Aircraft {
                source,
                icao,
                flags,
                callsign,
                squawk,
                category,
                lat_deg,
                lon_deg,
                baro_alt_ft,
                gnss_alt_ft,
                direction_deg,
                speed_kt,
                baro_vrate_fpm,
                gnss_vrate_fpm,
                uat_emergency,
                nicnac,
                acdims,
                version,
                signal_dbm,
                signal_quality,
                frames_per_s,
                es_frames_per_s,
            })
        }
    };
    let want = if tag == b'S' {
        STATS_FIELDS
    } else {
        AIRCRAFT_FIELDS
    };
    if f.taken != want || f.rest.is_some() {
        return Err(Error::FieldCount);
    }
    Ok(report)
}

/// The comma-separated fields of a line, taken in order.
struct Fields<'a> {
    rest: Option<&'a str>,
    taken: usize,
}

impl<'a> Fields<'a> {
    fn new(s: &'a str) -> Self {
        Self {
            rest: Some(s),
            taken: 0,
        }
    }

    fn bad(&self) -> Error {
        Error::BadField(self.taken.saturating_sub(1).min(255) as u8)
    }

    fn next(&mut self) -> Result<&'a str, Error> {
        let s = self.rest.ok_or(Error::FieldCount)?;
        let (field, rest) = match s.split_once(',') {
            Some((a, b)) => (a, Some(b)),
            None => (s, None),
        };
        self.rest = rest;
        self.taken += 1;
        Ok(field)
    }

    /// A decimal number (or, for floats, `%.Nf` text). Rust's `FromStr`
    /// accepts a leading `+` and, for floats, `inf`/`nan`; neither is ever
    /// printed by the ADSBee, so they are refused here.
    fn num<T: core::str::FromStr>(&mut self) -> Result<T, Error> {
        let s = self.next()?;
        let ok = !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_digit() || b == b'-' || b == b'.');
        if !ok {
            return Err(self.bad());
        }
        s.parse().map_err(|_| self.bad())
    }

    /// Upper-case hex as `%X` prints it (lower case accepted too).
    fn hex<T: TryFrom<u32>>(&mut self) -> Result<T, Error> {
        let s = self.next()?;
        if s.is_empty() || s.len() > 8 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(self.bad());
        }
        let v = u32::from_str_radix(s, 16).map_err(|_| self.bad())?;
        T::try_from(v).map_err(|_| self.bad())
    }

    fn callsign(&mut self) -> Result<Callsign, Error> {
        let s = self.next()?;
        Callsign::new(s).ok_or_else(|| self.bad())
    }

    /// Four octal digits, or `?` before one is received.
    fn squawk(&mut self) -> Result<Option<u16>, Error> {
        let s = self.next()?;
        if s == "?" {
            return Ok(None);
        }
        if s.len() != 4 || !s.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
            return Err(self.bad());
        }
        u16::from_str_radix(s, 8).map(Some).map_err(|_| self.bad())
    }
}
