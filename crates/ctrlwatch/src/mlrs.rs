//! mLRS (github.com/olliw42/mLRS, GPLv3): `mLRS/Common/frame_types.h`,
//! `frames.h`, `fhss.h`, `sx-drivers/*_driver.h`, `setup.h`.
//!
//! Every mLRS frame is 91 bytes, both directions: a 2-byte frame sync word
//! (a CRC of the bind phrase), a 5-byte status, then RC data and serial
//! payload (uplink) or 82 bytes of serial payload (downlink), and an X.25
//! CRC over the first 89 bytes. LoRa modes use implicit header, CRC off,
//! normal IQ and the chip's default (private) sync word, so a LoRa frame can
//! be received and checked with nothing from the bind phrase: a 91-byte
//! frame whose CRC checks is [`Confidence::Confirmed`] (odds of a random
//! frame passing: 2^-16, plus the header checks; uplink frames also carry a
//! second CRC over the header and RC data). The FLRC and FSK modes use the
//! bind phrase for the radio's own sync word: energy only.
//!
//! Uplink frames carry RC channels and serial bytes from the ground station;
//! downlink frames carry serial bytes from the flight controller, usually
//! MAVLink. With privacy (encryption) on, uplink frames are authenticated
//! (their header CRC is not filled in) and payloads are encrypted; such
//! frames are reported [`Opaque::Encrypted`]. Downlink encryption cannot be
//! told from the outside, so a downlink payload without parseable MAVLink is
//! [`Opaque::Unparsed`]. mLRS's "mavlinkX" link mode compresses MAVLink and
//! is not decoded here; it also shows up as `Unparsed`.

use crate::content::{Channels, ControlContent, Opaque};
use crate::crc::x25;
use crate::rx::{LoRa, LoRaBw, LoRaCr, LoRaHeader, Modulation, Probe, RxConfig, RxMeta};
use crate::{Confidence, Config, Direction, LinkId, Recognised, Region, System};

pub const FRAME_LEN: usize = 91;
const HEADER_LEN: usize = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum MlrsMode {
    /// 900 MHz LoRa SF5 / BW500 / CR4/5, 31 Hz.
    L900_31,
    /// 900 MHz LoRa SF6 / BW500 / CR4/5, 19 Hz (and "19 Hz 7x" on SX127x).
    L900_19,
    /// 2.4 GHz LoRa SF5 / BW812 / CR LI 4/5, 50 Hz.
    L2g4_50,
    /// 2.4 GHz LoRa SF6, 31 Hz.
    L2g4_31,
    /// 2.4 GHz LoRa SF7, 19 Hz.
    L2g4_19,
    /// 2.4 GHz FLRC, 111 Hz: sync word from the bind phrase, energy only.
    Flrc111,
    /// 900 MHz FSK, 50 Hz: sync word from the bind phrase, energy only.
    Fsk50,
}

impl MlrsMode {
    pub const LORA: [MlrsMode; 5] = [
        MlrsMode::L900_31,
        MlrsMode::L900_19,
        MlrsMode::L2g4_50,
        MlrsMode::L2g4_31,
        MlrsMode::L2g4_19,
    ];

    pub const fn sub_ghz(self) -> bool {
        matches!(
            self,
            MlrsMode::L900_31 | MlrsMode::L900_19 | MlrsMode::Fsk50
        )
    }

    /// Frame interval, µs.
    pub const fn interval_us(self) -> u32 {
        match self {
            MlrsMode::L900_31 | MlrsMode::L2g4_31 => 32_000,
            MlrsMode::L900_19 | MlrsMode::L2g4_19 => 53_000,
            MlrsMode::L2g4_50 | MlrsMode::Fsk50 => 20_000,
            MlrsMode::Flrc111 => 9_000,
        }
    }

    pub const fn lora(self) -> Option<(u8, LoRaBw, LoRaCr)> {
        match self {
            MlrsMode::L900_31 => Some((5, LoRaBw::Khz500, LoRaCr::Cr4_5)),
            MlrsMode::L900_19 => Some((6, LoRaBw::Khz500, LoRaCr::Cr4_5)),
            MlrsMode::L2g4_50 => Some((5, LoRaBw::Khz812, LoRaCr::Li4_5)),
            MlrsMode::L2g4_31 => Some((6, LoRaBw::Khz812, LoRaCr::Li4_5)),
            MlrsMode::L2g4_19 => Some((7, LoRaBw::Khz812, LoRaCr::Li4_5)),
            _ => None,
        }
    }
}

/// mLRS channel list for a region: (first Hz, step Hz, count). The 433
/// list is irregular (433.36, 433.92, 433.48 MHz) and handled separately.
pub const fn channels(region: Region) -> Option<(u32, u32, u8)> {
    match region {
        Region::Fcc915 => Some((902_400_000, 600_000, 43)),
        Region::Eu868 => Some((863_275_000, 525_000, 10)),
        Region::In866 => Some((865_375_000, 525_000, 4)),
        Region::Ism2g4 => Some((2_401_000_000, 1_000_000, 80)),
        _ => None,
    }
}

pub const FREQS_433: [u32; 3] = [433_360_000, 433_920_000, 433_480_000];

pub fn lora_rx(mode: MlrsMode, freq_hz: u32, region: Region) -> Option<RxConfig> {
    let (sf, bw, cr) = mode.lora()?;
    Some(RxConfig {
        freq_hz,
        modulation: Modulation::LoRa(LoRa {
            sf,
            bw,
            cr,
            preamble_symbols: 12,
            sync_word: 0x12,
            header: LoRaHeader::Implicit {
                len: FRAME_LEN as u8,
            },
            crc_on: false,
            invert_iq: false,
            ldro: false,
        }),
        probe: Probe::Mlrs(mode),
        region,
    })
}

/// The 40-bit status, LSB-first bitfields (GCC packed layout).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    pub seq_no: u8,
    pub frame_type: u8,
    pub is_32channels: bool,
    pub payload_len: u8,
}

pub fn status(b: &[u8]) -> Status {
    let v = u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], 0, 0, 0]);
    Status {
        seq_no: (v & 7) as u8,
        frame_type: ((v >> 4) & 7) as u8,
        is_32channels: (v >> 7) & 1 == 1,
        payload_len: ((v >> 33) & 0x7F) as u8,
    }
}

fn bits(b: &[u8], start: usize, n: usize) -> u32 {
    let mut v = 0u32;
    for i in 0..n {
        let bit = start + i;
        v |= (((b[bit / 8] >> (bit % 8)) & 1) as u32) << i;
    }
    v
}

/// mLRS 11-bit RC value (1..2047, 1024 = centre) to µs:
/// `pwm = (x - 1024) * 500 / 852 + 1500`.
fn rc_to_us(x: u32) -> u16 {
    ((x as i32 - 1024) * 500 / 852 + 1500) as u16
}

fn decode_rc(f: &[u8], st: &Status) -> Channels {
    let rc = &f[HEADER_LEN..HEADER_LEN + 18];
    let mut c = Channels::default();
    let three = |v: u32| match v {
        0 => rc_to_us(0),
        1 => 1500,
        _ => rc_to_us(2047),
    };
    if !st.is_32channels {
        for ch in 0..4 {
            c.set(ch, rc_to_us(bits(rc, ch * 11, 11)));
        }
        c.set(12, three(bits(rc, 44, 2)));
        c.set(13, three(bits(rc, 46, 2)));
        let rc2 = &rc[8..];
        for ch in 0..4 {
            c.set(4 + ch, rc_to_us(bits(rc2, ch * 11, 11)));
        }
        c.set(14, three(bits(rc2, 44, 2)));
        c.set(15, three(bits(rc2, 46, 2)));
        for ch in 0..4 {
            c.set(8 + ch, rc_to_us(rc2[6 + ch] as u32 * 8));
        }
    } else {
        for ch in 0..8 {
            c.set(ch, rc_to_us(bits(rc, ch * 11, 11)));
        }
        let ofs = (st.seq_no as usize & 1) * 4;
        for ch in 0..4 {
            c.set(8 + ofs + ch, rc_to_us(rc[13 + ch] as u32 * 8));
        }
    }
    c
}

pub fn recognise(f: &[u8], rx: &RxConfig, meta: RxMeta, cfg: &Config) -> Option<Recognised> {
    if f.len() != FRAME_LEN {
        return None;
    }
    let crc = u16::from_le_bytes([f[89], f[90]]);
    if x25(&f[..89]) != crc {
        return None;
    }
    let st = status(&f[2..7]);
    // Uplink (Tx) frame types 0, 2, 3 (payload <= 64); downlink (Rx) 1 or
    // a CMD (2) with payload <= 82.
    let uplink = match st.frame_type {
        0 | 3 => true,
        1 => false,
        // CMD frames go both ways; an uplink one has a valid crc1 or is
        // authenticated. Treat as unknown direction.
        2 => st.payload_len <= 64,
        _ => return None,
    };
    if st.payload_len > if uplink { 64 } else { 82 } {
        return None;
    }
    let rc1_len = if st.is_32channels { 11 } else { 6 };
    let crc1_at = HEADER_LEN + rc1_len;
    let crc1 = u16::from_le_bytes([f[crc1_at], f[crc1_at + 1]]);
    let plain_uplink = uplink && x25(&f[..crc1_at]) == crc1;
    let direction = match st.frame_type {
        0 | 3 => Direction::Uplink,
        1 => Direction::Downlink,
        _ => Direction::Unknown,
    };
    let mut s = crate::sighting(System::Mlrs, rx, meta, Confidence::Confirmed, direction);
    if cfg.read_ids {
        s.id = Some(LinkId::MlrsSyncWord(u16::from_le_bytes([f[0], f[1]])));
    }
    let mut r = Recognised::new(s);
    if !cfg.decode_content {
        return Some(r);
    }
    r.keep_raw(cfg, f);
    match direction {
        Direction::Uplink if !plain_uplink => r.push(ControlContent::Opaque(Opaque::Encrypted)),
        Direction::Uplink => {
            if st.frame_type == 0 {
                r.push(ControlContent::Channels(decode_rc(f, &st)));
            }
            let payload = &f[HEADER_LEN + 18..HEADER_LEN + 18 + st.payload_len as usize];
            push_mavlink(&mut r, payload);
        }
        Direction::Downlink => {
            let payload = &f[HEADER_LEN..HEADER_LEN + st.payload_len as usize];
            push_mavlink(&mut r, payload);
        }
        Direction::Unknown => {}
    }
    Some(r)
}

/// The MAVLink-carrying payload of one mLRS frame and its direction
/// (unencrypted frames only; `None` for a frame that does not check, an
/// encrypted uplink, or no payload). For `mavwatch`; only called with
/// content decoding on.
pub fn serial_bytes(f: &[u8]) -> Option<(&[u8], Direction)> {
    if f.len() != FRAME_LEN || x25(&f[..89]) != u16::from_le_bytes([f[89], f[90]]) {
        return None;
    }
    let st = status(&f[2..7]);
    let rc1_len = if st.is_32channels { 11 } else { 6 };
    let crc1_at = HEADER_LEN + rc1_len;
    let p = match st.frame_type {
        0 | 3 if st.payload_len <= 64 => {
            let crc1 = u16::from_le_bytes([f[crc1_at], f[crc1_at + 1]]);
            if x25(&f[..crc1_at]) != crc1 {
                return None;
            }
            (&f[HEADER_LEN + 18..HEADER_LEN + 18 + st.payload_len as usize], Direction::Uplink)
        }
        1 if st.payload_len <= 82 => (&f[HEADER_LEN..HEADER_LEN + st.payload_len as usize], Direction::Downlink),
        _ => return None,
    };
    (!p.0.is_empty()).then_some(p)
}

fn push_mavlink(r: &mut Recognised, payload: &[u8]) {
    if payload.is_empty() {
        return;
    }
    let n = crate::mavlink::decode_all(payload, |c| r.push(c));
    if n == 0 {
        r.push(ControlContent::Opaque(Opaque::Unparsed));
    }
}

/// Test-only frame builder (mirrors `_pack_txframe_w_type` /
/// `_pack_rxframe_w_type` without encryption).
#[cfg(test)]
pub mod testenc {
    use super::*;

    fn put_bits(b: &mut [u8], start: usize, n: usize, v: u32) {
        for i in 0..n {
            let bit = start + i;
            if (v >> i) & 1 == 1 {
                b[bit / 8] |= 1 << (bit % 8);
            }
        }
    }

    fn put_status(f: &mut [u8], seq: u8, ft: u8, is32: bool, plen: u8) {
        let v: u64 = (seq as u64 & 7)
            | ((ft as u64 & 7) << 4)
            | ((is32 as u64) << 7)
            | ((plen as u64 & 0x7F) << 33);
        f[2..7].copy_from_slice(&v.to_le_bytes()[..5]);
    }

    /// Uplink frame, V1 RC layout, `ch` 11-bit values for ch0-7.
    pub fn tx_frame(sync: u16, seq: u8, ch: [u16; 8], payload: &[u8]) -> std::vec::Vec<u8> {
        let mut f = std::vec![0u8; FRAME_LEN];
        f[..2].copy_from_slice(&sync.to_le_bytes());
        put_status(&mut f, seq, 0, false, payload.len() as u8);
        {
            let rc = &mut f[HEADER_LEN..HEADER_LEN + 18];
            for (i, v) in ch[..4].iter().enumerate() {
                put_bits(rc, i * 11, 11, *v as u32);
            }
            for (i, v) in ch[4..].iter().enumerate() {
                put_bits(&mut rc[8..], i * 11, 11, *v as u32);
            }
        }
        f[25..25 + payload.len()].copy_from_slice(payload);
        let c1 = x25(&f[..13]);
        f[13..15].copy_from_slice(&c1.to_le_bytes());
        let c = x25(&f[..89]);
        f[89..91].copy_from_slice(&c.to_le_bytes());
        f
    }

    pub fn rx_frame(sync: u16, seq: u8, payload: &[u8]) -> std::vec::Vec<u8> {
        let mut f = std::vec![0u8; FRAME_LEN];
        f[..2].copy_from_slice(&sync.to_le_bytes());
        put_status(&mut f, seq, 1, false, payload.len() as u8);
        f[7..7 + payload.len()].copy_from_slice(payload);
        let c = x25(&f[..89]);
        f[89..91].copy_from_slice(&c.to_le_bytes());
        f
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::{PositionKind, Telemetry};

    fn rx() -> RxConfig {
        lora_rx(MlrsMode::L900_31, 915_000_000, Region::Fcc915).unwrap()
    }
    fn meta() -> RxMeta {
        RxMeta {
            rssi_dbm: -80,
            snr_db: 5,
            t_us: 7,
        }
    }

    #[test]
    fn uplink_frame_confirmed_and_channels_decoded() {
        let f = testenc::tx_frame(0xBEEF, 3, [1024, 172, 1876, 1024, 1, 2047, 1024, 1024], &[]);
        let r = crate::recognise(&f, &rx(), meta(), &Config::default()).unwrap();
        assert_eq!(r.sighting.confidence, Confidence::Confirmed);
        assert_eq!(r.sighting.direction, Direction::Uplink);
        assert_eq!(r.sighting.id, None);
        assert!(r.content.is_empty());
        let r = crate::recognise(
            &f,
            &rx(),
            meta(),
            &Config {
                read_ids: true,
                decode_content: true,
            },
        )
        .unwrap();
        assert_eq!(r.sighting.id, Some(LinkId::MlrsSyncWord(0xBEEF)));
        let ControlContent::Channels(c) = &r.content[0] else {
            panic!()
        };
        assert_eq!(c.get(0), Some(1500));
        assert_eq!(c.get(1), Some(1000));
        assert_eq!(c.get(2), Some(2000));
        assert_eq!(c.get(5), Some(2100));
    }

    #[test]
    fn serial_bytes_both_directions() {
        let hb = crate::mavlink::encode_v2(4, 1, 1, crate::mavlink::MSG_HEARTBEAT, &crate::mavlink::heartbeat_payload());
        let f = testenc::rx_frame(0xBEEF, 2, &hb);
        let mut out = [0u8; 258];
        let (b, d) = crate::serial_bytes(&f, &rx(), &mut out).unwrap();
        assert_eq!((b, d), (&hb[..], Direction::Downlink));
        let f = testenc::tx_frame(0xBEEF, 3, [1024; 8], &hb);
        let (b, d) = crate::serial_bytes(&f, &rx(), &mut out).unwrap();
        assert_eq!((b, d), (&hb[..], Direction::Uplink));
        let mut bad = f.clone();
        bad[40] ^= 1;
        assert!(crate::serial_bytes(&bad, &rx(), &mut out).is_none());
        // No payload: nothing to scan.
        let f = testenc::tx_frame(0xBEEF, 3, [1024; 8], &[]);
        assert!(crate::serial_bytes(&f, &rx(), &mut out).is_none());
    }

    #[test]
    fn downlink_mavlink_position_decoded() {
        let mut pl = crate::mavlink::encode_v2(
            1,
            1,
            1,
            crate::mavlink::MSG_GLOBAL_POSITION_INT,
            &crate::mavlink::global_position_payload(
                399_000_000,
                -751_000_000,
                90_000,
                40_000,
                100,
            ),
        );
        pl.extend(crate::mavlink::encode_v2(
            2,
            1,
            1,
            crate::mavlink::MSG_HEARTBEAT,
            &crate::mavlink::heartbeat_payload(),
        ));
        let f = testenc::rx_frame(0x1234, 0, &pl);
        let r = crate::recognise(
            &f,
            &rx(),
            meta(),
            &Config {
                read_ids: false,
                decode_content: true,
            },
        )
        .unwrap();
        assert_eq!(r.sighting.direction, Direction::Downlink);
        assert!(
            matches!(r.content[0], ControlContent::Position(p) if p.kind == PositionKind::Aircraft && p.lat_e7 == 399_000_000)
        );
        assert!(matches!(
            r.content[1],
            ControlContent::Telemetry(Telemetry::Heartbeat { .. })
        ));
        assert!(r.raw.as_ref().is_some_and(|raw| raw.len() == FRAME_LEN));
    }

    #[test]
    fn authenticated_uplink_reported_encrypted() {
        let mut f = testenc::tx_frame(0xBEEF, 3, [1024; 8], &[1, 2, 3]);
        // Privacy >= 2: crc1 not filled; whole-frame CRC still valid.
        f[13] ^= 0xFF;
        let c = x25(&f[..89]);
        f[89..91].copy_from_slice(&c.to_le_bytes());
        let r = crate::recognise(
            &f,
            &rx(),
            meta(),
            &Config {
                read_ids: false,
                decode_content: true,
            },
        )
        .unwrap();
        assert_eq!(r.content[0], ControlContent::Opaque(Opaque::Encrypted));
    }

    #[test]
    fn random_and_elrs_sized_frames_rejected() {
        let mut x = 99u64;
        for _ in 0..20_000 {
            let mut f = [0u8; FRAME_LEN];
            for b in f.iter_mut() {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                *b = x as u8;
            }
            assert!(crate::recognise(&f, &rx(), meta(), &Config::default()).is_none());
        }
        assert!(crate::recognise(&[0u8; 8], &rx(), meta(), &Config::default()).is_none());
    }
}
