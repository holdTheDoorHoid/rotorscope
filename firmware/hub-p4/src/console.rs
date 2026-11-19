//! The USB-C console (CH340 on UART0, GPIO37 TX / 38 RX, 115200 baud):
//! log lines and the JSON-lines output (PLAN.md O4), one-letter commands in.
//!
//! The S3 hub streams JSON on its USB-Serial-JTAG port. On this board the
//! P4's own USB-Serial-JTAG pins (GPIO24/25) carry the microphone, so the
//! CH340 console is the only USB serial port, and it is shared: log lines
//! start `hub-p4:`, JSON lines start `{`. A reader takes the lines starting
//! with `{` (the field names match the S3 hub's USB stream).
//!
//! **Never block the loop on output.** Everything goes into a 16 KB ring
//! that [`pump`] moves into the 128-byte UART FIFO without waiting, from the
//! main loop and between screen bands. 115200 baud is ~11.5 KB/s, so:
//! - a JSON line that does not fit is dropped whole and counted
//!   (`console_drops` in the `hub` line), never written in part;
//! - a log line that does not fit waits for room (logs are rare and are the
//!   record of what happened; at boot this is the only output anyway).
//!
//! `crate::println!` (defined in `main.rs`) routes here once [`init`] has
//! run, and straight to esp-println before that. Panics still print through
//! esp-println (blocking, ROM routine), which is what a panic wants.
//!
//! Commands (one character, no Enter needed), as on the S3 hub's USB; they
//! are handled by `command` in `main.rs`, whose `h` text is the full list:
//!   `p` set own position to the test point (manual source); bench builds
//!       only (feature `testpoint`)
//!   `c` clear the manual own position
//!   `b` time the airspace lookups
//!   `a` aero nodes: step through auto and each single mode
//!   `m` hub moved: forget the GPS home the spoofing checks learned
//!   `H` accept the new GPS home the hub found (after a "NOT adopted" line)
//!   `T` accept GPS time as it is now (after a time jump / time floor alarm)
//!   `L` type one site line (node position, mute an alarm; `tracker::site`)
//!   `n` type a note into the evidence log
//!   `k` evidence checkpoint now
//!   `r` replay the evidence file over the console
//!   `u` evidence records also streamed on the console, on or off
//!   `e` stop cleanly: shutdown event, last checkpoint, flush the card
//!   `h` / `?` help
//! Builds with the bench feature `faulttest` also take `fault ...` lines.

use core::cell::RefCell;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};

use critical_section::Mutex;
use esp_hal::Blocking;
use esp_hal::uart::{Config, Uart};

const RING: usize = 16 * 1024;
static UART: Mutex<RefCell<Option<Uart<'static, Blocking>>>> = Mutex::new(RefCell::new(None));
static TX: Mutex<RefCell<heapless::Deque<u8, RING>>> = Mutex::new(RefCell::new(heapless::Deque::new()));
static READY: AtomicBool = AtomicBool::new(false);
/// JSON lines dropped for want of room.
pub static DROPS: AtomicU32 = AtomicU32::new(0);
/// Bytes written to the UART.
pub static SENT: AtomicU32 = AtomicU32::new(0);

/// Take over UART0 (the bootloader and esp-println already use it at
/// 115200; this keeps the same settings).
pub fn init(uart0: esp_hal::peripherals::UART0<'static>, tx: esp_hal::peripherals::GPIO37<'static>, rx: esp_hal::peripherals::GPIO38<'static>) {
    // Let anything esp-println already started finish leaving the FIFO.
    let t = esp_hal::time::Instant::now();
    while t.elapsed().as_millis() < 5 {}
    let Ok(uart) = Uart::new(uart0, Config::default().with_baudrate(115_200)) else { return };
    let uart = uart.with_tx(tx).with_rx(rx);
    critical_section::with(|cs| UART.borrow_ref_mut(cs).replace(uart));
    READY.store(true, Relaxed);
}

pub fn ready() -> bool {
    READY.load(Relaxed)
}

/// Move queued bytes into the TX FIFO, as many as fit right now.
pub fn pump() {
    critical_section::with(|cs| {
        let mut uart = UART.borrow_ref_mut(cs);
        let Some(uart) = uart.as_mut() else { return };
        let mut tx = TX.borrow_ref_mut(cs);
        while !tx.is_empty() && uart.write_ready() {
            let (front, _) = tx.as_slices();
            let Ok(w) = uart.write(&front[..front.len().min(32)]) else { break };
            for _ in 0..w {
                tx.pop_front();
            }
            SENT.fetch_add(w as u32, Relaxed);
        }
    });
}

/// Free bytes in the console ring.
pub fn room() -> usize {
    critical_section::with(|cs| {
        let tx = TX.borrow_ref(cs);
        tx.capacity() - tx.len()
    })
}

fn push(bytes: &[u8]) {
    critical_section::with(|cs| {
        let mut tx = TX.borrow_ref_mut(cs);
        for &b in bytes {
            let _ = tx.push_back(b);
        }
    });
}

/// A log line (without its newline): waits for room if it must.
pub fn log(line: &[u8]) {
    let need = line.len() + 1;
    let t = esp_hal::time::Instant::now();
    while room() < need {
        pump();
        // ~1.4 s drains the whole ring; past that something is wrong with
        // the UART: drop rather than hang.
        if t.elapsed().as_millis() > 2_000 {
            DROPS.fetch_add(1, Relaxed);
            return;
        }
    }
    push(line);
    push(b"\n");
    pump();
}

/// A complete JSON line (with its newline), or nothing.
pub fn json(line: &[u8]) -> bool {
    if room() < line.len() {
        DROPS.fetch_add(1, Relaxed);
        return false;
    }
    push(line);
    pump();
    true
}

/// The next command byte from the console, if any.
pub fn read_command() -> Option<u8> {
    critical_section::with(|cs| {
        let mut uart = UART.borrow_ref_mut(cs);
        let uart = uart.as_mut()?;
        let mut b = [0u8; 1];
        match uart.read_buffered(&mut b) {
            Ok(1) => Some(b[0]),
            _ => None,
        }
    })
}

/// `core::fmt::Write` into a fixed buffer, truncating at the end.
pub struct LineBuf {
    pub buf: [u8; 1536],
    pub len: usize,
}

impl LineBuf {
    pub const fn new() -> Self {
        Self { buf: [0; 1536], len: 0 }
    }
}

impl core::fmt::Write for LineBuf {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let n = s.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

/// Used by `crate::println!`.
pub fn print_fmt(args: core::fmt::Arguments<'_>) {
    if !ready() {
        esp_println::println!("{}", args);
        return;
    }
    let mut b = LineBuf::new();
    let _ = core::fmt::write(&mut b, args);
    log(&b.buf[..b.len]);
}
