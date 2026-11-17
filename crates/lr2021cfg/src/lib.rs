//! The Semtech LR2021 as a **receive-only** radio, described as data.
//!
//! Everything a firmware needs to drive an LR2021 is here as plain bytes
//! and pure functions, so it can be host-tested without the chip:
//!
//! - [`cmd`]: the SPI command encoders and response parsers this node uses
//!   (opcodes and field layouts ported from the MIT `lr2021` crate by
//!   TheClams, <https://github.com/TheClams/lr2021>, cross-checked against
//!   SoftRF's use of RadioLib). There is deliberately **no transmit command**
//!   in this module (no SetTx, no PA or TX power, no TX FIFO write): the
//!   node cannot be made to transmit through it.
//! - [`config`]: a generic [`config::RxConfig`] (frequency, band path,
//!   modem: OOK, FSK, LoRa, or an RSSI-only dwell) and the command
//!   sequences that apply it, retune it and start reception; a
//!   [`config::Board`] for what differs between modules (TCXO, IRQ DIO, RF
//!   switch DIOs).
//! - [`roles`]: the aero roles as `RxConfig`s (1090 MHz extended squitter,
//!   978 MHz UAT, FLARM, OGN), with SoftRF's settings.
//! - [`manchester`] and [`accept`]: the raw-chip post-processing (SoftRF's
//!   Manchester table and DF17 trick) and the checks that keep junk off the
//!   link.
//! - [`node`]: the node's pure logic: plan cursor, hop target, the 1090
//!   receive watchdog, the frame rate limit.
//!
//! Why not depend on the `lr2021` crate itself: version 0.14 hard-enables
//! `embassy-time`'s `defmt` and `tick-hz-32_768` features, while every
//! Rotorscope node runs esp-rtos, which fixes the tick at 1 MHz; the two
//! cannot link in one image. Its command layer is generated from the
//! datasheet and is the best public reference for the opcodes, so the few
//! commands needed are ported here (MIT, credited above).
#![cfg_attr(not(test), no_std)]

pub mod accept;
pub mod cmd;
pub mod config;
pub mod manchester;
pub mod node;
pub mod roles;
