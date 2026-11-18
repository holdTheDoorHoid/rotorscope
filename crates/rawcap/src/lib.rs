//! The pure-logic half of Rotorscope's ESP32-C5 raw-radio capture layer
//! (PLAN.md Phase 10b). Everything here is `no_std`, allocation-free and
//! tested on the host; `firmware/node-sdr` supplies the registers.
//!
//! The C5 gives two raw sample paths, both driven through the closed PHY
//! library and the modem's undocumented "dump engine":
//!
//! - **Path B, continuous** ([`iq`], [`ring`], [`trigger`], [`dsp`]): the
//!   modem's debug bus carries every ADC sample (10-bit I and Q). Eight of
//!   its bits are routed through the GPIO matrix to eight pads and straight
//!   back into PARL_IO RX, sampled at 40 MHz into a circular DMA ring: one
//!   byte per complex sample, a 4-bit window of I in the high nibble and of
//!   Q in the low nibble.
//! - **Path A, snapshot** ([`word`]): the dump engine writes 16,380 words of
//!   full 10-bit I/Q (plus the gain index) into a fixed SRAM block at 20 MS/s
//!   (822 µs, a whole DroneID burst), during which the CPU loses that block.
//!
//! [`dump`] streams either capture over USB as checksummed text lines, and
//! decodes them again on the laptop (`tools/rawcap-pull`), so real C5
//! captures can go through `tools/droneid-file`. [`cmd`] parses the USB
//! console.
//!
//! Hardware facts and their sources are cited where they are used; the
//! register-level ones live in `firmware/node-sdr`.
#![cfg_attr(not(any(test, feature = "std")), no_std)]

pub mod cmd;
pub mod budget;
pub mod crc32;
pub mod dsp;
pub mod dump;
pub mod iq;
pub mod ring;
pub mod trigger;
pub mod word;
