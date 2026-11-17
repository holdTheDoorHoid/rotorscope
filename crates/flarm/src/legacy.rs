//! FLARM "legacy" radio packets, versions 6 and 7 (cargo feature `legacy`).
//!
//! Ported from SoftRF's `protocol/radio/Legacy.cpp` (`legacy_decode`,
//! `legacy_v6_decode`, `make_v6_key`, `make_v7_key`, `obscure`, `btea`,
//! `descale`; decoder by Stanislaw Pusep, v7 by Linar Yusupov,
//! GPL-3.0-or-later) and the layouts in its `Legacy.h`. Receive only.
//!
//! Every key constant comes from [`Keys`], read at run time from the key
//! file; see [`crate::keys`] for the format and the rules (never commit,
//! print, log or relay them).
//!
//! **Input**: the 24 payload bytes after Manchester decoding, as SoftRF's
//! driver delivers them, optionally followed by the two CRC bytes (then
//! the CRC is checked here; with 24 bytes the radio node must have checked
//! it). **Time and place**: both versions derive their key from the current
//! UTC time (Unix seconds; v6 in 64 s steps, v7 in 16 s steps), and send
//! only the low bits of latitude and longitude, so decoding needs the
//! receiver's own time and fix ([`Receiver`]). A wrong time gives a wrong
//! key: v6 then usually fails its parity check; v7 has no check of its own
//! (SoftRF's timestamp check is disabled), so a hub without a GNSS time
//! must not feed v7 packets in.
//!
//! **Altitude**: FLARM sends height above the WGS-84 ellipsoid, which is
//! what [`adsb::Aircraft::gnss_alt_ft`] holds; SoftRF subtracts the geoid
//! separation for display, we do not.

use crate::keys::Keys;
use crate::tea::xxtea_decrypt;
use crate::{AddressType, AircraftType, Decoded, Motion, bearing_deg, record};

/// Payload bytes.
pub const PAYLOAD_BYTES: usize = 24;
/// Payload plus the CRC-16.
pub const PACKET_BYTES: usize = 26;

/// Why a packet was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Not 24 or 26 bytes.
    Length,
    /// The CRC-16 does not match.
    Crc,
    /// A packet type other than v6 (0) or v7 (2) position.
    NotPosition(u8),
    /// v6: the decrypted packet's parity is odd (wrong key/time, or noise).
    Parity,
}

/// The receiver's own time and position, which FLARM decoding needs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Receiver {
    /// UTC, Unix seconds (from GNSS).
    pub unix_time: u32,
    pub lat_deg: f64,
    pub lon_deg: f64,
}

/// CRC-16/CCITT-FALSE as SoftRF's driver computes it for legacy packets:
/// seeded 0xFFFF, over the last three sync bytes (0x31 0xFA 0xB6, the
/// nRF905 "address" FLARM inherited) and then the payload.
pub fn crc16(payload: &[u8]) -> u16 {
    let mut crc = 0xffffu16;
    for &b in [0x31u8, 0xFA, 0xB6].iter().chain(payload) {
        crc ^= u16::from(b) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// SoftRF's `obscure`.
fn obscure(key: u32, seed: u32) -> u32 {
    let m1 = seed.wrapping_mul(key ^ (key >> 16));
    let m2 = seed.wrapping_mul(m1 ^ (m1 >> 16));
    m2 ^ (m2 >> 16)
}

/// SoftRF's `make_v6_key`.
pub(crate) fn v6_key(keys: &Keys, timestamp: u32, address: u32) -> [u32; 4] {
    let mut key = [0u32; 4];
    for (i, k) in key.iter_mut().enumerate() {
        let ndx = if (timestamp >> 23) & 1 != 0 { i + 4 } else { i };
        *k = obscure(keys.table[ndx] ^ ((timestamp >> 6) ^ address), keys.mix) ^ keys.xor;
    }
    key
}

/// SoftRF's `make_v7_key`: two passes of a byte-wise XXTEA-like mix over
/// the 16 key bytes (little-endian words).
pub(crate) fn v7_key(words: [u32; 4]) -> [u32; 4] {
    const DELTA: u32 = 0x9e37_79b9;
    let mut b = [0u8; 16];
    for (i, w) in words.iter().enumerate() {
        b[4 * i..4 * i + 4].copy_from_slice(&w.to_le_bytes());
    }
    let mut x = u32::from(b[15]);
    let mut sum = 0u32;
    for _ in 0..2 {
        sum = sum.wrapping_add(DELTA);
        for p in 0..16 {
            let z = x & 0xff;
            let y = u32::from(b[(p + 1) % 16]);
            x = u32::from(b[p]);
            x = x.wrapping_add(
                (((z >> 5) ^ (y << 2)).wrapping_add((y >> 3) ^ (z << 4))) ^ (sum ^ y),
            );
            b[p] = x as u8;
        }
    }
    let w = |i: usize| u32::from_le_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]);
    [w(0), w(1), w(2), w(3)]
}

/// SoftRF's `descale`: a value with `mbits` of mantissa and `ebits` of
/// exponent, and a sign bit above them.
pub(crate) fn descale(value: u32, mbits: u32, ebits: u32) -> i32 {
    let offset = 1u32 << mbits;
    let signbit = offset << ebits;
    let negative = value & signbit != 0;
    let mut v = value & (signbit - 1);
    if v >= offset {
        let exp = v >> mbits;
        v &= offset - 1;
        v += offset;
        v <<= exp;
        v -= offset;
    }
    if negative { -(v as i32) } else { v as i32 }
}

/// Longitude scale for FLARM v7 by whole degrees of latitude, from 14
/// degrees up (`lon_div_table`; below 14 the scale is 52).
const LON_DIV: [u16; 77] = [
    53, 53, 54, 54, 55, 55, 56, 56, 57, 57, 58, 58, 59, 59, 60, 60, 61, 61, 62, 62, 63, 63, 64, 64,
    65, 65, 67, 68, 70, 71, 73, 74, 76, 77, 79, 80, 82, 83, 85, 86, 88, 89, 91, 94, 98, 101, 105,
    108, 112, 115, 119, 122, 126, 129, 137, 144, 152, 159, 167, 174, 190, 205, 221, 236, 252, 267,
    299, 330, 362, 425, 489, 552, 616, 679, 743, 806, 806,
];

pub(crate) fn lon_div(lat_deg: f64) -> i32 {
    let ilat = (libm::fabs(lat_deg) as i32).min(89);
    if ilat < 14 {
        52
    } else {
        i32::from(LON_DIV[(ilat - 14) as usize])
    }
}

/// Undo the truncation of a coordinate: `field` holds the low `bits` bits
/// of `value / scale`; pick the value nearest the receiver's.
fn unwrap(field: u32, bits: u32, reference: i32) -> i32 {
    let modulus = 1u32 << bits;
    let mut d = (field.wrapping_sub(reference as u32) % modulus) as i32;
    if d >= (modulus / 2) as i32 {
        d -= modulus as i32;
    }
    d + reference
}

fn words(p: &[u8; PAYLOAD_BYTES]) -> [u32; 6] {
    let mut w = [0u32; 6];
    for (i, x) in w.iter_mut().enumerate() {
        *x = u32::from_le_bytes([p[4 * i], p[4 * i + 1], p[4 * i + 2], p[4 * i + 3]]);
    }
    w
}

fn to_bytes(w: &[u32; 6]) -> [u8; PAYLOAD_BYTES] {
    let mut p = [0u8; PAYLOAD_BYTES];
    for (i, x) in w.iter().enumerate() {
        p[4 * i..4 * i + 4].copy_from_slice(&x.to_le_bytes());
    }
    p
}

/// `n` bits from bit `lsb` of the packet read as one little-endian bit
/// stream (how GCC lays out SoftRF's packed bit-field structs).
fn field(p: &[u8; PAYLOAD_BYTES], lsb: usize, n: usize) -> u32 {
    let mut v = 0u32;
    for i in 0..n {
        let b = lsb + i;
        v |= u32::from((p[b / 8] >> (b % 8)) & 1) << i;
    }
    v
}

fn sign_extend(v: u32, n: u32) -> i32 {
    let s = 32 - n;
    ((v << s) as i32) >> s
}

/// Decode one legacy packet: the call the hub makes for every FLARM
/// packet, with its GNSS time and fix.
pub fn decode(packet: &[u8], keys: &Keys, rx: &Receiver) -> Result<Decoded, Error> {
    let payload: &[u8; PAYLOAD_BYTES] = match packet.len() {
        PAYLOAD_BYTES => packet.try_into().map_err(|_| Error::Length)?,
        PACKET_BYTES => {
            let want = u16::from_be_bytes([packet[24], packet[25]]);
            if crc16(&packet[..PAYLOAD_BYTES]) != want {
                return Err(Error::Crc);
            }
            packet[..PAYLOAD_BYTES]
                .try_into()
                .map_err(|_| Error::Length)?
        }
        _ => return Err(Error::Length),
    };
    match payload[3] & 0x0f {
        0 => decode_v6(payload, keys, rx),
        2 => decode_v7(payload, keys, rx),
        t => Err(Error::NotPosition(t)),
    }
}

fn decode_v6(payload: &[u8; PAYLOAD_BYTES], keys: &Keys, rx: &Receiver) -> Result<Decoded, Error> {
    let mut w = words(payload);
    let addr = w[0] & 0x00ff_ffff;
    let key = v6_key(keys, rx.unix_time, (addr << 8) & 0x00ff_ffff);
    xxtea_decrypt(&mut w[1..6], &key, 6);
    let p = to_bytes(&w);
    let ones: u32 = p.iter().map(|b| b.count_ones()).sum();
    if !ones.is_multiple_of(2) {
        return Err(Error::Parity);
    }
    let addr_type = AddressType::from_bits(((w[0] >> 28) & 7) as u8);
    let vs = sign_extend(w[1] & 0x3ff, 10);
    let airborne = w[1] & (1 << 12) != 0;
    let stealth = w[1] & (1 << 13) != 0;
    let no_track = w[1] & (1 << 14) != 0;
    let ty = AircraftType((w[1] >> 28) as u8);
    let lat_f = w[2] & 0x7ffff;
    let alt = (w[2] >> 19) & 0x1fff;
    let lon_f = w[3] & 0xfffff;
    let smult = w[3] >> 30;

    let round_lat = ((rx.lat_deg * 1e7) as i32) >> 7;
    let lat = unwrap(lat_f, 19, round_lat) << 7;
    let round_lon = ((rx.lon_deg * 1e7) as i32) >> 7;
    let lon = unwrap(lon_f, 20, round_lon) << 7;

    // Four predicted velocity samples each way; their mean is now.
    let ns: i32 = p[16..20].iter().map(|&b| i32::from(b as i8)).sum::<i32>() / 4;
    let ew: i32 = p[20..24].iter().map(|&b| i32::from(b as i8)).sum::<i32>() / 4;
    let speed4 = libm::sqrt(f64::from(ew * ew + ns * ns)) * f64::from(1u32 << smult);
    let m = Motion {
        lat_lon: Some((f64::from(lat) / 1e7, f64::from(lon) / 1e7)),
        gnss_alt_m: Some(f64::from(alt)),
        baro_alt_m: None,
        track_deg: (speed4 > 0.0).then(|| bearing_deg(f64::from(ns), f64::from(ew))),
        speed_mps: Some(speed4 / 4.0),
        climb_mps: Some(f64::from(vs << smult) / 10.0),
        airborne,
    };
    Ok(Decoded {
        aircraft: record(adsb::Source::Flarm, addr, addr_type, ty, 0, &m),
        address_type: addr_type,
        aircraft_type: ty,
        stealth,
        no_track,
        relay_count: 0,
    })
}

fn decode_v7(payload: &[u8; PAYLOAD_BYTES], keys: &Keys, rx: &Receiver) -> Result<Decoded, Error> {
    let mut w = words(payload);
    xxtea_decrypt(&mut w[2..6], &keys.v7_xxtea, 6);
    let k = v7_key([w[0], w[1], rx.unix_time >> 4, keys.v7_word]);
    for i in 0..4 {
        w[2 + i] ^= k[i];
    }
    let p = to_bytes(&w);

    let addr = field(&p, 0, 24);
    let addr_type = AddressType::from_bits(field(&p, 28, 2) as u8);
    let urgency = field(&p, 30, 2) as u8;
    let stealth = field(&p, 54, 1) != 0;
    let no_track = field(&p, 55, 1) != 0;
    let ty = AircraftType(field(&p, 70, 5) as u8);
    let alt = descale(field(&p, 75, 13), 12, 1) - 1000;
    let lat_f = field(&p, 88, 20);
    let lon_f = field(&p, 108, 20);
    let hs = descale(field(&p, 137, 10), 8, 2);
    let vs = descale(field(&p, 147, 9), 6, 2);
    let course = field(&p, 156, 10);
    let airborne = field(&p, 166, 2) >= 2;

    let round_lat = ((rx.lat_deg * 1e7) as i32) / 52;
    let lat = unwrap(lat_f, 20, round_lat) * 52;
    let lat_deg = f64::from(lat) / 1e7;
    let div = lon_div(lat_deg);
    let round_lon = ((rx.lon_deg * 1e7) as i32) / div;
    let lon = unwrap(lon_f, 20, round_lon) * div;

    let m = Motion {
        lat_lon: Some((lat_deg, f64::from(lon) / 1e7)),
        gnss_alt_m: Some(f64::from(alt)),
        baro_alt_m: None,
        track_deg: Some(course as f32 / 2.0),
        speed_mps: Some(f64::from(hs) / 10.0),
        climb_mps: Some(f64::from(vs) / 10.0),
        airborne,
    };
    Ok(Decoded {
        aircraft: record(adsb::Source::Flarm, addr, addr_type, ty, urgency, &m),
        address_type: addr_type,
        aircraft_type: ty,
        stealth,
        no_track,
        relay_count: 0,
    })
}
