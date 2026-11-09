//! Wi-Fi Remote ID listener: Node A (ESP32-S3, 2.4 GHz) and Node C (ESP32-C5,
//! 5 GHz) from one source tree.
//!
//! Shape: the driver's promiscuous callback copies management frames into a
//! queue (`rx`); `drain` pulls them out and runs the `rid` crate; `report`
//! prints JSON lines on USB and sends `link` frames to the hub; `main` ends up
//! as the channel hopper. Receive-only: nothing here transmits on the air.
#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

mod chip;
mod report;
mod rx;

use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

use embassy_executor::Spawner;
use embassy_time::{Duration, Instant, Ticker, Timer};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::uart::{Config as UartConfig, UartTx};
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
/// Management frames per channel, indexed by the channel's first position in
/// `chip::PLAN`. Shows the hop plan is really being followed.
static PER_CHANNEL: [AtomicU32; MAX_PLAN] = [const { AtomicU32::new(0) }; MAX_PLAN];
const MAX_PLAN: usize = 16;
const _: () = assert!(chip::PLAN.len() <= MAX_PLAN);

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger(LOG_LEVEL);

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: chip::HEAP_BYTES);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    report::hello_json();

    let hub_uart = UartTx::new(peripherals.UART1, UartConfig::default().with_baudrate(chip::HUB_BAUD))
        .expect("hub UART config")
        .with_tx(chip::hub_tx_pin!(peripherals))
        .into_async();
    spawner.spawn(report::hub_writer(hub_uart).expect("spawn hub_writer"));

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

    spawner.spawn(drain().expect("spawn drain"));
    spawner.spawn(stats().expect("spawn stats"));

    // `sniffer` must stay alive: dropping it turns promiscuous mode off.
    hop_forever(&mut wifi).await
}

/// Walk the channel plan forever. Runs in `main`, which owns the controller.
async fn hop_forever(wifi: &mut WifiController<'_>) -> ! {
    // One error line per plan entry, then only the counter, so a channel the
    // driver rejects (e.g. 149 under the wrong country) is visible without
    // flooding the stream.
    let mut reported = [false; MAX_PLAN];
    loop {
        for (i, hop) in chip::PLAN.iter().enumerate() {
            let ch = hop.channel();
            match wifi.set_channel(ch, SecondaryChannel::None) {
                Ok(()) => rx::TUNED_CHANNEL.store(ch, Relaxed),
                Err(e) => {
                    CHANNEL_ERRORS.fetch_add(1, Relaxed);
                    if let Some(seen @ false) = reported.get_mut(i) {
                        *seen = true;
                        esp_println::println!(r#"{{"t":"err","what":"set_channel","ch":{ch},"e":"{e:?}"}}"#);
                    }
                }
            }
            Timer::after(Duration::from_millis(hop.dwell_ms())).await;
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
    loop {
        let f = rx::next().await;
        if f.truncated() {
            TRUNCATED.fetch_add(1, Relaxed);
        }
        check_ie_fit(&f);
        if let Some(i) = chip::PLAN.iter().position(|h| h.channel() == f.channel) {
            PER_CHANNEL[i].fetch_add(1, Relaxed);
        }

        let frame = f.mpdu();
        if frame.len() < 24 || !maybe_rid(frame) {
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

/// Stats every second on both outputs; the hub also gets Hello every 5 s so a
/// hub that restarts learns who is attached.
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
        let line = report::StatsLine {
            t: "stats",
            up_us: Instant::now().as_micros(),
            ch: rx::TUNED_CHANNEL.load(Relaxed),
            seen,
            fps: seen.wrapping_sub(last_seen),
            rid: RID_FOUND.load(Relaxed),
            drop: rx::DROPPED.load(Relaxed),
            qmax: rx::HIGH_WATER.swap(0, Relaxed),
            other: rx::OTHER.load(Relaxed),
            trunc: TRUNCATED.load(Relaxed),
            ch_err: CHANNEL_ERRORS.load(Relaxed),
            hub_drop: report::HUB_DROPPED.load(Relaxed),
            iefit: IE_FIT.each_ref().map(|c| c.load(Relaxed)),
            per_ch: chip::PLAN
                .iter()
                .enumerate()
                .filter(|&(i, h)| chip::PLAN[..i].iter().all(|p| p.channel() != h.channel()))
                .map(|(i, h)| (h.channel(), PER_CHANNEL[i].load(Relaxed)))
                .collect(),
        };
        last_seen = seen;
        report::stats_json(&line);
        report::stats_hub(&line);
        if tick.is_multiple_of(5) {
            report::hello_hub();
        }
    }
}
