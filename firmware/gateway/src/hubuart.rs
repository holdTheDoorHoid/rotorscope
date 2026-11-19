//! The hub UART, both ways, without losing the hub's messages (design
//! section 14: with an async read the gateway lost 45-58 % of the hub's
//! 160-byte messages, because the read did not run within the ~1.4 ms the
//! 128-byte FIFO gives it).
//!
//! Receive runs in the UART's interrupt: the FIFO raises it at
//! [`FIFO_THRESHOLD`] bytes, or when the line goes quiet with fewer
//! (the receive timeout), and the handler moves everything into a
//! [`RING`]-byte ring that [`read`] drains. Bytes that find the ring full
//! are counted ([`OVERRUN`]), and so are the receive errors the hardware
//! reports, a FIFO overflow among them ([`RX_ERRORS`]).
//!
//! The driver stays blocking (the async driver would install its own
//! handler), and lives in a `static` the handler and the transmit side
//! share. Transmit ([`write_all`]) fills the 128-byte FIFO a piece at a time,
//! under a critical section only for the copy, and waits on a timer while
//! the FIFO is full, so it never spins with interrupts off.

use core::cell::RefCell;
use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

use critical_section::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::pipe::Pipe;
use embassy_time::{Duration, Timer};
use esp_hal::Blocking;
use esp_hal::uart::{Config, RxConfig, Uart, UartInterrupt};

/// Receive ring. At 921,600 baud it holds 44 ms of line-rate input.
pub const RING: usize = 4096;
/// Interrupt when this many bytes wait in the 128-byte FIFO: 1.0 ms of
/// headroom at line rate before the FIFO would be full.
pub const FIFO_THRESHOLD: u16 = 32;
/// While the transmit FIFO is full, look again after this long (about 46
/// bytes' time at 921,600 baud).
const TX_POLL: Duration = Duration::from_micros(500);

static UART: Mutex<RefCell<Option<Uart<'static, Blocking>>>> = Mutex::new(RefCell::new(None));
static RX: Pipe<CriticalSectionRawMutex, RING> = Pipe::new();
/// Bytes received from the hub.
pub static BYTES: AtomicU32 = AtomicU32::new(0);
/// Bytes lost because the ring was full.
pub static OVERRUN: AtomicU32 = AtomicU32::new(0);
/// Receive errors (FIFO overflow, framing, parity, glitch).
pub static RX_ERRORS: AtomicU32 = AtomicU32::new(0);

/// The receive configuration: [`FIFO_THRESHOLD`] and the default timeout
/// (10 symbols of silence).
pub fn config(baud: u32) -> Config {
    Config::default().with_baudrate(baud).with_rx(RxConfig::default().with_fifo_full_threshold(FIFO_THRESHOLD))
}

/// Take over `uart` (blocking driver, pins attached): receive from now on
/// in the interrupt.
pub fn start(mut uart: Uart<'static, Blocking>) {
    uart.set_interrupt_handler(on_uart);
    critical_section::with(|cs| {
        uart.listen(UartInterrupt::RxFifoFull | UartInterrupt::RxTimeout);
        UART.borrow_ref_mut(cs).replace(uart);
    });
}

#[esp_hal::handler(priority = esp_hal::interrupt::Priority::Priority2)]
fn on_uart() {
    critical_section::with(|cs| {
        let mut u = UART.borrow_ref_mut(cs);
        let Some(u) = u.as_mut() else { return };
        let mut buf = [0u8; 128];
        // Bounded: the FIFO holds 128 bytes, a few rounds empty it.
        for _ in 0..4 {
            match u.read_buffered(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    BYTES.fetch_add(n as u32, Relaxed);
                    let mut rest = &buf[..n];
                    // The ring hands out contiguous space: two writes at a wrap.
                    while !rest.is_empty() {
                        match RX.try_write(rest) {
                            Ok(w) => rest = &rest[w..],
                            Err(_) => {
                                OVERRUN.fetch_add(rest.len() as u32, Relaxed);
                                break;
                            }
                        }
                    }
                }
                Err(_) => {
                    RX_ERRORS.fetch_add(1, Relaxed);
                }
            }
        }
        u.clear_interrupts(UartInterrupt::RxFifoFull | UartInterrupt::RxTimeout);
    });
}

/// Whatever has arrived (waits for at least one byte).
pub async fn read(buf: &mut [u8]) -> usize {
    RX.read(buf).await
}

/// Send all of `b` to the hub.
pub async fn write_all(mut b: &[u8]) {
    while !b.is_empty() {
        let n = critical_section::with(|cs| {
            let mut u = UART.borrow_ref_mut(cs);
            let u = u.as_mut()?;
            // `write` blocks only while the FIFO is completely full.
            Some(if u.write_ready() { u.write(b).unwrap_or(0) } else { 0 })
        });
        match n {
            None => return,
            Some(0) => Timer::after(TX_POLL).await,
            Some(n) => b = &b[n..],
        }
    }
}
