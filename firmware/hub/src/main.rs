//! Rotorscope hub: merges what the radio nodes hear into drone tracks and
//! serves them to a phone (hotspot + web page), ATAK (CoT multicast), a laptop
//! (USB JSON lines) and, later, a screen and an SD card.
//!
//! Shape:
//! - `links`: node UARTs -> `link::Decoder` on a high-priority interrupt
//!   executor -> `FRAMES` queue -> `ingest` -> `hub::Hub` (the track table).
//! - `hotspot`, `http`, `cot`, `station`: the network side, on embassy-net.
//! - `tick`: 1 Hz housekeeping and the USB stream; `usb`: output pipe + commands.
//! - `gnss`: the MAX-M10S over I2C (or UART), plus PPS timestamps.
//! - `periph` (S3 second core): `screen` (banded panel rendering, buttons),
//!   `sd` (CSV log), `panel` (buzzer, battery), sharing one SPI bus (`bus`).
//! - `adsbee` (feature, S3): crewed aircraft from an ADSBee 1090 on UART2,
//!   in their own table; never an alert.
//! - `demo` (feature): two synthetic drones in place of nodes (and, with
//!   `adsbee`, a synthetic helicopter).
//!
//! All tracker access is on the thread-mode executor of core 0, so it needs
//! no lock; the second core only sees copies (`screen::UI_STATE`, SD rows).
//! Every peripheral that is not fitted (GPS, panel, SD card, buttons, buzzer,
//! battery) degrades to "absent" without blocking anything else.
#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

mod adsbee;
mod api;
mod bus;
mod clock;
mod cot;
#[cfg(feature = "demo")]
mod demo;
mod gnss;
mod hotspot;
mod http;
mod hub;
mod links;
mod panel;
mod periph;
mod pins;
mod pwm;
mod screen;
mod sd;
#[cfg(hub_station)]
mod station;
mod tick;
mod usb;

use embassy_executor::Spawner;
use embassy_time::Timer;
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::gpio::Pin as _;
use esp_hal::interrupt::Priority;
use esp_hal::rng::Rng;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::uart::AnyUart;
use esp_hal::usb::usb_serial_jtag::UsbSerialJtag;
use esp_radio::wifi::{ControllerConfig, Interface, WifiController};
use esp_rtos::embassy::InterruptExecutor;
use serde::Serialize;
use static_cell::StaticCell;

use crate::hub::Hub;

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

#[cfg(feature = "esp32c5")]
use esp_wifi_sys_esp32c5::include as sys;
#[cfg(feature = "esp32s3")]
use esp_wifi_sys_esp32s3::include as sys;

#[cfg(all(feature = "esp32s3", feature = "esp32c5"))]
compile_error!("enable exactly one of the `esp32s3` / `esp32c5` features");
// The C5 has two UARTs and both carry nodes.
#[cfg(all(feature = "esp32c5", feature = "gps-uart"))]
compile_error!("`gps-uart` is S3-only: the C5's two UARTs both carry nodes; use the I2C GPS");
// The ADSBee needs a UART of its own (see adsbee.rs).
#[cfg(all(feature = "esp32c5", feature = "adsbee"))]
compile_error!("`adsbee` is S3-only: the C5's two UARTs both carry nodes");
#[cfg(all(feature = "adsbee", feature = "gps-uart"))]
compile_error!("`adsbee` and `gps-uart` both need the S3's third UART; use the I2C GPS");

const CHIP: &str = if cfg!(feature = "esp32s3") { "esp32s3" } else { "esp32c5" };

/// Heap in RAM that the second-stage bootloader used and gives back (the
/// largest block that is otherwise wasted), plus a block of ordinary DRAM.
/// esp-radio allocates Wi-Fi buffers and its task stacks here; embassy-net
/// sockets and everything else are static. Sized from measurements on the S3
/// without PSRAM: see `heap_min_free` in the `{"t":"hub"}` line.
#[cfg(feature = "esp32s3")]
const RECLAIMED_HEAP: usize = 73_744;
#[cfg(feature = "esp32c5")]
const RECLAIMED_HEAP: usize = 65_536;
#[cfg(feature = "esp32s3")]
const EXTRA_HEAP: usize = 64 * 1024;
/// 4 KB less on the C5: its main stack was already within ~1.2 KB of the
/// linker's 8 KB floor, and the open-FPV-link table (`tracker::openlinks`)
/// needed the room. The S3 keeps 64 KB; it showed >= 55 KB of heap unused.
/// Not measured on a C5 hub (none has run yet).
#[cfg(feature = "esp32c5")]
const EXTRA_HEAP: usize = 60 * 1024;

/// Centre of the demo drones' circles, and the reference for the USB `p`
/// test position: Philadelphia City Hall, as `rid-beacon` uses.
pub fn demo_centre() -> (f64, f64) {
    (39.9526, -75.1652)
}

/// The hub runs unattended: after a panic (backtrace already printed on USB),
/// reboot rather than sit halted until someone power-cycles the box.
#[unsafe(no_mangle)]
extern "Rust" fn custom_halt() -> ! {
    esp_hal::delay::Delay::new().delay_millis(500);
    esp_hal::system::software_reset()
}

#[derive(Serialize)]
struct BootLine {
    t: &'static str,
    fw: &'static str,
    ver: &'static str,
    chip: &'static str,
    proto: u8,
    demo: bool,
    station: bool,
    open_hotspot: bool,
    links: usize,
    /// How the GPS is wired: `"i2c"` or `"uart"` (feature `gps-uart`).
    gps: &'static str,
    /// An ADSBee on UART2 (feature `adsbee`), and the aircraft table size.
    adsbee: bool,
    max_aircraft: usize,
    /// Screen controller the build drives.
    panel: &'static str,
    max_tracks: usize,
    /// Static RAM the track table costs, bytes (open-link and aircraft
    /// tables included).
    track_bytes: usize,
    /// Open FPV links the table holds, and whether their identifiers are
    /// shown (feature `show-link-ids`; off by default).
    max_open_links: usize,
    show_link_ids: bool,
    heap_free: usize,
    pins: &'static [(&'static str, u8)],
}

static HUB: StaticCell<Hub> = StaticCell::new();
static UART_EXECUTOR: StaticCell<InterruptExecutor<2>> = StaticCell::new();

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: RECLAIMED_HEAP);
    esp_alloc::heap_allocator!(size: EXTRA_HEAP);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    // USB first, so everything after this can report.
    let (usb_rx, usb_tx) = UsbSerialJtag::new(peripherals.USB_DEVICE).into_async().split();
    spawner.spawn(usb::writer(usb_tx).expect("spawn usb writer"));

    let pins = pins::take!(peripherals);
    let hub: &'static Hub = HUB.init(Hub::new());
    hotspot::init_identity();
    screen::init_state();

    usb::line(&BootLine {
        t: "boot",
        fw: "hub",
        ver: env!("CARGO_PKG_VERSION"),
        chip: CHIP,
        proto: link::PROTOCOL_VERSION,
        demo: cfg!(feature = "demo"),
        station: cfg!(hub_station),
        open_hotspot: hotspot::OPEN,
        links: pins::NODE_LINKS - usize::from(cfg!(feature = "gps-uart") || cfg!(feature = "adsbee")),
        gps: if cfg!(feature = "gps-uart") { "uart" } else { "i2c" },
        adsbee: adsbee::ENABLED,
        max_aircraft: hub::MAX_AIRCRAFT,
        panel: if cfg!(feature = "ili9341") { "ili9341" } else { "st7789" },
        max_tracks: hub::MAX_TRACKS,
        track_bytes: core::mem::size_of::<hub::HubTracker>(),
        max_open_links: tracker::openlinks::MAX_LINK_TRACKS,
        show_link_ids: hub::SHOW_LINK_IDS,
        heap_free: esp_alloc::HEAP.free(),
        pins: pins::TABLE,
    });
    hotspot::print_info();

    // ---- Node links, on the interrupt executor (see links.rs) ----
    let uart_exec = UART_EXECUTOR.init(InterruptExecutor::new(peripherals.FROM_CPU_INTR2));
    let uart_spawner = uart_exec.start(Priority::Priority2);
    #[cfg(feature = "adsbee")]
    let pins::Pins { node_rx, gps_sda, gps_scl, pps, spi_sck, spi_mosi, spi_miso, screen_cs, screen_dc,
        screen_backlight, sd_cs, buttons, buzzer, battery, adsbee_rx, .. } = pins;
    #[cfg(not(feature = "adsbee"))]
    let pins::Pins { node_rx, gps_sda, gps_scl, pps, spi_sck, spi_mosi, spi_miso, screen_cs, screen_dc,
        screen_backlight, sd_cs, buttons, buzzer, battery, .. } = pins;
    #[cfg(feature = "esp32s3")]
    let [rx0, rx1, rx2] = node_rx;
    #[cfg(feature = "esp32c5")]
    let [rx0, rx1] = node_rx;
    uart_spawner.spawn(links::reader(0, AnyUart::from(peripherals.UART0), rx0).expect("spawn reader 0"));
    uart_spawner.spawn(links::reader(1, AnyUart::from(peripherals.UART1), rx1).expect("spawn reader 1"));
    // With `gps-uart` (the GPS) or `adsbee` (the ADSBee) the S3's third UART
    // is taken, so node link 2 goes.
    #[cfg(all(feature = "esp32s3", not(feature = "gps-uart"), not(feature = "adsbee")))]
    uart_spawner.spawn(links::reader(2, AnyUart::from(peripherals.UART2), rx2).expect("spawn reader 2"));
    #[cfg(all(feature = "esp32s3", any(feature = "gps-uart", feature = "adsbee")))]
    let _ = rx2;
    spawner.spawn(links::ingest(hub).expect("spawn ingest"));
    #[cfg(feature = "adsbee")]
    {
        uart_spawner.spawn(adsbee::reader(AnyUart::from(peripherals.UART2), adsbee_rx).expect("spawn adsbee"));
        spawner.spawn(adsbee::ingest(hub).expect("spawn adsbee ingest"));
    }

    // ---- GPS (P1, P3) ----
    let (sda, scl) = (gps_sda, gps_scl);
    uart_spawner.spawn(gnss::pps_task(pps).expect("spawn pps"));
    #[cfg(not(feature = "gps-uart"))]
    match esp_hal::i2c::master::I2c::new(peripherals.I2C0, gnss::I2cLink::config()) {
        Ok(i2c) => spawner.spawn(gnss::i2c_task(gnss::I2cLink(i2c.with_sda(sda).with_scl(scl).into_async())).expect("spawn gnss")),
        Err(_) => usb::msg("err", "gps i2c config"),
    }
    // UART: hub RX on the SDA pin, hub TX on the SCL pin (see pins.rs).
    #[cfg(feature = "gps-uart")]
    {
        match esp_hal::uart::Uart::new(peripherals.UART2, gnss::UartLink::config(115_200)) {
            Ok(u) => spawner.spawn(
                gnss::uart_task(gnss::UartLink { uart: u.with_rx(sda).with_tx(scl).into_async(), baud_idx: 0 })
                    .expect("spawn gnss"),
            ),
            Err(_) => usb::msg("err", "gps uart config"),
        }
    }

    // ---- Screen, SD, buttons, buzzer, battery (second core on the S3) ----
    let parts = periph::Parts {
        spi: peripherals.SPI2,
        #[cfg(feature = "esp32s3")]
        ledc: peripherals.LEDC,
        adc: peripherals.ADC1,
        sck: spi_sck,
        mosi: spi_mosi,
        miso: spi_miso,
        screen_cs,
        screen_dc,
        backlight: screen_backlight,
        sd_cs,
        buttons,
        buzzer,
        battery,
    };
    #[cfg(feature = "esp32s3")]
    periph::start_core1(peripherals.CPU_CTRL, peripherals.FROM_CPU_INTR1, parts);
    #[cfg(feature = "esp32c5")]
    periph::spawn(spawner, parts);
    spawner.spawn(screen::feeder(hub).expect("spawn screen feeder"));

    // ---- Wi-Fi: hotspot, and station when built ----
    let ap_iface = Interface::access_point();
    #[cfg(hub_station)]
    let sta_iface = Interface::station();
    #[cfg(hub_station)]
    let initial = esp_radio::wifi::Config::AccessPointStation(station::config(), hotspot::ap_config());
    #[cfg(not(hub_station))]
    let initial = esp_radio::wifi::Config::AccessPoint(hotspot::ap_config());
    // Fewer driver buffers than esp-radio's defaults (10 static RX, 32
    // dynamic RX/TX): the hub moves a web page and small JSON, not bulk data,
    // and every buffer is ~1.6 KB of heap when in use.
    let ctl = ControllerConfig::default()
        .with_static_rx_buf_num(8)
        .with_dynamic_rx_buf_num(16)
        .with_dynamic_tx_buf_num(16)
        .with_initial_config(initial);
    let wifi = WifiController::new(peripherals.WIFI, ctl).expect("Wi-Fi init");
    set_country_us();

    let rng = Rng::new();
    let seed = || (u64::from(rng.random()) << 32) | u64::from(rng.random());

    let (ap_stack, ap_runner) = hotspot::ap_stack(ap_iface, seed());
    spawner.spawn(hotspot::net_runner(ap_runner).expect("spawn ap net"));
    spawner.spawn(hotspot::dhcp(ap_stack).expect("spawn dhcp"));
    spawner.spawn(hotspot::dns(ap_stack).expect("spawn dns"));
    for i in 0..http::AP_TASKS {
        spawner.spawn(http::task(i, ap_stack, hub).expect("spawn http"));
    }
    spawner.spawn(cot::task(ap_stack, hub).expect("spawn cot"));

    #[cfg(hub_station)]
    {
        let (sta_stack, sta_runner) = station::stack(sta_iface, seed());
        spawner.spawn(hotspot::net_runner(sta_runner).expect("spawn sta net"));
        spawner.spawn(http::task(http::AP_TASKS, sta_stack, hub).expect("spawn sta http"));
        spawner.spawn(cot::task(sta_stack, hub).expect("spawn sta cot"));
        spawner.spawn(station::task(wifi).expect("spawn station"));
    }
    // Without station mode nothing else needs the controller, but dropping it
    // would shut Wi-Fi down, so main keeps it.
    #[cfg(not(hub_station))]
    let _wifi = wifi;

    #[cfg(feature = "demo")]
    {
        spawner.spawn(demo::task(demo_centre()).expect("spawn demo"));
        // Give the demo an own position so range/bearing are exercised.
        let (lat, lon) = usb::test_point();
        hub.set_manual(lat, lon, Some(15.0), hub::now_ms());
        usb::msg("demo", "two synthetic drones (DEMO-ALPHA-0001, DEMO-BRAVO-0002); own position set to test point");
        spawner.spawn(demo_state_check(hub).expect("spawn state check"));
    }

    let rtc = esp_hal::rtc_cntl::Rtc::new(peripherals.RTC_TIMER);
    spawner.spawn(tick::task(hub, rtc).expect("spawn tick"));
    spawner.spawn(tick::lag_probe().expect("spawn lag probe"));
    spawner.spawn(usb::commands(usb_rx, hub).expect("spawn usb commands"));

    loop {
        Timer::after_secs(3600).await;
    }
}

/// Demo builds check once, after both demo drones exist, that the streamed
/// `/api/state` body equals `tracker::json::state` byte for byte.
#[cfg(feature = "demo")]
#[embassy_executor::task]
async fn demo_state_check(hub: &'static Hub) {
    Timer::after_secs(6).await;
    usb::state_check(hub, hub::now_ms());
}

/// Country "US", set through the driver's country-code call as the nodes do:
/// it fills in the per-country channel rules (esp-radio beta.1's own
/// `CountryInfo` path sets a zero channel mask), and channel 11 is the top of
/// the US 2.4 GHz band.
fn set_country_us() {
    let code = *b"US\0";
    // SAFETY: NUL-terminated string that outlives the call; Wi-Fi is initialised.
    let err = unsafe { sys::esp_wifi_set_country_code(code.as_ptr().cast(), false) };
    if err != 0 {
        usb::msg("err", "esp_wifi_set_country_code");
    }
}
