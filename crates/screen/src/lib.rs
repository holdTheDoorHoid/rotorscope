//! Rotorscope's on-device screen (PLAN.md O2, O6, P2, P4).
//!
//! Three pieces, all `no_std` and allocation-free:
//!
//! - [`UiState`]: a plain view-model the hub fills each frame (from the
//!   tracker with [`from_tracker::fill_from_tracker`], plus GPS, node, power
//!   and SD status).
//! - [`Ui`]: the screen's own state machine (page, cursor, alert banner,
//!   settings). Feed it [`Button`] events (from [`ButtonDecoder`]),
//!   [`Ui::new_drone`] when the tracker announces one, and [`Ui::tick`].
//! - [`render`]: draws one 320x240 frame to any embedded-graphics
//!   `DrawTarget<Color = Rgb565>`, such as a `mipidsi` display or a
//!   framebuffer.
//! - [`band`]: the same frame drawn 24 rows at a time into a 15 KB buffer,
//!   for hubs without PSRAM ([`render_banded`]).
//!
//! ```ignore
//! let mut ui = Ui::default();
//! let mut keys = ButtonDecoder::new();
//! loop {
//!     let now = uptime_ms();
//!     fill_from_tracker(&tracker, &tracker.context(now, utc), &mut state);
//!     state.gps = ...; state.nodes = ...; state.power = ...; state.sd = ...;
//!     while let Some(id) = tracker.take_new_drone(now) { ui.new_drone(id.0, now) }
//!     if ui.take_buzz().is_some() { buzzer.chirp() }
//!     if let Some(b) = keys.poll(up.is_low(), down.is_low(), sel.is_low(), now) {
//!         ui.button(b, &state);
//!     }
//!     ui.tick(now);
//!     render(&ui, &state, &mut framebuffer)?;
//!     display.flush(&framebuffer);
//! }
//! ```
#![no_std]

pub mod band;
mod draw;
pub mod model;
mod render;
pub mod theme;
pub mod ui;

#[cfg(feature = "tracker")]
pub mod from_tracker;

pub use band::{BAND_ROWS, Band, render_banded};
pub use model::*;
pub use render::{BAR_H, HEIGHT, WIDTH, render};
pub use theme::Palette;
pub use ui::{Alert, Button, ButtonDecoder, Buzz, Mode, Page, Request, Settings, Theme, Ui};

#[cfg(test)]
mod tests;
