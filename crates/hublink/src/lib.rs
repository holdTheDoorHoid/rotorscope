//! The P4 hub's side of the keyed ESP-NOW link (docs/LINK-KEYS-DESIGN.md,
//! package 13e), as host-testable logic. The firmware
//! (`firmware/hub-p4/src/linkauth.rs`, `links.rs`, `flashrom.rs`) only moves
//! bytes: flash words in, UART bytes in and out.
//!
//! - [`flash`]: the flash trait, and the partition window every write goes
//!   through;
//! - [`parttable`]: finding `rskit` and `rsboot` in the partition table;
//! - [`kit`]: reading the kit records, deriving each roster slot's keys once
//!   through a `kitkeys::KeySource`, appending this boot's counter value;
//! - [`link`]: the gateway wire (protocol 14): verify, seal, beacon, roster,
//!   commands, syncs and time syncs, refusals, counts for the screen;
//! - [`continuity`]: the boot counter as the evidence log's.
#![cfg_attr(not(test), no_std)]

pub mod continuity;
pub mod flash;
pub mod kit;
pub mod link;
pub mod parttable;
