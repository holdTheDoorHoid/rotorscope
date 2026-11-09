//! Node links: up to three UARTs at 921600 baud carrying `link::Frame`s.
//!
//! The readers run on a high-priority *interrupt* executor. At 921600 baud the
//! 128-byte UART FIFO fills in 1.4 ms; on the thread-mode executor a long
//! synchronous step (formatting the track list for a phone, say) could leave
//! it unread long enough to overflow. Up here they preempt all of that. They
//! do the minimum: bytes -> `link::Decoder` -> whole frames into `FRAMES`.
//! The tracker work happens in `ingest` on the thread-mode executor.

use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use esp_hal::Async;
use esp_hal::gpio::AnyPin;
use esp_hal::uart::{self, AnyUart, RxConfig, UartRx};
use link::{Decoder, Frame, LinkError};

use crate::hub::{Hub, now_ms};
use crate::pins::{NODE_BAUD, NODE_LINKS};

/// Frames waiting for the tracker. A full Detection is ~260 bytes, so 24
/// slots is ~6 KB: about a quarter of a second of all three nodes at full
/// tilt, far more than `ingest` ever falls behind by.
pub static FRAMES: Channel<CriticalSectionRawMutex, (u8, Frame), 24> = Channel::new();

/// Per-link counters, readable from anywhere.
pub struct LinkStats {
    pub bytes: AtomicU32,
    pub frames: AtomicU32,
    /// COBS/postcard errors: corruption, a mid-frame join, or a node on
    /// another protocol version.
    pub corrupt: AtomicU32,
    /// More than `link::MAX_FRAME` bytes without a delimiter.
    pub overflow: AtomicU32,
    /// UART hardware errors (FIFO overflow, framing, glitch).
    pub uart_err: AtomicU32,
    /// Frames dropped because `FRAMES` was full.
    pub queue_full: AtomicU32,
}

impl LinkStats {
    const fn new() -> Self {
        Self {
            bytes: AtomicU32::new(0),
            frames: AtomicU32::new(0),
            corrupt: AtomicU32::new(0),
            overflow: AtomicU32::new(0),
            uart_err: AtomicU32::new(0),
            queue_full: AtomicU32::new(0),
        }
    }

    /// Frames the hub lost on this link (the `drops` it adds to the node's own).
    pub fn hub_lost(&self) -> u32 {
        self.corrupt.load(Relaxed)
            .wrapping_add(self.overflow.load(Relaxed))
            .wrapping_add(self.queue_full.load(Relaxed))
            .wrapping_add(self.uart_err.load(Relaxed))
    }
}

pub static STATS: [LinkStats; NODE_LINKS] = [const { LinkStats::new() }; NODE_LINKS];

/// UART config for a node link. The RX FIFO interrupt fires at half full (64
/// of 128 bytes) rather than esp-hal's default of 120: that leaves ~0.7 ms to
/// drain it instead of ~0.09 ms.
pub fn uart_config() -> uart::Config {
    uart::Config::default()
        .with_baudrate(NODE_BAUD)
        .with_rx(RxConfig::default().with_fifo_full_threshold(64).with_timeout(10))
}

/// One reader per link. Runs on the interrupt executor (see module docs).
///
/// The driver is built here rather than passed in: esp-hal's async drivers
/// are not `Send` (their interrupt is bound on the core that creates them),
/// and a task on another executor can only be handed `Send` arguments.
#[embassy_executor::task(pool_size = NODE_LINKS)]
pub async fn reader(index: u8, uart: AnyUart<'static>, pin: AnyPin<'static>) {
    let stats = &STATS[usize::from(index)];
    let mut rx: UartRx<'static, Async> = match UartRx::new(uart, uart_config()) {
        Ok(rx) => rx.with_rx(pin).into_async(),
        Err(_) => {
            crate::usb::msg("err", "node uart config");
            return;
        }
    };
    let mut decoder = Decoder::new();
    let mut buf = [0u8; 128];
    loop {
        let n = match rx.read_async(&mut buf).await {
            Ok(n) => n,
            Err(_) => {
                // The driver already reset the FIFO; whatever frame was in
                // flight is lost and the decoder resyncs at the next zero.
                stats.uart_err.fetch_add(1, Relaxed);
                continue;
            }
        };
        stats.bytes.fetch_add(n as u32, Relaxed);
        decoder.feed(&buf[..n], |r| match r {
            Ok(frame) => {
                stats.frames.fetch_add(1, Relaxed);
                if FRAMES.try_send((index, frame)).is_err() {
                    stats.queue_full.fetch_add(1, Relaxed);
                }
            }
            Err(LinkError::Corrupt) => {
                stats.corrupt.fetch_add(1, Relaxed);
            }
            Err(LinkError::Overflow) => {
                stats.overflow.fetch_add(1, Relaxed);
            }
        });
    }
}

/// Thread-mode side: frames into the hub state.
#[embassy_executor::task]
pub async fn ingest(hub: &'static Hub) {
    loop {
        let (node, frame) = FRAMES.receive().await;
        hub.on_frame(node, frame, now_ms());
    }
}
