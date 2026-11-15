//! Wi-Fi Remote ID listener: Node A (ESP32-S3, 2.4 GHz) and Node C (ESP32-C5,
//! 5 GHz) from one source tree.
//!
//! Shape: the driver's promiscuous callback copies management frames into a
//! queue (`rx`); `drain` pulls them out and runs the `rid` crate; `report`
//! prints JSON lines on USB and sends `link` frames to the hub; `main` ends up
//! as the channel hopper. Data frames are classified in the callback by the
//! `openlink` crate (open FPV / telemetry links) and only counted there;
//! `open_links` reports each stream at most once a second. Receive-only:
//! nothing here transmits on the air, except with the `espnow` feature, where
//! `uplink` sends hub frames to an ESP-NOW gateway instead of the UART.
#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

mod chip;
mod detect;
mod report;
mod role;
mod rx;
#[cfg(feature = "espnow")]
mod uplink;

use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

use embassy_executor::Spawner;
use embassy_time::{Duration, Instant, Ticker, with_timeout};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
#[cfg(not(feature = "espnow"))]
use esp_hal::uart::{Config as UartConfig, Uart};
use esp_radio::wifi::{SecondaryChannel, WifiController};
use rid::wifi::{BEACON_OUI, BEACON_OUI_TYPE, NAN_DEST};

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

/// Diagnostic log level. Off by default because USB carries machine-read JSON
/// lines; raise it (e.g. `Warn`) when debugging esp-radio itself.
const LOG_LEVEL: log::LevelFilter = log::LevelFilter::Off;

/// Frames that carried Remote ID.
static RID_FOUND: AtomicU32 = AtomicU32::new(0);
/// Frames longer than a slot.
static TRUNCATED: AtomicU32 = AtomicU32::new(0);
/// Failed `set_channel` calls.
static CHANNEL_ERRORS: AtomicU32 = AtomicU32::new(0);
/// Beacon element-walk fit, see `report::StatsLine::iefit`.
static IE_FIT: [AtomicU32; 3] = [const { AtomicU32::new(0) }; 3];
/// Management frames heard, and hopper visits, per channel number (1-177).
/// Shows the hop plan is really being followed.
static PER_CHANNEL: [AtomicU32; CHANNELS] = [const { AtomicU32::new(0) }; CHANNELS];
static VISITS: [AtomicU32; CHANNELS] = [const { AtomicU32::new(0) }; CHANNELS];
const CHANNELS: usize = 178;
/// Failed band changes (C5).
static BAND_ERRORS: AtomicU32 = AtomicU32::new(0);

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger(LOG_LEVEL);

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: chip::HEAP_BYTES);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    // Which images this board holds (OTA slots) and the job, if this image
    // was started by a role switch.
    let mut images = node_common::Images::scan(peripherals.FLASH, chip::OWN_ROLES);
    role::init(images.stored_job(), images.can);
    let names: heapless::Vec<Option<&'static str>, 8> =
        images.slots.iter().map(|s| s.image.map(nodeimage::Image::project)).collect();
    report::hello_json(&names);

    #[cfg(not(feature = "espnow"))]
    {
        // TX to the hub, RX for the hub's commands (`chip::hub_rx_pin!`).
        let (hub_rx, hub_tx) = Uart::new(peripherals.UART1, UartConfig::default().with_baudrate(chip::HUB_BAUD))
            .expect("hub UART config")
            .with_tx(chip::hub_tx_pin!(peripherals))
            .with_rx(chip::hub_rx_pin!(peripherals))
            .into_async()
            .split();
        spawner.spawn(report::hub_writer(hub_tx).expect("spawn hub_writer"));
        spawner.spawn(report::hub_reader(hub_rx).expect("spawn hub_reader"));
    }

    let mut wifi = WifiController::new(peripherals.WIFI, Default::default()).expect("Wi-Fi init");
    if let Err(what) = chip::configure_radio(&mut wifi) {
        // Keep going: on the S3 a failed country/filter call costs efficiency,
        // not reception. The error line says which call to look at.
        esp_println::println!(r#"{{"t":"err","what":"{what}"}}"#);
    }

    rx::init();
    let mut sniffer = wifi.sniffer();
    sniffer.set_receive_cb(rx::on_frame);
    sniffer.set_promiscuous_mode(true).expect("promiscuous mode");

    #[cfg(feature = "espnow")]
    spawner.spawn(uplink::run(wifi.esp_now()).expect("spawn uplink"));
    spawner.spawn(drain().expect("spawn drain"));
    spawner.spawn(stats().expect("spawn stats"));
    spawner.spawn(role::control(images).expect("spawn control"));
    if chip::OPEN_LINKS {
        spawner.spawn(open_links().expect("spawn open_links"));
    }

    // `sniffer` must stay alive: dropping it turns promiscuous mode off.
    hop_forever(&mut wifi).await
}

/// Walk the current channel plan forever; start over at once on a new plan
/// (`role::CHANGED`). Runs in `main`, which owns the controller.
async fn hop_forever(wifi: &mut WifiController<'_>) -> ! {
    // One error line per channel, then only the counter, so a channel the
    // driver rejects (e.g. 149 under the wrong country) is visible without
    // flooding the stream.
    let mut reported = [false; CHANNELS];
    let mut band: Option<bool> = None; // Some(true) = 5 GHz
    loop {
        let plan = role::current().plan;
        'plan: loop {
            for hop in &plan {
                let ch = hop.channel();
                #[cfg(feature = "espnow")]
                uplink::wait_idle().await;
                let five = ch > 14;
                if band != Some(five) {
                    match chip::set_band(wifi, ch) {
                        Ok(()) => band = Some(five),
                        Err(e) => {
                            BAND_ERRORS.fetch_add(1, Relaxed);
                            esp_println::println!(r#"{{"t":"err","what":"set_band","ch":{ch},"e":"{e:?}"}}"#);
                        }
                    }
                }
                match wifi.set_channel(ch, SecondaryChannel::None) {
                    Ok(()) => {
                        rx::TUNED_CHANNEL.store(ch, Relaxed);
                        if let Some(v) = VISITS.get(usize::from(ch)) {
                            v.fetch_add(1, Relaxed);
                        }
                    }
                    Err(e) => {
                        CHANNEL_ERRORS.fetch_add(1, Relaxed);
                        if let Some(seen @ false) = reported.get_mut(usize::from(ch)) {
                            *seen = true;
                            esp_println::println!(r#"{{"t":"err","what":"set_channel","ch":{ch},"e":"{e:?}"}}"#);
                        }
                    }
                }
                if with_timeout(Duration::from_millis(hop.dwell_ms()), role::CHANGED.wait()).await.is_ok() {
                    break 'plan;
                }
            }
        }
    }
}

/// Cheap test for "could this be Remote ID?", so `rid` only parses likely
/// frames: a NAN action frame to the Remote ID address, or a beacon containing
/// the ASD-STAN vendor OUI+type anywhere in its body.
fn maybe_rid(frame: &[u8]) -> bool {
    match frame[0] >> 4 {
        8 => {
            let needle = [BEACON_OUI[0], BEACON_OUI[1], BEACON_OUI[2], BEACON_OUI_TYPE];
            frame.get(36..).is_some_and(|body| body.windows(4).any(|w| w == needle))
        }
        13 => frame.get(4..10) == Some(&NAN_DEST[..]),
        _ => false,
    }
}

/// Field check of `chip::FCS_LEN` on every untruncated beacon: does the element
/// list end exactly at `sig_len - 4`, or at `sig_len`?
fn check_ie_fit(f: &rx::RxFrame) {
    // Beacon: 24-byte header + 12 bytes fixed fields, then elements.
    if f.data[0] != 0x80 || f.truncated() || f.raw_len < 40 {
        return;
    }
    let ends_at = |end: usize| {
        let mut ies = &f.data[36..end];
        while let [_, len, rest @ ..] = ies {
            match rest.get(usize::from(*len)..) {
                Some(after) => ies = after,
                None => return false,
            }
        }
        ies.is_empty()
    };
    let raw = usize::from(f.raw_len);
    let idx = match (ends_at(raw - 4), ends_at(raw)) {
        (true, false) => 0,
        (false, true) => 1,
        _ => 2,
    };
    IE_FIT[idx].fetch_add(1, Relaxed);
}

#[embassy_executor::task]
async fn drain() {
    // Declared outside the loop so it lives in the task's static state.
    let mut json = [0u8; report::JSON_BYTES];
    let mut detect = detect::Detect::new();
    loop {
        let f = rx::next().await;
        if f.truncated() {
            TRUNCATED.fetch_add(1, Relaxed);
        }
        check_ie_fit(&f);
        if let Some(c) = PER_CHANNEL.get(usize::from(f.channel)) {
            c.fetch_add(1, Relaxed);
        }

        let frame = f.mpdu();
        if frame.len() < 24 {
            continue;
        }
        detect.frame(&f, frame);
        if !maybe_rid(frame) {
            continue;
        }
        let Some(found) = rid::wifi::extract(frame) else { continue };
        RID_FOUND.fetch_add(1, Relaxed);

        let mut messages = report::Messages::new();
        let error = match rid::message::raw_messages(found.payload) {
            Ok(iter) => {
                for raw in iter {
                    // Cannot overflow: a pack holds at most MAX_PACK messages.
                    let _ = messages.push(*raw);
                }
                None
            }
            Err(e) => Some(e),
        };
        let hit = report::Hit {
            transport: found.transport,
            channel: f.channel,
            rssi: f.rssi,
            mac: found.src,
            counter: found.counter,
            up_us: f.up_us,
            messages: &messages,
            error,
        };
        report::detection_json(&hit, &mut json);
        report::detection_hub(&hit);
    }
}

/// Shortest gap between two reports of one open-link stream. A video
/// downlink is hundreds of frames a second; one line a second, carrying the
/// running count, says everything the hub shows.
const OPEN_REPORT_GAP_US: u64 = 1_000_000;

/// Report open-link streams that have new frames, each at most once per
/// `OPEN_REPORT_GAP_US`. Checked four times a second, so a new link is
/// reported within 250 ms of its first frame.
#[embassy_executor::task]
async fn open_links() {
    let mut ticker = Ticker::every(Duration::from_millis(250));
    loop {
        ticker.next().await;
        let now = Instant::now().as_micros();
        // Copy out under the lock (the callback writes the table); format
        // and send after releasing it.
        let mut due: heapless::Vec<openlink::Sighting, { rx::OPEN_STREAMS }> = heapless::Vec::new();
        critical_section::with(|cs| {
            rx::SIGHTINGS.borrow_ref_mut(cs).take_due(now, OPEN_REPORT_GAP_US, |s| {
                // Cannot overflow: the table holds OPEN_STREAMS entries.
                let _ = due.push(*s);
            })
        });
        for s in &due {
            report::open_json(s);
            report::open_hub(s);
        }
    }
}

/// Stats every second on both outputs; the hub also gets Hello + Status every
/// 5 s so a hub that restarts learns who is attached and what it can do.
#[embassy_executor::task]
async fn stats() {
    let mut ticker = Ticker::every(Duration::from_secs(1));
    let mut last_seen = 0;
    report::hello_hub();
    let mut tick: u32 = 0;
    loop {
        ticker.next().await;
        tick = tick.wrapping_add(1);
        let seen = rx::MGMT_SEEN.load(Relaxed);
        let job = role::current();
        let mut per_ch: heapless::Vec<(u8, u32, u32), 24> = heapless::Vec::new();
        for h in &job.plan {
            let ch = h.channel();
            if per_ch.iter().all(|p| p.0 != ch) {
                let i = usize::from(ch);
                let _ = per_ch.push((ch, PER_CHANNEL[i].load(Relaxed), VISITS[i].load(Relaxed)));
            }
        }
        let line = report::StatsLine {
            t: "stats",
            up_us: Instant::now().as_micros(),
            role: job.role,
            plan_id: job.id,
            cmds: [role::APPLIED.load(Relaxed), role::REPEATS.load(Relaxed), role::REJECTED.load(Relaxed)],
            ch: rx::TUNED_CHANNEL.load(Relaxed),
            seen,
            fps: seen.wrapping_sub(last_seen),
            data: rx::DATA_SEEN.load(Relaxed),
            open: rx::OPEN_SEEN.load(Relaxed),
            open_ev: critical_section::with(|cs| rx::SIGHTINGS.borrow_ref(cs).evicted),
            rid: RID_FOUND.load(Relaxed),
            drop: rx::DROPPED.load(Relaxed),
            qmax: rx::HIGH_WATER.swap(0, Relaxed),
            other: rx::OTHER.load(Relaxed),
            trunc: TRUNCATED.load(Relaxed),
            ch_err: CHANNEL_ERRORS.load(Relaxed),
            hub_drop: report::HUB_DROPPED.load(Relaxed),
            iefit: IE_FIT.each_ref().map(|c| c.load(Relaxed)),
            per_ch,
            band_err: BAND_ERRORS.load(Relaxed),
            dji: detect::DJI.load(Relaxed),
            wdrone: detect::WIFI_DRONES.load(Relaxed),
        };
        last_seen = seen;
        report::stats_json(&line);
        report::stats_hub(&line);
        if tick.is_multiple_of(5) {
            report::hello_hub();
        }
    }
}
