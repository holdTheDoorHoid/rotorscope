//! node-sdr: the ESP32-C5 as a raw radio sampler (PLAN.md Phase 10b).
//!
//! Boot: Wi-Fi driver up (it powers and calibrates the PHY; nothing is ever
//! transmitted), PSRAM up and the DroneID receiver built in it, then
//! [`radio::Radio::boot`]: PHY locked receive-only, dump engine free-running,
//! debug bus looped through eight pads into PARL_IO, 40 MS/s ring running.
//! `main` then runs the capture loop; a second task reads console commands
//! from USB. Output is JSON lines on USB-Serial-JTAG (plus `@H/@D/@E` dump
//! lines, see `rawcap::dump`). BRINGUP.md is the first-flash checklist.
//!
//! Jobs (`hub` / `link::NodeRole`): **DroneId** walks a plan of DroneID
//! frequencies and decodes bursts; **Fpv** walks the analog FPV channels and
//! classifies what is on them (`crates/fpvscan`). The hub assigns the job
//! and plan (`link::Command::Assign`); without one the node runs the DroneID
//! built-in plan. With the `espnow` feature, link frames go to the ESP-NOW
//! gateway in short uplink windows between captures (`uplink`): the only
//! time this firmware transmits.
//!
//! Crash recovery (docs/BENCH.md "Crash recovery"): a panic prints its
//! backtrace and reboots (`custom_halt`); the capture loop feeds the RTC
//! watchdog every time round ([`WATCHDOG`]); the `hello` line says why the
//! chip last reset.
#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

#[cfg_attr(not(feature = "espnow"), allow(dead_code, reason = "Uplink(now) is for the espnow build"))]
mod console;
mod engine;
#[cfg_attr(
    not(feature = "espnow"),
    allow(dead_code, reason = "the queue and command side serve the espnow uplink (the wired bus decodes its own commands)")
)]
mod hub;
mod out;
mod phy;
mod radio;
#[cfg(feature = "decode")]
mod report;
mod ring;
mod route;
#[cfg(feature = "espnow")]
mod uplink;

#[cfg(all(feature = "wirebus", feature = "espnow"))]
compile_error!("`wirebus` and `espnow` are two ways to reach the hub; build one");

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
use rawcap::cmd::LineBuf;

use console::{Cmd, Local};

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

/// Unattended node: after a panic (backtrace already printed to USB),
/// reboot rather than sit halted until someone power-cycles the box.
#[unsafe(no_mangle)]
extern "Rust" fn custom_halt() -> ! {
    node_common::fault::reboot_after_panic()
}

/// Hang watchdog timeout. The capture loop feeds it every time round (at
/// least every 50 ms when idle). The longest legitimate holds measured on
/// the bench (BRINGUP-RESULTS.md) are a DroneID decode (~150 ms), an FPV
/// analysis (~100 ms), a DC recalibration (~25 ms) and the first uplink
/// window (~0.5 s, its discovery capped at 1.2 s); a typed `dump` feeds per
/// line (`out::line`). 10 s is far above all of them.
const WATCHDOG: esp_hal::time::Duration = esp_hal::time::Duration::from_secs(10);

const LOG_LEVEL: log::LevelFilter = log::LevelFilter::Off;

/// Heap for the Wi-Fi driver: the 64 KiB the second-stage bootloader frees
/// (dram2), plus whatever block 0 has left below the radio bank.
const HEAP_RECLAIMED: usize = 64 * 1024;

static COMMANDS: Channel<CriticalSectionRawMutex, Cmd, 4> = Channel::new();

/// Console commands baked in at build time and run after boot, one every
/// 500 ms, separated by `;` (e.g.
/// `ROTORSCOPE_BENCH=1 NODE_SDR_BOOT="level;grab;dump" cargo fw --features bringup`).
/// For bring-up with a read-only serial capture such as tools/dev-board.sh.
/// A bench build switch (build.rs): the build needs `ROTORSCOPE_BENCH=1`.
const BOOT_SCRIPT: Option<&str> = option_env!("NODE_SDR_BOOT");

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

/// Lowest stack address (the stack grows down to it) and the paint word.
static STACK_LO: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
const PAINT: u32 = 0x5afe_57ac;

/// Paint the unused part of the main stack, so [`stack_free`] can tell how
/// deep it has ever been.
fn paint_stack(lo: usize) {
    let sp: usize;
    // SAFETY: reads the stack pointer.
    unsafe { core::arch::asm!("mv {}, sp", out(reg) sp) };
    // Skip esp-hal's stack guard word at the very bottom.
    let lo = lo + 256;
    STACK_LO.store(lo, core::sync::atomic::Ordering::Relaxed);
    let mut p = lo.next_multiple_of(4);
    while p + 4096 < sp {
        // SAFETY: below the live stack (with a 4 KiB margin), inside it.
        unsafe { (p as *mut u32).write_volatile(PAINT) };
        p += 4;
    }
}

/// Bytes of the main stack never used since boot.
pub fn stack_free() -> usize {
    let lo = STACK_LO.load(core::sync::atomic::Ordering::Relaxed).next_multiple_of(4);
    let mut p = lo;
    // SAFETY: inside the stack region, below anything live when untouched.
    while unsafe { (p as *const u32).read_volatile() } == PAINT {
        p += 4;
    }
    p - lo
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
    paint_stack(stack_lo);
    println!(
        r#"{{"t":"hello","fw":"node-sdr","ver":"{}","chip":"esp32c5","decode":{},"bank":"{bank:#x}","heap_gap":{gap},"heap_reclaimed":{HEAP_RECLAIMED},"stack":{},"reset":"{}","faulttest":{},"bench":{}}}"#,
        env!("CARGO_PKG_VERSION"),
        cfg!(feature = "decode"),
        stack_hi - stack_lo,
        node_common::fault::reset_reason(),
        cfg!(feature = "faulttest"),
        env!("ROTORSCOPE_BENCH_JSON")
    );
    // Bench features compiled in (build.rs): say so, with the tag
    // tools/flash-node.sh looks for in the ELF.
    if !env!("ROTORSCOPE_BENCH_LIST").is_empty() {
        println!(
            r#"{{"t":"warn","what":"bench_build","tag":"{}","note":"BENCH BUILD: never leave it on a fielded unit"}}"#,
            env!("ROTORSCOPE_BENCH_TAG")
        );
    }

    // On the heap, not in main's future: the main stack also carries the
    // DroneID decoder (~30 KiB) and sits right above SRAM block 1, so every
    // byte kept here is a byte closer to the capture bank (2026-11-14: with
    // these on the stack it overflowed into the decoder input).
    let mut radio = new_radio();

    // PSRAM: [DroneID receiver][FPV scratch][HIL staging], bump-allocated.
    let psram = esp_hal::psram::Psram::new(peripherals.PSRAM, Default::default());
    let (psram_ptr, psram_size) = psram.raw_parts();
    let mut psram_used = 0usize;
    #[cfg(feature = "decode")]
    {
        use droneid::rx::Receiver;
        let (ptr, size) = (psram_ptr, psram_size);
        let need = core::mem::size_of::<Receiver>();
        if size >= need && !ptr.is_null() && (ptr as usize).is_multiple_of(core::mem::align_of::<Receiver>()) {
            // SAFETY: the start of the mapped PSRAM, which nothing else uses
            // (no PSRAM heap region is registered); 'static for the whole run.
            let slot = unsafe { &mut *(ptr as *mut core::mem::MaybeUninit<Receiver>) };
            match Receiver::init_in_place(slot, radio::RATE_DECODE) {
                Ok(rx) => {
                    println!(r#"{{"t":"psram","bytes":{size},"receiver":{need},"at":"{:#x}"}}"#, ptr as usize);
                    radio.set_receiver(rx);
                    psram_used = need.next_multiple_of(64);
                }
                Err(e) => println!(r#"{{"t":"err","what":"receiver","e":"{e:?}"}}"#),
            }
        } else {
            println!(
                r#"{{"t":"err","what":"psram","bytes":{size},"need":{need},"note":"no PSRAM: decoding off; build with cargo fw-lite for boards without it"}}"#
            );
        }
    }

    // FPV analysis scratch and the HIL staging buffer, after the receiver.
    {
        // The FPV scratch goes in internal RAM (heap): in PSRAM one
        // analysis took 100 ms (2026-11-14), the ring stopped all that time.
        let sc = core::mem::size_of::<fpvscan::Scratch>();
        radio.set_fpv_scratch(alloc::boxed::Box::leak(new_scratch()));
        // DJI link fingerprints (crates/djilink): 12.6 KiB, internal RAM too.
        radio.set_dji_scratch(alloc::boxed::Box::leak(new_dji_scratch()));
        let at = psram_used.next_multiple_of(64);
        if !psram_ptr.is_null() && at + console::STAGE_LEN <= psram_size {
            let stage = at;
            // SAFETY: the next unused PSRAM range; only the console writes it
            // (before `hil run`), only the capture loop reads it (during).
            unsafe { console::set_stage(psram_ptr.add(stage)) };
            psram_used = stage + console::STAGE_LEN;
            println!(r#"{{"t":"psram","fpv_scratch":{sc},"hil_stage":{},"used":{psram_used}}}"#, console::STAGE_LEN);
        } else {
            println!(r#"{{"t":"err","what":"psram","note":"no room for the HIL stage"}}"#);
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
    let wifi = match wifi {
        Ok(w) => Some(w),
        Err(e) => {
            println!(r#"{{"t":"err","what":"wifi_init","e":"{e:?}"}}"#);
            None
        }
    };
    if wifi.is_some() {
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

    // How much heap the Wi-Fi driver really took (README.md "Memory").
    println!(r#"{{"t":"mem","heap_used":{},"heap_free":{}}}"#, esp_alloc::HEAP.used(), esp_alloc::HEAP.free());

    // The wired bus (docs/WIREBUS.md): TX open-drain on D9 (GPIO9), RX on D7
    // (GPIO12), on an interrupt-mode executor so a poll is answered in time
    // even while the capture loop is busy decoding (it never awaits then).
    #[cfg(feature = "wirebus")]
    {
        let exec = alloc::boxed::Box::leak(alloc::boxed::Box::new(esp_rtos::embassy::InterruptExecutor::new(
            peripherals.FROM_CPU_INTR2,
        )));
        let bus = exec.start(esp_hal::interrupt::Priority::Priority2);
        bus.spawn(bus_task(peripherals.UART1, peripherals.GPIO9, peripherals.GPIO12).expect("spawn bus_task"));
    }

    let (rx, tx) = UsbSerialJtag::new(peripherals.USB_DEVICE).split();
    spawner.spawn(console(rx, tx).expect("spawn console"));

    let hub = new_hub(radio.role);
    #[cfg(feature = "espnow")]
    let up = wifi.as_ref().map(|w| new_uplink(w.esp_now()));
    #[cfg(not(feature = "espnow"))]
    let up: Option<()> = None;
    println!(
        r#"{{"t":"hub","transport":"{}","role":"{}","can":"droneid,fpv","protocol":{}}}"#,
        if cfg!(feature = "espnow") {
            "espnow"
        } else if cfg!(feature = "wirebus") {
            "wirebus"
        } else {
            "none (build with --features wirebus / cargo fw-bus, or espnow / cargo fw-now)"
        },
        hub::role_name(radio.role),
        link::PROTOCOL_VERSION
    );

    // The capture loop runs as its own task so its state (the hub queue,
    // the uplink's futures, everything held across an await) is static, not
    // on the main stack: that stack also carries the DroneID decoder (~30
    // KiB) and ends right above SRAM block 1, the capture bank. With this
    // loop inline in `main`, main's future took 20 KiB of stack and the
    // decoder overflowed 12 KiB into the bank (found by the HIL test,
    // 2026-11-14).
    spawner.spawn(capture_loop(radio, hub, up).expect("spawn capture loop"));
    // `wifi` must stay alive (dropping the controller stops the driver).
    let _wifi = wifi;
    loop {
        Timer::after(Duration::from_secs(3600)).await;
    }
}

/// The wired-bus node: frames from `hub::BUS_OUT`, assignments to
/// `hub::BUS_COMMANDS` (applied by the capture loop).
#[cfg(feature = "wirebus")]
#[embassy_executor::task]
async fn bus_task(
    uart: esp_hal::peripherals::UART1<'static>,
    tx: esp_hal::peripherals::GPIO9<'static>,
    rx: esp_hal::peripherals::GPIO12<'static>,
) -> ! {
    // Made here: an async UART is bound to the executor that polls it.
    let uart = node_common::bus::uart(uart, tx, rx);
    let mut outbox = node_common::bus::Staged::new(|v: &mut node_common::bus::Unit| match hub::BUS_OUT.try_receive() {
        Ok(f) => v.extend_from_slice(&f).is_ok(),
        Err(_) => false,
    });
    node_common::bus::run(uart, &mut outbox, &mut |a| {
        let _ = hub::BUS_COMMANDS.try_send(a);
    })
    .await
}

#[cfg(feature = "espnow")]
type Up = Option<alloc::boxed::Box<uplink::Uplink>>;
#[cfg(not(feature = "espnow"))]
type Up = Option<()>;

#[embassy_executor::task]
async fn capture_loop(mut radio: alloc::boxed::Box<radio::Radio>, mut hub: alloc::boxed::Box<hub::Hub>, up: Up) -> ! {
    #[cfg(feature = "espnow")]
    let mut up = up;
    #[cfg(not(feature = "espnow"))]
    let _ = up;
    let mut last_stats = Instant::now();
    let mut script = BOOT_SCRIPT.unwrap_or("").split(';').map(str::trim).filter(|c| !c.is_empty());
    let mut next_script = Instant::now() + Duration::from_secs(2);
    node_common::fault::arm(WATCHDOG);
    loop {
        // A few register writes: cheap and silent, as the ring needs.
        node_common::fault::feed();
        let _ = with_timeout(Duration::from_millis(50), ring::NEW_DATA.wait()).await;
        while let Ok(c) = COMMANDS.try_receive() {
            match c {
                Cmd::Raw(c) => radio.command(c),
                Cmd::Local(l) => {
                    #[cfg(feature = "espnow")]
                    let force = matches!(l, Local::Uplink(true));
                    local(&mut radio, &mut hub, l);
                    #[cfg(feature = "espnow")]
                    if let Some(u) = up.as_mut() {
                        if force {
                            hub.keepalive_now();
                            if let Some(a) = u.window(&mut radio, &mut hub).await {
                                apply_assignment(&mut radio, &mut hub, &a);
                            }
                        } else {
                            u.print();
                        }
                    }
                }
            }
        }
        radio.poll();
        drain_events(&mut radio, &mut hub);
        hub.set(radio.role, radio.plan_id);
        hub.keepalive();
        if radio.dwell_over() {
            radio.end_dwell();
            drain_events(&mut radio, &mut hub);
            #[cfg(feature = "espnow")]
            if let Some(u) = up.as_mut()
                && u.due(&hub)
                && let Some(a) = u.window(&mut radio, &mut hub).await
            {
                apply_assignment(&mut radio, &mut hub, &a);
            }
            radio.next_hop();
        } else if !radio.hopping {
            // Parked: send whenever due.
            #[cfg(feature = "espnow")]
            if let Some(u) = up.as_mut()
                && u.due(&hub)
                && let Some(a) = u.window(&mut radio, &mut hub).await
            {
                apply_assignment(&mut radio, &mut hub, &a);
            }
        }
        #[cfg(feature = "wirebus")]
        {
            // Hand queued frames to the bus task; it sends them when polled.
            while let Some(f) = hub.front() {
                if hub::BUS_OUT.try_send(f.clone()).is_err() {
                    break;
                }
                hub.pop();
            }
            while let Ok(a) = hub::BUS_COMMANDS.try_receive() {
                hub.commands += 1;
                apply_assignment(&mut radio, &mut hub, &a);
            }
        }
        #[cfg(not(any(feature = "espnow", feature = "wirebus")))]
        while hub.pop().is_some() {}
        if Instant::now() >= next_script {
            next_script = Instant::now() + Duration::from_millis(500);
            if let Some(line) = script.next() {
                println!(r#"{{"t":"boot_cmd","cmd":"{line}"}}"#);
                match console::parse(line) {
                    Ok(Some(Cmd::Raw(c))) => radio.command(c),
                    Ok(Some(Cmd::Local(l))) => local(&mut radio, &mut hub, l),
                    Ok(None) => {}
                    Err(e) => println!(r#"{{"t":"err","what":"cmd","e":"{e}"}}"#),
                }
            }
        }
        if last_stats.elapsed() >= Duration::from_secs(1) {
            last_stats = Instant::now();
            // Probe between the pieces of the once-a-second work: the ring holds
            // only 1.43 ms, and the gain re-force plus a ~450-byte stats line
            // over USB took about 2 ms, enough to fall a lap behind every second.
            radio.tick();
            radio.poll();
            radio.stats_paced();
            radio.poll();
            link_line(&mut radio, &hub);
            #[cfg(feature = "wirebus")]
            out::paced(node_common::bus::json_line().as_str(), 128, || radio.probe());
        }
    }
}

// Built here, not in `main`: a value built in main's frame keeps its slot
// there for the whole run even after it is moved into a Box.
#[inline(never)]
fn new_radio() -> alloc::boxed::Box<radio::Radio> {
    alloc::boxed::Box::new(radio::Radio::new(radio::Settings::default()))
}

#[inline(never)]
fn new_scratch() -> alloc::boxed::Box<fpvscan::Scratch> {
    alloc::boxed::Box::new(fpvscan::Scratch::new())
}

#[inline(never)]
fn new_dji_scratch() -> alloc::boxed::Box<djilink::Scratch> {
    alloc::boxed::Box::new(djilink::Scratch::new())
}

#[inline(never)]
fn new_hub(role: link::NodeRole) -> alloc::boxed::Box<hub::Hub> {
    alloc::boxed::Box::new(hub::Hub::new(role))
}

#[cfg(feature = "espnow")]
#[inline(never)]
fn new_uplink(now: esp_radio::esp_now::EspNow) -> alloc::boxed::Box<uplink::Uplink> {
    alloc::boxed::Box::new(uplink::Uplink::new(now))
}

#[inline(never)]
fn print_plan(radio: &radio::Radio) {
    let mut line = radio::Line::new();
    use core::fmt::Write as _;
    let _ = write!(line, r#"{{"t":"plan","role":"{}","id":{},"hop":{},"hops":["#, hub::role_name(radio.role), radio.plan_id, radio.hopping);
    for (k, (f, ms)) in radio.plan().iter().enumerate() {
        let _ = write!(line, "{}[{f},{ms}]", if k == 0 { "" } else { "," });
    }
    println!("{}]}}", line.as_str());
}

/// The once-a-second link counters, paced like the stats line. Its own
/// function so its 2 KiB line buffer is not part of main's future.
#[inline(never)]
fn link_line(radio: &mut radio::Radio, hub: &hub::Hub) {
    let mut line = radio::Line::new();
    use core::fmt::Write as _;
    let _ = write!(
        line,
        r#"{{"t":"link","stack_free":{},"queued":{},"waiting":{},"dropped":{},"droneid":{},"rf":{},"threats":{},"hil_frames":{},"commands":{},"bad_commands":{}}}"#,
        stack_free(),
        hub.queued,
        hub.len(),
        hub.dropped,
        hub.droneid,
        hub.rf,
        hub.threats,
        hub.hil_frames,
        hub.commands,
        hub.bad_commands
    );
    out::paced(line.as_str(), 128, || radio.probe());
}

/// Hand the radio's events to the hub.
fn drain_events(radio: &mut radio::Radio, hub: &mut hub::Hub) {
    for e in radio.events.iter().cloned() {
        match e {
            radio::Event::DroneId { s, hil } => hub.droneid(s, hil),
            radio::Event::Rf { s, mhz } => hub.rf(s, mhz),
            radio::Event::Threat(s) => hub.threat(s),
        }
    }
    radio.events.clear();
}

fn apply_assignment(radio: &mut radio::Radio, hub: &mut hub::Hub, a: &link::Assignment) {
    if radio.pinned {
        return; // a console `role` holds the job (`role auto` releases it)
    }
    let same = a.role == radio.role && a.id == radio.plan_id && radio.hopping;
    let ok = radio.assign(a.role, &a.plan, a.id);
    if !same {
        println!(
            r#"{{"t":"assign","role":"{}","id":{},"hops":{},"applied":{ok}{}}}"#,
            hub::role_name(a.role),
            a.id,
            a.plan.len(),
            if ok { "" } else { r#","note":"not a raw-capture role: this image keeps its job""# }
        );
    }
    hub.set(radio.role, radio.plan_id);
}

/// A node-sdr console command (beyond rawcap's capture commands).
fn local(radio: &mut radio::Radio, hub: &mut hub::Hub, l: Local) {
    match l {
        Local::Role(Some(r)) => {
            radio.set_role(r);
            radio.builtin_plan();
            radio.pinned = true;
            println!(r#"{{"t":"ok","role":"{}","hops":{},"pinned":true}}"#, hub::role_name(r), radio.plan().len());
        }
        Local::Role(None) => {
            radio.pinned = false;
            println!(r#"{{"t":"ok","role":"auto"}}"#);
        }
        Local::Hop(on) => {
            radio.hopping = on && !radio.plan().is_empty();
            println!(r#"{{"t":"ok","hop":{}}}"#, radio.hopping);
        }
        Local::Plan => print_plan(radio),
        Local::Jam => radio.jam_report(),
        Local::Edge(on) => {
            radio.edge_on = on;
            println!(r#"{{"t":"ok","edge":{on}}}"#);
        }
        Local::FpvVerbose(on) => {
            radio.fpv_verbose = on;
            println!(r#"{{"t":"ok","fpv_verbose":{on}}}"#);
        }
        Local::Uplink(_) => {
            if !cfg!(feature = "espnow") {
                println!(r#"{{"t":"err","what":"uplink","note":"built without the espnow feature"}}"#);
            }
        }
        Local::Assign(a) => apply_assignment(radio, hub, &a),
        // Bench only (docs/BENCH.md "Crash recovery"). Runs in the capture
        // loop, so a hang here stops the watchdog's feeder.
        #[cfg(feature = "faulttest")]
        Local::Fault(true) => panic!("faulttest: panic requested on the console"),
        #[cfg(feature = "faulttest")]
        Local::Fault(false) => {
            println!(r#"{{"t":"faulttest","what":"hang","note":"spinning; the watchdog should reset the chip"}}"#);
            #[allow(clippy::empty_loop)]
            loop {
                core::hint::spin_loop();
            }
        }
        Local::HilRun { len, crc } => {
            let Some(data) = console::stage(len) else {
                return println!(r#"{{"t":"err","what":"hil","note":"no staging buffer or length too big"}}"#);
            };
            let mut c = rawcap::crc32::Crc32::new();
            c.update(data);
            let got = c.finish();
            if got != crc {
                return println!(r#"{{"t":"hil","ok":false,"crc":"{got:08x}","want":"{crc:08x}","note":"staging corrupt: resend"}}"#);
            }
            let frames0 = hub.hil_frames;
            let t0 = esp_hal::time::Instant::now();
            let (rises, accepted, matched, crc_ok, ms, intact) = radio.hil_run(data);
            let total_ms = t0.elapsed().as_millis();
            drain_events(radio, hub);
            println!(
                r#"{{"t":"hil","ok":true,"bytes":{len},"rises":{rises},"accepted":{accepted},"matched":{matched},"crc_ok":{crc_ok},"frames":{},"decode_ms":{ms},"total_ms":{total_ms},"intact":{intact},"stack_free":{}}}"#,
                hub.hil_frames - frames0,
                stack_free()
            );
        }
    }
}

/// Read console lines from USB and queue commands for the capture loop.
/// `@F` lines (HIL sample data) are written straight to the staging buffer.
/// Reads as fast as the host sends while data is arriving.
#[embassy_executor::task]
async fn console(mut rx: UsbSerialJtagRx<'static, Blocking>, _tx: UsbSerialJtagTx<'static, Blocking>) {
    let mut line = LineBuf::new();
    let mut buf = [0u8; 64];
    loop {
        let n = rx.drain_rx_fifo(&mut buf);
        for &b in &buf[..n] {
            match line.push(b) {
                Some(Ok(l)) if l.starts_with("@F ") => {
                    if cfg!(feature = "bringup") {
                        console::feed_line(l)
                    } else {
                        console::FEED_ERR.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                        println!(r#"{{"t":"err","what":"cmd","e":"{}"}}"#, console::NEEDS_BRINGUP);
                    }
                }
                Some(Ok(l)) => match console::parse(l) {
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
        if n == 0 {
            Timer::after(Duration::from_millis(20)).await;
        } else {
            Timer::after(Duration::from_micros(200)).await;
        }
    }
}
