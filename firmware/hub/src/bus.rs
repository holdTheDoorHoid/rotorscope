//! The SPI bus the screen and the SD card share (separate chip selects).
//!
//! Both devices are driven by tasks on the *same* executor (the S3's second
//! core; see `periph.rs`), with blocking SPI. Every transfer completes before
//! its task can yield, so two transfers can never interleave, and a plain
//! `RefCell` around the bus is enough: `embedded-hal-bus`'s `RefCellDevice`
//! gives each device its own `SpiDevice` with its own chip select.
//!
//! The one thing the shared-bus types do not do is per-device clock speed,
//! and the two devices need different ones: an SD card must be woken at
//! <= 400 kHz and then runs at up to 25 MHz in SPI mode, while the panel takes
//! 40 MHz. Each user calls [`Bus::set`] before a burst of transfers, which
//! reconfigures the peripheral only when the speed actually changes.

use core::cell::{Cell, RefCell};

use esp_hal::Blocking;
use esp_hal::spi::Mode;
use esp_hal::spi::master::{Config, Spi};
use esp_hal::time::Rate;

/// Clock speeds the bus runs at.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Speed {
    /// SD card identification mode (the spec's ceiling is 400 kHz).
    SdInit,
    /// SD card data transfer. 20 MHz leaves margin under the 25 MHz SPI-mode
    /// limit for long breakout wires.
    Sd,
    /// The panel. ST7789 and ILI9341 both accept 40 MHz writes (the ILI9341
    /// datasheet's write cycle is 66 ns, about 15 MHz, but every common board
    /// runs at 40 MHz; drop this if a particular panel shows noise).
    Panel,
}

impl Speed {
    fn rate(self) -> Rate {
        match self {
            Speed::SdInit => Rate::from_khz(400),
            Speed::Sd => Rate::from_mhz(20),
            Speed::Panel => Rate::from_mhz(40),
        }
    }
}

pub type SpiBus = Spi<'static, Blocking>;

pub struct Bus {
    pub spi: RefCell<SpiBus>,
    speed: Cell<Speed>,
}

impl Bus {
    /// `spi` must already be configured at [`Speed::SdInit`]: the SD card is
    /// probed first after boot, and a card must never see a fast clock before
    /// it is in SPI mode.
    pub fn new(spi: SpiBus) -> Self {
        Self { spi: RefCell::new(spi), speed: Cell::new(Speed::SdInit) }
    }

    pub fn config(speed: Speed) -> Config {
        Config::default().with_frequency(speed.rate()).with_mode(Mode::_0)
    }

    /// Switch the bus clock if needed. Never called while a device holds the
    /// bus (all transfers are synchronous), so the borrow cannot fail.
    pub fn set(&self, speed: Speed) {
        if self.speed.get() == speed {
            return;
        }
        if let Ok(mut spi) = self.spi.try_borrow_mut()
            && spi.apply_config(&Self::config(speed)).is_ok()
        {
            self.speed.set(speed);
        }
    }
}
