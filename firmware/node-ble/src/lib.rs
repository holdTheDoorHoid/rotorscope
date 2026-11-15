//! Node B: Bluetooth Remote ID listener.
//!
//! Data flow, one direction only:
//!
//! ```text
//! controller ─HCI─► trouble-host runner ─► adv::ScanHandler (cheap filter, copy)
//!                                               │ adv::REPORTS (fixed queue)
//!                                               ▼
//!                                     decode task (rid crate)
//!                                       │                 │
//!                         out::USB (JSON lines)   out::UART (link frames)
//! ```
//!
//! The handler runs inside the host's receive loop, so anything slow there
//! stalls HCI for every advert. It only filters and copies; decoding happens in
//! its own task.
#![no_std]

pub mod adv;
pub mod logger;
pub mod out;
pub mod role;
#[cfg(feature = "espnow")]
pub mod uplink;

/// Chip-specific constants. Only pins and names differ between the S3 and C5.
pub mod chip {
    #[cfg(all(feature = "esp32s3", feature = "esp32c5"))]
    compile_error!("enable exactly one chip feature: esp32s3 or esp32c5");

    #[cfg(feature = "esp32s3")]
    pub const NAME: &str = "esp32s3";
    #[cfg(feature = "esp32c5")]
    pub const NAME: &str = "esp32c5";
}

/// Link to the hub (wired builds; with `espnow` the frames go to the gateway).
/// The C5 uses the same pins as node-wifi and the gateway (XIAO D6 = GPIO11
/// TX, D7 = GPIO12 RX), so one carrier wiring serves every image on the board.
pub mod hub {
    /// Start fast: frames are small and the hub has to merge four nodes. Lower
    /// it if a long carrier-board trace shows errors.
    pub const BAUD: u32 = 921_600;
    /// How often the hub is reminded who we are, so a hub that restarts
    /// relearns its nodes without resetting them.
    pub const HELLO_EVERY_S: u64 = 5;
}

/// Scan timing. Scan window = interval on each PHY, so the radio never idles.
/// With two PHYs the controller alternates: one interval on 1M, one on coded.
/// Long enough to follow an AUX_ADV_IND chain within one window, short enough
/// that neither PHY goes deaf for long (BT4 Remote ID repeats each message type
/// at least once a second, on all three primary channels).
pub const SCAN_INTERVAL_MS: u64 = 100;
/// Scan window = interval: the radio never idles while scanning. (A shorter
/// window did not give Wi-Fi any receive time under coexistence; `espnow`
/// builds stop the scan outright for a moment instead, see `uplink`.)
pub const SCAN_WINDOW_MS: u64 = SCAN_INTERVAL_MS;

pub const FIRMWARE: &str = env!("CARGO_PKG_VERSION");
