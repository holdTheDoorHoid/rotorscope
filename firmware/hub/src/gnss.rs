//! GPS seam (PLAN.md P1, P3, P4). No-op until `crates/gnss` is integrated.
//!
//! What integration needs:
//! - `run()` becomes a task that owns I2C0 on `pins.gps_sda`/`gps_scl`
//!   (MAX-M10S at address 0x42), configures it through `crates/gnss`, polls
//!   UBX NAV-PVT at 1-5 Hz, and stores the result with `publish()`.
//! - `pins.pps` gets an edge interrupt that records `Instant::now()` at the
//!   top of each second; pairing it with NAV-PVT's UTC gives `utc_ms` to
//!   well under a millisecond (P3).
//! - Height must be NAV-PVT `height` (above the ellipsoid), not `hMSL`:
//!   Remote ID altitudes are geodetic (see `tracker::OwnFix`).
//!
//! Everything downstream (tracker geometry, `/api/state` `own`, CoT time)
//! already reads `latest_fix()` and `utc_ms()`, so nothing else changes.

use core::cell::Cell;

use critical_section::Mutex;
use esp_hal::gpio::AnyPin;

/// One GPS solution.
#[derive(Clone, Copy)]
pub struct Fix {
    pub lat: f64,
    pub lon: f64,
    /// WGS-84 ellipsoid height, metres; `None` for a 2D fix.
    pub alt_hae_m: Option<f64>,
    /// Hub time of the fix.
    pub at_ms: u64,
}

/// UTC anchored to hub time: `utc_ms` at hub time `at_ms`.
#[derive(Clone, Copy)]
pub struct TimeAnchor {
    pub utc_ms: u64,
    pub at_ms: u64,
}

static FIX: Mutex<Cell<Option<Fix>>> = Mutex::new(Cell::new(None));
static TIME: Mutex<Cell<Option<TimeAnchor>>> = Mutex::new(Cell::new(None));

/// Pins reserved for the GPS.
#[allow(dead_code, reason = "held so the pins stay reserved until the seam is integrated")]
pub struct GnssPins {
    pub sda: AnyPin<'static>,
    pub scl: AnyPin<'static>,
    pub pps: AnyPin<'static>,
}

/// Start the GPS. No-op default: keeps the pins reserved and returns.
pub fn start(_pins: GnssPins) {}

/// For the integration: store a new fix and/or time anchor.
#[allow(dead_code)]
pub fn publish(fix: Option<Fix>, time: Option<TimeAnchor>) {
    critical_section::with(|cs| {
        if fix.is_some() {
            FIX.borrow(cs).set(fix);
        }
        if time.is_some() {
            TIME.borrow(cs).set(time);
        }
    });
}

pub fn latest_fix() -> Option<Fix> {
    critical_section::with(|cs| FIX.borrow(cs).get())
}

/// UTC for hub time `now_ms`, if GPS time has ever been seen. The crystal
/// drifts ~20 ppm, so an anchor older than ~10 minutes is ~10 ms off; the
/// integration should refresh it every second from PPS.
pub fn utc_ms(now_ms: u64) -> Option<u64> {
    critical_section::with(|cs| TIME.borrow(cs).get())
        .map(|a| a.utc_ms.saturating_add(now_ms.saturating_sub(a.at_ms)))
}

/// Receiver state for `/api/state` `gps`. `Absent` until the integration
/// reports otherwise.
pub fn status() -> tracker::json::GpsStatus {
    critical_section::with(|cs| STATUS.borrow(cs).get())
}

static STATUS: Mutex<Cell<tracker::json::GpsStatus>> =
    Mutex::new(Cell::new(tracker::json::GpsStatus { state: tracker::json::GpsState::Absent, sats: None }));

/// For the integration: receiver state (absent / searching / 2d / 3d, sats).
#[allow(dead_code)]
pub fn publish_status(s: tracker::json::GpsStatus) {
    critical_section::with(|cs| STATUS.borrow(cs).set(s));
}
