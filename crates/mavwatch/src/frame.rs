//! MAVLink v1/v2 frame scanner, shared with `ctrlwatch` (which decodes
//! positions and telemetry from the same frames).
//!
//! A frame is accepted only when its X.25 checksum, seeded with the
//! message's CRC_EXTRA, checks. CRC_EXTRA comes from the message definition,
//! so only the messages in [`MESSAGES`] can be checked; any other message ID
//! is skipped (never guessed at).
//!
//! **Any payload length is accepted** (0 to 255), MAVLink 1 or 2, as the
//! common parsers do: the MAVLink C library built without
//! `MAVLINK_CHECK_MESSAGE_LENGTH` (its default) and pymavlink check the
//! checksum, then decode by copying at most the message's length and
//! zero-filling the rest. So a hostile sender can deliver a payload in a
//! frame that is longer or shorter than the message, and the ground station
//! still acts on it. [`Frame::payload`] is what those parsers see (cut at
//! the message's full length; the accessors zero-extend), and
//! [`Frame::odd_length`] says when the length is one no conforming sender
//! uses (until WP-11a such frames were rejected as `Bad`, which hid them).
//! Not every ground station has been checked: QGroundControl and Mission
//! Planner may be stricter or not; this follows the two parsers above. MAVLink 2 signed frames are parsed and
//! their signature block (link ID, timestamp, signature) is returned; the
//! signature itself cannot be checked without the link's secret key, and is
//! not.
//!
//! CRC_EXTRA values and payload lengths were computed from
//! mavlink/mavlink `message_definitions/v1.0/{minimal,standard,common}.xml`
//! (commit 2a8f8cf, 2026-11) with the generator's own rule (fields sorted
//! by type size, extensions excluded from the seed), and agree with the
//! values pymavlink publishes for the messages ctrlwatch has always used.

use crate::crc::{x25_accumulate, x25_continue};

/// MAVLink 2 start byte.
pub const STX_V2: u8 = 0xFD;
/// MAVLink 1 start byte.
pub const STX_V1: u8 = 0xFE;
/// Longest MAVLink 2 frame: 10 header + 255 payload + 2 CRC + 13 signature.
pub const MAX_FRAME_LEN: usize = 280;
/// Signature block length (link ID 1, timestamp 6, signature 6).
pub const SIGNATURE_LEN: usize = 13;
/// `incompat_flags` bit: the frame is signed.
pub const INCOMPAT_SIGNED: u8 = 0x01;

/// A message this scanner can check: ID, CRC_EXTRA, base payload length
/// (MAVLink 1, and without extension fields), full length (with extensions).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MsgInfo {
    pub id: u32,
    pub name: &'static str,
    pub crc_extra: u8,
    pub base_len: u8,
    pub max_len: u8,
}

const fn m(id: u32, name: &'static str, crc_extra: u8, base_len: u8, max_len: u8) -> MsgInfo {
    MsgInfo {
        id,
        name,
        crc_extra,
        base_len,
        max_len,
    }
}

/// Every message the scanner knows, in ID order.
pub const MESSAGES: [MsgInfo; 27] = [
    m(0, "HEARTBEAT", 50, 9, 9),
    m(1, "SYS_STATUS", 124, 31, 43),
    m(11, "SET_MODE", 89, 6, 6),
    m(20, "PARAM_REQUEST_READ", 214, 20, 20),
    m(21, "PARAM_REQUEST_LIST", 159, 2, 2),
    m(22, "PARAM_VALUE", 220, 25, 25),
    m(23, "PARAM_SET", 168, 23, 23),
    m(24, "GPS_RAW_INT", 24, 30, 52),
    m(33, "GLOBAL_POSITION_INT", 104, 28, 28),
    m(44, "MISSION_COUNT", 221, 4, 9),
    m(65, "RC_CHANNELS", 118, 42, 42),
    m(66, "REQUEST_DATA_STREAM", 148, 6, 6),
    m(69, "MANUAL_CONTROL", 243, 11, 30),
    m(70, "RC_CHANNELS_OVERRIDE", 124, 18, 38),
    m(73, "MISSION_ITEM_INT", 38, 37, 38),
    m(75, "COMMAND_INT", 158, 35, 35),
    m(76, "COMMAND_LONG", 152, 33, 33),
    m(109, "RADIO_STATUS", 185, 9, 9),
    m(110, "FILE_TRANSFER_PROTOCOL", 84, 254, 254),
    m(126, "SERIAL_CONTROL", 220, 79, 81),
    m(242, "HOME_POSITION", 104, 52, 60),
    m(253, "STATUSTEXT", 83, 51, 54),
    m(256, "SETUP_SIGNING", 71, 42, 42),
    m(259, "CAMERA_INFORMATION", 92, 235, 237),
    m(269, "VIDEO_STREAM_INFORMATION", 109, 213, 217),
    m(395, "COMPONENT_INFORMATION", 0, 212, 212),
    m(397, "COMPONENT_METADATA", 182, 108, 108),
];

/// The scanner's entry for `id`, if it has one.
pub fn info(id: u32) -> Option<&'static MsgInfo> {
    MESSAGES.iter().find(|m| m.id == id)
}

/// A MAVLink 2 signature block, as sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Signature {
    pub link_id: u8,
    /// 10 µs units since 2015-01-01 (48 bits).
    pub timestamp: u64,
    pub sig: [u8; 6],
}

/// One CRC-valid frame found in a buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame<'a> {
    /// 1 or 2.
    pub version: u8,
    pub seq: u8,
    pub sysid: u8,
    pub compid: u8,
    pub msgid: u32,
    /// Payload as a lenient parser decodes it: the bytes sent, cut at the
    /// message's full length (MAVLink 2 trims trailing zero bytes, and a
    /// short frame is zero-filled: read fields with the zero-extending
    /// accessors below).
    pub payload: &'a [u8],
    /// Payload length as sent in the header.
    pub sent_len: u8,
    /// MAVLink 2 signed frames only.
    pub signature: Option<Signature>,
    /// The whole frame, header to signature, as it was in the buffer.
    pub bytes: &'a [u8],
}

impl Frame<'_> {
    /// Total frame length in the buffer.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn signed(&self) -> bool {
        self.signature.is_some()
    }

    /// A payload length no conforming sender uses for this message: longer
    /// than the message (either version), a MAVLink 1 frame not exactly the
    /// base length, or an empty MAVLink 2 payload (trimming keeps one
    /// byte). `(sent, expected)`, `expected` being the full length (MAVLink
    /// 2) or the base length (MAVLink 1).
    pub fn odd_length(&self) -> Option<(u8, u8)> {
        let mi = info(self.msgid)?;
        let n = self.sent_len;
        let odd = match self.version {
            1 => n != mi.base_len,
            _ => n > mi.max_len || n == 0,
        };
        odd.then_some((n, if self.version == 1 { mi.base_len } else { mi.max_len }))
    }

    /// Payload byte `i`, zero past the end (MAVLink 2 truncation).
    pub fn u8(&self, i: usize) -> u8 {
        self.payload.get(i).copied().unwrap_or(0)
    }

    pub fn u16(&self, i: usize) -> u16 {
        u16::from_le_bytes([self.u8(i), self.u8(i + 1)])
    }

    pub fn u32(&self, i: usize) -> u32 {
        u32::from_le_bytes([self.u8(i), self.u8(i + 1), self.u8(i + 2), self.u8(i + 3)])
    }

    pub fn i32(&self, i: usize) -> i32 {
        self.u32(i) as i32
    }

    pub fn f32(&self, i: usize) -> f32 {
        f32::from_bits(self.u32(i))
    }

    /// A `char[n]` / `uint8_t[n]` field at `off`: the bytes actually sent,
    /// up to the first NUL (a full field has none). Never longer than `n`.
    pub fn text(&self, off: usize, n: usize) -> &[u8] {
        let end = (off + n).min(self.payload.len());
        let f = self.payload.get(off..end).unwrap_or(&[]);
        let k = f.iter().position(|&b| b == 0).unwrap_or(f.len());
        &f[..k]
    }

    /// A `uint8_t[n]` field at `off`, the bytes actually sent (NULs kept).
    pub fn raw(&self, off: usize, n: usize) -> &[u8] {
        let end = (off + n).min(self.payload.len());
        self.payload.get(off..end).unwrap_or(&[])
    }
}

/// What [`parse_at`] made of the bytes at the start of a buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parse<'a> {
    Frame(Frame<'a>),
    /// A plausible header for a known message, but the frame runs past the
    /// end of the buffer: wait for more bytes.
    Incomplete,
    /// Not a frame starting here (wrong start byte, unknown message, unknown
    /// incompatibility flags, CRC mismatch).
    Bad,
}

/// Try to read one frame at the start of `b`.
pub fn parse_at(b: &[u8]) -> Parse<'_> {
    let Some(&stx) = b.first() else {
        return Parse::Incomplete;
    };
    match stx {
        STX_V2 => {
            if b.len() < 10 {
                return Parse::Incomplete;
            }
            let plen = b[1] as usize;
            let incompat = b[2];
            if incompat & !INCOMPAT_SIGNED != 0 {
                return Parse::Bad;
            }
            let msgid = u32::from_le_bytes([b[7], b[8], b[9], 0]);
            let Some(mi) = info(msgid) else {
                return Parse::Bad;
            };
            let crc_at = 10 + plen;
            let sig = if incompat & INCOMPAT_SIGNED != 0 {
                SIGNATURE_LEN
            } else {
                0
            };
            let total = crc_at + 2 + sig;
            if b.len() < crc_at + 2 {
                return Parse::Incomplete;
            }
            let crc = u16::from_le_bytes([b[crc_at], b[crc_at + 1]]);
            if x25_accumulate(x25_continue(0xFFFF, &b[1..crc_at]), mi.crc_extra) != crc {
                return Parse::Bad;
            }
            if b.len() < total {
                return Parse::Incomplete;
            }
            let signature = (sig != 0).then(|| {
                let s = &b[crc_at + 2..total];
                let mut ts = [0u8; 8];
                ts[..6].copy_from_slice(&s[1..7]);
                let mut sg = [0u8; 6];
                sg.copy_from_slice(&s[7..13]);
                Signature {
                    link_id: s[0],
                    timestamp: u64::from_le_bytes(ts),
                    sig: sg,
                }
            });
            Parse::Frame(Frame {
                version: 2,
                seq: b[4],
                sysid: b[5],
                compid: b[6],
                msgid,
                payload: &b[10..10 + plen.min(mi.max_len as usize)],
                sent_len: plen as u8,
                signature,
                bytes: &b[..total],
            })
        }
        STX_V1 => {
            if b.len() < 6 {
                return Parse::Incomplete;
            }
            let plen = b[1] as usize;
            let msgid = b[5] as u32;
            let Some(mi) = info(msgid) else {
                return Parse::Bad;
            };
            // MAVLink 1 has neither extensions nor truncation, but lenient
            // parsers take any length (see the module docs).
            let crc_at = 6 + plen;
            if b.len() < crc_at + 2 {
                return Parse::Incomplete;
            }
            let crc = u16::from_le_bytes([b[crc_at], b[crc_at + 1]]);
            if x25_accumulate(x25_continue(0xFFFF, &b[1..crc_at]), mi.crc_extra) != crc {
                return Parse::Bad;
            }
            Parse::Frame(Frame {
                version: 1,
                seq: b[2],
                sysid: b[3],
                compid: b[4],
                msgid,
                payload: &b[6..6 + plen.min(mi.max_len as usize)],
                sent_len: plen as u8,
                signature: None,
                bytes: &b[..crc_at + 2],
            })
        }
        _ => Parse::Bad,
    }
}

/// Find complete CRC-valid frames of the known messages in `buf`, in order.
/// Bytes between frames, and frames cut off at either end, are skipped.
pub fn frames(buf: &[u8]) -> impl Iterator<Item = Frame<'_>> {
    let mut i = 0;
    core::iter::from_fn(move || {
        while i < buf.len() {
            if let Parse::Frame(f) = parse_at(&buf[i..]) {
                i += f.len();
                return Some(f);
            }
            i += 1;
        }
        None
    })
}

/// Frame builders for tests (feature `encode`). They build bytes only;
/// nothing in this crate sends anything.
#[cfg(feature = "encode")]
pub mod encode {
    use super::*;

    /// Optional MAVLink 2 signature block to append.
    #[derive(Debug, Clone, Copy)]
    pub struct Sign {
        pub link_id: u8,
        pub timestamp: u64,
        pub sig: [u8; 6],
    }

    /// A MAVLink 2 frame into `out`; returns its length. Trailing zero bytes
    /// of the payload are trimmed, as MAVLink 2 senders do (at least one
    /// byte is kept). Panics if `msgid` is unknown or `out` is too small.
    pub fn v2(
        out: &mut [u8],
        seq: u8,
        sysid: u8,
        compid: u8,
        msgid: u32,
        payload: &[u8],
        sign: Option<Sign>,
    ) -> usize {
        v2_raw(out, seq, sysid, compid, msgid, payload, sign, true)
    }

    /// As [`v2`], choosing whether to trim trailing zeros.
    #[allow(clippy::too_many_arguments)]
    pub fn v2_raw(
        out: &mut [u8],
        seq: u8,
        sysid: u8,
        compid: u8,
        msgid: u32,
        payload: &[u8],
        sign: Option<Sign>,
        trim: bool,
    ) -> usize {
        let mi = info(msgid).expect("known message");
        let mut n = payload.len();
        if trim {
            while n > 1 && payload[n - 1] == 0 {
                n -= 1;
            }
        }
        let flags = if sign.is_some() { INCOMPAT_SIGNED } else { 0 };
        let id = msgid.to_le_bytes();
        out[..10].copy_from_slice(&[
            STX_V2, n as u8, flags, 0, seq, sysid, compid, id[0], id[1], id[2],
        ]);
        out[10..10 + n].copy_from_slice(&payload[..n]);
        let c = x25_accumulate(x25_continue(0xFFFF, &out[1..10 + n]), mi.crc_extra);
        out[10 + n..12 + n].copy_from_slice(&c.to_le_bytes());
        let mut len = 12 + n;
        if let Some(s) = sign {
            out[len] = s.link_id;
            out[len + 1..len + 7].copy_from_slice(&s.timestamp.to_le_bytes()[..6]);
            out[len + 7..len + 13].copy_from_slice(&s.sig);
            len += SIGNATURE_LEN;
        }
        len
    }

    /// A MAVLink 1 frame (`payload` must be the base length).
    pub fn v1(out: &mut [u8], seq: u8, sysid: u8, compid: u8, msgid: u32, payload: &[u8]) -> usize {
        let mi = info(msgid).expect("known message");
        let n = payload.len();
        out[..6].copy_from_slice(&[STX_V1, n as u8, seq, sysid, compid, msgid as u8]);
        out[6..6 + n].copy_from_slice(payload);
        let c = x25_accumulate(x25_continue(0xFFFF, &out[1..6 + n]), mi.crc_extra);
        out[6 + n..8 + n].copy_from_slice(&c.to_le_bytes());
        8 + n
    }
}

#[cfg(test)]
mod tests {
    use super::encode::*;
    use super::*;

    #[test]
    fn table_is_sorted_and_lengths_sane() {
        for w in MESSAGES.windows(2) {
            assert!(w[0].id < w[1].id);
        }
        for m in MESSAGES {
            assert!(m.base_len <= m.max_len, "{}", m.name);
        }
    }

    /// Spot values that are widely published (pymavlink's tables): a
    /// regression guard for the table.
    #[test]
    fn well_known_crc_extras() {
        let want = [
            (0, 50),
            (22, 220),
            (23, 168),
            (33, 104),
            (76, 152),
            (110, 84),
            (126, 220),
            (253, 83),
        ];
        for (id, x) in want {
            assert_eq!(info(id).unwrap().crc_extra, x, "msg {id}");
        }
    }

    #[test]
    fn signed_v2_round_trip_and_partial() {
        let mut b = [0u8; 300];
        let s = Sign {
            link_id: 3,
            timestamp: 0x0102_0304_0506,
            sig: [9; 6],
        };
        let n = v2(&mut b, 5, 1, 1, 0, &[1, 0, 0, 0, 2, 3, 4, 5, 3], Some(s));
        let Parse::Frame(f) = parse_at(&b[..n]) else {
            panic!()
        };
        assert_eq!((f.seq, f.sysid, f.msgid, f.len()), (5, 1, 0, n));
        assert_eq!(
            f.signature,
            Some(Signature {
                link_id: 3,
                timestamp: 0x0102_0304_0506,
                sig: [9; 6]
            })
        );
        // Cut anywhere: incomplete, never a frame.
        for k in 0..n {
            assert_eq!(parse_at(&b[..k]), Parse::Incomplete, "{k}");
        }
        b[12] ^= 1;
        assert_eq!(parse_at(&b[..n]), Parse::Bad);
    }

    #[test]
    fn v1_and_truncated_v2() {
        let mut b = [0u8; 64];
        let n = v1(&mut b, 1, 2, 3, 0, &[0, 0, 0, 0, 2, 3, 0x81, 4, 3]);
        assert!(matches!(
            parse_at(&b[..n]),
            Parse::Frame(Frame {
                version: 1,
                sysid: 2,
                ..
            })
        ));
        // PARAM_SET with trailing zeros trimmed reads back zero-extended.
        let mut p = [0u8; 23];
        p[0..4].copy_from_slice(&1.5f32.to_le_bytes());
        p[6] = b'A';
        let n = v2(&mut b, 0, 255, 190, 23, &p, None);
        let Parse::Frame(f) = parse_at(&b[..n]) else {
            panic!()
        };
        assert!(f.payload.len() < 23);
        assert_eq!((f.f32(0), f.u8(22), f.text(6, 16)), (1.5, 0, &b"A"[..]));
    }

    #[test]
    fn any_length_parses_as_lenient_parsers_do() {
        let mut b = [0u8; 300];
        // HEARTBEAT (9 bytes) sent with 40 in MAVLink 2: cut at 9.
        let n = v2_raw(&mut b, 0, 1, 1, 0, &[7; 40], None, false);
        let Parse::Frame(f) = parse_at(&b[..n]) else { panic!() };
        assert_eq!((f.payload.len(), f.sent_len, f.len(), f.odd_length()), (9, 40, n, Some((40, 9))));
        // ...and 3 bytes in MAVLink 1: zero-extended.
        let n = v1(&mut b, 0, 1, 1, 0, &[7; 3]);
        let Parse::Frame(f) = parse_at(&b[..n]) else { panic!() };
        assert_eq!((f.payload.len(), f.u8(2), f.u8(8), f.odd_length()), (3, 7, 0, Some((3, 9))));
        // Conforming lengths are not odd.
        let n = v1(&mut b, 0, 1, 1, 0, &[7; 9]);
        let Parse::Frame(f) = parse_at(&b[..n]) else { panic!() };
        assert_eq!(f.odd_length(), None);
        let n = v2(&mut b, 0, 1, 1, 0, &[7, 0, 0, 0, 0, 0, 0, 0, 0], None);
        let Parse::Frame(f) = parse_at(&b[..n]) else { panic!() };
        assert_eq!((f.sent_len, f.odd_length()), (1, None));
        // Every cut of a long frame is incomplete, never a frame.
        let n = v2_raw(&mut b, 0, 1, 1, 0, &[7; 200], None, false);
        for k in 0..n {
            assert_eq!(parse_at(&b[..k]), Parse::Incomplete, "{k}");
        }
    }

    #[test]
    fn unknown_message_skipped() {
        let mut b = [0u8; 64];
        let n = v2(&mut b, 0, 1, 1, 0, &[1, 2, 3, 4, 5, 6, 7, 8, 9], None);
        b[7] = 30; // ATTITUDE: not in the table, cannot be checked
        assert_eq!(parse_at(&b[..n]), Parse::Bad);
        assert_eq!(frames(&b[..n]).count(), 0);
    }
}
