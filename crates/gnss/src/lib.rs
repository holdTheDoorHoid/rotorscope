//! GNSS driver core for the Rotorscope hub (u-blox MAX-M10S, protocol 34.x).
//!
//! `#![no_std]`, no alloc, no unsafe, no dependencies. The crate never touches a bus: the
//! caller reads bytes from UART or I2C (DDC) and calls [`Parser::feed`]; the config
//! builders write UBX frames into a caller buffer for the caller to send.
//!
//! * [`Parser`] turns a byte stream into [`Event`]s: a [`Fix`] (from UBX NAV-PVT or from NMEA
//!   GGA + RMC), UBX ACK/NAK, MON-VER and CFG-VALGET replies.
//! * [`config`] builds CFG-VALSET / CFG-VALGET / MON-VER frames and offers
//!   [`configure_sequence`], the boot-time configuration.
//! * [`ddc`] holds the I2C register constants and helpers.
//!
//! See the crate README for wiring (UART vs I2C, PPS) and for why this is not built on the
//! `ublox` crate.
#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod config;
pub mod ddc;
mod fix;
mod nmea;
mod parser;
mod ubx;

pub use config::{
    ConfigFrame, ConfigOptions, ConfigSequence, configure_sequence, BuildError, Value,
};
pub use fix::{Fix, FixType, Source, UtcTime};
pub use parser::{Event, Events, Parser, Stats, MAX_NMEA_LEN, MAX_UBX_PAYLOAD};
pub use ubx::{build_frame, fletcher, MonVer, ValGet, ValGetItem};

#[cfg(test)]
extern crate std;
#[cfg(test)]
mod tests;
