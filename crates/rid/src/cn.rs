//! China's GB 46750-2025 broadcast Remote ID packet.
//!
//! Not wire-compatible with ASTM F3411: one variable-length packet,
//! `0xFF, version, data length, flag bytes..., data...`, where each flag bit
//! announces one fixed-size field and bit 0 of each flag byte chains to the
//! next. Layout and acceptance rules follow core-c's `libopendroneidcn`
//! (`CN46750_RID_Decode`, Apache-2.0), which the differential tests compare
//! against: the same packets are accepted and rejected.
//!
//! Carriage is not settled: reports say DJI's 2026 firmware puts this packet
//! inside the ASTM beacon element (so a [`crate::wifi::WifiRid`] payload that
//! starts with `0xFF 0x2x` is a candidate), while core-c's demo appends it
//! raw to the beacon body. [`find`] scans any byte range for a plausible
//! packet, as core-c's `CN46750_FindPacket` does.

use crate::Error;
use crate::message::{FixedStr, HorizontalAccuracy, LatLon, SpeedAccuracy, VerticalAccuracy};

/// First byte of every packet.
pub const PACKET_TYPE: u8 = 0xFF;
/// Largest data region a packet may declare.
pub const MAX_DATA: usize = 200;

code_enum! {
    /// Field 003: operation category.
    Category {
        Undefined = 0,
        Open = 1,
        Specific = 2,
        Certified = 3,
    }
}

code_enum! {
    /// Field 004: civil UAV class.
    UaClass {
        Micro = 0,
        Light = 1,
        Small = 2,
        Medium = 3,
        Large = 4,
    }
}

code_enum! {
    /// Field 005: what the ground-station position is.
    GcsPositionType {
        Takeoff = 0,
        GroundStation = 1,
    }
}

code_enum! {
    /// Field 015: operation status.
    Status {
        Unknown = 0,
        Ground = 1,
        Airborne = 2,
        Emergency = 3,
        /// Remote ID failure, not an emergency.
        RidFailure = 4,
        /// Remote ID failure during an emergency.
        RidFailureEmergency = 5,
    }
}

code_enum! {
    /// Field 016: datum of all positions in the packet.
    CoordinateSystem {
        Wgs84 = 0,
        Cgcs2000 = 1,
    }
}

/// One decoded GB 46750-2025 packet.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct CnRid {
    /// Version byte bits 4-0 (the "X" of V1.X).
    pub sub_version: u8,
    /// 001: product serial number (GB/T 41300), NUL-padded ASCII.
    pub serial: FixedStr<20>,
    /// 002: last eight characters of the real-name registration mark.
    pub uin: FixedStr<8>,
    /// 003 (optional).
    pub category: Option<Category>,
    /// 004.
    pub class: UaClass,
    /// 005.
    pub gcs_position_type: GcsPositionType,
    /// 006: `None` when either axis is the invalid code or out of range.
    pub gcs_position: Option<LatLon>,
    /// 007: metres; `None` when unknown.
    pub gcs_altitude: Option<f32>,
    /// 008: `None` when either axis is the invalid code or out of range.
    pub position: Option<LatLon>,
    /// 009: degrees from true north, 0.1 steps; `None` when unknown.
    pub track: Option<f32>,
    /// 010: m/s, 0.1 steps; `None` when unknown.
    pub ground_speed: Option<f32>,
    /// 011 (optional): metres above takeoff, 0.5 steps from -9000.
    pub height_above_takeoff: Option<f32>,
    /// 012 (optional): m/s, positive up, 0.5 steps.
    pub speed_vertical: Option<f32>,
    /// 013: metres; `None` when unknown.
    pub altitude_geodetic: Option<f32>,
    /// 014 (optional): metres (ref 101.325 kPa).
    pub altitude_pressure: Option<f32>,
    /// 015.
    pub status: Status,
    /// 016.
    pub coordinate_system: CoordinateSystem,
    /// 017: same NACp-style table as ASTM.
    pub horizontal_accuracy: HorizontalAccuracy,
    /// 018: same table as ASTM.
    pub vertical_accuracy: VerticalAccuracy,
    /// 019: same table as ASTM.
    pub speed_accuracy: SpeedAccuracy,
    /// 020: Unix time in milliseconds; `None` when zero.
    pub timestamp_ms: Option<u64>,
    /// 021: code 0-15 (0 = worse than 0.5 s, 1 = <0.5 s, 2 = <0.4 s, 3 = <0.3 s,
    /// 4 = <0.2 s, 5 = <0.1 s, 6 = <50 ms, 7 = <20 ms, 8 = <10 ms).
    pub timestamp_accuracy: u8,
    /// More than the three defined flag bytes were present; the fields they
    /// announce were skipped.
    pub has_extensions: bool,
}

/// Header byte 1 must have its top three bits `001` (major version 1).
fn version_ok(b: u8) -> bool {
    b & 0xE0 == 0x20
}

/// Number of flag bytes starting at `flags` (bit 0 chains), counting a chain
/// that runs off the end as everything remaining.
fn flag_count(flags: &[u8]) -> usize {
    flags
        .iter()
        .position(|b| b & 0x01 == 0)
        .map_or(flags.len(), |i| i + 1)
}

/// Find the first plausible packet in `bytes` (header and declared length
/// fit), as core-c's `CN46750_FindPacket` does. Returns exactly the packet.
pub fn find(bytes: &[u8]) -> Option<&[u8]> {
    if bytes.len() < 6 {
        return None;
    }
    (0..bytes.len() - 3).find_map(|i| {
        let rest = &bytes[i..];
        if rest[0] != PACKET_TYPE
            || !version_ok(rest[1])
            || !(1..=MAX_DATA).contains(&usize::from(rest[2]))
        {
            return None;
        }
        let flags = flag_count(&rest[3..]);
        rest.get(..3 + flags + usize::from(rest[2]))
    })
}

/// Field reader over the data region.
struct Fields<'a>(&'a [u8]);

impl<'a> Fields<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if self.0.len() < n {
            return Err(Error::Invalid);
        }
        let (head, tail) = self.0.split_at(n);
        self.0 = tail;
        Ok(head)
    }
    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }
    fn nibble(&mut self) -> Result<u8, Error> {
        Ok(self.u8()? & 0x0F)
    }
    fn u16(&mut self) -> Result<u16, Error> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32, Error> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    /// NUL-padded printable ASCII, as core-c validates it.
    fn ascii<const N: usize>(&mut self) -> Result<FixedStr<N>, Error> {
        let b = self.take(N)?;
        if b.iter().any(|&c| c != 0 && !(0x20..=0x7E).contains(&c)) {
            return Err(Error::Invalid);
        }
        FixedStr::new(b)
    }
    /// Longitude then latitude, int32 1e-7 degree, 0xFFFFFFFF per axis = invalid.
    fn lon_lat(&mut self) -> Result<Option<LatLon>, Error> {
        let (lon, lat) = (self.u32()?, self.u32()?);
        if lon == u32::MAX || lat == u32::MAX {
            return Ok(None);
        }
        let p = LatLon {
            lat: f64::from(lat as i32) / 1e7,
            lon: f64::from(lon as i32) / 1e7,
        };
        Ok(((-90.0..=90.0).contains(&p.lat) && (-180.0..=180.0).contains(&p.lon)).then_some(p))
    }
    /// 0.5 m steps from -1000 m; code 0 = unknown.
    fn altitude(&mut self) -> Result<Option<f32>, Error> {
        let code = self.u16()?;
        Ok((code != 0).then(|| f32::from(code) / 2.0 - 1000.0))
    }
}

fn need(present: bool) -> Result<(), Error> {
    if present { Ok(()) } else { Err(Error::Invalid) }
}

/// Decode one packet. `buf` must start at the `0xFF` type byte; bytes after
/// the declared end are ignored. A mandatory field missing from the flags, a
/// field running past the declared data length, a non-ASCII ID, or a track
/// angle above 359.9 is [`Error::Invalid`]; a buffer shorter than the header
/// declares is [`Error::Truncated`].
pub fn decode(buf: &[u8]) -> Result<CnRid, Error> {
    if buf.len() < 4 {
        return Err(Error::Truncated);
    }
    if buf[0] != PACKET_TYPE || !version_ok(buf[1]) {
        return Err(Error::Invalid);
    }
    let data_len = usize::from(buf[2]);
    if !(1..=MAX_DATA).contains(&data_len) {
        return Err(Error::Invalid);
    }
    let n_flags = flag_count(&buf[3..]);
    let data = buf
        .get(3 + n_flags..3 + n_flags + data_len)
        .ok_or(Error::Truncated)?;
    let flag = |i: usize| if i < n_flags { buf[3 + i] } else { 0 };
    let (f1, f2, f3) = (flag(0), flag(1), flag(2));
    let mut d = Fields(data);

    need(f1 & 0x80 != 0)?;
    let serial = d.ascii()?;
    need(f1 & 0x40 != 0)?;
    let uin = d.ascii()?;
    let category = if f1 & 0x20 != 0 {
        Some(Category::from_code(d.nibble()?))
    } else {
        None
    };
    need(f1 & 0x10 != 0)?;
    let class = UaClass::from_code(d.nibble()?);
    need(f1 & 0x08 != 0)?;
    let gcs_position_type = GcsPositionType::from_code(d.nibble()?);
    need(f1 & 0x04 != 0)?;
    let gcs_position = d.lon_lat()?;
    need(f1 & 0x02 != 0)?;
    let gcs_altitude = d.altitude()?;

    need(f2 & 0x80 != 0)?;
    let position = d.lon_lat()?;
    need(f2 & 0x40 != 0)?;
    let track = match d.u16()? {
        0xFFFF => None,
        t if t > 3599 => return Err(Error::Invalid),
        t => Some(f32::from(t) / 10.0),
    };
    need(f2 & 0x20 != 0)?;
    let ground_speed = match d.u16()? {
        0xFFFF => None,
        s => Some(f32::from(s) / 10.0),
    };
    let height_above_takeoff = if f2 & 0x10 != 0 {
        let code = d.u16()?;
        (code != 0).then(|| f32::from(code) / 2.0 - 9000.0)
    } else {
        None
    };
    let speed_vertical = if f2 & 0x08 != 0 {
        match d.u8()? {
            0xFF => None,
            v => {
                let mag = f32::from(v & 0x7F) / 2.0;
                Some(if v & 0x80 != 0 { -mag } else { mag })
            }
        }
    } else {
        None
    };
    need(f2 & 0x04 != 0)?;
    let altitude_geodetic = d.altitude()?;
    let altitude_pressure = if f2 & 0x02 != 0 { d.altitude()? } else { None };

    need(f3 & 0x80 != 0)?;
    let status = Status::from_code(d.nibble()?);
    need(f3 & 0x40 != 0)?;
    let coordinate_system = CoordinateSystem::from_code(d.nibble()?);
    need(f3 & 0x20 != 0)?;
    let horizontal_accuracy = HorizontalAccuracy::from_code(d.nibble()?);
    need(f3 & 0x10 != 0)?;
    let vertical_accuracy = VerticalAccuracy::from_code(d.nibble()?);
    need(f3 & 0x08 != 0)?;
    let speed_accuracy = SpeedAccuracy::from_code(d.nibble()?);
    need(f3 & 0x04 != 0)?;
    let ts = d.take(6)?;
    let timestamp_ms = u64::from_le_bytes([ts[0], ts[1], ts[2], ts[3], ts[4], ts[5], 0, 0]);
    need(f3 & 0x02 != 0)?;
    let timestamp_accuracy = d.nibble()?;

    Ok(CnRid {
        sub_version: buf[1] & 0x1F,
        serial,
        uin,
        category,
        class,
        gcs_position_type,
        gcs_position,
        gcs_altitude,
        position,
        track,
        ground_speed,
        height_above_takeoff,
        speed_vertical,
        altitude_geodetic,
        altitude_pressure,
        status,
        coordinate_system,
        horizontal_accuracy,
        vertical_accuracy,
        speed_accuracy,
        timestamp_ms: (timestamp_ms != 0).then_some(timestamp_ms),
        timestamp_accuracy,
        has_extensions: n_flags > 3,
    })
}
