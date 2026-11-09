//! SD-card log seam (PLAN.md O3). No-op until SD is integrated.
//!
//! The hub calls [`log_tick`] once a second with the tracker. What integration
//! needs:
//! - The shared SPI bus (see `screen.rs`) with `pins.sd_cs`, and a FAT driver
//!   (`embedded-sdmmc` is the usual no_std choice).
//! - One file per boot (or per UTC day once GPS time exists), starting with
//!   `tracker::csv::HEADER`, then `tracker::csv::row` for every track heard
//!   since the last tick (compare `Track::last_seen_ms` with the previous tick).
//! - Writes off the 1 Hz path: queue rows into a `Pipe` and let an SD task
//!   flush them, so a slow card never stalls USB/CoT. A 512-byte sector buffer
//!   is all the RAM it needs.
//! - [`recording`] drives `"rec"` in `/api/state`.

use esp_hal::gpio::AnyPin;
use tracker::{Context, Tracker};

/// Mount the card. No-op default: keeps the pin reserved.
pub fn start(_cs: AnyPin<'static>) {}

/// Log every track heard since `since_ms`. No-op default.
pub fn log_tick<const N: usize>(_tracker: &Tracker<N>, _ctx: &Context, _since_ms: u64) {}

/// True while rows are being written to a card.
pub fn recording() -> bool {
    false
}
