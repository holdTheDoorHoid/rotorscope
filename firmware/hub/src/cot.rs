//! TAK Cursor-on-Target over UDP multicast (PLAN.md O1/O5).
//!
//! Once a second, every track with a position becomes a drone event, every
//! track with an operator position an operator event, and (feature `adsbee`)
//! every active aircraft with a position an aircraft event (`tracker::cot`;
//! an ADS-B "UAV" that is one of the drone tracks is skipped), sent to
//! 239.2.3.1:6969: ATAK's default "SA multicast" input, so a phone joined to
//! the hotspot shows the drones with no setup. One task per network (hotspot,
//! and the station LAN when built).
//!
//! CoT needs UTC (event time and stale time). Without it events are skipped and
//! counted in `NO_UTC`; see `clock.rs`.
//!
//! Runtime toggle: USB `m`, or `POST /api/cot {"on":bool}`. On by default.

use core::net::{Ipv4Addr, SocketAddrV4};
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};

use embassy_net::Stack;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_time::{Duration, Instant, Ticker};
use tracker::cot::{self, MAX_EVENT};
use tracker::{AircraftKey, FormatError, TrackId, TrackState};

use crate::hub::{Hub, MAX_AIRCRAFT, MAX_TRACKS, now_ms};

pub const GROUP: Ipv4Addr = Ipv4Addr::new(239, 2, 3, 1);
pub const PORT: u16 = 6969;

static ENABLED: AtomicBool = AtomicBool::new(true);
/// Datagrams sent.
pub static SENT: AtomicU32 = AtomicU32::new(0);
/// Events skipped because there is no UTC.
pub static NO_UTC: AtomicU32 = AtomicU32::new(0);
/// Send errors (no route, buffer).
pub static ERRORS: AtomicU32 = AtomicU32::new(0);
/// Longest 1 Hz CoT pass, microseconds (reset when read).
pub static TICK_MAX_US: AtomicU32 = AtomicU32::new(0);

pub fn enabled() -> bool {
    ENABLED.load(Relaxed)
}

pub fn set(on: bool) {
    ENABLED.store(on, Relaxed);
}

/// Flip the toggle; returns the new state.
pub fn toggle() -> bool {
    !ENABLED.fetch_xor(true, Relaxed)
}

#[embassy_executor::task(pool_size = 2)]
pub async fn task(stack: Stack<'static>, hub: &'static Hub) {
    let mut rx_meta = [PacketMetadata::EMPTY; 1];
    let mut rx_buf = [0u8; 64];
    // Two events (drone + operator) in flight at once.
    let mut tx_meta = [PacketMetadata::EMPTY; 4];
    let mut tx_buf = [0u8; 2 * MAX_EVENT];
    let mut socket = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    // Any local port: we only send.
    if socket.bind(0).is_err() {
        crate::usb::msg("err", "cot bind");
        return;
    }
    let dest = SocketAddrV4::new(GROUP, PORT);
    let mut event = [0u8; MAX_EVENT];
    let mut ticker = Ticker::every(Duration::from_secs(1));
    loop {
        ticker.next().await;
        if !enabled() || !stack.is_config_up() {
            continue;
        }
        let t0 = Instant::now();
        let now = now_ms();
        let ctx = hub.context(now);
        let ids: heapless::Vec<TrackId, MAX_TRACKS> = hub.tracker.borrow().tracks().iter().map(|t| t.id).collect();
        for id in ids {
            for which in [Which::Drone, Which::Operator] {
                // Borrow only while formatting; never across the send.
                let n = {
                    let tracker = hub.tracker.borrow();
                    let Some(t) = tracker.get(id) else { break };
                    match which {
                        Which::Drone => cot::drone_event(t, &ctx, &mut event)
                            .map(|n| with_flags(&mut event, n, &crate::flags::remark(&hub.flags.borrow(), t, now))),
                        Which::Operator => cot::operator_event(t, &ctx, &mut event),
                    }
                };
                match n {
                    Ok(n) => match socket.send_to(&event[..n], dest).await {
                        Ok(()) => {
                            SENT.fetch_add(1, Relaxed);
                        }
                        Err(_) => {
                            ERRORS.fetch_add(1, Relaxed);
                        }
                    },
                    Err(FormatError::NoTime) => {
                        NO_UTC.fetch_add(1, Relaxed);
                    }
                    // No position yet (or no operator position): nothing to plot.
                    Err(_) => {}
                }
                // ~1.5 ms per event to format; let other tasks in between.
                embassy_futures::yield_now().await;
            }
        }
        let aircraft: heapless::Vec<AircraftKey, MAX_AIRCRAFT> = hub
            .tracker
            .borrow()
            .aircraft()
            .iter()
            .filter(|a| ctx.aircraft_state(a) == TrackState::Active)
            .map(|a| a.key)
            .collect();
        for key in aircraft {
            let n = {
                let tracker = hub.tracker.borrow();
                let Some(a) = tracker.get_aircraft(key) else { continue };
                cot::aircraft_event(a, tracker.tracks(), &ctx, &mut event)
            };
            match n {
                Ok(n) => match socket.send_to(&event[..n], dest).await {
                    Ok(()) => {
                        SENT.fetch_add(1, Relaxed);
                    }
                    Err(_) => {
                        ERRORS.fetch_add(1, Relaxed);
                    }
                },
                Err(FormatError::NoTime) => {
                    NO_UTC.fetch_add(1, Relaxed);
                }
                // No position, or a drone already sent as itself.
                Err(_) => {}
            }
            embassy_futures::yield_now().await;
        }
        let us = Instant::now().duration_since(t0).as_micros() as u32;
        TICK_MAX_US.fetch_max(us, Relaxed);
    }
}

/// Insert the P4's possible-violation flags (if any) at the end of the
/// drone event's `<remarks>`, when they fit. Returns the new length.
fn with_flags(event: &mut [u8], n: usize, extra: &str) -> usize {
    const END: &[u8] = b"</remarks>";
    if extra.is_empty() || n + extra.len() > event.len() {
        return n;
    }
    let Some(at) = event[..n].windows(END.len()).position(|w| w == END) else { return n };
    // XML-escape is not needed: the flag words never contain '<' or '&'.
    event.copy_within(at..n, at + extra.len());
    event[at..at + extra.len()].copy_from_slice(extra.as_bytes());
    n + extra.len()
}

#[derive(Clone, Copy)]
enum Which {
    Drone,
    Operator,
}
