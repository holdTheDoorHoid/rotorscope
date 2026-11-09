//! Every pin the hub uses, in one place.
//!
//! PLACEHOLDERS: these are dev-board choices until the carrier PCB exists
//! (PLAN.md Phase 5). Change them here and nowhere else. The README's pin
//! table is copied from this file.
//!
//! Rules followed on the S3, so the choices survive the move to a PSRAM hub
//! (N16R8-class module):
//! - not GPIO26-37 (SPI flash and octal PSRAM on N16R8 modules);
//! - not GPIO0, 3, 45, 46 (strapping pins);
//! - not GPIO19/20 (USB: the console is USB-Serial-JTAG);
//! - not GPIO43/44 (UART0's boot pins: the ROM bootloader prints there at reset,
//!   which would be sent to whatever node is wired up);
//! - battery sense on ADC1 (GPIO1-10): ADC2 cannot be used while Wi-Fi runs.
//!
//! The node links use the UART *peripherals* UART0/1/2 routed through the GPIO
//! matrix to the pins below; the console is USB-Serial-JTAG, so UART0 is free.
//! Only the RX side is used today (nodes only talk to the hub); the TX pins are
//! reserved so a later hub-to-node command channel needs no rewiring.

use esp_hal::gpio::AnyPin;

/// Pins handed out by [`take!`]. Each is a placeholder (see the module docs).
pub struct Pins {
    /// Node link RX pins, one per UART, index = link number.
    pub node_rx: [AnyPin<'static>; NODE_LINKS],
    /// Reserved: hub -> node TX (unused today).
    #[allow(dead_code)]
    pub node_tx: [AnyPin<'static>; NODE_LINKS],
    /// GPS (u-blox MAX-M10S) on I2C.
    pub gps_sda: AnyPin<'static>,
    pub gps_scl: AnyPin<'static>,
    /// GPS time pulse, into every chip (P3).
    pub pps: AnyPin<'static>,
    /// One SPI bus shared by the screen and the SD card, separate chip selects.
    pub spi_sck: AnyPin<'static>,
    pub spi_mosi: AnyPin<'static>,
    pub spi_miso: AnyPin<'static>,
    pub screen_cs: AnyPin<'static>,
    pub screen_dc: AnyPin<'static>,
    pub screen_backlight: AnyPin<'static>,
    pub sd_cs: AnyPin<'static>,
    /// Buttons, active low with internal pull-ups.
    pub buttons: [AnyPin<'static>; 3],
    pub buzzer: AnyPin<'static>,
    /// Battery divider into ADC1. Kept as its concrete type: the ADC driver
    /// needs to know the channel at compile time.
    pub battery: BatteryPin,
}

// ---------------------------------------------------------------- ESP32-S3 --
#[cfg(feature = "esp32s3")]
mod imp {
    /// The S3 has three UARTs, so all three radio nodes get one.
    pub const NODE_LINKS: usize = 3;
    pub type BatteryPin = esp_hal::peripherals::GPIO1<'static>;

    /// (name, GPIO) for the boot banner and the README. Keep in step with `take!`.
    pub const TABLE: &[(&str, u8)] = &[
        ("node0_rx", 4), ("node0_tx", 5),     // UART0: Wi-Fi 2.4 GHz node (A)
        ("node1_rx", 6), ("node1_tx", 7),     // UART1: Bluetooth node (B)
        ("node2_rx", 15), ("node2_tx", 16),   // UART2: Wi-Fi 5 GHz node (C)
        ("gps_sda", 8), ("gps_scl", 9), ("pps", 2),
        ("spi_sck", 12), ("spi_mosi", 11), ("spi_miso", 13),
        ("screen_cs", 10), ("screen_dc", 17), ("screen_bl", 18), ("sd_cs", 14),
        ("btn0", 38), ("btn1", 39), ("btn2", 40),
        ("buzzer", 21), ("battery_adc", 1),
    ];

    macro_rules! take {
        ($p:ident) => {
            $crate::pins::Pins {
                node_rx: [$p.GPIO4.degrade(), $p.GPIO6.degrade(), $p.GPIO15.degrade()],
                node_tx: [$p.GPIO5.degrade(), $p.GPIO7.degrade(), $p.GPIO16.degrade()],
                gps_sda: $p.GPIO8.degrade(),
                gps_scl: $p.GPIO9.degrade(),
                pps: $p.GPIO2.degrade(),
                spi_sck: $p.GPIO12.degrade(),
                spi_mosi: $p.GPIO11.degrade(),
                spi_miso: $p.GPIO13.degrade(),
                screen_cs: $p.GPIO10.degrade(),
                screen_dc: $p.GPIO17.degrade(),
                screen_backlight: $p.GPIO18.degrade(),
                sd_cs: $p.GPIO14.degrade(),
                buttons: [$p.GPIO38.degrade(), $p.GPIO39.degrade(), $p.GPIO40.degrade()],
                buzzer: $p.GPIO21.degrade(),
                battery: $p.GPIO1,
            }
        };
    }
    pub(crate) use take;
}

// ---------------------------------------------------------------- ESP32-C5 --
#[cfg(feature = "esp32c5")]
mod imp {
    /// The C5 has only two HP UARTs (UART0/1), so a C5 hub takes two nodes.
    /// (LP_UART exists but is a different driver; not worth it for a fallback.)
    pub const NODE_LINKS: usize = 2;
    pub type BatteryPin = esp_hal::peripherals::GPIO1<'static>;

    /// The C5 is short of free pins once flash (GPIO16-22) and USB (13/14) are
    /// excluded, so the buttons and buzzer sit on strapping pins (25-28): do not
    /// hold a button during reset. Placeholder only; the S3 is the real hub.
    pub const TABLE: &[(&str, u8)] = &[
        ("node0_rx", 3), ("node0_tx", 4),
        ("node1_rx", 5), ("node1_tx", 6),
        ("gps_sda", 23), ("gps_scl", 24), ("pps", 0),
        ("spi_sck", 9), ("spi_mosi", 10), ("spi_miso", 8),
        ("screen_cs", 11), ("screen_dc", 12), ("screen_bl", 2), ("sd_cs", 15),
        ("btn0", 25), ("btn1", 26), ("btn2", 27),
        ("buzzer", 28), ("battery_adc", 1),
    ];

    macro_rules! take {
        ($p:ident) => {
            $crate::pins::Pins {
                node_rx: [$p.GPIO3.degrade(), $p.GPIO5.degrade()],
                node_tx: [$p.GPIO4.degrade(), $p.GPIO6.degrade()],
                gps_sda: $p.GPIO23.degrade(),
                gps_scl: $p.GPIO24.degrade(),
                pps: $p.GPIO0.degrade(),
                spi_sck: $p.GPIO9.degrade(),
                spi_mosi: $p.GPIO10.degrade(),
                spi_miso: $p.GPIO8.degrade(),
                screen_cs: $p.GPIO11.degrade(),
                screen_dc: $p.GPIO12.degrade(),
                screen_backlight: $p.GPIO2.degrade(),
                sd_cs: $p.GPIO15.degrade(),
                buttons: [$p.GPIO25.degrade(), $p.GPIO26.degrade(), $p.GPIO27.degrade()],
                buzzer: $p.GPIO28.degrade(),
                battery: $p.GPIO1,
            }
        };
    }
    pub(crate) use take;
}

pub use imp::*;

/// Node link speed; must match the nodes (`HUB_BAUD` / `hub::BAUD`).
pub const NODE_BAUD: u32 = 921_600;
