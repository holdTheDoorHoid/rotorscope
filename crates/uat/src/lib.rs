//! UAT (Universal Access Transceiver, 978 MHz) ADS-B decoder (PLAN.md
//! Phase 14b). `no_std`, no allocator; dependencies `adsb` (the output
//! record) and `libm`.
//!
//! Input: the raw bytes of one downlink frame as the radio received them
//! after the 36-bit synchronisation word, Reed-Solomon parity included.
//! A receiver cannot tell a basic frame from a long one before decoding, so
//! it should capture [`LONG_BYTES`] (48) bytes every time; [`decode`] tries
//! long first and then basic, exactly as dump978's `correct_adsb_frame`.
//!
//! - [`rs`]: Reed-Solomon over GF(256): RS(30,18) for basic frames (up to 6
//!   bad bytes corrected) and RS(48,34) for long frames (up to 7);
//! - [`payload`]: header (address, address qualifier), state vector
//!   (latitude/longitude, barometric or geometric altitude, north/east
//!   velocity or ground speed and track, vertical rate), mode status
//!   (call sign or squawk, emitter category, emergency) and the auxiliary
//!   state vector's second altitude;
//! - [`Decoded::aircraft`]: the same [`adsb::Aircraft`] record the ADSBee's
//!   CSBee lines and the `modes` decoder give, with [`adsb::Source::Uat`].
//!
//! Ground uplink frames (FIS-B weather, 432 bytes) are out of scope.
//!
//! **Credit and licence.** The field layout and the frame-type logic
//! follow dump978 by Oliver Jowett (`fec.c`, `uat_decode.c`;
//! GPL-2.0-or-later, as carried in SoftRF's `libraries/dump978`), ported to
//! Rust under Rotorscope's GPL-3.0-or-later. The Reed-Solomon decoder is
//! written fresh from the textbook algorithm with dump978's code parameters
//! (it does not port Phil Karn's LGPL `fec` library).
#![no_std]

pub mod payload;
pub mod rs;

#[cfg(test)]
pub(crate) mod tests;
#[cfg(test)]
mod fuzz_tests;

pub use payload::{AirGround, AltKind, ModeStatus, Payload, StateVector, TrackKind};

/// Bytes in a basic ADS-B frame (18 data + 12 parity).
pub const BASIC_BYTES: usize = 30;
/// Data bytes in a basic frame.
pub const BASIC_DATA_BYTES: usize = 18;
/// Bytes in a long ADS-B frame (34 data + 14 parity).
pub const LONG_BYTES: usize = 48;
/// Data bytes in a long frame.
pub const LONG_DATA_BYTES: usize = 34;

/// Why a frame was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Fewer than [`BASIC_BYTES`] bytes.
    TooShort,
    /// Neither a long nor a basic frame after error correction: too many
    /// bad bytes, or not UAT ADS-B at all.
    Uncorrectable,
}

/// Basic or long ADS-B frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    Basic,
    Long,
}

impl FrameKind {
    /// Bytes on air, parity included.
    pub const fn bytes(self) -> usize {
        match self {
            FrameKind::Basic => BASIC_BYTES,
            FrameKind::Long => LONG_BYTES,
        }
    }

    /// Data bytes.
    pub const fn data_bytes(self) -> usize {
        match self {
            FrameKind::Basic => BASIC_DATA_BYTES,
            FrameKind::Long => LONG_DATA_BYTES,
        }
    }
}

/// Error-correct an ADS-B frame in place, as dump978's
/// `correct_adsb_frame`: with at least 48 bytes, try it as a long frame
/// (valid only if the payload type is not 0); then the first 30 bytes as a
/// basic frame (valid only if the payload type is 0). Returns the kind and
/// the number of bytes corrected. On error `frame` is unchanged.
pub fn correct_adsb_frame(frame: &mut [u8]) -> Result<(FrameKind, u8), Error> {
    if frame.len() < BASIC_BYTES {
        return Err(Error::TooShort);
    }
    if frame.len() >= LONG_BYTES {
        let mut w = [0u8; LONG_BYTES];
        w.copy_from_slice(&frame[..LONG_BYTES]);
        if let Some(n) = rs::LONG.correct(&mut w)
            && w[0] >> 3 != 0
        {
            frame[..LONG_BYTES].copy_from_slice(&w);
            return Ok((FrameKind::Long, n as u8));
        }
    }
    let mut w = [0u8; BASIC_BYTES];
    w.copy_from_slice(&frame[..BASIC_BYTES]);
    if let Some(n) = rs::BASIC.correct(&mut w)
        && w[0] >> 3 == 0
    {
        frame[..BASIC_BYTES].copy_from_slice(&w);
        return Ok((FrameKind::Basic, n as u8));
    }
    Err(Error::Uncorrectable)
}

/// One decoded ADS-B frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Decoded {
    pub kind: FrameKind,
    /// Bytes the Reed-Solomon decoder corrected.
    pub corrected: u8,
    pub payload: Payload,
}

/// Correct (in place) and decode one frame: the call the hub makes for
/// every UAT frame the radio node sends.
pub fn decode(frame: &mut [u8]) -> Result<Decoded, Error> {
    let (kind, corrected) = correct_adsb_frame(frame)?;
    let payload = payload::decode(&frame[..kind.data_bytes()]).ok_or(Error::Uncorrectable)?;
    Ok(Decoded {
        kind,
        corrected,
        payload,
    })
}

impl Decoded {
    /// This frame as the aircraft record the tracker ingests.
    ///
    /// One frame per record: only what this frame carried is flagged valid
    /// (and "updated"), so the tracker keeps earlier values for the rest
    /// (a basic frame has no call sign; the tracker keeps the last one).
    /// `frames_per_s` is 1 so the record counts as hearing the aircraft;
    /// `signal_quality` is the number of bytes corrected; `signal_dbm` is
    /// 0, for the caller to fill from the radio.
    pub fn aircraft(&self) -> adsb::Aircraft {
        use adsb::flag::*;
        let p = &self.payload;
        let mut flags = 0u32;
        let mut set = |bit: u32| flags |= 1 << bit;
        let mut a = adsb::Aircraft {
            source: adsb::Source::Uat,
            icao: (p.address & 0x00ff_ffff) | (u32::from(p.address_qualifier & 0x0f) << 24),
            flags: 0,
            callsign: adsb::Callsign::default(),
            squawk: None,
            category: -1,
            lat_deg: 0.0,
            lon_deg: 0.0,
            baro_alt_ft: 0,
            gnss_alt_ft: 0,
            direction_deg: 0.0,
            speed_kt: 0,
            baro_vrate_fpm: 0,
            gnss_vrate_fpm: 0,
            uat_emergency: 0,
            nicnac: 0,
            acdims: 0,
            version: 0,
            signal_dbm: 0,
            signal_quality: i16::from(self.corrected),
            frames_per_s: 1,
            es_frames_per_s: 0,
        };
        let put_alt =
            |a: &mut adsb::Aircraft, alt: Option<(i32, AltKind)>, set: &mut dyn FnMut(u32)| {
                match alt {
                    Some((ft, AltKind::Baro)) => {
                        a.baro_alt_ft = ft;
                        set(BARO_ALT_VALID);
                        set(UPDATED_BARO_ALT);
                    }
                    Some((ft, AltKind::Geo)) => {
                        a.gnss_alt_ft = ft;
                        set(GNSS_ALT_VALID);
                        set(UPDATED_GNSS_ALT);
                    }
                    None => {}
                }
            };
        if let Some(sv) = &p.sv {
            if matches!(sv.air_ground, AirGround::Subsonic | AirGround::Supersonic) {
                set(AIRBORNE);
            }
            if let Some((lat, lon)) = sv.position {
                a.lat_deg = lat;
                a.lon_deg = lon;
                set(POSITION_VALID);
                set(UPDATED_POSITION);
            }
            put_alt(&mut a, sv.altitude, &mut set);
            if let Some((d, _)) = sv.track {
                a.direction_deg = d;
                set(DIRECTION_VALID);
                set(UPDATED_DIRECTION);
            }
            if let Some(s) = sv.speed_kt {
                a.speed_kt = s;
                set(SPEED_VALID);
                set(UPDATED_SPEED);
            }
            match sv.vrate {
                Some((v, AltKind::Baro)) => {
                    a.baro_vrate_fpm = v;
                    set(BARO_VRATE_VALID);
                    set(UPDATED_BARO_VRATE);
                }
                Some((v, AltKind::Geo)) => {
                    a.gnss_vrate_fpm = v;
                    set(GNSS_VRATE_VALID);
                    set(UPDATED_GNSS_VRATE);
                }
                None => {}
            }
        }
        put_alt(&mut a, p.aux_altitude, &mut set);
        // NICNAC as the ADSBee packs it (`adsb::nicnac`): NIC from the state
        // vector, the rest from the mode status. A field this frame did not
        // carry is 0, as the ADSBee prints values it has not received; a
        // basic frame (no mode status) therefore says NACp 0, which a
        // reader must treat as "not in this record".
        let nic = p.sv.map_or(0, |sv| sv.nic);
        a.nicnac = match &p.ms {
            Some(ms) => adsb::nicnac::pack(ms.sil, 0, ms.nac_p, ms.nac_v, ms.nic_baro != 0, nic),
            None => adsb::nicnac::pack(0, 0, 0, 0, false, nic),
        };
        if let Some(ms) = &p.ms {
            a.callsign = ms.callsign;
            a.squawk = ms.squawk;
            a.category = ms.emitter_category as i8;
            a.uat_emergency = ms.emergency;
            a.version = ms.uat_version;
        }
        a.flags = flags;
        a
    }
}
