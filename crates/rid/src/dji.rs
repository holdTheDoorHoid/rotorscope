//! DJI's legacy Wi-Fi "DroneID": a vendor-specific element (OUI `26:37:12`)
//! in the beacons of older Wi-Fi-linked DJI aircraft (Spark, Mavic Air, Mini
//! and others). Not a standard; layout from public research.
//!
//! Element body after the OUI: vendor type, two unknown bytes, a subcommand,
//! then the record. Subcommand 0x10 carries flight information, 0x11 the
//! user-entered purpose.
//!
//! Confidence: the first 29 bytes of the flight-information record (version,
//! sequence, state flags, serial, position) and the whole purpose record are
//! the parts Kismet's parser relies on (read for facts only; no Kismet code
//! is used here). The rest of the flight-information record (altitudes,
//! velocities, attitude, home point, product type, UUID) follows the layout
//! published by DJI DroneID research but has not been verified on air by
//! this project, so it is decoded into [`FlightInfoExt`] only when present
//! and its units are documented as "believed".

use crate::message::{FixedStr, LatLon};
use crate::wifi::{Mgmt, elements};

/// DJI's DroneID OUI.
pub const OUI: [u8; 3] = [0x26, 0x37, 0x12];
/// Subcommand: flight information.
pub const FLIGHT_INFO: u8 = 0x10;
/// Subcommand: flight purpose.
pub const FLIGHT_PURPOSE: u8 = 0x11;

/// A DroneID record found in one beacon.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct DjiBeacon<'a> {
    /// Transmitter address (802.11 addr2).
    pub src: [u8; 6],
    pub record: Record<'a>,
}

/// The record a DroneID element carries.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Record<'a> {
    FlightInfo(FlightInfo),
    FlightPurpose(FlightPurpose<'a>),
    /// A subcommand this crate does not know, or a record too short to read.
    Unknown {
        subcommand: u8,
    },
}

/// Subcommand 0x10.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct FlightInfo {
    pub version: u8,
    pub seq: u16,
    /// Validity and state flags; see the `STATE_*` constants and accessors.
    pub state: u16,
    pub serial: FixedStr<16>,
    /// `None` when zero (privacy mode or no fix) or out of range.
    pub position: Option<LatLon>,
    /// The rest of the record, when the element is long enough to hold it.
    pub ext: Option<FlightInfoExt>,
}

/// The part of the flight-information record after the position. Layout from
/// public research; not yet verified on air by this project.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct FlightInfoExt {
    /// Believed: metres (barometric / takeoff-referenced).
    pub altitude: i16,
    /// Believed: metres above ground or takeoff.
    pub height: i16,
    /// Believed: cm/s, north positive.
    pub v_north: i16,
    /// Believed: cm/s, east positive.
    pub v_east: i16,
    /// Believed: cm/s, up positive.
    pub v_up: i16,
    /// Degrees (transmitted in hundredths).
    pub pitch: f32,
    pub roll: f32,
    pub yaw: f32,
    /// Home point; `None` when zero or out of range.
    pub home: Option<LatLon>,
    /// DJI model code (e.g. 16 = Mavic Pro per public tables).
    pub product_type: u8,
    /// App/session UUID; empty when absent.
    pub uuid: FixedStr<20>,
}

impl FlightInfo {
    pub const STATE_SERIAL_VALID: u16 = 0x0001;
    /// Set when privacy mode is *off* (the position fields are real).
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
    pub const STATE_PITCH_ROLL_VALID: u16 = 0x0800;

    fn flag(&self, bit: u16) -> bool {
        self.state & bit != 0
    }
    pub fn motor_on(&self) -> bool {
        self.flag(Self::STATE_MOTOR_ON)
    }
    pub fn in_air(&self) -> bool {
        self.flag(Self::STATE_IN_AIR)
    }
    pub fn gps_valid(&self) -> bool {
        self.flag(Self::STATE_GPS_VALID)
    }
    pub fn privacy_enabled(&self) -> bool {
        !self.flag(Self::STATE_PRIVACY_DISABLED)
    }
}

/// Subcommand 0x11: what the pilot typed into the app.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct FlightPurpose<'a> {
    pub serial: FixedStr<16>,
    pub drone_id: FixedStr<10>,
    /// Free text, borrowed from the frame. DJI has been seen sending a wrong
    /// length byte, so this is the shorter of the declared length and what
    /// the element actually holds.
    pub purpose: Text<'a>,
}

/// Borrowed text of arbitrary length. Serializes like [`FixedStr`]: a string
/// when printable ASCII, otherwise hex.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Text<'a>(pub &'a [u8]);

impl<'a> Text<'a> {
    pub fn as_str(&self) -> Option<&'a str> {
        core::str::from_utf8(self.0).ok()
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for Text<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self.as_str() {
            Some(t) if t.bytes().all(|b| (0x20..0x7F).contains(&b)) => s.serialize_str(t),
            _ => s.collect_str(&crate::message::Hex(self.0)),
        }
    }
}

/// Find a DroneID element in a beacon. `frame` starts at frame control and
/// excludes the FCS, as for [`crate::wifi::extract`].
pub fn extract(frame: &[u8]) -> Option<DjiBeacon<'_>> {
    let mgmt = Mgmt::parse(frame)?;
    let record = elements(mgmt.beacon_ies()?)
        .find_map(|(id, body)| if id == 221 { parse_element(body) } else { None })?;
    Some(DjiBeacon {
        src: mgmt.src,
        record,
    })
}

/// Parse the body of a vendor-specific element (starting at the OUI).
/// `None` unless the OUI is DJI's and the 4-byte header is present.
pub fn parse_element(body: &[u8]) -> Option<Record<'_>> {
    if body.get(..3)? != OUI {
        return None;
    }
    let subcommand = *body.get(6)?;
    let rec = body.get(7..)?;
    Some(match subcommand {
        FLIGHT_INFO => flight_info(rec).map_or(Record::Unknown { subcommand }, Record::FlightInfo),
        FLIGHT_PURPOSE => {
            flight_purpose(rec).map_or(Record::Unknown { subcommand }, Record::FlightPurpose)
        }
        _ => Record::Unknown { subcommand },
    })
}

fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn le_i32(b: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// DJI sends coordinates in radians * 1e7. Kismet divides by 174533.0, a
/// rounded form of the same factor (they differ by under 0.5 ppm).
fn position(lat: i32, lon: i32) -> Option<LatLon> {
    const RAD_E7_TO_DEG: f64 = 180.0 / core::f64::consts::PI / 1e7;
    let p = LatLon {
        lat: f64::from(lat) * RAD_E7_TO_DEG,
        lon: f64::from(lon) * RAD_E7_TO_DEG,
    };
    let ok = (lat != 0 || lon != 0)
        && (-90.0..=90.0).contains(&p.lat)
        && (-180.0..=180.0).contains(&p.lon);
    ok.then_some(p)
}

fn fixed<const N: usize>(b: &[u8], at: usize) -> Option<FixedStr<N>> {
    FixedStr::new(b.get(at..at + N)?).ok()
}

fn flight_info(r: &[u8]) -> Option<FlightInfo> {
    let ext = (|| {
        let uuid_len = usize::from(*r.get(54)?).min(20);
        let uuid = r.get(55..).map_or(&[][..], |u| &u[..u.len().min(uuid_len)]);
        Some(FlightInfoExt {
            altitude: le16(r, 29)? as i16,
            height: le16(r, 31)? as i16,
            v_north: le16(r, 33)? as i16,
            v_east: le16(r, 35)? as i16,
            v_up: le16(r, 37)? as i16,
            pitch: f32::from(le16(r, 39)? as i16) / 100.0,
            roll: f32::from(le16(r, 41)? as i16) / 100.0,
            yaw: f32::from(le16(r, 43)? as i16) / 100.0,
            home: position(le_i32(r, 49)?, le_i32(r, 45)?),
            product_type: *r.get(53)?,
            uuid: FixedStr::new(uuid).ok()?,
        })
    })();
    Some(FlightInfo {
        version: *r.first()?,
        seq: le16(r, 1)?,
        state: le16(r, 3)?,
        serial: fixed(r, 5)?,
        // Longitude comes first on the wire.
        position: position(le_i32(r, 25)?, le_i32(r, 21)?),
        ext,
    })
}

fn flight_purpose(r: &[u8]) -> Option<FlightPurpose<'_>> {
    let serial = fixed(r, 0)?;
    let id_len = usize::from(*r.get(16)?).min(10);
    let drone_id = FixedStr::new(r.get(17..17 + id_len)?).ok()?;
    let purpose_len = usize::from(*r.get(27)?);
    let rest = r.get(28..)?;
    Some(FlightPurpose {
        serial,
        drone_id,
        purpose: Text(&rest[..purpose_len.min(rest.len())]),
    })
}
