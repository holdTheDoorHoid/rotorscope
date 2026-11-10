//! node-sdr: the ESP32-C5 as a raw radio sampler (PLAN.md Phase 10b).
//!
//! Boot: Wi-Fi driver up (it powers and calibrates the PHY; nothing is ever
//! transmitted), PSRAM up and the DroneID receiver built in it, then
//! [`radio::Radio::boot`]: PHY locked receive-only, dump engine free-running,
//! debug bus looped through eight pads into PARL_IO, 40 MS/s ring running.
//! `main` then runs the capture loop; a second task reads console commands
//! from USB. Output is JSON lines on USB-Serial-JTAG (plus `@H/@D/@E` dump
//! lines, see `rawcap::dump`). BRINGUP.md is the first-flash checklist.
#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

mod engine;
mod phy;
mod radio;
#[cfg(feature = "decode")]
mod report;
mod ring;
mod route;

use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_backtrace as _;
use esp_hal::Blocking;
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::usb::usb_serial_jtag::{UsbSerialJtag, UsbSerialJtagRx, UsbSerialJtagTx};
use esp_println::println;
use rawcap::cmd::{self, Command, LineBuf};

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

const LOG_LEVEL: log::LevelFilter = log::LevelFilter::Off;

/// Heap for the Wi-Fi driver: the 64 KiB the second-stage bootloader frees
/// (dram2), plus whatever block 0 has left below the radio bank.
const HEAP_RECLAIMED: usize = 64 * 1024;

static COMMANDS: Channel<CriticalSectionRawMutex, Command, 4> = Channel::new();

unsafe extern "C" {
    static _sdr_gap_start: u8;
    static _sdr_gap_end: u8;
    static _sdr_bank_start: u8;
    static _stack_start: u8;
    static _stack_end: u8;
}

fn addr(s: &u8) -> usize {
    s as *const u8 as usize
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger(LOG_LEVEL);

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: HEAP_RECLAIMED);
    // SAFETY: linker symbols from ld/sdr_bank.x; the gap between .bss and the
    // bank is otherwise unused RAM.
    let (gap_start, gap_end) = unsafe { (addr(&_sdr_gap_start), addr(&_sdr_gap_end)) };
    let gap = gap_end.saturating_sub(gap_start.next_multiple_of(8));
    if gap >= 4096 {
        unsafe {
            esp_alloc::HEAP.add_region(esp_alloc::HeapRegion::new(
                gap_start.next_multiple_of(8) as *mut u8,
                gap,
                esp_alloc::MemoryCapability::Internal.into(),
            ));
        }
    }

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    let (stack_lo, stack_hi, bank) = unsafe { (addr(&_stack_end), addr(&_stack_start), addr(&_sdr_bank_start)) };
    println!(
        r#"{{"t":"hello","fw":"node-sdr","ver":"{}","chip":"esp32c5","decode":{},"bank":"{bank:#x}","heap_gap":{gap},"heap_reclaimed":{HEAP_RECLAIMED},"stack":{}}}"#,
        env!("CARGO_PKG_VERSION"),
        cfg!(feature = "decode"),
        stack_hi - stack_lo
    );

    let mut radio = radio::Radio::new(radio::Settings::default());

    #[cfg(feature = "decode")]
    {
        use droneid::rx::Receiver;
        let psram = esp_hal::psram::Psram::new(peripherals.PSRAM, Default::default());
        let (ptr, size) = psram.raw_parts();
        let need = core::mem::size_of::<Receiver>();
        if size >= need && !ptr.is_null() && (ptr as usize).is_multiple_of(core::mem::align_of::<Receiver>()) {
            // SAFETY: the start of the mapped PSRAM, which nothing else uses
            // (no PSRAM heap region is registered); 'static for the whole run.
            let slot = unsafe { &mut *(ptr as *mut core::mem::MaybeUninit<Receiver>) };
            match Receiver::init_in_place(slot, radio::RATE_DECODE) {
                Ok(rx) => {
                    println!(r#"{{"t":"psram","bytes":{size},"receiver":{need},"at":"{:#x}"}}"#, ptr as usize);
                    radio.set_receiver(rx);
                }
                Err(e) => println!(r#"{{"t":"err","what":"receiver","e":"{e:?}"}}"#),
            }
        } else {
            println!(
                r#"{{"t":"err","what":"psram","bytes":{size},"need":{need},"note":"no PSRAM: decoding off; build with cargo fw-lite for boards without it"}}"#
            );
        }
    }

    // Wi-Fi only to bring the PHY up. Power management off (it would gate
    // the PHY clocks that keep the debug bus running: C5VRX `rf_start`),
    // small buffer pools (we never receive frames: the promiscuous filter
    // is empty), station mode never connected or scanning.
    let wifi_cfg = esp_radio::wifi::ControllerConfig::default()
        .with_sta_disconnected_pm(false)
        .with_static_rx_buf_num(2)
        .with_dynamic_rx_buf_num(4)
        .with_dynamic_tx_buf_num(2)
        .with_ampdu_rx_enable(false)
        .with_ampdu_tx_enable(false)
        .with_rx_ba_win(1);
    let wifi = esp_radio::wifi::WifiController::new(peripherals.WIFI, wifi_cfg);
    let _wifi = match wifi {
        Ok(w) => Some(w),
        Err(e) => {
            println!(r#"{{"t":"err","what":"wifi_init","e":"{e:?}"}}"#);
            None
        }
    };
    if _wifi.is_some() {
        use esp_wifi_sys_esp32c5::include as sys;
        // Promiscuous with an empty filter keeps the receive path powered
        // without buffering or interrupting on frames (C5VRX).
        // SAFETY: plain calls into the running driver.
        let filter = sys::wifi_promiscuous_filter_t { filter_mask: 0 };
        let e1 = unsafe { sys::esp_wifi_set_promiscuous_filter(&filter) };
        let e2 = unsafe { sys::esp_wifi_set_promiscuous(true) };
        if e1 != 0 || e2 != 0 {
            println!(r#"{{"t":"err","what":"promiscuous","e":[{e1},{e2}]}}"#);
        }
        // The pads and peripherals the radio owns from here on (tokens only:
        // the registers are driven directly, see route.rs and ring.rs).
        let _owned = (
            peripherals.PARL_IO,
            peripherals.DMA_CH0,
            peripherals.GPIO0,
            peripherals.GPIO1,
            peripherals.GPIO2,
            peripherals.GPIO3,
            peripherals.GPIO4,
            peripherals.GPIO5,
            peripherals.GPIO25,
            peripherals.GPIO27,
        );
        radio.boot();
    }

    let (rx, tx) = UsbSerialJtag::new(peripherals.USB_DEVICE).split();
    spawner.spawn(console(rx, tx).expect("spawn console"));

    let mut last_stats = Instant::now();
    loop {
        let _ = with_timeout(Duration::from_millis(50), ring::NEW_DATA.wait()).await;
        while let Ok(c) = COMMANDS.try_receive() {
            radio.command(c);
        }
        radio.poll();
        if last_stats.elapsed() >= Duration::from_secs(1) {
            last_stats = Instant::now();
            radio.tick();
            radio.stats();
        }
    }
}

/// Read console lines from USB and queue commands for the capture loop.
#[embassy_executor::task]
async fn console(mut rx: UsbSerialJtagRx<'static, Blocking>, _tx: UsbSerialJtagTx<'static, Blocking>) {
    let mut line = LineBuf::new();
    let mut buf = [0u8; 64];
    loop {
        let n = rx.drain_rx_fifo(&mut buf);
        for &b in &buf[..n] {
            match line.push(b) {
                Some(Ok(l)) => match cmd::parse(l) {
                    Ok(Some(c)) => {
                        if COMMANDS.try_send(c).is_err() {
                            println!(r#"{{"t":"err","what":"busy","note":"command queue full"}}"#);
                        }
                    }
                    Ok(None) => {}
                    Err(e) => println!(r#"{{"t":"err","what":"cmd","e":"{e}"}}"#),
                },
                Some(Err(e)) => println!(r#"{{"t":"err","what":"cmd","e":"{e}"}}"#),
                None => {}
            }
        }
        Timer::after(Duration::from_millis(20)).await;
    }
}
