//! Buttons, buzzer and battery seam (PLAN.md O6, H2). No-op until the panel
//! hardware exists.
//!
//! What integration needs:
//! - `buttons`: three inputs with pull-ups, debounced in a task (20 ms), sent
//!   as events to whatever owns the screen's menu.
//! - `buzzer`: LEDC PWM at ~2.7 kHz; `new_drone` plays a short chirp. Rate-limit
//!   it (a swarm of new tracks should not sound like a siren).
//! - `battery`: ADC1 with the divider ratio of the PCB, averaged over a few
//!   samples; feeds `UiState::battery_mv`.

use esp_hal::gpio::AnyPin;
use tracker::TrackId;

use crate::pins::BatteryPin;

#[allow(dead_code, reason = "held so the pins stay reserved until the seam is integrated")]
pub struct PanelPins {
    pub buttons: [AnyPin<'static>; 3],
    pub buzzer: AnyPin<'static>,
    pub battery: BatteryPin,
}

/// Configure the panel. No-op default: keeps the pins reserved.
pub fn start(_pins: PanelPins) {}

/// A drone was seen for the first time. No-op default (the USB stream still
/// gets a `{"t":"new"}` line).
pub fn new_drone(_id: TrackId) {}

/// Battery voltage in millivolts, when measured.
pub fn battery_mv() -> Option<u16> {
    None
}
