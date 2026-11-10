//! The 1 Hz heartbeat: housekeeping, the SD log rows, and the USB stream (O4).
//!
//! Each second: refresh own position (GPS, else the manual position,
//! re-applied so the tracker's 10 s fix timeout never drops it), expire
//! tracks, hand out new-drone alerts, queue one CSV row per active drone and
//! open FPV link for the SD card, then send one `{"t":"track",...}` line per
//! track, one `{"t":"link",...}` line per open link and one `{"t":"hub",...}`
//! status line on USB. The screen redraws on its own
//! (`screen::feeder`), not from here.

use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

use embassy_time::{Duration, Instant, Ticker, Timer};
use esp_hal::rtc_cntl::{Rtc, RwdtStage};
use serde::Serialize;
use tracker::openlinks::MAX_LINK_TRACKS;
use tracker::{AircraftKey, LinkTrackId, TrackId};
use tracker::json::{self, MAX_LINE};

use crate::api::OwnJson;
use crate::hub::{Hub, MAX_AIRCRAFT, MAX_TRACKS, now_ms};
use crate::pins::NODE_LINKS;
use crate::{adsbee, clock, cot, gnss, hotspot, http, links, panel, screen, sd, usb};

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
    /// Open FPV links in the table, and how many were heard recently.
    open_links: usize,
    open_active: usize,
    /// ADS-B aircraft in the table, and how many were heard recently
    /// (always 0 without the `adsbee` feature).
    aircraft: usize,
    aircraft_active: usize,
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
    gps: gnss::GpsLine,
    /// The ADSBee link; `null` when not built in.
    adsb: Option<adsbee::AdsbLine>,
    sd: sd::SdLine,
    screen: screen::ScreenLine,
    batt: panel::BattLine,
    buzz: u32,
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

        sd::log_tick(hub, &ctx).await;

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
        // Open FPV links, the same way (no floats in these; cheap).
        let links: heapless::Vec<LinkTrackId, MAX_LINK_TRACKS> =
            hub.tracker.borrow().open_links().iter().map(|l| l.id).collect();
        for id in links {
            let n = {
                let tracker = hub.tracker.borrow();
                let Some(l) = tracker.get_open_link(id) else { continue };
                json::link_line(l, &ctx, &mut line)
            };
            if let Ok(n) = n
                && !usb::emit_patiently(&line[..n], Duration::from_millis(50)).await
            {
                break;
            }
            embassy_futures::yield_now().await;
        }
        // ADS-B aircraft (feature `adsbee`; the table is empty otherwise).
        let aircraft: heapless::Vec<AircraftKey, MAX_AIRCRAFT> =
            hub.tracker.borrow().aircraft().iter().map(|a| a.key).collect();
        for key in aircraft {
            let n = {
                let tracker = hub.tracker.borrow();
                let Some(a) = tracker.get_aircraft(key) else { continue };
                json::aircraft_line(a, tracker.tracks(), &ctx, &mut line)
            };
            if let Ok(n) = n
                && !usb::emit_patiently(&line[..n], Duration::from_millis(50)).await
            {
                break;
            }
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
            open_links: hub.tracker.borrow().open_links().len(),
            open_active: hub.tracker.borrow().active_link_count(now),
            aircraft: hub.tracker.borrow().aircraft().len(),
            aircraft_active: hub.tracker.borrow().active_aircraft_count(now),
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
            gps: gnss::GpsLine::now(now),
            adsb: adsbee::AdsbLine::now(hub, now),
            sd: sd::SdLine::now(),
            screen: screen::ScreenLine::now(),
            batt: panel::BattLine::now(),
            buzz: panel::BUZZES.load(Relaxed),
        };
        drop(nodes);
        usb::line(&status);
    }
}
