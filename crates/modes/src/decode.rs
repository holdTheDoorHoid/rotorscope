//! One frame to one message: downlink format, address, parity check and the
//! fields of each format.

use crate::alt::{self, Altitude};
use crate::es::{self, Es};
use crate::frame::{Fix, Frame, correct};

/// How the address of a message was obtained, which decides how far it
/// can be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressCheck {
    /// DF17/18 (and DF11 answering no interrogator): the address is in the
    /// clear and the CRC checked out, possibly after a repair.
    Verified(Fix),
    /// DF11 replying to an interrogator: the CRC is overlaid with its code
    /// (`ii_si`, 0-79). The address is in the clear and the overlay is small,
    /// so this still checks most of the frame.
    Interrogator(u8),
    /// DF0/4/5/16/20/21: the parity is the CRC XOR the address, so the
    /// "address" is whatever the CRC leaves. Any corrupt frame gives *some*
    /// address: only believe it for an aircraft already known from a
    /// verified message (the table does this).
    AddressParity,
}

/// Who sent a DF17/18 and how: the receiver should know whether an aircraft
/// is broadcasting for itself or being relayed by a ground station.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EsSource {
    /// DF17: a transponder-based ADS-B transmitter.
    AdsB,
    /// DF18 CF=0/1: a non-transponder ADS-B device (ground vehicles,
    /// obstacles, some light aircraft). `icao` false: anonymous or
    /// self-assigned address.
    NonTransponder { icao: bool },
    /// DF18 CF=2/3/5: TIS-B, a ground station relaying a radar target.
    /// `fine` false for CF=3 coarse format (its position layout differs
    /// and is not decoded).
    TisB { icao: bool, fine: bool },
    /// DF18 CF=6: ADS-R, a ground station rebroadcasting a UAT aircraft.
    AdsR { icao: bool },
    /// DF18 CF=4 (TIS-B/ADS-R management) or 7 (reserved).
    Other(u8),
}

impl EsSource {
    /// A ground station relays it (TIS-B, ADS-R): not the aircraft's own
    /// broadcast.
    pub fn relayed(self) -> bool {
        matches!(self, EsSource::TisB { .. } | EsSource::AdsR { .. })
    }
}

/// A decoded message.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Message {
    pub df: u8,
    /// 24-bit address (see [`Message::check`] for how far to trust it).
    pub address: u32,
    pub check: AddressCheck,
    pub body: Body,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Body {
    /// DF0 short air-air surveillance (ACAS). `airborne`: VS bit clear.
    ShortAcas { airborne: bool, altitude: Altitude },
    /// DF4 altitude reply. `fs`: flight status (0/2 airborne, 1/3 on the
    /// ground, 2-5 alert, 4-5 SPI).
    AltitudeReply { fs: u8, altitude: Altitude },
    /// DF5 identity reply.
    IdentityReply { fs: u8, squawk: u16 },
    /// DF11 all-call reply. `ca`: transponder capability (4 on ground,
    /// 5 airborne, 6/7 either).
    AllCall { ca: u8 },
    /// DF16 long air-air surveillance.
    LongAcas { airborne: bool, altitude: Altitude, mv: [u8; 7] },
    /// DF17/18 extended squitter.
    Extended { source: EsSource, ca: u8, es: Es },
    /// DF20 Comm-B with altitude.
    CommBAltitude { fs: u8, altitude: Altitude, mb: [u8; 7] },
    /// DF21 Comm-B with identity.
    CommBIdentity { fs: u8, squawk: u16, mb: [u8; 7] },
    /// DF19 military extended squitter (content not decoded).
    Military,
    /// DF24 Comm-D and the unassigned formats.
    Other,
}

/// Why a frame was not decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    /// DF17/18 whose CRC fails and could not be repaired within the rules.
    BadCrc,
    /// DF11 whose CRC leaves more than an interrogator code: corrupt.
    BadAllCall,
    /// A format with no parity scheme this decoder uses (DF19, DF22, DF24),
    /// kept only as a count.
    Unchecked,
}

/// Decode one frame. `max_fix_bits` (0-2) is how many bits DF17/18 error
/// correction may repair; the frame is repaired in place.
pub fn decode(frame: &mut Frame, max_fix_bits: u8) -> Result<Message, Reject> {
    let df = frame.df();
    let address_parity = |f: &Frame| f.syndrome();
    let body_and_check = match df {
        0 => {
            let altitude = alt::ac13(frame.field(19, 13) as u16);
            let airborne = frame.field(5, 1) == 0;
            (Body::ShortAcas { airborne, altitude }, AddressCheck::AddressParity, address_parity(frame))
        }
        4 | 20 => {
            let fs = frame.field(5, 3) as u8;
            let altitude = alt::ac13(frame.field(19, 13) as u16);
            let body = if df == 4 {
                Body::AltitudeReply { fs, altitude }
            } else {
                Body::CommBAltitude { fs, altitude, mb: frame.payload() }
            };
            (body, AddressCheck::AddressParity, address_parity(frame))
        }
        5 | 21 => {
            let fs = frame.field(5, 3) as u8;
            let squawk = alt::squawk(frame.field(19, 13) as u16);
            let body = if df == 5 {
                Body::IdentityReply { fs, squawk }
            } else {
                Body::CommBIdentity { fs, squawk, mb: frame.payload() }
            };
            (body, AddressCheck::AddressParity, address_parity(frame))
        }
        16 => {
            let altitude = alt::ac13(frame.field(19, 13) as u16);
            let airborne = frame.field(5, 1) == 0;
            let body = Body::LongAcas { airborne, altitude, mv: frame.payload() };
            (body, AddressCheck::AddressParity, address_parity(frame))
        }
        11 => {
            let s = frame.syndrome();
            // II codes (1-15) and SI codes (1-63) live in the low 7 bits.
            if s > 0x7F {
                return Err(Reject::BadAllCall);
            }
            let check = if s == 0 {
                AddressCheck::Verified(Fix::Clean)
            } else {
                AddressCheck::Interrogator(s as u8)
            };
            (Body::AllCall { ca: frame.field(5, 3) as u8 }, check, frame.field(8, 24))
        }
        17 | 18 => {
            let fix = correct(frame, max_fix_bits).ok_or(Reject::BadCrc)?;
            let ca = frame.field(5, 3) as u8;
            let source = if df == 17 {
                EsSource::AdsB
            } else {
                match ca {
                    0 => EsSource::NonTransponder { icao: true },
                    1 => EsSource::NonTransponder { icao: false },
                    2 => EsSource::TisB { icao: frame.field(32 + 7, 1) == 0, fine: true },
                    3 => EsSource::TisB { icao: true, fine: false },
                    5 => EsSource::TisB { icao: false, fine: true },
                    // ADS-R and fine TIS-B with the IMF bit: ME bit 8
                    // (where DF17 has NIC-B / single antenna) says whether
                    // the address is an ICAO one (0) or not (1).
                    6 => EsSource::AdsR { icao: frame.field(32 + 7, 1) == 0 },
                    other => EsSource::Other(other),
                }
            };
            let es = match source {
                EsSource::TisB { fine: false, .. } | EsSource::Other(_) => es::Es::Other {
                    tc: frame.field(32, 5) as u8,
                    subtype: frame.field(37, 3) as u8,
                },
                _ => es::decode(&frame.payload()),
            };
            (Body::Extended { source, ca, es }, AddressCheck::Verified(fix), frame.field(8, 24))
        }
        19 => {
            // AF=0 is the DF17 layout with plain parity; the rest is not
            // public. Only the address and the CRC are used.
            if frame.syndrome() != 0 {
                return Err(Reject::BadCrc);
            }
            (Body::Military, AddressCheck::Verified(Fix::Clean), frame.field(8, 24))
        }
        22 | 24 => return Err(Reject::Unchecked),
        _ => (Body::Other, AddressCheck::AddressParity, address_parity(frame)),
    };
    let (body, check, address) = body_and_check;
    Ok(Message { df, address, check, body })
}
