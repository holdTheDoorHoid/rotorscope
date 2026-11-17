//! CRSF telemetry frames, as ExpressLRS carries them in its downlink.
//! Frame: `[addr][len][type][payload: len-2][crc8]`, CRC-8/DVB-S2 over type
//! and payload. Field layouts from ExpressLRS `src/lib/CrsfProtocol/
//! crsf_protocol.h` (GPLv3): all multi-byte fields big-endian.

use crate::content::{AltRef, ControlContent, Position, PositionKind, PositionSource, Telemetry};
use crate::crc::crc8_d5;

pub const TYPE_GPS: u8 = 0x02;
pub const TYPE_VARIO: u8 = 0x07;
pub const TYPE_BATTERY: u8 = 0x08;
pub const TYPE_BARO_ALT: u8 = 0x09;
pub const TYPE_LINK_STATS: u8 = 0x14;
pub const TYPE_ATTITUDE: u8 = 0x1E;
pub const TYPE_FLIGHT_MODE: u8 = 0x21;
/// ELRS's own address for "this frame is MAVLink" in MAVLink link mode.
pub const ADDR_USB: u8 = 0x10;

/// Total length of the frame starting at `buf[0]`, if the length byte is
/// sane (CRSF frames are at most 64 bytes).
pub fn frame_len(buf: &[u8]) -> Option<usize> {
    let len = *buf.get(1)? as usize;
    (2..=62).contains(&len).then_some(len + 2)
}

/// Decode one complete CRSF frame. `None` if the CRC fails.
pub fn decode(frame: &[u8]) -> Option<ControlContent> {
    let total = frame_len(frame)?;
    let f = frame.get(..total)?;
    let body = &f[2..total - 1];
    if crc8_d5(body) != f[total - 1] {
        return None;
    }
    let (&ty, p) = body.split_first()?;
    let be16 = |o: usize| -> Option<u16> { Some(u16::from_be_bytes([*p.get(o)?, *p.get(o + 1)?])) };
    let be32 = |o: usize| -> Option<i32> {
        Some(i32::from_be_bytes([
            *p.get(o)?,
            *p.get(o + 1)?,
            *p.get(o + 2)?,
            *p.get(o + 3)?,
        ]))
    };
    Some(match ty {
        TYPE_GPS => ControlContent::Position(Position {
            kind: PositionKind::Aircraft,
            lat_e7: be32(0)?,
            lon_e7: be32(4)?,
            // altitude: metres + 1000
            alt_mm: Some(((be16(12)? as i32 - 1000) * 1000, AltRef::Msl)),
            rel_alt_mm: None,
            // groundspeed: km/h * 10 -> cm/s
            ground_speed_cms: Some((be16(8)? as u32 * 100 / 36).min(u16::MAX as u32) as u16),
            heading_cdeg: Some(be16(10)?),
            satellites: Some(*p.get(14)?),
            fix_type: None,
            source: PositionSource::CrsfGps,
        }),
        TYPE_BATTERY => ControlContent::Telemetry(Telemetry::Battery {
            voltage_mv: be16(0)? as u32 * 100,
            current_ma: Some(be16(2)? as i32 * 100),
            used_mah: Some(u32::from_be_bytes([0, *p.get(4)?, *p.get(5)?, *p.get(6)?])),
            remaining_pct: Some(*p.get(7)?),
        }),
        TYPE_BARO_ALT => {
            let raw = be16(0)?;
            let alt_dm = if raw & 0x8000 != 0 {
                (raw & 0x7FFF) as i32 * 10
            } else {
                raw as i32 - 10000
            };
            ControlContent::Telemetry(Telemetry::BaroAltitude {
                alt_dm,
                vario_cms: be16(2).map(|v| v as i16),
            })
        }
        TYPE_VARIO => ControlContent::Telemetry(Telemetry::Vario {
            cms: be16(0)? as i16,
        }),
        TYPE_LINK_STATS => ControlContent::Telemetry(Telemetry::Link {
            uplink_rssi_dbm: -(*p.first()? as i16),
            uplink_lq: *p.get(2)?,
            uplink_snr_db: *p.get(3)? as i8,
            downlink_rssi_dbm: Some(-(*p.get(7)? as i16)),
            downlink_lq: Some(*p.get(8)?),
        }),
        TYPE_ATTITUDE => {
            // radians * 10000 -> centidegrees
            let cd = |v: u16| (v as i16 as i32) * 18000 / 31416;
            ControlContent::Telemetry(Telemetry::Attitude {
                pitch_cdeg: cd(be16(0)?),
                roll_cdeg: cd(be16(2)?),
                yaw_cdeg: cd(be16(4)?),
            })
        }
        TYPE_FLIGHT_MODE => {
            let end = p.iter().position(|&b| b == 0).unwrap_or(p.len());
            let s = core::str::from_utf8(&p[..end]).ok()?;
            let mut out = heapless::String::new();
            for c in s.chars().take(16) {
                let _ = out.push(c);
            }
            ControlContent::Telemetry(Telemetry::FlightMode(out))
        }
        other => ControlContent::Telemetry(Telemetry::Other { kind: other as u32 }),
    })
}

/// Test-only encoder: wrap a payload in a CRSF frame.
#[cfg(test)]
pub fn encode(addr: u8, ty: u8, payload: &[u8]) -> std::vec::Vec<u8> {
    let mut f = std::vec![addr, (payload.len() + 2) as u8, ty];
    f.extend_from_slice(payload);
    let c = crc8_d5(&f[2..]);
    f.push(c);
    f
}

#[cfg(test)]
pub fn gps_payload(
    lat_e7: i32,
    lon_e7: i32,
    kmh10: u16,
    hdg_cdeg: u16,
    alt_m: i32,
    sats: u8,
) -> std::vec::Vec<u8> {
    let mut p = std::vec::Vec::new();
    p.extend_from_slice(&lat_e7.to_be_bytes());
    p.extend_from_slice(&lon_e7.to_be_bytes());
    p.extend_from_slice(&kmh10.to_be_bytes());
    p.extend_from_slice(&hdg_cdeg.to_be_bytes());
    p.extend_from_slice(&((alt_m + 1000) as u16).to_be_bytes());
    p.push(sats);
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gps_round_trip() {
        let f = encode(
            0xEA,
            TYPE_GPS,
            &gps_payload(399_526_000, -751_652_000, 360, 9000, 42, 11),
        );
        let Some(ControlContent::Position(p)) = decode(&f) else {
            panic!()
        };
        assert_eq!((p.lat_e7, p.lon_e7), (399_526_000, -751_652_000));
        assert_eq!(p.alt_mm, Some((42_000, AltRef::Msl)));
        assert_eq!(p.ground_speed_cms, Some(1000));
        assert_eq!(p.satellites, Some(11));
        assert!(p.is_plausible());
    }

    #[test]
    fn battery_and_mode() {
        let f = encode(0xEA, TYPE_BATTERY, &[0, 168, 0, 125, 0, 2, 0x58, 77]);
        assert_eq!(
            decode(&f),
            Some(ControlContent::Telemetry(Telemetry::Battery {
                voltage_mv: 16_800,
                current_ma: Some(12_500),
                used_mah: Some(600),
                remaining_pct: Some(77)
            }))
        );
        let f = encode(0xEA, TYPE_FLIGHT_MODE, b"ACRO\0");
        let Some(ControlContent::Telemetry(Telemetry::FlightMode(s))) = decode(&f) else {
            panic!()
        };
        assert_eq!(s.as_str(), "ACRO");
    }

    #[test]
    fn bad_crc_rejected() {
        let mut f = encode(0xEA, TYPE_VARIO, &[0, 10]);
        let n = f.len();
        f[n - 1] ^= 1;
        assert_eq!(decode(&f), None);
    }
}
