//! GNSS driver core for the Rotorscope hub: u-blox MAX-M10S (protocol 34.x) and Quectel
//! LC29H(AA) (NMEA + PAIR/PQTM).
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
//! * [`sats`] keeps the satellites of the last epoch one by one (UBX NAV-SAT or NMEA GSV).
//! * [`integrity`] watches for jamming and spoofing of the receiver itself.
//!
//! See the crate README for wiring (UART vs I2C, PPS) and for why this is not built on the
//! `ublox` crate.
#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod config;
pub mod ddc;
mod fix;
pub mod integrity;
pub mod module;
mod nmea;
mod parser;
pub mod quectel;
pub mod sats;
pub mod sky;
pub mod timeguard;
pub mod trust;
mod ubx;

pub use config::{
    ConfigFrame, ConfigOptions, ConfigSequence, configure_sequence, BuildError, Value,
};
pub use fix::{Fix, FixType, Source, UtcTime};
pub use module::{Module, Step, Steps};
pub use quectel::{Lc29hOptions, Lc29hSequence, QuectelCommand, Reply};
pub use sky::{Band, ConstellationView, SkyView, System};
pub use parser::{Event, Events, Parser, Stats, MAX_NMEA_LEN, MAX_UBX_PAYLOAD};
pub use ubx::{build_frame, fletcher, MonRf, MonVer, NavClock, NavStatus, RfBlock, ValGet, ValGetItem};
pub use sats::{Sat, SatTable};
pub use integrity::{HomeEvent, HomeOrigin, Monitor, State as IntegrityState, Status as IntegrityStatus};

#[cfg(test)]
extern crate std;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_lc29h;
#[cfg(test)]
mod tests_integrity;
