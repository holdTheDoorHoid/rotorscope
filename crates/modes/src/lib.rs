//! Mode S / ADS-B decoder for 1090 MHz frames (PLAN.md Phase 14a).
//! `no_std`, no allocator; dependencies `adsb` (for the output record) and
//! `libm`.
//!
//! The ADSBee's CSBee output is its own summary of what it heard. This
//! crate takes the raw frames instead (Beast or RAW output, [`input`]) and
//! does everything above the radio itself, so the hub's aircraft picture no
//! longer rests on someone else's decoder and every step can be checked:
//!
//! - [`crc`]: CRC-24 parity; [`frame`]: the frame, its downlink format, and
//!   conservative 1- and 2-bit repair of DF17/18;
//! - [`decode`]: per format: DF0/4/5/16/20/21 (address/parity), DF11
//!   all-call, DF17/18 extended squitter including TIS-B and ADS-R (by the
//!   DF18 CF field), DF19 military;
//! - [`alt`]: altitude (25 ft Q-bit and Gillham/Mode C) and squawk codes;
//! - [`es`]: extended squitter type codes: identification and category,
//!   airborne position (barometric and GNSS height), surface position,
//!   velocity subtypes 1-4, emergency status, operational status;
//! - [`cpr`]: CPR global (even/odd) and local decoding with the NL table;
//! - [`commb`]: Comm-B register inference for BDS 1,0 / 2,0 / 4,0 / 5,0 /
//!   6,0 with plausibility checks;
//! - [`table`]: per-aircraft state with the trust rules (which messages may
//!   create an aircraft, which only update one), position plausibility, and
//!   a once-a-second report in the same [`adsb::Aircraft`] record the CSBee
//!   parser gives.
//!
//! **Sources** for the formats: ICAO Annex 10 Vol IV (Mode S), ICAO Doc
//! 9871 (Technical provisions for Mode S services and extended squitter,
//! 2nd ed.), RTCA DO-260B as summarised in Junzi Sun, *The 1090 Megahertz
//! Riddle* (2nd ed., TU Delft OPEN, 2021, <https://mode-s.org/1090mhz/>).
//! Example frames in the tests are the ones that book and the pyModeS test
//! suite publish (cited where used). No code is copied from either.
//!
//! **GNSS height (TC 20-22).** Read with the same 12-bit, 25 ft layout as
//! the barometric altitude, as dump1090-fa and readsb do. pyModeS reads it
//! as metres instead. These type codes are rare (none in our recordings);
//! flagged here in case a real one disagrees.
#![no_std]

pub mod alt;
pub mod commb;
pub mod cpr;
pub mod crc;
pub mod decode;
pub mod es;
pub mod frame;
pub mod input;
pub mod table;

pub use decode::{AddressCheck, Body, EsSource, Message, Reject, decode};
pub use frame::{Fix, Frame};
pub use input::{BeastReader, Received};
pub use table::{Config, Table};

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_table;
