//! OGN tracker packets (the Open Glider Network's open radio protocol).
//!
//! Ported from SoftRF's `protocol/radio/OGNTP.cpp` (`ogntp_decode`) and the
//! OGN tracker library it carries (`libraries/OGN/ogn.h`: `OGN_Packet`,
//! `ldpc.cpp`: `LDPC_Check`), GPL-3.0. Air format: 2-GFSK, 100 kchip/s,
//! Manchester; after the sync word, 26 bytes: a 32-bit header, four 32-bit
//! words of position data (whitened with TEA, all-zero key), and 48 bits of
//! LDPC parity. Words are little-endian, as the tracker's C structs lay
//! them out in memory; bit fields are numbered from the least significant
//! bit.
//!
//! Like SoftRF, packets with any failed parity check are refused (no
//! soft-decision repair), as are status/meteo packets ("other") and
//! encrypted ones (only the owner's key can open those).

use crate::ldpc_tables::PARITY_CHECK;
use crate::tea::tea_decrypt_key0;
use crate::{AddressType, AircraftType, Decoded, Motion, record};

/// Bytes on air after Manchester decoding: 20 data + 6 parity.
pub const PACKET_BYTES: usize = 26;
/// Data bytes (header and four position words).
pub const DATA_BYTES: usize = 20;

/// Why a packet was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Fewer than [`PACKET_BYTES`] bytes.
    TooShort,
    /// This many of the 48 LDPC parity checks failed.
    Parity(u8),
    /// A status, meteo or other non-position packet.
    NotPosition,
    /// Encrypted by its owner; we have no key.
    Encrypted,
}

/// The number of failed LDPC parity checks over 26 bytes (0 = good), as
/// the OGN code's `LDPC_Check`.
pub fn ldpc_check(packet: &[u8; PACKET_BYTES]) -> u8 {
    let mut failed = 0u8;
    for row in &PARITY_CHECK {
        let mut ones = 0u32;
        for (i, &b) in packet.iter().enumerate() {
            let mask = (row[i / 4] >> (8 * (i % 4))) as u8;
            ones += (b & mask).count_ones();
        }
        failed += (ones & 1) as u8;
    }
    failed
}

fn word(p: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([p[4 * i], p[4 * i + 1], p[4 * i + 2], p[4 * i + 3]])
}

fn bits(w: u32, lsb: u32, n: u32) -> u32 {
    (w >> lsb) & ((1u32 << n) - 1)
}

fn sign_extend(v: u32, n: u32) -> i32 {
    let s = 32 - n;
    ((v << s) as i32) >> s
}

// The OGN tracker's variable-resolution codes (`ogn.h`).

/// 14-bit code to 0..61432 (`DecodeUR2V12`).
pub(crate) fn decode_ur2v12(v: u32) -> u32 {
    let range = (v >> 12) & 3;
    let v = v & 0x0fff;
    match range {
        0 => v,
        1 => 0x1001 + (v << 1),
        2 => 0x3002 + (v << 2),
        _ => 0x7004 + (v << 3),
    }
}

/// 10-bit code to 0..3832 (`DecodeUR2V8`).
pub(crate) fn decode_ur2v8(v: u32) -> u32 {
    let range = (v >> 8) & 3;
    let v = v & 0xff;
    match range {
        0 => v,
        1 => 0x101 + (v << 1),
        2 => 0x302 + (v << 2),
        _ => 0x704 + (v << 3),
    }
}

/// 9-bit signed code to -952..952 (`DecodeSR2V6`).
pub(crate) fn decode_sr2v6(v: u32) -> i32 {
    let range = (v >> 6) & 3;
    let m = v & 0x3f;
    let mag = match range {
        0 => m,
        1 => 0x41 + (m << 1),
        2 => 0xC2 + (m << 2),
        _ => 0x1C4 + (m << 3),
    } as i32;
    if v & 0x100 != 0 { -mag } else { mag }
}

/// The header and de-whitened position words of a good packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Packet {
    pub header: u32,
    pub data: [u32; 4],
}

impl Packet {
    pub fn address(&self) -> u32 {
        self.header & 0x00ff_ffff
    }
    pub fn address_type(&self) -> AddressType {
        AddressType::from_bits(bits(self.header, 24, 2) as u8)
    }
    pub fn other(&self) -> bool {
        self.header & (1 << 26) != 0
    }
    /// Even parity over header bits 0-27 (the address parity bit makes it so).
    pub fn good_address_parity(&self) -> bool {
        (self.header & 0x0fff_ffff).count_ones() & 1 == 0
    }
    pub fn relay_count(&self) -> u8 {
        bits(self.header, 28, 2) as u8
    }
    pub fn encrypted(&self) -> bool {
        self.header & (1 << 30) != 0
    }
    pub fn emergency(&self) -> bool {
        self.header & (1 << 31) != 0
    }

    /// Latitude, 1/600000 degree (`DecodeLatitude`).
    pub fn latitude(&self) -> i32 {
        (sign_extend(bits(self.data[0], 0, 24), 24) << 3) + 4
    }
    /// Seconds field of the fix time (0-59, or 60+ for none).
    pub fn time_s(&self) -> u8 {
        bits(self.data[0], 24, 6) as u8
    }
    /// 0 none, 1 GPS, 2 DGPS, 3 other. SoftRF sends 0 even with a fix.
    pub fn fix_quality(&self) -> u8 {
        bits(self.data[0], 30, 2) as u8
    }
    /// Longitude, 1/600000 degree (`DecodeLongitude`).
    pub fn longitude(&self) -> i32 {
        (sign_extend(bits(self.data[1], 0, 24), 24) << 4) + 8
    }
    /// GNSS altitude, metres (`DecodeAltitude`).
    pub fn altitude_m(&self) -> i32 {
        decode_ur2v12(bits(self.data[2], 0, 14)) as i32
    }
    /// Ground speed, 0.1 m/s (`DecodeSpeed`).
    pub fn speed_dmps(&self) -> u32 {
        decode_ur2v8(bits(self.data[2], 14, 10))
    }
    /// Heading (track), 0.1 degree (`DecodeHeading`).
    pub fn heading_ddeg(&self) -> u32 {
        (bits(self.data[3], 0, 10) * 3600 + 512) >> 10
    }
    /// Climb rate, 0.1 m/s (`DecodeClimbRate`).
    pub fn climb_dmps(&self) -> i32 {
        decode_sr2v6(bits(self.data[3], 10, 9))
    }
    pub fn stealth(&self) -> bool {
        self.data[3] & (1 << 19) != 0
    }
    pub fn aircraft_type(&self) -> AircraftType {
        AircraftType(bits(self.data[3], 20, 4) as u8)
    }
    /// A pressure altitude is present (`hasBaro`).
    pub fn has_baro(&self) -> bool {
        self.data[1] & (1 << 30) != 0 || bits(self.data[3], 24, 8) != 0
    }
    /// Pressure altitude, metres (`DecodeStdAltitude`).
    pub fn std_altitude_m(&self) -> i32 {
        let mut diff = bits(self.data[3], 24, 8) as u16;
        if self.data[1] & (1 << 30) == 0 {
            diff |= 0xff00;
        }
        self.altitude_m() + i32::from(diff as i16)
    }
}

/// Check the parity, refuse non-position and encrypted packets, and
/// de-whiten. `packet` must hold at least [`PACKET_BYTES`] bytes.
pub fn open(packet: &[u8]) -> Result<Packet, Error> {
    let p: &[u8; PACKET_BYTES] = packet
        .get(..PACKET_BYTES)
        .and_then(|s| s.try_into().ok())
        .ok_or(Error::TooShort)?;
    let failed = ldpc_check(p);
    if failed != 0 {
        return Err(Error::Parity(failed));
    }
    let header = word(p, 0);
    let mut pk = Packet {
        header,
        data: [word(p, 1), word(p, 2), word(p, 3), word(p, 4)],
    };
    if pk.other() {
        return Err(Error::NotPosition);
    }
    if pk.encrypted() {
        return Err(Error::Encrypted);
    }
    // `Dewhiten`: TEA, zero key, 8 loops, over each pair of words.
    let mut a = [pk.data[0], pk.data[1]];
    let mut b = [pk.data[2], pk.data[3]];
    tea_decrypt_key0(&mut a, 8);
    tea_decrypt_key0(&mut b, 8);
    pk.data = [a[0], a[1], b[0], b[1]];
    Ok(pk)
}

/// Decode one OGN packet (26 bytes as the radio delivers them) into the
/// aircraft record: the call the hub makes for every OGN packet.
pub fn decode(packet: &[u8]) -> Result<Decoded, Error> {
    let p = open(packet)?;
    let ty = p.aircraft_type();
    let speed = f64::from(p.speed_dmps()) * 0.1;
    let m = Motion {
        lat_lon: Some((
            f64::from(p.latitude()) / 600_000.0,
            f64::from(p.longitude()) / 600_000.0,
        )),
        gnss_alt_m: Some(f64::from(p.altitude_m())),
        baro_alt_m: p.has_baro().then(|| f64::from(p.std_altitude_m())),
        track_deg: Some(p.heading_ddeg() as f32 / 10.0),
        speed_mps: Some(speed),
        climb_mps: Some(f64::from(p.climb_dmps()) * 0.1),
        // OGN has no air/ground bit: judge by speed, as FLARM does.
        airborne: ty
            .airborne_threshold_mps()
            .is_some_and(|t| speed >= f64::from(t)),
    };
    let aircraft = record(
        adsb::Source::Ogn,
        p.address(),
        p.address_type(),
        ty,
        u8::from(p.emergency()),
        &m,
    );
    Ok(Decoded {
        aircraft,
        address_type: p.address_type(),
        aircraft_type: ty,
        stealth: p.stealth(),
        no_track: false,
        relay_count: p.relay_count(),
    })
}
