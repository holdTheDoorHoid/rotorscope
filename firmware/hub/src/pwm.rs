//! A duty-cycle output for the backlight and the buzzer.
//!
//! On the S3 this is an LEDC channel (hardware PWM: the backlight dims, the
//! buzzer gets a 2.7 kHz tone). esp-hal 1.2 has no LEDC driver for the C5, so
//! the C5 fallback build only switches the pin: the backlight is on or off,
//! and the buzzer must be an *active* one (it makes its own tone when
//! powered).

#[cfg(feature = "esp32s3")]
pub struct Pwm(pub esp_hal::ledc::channel::Channel<'static, esp_hal::ledc::LowSpeed>);

#[cfg(feature = "esp32s3")]
impl Pwm {
    pub fn set_pct(&mut self, pct: u8) {
        use esp_hal::ledc::channel::ChannelIFace;
        let _ = self.0.set_duty(pct.min(100));
    }
}

#[cfg(not(feature = "esp32s3"))]
pub struct Pwm(pub esp_hal::gpio::Output<'static>);

#[cfg(not(feature = "esp32s3"))]
impl Pwm {
    pub fn set_pct(&mut self, pct: u8) {
        self.0.set_level(if pct > 0 { esp_hal::gpio::Level::High } else { esp_hal::gpio::Level::Low });
    }
}
