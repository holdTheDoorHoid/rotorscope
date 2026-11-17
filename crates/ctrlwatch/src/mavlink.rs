//! MAVLink v1/v2 frame scanner and the handful of messages worth decoding
//! (common.xml): HEARTBEAT, SYS_STATUS, GPS_RAW_INT, GLOBAL_POSITION_INT,
//! RC_CHANNELS, HOME_POSITION. Frames are found anywhere in a byte slice and
//! accepted only when their X.25 CRC (with the message's CRC_EXTRA) checks.
//! Signed MAVLink 2 frames are parsed (signing authenticates, it does not
//! encrypt); the signature is not checked.

use crate::content::{
    AltRef, Channels, ControlContent, Position, PositionKind, PositionSource, Telemetry,
};
use crate::crc::{x25_accumulate, x25_continue};

pub const MSG_HEARTBEAT: u32 = 0;
pub const MSG_SYS_STATUS: u32 = 1;
pub const MSG_GPS_RAW_INT: u32 = 24;
pub const MSG_GLOBAL_POSITION_INT: u32 = 33;
pub const MSG_RC_CHANNELS: u32 = 65;
pub const MSG_HOME_POSITION: u32 = 242;

/// (message ID, CRC_EXTRA, full payload length).
const KNOWN: [(u32, u8, usize); 6] = [
    (MSG_HEARTBEAT, 50, 9),
    (MSG_SYS_STATUS, 124, 31),
    (MSG_GPS_RAW_INT, 24, 30),
    (MSG_GLOBAL_POSITION_INT, 104, 28),
    (MSG_RC_CHANNELS, 118, 42),
    (MSG_HOME_POSITION, 104, 52),
];

fn known(id: u32) -> Option<(u8, usize)> {
    KNOWN.iter().find(|k| k.0 == id).map(|k| (k.1, k.2))
}

/// One frame found in a buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame<'a> {
    pub sysid: u8,
    pub compid: u8,
    pub msgid: u32,
    /// Payload as sent (MAVLink 2 may truncate trailing zeros).
    pub payload: &'a [u8],
    /// Total frame length in the buffer.
    pub len: usize,
}

/// Find CRC-valid frames of the known messages in `buf`, in order. Unknown
/// message IDs cannot be CRC-checked (no CRC_EXTRA) and are skipped.
pub fn frames(buf: &[u8]) -> impl Iterator<Item = Frame<'_>> {
    let mut i = 0;
    core::iter::from_fn(move || {
        while i < buf.len() {
            if let Some(f) = frame_at(&buf[i..]) {
                i += f.len;
                return Some(f);
            }
            i += 1;
        }
        None
    })
}

fn frame_at(b: &[u8]) -> Option<Frame<'_>> {
    match *b.first()? {
        0xFD => {
            let plen = *b.get(1)? as usize;
            let incompat = *b.get(2)?;
            if incompat & !0x01 != 0 {
                return None;
            }
            let msgid = u32::from_le_bytes([*b.get(7)?, *b.get(8)?, *b.get(9)?, 0]);
            let (extra, full) = known(msgid)?;
            if plen > full {
                return None;
            }
            let crc_at = 10 + plen;
            let crc = u16::from_le_bytes([*b.get(crc_at)?, *b.get(crc_at + 1)?]);
            let c = x25_accumulate(x25_continue(0xFFFF, &b[1..crc_at]), extra);
            if c != crc {
                return None;
            }
            let sig = if incompat & 1 != 0 { 13 } else { 0 };
            Some(Frame {
                sysid: b[5],
                compid: b[6],
                msgid,
                payload: &b[10..crc_at],
                len: crc_at + 2 + sig,
            })
        }
        0xFE => {
            let plen = *b.get(1)? as usize;
            let msgid = *b.get(5)? as u32;
            let (extra, full) = known(msgid)?;
            if plen != full {
                return None;
            }
            let crc_at = 6 + plen;
            let crc = u16::from_le_bytes([*b.get(crc_at)?, *b.get(crc_at + 1)?]);
            let c = x25_accumulate(x25_continue(0xFFFF, &b[1..crc_at]), extra);
            if c != crc {
                return None;
            }
            Some(Frame {
                sysid: b[3],
                compid: b[4],
                msgid,
                payload: &b[6..crc_at],
                len: crc_at + 2,
            })
        }
        _ => None,
    }
}

/// Decode one frame's payload into content.
pub fn decode(f: &Frame<'_>) -> Option<ControlContent> {
    // MAVLink 2 truncates trailing zero bytes: zero-extend.
    let mut p = [0u8; 64];
    let n = f.payload.len().min(64);
    p[..n].copy_from_slice(&f.payload[..n]);
    let u16le = |o: usize| u16::from_le_bytes([p[o], p[o + 1]]);
    let i32le = |o: usize| i32::from_le_bytes([p[o], p[o + 1], p[o + 2], p[o + 3]]);
    let u32le = |o: usize| u32::from_le_bytes([p[o], p[o + 1], p[o + 2], p[o + 3]]);
    Some(match f.msgid {
        MSG_HEARTBEAT => ControlContent::Telemetry(Telemetry::Heartbeat {
            sysid: f.sysid,
            compid: f.compid,
            custom_mode: u32le(0),
            mav_type: p[4],
            autopilot: p[5],
            base_mode: p[6],
            system_status: p[7],
        }),
        MSG_SYS_STATUS => {
            let rem = p[30] as i8;
            let cur = u16le(16) as i16;
            ControlContent::Telemetry(Telemetry::Battery {
                voltage_mv: u16le(14) as u32,
                current_ma: (cur != -1).then_some(cur as i32 * 10),
                used_mah: None,
                remaining_pct: (rem >= 0).then_some(rem as u8),
            })
        }
        MSG_GPS_RAW_INT => ControlContent::Position(Position {
            kind: PositionKind::Aircraft,
            lat_e7: i32le(8),
            lon_e7: i32le(12),
            alt_mm: Some((i32le(16), AltRef::Msl)),
            rel_alt_mm: None,
            ground_speed_cms: (u16le(24) != u16::MAX).then(|| u16le(24)),
            heading_cdeg: (u16le(26) != u16::MAX).then(|| u16le(26)),
            satellites: (p[29] != u8::MAX).then_some(p[29]),
            fix_type: Some(p[28]),
            source: PositionSource::MavGpsRawInt,
        }),
        MSG_GLOBAL_POSITION_INT => {
            let (vx, vy) = (u16le(20) as i16 as i32, u16le(22) as i16 as i32);
            let gs = ((vx * vx + vy * vy) as u32).isqrt();
            ControlContent::Position(Position {
                kind: PositionKind::Aircraft,
                lat_e7: i32le(4),
                lon_e7: i32le(8),
                alt_mm: Some((i32le(12), AltRef::Msl)),
                rel_alt_mm: Some(i32le(16)),
                ground_speed_cms: Some(gs.min(u16::MAX as u32) as u16),
                heading_cdeg: (u16le(26) != u16::MAX).then(|| u16le(26)),
                satellites: None,
                fix_type: None,
                source: PositionSource::MavGlobalPositionInt,
            })
        }
        MSG_HOME_POSITION => ControlContent::Position(Position {
            kind: PositionKind::Home,
            lat_e7: i32le(0),
            lon_e7: i32le(4),
            alt_mm: Some((i32le(8), AltRef::Msl)),
            rel_alt_mm: None,
            ground_speed_cms: None,
            heading_cdeg: None,
            satellites: None,
            fix_type: None,
            source: PositionSource::MavHomePosition,
        }),
        MSG_RC_CHANNELS => {
            let mut c = Channels::default();
            let count = (p[40] as usize).min(18);
            for ch in 0..count.min(crate::content::MAX_CHANNELS) {
                let v = u16le(4 + ch * 2);
                if v != u16::MAX {
                    c.set(ch, v);
                }
            }
            ControlContent::Channels(c)
        }
        other => ControlContent::Telemetry(Telemetry::Other { kind: other }),
    })
}

/// Decode every known frame in `buf`, calling `out` for each.
pub fn decode_all(buf: &[u8], mut out: impl FnMut(ControlContent)) -> usize {
    let mut n = 0;
    for f in frames(buf) {
        if let Some(c) = decode(&f) {
            out(c);
            n += 1;
        }
    }
    n
}

/// Test-only encoder for a MAVLink 2 frame (no signing, no truncation).
#[cfg(test)]
pub fn encode_v2(seq: u8, sysid: u8, compid: u8, msgid: u32, payload: &[u8]) -> std::vec::Vec<u8> {
    let (extra, _) = known(msgid).expect("known msg");
    let mut f = std::vec![0xFD, payload.len() as u8, 0, 0, seq, sysid, compid];
    f.extend_from_slice(&msgid.to_le_bytes()[..3]);
    f.extend_from_slice(payload);
    let c = x25_accumulate(x25_continue(0xFFFF, &f[1..]), extra);
    f.extend_from_slice(&c.to_le_bytes());
    f
}

#[cfg(test)]
pub fn encode_v1(seq: u8, sysid: u8, compid: u8, msgid: u32, payload: &[u8]) -> std::vec::Vec<u8> {
    let (extra, _) = known(msgid).expect("known msg");
    let mut f = std::vec![0xFE, payload.len() as u8, seq, sysid, compid, msgid as u8];
    f.extend_from_slice(payload);
    let c = x25_accumulate(x25_continue(0xFFFF, &f[1..]), extra);
    f.extend_from_slice(&c.to_le_bytes());
    f
}

/// Test helper: a GLOBAL_POSITION_INT payload.
#[cfg(test)]
pub fn global_position_payload(
    lat: i32,
    lon: i32,
    alt_mm: i32,
    rel_mm: i32,
    hdg: u16,
) -> std::vec::Vec<u8> {
    let mut p = std::vec::Vec::new();
    p.extend_from_slice(&1234u32.to_le_bytes());
    p.extend_from_slice(&lat.to_le_bytes());
    p.extend_from_slice(&lon.to_le_bytes());
    p.extend_from_slice(&alt_mm.to_le_bytes());
    p.extend_from_slice(&rel_mm.to_le_bytes());
    p.extend_from_slice(&300i16.to_le_bytes());
    p.extend_from_slice(&400i16.to_le_bytes());
    p.extend_from_slice(&0i16.to_le_bytes());
    p.extend_from_slice(&hdg.to_le_bytes());
    p
}

#[cfg(test)]
pub fn heartbeat_payload() -> std::vec::Vec<u8> {
    let mut p = std::vec::Vec::new();
    p.extend_from_slice(&4u32.to_le_bytes()); // custom_mode (e.g. GUIDED)
    p.extend_from_slice(&[2, 3, 0x81, 4, 3]); // quadrotor, ArduPilot, armed, active, v3
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_global_position_round_trip() {
        let mut buf = std::vec![0x55, 0x00, 0xFD]; // junk, incl. a false start byte
        buf.extend(encode_v2(
            7,
            1,
            1,
            MSG_GLOBAL_POSITION_INT,
            &global_position_payload(399_500_000, -751_600_000, 120_000, 50_000, 18_000),
        ));
        buf.extend(encode_v1(8, 1, 1, MSG_HEARTBEAT, &heartbeat_payload()));
        let got: std::vec::Vec<_> = frames(&buf).collect();
        assert_eq!(got.len(), 2);
        let Some(ControlContent::Position(p)) = decode(&got[0]) else {
            panic!()
        };
        assert_eq!(
            (p.lat_e7, p.lon_e7, p.rel_alt_mm),
            (399_500_000, -751_600_000, Some(50_000))
        );
        assert_eq!(p.ground_speed_cms, Some(500));
        assert!(matches!(
            decode(&got[1]),
            Some(ControlContent::Telemetry(Telemetry::Heartbeat {
                mav_type: 2,
                base_mode: 0x81,
                ..
            }))
        ));
    }

    #[test]
    fn truncated_v2_payload_zero_extends() {
        // GPS_RAW_INT with trailing zeros trimmed (sats=0 fix=0 removed).
        let mut p = std::vec![0u8; 30];
        p[8..12].copy_from_slice(&100i32.to_le_bytes());
        p.truncate(12);
        let f = encode_v2(0, 1, 1, MSG_GPS_RAW_INT, &p);
        let fr = frames(&f).next().unwrap();
        let Some(ControlContent::Position(pos)) = decode(&fr) else {
            panic!()
        };
        assert_eq!(pos.lat_e7, 100);
        assert_eq!(pos.fix_type, Some(0));
        assert!(!pos.is_plausible());
    }

    #[test]
    fn corrupt_frame_ignored() {
        let mut f = encode_v2(0, 1, 1, MSG_HEARTBEAT, &heartbeat_payload());
        f[12] ^= 0x40;
        assert_eq!(frames(&f).count(), 0);
    }
}
