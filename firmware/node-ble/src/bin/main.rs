//! Node B firmware entry point: wiring only. Logic lives in the library
//! (`adv` = report intake, `out` = USB/UART output).
#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

use bt_hci::controller::ExternalController;
use bt_hci::param::FilterDuplicates;
use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_time::{Duration, Instant, Ticker, Timer};
use embedded_io_async::Write as _;
use esp_backtrace as _;
use esp_hal::Async;
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
#[cfg(not(feature = "espnow"))]
use esp_hal::uart::{self, Uart, UartRx, UartTx};
use esp_hal::usb::usb_serial_jtag::{UsbSerialJtag, UsbSerialJtagTx};
use esp_radio::ble::controller::BleConnector;
use log::{error, warn};
use node_ble::adv::{self, COUNTERS, bump};
use node_ble::out::{self, HelloLine, Mac, RidErrLine, RidLine, ScanInfo, StatsLine};
use node_ble::{FIRMWARE, SCAN_INTERVAL_MS, SCAN_WINDOW_MS, chip, hub, role};
use trouble_host::prelude::*;

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

/// Unattended node: after a panic (backtrace already printed to USB), reboot
/// rather than sit halted until someone power-cycles the box.
#[unsafe(no_mangle)]
extern "Rust" fn custom_halt() -> ! {
    // Give the USB host a moment to collect the backtrace text.
    esp_hal::delay::Delay::new().delay_millis(500);
    esp_hal::system::software_reset()
}

fn hello_line<'a>(images: &'a [Option<&'static str>]) -> HelloLine<'a> {
    HelloLine {
        t: "hello",
        node: "ble",
        fw: FIRMWARE,
        chip: chip::NAME,
        proto: link::PROTOCOL_VERSION,
        scan: ScanInfo {
            phys: "1M+Coded",
            interval_ms: SCAN_INTERVAL_MS,
            window_ms: SCAN_WINDOW_MS,
            active: false,
            dup_filter: false,
        },
        uart_baud: hub::BAUD,
        espnow: cfg!(feature = "espnow"),
        can: role::CAN.load(core::sync::atomic::Ordering::Relaxed),
        images,
        plan_id: role::PLAN_ID.load(core::sync::atomic::Ordering::Relaxed),
    }
}

fn hello_frame() -> link::Frame {
    let mut firmware = heapless::String::new();
    let _ = firmware.push_str(&FIRMWARE[..FIRMWARE.len().min(16)]);
    link::Frame::Hello { role: link::NodeRole::Ble, protocol: link::PROTOCOL_VERSION, firmware }
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    node_ble::logger::init();

    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));

    // The BLE controller allocates its event buffers from this heap.
    #[cfg(feature = "esp32s3")]
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 73744);
    #[cfg(feature = "esp32c5")]
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 65536);
    // Headroom: esp-radio queues every HCI event on the heap until the host
    // reads it, so a burst of adverts needs more than the controller's own use.
    esp_alloc::heap_allocator!(size: 32 * 1024);
    // The Wi-Fi driver (ESP-NOW) wants its own buffers on top.
    #[cfg(feature = "espnow")]
    esp_alloc::heap_allocator!(size: 48 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    // USB: the laptop / DragonSync side.
    let (_, usb_tx) = UsbSerialJtag::new(peripherals.USB_DEVICE).into_async().split();
    spawner.spawn(usb_task(usb_tx).unwrap());

    // Which images this board holds (OTA slots) and the job, if this image
    // was started by a role switch.
    let mut images = node_common::Images::scan(peripherals.FLASH, link::NodeRole::Ble.bit());
    role::init(images.stored_job(), images.can);
    let names: heapless::Vec<Option<&'static str>, 8> =
        images.slots.iter().map(|s| s.image.map(nodeimage::Image::project)).collect();

    // UART: the hub side (wired builds). TX to the hub, RX for its commands.
    #[cfg(not(feature = "espnow"))]
    {
        #[cfg(feature = "esp32s3")]
        let (hub_tx, hub_rx) = (peripherals.GPIO17, peripherals.GPIO18);
        #[cfg(feature = "esp32c5")]
        let (hub_tx, hub_rx) = (peripherals.GPIO11, peripherals.GPIO12);
        let (rx, tx) = Uart::new(peripherals.UART1, uart::Config::default().with_baudrate(hub::BAUD))
            .unwrap()
            .with_tx(hub_tx)
            .with_rx(hub_rx)
            .into_async()
            .split();
        spawner.spawn(uart_task(tx).unwrap());
        spawner.spawn(uart_rx_task(rx).unwrap());
    }

    // ESP-NOW: Wi-Fi parked on the gateway's channel, sharing the radio with
    // the Bluetooth scan (coexistence). Started before Bluetooth.
    #[cfg(feature = "espnow")]
    let mut wifi = {
        let mut wifi = esp_radio::wifi::WifiController::new(peripherals.WIFI, Default::default()).unwrap();
        if let Err(e) = wifi.set_band_mode(esp_radio::wifi::BandMode::_2_4G) {
            warn!("set_band_mode: {e:?}");
        }
        let now = wifi.esp_now();
        if let Err(e) = now.set_channel(node_ble::uplink::CHANNEL) {
            warn!("esp-now set_channel: {e:?}");
        }
        spawner.spawn(node_ble::uplink::run(now).unwrap());
        wifi
    };

    let mut line = [0u8; 512];
    out::json_line(&hello_line(&names), &mut line);
    out::hub_frame(&hello_frame());
    out::hub_frame(&role::status_frame());

    spawner.spawn(decode_task().unwrap());
    spawner.spawn(stats_task().unwrap());
    spawner.spawn(role::control(images).unwrap());

    let connector = BleConnector::new(peripherals.BT, Default::default()).unwrap();
    let controller: ExternalController<_, 4> = ExternalController::new(connector);
    let mut resources: HostResources<DefaultPacketPool, 1, 1> = HostResources::new();
    let stack = trouble_host::new(controller, &mut resources).build();
    let mut runner = stack.runner();
    let mut central = stack.central();
    let handler = adv::ScanHandler::new();

    let config = ScanConfig {
        // Passive: we never transmit, and scan requests would only add
        // traffic. Remote ID needs no scan response.
        active: false,
        phys: PhySet::M1Coded,
        interval: Duration::from_millis(SCAN_INTERVAL_MS),
        window: Duration::from_millis(SCAN_WINDOW_MS),
        // No controller-timed end, in either build. ESP-NOW builds stop the
        // scan from the host after SCAN_ON_MS (below). Letting the controller
        // end each scan by its duration crashed the C5 controller under
        // Long Range traffic: `ASSERT r_ble_lll_mmgmt_block_cnt_check:669`,
        // 4 resets in 120 s with the test beacon's three BLE transports on, 1
        // in 90 s with Long Range alone; host stop: 0 in 180 s (docs/BENCH.md,
        // "End-to-end through the P4").
        timeout: Duration::from_secs(0),
        // Off: a drone's repeated adverts carry fresh positions, and the
        // controller's duplicate filter would swallow all but the first.
        filter_duplicates: FilterDuplicates::Disabled,
        filter_accept_list: &[],
    };

    let host = async {
        if let Err(e) = runner.run_with_handler(&handler).await {
            error!("BLE host stopped: {e:?}");
        }
        // The stack cannot be restarted in place; a reset is the clean way back.
        Timer::after_millis(200).await;
        esp_hal::system::software_reset()
    };
    let scan = async {
        let mut scanner = Scanner::new(&mut central);
        loop {
            match scanner.scan_ext(&config).await {
                // With no timeout the session never completes; awaiting it
                // keeps it (and the scan) alive.
                #[cfg(not(feature = "espnow"))]
                Ok(session) => session.await,
                // ESP-NOW: scan for a while, then stop (dropping the session
                // stops the scan) and leave the radio to Wi-Fi for a moment.
                #[cfg(feature = "espnow")]
                Ok(session) => {
                    // The session never completes by itself; dropping it at
                    // the timeout disables the scan from the host.
                    let _ = embassy_time::with_timeout(Duration::from_millis(node_ble::uplink::SCAN_ON_MS), session).await;
                    node_ble::uplink::WIFI_TURN.store(true, core::sync::atomic::Ordering::Relaxed);
                    node_ble::uplink::GAPS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                    Timer::after_millis(node_ble::uplink::WIFI_GAP_MS).await;
                    node_ble::uplink::WIFI_TURN.store(false, core::sync::atomic::Ordering::Relaxed);
                }
                Err(e) => {
                    bump(&COUNTERS.scan_err);
                    warn!("scan_ext failed: {e:?}");
                    Timer::after_secs(1).await;
                }
            }
        }
    };
    join(host, scan).await;
    // The Wi-Fi controller must outlive the uplink (dropping it stops Wi-Fi).
    #[cfg(feature = "espnow")]
    let _ = &mut wifi;
    unreachable!()
}

#[embassy_executor::task]
async fn usb_task(mut tx: UsbSerialJtagTx<'static, Async>) {
    let mut buf = [0u8; 256];
    loop {
        let n = out::USB.read(&mut buf).await;
        let _ = tx.write_all(&buf[..n]).await;
        if out::USB.is_empty() {
            // Ends the USB transfer so the host sees the line now.
            let _ = tx.flush().await;
        }
    }
}

#[cfg(not(feature = "espnow"))]
#[embassy_executor::task]
async fn uart_rx_task(mut rx: UartRx<'static, Async>) {
    let mut dec = link::CommandDecoder::new();
    let mut buf = [0u8; 128];
    loop {
        let Ok(n) = rx.read_async(&mut buf).await else { continue };
        dec.feed(&buf[..n], |r| {
            if let Ok(link::Command::Assign(a)) = r {
                let _ = role::COMMANDS.try_send(a);
            }
        });
    }
}

#[cfg(not(feature = "espnow"))]
#[embassy_executor::task]
async fn uart_task(mut tx: UartTx<'static, Async>) {
    let mut buf = [0u8; 256];
    loop {
        let n = out::UART.read(&mut buf).await;
        let _ = tx.write_all(&buf[..n]).await;
    }
}

#[embassy_executor::task]
async fn decode_task() {
    // Held across awaits, so it lives in the task's static storage, not on
    // the stack.
    let mut line = [0u8; out::MAX_LINE];
    loop {
        let rep = adv::REPORTS.receive().await;
        // The pre-check is looser than `extract` (it accepts a structure
        // that runs off the end of a fragment); `None` here is that case.
        let Some(ble) = rid::ble::extract(rep.data()) else {
            continue;
        };
        bump(&COUNTERS.rid_found);
        let raws = match rid::message::raw_messages(ble.payload) {
            Ok(raws) => raws,
            Err(error) => {
                bump(&COUNTERS.rid_err);
                let l = RidErrLine {
                    t: "rid_err",
                    tr: rep.transport,
                    rssi: rep.rssi,
                    mac: Mac(rep.addr),
                    up_us: rep.uptime_us,
                    error,
                    len: rep.data().len(),
                    data: out::HexAll(rep.data()),
                };
                out::json_line(&l, &mut line);
                continue;
            }
        };
        let mut messages = heapless::Vec::new();
        for raw in raws {
            if messages.push(*raw).is_err() {
                break;
            }
        }
        let det = link::Detection {
            transport: rep.transport,
            // HCI reports do not say which advertising channel it was.
            channel: 0,
            rssi: rep.rssi,
            mac: rep.addr,
            counter: ble.counter,
            uptime_us: rep.uptime_us,
            messages,
        };
        out::json_line(&RidLine::new(&det, rep.truncated), &mut line);
        out::hub_frame(&link::Frame::Detection(det));
    }
}

/// Counter values at one instant; differences give per-second rates.
#[derive(Clone, Copy)]
struct Snapshot {
    adv: u32,
    legacy: u32,
    ext1m: u32,
    coded: u32,
    old: u32,
    rid: u32,
    rid_ok: u32,
    rid_err: u32,
    trunc: u32,
    drops: u32,
    usb_drops: u32,
    uart_drops: u32,
    hci_err: u32,
    scan_err: u32,
}

impl Snapshot {
    fn take() -> Self {
        use core::sync::atomic::Ordering::Relaxed;
        let c = &COUNTERS;
        Self {
            adv: c.adv.load(Relaxed),
            legacy: c.legacy.load(Relaxed),
            ext1m: c.ext_1m.load(Relaxed),
            coded: c.coded.load(Relaxed),
            old: c.old_reports.load(Relaxed),
            rid: c.rid_queued.load(Relaxed),
            rid_ok: c.rid_found.load(Relaxed),
            rid_err: c.rid_err.load(Relaxed),
            trunc: c.truncated.load(Relaxed),
            drops: c.drops.load(Relaxed),
            usb_drops: c.usb_drops.load(Relaxed),
            uart_drops: c.uart_drops.load(Relaxed),
            hci_err: c.hci_err.load(Relaxed),
            scan_err: c.scan_err.load(Relaxed),
        }
    }
}

#[embassy_executor::task]
async fn stats_task() {
    let mut line = [0u8; 768];
    let mut ticker = Ticker::every(Duration::from_secs(1));
    let mut prev = Snapshot::take();
    let mut last = Instant::now();
    let mut secs: u64 = 0;
    loop {
        ticker.next().await;
        secs += 1;
        let now = Instant::now();
        let cur = Snapshot::take();
        let d = |f: fn(&Snapshot) -> u32| f(&cur).wrapping_sub(f(&prev));
        let stats = StatsLine {
            t: "stats",
            up_us: now.as_micros(),
            win_ms: (now - last).as_millis() as u32,
            adv: d(|s| s.adv),
            legacy: d(|s| s.legacy),
            ext1m: d(|s| s.ext1m),
            coded: d(|s| s.coded),
            old: d(|s| s.old),
            rid: d(|s| s.rid),
            rid_ok: d(|s| s.rid_ok),
            rid_err: d(|s| s.rid_err),
            trunc: d(|s| s.trunc),
            drops: d(|s| s.drops),
            usb_drops: d(|s| s.usb_drops),
            uart_drops: d(|s| s.uart_drops),
            hci_err: d(|s| s.hci_err),
            scan_err: d(|s| s.scan_err),
            heap_free: esp_alloc::HEAP.free(),
            plan_id: role::PLAN_ID.load(core::sync::atomic::Ordering::Relaxed),
            cmds: {
                use core::sync::atomic::Ordering::Relaxed;
                [role::APPLIED.load(Relaxed), role::REPEATS.load(Relaxed), role::REJECTED.load(Relaxed)]
            },
            tot_adv: cur.adv,
            tot_rid: cur.rid,
            tot_drops: cur.drops,
        };
        out::json_line(&stats, &mut line);
        // The hub gets running totals: it can difference them itself, and a
        // lost frame then costs nothing.
        out::hub_frame(&link::Frame::Stats {
            uptime_us: now.as_micros(),
            frames_seen: cur.adv,
            rid_found: cur.rid_ok,
            dropped: cur.drops.wrapping_add(cur.uart_drops),
            by_transport: [
                (rid::Transport::BleLegacy, cur.legacy),
                (rid::Transport::BleExtended, cur.ext1m),
                (rid::Transport::BleLongRange, cur.coded),
            ]
            .into_iter()
            .collect(),
        });
        if secs.is_multiple_of(hub::HELLO_EVERY_S) {
            out::hub_frame(&hello_frame());
            out::hub_frame(&role::status_frame());
        }
        prev = cur;
        last = now;
    }
}
