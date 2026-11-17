//! **Every pin and every module-specific LR2021 setting is in this file.**
//!
//! No LR2021 hardware has been in hand while writing this (PLAN.md Phase 9):
//! the pins below are a proposal for a bare ESP32-C3 board (SuperMini /
//! DevKitM-1 style) wired by hand to an LR2021 module (Waveshare Core2021,
//! Elecrow LR2021, or an ExpressLRS-style LR2021 receiver's radio). An
//! ExpressLRS-style ESP32-C3 + LR2021 receiver has its own fixed wiring:
//! read it from that receiver's ExpressLRS hardware target file and change
//! the numbers here (and only here) to match.
//!
//! ESP32-C3 pins avoided on purpose: GPIO2, GPIO8, GPIO9 (strapping pins:
//! a module pulling them the wrong way at reset stops the board booting),
//! GPIO18/19 (USB, the console), GPIO11-17 (the flash).
//!
//! | Signal          | ESP32-C3 | LR2021 / hub            |
//! |-----------------|----------|-------------------------|
//! | SPI SCK         | GPIO4    | SCK                     |
//! | SPI MISO        | GPIO5    | MISO                    |
//! | SPI MOSI        | GPIO6    | MOSI                    |
//! | SPI NSS (CS)    | GPIO7    | NSS                     |
//! | BUSY            | GPIO3    | BUSY                    |
//! | IRQ             | GPIO10   | DIO9 (see [`LR2021`])   |
//! | RESET           | GPIO1    | NRESET                  |
//! | hub TX (wired)  | GPIO21   | hub RX                  |
//! | hub RX          | GPIO20   | hub TX                  |
//! | bus TX (wirebus)| GPIO0    | shared return wire, open-drain |
//! | 3V3, GND        |          | VCC, GND                |
//!
//! The wired-bus TX is GPIO0, not GPIO21: GPIO21 is UART0's TX, where the
//! ROM prints its boot banner push-pull at every reset, which would garble
//! the shared wire (the same reason the C5 nodes use D9, see
//! firmware/node-common/src/bus.rs).

use lr2021cfg::config::{Board, RfSwitchDio};

/// SPI clock. The LR2021 accepts more; 8 MHz is gentle on hand wiring.
pub const SPI_MHZ: u32 = 8;

/// RF switch levels for an ExpressLRS-style LR2021 module, from SoftRF's
/// `rfswitch_table_ELRS` (DIO5-8): sub-GHz RX = DIO8 high, 2.4 GHz RX =
/// DIO6 high, everything low in standby. Only the receive columns are used.
/// A module with a different switch (or none) needs its own table here.
pub const RF_SWITCH: [RfSwitchDio; 4] = [
    RfSwitchDio { dio: 5, rx_lf: false, rx_hf: false, standby: false },
    RfSwitchDio { dio: 6, rx_lf: false, rx_hf: true, standby: false },
    RfSwitchDio { dio: 7, rx_lf: false, rx_hf: false, standby: false },
    RfSwitchDio { dio: 8, rx_lf: true, rx_hf: false, standby: false },
];

/// The module: SoftRF's "Nano MK2" (ExpressLRS-style) settings: IRQ on
/// DIO9, a TCXO powered by the chip at 3.3 V (index 7), ~5 ms TCXO start
/// (164 ticks of 30.5 us), RX boost at its maximum.
/// UNVERIFIED: check the module's schematic for TCXO voltage and IRQ DIO.
pub const LR2021: Board = Board { tcxo: Some((7, 164)), irq_dio: 9, rf_switch: &RF_SWITCH, rx_boost: 7 };

/// UART speed to the hub (wired, point to point); the bus has its own.
#[cfg_attr(any(feature = "espnow", feature = "wirebus"), allow(dead_code))]
pub const HUB_BAUD: u32 = 921_600;

/// Display name of the chip in the hello line.
pub const CHIP: &str = "esp32c3";
