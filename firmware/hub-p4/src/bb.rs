//! Bit-banged I2C master (bring-up aid). Slow (~50 kHz) and blocking, but it
//! needs nothing from the I2C controller, so it separates "pins/wiring" from
//! "controller driver" problems on this pre-v3 chip.

use esp_hal::gpio::{DriveMode, Flex, InputConfig, OutputConfig, Pull};
use esp_hal::time::{Duration, Instant};

pub struct BbI2c<'d> {
    sda: Flex<'d>,
    scl: Flex<'d>,
}

fn half() {
    let t = Instant::now();
    while t.elapsed() < Duration::from_micros(10) {}
}

impl<'d> BbI2c<'d> {
    pub fn new(mut sda: Flex<'d>, mut scl: Flex<'d>) -> Self {
        let oc = OutputConfig::default().with_drive_mode(DriveMode::OpenDrain).with_pull(Pull::Up);
        let ic = InputConfig::default().with_pull(Pull::Up);
        for p in [&mut sda, &mut scl] {
            p.set_high();
            p.apply_output_config(&oc);
            p.apply_input_config(&ic);
            p.set_input_enable(true);
            p.set_output_enable(true);
        }
        Self { sda, scl }
    }

    /// Idle line levels (both should read high with pull-ups).
    pub fn levels(&self) -> (bool, bool) {
        (self.sda.is_high(), self.scl.is_high())
    }

    /// Drive each line low and read it back: shows whether the pads can pull
    /// the bus down (returns sda_low_ok, scl_low_ok).
    pub fn drive_test(&mut self) -> (bool, bool) {
        self.sda.set_low();
        half();
        let sda = !self.sda.is_high();
        self.sda.set_high();
        self.scl.set_low();
        half();
        let scl = !self.scl.is_high();
        self.scl.set_high();
        half();
        (sda, scl)
    }

    fn scl_high(&mut self) {
        self.scl.set_high();
        let t = Instant::now();
        // Clock stretching, bounded.
        while !self.scl.is_high() && t.elapsed() < Duration::from_millis(2) {}
        half();
    }

    fn start(&mut self) {
        self.sda.set_high();
        self.scl_high();
        self.sda.set_low();
        half();
        self.scl.set_low();
        half();
    }

    fn stop(&mut self) {
        self.sda.set_low();
        half();
        self.scl_high();
        self.sda.set_high();
        half();
    }

    /// Returns true on ACK.
    fn write_byte(&mut self, b: u8) -> bool {
        for i in (0..8).rev() {
            if b >> i & 1 == 1 { self.sda.set_high() } else { self.sda.set_low() }
            half();
            self.scl_high();
            self.scl.set_low();
        }
        self.sda.set_high();
        half();
        self.scl_high();
        let ack = !self.sda.is_high();
        self.scl.set_low();
        half();
        ack
    }

    /// Write `data` to `addr`. Returns false if any byte was not acknowledged.
    pub fn write(&mut self, addr: u8, data: &[u8]) -> bool {
        self.start();
        let mut ok = self.write_byte(addr << 1);
        for &b in data {
            if !ok { break; }
            ok = self.write_byte(b);
        }
        self.stop();
        ok
    }
}
