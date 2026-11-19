//! The USB (USB-Serial-JTAG) output: JSON lines, written by an async task,
//! after the node's writer (firmware/node-wifi/src/usb.rs).
//!
//! Nothing here blocks or masks interrupts for longer than a copy into RAM.
//! `esp_println` writes each line to the port inside a critical section,
//! which held off the hub UART's receive long enough to lose bytes (design
//! section 14): lines are formatted into RAM, queued whole in a byte pipe,
//! and [`writer`], which owns the port, sends them. When the host is slow or
//! not reading, new lines are dropped and counted ([`DROPPED`]) instead of
//! holding up the task that made them. A panic backtrace still goes through
//! `esp_println` (esp-backtrace), straight to the port.

use core::fmt::{self, Write as _};
use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::pipe::Pipe;
use embassy_sync::signal::Signal;
use embedded_io_async::Write as _;
use esp_hal::Async;
use esp_hal::usb::usb_serial_jtag::UsbSerialJtagTx;

/// Lines waiting for USB. The longest is the stats line (~700 bytes).
pub const PIPE_BYTES: usize = 2048;
/// Longest line [`usb_line!`] formats (on the caller's stack).
const FMT_BYTES: usize = 768;

static PIPE: Pipe<CriticalSectionRawMutex, PIPE_BYTES> = Pipe::new();
static WAKE: Signal<CriticalSectionRawMutex, ()> = Signal::new();
/// Lines dropped: no room (USB host slow or absent) or too long. Total.
pub static DROPPED: AtomicU32 = AtomicU32::new(0);

/// Queue `text` plus a newline, whole or not at all. False if dropped.
pub fn line(text: &[u8]) -> bool {
    let ok = critical_section::with(|_| {
        if PIPE.free_capacity() < text.len() + 1 {
            return false;
        }
        // The ring hands out contiguous space only, so a wrap takes two
        // writes; with the space checked above this always finishes.
        for part in [text, b"\n"] {
            let mut rest = part;
            while !rest.is_empty() {
                match PIPE.try_write(rest) {
                    Ok(n) => rest = &rest[n..],
                    Err(_) => break,
                }
            }
        }
        true
    });
    if ok {
        WAKE.signal(());
    } else {
        DROPPED.fetch_add(1, Relaxed);
    }
    ok
}

/// Keeps what fits; remembers that something did not.
struct Fill<'a> {
    buf: &'a mut [u8],
    len: usize,
    over: bool,
}

impl fmt::Write for Fill<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let b = s.as_bytes();
        match self.buf.get_mut(self.len..self.len + b.len()) {
            Some(dst) => {
                dst.copy_from_slice(b);
                self.len += b.len();
                Ok(())
            }
            None => {
                self.over = true;
                Err(fmt::Error)
            }
        }
    }
}

/// Format and queue one line (see [`usb_line!`]). A line longer than
/// [`FMT_BYTES`] is dropped whole, never cut.
pub fn fmt_line(args: fmt::Arguments<'_>) {
    let mut buf = [0u8; FMT_BYTES];
    let mut f = Fill { buf: &mut buf, len: 0, over: false };
    if f.write_fmt(args).is_err() || f.over {
        DROPPED.fetch_add(1, Relaxed);
        return;
    }
    let n = f.len;
    line(&buf[..n]);
}

/// `println!` for the USB stream: one JSON line, never blocking.
macro_rules! usb_line {
    ($($arg:tt)*) => {
        $crate::usb::fmt_line(format_args!($($arg)*))
    };
}
pub(crate) use usb_line;

/// Owns the USB port's transmit side. A host that stops reading stalls
/// only this task; producers then drop lines.
#[embassy_executor::task]
pub async fn writer(mut tx: UsbSerialJtagTx<'static, Async>) {
    let mut chunk = [0u8; 64];
    let mut dirty = false;
    loop {
        while let Ok(n) = PIPE.try_read(&mut chunk) {
            let _ = tx.write_all(&chunk[..n]).await;
            dirty = true;
        }
        if dirty {
            // Ends the USB transfer so the host sees the line now.
            let _ = tx.flush().await;
            dirty = false;
            continue;
        }
        WAKE.wait().await;
    }
}
