//! Host side of the TI CC13x2/CC26x2 ROM serial bootloader, aimed at the
//! Sonoff Zigbee 3.0 USB Dongle Plus (CC2652P behind a CP2102N). Written
//! from TI's Technical Reference Manual (SWCU185, "Bootloader") with
//! cc2538-bsl read for protocol facts; no code is shared with either.

pub mod ccfg;
pub mod crc;
pub mod hex;
pub mod ops;
pub mod proto;
pub mod serial;
pub mod sim;
