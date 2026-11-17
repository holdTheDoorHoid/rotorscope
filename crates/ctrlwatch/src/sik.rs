//! SiK / RFD900 telemetry radios (github.com/ArduPilot/SiK, BSD-2-Clause;
//! `Firmware/radio/{radio_443x.c, tdm.c, freq_hopping.c, main.c,
//! parameters.c}`), and the Si443x packet framing they share with
//! OpenLRSng's RFM22B.
//!
//! Air format (Si443x packet handler, ECC off, the SiK default): preamble
//! (16 nibbles), sync word 0x2D 0xD4, two header bytes = NET ID (high,
//! low), a length byte, the data, a hardware CRC-16 ("CRC-16 IBM",
//! polynomial 0x8005). Data whitening (PN9) is on at all air speeds. The
//! data ends with SiK's 2-byte TDM trailer (window:13, command, bonus,
//! resend; 4 bytes with AES); everything before it is the serial stream,
//! normally MAVLink.
//!
//! The node receives with the radio's whitening and CRC off and a fixed
//! read length; [`recognise`] de-whitens, finds the length byte and checks
//! the CRC in software. A frame whose CRC checks (with either of the two
//! possible CRC seeds) and whose length is consistent is
//! [`Confidence::Confirmed`].
//!
//! Unverified without hardware: the exact PN9 bit order (implemented as
//! TI's CC1101 PN9, which is the same polynomial and seed), the CRC seed
//! (both 0x0000 and 0xFFFF are accepted), and the 16-bit-`int` overflow in
//! SiK's NET ID frequency offset (SDCC `int` is 16 bits; see
//! [`channel_plan`]).
//!
//! ECC mode (Golay 23/12, off by default) has no hardware header or CRC,
//! and its CRC sits inside the Golay-coded body; it is not decoded here, so
//! ECC-mode links are seen by energy only.

use crate::content::{ControlContent, Opaque};
use crate::crc::{crc16_msb, pn9_xor};
use crate::rx::{Gfsk, Modulation, Probe, RxConfig, RxMeta};
use crate::{Confidence, Config, Direction, LinkId, Recognised, Region, System};

pub const SYNC: [u8; 2] = [0x2D, 0xD4];
pub const DEFAULT_NETID: u16 = 25;
/// Bytes the node should read after the sync word.
pub const READ_LEN: u8 = 255;

/// Parse a Si443x packet: `hdr_len` header bytes, a length byte, data and a
/// CRC-16 over header + length + data. Returns (header, data).
pub(crate) fn si443x_parse(buf: &[u8], hdr_len: usize, poly: u16) -> Option<(&[u8], &[u8])> {
    let len = *buf.get(hdr_len)? as usize;
    let end = hdr_len + 1 + len;
    let crc = u16::from_be_bytes([*buf.get(end)?, *buf.get(end + 1)?]);
    let ok = [0x0000u16, 0xFFFF]
        .iter()
        .any(|&seed| crc16_msb(poly, seed, &buf[..end]) == crc);
    ok.then(|| (&buf[..hdr_len], &buf[hdr_len + 1..end]))
}

/// GFSK parameters for an air speed (kbit/s), from SiK's 915 MHz register
/// table: deviation register 0x72 in 625 Hz units.
pub fn gfsk(kbps: u8) -> Option<Gfsk> {
    let fdev_reg: u32 = match kbps {
        2 => 0x03,
        4 => 0x06,
        8 => 0x0D,
        16 => 0x1A,
        19 => 0x1E,
        24 => 0x26,
        32 => 0x33,
        48 => 0x4D,
        64 => 0x66,
        96 => 0x9A,
        128 => 0xCD,
        192 | 250 => 0xFE,
        _ => return None,
    };
    let br = kbps as u32 * 1000;
    let fdev = fdev_reg * 625;
    Some(Gfsk {
        bitrate_bps: br,
        fdev_hz: fdev,
        rx_bw_hz: 2 * fdev + br,
        preamble_bits: 64,
        sync_word: [SYNC[0], SYNC[1], 0, 0],
        sync_len_bits: 16,
        read_len: READ_LEN,
        radio_whitening: false,
        radio_crc: false,
    })
}

/// SiK's `r_rand`, with SDCC's 32-bit `unsigned long` state.
struct RRand(u32);
impl RRand {
    fn next(&mut self) -> u16 {
        self.0 = self.0.wrapping_mul(1_103_515_245).wrapping_add(12345);
        ((self.0 / 65536) % 32768) as u16
    }
}

/// Channel plan for a board band and NET ID: (first channel Hz, spacing
/// Hz, channel count), as `radio_init` computes it with default
/// MIN/MAX_FREQ and NUM_CHANNELS.
pub fn channel_plan(region: Region, netid: u16) -> Option<(u32, u32, u8)> {
    let (min, max, n): (u32, u32, u8) = match region {
        Region::Fcc915 | Region::Au915 => (915_000_000, 928_000_000, 50),
        Region::Eu868 => (868_000_000, 870_000_000, 10),
        Region::Eu433 | Region::Au433 | Region::Us433 | Region::Us433W => {
            (433_050_000, 434_790_000, 10)
        }
        _ => return None,
    };
    let spacing = (max - min) / (n as u32 + 2);
    let mut start = min + spacing / 2;
    if n > 5 {
        let mut r = RRand(netid as u32);
        // `(unsigned long)(r_rand()*625)`: the product is a 16-bit int on
        // the 8051 (SDCC), so it wraps and sign-extends before the cast.
        let prod = (r.next() as i32 * 625) as i16 as i32 as u32;
        start += prod % spacing;
    }
    Some((start, spacing, n))
}

pub fn rx_config(region: Region, netid: u16, channel: u8, kbps: u8) -> Option<RxConfig> {
    let (start, spacing, n) = channel_plan(region, netid)?;
    if channel >= n {
        return None;
    }
    Some(RxConfig {
        freq_hz: start + channel as u32 * spacing,
        modulation: Modulation::Gfsk(gfsk(kbps)?),
        probe: Probe::Sik { kbps },
        region,
    })
}

pub fn recognise(raw: &[u8], rx: &RxConfig, meta: RxMeta, cfg: &Config) -> Option<Recognised> {
    let mut buf = [0u8; 258];
    let n = raw.len().min(buf.len());
    buf[..n].copy_from_slice(&raw[..n]);
    let buf = &mut buf[..n];
    pn9_xor(buf);
    if let Some((hdr, data)) = si443x_parse(buf, 2, 0x8005) {
        // At least the 2-byte TDM trailer.
        if data.len() < 2 {
            return None;
        }
        let netid = u16::from_be_bytes([hdr[0], hdr[1]]);
        let mut s = crate::sighting(
            System::Sik,
            rx,
            meta,
            Confidence::Confirmed,
            Direction::Unknown,
        );
        if cfg.read_ids {
            s.id = Some(LinkId::SikNetId(netid));
        }
        let mut r = Recognised::new(s);
        if cfg.decode_content {
            r.keep_raw(cfg, &buf[..(2 + 1 + data.len() + 2).min(buf.len())]);
            let trailer = u16::from_le_bytes([data[data.len() - 2], data[data.len() - 1]]);
            let command = trailer & (1 << 13) != 0;
            let serial = &data[..data.len() - 2];
            if !serial.is_empty() && !command {
                let found = crate::mavlink::decode_all(serial, |c| r.push(c));
                if found == 0 {
                    // AES (4-byte trailer, encrypted body) or non-MAVLink.
                    r.push(ControlContent::Opaque(Opaque::Unparsed));
                }
            }
        }
        return Some(r);
    }
    None
}

/// Test-only encoder: a whitened SiK frame as the radio would send it
/// (after the sync word), padded to `READ_LEN` with noise.
#[cfg(test)]
pub fn encode(netid: u16, serial: &[u8], trailer: u16) -> std::vec::Vec<u8> {
    let mut f = std::vec::Vec::new();
    f.extend_from_slice(&netid.to_be_bytes());
    f.push((serial.len() + 2) as u8);
    f.extend_from_slice(serial);
    f.extend_from_slice(&trailer.to_le_bytes());
    let c = crc16_msb(0x8005, 0xFFFF, &f);
    f.extend_from_slice(&c.to_be_bytes());
    pn9_xor(&mut f);
    let mut x = 0xACE1u16;
    while f.len() < READ_LEN as usize {
        x = (x >> 1) ^ (if x & 1 != 0 { 0xB400 } else { 0 });
        f.push(x as u8);
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::PositionKind;

    fn rx() -> RxConfig {
        rx_config(Region::Fcc915, DEFAULT_NETID, 0, 64).unwrap()
    }
    fn meta() -> RxMeta {
        RxMeta {
            rssi_dbm: -90,
            snr_db: 0,
            t_us: 3,
        }
    }

    #[test]
    fn channel_plan_default_netid() {
        let (start, spacing, n) = channel_plan(Region::Fcc915, 25).unwrap();
        assert_eq!((spacing, n), (250_000, 50));
        assert!((915_125_000..915_375_000).contains(&start));
        assert!(start + 49 * spacing < 928_000_000);
    }

    #[test]
    fn mavlink_over_sik_decoded_with_netid_opt_in() {
        let mut serial = crate::mavlink::encode_v1(
            0,
            1,
            1,
            crate::mavlink::MSG_HEARTBEAT,
            &crate::mavlink::heartbeat_payload(),
        );
        let mut home = std::vec![0u8; 52];
        home[0..4].copy_from_slice(&399_100_000i32.to_le_bytes());
        home[4..8].copy_from_slice(&(-751_200_000i32).to_le_bytes());
        serial.extend(crate::mavlink::encode_v2(
            1,
            1,
            1,
            crate::mavlink::MSG_HOME_POSITION,
            &home,
        ));
        let f = encode(DEFAULT_NETID, &serial, 120);
        let r = crate::recognise(&f, &rx(), meta(), &Config::default()).unwrap();
        assert_eq!(r.sighting.confidence, Confidence::Confirmed);
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
        assert_eq!(r.sighting.id, Some(LinkId::SikNetId(25)));
        assert_eq!(r.content.len(), 2);
        assert!(
            matches!(r.content[1], ControlContent::Position(p) if p.kind == PositionKind::Home)
        );
    }

    #[test]
    fn encrypted_body_is_opaque_and_noise_rejected() {
        let f = encode(7, &[0x11; 40], 0);
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
        assert_eq!(r.content[0], ControlContent::Opaque(Opaque::Unparsed));
        let mut x = 5u64;
        let mut hits = 0;
        for _ in 0..5_000 {
            let mut b = [0u8; 255];
            for v in b.iter_mut() {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                *v = x as u8;
            }
            if let Some(r) = crate::recognise(&b, &rx(), meta(), &Config::default())
                && r.sighting.confidence == Confidence::Confirmed
            {
                hits += 1;
            }
        }
        assert_eq!(hits, 0);
    }
}
