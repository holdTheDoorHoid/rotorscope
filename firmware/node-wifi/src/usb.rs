//! The USB (USB-Serial-JTAG) output: JSON lines, written by an async task.
//!
//! Nothing here blocks or masks interrupts while a line goes out. Lines are
//! formatted into RAM and handed to [`writer`], which owns the port; when
//! the host is slow or not reading, new lines are dropped and counted
//! ([`DROPPED`]) instead of holding up the task that made them. (Before,
//! `esp_println` formatted and pushed every line to USB inside a critical
//! section: an attacker-sized Remote ID line, up to [`RID_BYTES`], kept
//! interrupts off for as long as that took.)
//!
//! Two ways in, so the largest line does not need a buffer of its own on
//! top of the one it is formatted in:
//!
//! - [`line`] / [`usb_line!`]: short lines through a byte pipe
//!   ([`PIPE_BYTES`]), each written whole or not at all.
//! - [`rid_line`]: the Remote ID line, formatted straight into the one
//!   [`RID_BYTES`] buffer the writer sends from. While it is still going out
//!   the next Remote ID line is dropped (counted).
//!
//! The writer sends whatever short lines are queued (the pipe only ever holds
//! whole lines, so an empty pipe is a line boundary), then the Remote ID
//! line if one is ready, then flushes. A panic backtrace still goes through
//! `esp_println` (esp-backtrace), straight to the port.

use core::cell::UnsafeCell;
use core::fmt::{self, Write as _};
use core::sync::atomic::{AtomicU8, AtomicU32, AtomicUsize, Ordering::{Acquire, Relaxed, Release}};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::pipe::Pipe;
use embassy_sync::signal::Signal;
use embedded_io_async::Write as _;
use esp_hal::Async;
use esp_hal::usb::usb_serial_jtag::UsbSerialJtagTx;

/// Short lines waiting for USB. The longest is a stats line (~1.2 KB); at
/// normal rates the writer keeps this near empty.
pub const PIPE_BYTES: usize = 2048;
/// The Remote ID line: a full Message Pack decoded to JSON (9 Location-sized
/// messages are ~4.2 KB) plus the raw hex (~0.5 KB).
pub const RID_BYTES: usize = 6 * 1024;
/// Longest line [`usb_line!`] formats (on the caller's stack).
const FMT_BYTES: usize = 512;

static PIPE: Pipe<CriticalSectionRawMutex, PIPE_BYTES> = Pipe::new();
static WAKE: Signal<CriticalSectionRawMutex, ()> = Signal::new();
/// Lines dropped: no room (USB host slow or absent), Remote ID buffer still
/// busy, or too long to format. Total.
pub static DROPPED: AtomicU32 = AtomicU32::new(0);

fn drop_line() {
    DROPPED.fetch_add(1, Relaxed);
}

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
        drop_line();
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

/// Format and queue one short line (see [`usb_line!`]). A line longer than
/// [`FMT_BYTES`] is dropped whole, never cut.
pub fn fmt_line(args: fmt::Arguments<'_>) {
    let mut buf = [0u8; FMT_BYTES];
    let mut f = Fill { buf: &mut buf, len: 0, over: false };
    if f.write_fmt(args).is_err() || f.over {
        drop_line();
        return;
    }
    let n = f.len;
    line(&buf[..n]);
}

/// `println!` for the USB stream: one short JSON line, never blocking.
macro_rules! usb_line {
    ($($arg:tt)*) => {
        $crate::usb::fmt_line(format_args!($($arg)*))
    };
}
pub(crate) use usb_line;

const FREE: u8 = 0;
const FILLING: u8 = 1;
const READY: u8 = 2;
const SENDING: u8 = 3;

/// The Remote ID line's buffer. Ownership moves by `state`: the drain task
/// fills it (FREE -> FILLING -> READY), the writer sends it (READY ->
/// SENDING -> FREE).
struct RidLine {
    buf: UnsafeCell<[u8; RID_BYTES]>,
    len: AtomicUsize,
    state: AtomicU8,
}

// SAFETY: the buffer is only touched by whoever moved `state` to FILLING
// (writes) or SENDING (reads); the other side waits for FREE / READY.
unsafe impl Sync for RidLine {}

static RID: RidLine = RidLine { buf: UnsafeCell::new([0; RID_BYTES]), len: AtomicUsize::new(0), state: AtomicU8::new(FREE) };

/// Format the Remote ID line with `fill` (which returns its length, or
/// `None` if it could not) straight into the writer's buffer. If the last
/// one is still going out, drop this one (counted) and return true; return
/// false only when `fill` failed, so the caller can say why.
pub fn rid_line(fill: impl FnOnce(&mut [u8]) -> Option<usize>) -> bool {
    if RID.state.compare_exchange(FREE, FILLING, Acquire, Relaxed).is_err() {
        drop_line();
        return true;
    }
    // SAFETY: we moved the state to FILLING; nobody else touches the buffer.
    let buf = unsafe { &mut *RID.buf.get() };
    // Leave a byte for the newline.
    match fill(&mut buf[..RID_BYTES - 1]) {
        Some(n) => {
            buf[n] = b'\n';
            RID.len.store(n + 1, Relaxed);
            RID.state.store(READY, Release);
            WAKE.signal(());
            true
        }
        None => {
            RID.state.store(FREE, Release);
            false
        }
    }
}

/// Owns the USB port's transmit side. A host that stops reading stalls
/// only this task; producers then drop lines.
#[embassy_executor::task]
pub async fn writer(mut tx: UsbSerialJtagTx<'static, Async>) {
    let mut chunk = [0u8; 64];
    // Written since the last flush.
    let mut dirty = false;
    loop {
        while let Ok(n) = PIPE.try_read(&mut chunk) {
            let _ = tx.write_all(&chunk[..n]).await;
            dirty = true;
        }
        if RID.state.compare_exchange(READY, SENDING, Acquire, Relaxed).is_ok() {
            // SAFETY: we moved the state to SENDING; the drain task waits for FREE.
            let buf = unsafe { &*RID.buf.get() };
            let _ = tx.write_all(&buf[..RID.len.load(Relaxed)]).await;
            RID.state.store(FREE, Release);
            dirty = true;
            continue;
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
