//! ADS-B from an ADSBee 1090 (PLAN.md Phase 6), feature `adsbee`, S3 only.
//!
//! The ADSBee's COMMS_UART sends CSBee text (`crates/adsb`) at 115200 baud
//! once `AT+PROTOCOL_OUT=COMMS_UART,CSBEE` has been set on its USB console:
//! one `#A:` line per aircraft in its table every second, then a `#S:`
//! statistics line. Only its TX is wired, to [`crate::pins`] `adsbee_rx`:
//! the ADSBee reads AT commands from its USB console only (its
//! `CommsManager::UpdateAT` never looks at COMMS_UART), so the hub could not
//! configure it over this wire even if it wanted to. The setup is a one-time
//! job for the owner; see `docs/ADSBEE.md`.
//!
//! **UART budget.** The S3 has three UARTs and the three radio nodes use
//! them all, so this feature takes UART2 from node link 2 (the 5 GHz node),
//! exactly as `gps-uart` does; the two features exclude each other. A
//! fourth serial port without losing a node needs an I2C UART bridge
//! (SC16IS752, two UARTs) on the GPS's I2C bus: a carrier-PCB decision.
//!
//! Two halves, like the node links:
//! - [`reader`] on the interrupt executor: bytes -> `adsb::LineSplitter`
//!   -> parsed reports into [`REPORTS`]. Refused lines are only counted.
//! - [`ingest`] on the thread-mode executor: reports into the tracker's
//!   aircraft table (`Hub::on_adsb`). Never the drone table, never an alert.

use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

use tracker::json::AdsbStatus;

use crate::hub::Hub;

/// Built with an aircraft table: an ADSBee on this board's UART (feature
/// `adsbee`), or a P4 hub forwarding its ADSBee's lines (feature `webhead`).
pub const ENABLED: bool = cfg!(any(feature = "adsbee", feature = "webhead"));
/// The ADSBee reports once a second; quiet for this long means it is gone.
pub const ALIVE_MS: u64 = 3_000;

/// Lines parsed (aircraft and statistics).
pub static LINES: AtomicU32 = AtomicU32::new(0);
/// Lines refused: bad CRC, malformed, too long.
pub static ERRORS: AtomicU32 = AtomicU32::new(0);
/// Text that is not CSBee at all (e.g. the wrong protocol selected).
pub static OTHER: AtomicU32 = AtomicU32::new(0);
pub static BYTES: AtomicU32 = AtomicU32::new(0);
/// UART hardware errors (FIFO overflow, framing).
pub static UART_ERR: AtomicU32 = AtomicU32::new(0);
/// Reports dropped because the queue to the tracker was full.
pub static QUEUE_FULL: AtomicU32 = AtomicU32::new(0);

#[cfg(feature = "adsbee")]
mod imp {
    use core::sync::atomic::Ordering::Relaxed;

    use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
    use embassy_sync::channel::Channel;
    use esp_hal::Async;
    use esp_hal::gpio::AnyPin;
    use esp_hal::uart::{self, AnyUart, RxConfig, UartRx};

    use super::*;
    use crate::hub::now_ms;

    /// Parsed reports waiting for the tracker. A report is ~100 bytes; 16
    /// is a few hundred milliseconds of a busy sky at 115200 baud, far more
    /// than the thread-mode executor ever falls behind by.
    pub static REPORTS: Channel<CriticalSectionRawMutex, adsb::Report, 16> = Channel::new();

    /// The ADSBee's COMMS_UART: 115200 8N1 (its default; see `docs/ADSBEE.md`).
    fn config() -> uart::Config {
        uart::Config::default()
            .with_baudrate(adsb::DEFAULT_BAUD)
            .with_rx(RxConfig::default().with_fifo_full_threshold(64).with_timeout(10))
    }

    /// Reads the ADSBee. Runs on the interrupt executor next to the node
    /// readers (a CSBee line is parsed in well under a millisecond, and at
    /// 115200 baud the FIFO threshold leaves ~5 ms to spare).
    #[embassy_executor::task]
    pub async fn reader(uart: AnyUart<'static>, pin: AnyPin<'static>) {
        let mut rx: UartRx<'static, Async> = match UartRx::new(uart, config()) {
            Ok(rx) => rx.with_rx(pin).into_async(),
            Err(_) => {
                crate::usb::msg("err", "adsbee uart config");
                return;
            }
        };
        let mut split = adsb::LineSplitter::new();
        let mut buf = [0u8; 128];
        loop {
            let n = match rx.read_async(&mut buf).await {
                Ok(n) => n,
                Err(_) => {
                    UART_ERR.fetch_add(1, Relaxed);
                    split.reset();
                    continue;
                }
            };
            BYTES.fetch_add(n as u32, Relaxed);
            split.feed(&buf[..n], |r| match r {
                Ok(report) => {
                    LINES.fetch_add(1, Relaxed);
                    if REPORTS.try_send(report).is_err() {
                        QUEUE_FULL.fetch_add(1, Relaxed);
                    }
                }
                Err(adsb::Error::NotCsbee) => {
                    OTHER.fetch_add(1, Relaxed);
                }
                Err(_) => {
                    ERRORS.fetch_add(1, Relaxed);
                }
            });
        }
    }

    /// Thread-mode side: reports into the hub.
    #[embassy_executor::task]
    pub async fn ingest(hub: &'static Hub) {
        loop {
            let r = REPORTS.receive().await;
            hub.on_adsb(&r, now_ms());
        }
    }
}

#[cfg(all(feature = "adsbee", feature = "demo"))]
pub use imp::REPORTS;
#[cfg(feature = "adsbee")]
pub use imp::{ingest, reader};

/// One CSBee line forwarded by an upstream hub (`link::Frame::Csbee`, web
/// head): the same parser and the same counters as a line from the UART.
/// Ignored without an aircraft table.
pub fn on_forwarded_line(hub: &Hub, line: &[u8], now: u64) {
    if !ENABLED {
        return;
    }
    BYTES.fetch_add(line.len() as u32, Relaxed);
    match adsb::parse(line) {
        Ok(r) => {
            LINES.fetch_add(1, Relaxed);
            hub.on_adsb(&r, now);
        }
        Err(adsb::Error::NotCsbee) => {
            OTHER.fetch_add(1, Relaxed);
        }
        Err(_) => {
            ERRORS.fetch_add(1, Relaxed);
        }
    }
}

/// The ADSBee's health for `/api/state`; `None` when not built in.
pub fn status(hub: &Hub, now: u64) -> Option<AdsbStatus> {
    if !ENABLED {
        return None;
    }
    let h = hub.adsb.get();
    let age = h.last_ms.map(|t| now.saturating_sub(t));
    Some(AdsbStatus {
        alive: age.is_some_and(|a| a <= ALIVE_MS),
        age_ms: age,
        frames_per_s: h.stats.map(|s| s.valid_per_s()),
        aircraft: h.stats.map(|s| s.aircraft),
        errors: ERRORS.load(Relaxed),
    })
}

/// For the USB `hub` line.
#[derive(serde::Serialize)]
pub struct AdsbLine {
    alive: bool,
    age_ms: Option<u64>,
    bytes: u32,
    lines: u32,
    errors: u32,
    other: u32,
    uart_err: u32,
    qfull: u32,
    /// From the last `#S:` line: CSBee version, valid frames/s, aircraft in
    /// the ADSBee's own table, its uptime.
    csbee: Option<u16>,
    valid_ps: Option<u32>,
    table: Option<u16>,
    up_s: Option<u32>,
}

impl AdsbLine {
    pub fn now(hub: &Hub, now: u64) -> Option<Self> {
        let st = status(hub, now)?;
        let s = hub.adsb.get().stats;
        Some(Self {
            alive: st.alive,
            age_ms: st.age_ms,
            bytes: BYTES.load(Relaxed),
            lines: LINES.load(Relaxed),
            errors: ERRORS.load(Relaxed),
            other: OTHER.load(Relaxed),
            uart_err: UART_ERR.load(Relaxed),
            qfull: QUEUE_FULL.load(Relaxed),
            csbee: s.map(|s| s.version),
            valid_ps: s.map(|s| s.valid_per_s()),
            table: s.map(|s| s.aircraft),
            up_s: s.map(|s| s.uptime_s),
        })
    }
}
