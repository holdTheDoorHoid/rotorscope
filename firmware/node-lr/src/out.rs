//! Output: JSON lines for the laptop (USB) and link frames for the hub
//! (UART, wired bus or ESP-NOW, all drained from the `UART` pipe), plus the
//! node's counters. A line or frame is written whole or not at all.

use core::fmt::Write as _;
use portable_atomic::{AtomicU32, Ordering::Relaxed};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::pipe::Pipe;

pub static USB: Pipe<CriticalSectionRawMutex, 4096> = Pipe::new();
/// Link frames for the hub. A second of Air frames at the 200/s limit is
/// ~8 KB; 6 KB rides out a short stall, then frames are dropped (counted).
pub static UART: Pipe<CriticalSectionRawMutex, 6144> = Pipe::new();

pub struct Counters {
    /// Packets the radio delivered (RX_DONE).
    pub packets: AtomicU32,
    /// Packets that passed the checks and went out as Frame::Air.
    pub air: AtomicU32,
    /// Packets that failed the checks (CRC, Manchester, length).
    pub rejected: AtomicU32,
    /// Accepted packets not sent because of the rate limit.
    pub rate_dropped: AtomicU32,
    pub usb_drops: AtomicU32,
    pub uart_drops: AtomicU32,
    /// SPI / BUSY errors talking to the LR2021.
    pub radio_errors: AtomicU32,
    pub retunes: AtomicU32,
    pub rx_restarts: AtomicU32,
    pub time_syncs: AtomicU32,
    /// Control scan: packets received, recognised by ctrlwatch, link
    /// frames sent (summaries, content, energy), content dropped by the
    /// rate limit, energy windows closed, ExpressLRS follows started,
    /// refused (`lr2021cfg::ctrl::FollowGate`) and cut by its share.
    pub ctrl_packets: AtomicU32,
    pub ctrl_recognised: AtomicU32,
    pub ctrl_reports: AtomicU32,
    pub ctrl_dropped: AtomicU32,
    /// Hostile-MAVLink findings sent (`mavwatch`).
    pub mav_findings: AtomicU32,
    pub sweeps: AtomicU32,
    pub follows: AtomicU32,
    pub follows_refused: AtomicU32,
    pub follows_cut: AtomicU32,
}

pub static COUNTERS: Counters = Counters {
    packets: AtomicU32::new(0),
    air: AtomicU32::new(0),
    rejected: AtomicU32::new(0),
    rate_dropped: AtomicU32::new(0),
    usb_drops: AtomicU32::new(0),
    uart_drops: AtomicU32::new(0),
    radio_errors: AtomicU32::new(0),
    retunes: AtomicU32::new(0),
    rx_restarts: AtomicU32::new(0),
    time_syncs: AtomicU32::new(0),
    ctrl_packets: AtomicU32::new(0),
    ctrl_recognised: AtomicU32::new(0),
    ctrl_reports: AtomicU32::new(0),
    ctrl_dropped: AtomicU32::new(0),
    mav_findings: AtomicU32::new(0),
    sweeps: AtomicU32::new(0),
    follows: AtomicU32::new(0),
    follows_refused: AtomicU32::new(0),
    follows_cut: AtomicU32::new(0),
};

pub fn bump(c: &AtomicU32) {
    c.fetch_add(1, Relaxed);
}

/// Write all of `bytes` to `pipe`, or nothing if it does not fit.
pub fn emit<const N: usize>(pipe: &Pipe<CriticalSectionRawMutex, N>, bytes: &[u8], drops: &AtomicU32) -> bool {
    critical_section::with(|_| {
        if pipe.free_capacity() < bytes.len() {
            bump(drops);
            return false;
        }
        let mut rest = bytes;
        while !rest.is_empty() {
            match pipe.try_write(rest) {
                Ok(n) => rest = &rest[n..],
                Err(_) => break,
            }
        }
        true
    })
}

/// One JSON line (already formatted, without the newline) onto USB.
pub fn line(args: core::fmt::Arguments<'_>) {
    let mut s: heapless::String<384> = heapless::String::new();
    if s.write_fmt(args).is_err() || s.push('\n').is_err() {
        bump(&COUNTERS.usb_drops);
        return;
    }
    emit(&USB, s.as_bytes(), &COUNTERS.usb_drops);
}

/// Encode a link frame onto the hub pipe.
pub fn hub_frame(frame: &link::Frame) -> bool {
    let mut buf = [0u8; link::MAX_FRAME];
    match link::encode(frame, &mut buf) {
        Ok(wire) => emit(&UART, wire, &COUNTERS.uart_drops),
        Err(_) => {
            bump(&COUNTERS.uart_drops);
            false
        }
    }
}

/// Lowercase hex of up to 64 bytes.
pub struct Hex<'a>(pub &'a [u8]);

impl core::fmt::Display for Hex<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}
