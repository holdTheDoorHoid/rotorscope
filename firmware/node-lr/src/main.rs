//! The aero node: a receive-only Semtech LR2021 on an ESP32-C3, hearing
//! 1090 MHz extended squitter, 978 MHz UAT, or FLARM / OGN trackers
//! followed through their frequency hops, and sending the raw frames to the
//! hub (docs/AERO-NODE.md); and, when the plan says so, scanning for drone
//! control links (ExpressLRS, mLRS, SiK, OpenLRSng, energy of closed
//! systems; docs/CONTROL-LINKS.md, `control.rs`).
//!
//! ```text
//! LR2021 --SPI--> radio task (lr2021cfg: configs, checks; aerohop: hops)
//!                    |  Frame::Air, AeroStatus
//!                    v
//!               out::UART pipe --> UART / wired bus / ESP-NOW --> hub
//! hub --> Command::Assign (plan) / TimeSync (UTC) --> state
//! ```
//!
//! It never transmits on 1090, 978 or 902-928 MHz (or anywhere else) with
//! the LR2021: there is no transmit path to it. With `espnow` the ESP32-C3's
//! own Wi-Fi radio talks to the gateway on 2.4 GHz channel 6, like every
//! other ESP-NOW node.
#![no_std]
#![no_main]

#[cfg(all(feature = "wirebus", feature = "espnow"))]
compile_error!("`wirebus` and `espnow` are two ways to reach the hub; build one");

mod board;
mod control;
mod lr;
mod out;
mod radio;
mod state;
#[cfg(feature = "espnow")]
mod uplink;

use embassy_executor::Spawner;
use embassy_time::{Duration, Instant, Ticker};
use embedded_io_async::Write as _;
use esp_backtrace as _;
use esp_hal::Async;
use esp_hal::clock::CpuClock;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;
#[cfg(not(any(feature = "espnow", feature = "wirebus")))]
use esp_hal::uart::{self, Uart, UartRx, UartTx};
use esp_hal::usb::usb_serial_jtag::{UsbSerialJtag, UsbSerialJtagTx};
use portable_atomic::Ordering::Relaxed;

use crate::out::COUNTERS;

esp_bootloader_esp_idf::esp_app_desc!();

pub const FIRMWARE: &str = env!("CARGO_PKG_VERSION");
/// Hello / Status / AeroStatus every this many seconds.
const HELLO_EVERY_S: u64 = 5;

/// Unattended node: after a panic, reboot rather than sit halted.
#[unsafe(no_mangle)]
extern "Rust" fn custom_halt() -> ! {
    esp_hal::delay::Delay::new().delay_millis(500);
    esp_hal::system::software_reset()
}

/// Hang watchdog timeout (docs/BENCH.md "Crash recovery"). The once-a-second
/// stats loop in `main` feeds it, so it fires when the executor stops going
/// round (a task that never yields, an SPI wait that never ends, interrupts
/// left off). Not fed from the radio task: a control-scan plan entry may
/// legitimately keep it inside one call for the entry's whole length, and a
/// missing or deaf LR2021 is handled there already (`bring_up`, the 1090
/// receive watchdog).
const WATCHDOG: esp_hal::time::Duration = esp_hal::time::Duration::from_secs(10);

fn hello_frame() -> link::Frame {
    let mut firmware = heapless::String::new();
    let _ = firmware.push_str(&FIRMWARE[..FIRMWARE.len().min(16)]);
    link::Frame::Hello { role: link::NodeRole::Aero, protocol: link::PROTOCOL_VERSION, firmware }
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let p = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    // The Wi-Fi driver (ESP-NOW) allocates its buffers here; the rest of
    // the node allocates nothing.
    esp_alloc::heap_allocator!(size: 72 * 1024);

    let timg0 = TimerGroup::new(p.TIMG0);
    esp_rtos::start(timg0.timer0, p.FROM_CPU_INTR0);

    let (_usb_rx, usb_tx) = UsbSerialJtag::new(p.USB_DEVICE).into_async().split();
    spawner.spawn(usb_task(usb_tx).unwrap());
    #[cfg(feature = "faulttest")]
    spawner.spawn(faulttest(_usb_rx).unwrap());

    // The LR2021 (pins: src/board.rs).
    let spi = Spi::new(p.SPI2, SpiConfig::default().with_frequency(Rate::from_mhz(board::SPI_MHZ)))
        .expect("SPI config")
        .with_sck(p.GPIO4)
        .with_miso(p.GPIO5)
        .with_mosi(p.GPIO6);
    let nss = Output::new(p.GPIO7, Level::High, OutputConfig::default());
    let busy = Input::new(p.GPIO3, InputConfig::default().with_pull(Pull::None));
    let rst = Output::new(p.GPIO1, Level::High, OutputConfig::default());
    let irq = Input::new(p.GPIO10, InputConfig::default().with_pull(Pull::Down));
    let lr = lr::Lr::new(spi, nss, busy, rst, irq);

    out::line(format_args!(
        r#"{{"t":"hello","node":"lr","fw":"{FIRMWARE}","chip":"{}","proto":{},"espnow":{},"wirebus":{},"tx":"never on the LR2021","reset":"{}","faulttest":{}}}"#,
        board::CHIP,
        link::PROTOCOL_VERSION,
        cfg!(feature = "espnow"),
        cfg!(feature = "wirebus"),
        node_common::fault::reset_reason(),
        cfg!(feature = "faulttest"),
    ));

    // The hub link.
    #[cfg(not(any(feature = "espnow", feature = "wirebus")))]
    {
        let (rx, tx) = Uart::new(p.UART1, uart::Config::default().with_baudrate(board::HUB_BAUD))
            .unwrap()
            .with_tx(p.GPIO21)
            .with_rx(p.GPIO20)
            .into_async()
            .split();
        spawner.spawn(uart_tx_task(tx).unwrap());
        spawner.spawn(uart_rx_task(rx).unwrap());
    }
    #[cfg(feature = "wirebus")]
    {
        static EXEC: static_cell::StaticCell<esp_rtos::embassy::InterruptExecutor<2>> = static_cell::StaticCell::new();
        let exec = EXEC.init(esp_rtos::embassy::InterruptExecutor::new(p.FROM_CPU_INTR2));
        let bus = exec.start(esp_hal::interrupt::Priority::Priority2);
        bus.spawn(bus_task(p.UART1, p.GPIO0, p.GPIO20).unwrap());
    }
    #[cfg(feature = "espnow")]
    let _wifi = {
        let wifi = esp_radio::wifi::WifiController::new(p.WIFI, Default::default()).unwrap();
        let now = wifi.esp_now();
        if let Err(e) = now.set_channel(uplink::CHANNEL) {
            out::line(format_args!(r#"{{"t":"warn","what":"espnow","note":"set_channel {e:?}"}}"#));
        }
        spawner.spawn(uplink::run(now).unwrap());
        wifi
    };

    out::hub_frame(&hello_frame());
    out::hub_frame(&state::status_frame());
    spawner.spawn(radio::run(lr).unwrap());

    // Once a second: Stats (and a JSON line); every few seconds Hello,
    // Status and AeroStatus; AeroStatus at once when it changes.
    let mut ticker = Ticker::every(Duration::from_secs(1));
    let mut secs: u64 = 0;
    node_common::fault::arm(WATCHDOG);
    loop {
        ticker.next().await;
        node_common::fault::feed();
        secs += 1;
        let up = Instant::now().as_micros();
        let c = &COUNTERS;
        let packets = c.packets.load(Relaxed);
        let air = c.air.load(Relaxed);
        let dropped = c.rate_dropped.load(Relaxed).wrapping_add(c.uart_drops.load(Relaxed));
        out::hub_frame(&link::Frame::Stats {
            uptime_us: up,
            frames_seen: packets,
            rid_found: air,
            dropped,
            by_transport: heapless::Vec::new(),
        });
        let (clock_rate, clock_err, clock_age) = critical_section::with(|cs| {
            let k = state::CLOCK.borrow_ref(cs);
            (k.rate_ppm(), k.last_error_ms, k.age_us(up).map_or(-1, |a| (a / 1000) as i64))
        });
        let (cp, cr, crep, cdrop, csw, cfol) = control::stats();
        out::line(format_args!(r#"{{"t":"ctrl_stats","packets":{cp},"recognised":{cr},"reports":{crep},"dropped":{cdrop},"windows":{csw},"follows":{cfol}}}"#));
        out::line(format_args!(
            r#"{{"t":"stats","up_us":{up},"packets":{packets},"air":{air},"rejected":{},"rate_dropped":{},"uart_drops":{},"usb_drops":{},"radio_err":{},"retunes":{},"rx_restarts":{},"syncs":{},"utc":{},"clock_ppm":{clock_rate:.1},"clock_err_ms":{clock_err},"sync_age_ms":{clock_age},"plan_id":{},"cmds":[{},{},{}]}}"#,
            c.rejected.load(Relaxed),
            c.rate_dropped.load(Relaxed),
            c.uart_drops.load(Relaxed),
            c.usb_drops.load(Relaxed),
            c.radio_errors.load(Relaxed),
            c.retunes.load(Relaxed),
            c.rx_restarts.load(Relaxed),
            c.time_syncs.load(Relaxed),
            state::utc_ms(up).map_or(0, |u| u),
            state::PLAN_ID.load(Relaxed),
            state::APPLIED.load(Relaxed),
            state::REPEATS.load(Relaxed),
            state::REJECTED.load(Relaxed),
        ));
        #[cfg(feature = "wirebus")]
        {
            let l = node_common::bus::json_line();
            out::line(format_args!("{}", l.as_str()));
        }
        let hello = secs.is_multiple_of(HELLO_EVERY_S);
        if hello {
            out::hub_frame(&hello_frame());
            out::hub_frame(&state::status_frame());
        }
        if hello || state::STATUS_CHANGED.swap(false, Relaxed) {
            out::hub_frame(&state::aero_status_frame());
        }
    }
}

/// Bench only: `fault panic` / `fault hang` typed on USB (docs/BENCH.md).
#[cfg(feature = "faulttest")]
#[embassy_executor::task]
async fn faulttest(mut rx: esp_hal::usb::usb_serial_jtag::UsbSerialJtagRx<'static, Async>) {
    node_common::fault::console(|b| rx.drain_rx_fifo(b), |l| out::line(format_args!("{l}"))).await
}

#[embassy_executor::task]
async fn usb_task(mut tx: UsbSerialJtagTx<'static, Async>) {
    let mut buf = [0u8; 256];
    loop {
        let n = out::USB.read(&mut buf).await;
        let _ = tx.write_all(&buf[..n]).await;
        if out::USB.is_empty() {
            let _ = tx.flush().await;
        }
    }
}

#[cfg(not(any(feature = "espnow", feature = "wirebus")))]
#[embassy_executor::task]
async fn uart_tx_task(mut tx: UartTx<'static, Async>) {
    let mut buf = [0u8; 256];
    loop {
        let n = out::UART.read(&mut buf).await;
        let _ = tx.write_all(&buf[..n]).await;
    }
}

#[cfg(not(any(feature = "espnow", feature = "wirebus")))]
#[embassy_executor::task]
async fn uart_rx_task(mut rx: UartRx<'static, Async>) {
    let mut dec = link::CommandDecoder::new();
    let mut buf = [0u8; 128];
    loop {
        let Ok(n) = rx.read_async(&mut buf).await else { continue };
        let at = state::now_us();
        dec.feed(&buf[..n], |r| {
            if let Ok(c) = r {
                state::on_command(c, at);
            }
        });
    }
}

/// The wired bus: whole hub frames from the `out::UART` pipe; `Assign`s and
/// `TimeSync`s to `state`.
#[cfg(feature = "wirebus")]
#[embassy_executor::task]
async fn bus_task(
    uart: esp_hal::peripherals::UART1<'static>,
    tx: esp_hal::peripherals::GPIO0<'static>,
    rx: esp_hal::peripherals::GPIO20<'static>,
) -> ! {
    let uart = node_common::bus::uart(uart, tx, rx);
    let mut outbox = node_common::bus::Staged::new(|v: &mut node_common::bus::Unit| {
        let mut b = [0u8; 1];
        while let Ok(1) = out::UART.try_read(&mut b) {
            if v.push(b[0]).is_err() {
                v.clear();
            }
            if b[0] == 0 {
                if v.len() > 1 {
                    return true;
                }
                v.clear();
            }
        }
        v.clear();
        false
    });
    node_common::bus::run_with_time(uart, &mut outbox, &mut |a| state::on_assign(a), &mut |t| state::on_time(t.at_us, t.utc_ms, t.pps))
        .await
}
