//! Receive-only decoders for the FLARM family on 868 MHz (Europe) and
//! 902-928 MHz (North America): PLAN.md Phase 14d. `no_std`, no allocator;
//! dependencies `adsb` (the output record) and `libm`.
//!
//! - [`ogn`]: OGN tracker packets (the open protocol): LDPC parity check,
//!   de-whitening, position, altitudes, climb, speed, heading, aircraft
//!   and address type, into an [`adsb::Aircraft`] with
//!   [`adsb::Source::Ogn`].
//! - `legacy` (cargo feature `legacy`, off by default): FLARM "legacy"
//!   packets, versions 6 and 7, as SoftRF decodes them (XXTEA decryption
//!   with keys derived from the time and the address, relative position
//!   against the receiver's own fix), into [`adsb::Source::Flarm`]. Every
//!   key constant comes from a [`Keys`] value parsed at run time from a
//!   small text file; **no key is in this source code**, and the keys must
//!   never be printed, logged or sent between boards.
//!
//! **Input** for both: the packet bytes after Manchester decoding, exactly
//! as SoftRF's radio driver hands them to its protocol decoders (its
//! `RxBuffer`): 26 bytes for OGN (20 data + 6 LDPC parity), 24 or 26 bytes
//! for FLARM (24 payload bytes, optionally followed by the 2 CRC bytes).
//!
//! **Receive only.** There is no transmit path in this crate. The tests
//! carry encoders (ports of SoftRF's) to make round-trip vectors; they are
//! `#[cfg(test)]` and never reach a firmware build.
//!
//! **Credit and licence.** Ported to Rust under Rotorscope's
//! GPL-3.0-or-later from SoftRF by Linar Yusupov
//! (<https://github.com/lyusupov/SoftRF>, GPL-3.0-or-later):
//! `protocol/radio/OGNTP.cpp` and `Legacy.cpp` (whose decoder is by
//! Stanislaw Pusep, GPL-3.0-or-later), and the OGN tracker library in
//! SoftRF's `libraries/OGN` (`ogn.h`, `ldpc.cpp`; the OGN tracker project
//! by Pawel Jalocha, GPL-3.0).
#![no_std]

mod ldpc_tables;
pub mod ogn;
mod tea;

#[cfg(feature = "legacy")]
mod keys;
#[cfg(feature = "legacy")]
pub mod legacy;

#[cfg(feature = "legacy")]
pub use keys::{KeyError, Keys};

#[cfg(all(test, feature = "legacy"))]
pub(crate) mod tests_legacy;
#[cfg(test)]
pub(crate) mod tests_ogn;
#[cfg(test)]
mod fuzz_tests;

const FT_PER_M: f64 = 1.0 / 0.3048;
const KT_PER_MPS: f64 = 3600.0 / 1852.0;
const FPM_PER_MPS: f64 = 60.0 / 0.3048;

/// Address type, the same numbering in OGN and FLARM headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressType {
    /// Random (changes over time; privacy).
    Random,
    /// The aircraft's 24-bit ICAO address.
    Icao,
    /// A FLARM device ID.
    Flarm,
    /// OGN tracker ID, or an anonymous (stealth) FLARM address.
    Ogn,
}

impl AddressType {
    pub const fn from_bits(b: u8) -> Self {
        match b & 3 {
            0 => AddressType::Random,
            1 => AddressType::Icao,
            2 => AddressType::Flarm,
            _ => AddressType::Ogn,
        }
    }

    pub const fn bits(self) -> u8 {
        self as u8
    }
}

/// FLARM / OGN aircraft type (4 bits; FLARM v7 uses 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AircraftType(pub u8);

impl AircraftType {
    pub const GLIDER: Self = Self(1);
    pub const TOWPLANE: Self = Self(2);
    pub const HELICOPTER: Self = Self(3);
    pub const PARACHUTE: Self = Self(4);
    pub const POWERED: Self = Self(8);
    pub const UAV: Self = Self(13);
    pub const STATIC: Self = Self(15);

    /// A short English name (SoftRF's table; FLARM v7 adds 16-19).
    pub const fn label(self) -> &'static str {
        match self.0 {
            1 => "Glider",
            2 => "Tow plane",
            3 => "Helicopter",
            4 => "Parachute",
            5 => "Drop plane",
            6 => "Hang glider",
            7 => "Paraglider",
            8 => "Powered aircraft",
            9 => "Jet",
            10 => "Gyrocopter/other",
            11 => "Balloon",
            12 => "Airship",
            13 => "Drone",
            14 => "Ground station",
            15 => "Obstacle",
            16 => "eVTOL",
            17..=19 => "Drone",
            _ => "Aircraft",
        }
    }

    /// The nearest ADS-B emitter category, numbered as
    /// [`adsb::Kind::of`] takes it (-1: none fits).
    pub const fn emitter_category(self) -> i8 {
        match self.0 {
            1 => 9,             // glider
            2 | 5 | 8 => 1,     // tow plane, drop plane, powered: light
            3 => 7,             // rotorcraft
            4 => 11,            // parachutist
            6 | 7 => 12,        // hang glider, paraglider: ultralight
            11 | 12 => 10,      // lighter than air
            13 | 17..=19 => 14, // UAV
            15 => 19,           // point obstacle
            _ => 0,             // unknown, jet, other, ground station, eVTOL
        }
    }

    /// FLARM's ground-speed threshold (FTD-014, as SoftRF's
    /// `legacy_GS_threshold`) below which this type counts as on the ground,
    /// m/s. `None` for ground stations and obstacles, never airborne.
    pub const fn airborne_threshold_mps(self) -> Option<f32> {
        match self.0 {
            14 | 15 => None,
            1 | 3 | 6 | 7 | 13 => Some(2.0),
            4 => Some(5.0),
            2 | 5 | 8 | 9 => Some(10.0),
            _ => Some(0.0),
        }
    }
}

/// One decoded packet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Decoded {
    /// The record the tracker ingests. One packet per record: what the
    /// packet carried is flagged valid and "updated"; `frames_per_s` is 1;
    /// `signal_dbm` is 0 for the caller to fill from the radio. The address
    /// type is in bits 24-27 of `icao` ([`adsb::Aircraft::address_qualifier`]).
    pub aircraft: adsb::Aircraft,
    pub address_type: AddressType,
    pub aircraft_type: AircraftType,
    /// The sender asks not to be shown on public web pages (OGN "stealth").
    pub stealth: bool,
    /// The sender asks not to be tracked (FLARM "no-track"). OGN honours
    /// this on its public network; the Rotorscope hub logs, shows and
    /// relays such aircraft like any other (owner decision 2026-11-17).
    pub no_track: bool,
    /// OGN: times relayed by other trackers (0 = heard directly).
    pub relay_count: u8,
}

/// Fill the movement fields common to OGN and FLARM.
#[derive(Default)]
pub(crate) struct Motion {
    pub lat_lon: Option<(f64, f64)>,
    /// GNSS altitude, metres.
    pub gnss_alt_m: Option<f64>,
    /// Pressure altitude, metres.
    pub baro_alt_m: Option<f64>,
    pub track_deg: Option<f32>,
    pub speed_mps: Option<f64>,
    pub climb_mps: Option<f64>,
    pub airborne: bool,
}

pub(crate) fn record(
    source: adsb::Source,
    address: u32,
    address_type: AddressType,
    aircraft_type: AircraftType,
    emergency: u8,
    m: &Motion,
) -> adsb::Aircraft {
    use adsb::flag::*;
    let mut flags = 0u32;
    let mut set = |bits: &[u32]| {
        for b in bits {
            flags |= 1 << b;
        }
    };
    let mut a = adsb::Aircraft {
        source,
        icao: (address & 0x00ff_ffff) | (u32::from(address_type.bits()) << 24),
        flags: 0,
        callsign: adsb::Callsign::default(),
        squawk: None,
        category: aircraft_type.emitter_category(),
        lat_deg: 0.0,
        lon_deg: 0.0,
        baro_alt_ft: 0,
        gnss_alt_ft: 0,
        direction_deg: 0.0,
        speed_kt: 0,
        baro_vrate_fpm: 0,
        gnss_vrate_fpm: 0,
        uat_emergency: emergency,
        nicnac: 0,
        acdims: 0,
        version: 0,
        signal_dbm: 0,
        signal_quality: 0,
        frames_per_s: 1,
        es_frames_per_s: 0,
    };
    if m.airborne {
        set(&[AIRBORNE]);
    }
    if let Some((lat, lon)) = m.lat_lon {
        a.lat_deg = lat;
        a.lon_deg = lon;
        set(&[POSITION_VALID, UPDATED_POSITION]);
    }
    if let Some(h) = m.gnss_alt_m {
        a.gnss_alt_ft = libm::round(h * FT_PER_M) as i32;
        set(&[GNSS_ALT_VALID, UPDATED_GNSS_ALT]);
    }
    if let Some(h) = m.baro_alt_m {
        a.baro_alt_ft = libm::round(h * FT_PER_M) as i32;
        set(&[BARO_ALT_VALID, UPDATED_BARO_ALT]);
    }
    if let Some(t) = m.track_deg {
        a.direction_deg = t;
        set(&[DIRECTION_VALID, UPDATED_DIRECTION]);
    }
    if let Some(s) = m.speed_mps {
        a.speed_kt = libm::round(s * KT_PER_MPS) as i32;
        set(&[SPEED_VALID, UPDATED_SPEED]);
    }
    if let Some(c) = m.climb_mps {
        a.gnss_vrate_fpm = libm::round(c * FPM_PER_MPS) as i32;
        set(&[GNSS_VRATE_VALID, UPDATED_GNSS_VRATE]);
    }
    a.flags = flags;
    a
}

/// Bearing 0..<360 of a north / east vector.
#[cfg(feature = "legacy")]
pub(crate) fn bearing_deg(north: f64, east: f64) -> f32 {
    let d = libm::atan2(east, north).to_degrees();
    (if d < 0.0 { d + 360.0 } else { d }) as f32
}
