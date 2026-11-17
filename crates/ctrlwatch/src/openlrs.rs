//! OpenLRSng (github.com/openLRSng/openLRSng, GPLv3): `openLRSng/
//! {binding.h, common.h, RFM.h, TX.h}`.
//!
//! RFM22B (Si443x) GFSK, no whitening, sync word 0x2D 0xD4, a 4-byte header
//! = `rf_magic` (most significant byte first) with the last byte XORed with
//! the hop index (0-23), a length byte, the packet, and the Si443x CCITT
//! CRC-16 (`DACTL = 0x8C`). Factory defaults: 435 MHz base (433 build),
//! 50 kHz channel spacing, hop list 22,10,19,34,49,41, 19200 bit/s, magic
//! 0xDEADFEED; but the TX randomises the magic (and usually the hop list)
//! when it is bound, so in the field only the structure is known.
//!
//! An uplink packet is `[seq|type][channels...]` with a size that names the
//! channel layout (7, 11, 12, 16, 17 or 21 bytes); telemetry is 9 bytes. A
//! packet with a valid CRC, one of those sizes and a valid type byte is
//! [`Confidence::Confirmed`]. Channel decoding needs nothing from the bind
//! data: the size gives the layout.
//!
//! Binding packets (`'b'` + the whole bind data, at the binding frequency
//! with a fixed magic) carry the hop list and magic in the clear; they are
//! recognised, and their contents are only exposed as an identifier.

use crate::content::{Channels, ControlContent, Opaque};
use crate::rx::{Gfsk, Modulation, Probe, RxConfig, RxMeta};
use crate::sik::si443x_parse;
use crate::{Confidence, Config, Direction, LinkId, Recognised, Region, System};

pub const DEFAULT_MAGIC: u32 = 0xDEAD_FEED;
pub const DEFAULT_HOPS: [u8; 6] = [22, 10, 19, 34, 49, 41];
pub const DEFAULT_SPACING_HZ: u32 = 50_000;
pub const TELEMETRY_SIZE: usize = 9;
/// `pktsizes[]`: packet size by channel config 1-6.
pub const PACKET_SIZES: [usize; 6] = [7, 11, 12, 16, 17, 21];

/// (bit/s, deviation Hz) by `modem_params` index.
pub const RATES: [(u32, u32); 5] = [
    (4800, 30_000),
    (9600, 30_000),
    (19200, 30_000),
    (57600, 28_750),
    (125_000, 125_000),
];

/// Default base frequency for a region's RFM build.
pub const fn base_freq(region: Region) -> Option<u32> {
    match region {
        Region::Eu868 => Some(868_000_000),
        Region::Fcc915 | Region::Au915 => Some(915_000_000),
        Region::Eu433 | Region::Au433 | Region::Us433 | Region::Us433W => Some(435_000_000),
        _ => None,
    }
}

pub fn rx_config(region: Region, freq_hz: u32, rate: u8, bind: bool) -> Option<RxConfig> {
    let (br, fdev) = *RATES.get(rate as usize)?;
    Some(RxConfig {
        freq_hz,
        modulation: Modulation::Gfsk(Gfsk {
            bitrate_bps: br,
            fdev_hz: fdev,
            rx_bw_hz: 2 * fdev + br,
            preamble_bits: 40,
            sync_word: [0x2D, 0xD4, 0, 0],
            sync_len_bits: 16,
            read_len: 32,
            radio_whitening: false,
            radio_crc: false,
        }),
        probe: Probe::OpenLrs { rate, bind },
        region,
    })
}

/// `servoBits2Us`.
pub fn bits_to_us(x: u16) -> u16 {
    if x < 12 {
        808 + x * 16
    } else if x < 1012 {
        x + 988
    } else {
        2000 + (x.min(1023) - 1012) * 16
    }
}

/// `unpackChannels(config, ...)`.
pub fn unpack(config: usize, p: &[u8]) -> Channels {
    let mut c = Channels::default();
    let mut ch = 0;
    let mut o = 0;
    for _ in 0..=config / 2 {
        if o + 5 > p.len() {
            break;
        }
        let hi = p[o + 4] as u16;
        for k in 0..4 {
            let v = (((hi >> (2 * k)) & 3) << 8) | p[o + k] as u16;
            c.set(ch, bits_to_us(v));
            ch += 1;
        }
        o += 5;
    }
    if config & 1 == 1
        && let Some(&b) = p.get(o)
    {
        for k in 0..4 {
            let v = ((b >> (6 - 2 * k)) & 3) as u16 * 333 + 12;
            c.set(ch + k, bits_to_us(v));
        }
    }
    c
}

pub fn recognise(raw: &[u8], rx: &RxConfig, meta: RxMeta, cfg: &Config) -> Option<Recognised> {
    let (hdr, pkt) = si443x_parse(raw, 4, 0x1021)?;
    let magic = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
    let (&b0, body) = pkt.split_first()?;
    let kind = b0 & 0x3F;
    let bind = b0 == b'b' || b0 == b'B';
    let telemetry = pkt.len() == TELEMETRY_SIZE && (kind == 0 || (0x38..=0x3F).contains(&kind));
    let rc_cfg = PACKET_SIZES
        .iter()
        .position(|&s| s == pkt.len())
        .map(|i| i + 1);
    let uplink = rc_cfg.is_some() && matches!(kind, 0x00 | 0x01 | 0x38..=0x3F);
    if !(bind || telemetry || uplink) {
        return None;
    }
    let dir = if telemetry && !uplink {
        Direction::Downlink
    } else if uplink {
        Direction::Uplink
    } else {
        Direction::Unknown
    };
    let mut s = crate::sighting(System::OpenLrsNg, rx, meta, Confidence::Confirmed, dir);
    if cfg.read_ids {
        s.id = Some(LinkId::OpenLrsMagic(magic));
    }
    let mut r = Recognised::new(s);
    if cfg.decode_content {
        r.keep_raw(cfg, &raw[..(4 + 1 + pkt.len() + 2).min(raw.len())]);
        match (uplink, rc_cfg, kind) {
            (true, Some(c), 0x00 | 0x01) => r.push(ControlContent::Channels(unpack(c, body))),
            _ if bind => {}
            _ => r.push(ControlContent::Opaque(Opaque::Unparsed)),
        }
    }
    Some(r)
}

/// Test-only encoder: an OpenLRSng RC packet as received after the sync.
#[cfg(test)]
pub fn encode(magic: u32, hop: u8, config: usize, ppm: &[u16; 16]) -> std::vec::Vec<u8> {
    let mut f = std::vec::Vec::new();
    let m = magic ^ hop as u32;
    f.extend_from_slice(&m.to_be_bytes());
    let size = PACKET_SIZES[config - 1];
    f.push(size as u8);
    let mut p = std::vec![0u8; size];
    p[0] = 0x40; // seq bit, servo data
    let mut o = 1;
    let mut ch = 0;
    for _ in 0..=config / 2 {
        p[o..o + 4].copy_from_slice(&[
            ppm[ch] as u8,
            ppm[ch + 1] as u8,
            ppm[ch + 2] as u8,
            ppm[ch + 3] as u8,
        ]);
        p[o + 4] = ((ppm[ch] >> 8) & 3) as u8
            | (((ppm[ch + 1] >> 8) & 3) << 2) as u8
            | (((ppm[ch + 2] >> 8) & 3) << 4) as u8
            | (((ppm[ch + 3] >> 8) & 3) << 6) as u8;
        o += 5;
        ch += 4;
    }
    f.extend_from_slice(&p);
    let c = crate::crc::crc16_msb(0x1021, 0, &f);
    f.extend_from_slice(&c.to_be_bytes());
    f.resize(32, 0x55);
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rc_packet_round_trip() {
        let rx = rx_config(
            Region::Eu433,
            435_000_000 + 22 * DEFAULT_SPACING_HZ,
            2,
            false,
        )
        .unwrap();
        let mut ppm = [512u16; 16];
        ppm[0] = 12;
        ppm[1] = 1011;
        let f = encode(0x1234_5678, 3, 2, &ppm);
        let meta = RxMeta {
            rssi_dbm: -60,
            snr_db: 0,
            t_us: 0,
        };
        let r = crate::recognise(&f, &rx, meta, &Config::default()).unwrap();
        assert_eq!(r.sighting.confidence, Confidence::Confirmed);
        assert_eq!(r.sighting.direction, Direction::Uplink);
        assert!(r.sighting.id.is_none());
        let r = crate::recognise(
            &f,
            &rx,
            meta,
            &Config {
                read_ids: true,
                decode_content: true,
            },
        )
        .unwrap();
        assert_eq!(r.sighting.id, Some(LinkId::OpenLrsMagic(0x1234_567B)));
        let ControlContent::Channels(c) = &r.content[0] else {
            panic!()
        };
        assert_eq!(c.get(0), Some(1000));
        assert_eq!(c.get(1), Some(1999));
        assert_eq!(c.get(2), Some(1500));
        assert_eq!(c.get(7), Some(1500));
        assert_eq!(c.get(8), None);
    }

    #[test]
    fn random_reads_rejected() {
        let rx = rx_config(Region::Eu433, 435_000_000, 2, false).unwrap();
        let mut x = 11u64;
        for _ in 0..50_000 {
            let mut b = [0u8; 32];
            for v in b.iter_mut() {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                *v = x as u8;
            }
            assert!(crate::recognise(&b, &rx, RxMeta::default(), &Config::default()).is_none());
        }
    }

    #[test]
    fn sik_frame_is_not_openlrs() {
        let rx = rx_config(Region::Eu433, 435_000_000, 2, false).unwrap();
        let f = crate::sik::encode(25, &[1, 2, 3], 0);
        assert!(crate::recognise(&f, &rx, RxMeta::default(), &Config::default()).is_none());
    }
}
