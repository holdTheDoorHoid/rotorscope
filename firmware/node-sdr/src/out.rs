//! Dump output that waits for the host.
//!
//! esp-println waits only ~50,000 spin iterations (about a millisecond) for the
//! USB-Serial-JTAG FIFO to drain, then drops output until it empties. That is
//! right for logs (a node without a host must never block), but a dump is
//! hundreds of lines in a burst, and behind a busy USB hub the host misses
//! enough polls that whole stretches vanish: on 2026-11-12 every typed `dump`
//! stopped after line 47 and even `resend` output was lost. A dump only ever
//! runs because a host asked for it, so here we wait properly (up to
//! [`WAIT`] per FIFO refill) and give up on the rest of the dump only if the
//! host has really gone.

use core::fmt::{self, Write};
use esp_hal::time::{Duration, Instant};

// ESP32-C5 USB-Serial-JTAG (same addresses as esp-println's C5 printer).
const FIFO: *mut u32 = 0x6000_F000 as *mut u32;
const CONF: *mut u32 = 0x6000_F004 as *mut u32;
/// Longest wait for the host to take one 64-byte FIFO load.
const WAIT: Duration = Duration::from_millis(250);

fn room() -> bool {
    // SAFETY: plain reads/writes of the USB-Serial-JTAG FIFO registers, from
    // the one task that prints dumps (esp-println uses the same registers the
    // same way; the capture loop never prints from an interrupt).
    unsafe { CONF.read_volatile() & 0b010 != 0 }
}

fn flush() {
    // SAFETY: as in `room`. Bit 0 = WR_DONE: send what is in the FIFO now.
    unsafe { CONF.write_volatile(0b001) }
}

/// Waits for the host up to `.0` per FIFO load.
struct Usb(Duration);

/// Set when a paced line found no host; cleared once the FIFO has room again.
static HOST_GONE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// Paced (unrequested, periodic) output waits much less: a node with no host
/// attached must not stall its loop.
const WAIT_PACED: Duration = Duration::from_millis(3);

impl Write for Usb {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &b in s.as_bytes() {
            if !room() {
                flush();
                let t0 = Instant::now();
                while !room() {
                    if t0.elapsed() > self.0 {
                        return Err(fmt::Error);
                    }
                }
            }
            // SAFETY: as in `room`; there is space for one byte.
            unsafe { FIFO.write_volatile(b as u32) }
        }
        Ok(())
    }
}

/// Print one line of a dump. Returns false if the host stopped reading, so
/// the caller can abandon the rest instead of waiting on every line.
pub fn line(args: fmt::Arguments) -> bool {
    let ok = Usb(WAIT).write_fmt(args).is_ok() && Usb(WAIT).write_str("\n").is_ok();
    flush();
    ok
}

/// Write `s` and a newline in pieces of at most `piece` bytes, calling
/// `between` after each piece. `between` must not print. Gives up quietly
/// (the rest of the line is dropped) if the host stops reading.
pub fn paced(s: &str, piece: usize, mut between: impl FnMut()) {
    use core::sync::atomic::Ordering::Relaxed;
    if HOST_GONE.load(Relaxed) {
        if !room() {
            return;
        }
        HOST_GONE.store(false, Relaxed);
    }
    let b = s.as_bytes();
    let mut k = 0;
    while k < b.len() {
        let end = (k + piece).min(b.len());
        // SAFETY of the slice: `s` is ASCII JSON, so any byte offset is a char boundary.
        let ok = Usb(WAIT_PACED).write_str(core::str::from_utf8(&b[k..end]).unwrap_or("")).is_ok();
        flush();
        if !ok {
            HOST_GONE.store(true, Relaxed);
            return;
        }
        between();
        k = end;
    }
    let _ = Usb(WAIT_PACED).write_str("\n");
    flush();
}
