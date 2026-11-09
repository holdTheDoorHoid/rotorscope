//! The 1 Hz heartbeat: housekeeping, the seams, and the USB stream (O4).
//!
//! Each second: refresh own position, expire tracks, hand out new-drone
//! alerts, render the screen, log to SD, then send one `{"t":"track",...}`
//! line per track and one `{"t":"hub",...}` status line on USB.

use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

use embassy_time::{Duration, Instant, Ticker, Timer};
use esp_hal::rtc_cntl::{Rtc, RwdtStage};
use serde::Serialize;
use tracker::TrackId;
use tracker::json::{self, MAX_LINE};

use crate::api::OwnJson;
use crate::hub::{Hub, MAX_TRACKS, now_ms};
use crate::pins::NODE_LINKS;
use crate::{clock, cot, hotspot, http, links, panel, screen, sd, usb};

/// Worst lateness of the thread-mode executor, microseconds (reset each second).
static LAG_MAX_US: AtomicU32 = AtomicU32::new(0);

/// Wakes every 10 ms and records how late it woke: how long the busiest
/// stretch of thread-mode work kept everything else waiting.
#[embassy_executor::task]
pub async fn lag_probe() {
    loop {
        let t0 = Instant::now();
        Timer::after_millis(10).await;
        let late = Instant::now().duration_since(t0).as_micros().saturating_sub(10_000);
        LAG_MAX_US.fetch_max(late as u32, Relaxed);
    }
}

#[derive(Serialize)]
struct LinkLine {
    role: Option<link::NodeRole>,
    alive: bool,
    bytes: u32,
    frames: u32,
    corrupt: u32,
    ovf: u32,
    uart_err: u32,
    qfull: u32,
    seen_ps: Option<u32>,
    /// Remote ID frames per second: the node's own count, and what reached the hub.
    rid_ps_node: Option<u32>,
    rid_ps_hub: u32,
    drops: u32,
}

#[derive(Serialize)]
struct HubLine {
    t: &'static str,
    up_ms: u64,
    utc_ms: Option<u64>,
    utc_src: Option<&'static str>,
    tracks: usize,
    active: usize,
    own: Option<OwnJson>,
    links: heapless::Vec<LinkLine, NODE_LINKS>,
    heap_used: usize,
    heap_free: usize,
    /// Lowest `heap_free` seen since boot.
    heap_min_free: usize,
    usb_drops: u32,
    cot: bool,
    cot_sent: u32,
    cot_no_utc: u32,
    cot_err: u32,
    http_req: u32,
    /// This tick's housekeeping + USB dump, microseconds.
    tick_us: u32,
    /// Longest CoT pass in the last second, microseconds.
    cot_us: u32,
    /// Worst thread-mode executor lateness in the last second, microseconds.
    lag_max_us: u32,
    ssid: &'static str,
}

/// Watchdog timeout. The tick feeds it; if the thread-mode executor hangs
/// for this long the chip resets (visible on USB as a fresh boot line),
/// instead of an unattended box silently showing nothing.
const WATCHDOG: esp_hal::time::Duration = esp_hal::time::Duration::from_secs(10);

#[embassy_executor::task]
pub async fn task(hub: &'static Hub, mut rtc: Rtc<'static>) {
    rtc.rwdt.set_timeout(RwdtStage::Stage0, WATCHDOG);
    rtc.rwdt.enable();
    let mut ticker = Ticker::every(Duration::from_secs(1));
    let mut line = [0u8; MAX_LINE];
    let mut heap_min_free = usize::MAX;
    let mut last_tick = 0u64;
    loop {
        ticker.next().await;
        rtc.rwdt.feed();
        let t0 = Instant::now();
        let now = now_ms();

        for n in hub.nodes.borrow_mut().iter_mut() {
            n.roll_rate();
        }
        hub.refresh_own(now);
        hub.tracker.borrow_mut().expire(now);
        hub.announce_new(now);
        let ctx = hub.context(now);

        screen::render(&ui_state(hub, now));
        sd::log_tick(&hub.tracker.borrow(), &ctx, last_tick);
        last_tick = now;

        // Track lines. Formatted one at a time with the tracker borrowed only
        // while formatting; a laptop that stops reading costs at most 50 ms
        // per line before lines are dropped (counted in usb_drops).
        let ids: heapless::Vec<TrackId, MAX_TRACKS> = hub.tracker.borrow().tracks().iter().map(|t| t.id).collect();
        for id in ids {
            let n = {
                let tracker = hub.tracker.borrow();
                let Some(t) = tracker.get(id) else { continue };
                json::track_line(t, &ctx, &mut line)
            };
            if let Ok(n) = n
                && !usb::emit_patiently(&line[..n], Duration::from_millis(50)).await
            {
                break;
            }
            // A track line takes ~3 ms to format on the S3 (f64 formatting is
            // soft-float on Xtensa); yield so 64 of them are not one 200 ms
            // stretch with the network stack and HTTP waiting.
            embassy_futures::yield_now().await;
        }

        let heap_free = esp_alloc::HEAP.free();
        heap_min_free = heap_min_free.min(heap_free);
        let nodes = hub.nodes.borrow();
        let status = HubLine {
            t: "hub",
            up_ms: now,
            utc_ms: ctx.utc_ms,
            utc_src: clock::source(now),
            tracks: hub.tracker.borrow().len(),
            active: hub.tracker.borrow().active_count(now),
            own: hub.own_pos(now).map(|p| OwnJson::of(&p, now)),
            links: (0..NODE_LINKS)
                .map(|i| {
                    let s = &links::STATS[i];
                    let n = &nodes[i];
                    LinkLine {
                        role: n.role,
                        alive: n.alive(now),
                        bytes: s.bytes.load(Relaxed),
                        frames: s.frames.load(Relaxed),
                        corrupt: s.corrupt.load(Relaxed),
                        ovf: s.overflow.load(Relaxed),
                        uart_err: s.uart_err.load(Relaxed),
                        qfull: s.queue_full.load(Relaxed),
                        seen_ps: n.seen_per_s,
                        rid_ps_node: n.node_rid_per_s,
                        rid_ps_hub: n.rid_per_s_hub,
                        drops: hub.link_drops(i),
                    }
                })
                .collect(),
            heap_used: esp_alloc::HEAP.used(),
            heap_free,
            heap_min_free,
            usb_drops: usb::DROPS.load(Relaxed),
            cot: cot::enabled(),
            cot_sent: cot::SENT.load(Relaxed),
            cot_no_utc: cot::NO_UTC.load(Relaxed),
            cot_err: cot::ERRORS.load(Relaxed),
            http_req: http::REQUESTS.load(Relaxed),
            tick_us: Instant::now().duration_since(t0).as_micros() as u32,
            cot_us: cot::TICK_MAX_US.swap(0, Relaxed),
            lag_max_us: LAG_MAX_US.swap(0, Relaxed),
            ssid: hotspot::identity().ssid.as_str(),
        };
        drop(nodes);
        usb::line(&status);
    }
}

/// What the screen would show now.
fn ui_state(hub: &Hub, now: u64) -> screen::UiState {
    let tracker = hub.tracker.borrow();
    let id = hotspot::identity();
    let nearest = tracker.nearest(now).and_then(|(t, g)| {
        let rb = g.drone?;
        let mut name = heapless::String::new();
        if let Some(uas) = t.primary_id() {
            let _ = name.push_str(uas.id.as_str().unwrap_or("?"));
        }
        Some(screen::Nearest { id: name, dist_m: rb.distance_m as f32, brg_deg: rb.bearing_deg as f32 })
    });
    screen::UiState {
        now_ms: now,
        active_drones: tracker.active_count(now) as u16,
        total_tracks: tracker.len() as u16,
        nearest,
        new_drone_alert: false,
        own_source: hub.own_pos(now).map(|p| match p.source {
            crate::hub::OwnSource::Gps => "gps",
            crate::hub::OwnSource::Manual => "manual",
        }),
        nodes_alive: hub.nodes.borrow().iter().map(|n| n.alive(now)).collect(),
        ssid: id.ssid.clone(),
        password: id.password.clone(),
        battery_mv: panel::battery_mv(),
        recording: sd::recording(),
    }
}
