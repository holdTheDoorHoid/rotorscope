//! USB-Serial-JTAG: JSON lines out (PLAN.md O4), one-letter commands in.
//!
//! Output goes through a byte pipe drained by its own task (the node-ble
//! pattern), so a laptop that is not reading never stalls anything else; a
//! line that does not fit is dropped whole and counted (`usb_drops`), never
//! written in part.
//!
//! Commands (one character, no Enter needed):
//!   `p` set own position to the test point (manual source)
//!   `c` clear the manual own position
//!   `m` toggle CoT multicast
//!   `s` print the /api/state body as one line
//!   `h` / `?` hotspot details and this help

use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::pipe::Pipe;
use embassy_time::{Duration, Instant, Timer};
use embedded_io_async::{Read as _, Write as _};
use esp_hal::Async;
use esp_hal::usb::usb_serial_jtag::{UsbSerialJtagRx, UsbSerialJtagTx};
use serde::Serialize;

use crate::hub::{Hub, now_ms};
use crate::{api, cot, hotspot};

/// A couple of seconds of output with a handful of tracks.
static OUT: Pipe<CriticalSectionRawMutex, 8192> = Pipe::new();
pub static DROPS: AtomicU32 = AtomicU32::new(0);

/// Write all of `bytes` or nothing. The check and the write are one critical
/// section because lines come from both executors.
pub fn emit(bytes: &[u8]) -> bool {
    critical_section::with(|_| {
        if OUT.free_capacity() < bytes.len() {
            DROPS.fetch_add(1, Relaxed);
            return false;
        }
        let mut rest = bytes;
        // The ring hands out contiguous space only, so a wrap takes two writes.
        while !rest.is_empty() {
            match OUT.try_write(rest) {
                Ok(n) => rest = &rest[n..],
                Err(_) => break,
            }
        }
        true
    })
}

/// As [`emit`], but wait up to `patience` for room. For the bulk track dump,
/// which can be bigger than the pipe: waiting lets the writer drain it, and
/// the time limit stops a laptop that is not reading from holding the 1 Hz
/// tick hostage.
pub async fn emit_patiently(bytes: &[u8], patience: Duration) -> bool {
    let give_up = Instant::now() + patience;
    loop {
        let fits = critical_section::with(|_| OUT.free_capacity() >= bytes.len());
        if fits {
            return emit(bytes);
        }
        if Instant::now() >= give_up {
            DROPS.fetch_add(1, Relaxed);
            return false;
        }
        Timer::after_millis(5).await;
    }
}

/// Serialize `value` as one JSON line.
pub fn line<T: Serialize>(value: &T) {
    // The hub status line with three links is ~900 bytes.
    let mut buf = [0u8; 1536];
    match serde_json_core::to_slice(value, &mut buf) {
        Ok(n) if n < buf.len() => {
            buf[n] = b'\n';
            emit(&buf[..=n]);
        }
        _ => {
            DROPS.fetch_add(1, Relaxed);
        }
    }
}

#[derive(Serialize)]
struct NewDrone {
    t: &'static str,
    id: u32,
    up_ms: u64,
}

/// `{"t":"new","id":n}` when a drone is first seen (the buzzer's USB twin).
pub fn event_new_drone(id: u32) {
    line(&NewDrone { t: "new", id, up_ms: now_ms() });
}

#[derive(Serialize)]
pub struct Msg<'a> {
    pub t: &'static str,
    pub what: &'a str,
}

/// `{"t":"<t>","what":"..."}`.
pub fn msg(t: &'static str, what: &str) {
    line(&Msg { t, what });
}

#[embassy_executor::task]
pub async fn writer(mut tx: UsbSerialJtagTx<'static, Async>) {
    let mut buf = [0u8; 256];
    loop {
        let n = OUT.read(&mut buf).await;
        let _ = tx.write_all(&buf[..n]).await;
        if OUT.is_empty() {
            // Ends the USB transfer so the host sees the line now.
            let _ = tx.flush().await;
        }
    }
}

/// Test point for the `p` command (and the demo's own position): 300 m south
/// of the demo circle centre, so bearings point north-ish at the drones.
pub fn test_point() -> (f64, f64) {
    let (lat, lon) = crate::demo_centre();
    (lat - 300.0 / 111_195.0, lon)
}

#[embassy_executor::task]
pub async fn commands(mut rx: UsbSerialJtagRx<'static, Async>, hub: &'static Hub) {
    let mut b = [0u8; 1];
    loop {
        if rx.read(&mut b).await.unwrap_or(0) == 0 {
            continue;
        }
        let now = now_ms();
        match b[0] {
            b'p' => {
                let (lat, lon) = test_point();
                hub.set_manual(lat, lon, Some(15.0), now);
                msg("cmd", "own position set to test point (manual)");
            }
            b'c' => {
                hub.clear_manual(now);
                msg("cmd", "manual own position cleared");
            }
            b'm' => {
                let on = cot::toggle();
                msg("cmd", if on { "cot on" } else { "cot off" });
            }
            b's' => {
                dump_state(hub, now).await;
                state_check(hub, now);
            }
            b'h' | b'?' => {
                hotspot::print_info();
                msg("help", "p=set test own pos, c=clear own pos, m=toggle CoT, s=state JSON, h=help");
            }
            b'\r' | b'\n' | b' ' => {}
            _ => msg("cmd", "unknown command (h for help)"),
        }
    }
}

/// The `/api/state` body on one USB line, built by the same code as HTTP.
/// Assembled on the heap first so it reaches the pipe in one piece (other
/// tasks emit lines too); bodies over the pipe size are refused, not split.
async fn dump_state(hub: &Hub, now: u64) {
    let mut w = api::StateWriter::new(hub, now);
    let mut piece = [0u8; tracker::json::MAX_LINE];
    let mut out: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
    while let Some(n) = w.next_piece(hub, &mut piece) {
        out.extend_from_slice(&piece[..n]);
        if out.len() > 8000 {
            msg("err", "state body too big for one USB line; use HTTP");
            return;
        }
    }
    out.push(b'\n');
    if !emit_patiently(&out, Duration::from_millis(500)).await {
        msg("err", "USB busy; state dump dropped");
    }
}

#[derive(Serialize)]
struct StateCheck {
    t: &'static str,
    /// Streamed body == `tracker::json::state` output, byte for byte.
    identical: bool,
    bytes: usize,
    tracks: usize,
}

/// Compare the streamed `/api/state` body with `tracker::json::state` (see
/// `api.rs`) and report on USB. Skipped above a few tracks (heap).
pub fn state_check(hub: &Hub, now: u64) {
    match api::check_against_reference(hub, now) {
        Some((identical, bytes)) => {
            line(&StateCheck { t: "state_check", identical, bytes, tracks: hub.tracker.borrow().len() })
        }
        None => msg("state_check", "skipped: too many tracks to hold the reference copy"),
    }
}
