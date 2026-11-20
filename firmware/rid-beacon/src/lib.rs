//! rid-beacon: a bench Remote ID test transmitter. See README.md for conduct
//! rules, build commands and the console protocol.
#![no_std]

pub mod control;
pub mod flight;
pub mod hci_fix;
pub mod rid_tx;
/// Scripted flights for the threat-level bench: bench builds only
/// (`ROTORSCOPE_BENCH=1`, feature `scripted`; build.rs).
#[cfg(feature = "scripted")]
pub mod script;
