//! Screen seam (PLAN.md O2). No-op until `crates/screen` is integrated.
//!
//! The hub builds a [`UiState`] once a second and calls [`render`]. What
//! integration needs:
//! - An SPI bus on `pins.spi_sck/mosi/miso` shared with the SD card
//!   (`embassy-embedded-hal`'s shared-bus `SpiDevice` with `screen_cs`; the
//!   panel also needs `screen_dc` and `screen_backlight`).
//! - A 320x240 panel framebuffer does NOT fit in RAM without PSRAM
//!   (150 KB at RGB565). Render in horizontal bands (e.g. 320x24 = 15 KB) or
//!   draw primitives straight to the panel; `crates/screen` should be written
//!   for that.
//! - `render` should run in its own task, fed through a `Signal<UiState>`, so a
//!   slow SPI transfer never delays the tracker.

use esp_hal::gpio::AnyPin;

/// Everything the on-device screen shows (O2).
#[derive(Clone, Default)]
#[allow(dead_code)]
pub struct UiState {
    pub now_ms: u64,
    pub active_drones: u16,
    pub total_tracks: u16,
    /// Nearest active drone: display ID, distance (m), bearing (deg true).
    pub nearest: Option<Nearest>,
    /// A drone appeared in the last few seconds (flash / banner).
    pub new_drone_alert: bool,
    /// `None` = own position unknown (P4): show "no fix", no bearings.
    pub own_source: Option<&'static str>,
    /// Alive flag per node link, in link order.
    pub nodes_alive: heapless::Vec<bool, 3>,
    /// What a phone needs to join (for a QR code: `WIFI:T:WPA;S:<ssid>;P:<pw>;;`).
    pub ssid: heapless::String<32>,
    /// Empty for an open-hotspot build.
    pub password: heapless::String<16>,
    pub battery_mv: Option<u16>,
    pub recording: bool,
}

#[derive(Clone)]
#[allow(dead_code)]
pub struct Nearest {
    pub id: heapless::String<20>,
    pub dist_m: f32,
    pub brg_deg: f32,
}

#[allow(dead_code, reason = "held so the pins stay reserved until the seam is integrated")]
pub struct ScreenPins {
    pub cs: AnyPin<'static>,
    pub dc: AnyPin<'static>,
    pub backlight: AnyPin<'static>,
}

/// Bring up the panel. No-op default.
pub fn start(_pins: ScreenPins) {}

/// Draw one frame. No-op default.
pub fn render(_ui: &UiState) {}
