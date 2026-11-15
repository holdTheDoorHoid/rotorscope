//! The feed to the web head: a second ESP32 board (the old S3 hub today, a
//! C5 later) that runs the phone hotspot on channel 11, the phone page, TAK
//! CoT and station mode, which the P4 cannot (it has no radio of its own;
//! see PARITY.md and WEBHEAD.md).
//!
//! One wire: **UART2 TX on GPIO31** (a wireless-module socket pin, free while
//! no radio module is fitted) to the web head's node-link RX, plus ground,
//! 921600 baud, the same `link` frames the nodes use. Nothing comes back:
//! the web head never commands nodes (coverage stays on the P4).
//!
//! What goes down the wire:
//! - every frame decoded from the gateway, unchanged, `Via` markers included
//!   (`link::forward::Forwarder` keeps each `Via` with its frame), so the
//!   web head's tracker sees exactly what the P4's does, node by node;
//! - every ADSBee report, re-printed as the CSBee line it came from
//!   (`Frame::Csbee`, protocol 5);
//! - once a second `Frame::HubFix` (protocol 5: GPS state, the latest usable
//!   fix, UTC) and the P4's own `Stats`; every 5 s its `Hello` (role
//!   `Gateway`: to the web head the P4 is a gateway that relays nodes).
//!
//! Sending never waits: frames go into an 8 KB queue (~90 ms of the wire at
//! full speed) and [`Feed::pump`] tops up the 128-byte TX FIFO whenever the
//! main loop passes by. A frame that does not fit is dropped whole and
//! counted (`dropped`). With nothing connected the bytes simply leave the pin.

// `selftest` and `gpsloop` take UART2, so those bench builds have no feed.
#![cfg_attr(any(feature = "selftest", feature = "gpsloop"), allow(dead_code))]

use esp_hal::Blocking;
use esp_hal::uart::{Config, Uart};
use link::forward::Forwarder;
use link::{Frame, GnssState, HubFix, HubPosition, NodeRole};
use tracker::json::GpsState;

use crate::gps::Gps;

/// The P4 pin the web head listens to.
pub const TX_PIN: u8 = 31;
pub const BAUD: u32 = crate::links::BAUD;
const QUEUE: usize = 8192;
const HELLO_EVERY_MS: u64 = 5_000;

const _: () = assert!(link::MAX_CSBEE_LINE == adsb::MAX_LINE);

pub struct Feed {
    uart: Uart<'static, Blocking>,
    pub fwd: Forwarder<QUEUE>,
    /// Bytes written to the TX FIFO.
    pub tx_bytes: u32,
    /// ADSBee reports that would not re-print as a line (cannot happen for
    /// parsed ones; counted so it would show).
    pub csbee_err: u32,
    last_hello: Option<u64>,
    /// Remote ID detections relayed, for the P4's own `Stats`.
    detections: u32,
}

impl Feed {
    pub fn new(uart: esp_hal::peripherals::UART2<'static>, tx: impl esp_hal::gpio::interconnect::PeripheralOutput<'static>) -> Self {
        let uart = Uart::new(uart, Config::default().with_baudrate(BAUD)).expect("UART2 (web head)").with_tx(tx);
        Self { uart, fwd: Forwarder::new(), tx_bytes: 0, csbee_err: 0, last_hello: None, detections: 0 }
    }

    /// One frame of the gateway's stream, in order (`Via` included).
    pub fn relay(&mut self, f: &Frame) {
        if matches!(f, Frame::Detection(_)) {
            self.detections = self.detections.wrapping_add(1);
        }
        self.fwd.relay(f);
    }

    /// The gateway stream had an error: a held `Via` names nothing now.
    pub fn stream_error(&mut self) {
        self.fwd.reset_via();
    }

    /// One ADSBee report, as its CSBee line.
    pub fn adsb(&mut self, r: &adsb::Report) {
        let mut line = [0u8; adsb::MAX_LINE];
        let n = match r {
            adsb::Report::Aircraft(a) => adsb::encode::aircraft(a, &mut line),
            adsb::Report::Stats(s) => adsb::encode::stats(s, &mut line),
        };
        match n.and_then(|n| heapless::Vec::from_slice(&line[..n]).ok()) {
            Some(v) => self.fwd.send(&Frame::Csbee(v)),
            None => self.csbee_err += 1,
        }
    }

    /// Once a second: the P4's GPS, its health as a "node", and every 5 s
    /// its Hello. `gw_frames` and `gw_lost` are the gateway link's totals.
    pub fn tick(&mut self, gps: &Gps, fix_max_age_ms: u64, gw_frames: u32, gw_lost: u32, now: u64) {
        if self.last_hello.is_none_or(|t| now.saturating_sub(t) >= HELLO_EVERY_MS) {
            self.last_hello = Some(now);
            let mut fw = heapless::String::new();
            let _ = fw.push_str(concat!("hub-p4 ", env!("CARGO_PKG_VERSION")));
            self.fwd.send(&Frame::Hello { role: NodeRole::Gateway, protocol: link::PROTOCOL_VERSION, firmware: fw });
        }
        self.fwd.send(&Frame::Stats {
            uptime_us: now * 1000,
            frames_seen: gw_frames,
            rid_found: self.detections,
            // What the P4 lost: on the gateway wire, and here for want of room.
            dropped: gw_lost.wrapping_add(self.fwd.dropped),
            by_transport: heapless::Vec::new(),
        });
        self.fwd.send(&Frame::HubFix(hub_fix(gps, fix_max_age_ms, now)));
        self.pump();
    }

    /// Move queued bytes into the TX FIFO, as many as fit right now.
    pub fn pump(&mut self) {
        while !self.fwd.is_empty() && self.uart.write_ready() {
            let chunk = self.fwd.peek();
            // `write` only waits when the FIFO is full; write_ready says it is not.
            let Ok(w) = self.uart.write(&chunk[..chunk.len().min(64)]) else { break };
            self.fwd.consume(w);
            self.tx_bytes = self.tx_bytes.wrapping_add(w as u32);
        }
    }
}

/// The P4's GPS as a protocol 5 frame. Only a real fix is sent: a manual
/// position typed on the P4's console stays on the P4 (the web head has the
/// phone page's own form for that).
pub fn hub_fix(gps: &Gps, fix_max_age_ms: u64, now: u64) -> HubFix {
    let state = match gps.status.state {
        GpsState::Absent => GnssState::Absent,
        GpsState::Searching => GnssState::Searching,
        GpsState::Fix2d => GnssState::Fix2d,
        GpsState::Fix3d => GnssState::Fix3d,
    };
    let position = gps.fresh_fix(now, fix_max_age_ms).map(|f| HubPosition {
        lat_e7: round(f.lat * 1e7),
        lon_e7: round(f.lon * 1e7),
        alt_hae_mm: f.alt_hae_m.map(|a| round(a * 1000.0)),
        age_ms: now.saturating_sub(f.at_ms).min(u64::from(u32::MAX)) as u32,
    });
    HubFix {
        state,
        sats: gps.status.sats,
        position,
        utc_ms: gps.utc_ms(now),
        utc_pps: gps.time.is_some_and(|t| t.pps),
    }
}

/// Nearest integer (no `f64::round` in `core`); saturates like `as`.
fn round(x: f64) -> i32 {
    (if x < 0.0 { x - 0.5 } else { x + 0.5 }) as i32
}
