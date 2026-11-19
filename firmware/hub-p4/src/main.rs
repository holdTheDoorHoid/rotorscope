//! Rotorscope hub on the Elecrow CrowPanel Advanced 5" (ESP32-P4 v1.3).
//!
//! Bring-up step 1 (see BRINGUP.md): the patched esp-hal boots on this chip,
//! GPIO routing works, and the board's I2C bus answers: touch controller and
//! backlight MCU are found and the backlight takes writes.
//!
//! The board I2C runs bit-banged (`bb`). The esp-hal I2C controller driver
//! starts no transfer at all on this pre-v3 chip (clock, reset and pins all
//! check out; every transaction times out with nothing on the bus). Touch and
//! backlight need a few hundred bytes a second, so this costs nothing yet.

#![no_std]
#![no_main]

/// Log a line on the console without blocking the loop (see `console.rs`).
macro_rules! println {
    ($($arg:tt)*) => { $crate::console::print_fmt(format_args!($($arg)*)) };
}
pub(crate) use println;

#[cfg(all(feature = "selftest", feature = "gpsloop"))]
compile_error!("`selftest` and `gpsloop` both use UART2; build one at a time");
#[cfg(all(feature = "modesloop", any(feature = "gpsloop", feature = "webheadloop")))]
compile_error!("`modesloop` drives the ADSBee pad from UART4, which `gpsloop` and `webheadloop` use too; build one at a time");
#[cfg(all(feature = "webheadloop", any(feature = "selftest", feature = "gpsloop")))]
compile_error!("`webheadloop` needs the web-head feed on UART2, which `selftest` and `gpsloop` take");

mod adsbee;
mod aero;
mod app;
#[cfg(feature = "benchlog")]
mod bench;
mod bb;
mod bus;
mod civil;
mod console;
mod display;
mod downlink;
mod evlog;
mod faa;
mod fault;
mod flashrom;
mod gps;
mod gpsmem;
#[cfg(feature = "gpsloop")]
mod gpsloop;
#[cfg(feature = "modesloop")]
mod modesloop;
mod linkauth;
mod links;
mod psram_test;
mod report;
mod rgb;
mod sd;
mod sdbb;
mod site;
mod touch;
mod uartring;
mod webhead;
#[cfg(feature = "webheadloop")]
mod webheadloop;

use esp_backtrace as _;
use esp_hal::{
    gpio::{Flex, Pin},
    main,
    time::{Duration, Instant},
};
esp_bootloader_esp_idf::esp_app_desc!();

/// Strings anyone holding the ELF can find (`grep -a`): the bench tag
/// tools/dev-board.sh looks for (`bench-build:none` for a normal build) and
/// the source commit (`-dirty` if tracked files were changed). The hello
/// line and the evidence boot event say the same. Each ends in a NUL so
/// whatever the linker puts next cannot run on into the tag.
#[used]
static BUILD_TAGS: [&str; 2] =
    [concat!(env!("ROTORSCOPE_BENCH_TAG"), "\0"), concat!("rotorscope-git:", env!("ROTORSCOPE_GIT"), "\0")];

/// Unattended hub: after a panic (backtrace already printed on the USB-C
/// console), reboot rather than sit halted on a stale screen (`fault.rs`).
#[unsafe(no_mangle)]
extern "Rust" fn custom_halt() -> ! {
    fault::reboot_after_panic()
}

/// STC8 helper MCU on the board I2C (SDA GPIO45, SCL GPIO46): register 0x20
/// takes the backlight brightness, 0..=100.
const STC8_ADDR: u8 = 0x2F;
const STC8_BACKLIGHT: u8 = 0x20;
/// GT911 touch controller (0x5D when INT is low at reset, else 0x14).
const GT911_ADDRS: [u8; 2] = [0x5D, 0x14];


/// GPIO48 (UART1 header "RX") for the node link's receiver (UART3 peripheral).
/// With `selftest` the same pad is also driven by UART2's transmitter, so the
/// pin is handed out twice on purpose.
fn unsafe_link_rx() -> esp_hal::peripherals::GPIO48<'static> {
    // SAFETY: bring-up only. Without `selftest`, `main` never uses GPIO48
    // otherwise; with it, a second user drives the pad as output (see below).
    unsafe { esp_hal::peripherals::GPIO48::steal() }
}

#[cfg(feature = "selftest")]
mod selftest {
    //! Sends a fixed sequence (Via, Hello, Via, Status, Via, Detection, a
    //! direct Stats) every 100 ms from UART2 TX on GPIO48, the node-link RX
    //! pin, and reads the hub's commands back from the node-link TX pad (GPIO47)
    //! on UART2 RX, decoding them as the gateway would.
    use esp_hal::time::{Duration, Instant};
    use esp_hal::uart::{Config, Uart};
    use esp_hal::Blocking;

    pub struct Sender {
        uart: Uart<'static, Blocking>,
        last: Instant,
        dec: link::CommandDecoder,
        to: u32,
        assign: u32,
        bad: u32,
        last_print: Instant,
        last_assign: Option<(link::NodeRole, u16, usize)>,
    }

    impl Sender {
        pub fn new(uart: esp_hal::peripherals::UART2<'static>) -> Self {
            // SAFETY: bring-up only: GPIO47 is the node link's TX pad; we only listen to it.
            let pin47 = unsafe { esp_hal::peripherals::GPIO47::steal() };
            let uart = Uart::new(uart, Config::default().with_baudrate(super::links::BAUD))
                .expect("UART2")
                .with_tx(super::unsafe_link_rx())
                .with_rx(pin47);
            Self {
                uart,
                last: Instant::now(),
                dec: link::CommandDecoder::new(),
                to: 0,
                assign: 0,
                bad: 0,
                last_print: Instant::now(),
                last_assign: None,
            }
        }

        pub fn tick(&mut self) {
            let mut buf = [0u8; 128];
            while let Ok(n @ 1..) = self.uart.read_buffered(&mut buf) {
                let (to, assign, bad, last) = (&mut self.to, &mut self.assign, &mut self.bad, &mut self.last_assign);
                self.dec.feed(&buf[..n], |r| match r {
                    Ok(link::Command::To { .. }) => *to += 1,
                    Ok(link::Command::Assign(a)) => {
                        *assign += 1;
                        *last = Some((a.role, a.id, a.plan.len()));
                    }
                    // Bus polls, join slots and grants (protocol 10).
                    Ok(_) => {}
                    Err(_) => *bad += 1,
                });
            }
            if self.last_print.elapsed() >= Duration::from_millis(5000) {
                self.last_print = Instant::now();
                crate::println!(
                    "hub-p4: selftest loopback to={} assign={} bad={} last={:?}",
                    self.to, self.assign, self.bad, self.last_assign
                );
            }
            if self.last.elapsed() < Duration::from_millis(100) {
                return;
            }
            self.last = Instant::now();
            let node = [0x02, 0, 0, 0, 0, 0x01];
            let mut msgs = heapless::Vec::new();
            let _ = msgs.push([0x5A; 25]);
            let frames = [
                link::Frame::Via { node, rssi: -40 },
                link::Frame::Hello {
                    role: link::NodeRole::Wifi24,
                    protocol: link::PROTOCOL_VERSION,
                    firmware: heapless::String::try_from("selftest").unwrap(),
                },
                link::Frame::Via { node, rssi: -41 },
                link::Frame::Status {
                    role: link::NodeRole::Wifi24,
                    can: link::NodeRole::Wifi24.bit() | link::NodeRole::Wifi5.bit() | link::NodeRole::DroneId.bit(),
                    plan: 0,
                },
                link::Frame::Via { node, rssi: -41 },
                link::Frame::Detection(link::Detection {
                    transport: rid::Transport::WifiBeacon,
                    channel: 6,
                    rssi: -41,
                    mac: [0x60, 0x60, 0x1F, 0, 0, 1],
                    counter: 7,
                    uptime_us: 123,
                    messages: msgs,
                }),
                link::Frame::Stats { uptime_us: 1, frames_seen: 2, rid_found: 3, dropped: 0, by_transport: heapless::Vec::new() },
            ];
            for f in &frames {
                let mut buf = [0u8; link::MAX_FRAME];
                if let Ok(b) = link::encode(f, &mut buf) {
                    let _ = self.uart.write(b);
                }
            }
        }
    }
}

#[main]
fn main() -> ! {
    // 360 MHz (the pre-v3 maximum). esp-hal defaults to 90 MHz on the P4.
    let p = esp_hal::init(esp_hal::Config::default().with_cpu_clock(esp_hal::clock::CpuClock::max()));
    // Console first: from here on log lines never block the loop.
    console::init(p.UART0, p.GPIO37, p.GPIO38);
    println!(
        "hub-p4: hello, git {}, chip revision {:?}, last reset: {} (cause {:#04x}), evidence key: {} ({}), bench: [{}]{}",
        evlog::GIT,
        esp_hal::efuse::chip_revision(),
        fault::reset_reason(),
        fault::reset_code(),
        evlog::built_key_class().name(),
        evlog::built_key_class().note(),
        evlog::BENCH,
        if cfg!(feature = "faulttest") { ", FAULT TEST BUILD: console 'fault panic' / 'fault hang'" } else { "" }
    );
    if !evlog::BENCH.is_empty() {
        println!("hub-p4: BENCH BUILD ({}): fakes data or opens debug doors; never leave it on a fielded hub", evlog::BENCH);
    }
    // SAFETY: read-only CACHE register (IDF 5.4.2 cache_reg.h, rev<3 map):
    // L2_CACHE_CACHESIZE_CONF, bit 10 = 256 KB.
    let l2 = unsafe { (0x3FF1_0278 as *const u32).read_volatile() };
    let sp: u32;
    // SAFETY: reads the stack pointer only.
    unsafe { core::arch::asm!("mv {0}, sp", out(reg) sp) };
    println!("hub-p4: L2 cachesize conf {l2:#x}, sp {sp:#x} (must be below 0x4ff80000)");
    // The stack must not be in the L2 cache's RAM (see BRINGUP.md "Stack and
    // the L2 cache"); the memory.x fix guarantees it, this catches a regression.
    assert!(l2 == 0x400 && sp < 0x4FF8_0000, "stack inside the L2 cache or unexpected cache split");

    let mut i2c = bb::BbI2c::new(Flex::new(p.GPIO45), Flex::new(p.GPIO46));

    let psram = psram_test::run(p.PSRAM, esp_hal::psram::SpiRamFreq::Mhz80);
    println!("hub-p4: psram ok = {}", psram.is_some());
    // WP-13e: kit records and boot counter from flash, keys derived once,
    // the link's state in PSRAM. Before the node link's UART interrupt and
    // the panel's DMA start (linkauth.rs, flashrom.rs).
    let link_parts = linkauth::boot(psram);
    let boot_counter = link_parts.counter.epoch();

    println!("hub-p4: i2c idle sda/scl high = {:?}", i2c.levels());
    for addr in 0x08u8..0x78 {
        if i2c.write(addr, &[]) {
            let what = match addr {
                STC8_ADDR => "backlight MCU",
                a if GT911_ADDRS.contains(&a) => "touch GT911",
                _ => "?",
            };
            println!("hub-p4: i2c 0x{addr:02x} {what}");
        }
    }
    println!("hub-p4: backlight 100: ack {}", i2c.write(STC8_ADDR, &[STC8_BACKLIGHT, 100]));
    println!("hub-p4: touch product id {:?}", touch::Gt911::product_id(&mut i2c).map(|id| id.map(char::from)));

    // Step 6: RGB panel from a PSRAM framebuffer (overwrites the PSRAM test
    // pattern). Needs no CPU per frame once started.
    let mut panel = psram.map(|(base, _)| {
        let pins = rgb::Pins {
            de: p.GPIO2.degrade(),
            pclk: p.GPIO3.degrade(),
            hsync: p.GPIO40.degrade(),
            vsync: p.GPIO41.degrade(),
            data: [
                p.GPIO8.degrade(), p.GPIO7.degrade(), p.GPIO6.degrade(), p.GPIO5.degrade(), p.GPIO4.degrade(),
                p.GPIO14.degrade(), p.GPIO13.degrade(), p.GPIO12.degrade(), p.GPIO11.degrade(), p.GPIO10.degrade(),
                p.GPIO9.degrade(),
                p.GPIO19.degrade(), p.GPIO18.degrade(), p.GPIO17.degrade(), p.GPIO16.degrade(), p.GPIO15.degrade(),
            ],
        };
        let t = Instant::now();
        let panel = rgb::Panel::start(pins, base.cast());
        println!(
            "hub-p4: rgb started in {} ms: fb {:p}, descs {:#x}, {}x{} total {}x{}, pclk {} Hz -> {} mHz frame",
            t.elapsed().as_millis(), base, rgb::Panel::descs_base(), rgb::H_RES, rgb::V_RES, rgb::H_TOTAL, rgb::V_TOTAL,
            rgb::PCLK_HZ, rgb::PCLK_HZ as u64 * 1000 / (rgb::H_TOTAL * rgb::V_TOTAL) as u64
        );
        panel
    });

    // Node link on the UART1 header (RX GPIO48, TX GPIO47), driven by the
    // UART3 peripheral; the GPS has the UART3-IN header. Swapped 2026-11-17.
    let mut gw = links::GatewayLink::new(p.UART3, unsafe_link_rx(), p.GPIO47, link_parts);
    println!(
        "hub-p4: node link on the UART1 header RX GPIO48, TX GPIO47 at {} baud: {}",
        links::BAUD,
        if bus::BUS_DEFAULT { "wired bus (an ESP-NOW gateway is recognised and read as before)" } else { "gateway / point-to-point only" }
    );
    let mut down = downlink::Downlink::new();
    // Aero (LR2021) nodes: their frame queue and the 1090 table (the
    // ADSBee's too with `modes`) in PSRAM (aero.rs).
    let (air_q, mut aero_es, control_table, area_watch) = aero::split(aero::store(psram));
    println!(
        "hub-p4: aero frames queued in {}; 1090 table {}",
        if air_q.is_some() { "PSRAM" } else { "the link queue (no PSRAM)" },
        match (aero_es.is_some(), cfg!(feature = "modes")) {
            (true, true) => "in PSRAM, shared by the ADSBee and aero nodes",
            (true, false) => "in PSRAM, for aero nodes",
            (false, _) => "missing (no PSRAM): 1090 frames counted, not decoded",
        }
    );
    gw.air = air_q;
    // The web head (WEBHEAD.md): UART2 TX on GPIO31. The bench self-tests
    // `selftest` and `gpsloop` need UART2 themselves, so they build without it.
    #[cfg(not(any(feature = "selftest", feature = "gpsloop")))]
    {
        gw.feed = Some(webhead::Feed::new(p.UART2, p.GPIO31));
        println!("hub-p4: web head feed on UART2 TX GPIO{} at {} baud", webhead::TX_PIN, webhead::BAUD);
    }

    // GPS on the UART3-IN header (RX GPIO28 / TX GPIO27, UART1 peripheral),
    // PPS on GPIO29; ADSBee on
    // UART4 RX GPIO30. See gps.rs, adsbee.rs and PARITY.md.
    let gps_uart = esp_hal::uart::Uart::new(p.UART1, uartring::gps_rx::config(gps::BAUDS[0]))
        .expect("UART1")
        .with_rx(p.GPIO28)
        .with_tx(p.GPIO27);
    uartring::gps_rx::install(gps_uart);
    let mut io = esp_hal::gpio::Io::new(p.IO_MUX);
    gps::pps_init(&mut io, Flex::new(p.GPIO29));
    let mut gps = gps::Gps::new();
    #[cfg(not(feature = "webheadloop"))]
    let adsb_uart = esp_hal::uart::Uart::new(p.UART4, uartring::adsb_rx::config(adsb::DEFAULT_BAUD))
        .expect("UART4")
        .with_rx(p.GPIO30);
    // Web-head loop-back test: UART4 listens to the web-head TX pad instead
    // (see webheadloop.rs); the ADSBee input is fed from inside the chip.
    #[cfg(feature = "webheadloop")]
    let adsb_uart = esp_hal::uart::Uart::new(p.UART4, uartring::adsb_rx::config(webhead::BAUD))
        .expect("UART4")
        // SAFETY: bench only; GPIO31 is driven by UART2 TX and read here on purpose.
        .with_rx(unsafe { esp_hal::peripherals::GPIO31::steal() });
    // Loop-back test: UART4 also drives its own RX pad.
    #[cfg(any(feature = "gpsloop", feature = "modesloop"))]
    // SAFETY: bench only; GPIO30 is shared with UART4 RX on purpose.
    let adsb_uart = adsb_uart.with_tx(unsafe { esp_hal::peripherals::GPIO30::steal() });
    uartring::adsb_rx::install(adsb_uart);
    let mut adsbee = adsbee::Adsbee::default();
    // `modes`: the ADSBee's Beast frames and aero nodes' 1090 frames build
    // one picture in the PSRAM table.
    #[cfg(feature = "modes")]
    {
        adsbee.raw.table = aero_es.take();
    }
    println!("hub-p4: gps on the UART3-IN header RX GPIO28 TX GPIO27 (hunting {:?} baud), PPS GPIO29; adsbee on UART4 RX GPIO30 at {}", gps::BAUDS, adsb::DEFAULT_BAUD);
    #[cfg(feature = "gpsloop")]
    let mut gpsloop = gpsloop::Loop::new(p.UART2);
    #[cfg(feature = "modesloop")]
    let mut modesloop = modesloop::Loop::new();
    // microSD log (1-bit native mode, bit-banged): CLK 43, CMD 44, D0 39.
    let mut sdlog = sd::Logger::new(Flex::new(p.GPIO43), Flex::new(p.GPIO44), Flex::new(p.GPIO39));
    // One boot counter: the rsboot value is also the evidence log's.
    sdlog.set_boot_counter(boot_counter);
    // Phase 12: the airspace pack goes to PSRAM above the framebuffer.
    // SAFETY: taken exactly once, here (a static; the rule engine itself,
    // ~84 KB, is placed in PSRAM by Faa::new at faa::ENGINE_OFFSET).
    let faa = unsafe {
        (&raw mut FAA).write(core::mem::MaybeUninit::new(faa::Faa::new(psram)));
        (*(&raw mut FAA)).assume_init_mut()
    };

    // Phase 13: the evidence log (evlog.rs). A boot id names this boot's
    // chain: the hardware RNG mixed with timing and the chip's own ids
    // (WP-14a: the RNG alone is not a proven entropy source on the P4).
    let (boot_id, rng_first) = evlog::make_boot_id(fault::reset_code());
    println!(
        "hub-p4: boot id inputs: rng first bytes {:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x} (compare across power cycles: they must differ)",
        rng_first[0], rng_first[1], rng_first[2], rng_first[3], rng_first[4], rng_first[5], rng_first[6], rng_first[7]
    );
    let mut ev = evlog::Evidence::new(psram, boot_id);
    let chip = esp_hal::efuse::chip_revision();
    {
        let fw = ev.fw_hash();
        println!(
            "hub-p4: evidence boot id {:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}, firmware sha256 {:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}..",
            boot_id[0], boot_id[1], boot_id[2], boot_id[3], boot_id[4], boot_id[5], boot_id[6], boot_id[7],
            fw[0], fw[1], fw[2], fw[3], fw[4], fw[5], fw[6], fw[7]
        );
    }
    ev.ctx.uptime_ms = app::now_ms();
    ev.boot(
        (chip.major, chip.minor),
        fault::reset_reason(),
        sdlog.counter_source(),
        evidence::format::boot::BOOT_ID_MIXED,
    );
    let mut note: Option<heapless::String<120>> = None;
    // Console `L`: one site-file line being typed (node position / mute).
    let mut site_line: Option<heapless::String<120>> = None;
    let mut site = site::Site::default();
    let mut gpsmem = gpsmem::GpsMem::default();
    #[cfg(feature = "gpsloop")]
    gpsloop::boot_scenario(&mut gps);

    #[cfg(feature = "selftest")]
    let mut selftest = selftest::Sender::new(p.UART2);
    #[cfg(feature = "webheadloop")]
    let mut whloop = webheadloop::Loop::new();

    let tracker = place_tracker(psram);
    let band = place_band(psram);
    let mut app = app::App::new(tracker, band);
    // Protocol 12: drone control links from the same nodes, in PSRAM.
    if control_table.is_none() {
        println!("hub-p4: no PSRAM: control-link frames are logged but not tabled");
    }
    app.control = control_table;
    // GPS interference in the area (aircraft integrity, drones jumping
    // together), also in PSRAM.
    if area_watch.is_none() {
        println!("hub-p4: no PSRAM: no area GPS check");
    }
    app.area = area_watch;

    #[cfg(feature = "fakenode")]
    let mut last_fake = Instant::now();
    #[cfg(feature = "fakedrone")]
    let mut last_drone = Instant::now();
    #[cfg(feature = "benchlog")]
    let mut bench = bench::Bench::default();
    let mut last = Instant::now();
    let mut last_report = 0u64;
    // The backlight level last written (0: none yet; boot wrote 100%).
    let mut backlight = 0u8;
    let mut loops = 0u32;
    let mut max_gap_us = 0u64;
    let mut prev = Instant::now();
    // Hang watchdog (fault.rs): armed here, after the slow parts of boot,
    // and fed once per loop below.
    match fault::arm() {
        Ok(s) => println!("hub-p4: watchdog on: resets the chip if the main loop stops for {s:.1} s"),
        Err((why, v)) => println!("hub-p4: watchdog LEFT OFF: {why} ({v:.2})"),
    }
    loop {
        fault::feed();
        let now = Instant::now();
        max_gap_us = max_gap_us.max((now - prev).as_micros());
        loops += 1;
        let ms = app::now_ms();
        console::pump();
        gw.poll(ms);
        ev.set_context(&app, &gps, ms);
        {
            #[cfg(feature = "modes")]
            let es = adsbee.raw.table.as_deref_mut();
            #[cfg(not(feature = "modes"))]
            let es = aero_es.as_deref_mut();
            // FLARM needs GPS time: `gps.time` is only ever set from the GPS.
            let mut actx = aero::Ctx { gps_utc_ms: gps.utc_ms(ms), es };
            app.ingest(&mut gw, &mut ev, &mut actx, ms);
        }
        down.tick(&mut gw, adsbee.heard_within(ms, ADSBEE_COVERS_MS), ms);
        gps.tick(ms);
        #[cfg(not(feature = "webheadloop"))]
        adsbee.tick(app.tracker, ms, gw.feed.as_mut());
        #[cfg(feature = "webheadloop")]
        whloop.tick(&mut adsbee, app.tracker, &mut gw, ms);
        let mut lines = Lines { note: &mut note, site: &mut site_line, gm: &mut gpsmem };
        if let Some(line) = command(&mut app, &mut gps, faa, &mut ev, &mut sdlog, &mut down, &mut lines, ms) {
            let own = app.own_pos(&gps, ms).map(|p| (p.lat, p.lon));
            site.operator_line(&line, &mut app, &gw, own, &mut sdlog, &mut ev, ms);
        }
        ev.tick(ms);
        if ms.saturating_sub(last_report) >= 1000 {
            last_report = ms;
            app.housekeeping(&gps, ms);
            // Protocol 11: the hub's UTC to every node that wants it (aero
            // nodes follow FLARM/OGN hops with it), on the bus or through
            // the gateway. Stamped again as it is written (bus.rs).
            if let (Some(a), Some(utc)) = (gps.time, gps.utc_ms(ms)) {
                gw.time_sync(utc, ms, a.pps);
            }
            #[cfg(not(feature = "modes"))]
            app.aero.report(aero_es.as_deref_mut(), app.tracker, gw.feed.as_mut(), ms);
            app.aero.print(if app.aero.has_keys() { "loaded" } else { sdlog.flarm_key_state }, bus::syncs(), ms);
            site.apply(&mut app, &gw);
            control_settings(&site, &mut down, &mut app, &mut ev);
            ev.watch(&gps, &gw.nodes, faa.pack().zip(faa.pack_sha256()), ms);
            gpsmem.tick(&mut gps, &mut sdlog, &mut ev, ms);
            faa.tick(app.tracker, ms, gps.utc_ms(ms));
            let (frames, lost) = (gw.stats.frames, gw.stats.corrupt + gw.stats.overflow + gw.stats.uart_err + gw.stats.ring_full);
            let max_age = app.tracker.config().own_fix_max_age_ms;
            if let Some(f) = gw.feed.as_mut() {
                f.tick(&gps, max_age, frames, lost, ms);
                faa.forward(app.tracker, f);
            }
            report::tick(&mut app, &gw, &gps, &adsbee, &sdlog, faa, ms);
        }
        app.touch(&mut i2c, ms);
        // A settings action: done, then the result back to the screen
        // (it says "done" only when the hub says so).
        let result = match app.take_request() {
            Some(screen::Request::HubMoved) => Some(gpsmem.forget_home(&mut gps, &mut sdlog, &mut ev, "screen", ms)),
            Some(screen::Request::AcceptHome) => Some(gpsmem.confirm_home(&mut gps, &mut sdlog, &mut ev, "screen", ms)),
            Some(screen::Request::MuteAlarms { seen_ms }) => Some(site.silence_seen(&mut app, seen_ms, false, ms, &mut ev)),
            Some(screen::Request::SnoozeAlarms { seen_ms }) => Some(site.silence_seen(&mut app, seen_ms, true, ms, &mut ev)),
            Some(screen::Request::UnmuteAlarms) => Some(site.unmute_operator(&mut app, ms, &mut ev)),
            Some(screen::Request::NodesAtHub) => {
                let own = app.own_pos(&gps, ms).map(|p| (p.lat, p.lon));
                Some(site.all_nodes_here(&mut app, &gw, own, &mut sdlog, &mut ev, site::From::Screen))
            }
            None => None,
        };
        if let Some(r) = result {
            app.action_result(r);
        }
        // The backlight follows Brightness (capped in Red Night): written
        // when the level changes.
        let level = app.backlight_level();
        if level != backlight {
            let pct = screen::backlight_pct(level);
            let ack = i2c.write(STC8_ADDR, &[STC8_BACKLIGHT, pct]);
            println!("hub-p4: backlight level {level} ({pct}%): ack {ack}");
            backlight = level;
        }
        // A finished settings edit that changed the theme: to the card.
        if let Some((before, after)) = app.take_settings_change() {
            site.settings_changed(&before, &after, &mut sdlog, &mut ev);
        }
        let utc = gps.utc_ms(ms);
        sd::set_clock(utc, ms);
        if let Some(panel) = panel.as_mut() {
            panel.poll();
            let ext = app::Ext {
                gps: gps_view(&app, &gps, ms),
                utc_ms: utc,
                sd: sdlog.ui_state(),
                adsb: Some((adsbee.status(ms), adsbee.stats.map_or(0, |s| s.uptime_s))),
                flags: Some(faa.view(app.tracker)),
                caveats: caveats(&ev, &gw, &sdlog),
            };
            app.draw(panel, &mut gw, &ext, ms, |gw| {
                gw.poll(app::now_ms());
                console::pump();
            });
        }
        // The SD step can block ~0.6 s (a card probe or the mount): when one
        // is due, a beacon first if the last is 250 ms old, so the beacon gap
        // stays under 1 s. Not on every pass: that halved the beacon period.
        if sdlog.slow_step_due(ms) {
            gw.beacon_if_older(ms, 250);
        }
        sdlog.tick(app.tracker, faa, &mut ev, ms, utc, gps.status.state != tracker::json::GpsState::Absent);
        if let Some((st, facts)) = sdlog.take_site() {
            site.load(&st, &facts, &mut app, &mut ev, ms);
        }
        if let Some(home) = site.take_home() {
            gpsmem.placed(&mut gps, &mut sdlog, &mut ev, home, ms);
        }
        if let Some(m) = sdlog.take_gps_mem() {
            gpsmem.loaded(&mut gps, &mut ev, m);
        }
        if let Some(k) = sdlog.take_o4_key() {
            let fp = k.fingerprint();
            app.o4.set_key(k);
            let mut t: heapless::String<64> = heapless::String::new();
            let _ = core::fmt::Write::write_fmt(
                &mut t,
                format_args!("O4 DroneID key installed (public fingerprint {:02x}{:02x}-{:02x}{:02x})", fp[0], fp[1], fp[2], fp[3]),
            );
            ev.event(evidence::format::EventCode::Setting, t.as_bytes(), &fp);
        }
        if let Some(k) = sdlog.take_flarm_keys() {
            app.aero.set_keys(k);
            ev.event(evidence::format::EventCode::Setting, b"FLARM legacy keys installed from FLARMKEY.TXT (contents never logged)", &[]);
        }
        #[cfg(feature = "gpsloop")]
        gpsloop.tick();
        #[cfg(feature = "modesloop")]
        modesloop.tick();
        #[cfg(feature = "benchlog")]
        bench.tick(app.tracker, &gw, ms);
        #[cfg(feature = "selftest")]
        selftest.tick();
        #[cfg(feature = "fakedrone")]
        if last_drone.elapsed() >= Duration::from_millis(1000) {
            last_drone = Instant::now();
            let node = [0x02, 0x52, 0x53, 0, 0, 0x02];
            // Remote ID Basic ID (type 0, protocol 2): serial number,
            // multirotor, "RSTEST-SD-0001".
            let mut basic = [0u8; 25];
            basic[0] = 0x02;
            basic[1] = 0x12;
            basic[2..16].copy_from_slice(b"RSTEST-SD-0001");
            let mut msgs = heapless::Vec::new();
            let _ = msgs.push(basic);
            for m in fake_flight(app::now_ms()) {
                if let Ok(raw) = rid::encode::message(&m) {
                    let _ = msgs.push(raw);
                }
            }
            let frames = [
                link::Frame::Via { node, rssi: -45 },
                link::Frame::Detection(link::Detection {
                    transport: rid::Transport::WifiBeacon,
                    channel: 6,
                    rssi: -45,
                    mac: [0x02, 0x52, 0x53, 0x0D, 0x00, 0x01],
                    counter: (app::now_ms() / 1000) as u8,
                    uptime_us: app::now_ms() * 1000,
                    messages: msgs,
                }),
            ];
            for f in &frames {
                let mut buf = [0u8; link::MAX_FRAME];
                if let Ok(b) = link::encode(f, &mut buf) {
                    gw.inject(b);
                }
            }
        }
        #[cfg(feature = "fakenode")]
        if last_fake.elapsed() >= Duration::from_millis(1000) {
            last_fake = Instant::now();
            let node = [0x02, 0x52, 0x53, 0, 0, 0x01];
            let frames = [
                link::Frame::Via { node, rssi: -40 },
                link::Frame::Hello {
                    role: link::NodeRole::Wifi24,
                    protocol: link::PROTOCOL_VERSION,
                    firmware: heapless::String::try_from("fakenode").unwrap(),
                },
                link::Frame::Via { node, rssi: -40 },
                link::Frame::Status {
                    role: link::NodeRole::Wifi24,
                    can: link::NodeRole::Wifi24.bit() | link::NodeRole::Wifi5.bit(),
                    plan: 0,
                },
            ];
            for f in &frames {
                let mut buf = [0u8; link::MAX_FRAME];
                if let Ok(b) = link::encode(f, &mut buf) {
                    gw.inject(b);
                }
            }
        }
        prev = Instant::now();
        if last.elapsed() >= Duration::from_millis(1000) {
            last = Instant::now();
            let s = &gw.stats;
            let (vs, err) = panel.as_mut().map_or((0, 0), |p| {
                let r = (p.vsync, p.dma_err_count);
                p.vsync = 0;
                p.dma_err_count = 0;
                r
            });
            println!(
                "hub-p4: link bytes={} frames={} corrupt={} uart_err={} ring_full={} direct={} refused={} nodes={} evicted={} table_full={} out_full={} tx={} tx_full={} cmds={} | tracks={} | lcd vsync={} dma_err={} drawn={} max_band={}us | loops={} max_gap={}us",
                s.bytes, s.frames, s.corrupt, s.uart_err, s.ring_full, s.direct, s.refused, gw.nodes.len(), s.evicted, s.table_full, gw.out_full,
                gw.tx_bytes, gw.tx_full, down.commands,
                app.tracker.tracks().len(), vs, err, app.frames_drawn, app.max_band_us, loops, max_gap_us
            );
            if ms / 1000 % 5 == 0 {
                print_gps_adsb(&app, &gps, &adsbee, ms);
                if let Some(f) = gw.feed.as_ref() {
                    println!(
                        "hub-p4: webhead tx={} frames={} dropped={} refused={} queued={} csbee_err={} encode_err={}",
                        f.tx_bytes, f.fwd.frames, f.fwd.dropped, f.fwd.refused, f.fwd.len(), f.csbee_err, f.fwd.encode_err
                    );
                }
            }
            #[cfg(feature = "webheadloop")]
            whloop.print();
            #[cfg(feature = "gpsloop")]
            println!("hub-p4: gpsloop emulator cmds={} bad={}", gpsloop.cmds, gpsloop.bad);
            app.frames_drawn = 0;
            app.max_band_us = 0;
            loops = 0;
            max_gap_us = 0;
        }
    }
}

/// What must stay on screen (`screen::Caveats`): the evidence key class,
/// a bench build, an open gateway link, evidence not reaching the card.
fn caveats(ev: &evlog::Evidence, gw: &links::GatewayLink<'_>, sd: &sd::Logger) -> screen::Caveats {
    screen::Caveats {
        key: match ev.key_class() {
            evidence::KeyClass::PublicTest => screen::EvidenceKey::Test,
            evidence::KeyClass::DevelopmentSoftware => screen::EvidenceKey::Development,
            evidence::KeyClass::HardwareEfuse | evidence::KeyClass::SecureElement => screen::EvidenceKey::NotShown,
        },
        bench: !evlog::BENCH.is_empty(),
        link_open: gw.link_open(),
        link: gw.link_caveats(app::now_ms()),
        evidence: sd.evidence_stop(ev),
        // Silenced alarms and the site switches: filled by `App::fill_state`
        // from the tracker and the control flags.
        ..screen::Caveats::default()
    }
}

/// What the status bar says about position (as the S3 hub's `gps_view`).
fn gps_view(app: &app::App, gps: &gps::Gps, now: u64) -> screen::Gps {
    use screen::Fix;
    use tracker::json::GpsState;
    let fix = match app.own_pos(gps, now).map(|p| p.source) {
        Some(app::OwnSource::Gps) => match gps.status.state {
            GpsState::Fix2d => Fix::Fix2D,
            _ => Fix::Fix3D,
        },
        Some(app::OwnSource::Manual) => Fix::Manual,
        None if gps.status.state == GpsState::Absent => Fix::Absent,
        None => Fix::None,
    };
    use gnss::IntegrityState as I;
    use screen::GpsIntegrity as G;
    let integrity = match gps.integrity().state {
        I::Ok => G::Ok,
        I::SuspectJamming => G::SuspectJamming,
        I::Jamming => G::Jamming,
        I::SuspectSpoofing => G::SuspectSpoofing,
        I::Spoofing => G::Spoofing,
    };
    screen::Gps {
        fix,
        sats: gps.status.sats.unwrap_or(0),
        integrity,
        untrusted_pos: gps.untrusted_pos,
        untrusted_time: gps.untrusted_time,
        home_pending: gps.home_pending().is_some(),
    }
}

/// One console command byte, if any (see `console.rs`). `n` starts an
/// operator note: the text up to Enter goes into the evidence log.
/// Console text being typed: an evidence note (`n`) or a site line (`L`).
struct Lines<'a> {
    note: &'a mut Option<heapless::String<120>>,
    site: &'a mut Option<heapless::String<120>>,
    /// GPS memory and the operator's GPS decisions (`m`, `H`, `T`).
    gm: &'a mut gpsmem::GpsMem,
}

/// `faulttest` builds: the console line being typed after `f`.
#[cfg(feature = "faulttest")]
static mut FAULT_LINE: Option<heapless::String<16>> = None;

/// `faulttest`: one console byte while a `fault ...` line is being typed;
/// true if it was taken. On Enter, `fault panic` panics and `fault hang`
/// spins in the main loop until the watchdog resets the chip.
#[cfg(feature = "faulttest")]
fn fault_line(c: u8) -> bool {
    // SAFETY: only the main loop touches it (single-threaded, no interrupt uses it).
    let line = unsafe { &mut *(&raw mut FAULT_LINE) };
    let Some(l) = line.as_mut() else {
        if c != b'f' {
            return false;
        }
        let mut l = heapless::String::new();
        let _ = l.push('f');
        *line = Some(l);
        return true;
    };
    match c {
        b'\r' | b'\n' => {
            match line.take().as_deref() {
                Some("fault panic") => panic!("faulttest: panic requested on the console"),
                Some("fault hang") => {
                    esp_println::println!("hub-p4: faulttest hang: spinning; the watchdog should reset the chip within {} s", fault::WATCHDOG.as_secs());
                    #[allow(clippy::empty_loop)]
                    loop {
                        core::hint::spin_loop();
                    }
                }
                _ => println!("hub-p4: cmd faulttest: type 'fault panic' or 'fault hang', then Enter"),
            }
        }
        0x1b => *line = None,
        c if c.is_ascii() && !c.is_ascii_control() => {
            if l.push(c as char).is_err() {
                *line = None;
            }
        }
        _ => {}
    }
    true
}

/// The control-link opt-ins from the site settings to the coverage
/// coordinator (which puts them in the LR2021 nodes' plans and, protocol
/// 13, the Wi-Fi nodes' plans: content decoding also turns on their scan
/// for hostile MAVLink on open Wi-Fi). A change is
/// printed and logged as a Setting event, whatever made it (the card's
/// file at mount, or a console line, which the site code logs as well).
#[inline(never)]
fn control_settings(site: &site::Site, down: &mut downlink::Downlink, app: &mut app::App, ev: &mut evlog::Evidence) {
    let (ids, content) = site.control();
    app.control_flags = (ids, content);
    if !down.set_control(ids, content) {
        return;
    }
    let onoff = |b: bool| if b { "on" } else { "off" };
    let mut t: heapless::String<96> = heapless::String::new();
    let _ = core::fmt::Write::write_fmt(
        &mut t,
        format_args!("control links: read IDs {}, decode content {} (LR2021 and Wi-Fi nodes' plans)", onoff(ids), onoff(content)),
    );
    ev.event(evidence::EventCode::Setting, t.as_bytes(), &[ids as u8, content as u8]);
    println!("hub-p4: {t}");
}

/// Returns a finished site line (console `L`) for the caller to apply.
fn command(
    app: &mut app::App,
    gps: &mut gps::Gps,
    faa: &faa::Faa,
    ev: &mut evlog::Evidence,
    sdlog: &mut sd::Logger,
    down: &mut downlink::Downlink,
    lines: &mut Lines<'_>,
    now: u64,
) -> Option<heapless::String<120>> {
    let c = console::read_command()?;
    #[cfg(feature = "faulttest")]
    if lines.site.is_none() && lines.note.is_none() && fault_line(c) {
        return None;
    }
    if let Some(n) = lines.site.as_mut() {
        match c {
            b'\r' | b'\n' => return lines.site.take(),
            0x1b => {
                *lines.site = None;
                println!("hub-p4: cmd site line cancelled");
            }
            c if c.is_ascii() && !c.is_ascii_control() => {
                let _ = n.push(c as char);
            }
            _ => {}
        }
        return None;
    }
    let note = &mut *lines.note;
    if let Some(n) = note.as_mut() {
        match c {
            b'\r' | b'\n' => {
                let text = note.take().unwrap_or_default();
                if !text.is_empty() {
                    ev.set_context(app, gps, now);
                    ev.event(evidence::EventCode::Note, text.as_bytes(), &[]);
                    println!("hub-p4: cmd evidence note logged ({} chars)", text.len());
                }
            }
            0x1b => {
                *note = None;
                println!("hub-p4: cmd note cancelled");
            }
            c if c.is_ascii() && !c.is_ascii_control() => {
                let _ = n.push(c as char);
            }
            _ => {}
        }
        return None;
    }
    ev.set_context(app, gps, now);
    match c {
        b'n' => {
            *note = Some(heapless::String::new());
            println!("hub-p4: cmd type the note, then Enter (Esc cancels)");
        }
        b'u' => {
            ev.stream = !ev.stream;
            let t: &[u8] = if ev.stream { b"console evidence stream on (command u)" } else { b"console evidence stream off (command u)" };
            ev.event(evidence::EventCode::Setting, t, &[ev.stream as u8]);
            println!("hub-p4: cmd evidence stream {}", if ev.stream { "on" } else { "off" });
        }
        b'k' => {
            ev.checkpoint(evidence::format::CheckpointReason::Requested, now);
            println!("hub-p4: cmd evidence checkpoint #{} queued", ev.seq().saturating_sub(1));
        }
        b'r' => {
            if sdlog.start_replay() {
                println!("hub-p4: cmd evidence replay of {} started", sdlog.ev_name());
            } else {
                println!("hub-p4: cmd evidence replay: no evidence file open");
            }
        }
        b'e' => {
            ev.event(evidence::EventCode::Shutdown, b"operator asked to stop (command e)", &[]);
            ev.checkpoint(evidence::format::CheckpointReason::Shutdown, now);
            sdlog.flush_now();
            println!("hub-p4: cmd evidence shutdown checkpoint queued; the card is safe to remove after the next 'evidence ... flushes' line shows queued=0");
        }
        // Bench only (`testpoint`): a fixed position would put every
        // distance and flag in the wrong place in the field.
        #[cfg(feature = "testpoint")]
        b'p' => {
            // The S3 hub's test point (300 m south of its demo circle).
            app.manual = Some(app::OwnPos {
                lat: 39.9526 - 300.0 / 111_195.0,
                lon: -75.1652,
                alt: Some(15.0),
                source: app::OwnSource::Manual,
                at_ms: now,
            });
            app.housekeeping(gps, now);
            app.redraw_now();
            ev.event(evidence::EventCode::Setting, b"manual own position set to the test point 39.949902,-75.165200 (command p)", &[1]);
            println!("hub-p4: cmd own position set to the test point (manual)");
        }
        b'c' => {
            app.manual = None;
            app.housekeeping(gps, now);
            app.redraw_now();
            ev.event(evidence::EventCode::Setting, b"manual own position cleared (command c)", &[0]);
            println!("hub-p4: cmd manual own position cleared");
        }
        b'b' => faa.bench(),
        b'a' => {
            // Aero nodes: auto (the coverage default) or one mode for all.
            let next = down.cycle_aero();
            let mut t: heapless::String<80> = heapless::String::new();
            let _ = core::fmt::Write::write_fmt(&mut t, format_args!("aero plan: {} (command a)", downlink::aero_only_label(next)));
            ev.event(evidence::EventCode::Setting, t.as_bytes(), &[]);
            println!("hub-p4: cmd {t}");
        }
        b'm' => drop(lines.gm.forget_home(gps, sdlog, ev, "console", now)),
        b'H' => drop(lines.gm.confirm_home(gps, sdlog, ev, "console", now)),
        b'T' => lines.gm.accept_time(gps, ev, "console"),
        b'L' => {
            *lines.site = Some(heapless::String::new());
            println!("hub-p4: cmd type one site line, then Enter (Esc cancels): node <address> <lat> <lon> | node <address> clear | node all here | home <lat> <lon> [height] | home clear | mute <kind> [source=..] [target=..] [mhz=..] [ssid=..] | unmute all | control ids on|off | control content on|off | theme auto|glass-cockpit|field-day|red-night | theme-day <theme> | theme-night <theme> | theme-switch twilight|sunrise");
        }
        #[cfg(not(feature = "testpoint"))]
        b'p' => println!("hub-p4: cmd p (test position) is in bench builds only (feature testpoint)"),
        b'h' | b'?' => println!("hub-p4: cmd help: {}c=clear the manual own position, b=time airspace lookups, a=aero plan (auto / 1090 / UAT / FLARM / OGN / control links), m=hub moved (forget the GPS home), H=accept the new GPS home the hub found, T=accept GPS time as it is now, L=site line (node position / hub home / mute alarm / control ids|content on|off / screen theme), n=evidence note, k=evidence checkpoint now, r=replay evidence file, u=evidence stream on/off, e=evidence shutdown checkpoint, h=help. JSON lines start with '{{', evidence lines with '@EV1'.", if cfg!(feature = "testpoint") { "p=set the test own position (bench), " } else { "" }),
        b'\r' | b'\n' | b' ' => {}
        _ => println!("hub-p4: cmd unknown (h for help)"),
    }
    None
}

/// `fakedrone`: the made-up drone flies over PHL's runways, inside the
/// Class B surface area and a 0 ft facility-map cell, with its operator
/// 1.2 km north (live GNSS). For the first 40 s of each 80 s it is 160 m
/// (525 ft) above the ground, then 280 m (920 ft). Altitudes are WGS-84:
/// ground there is ~2 m above the geoid, and the geoid ~33 m below the
/// ellipsoid. Expected flags (PLAN.md Phase 12 done-when): Class B note,
/// facility map Warning, height Watch ("check for structures", urban cell)
/// then Warning, distance from operator Watch then Warning.
#[cfg(feature = "fakedrone")]
fn fake_flight(now: u64) -> [rid::message::Message; 2] {
    use rid::message::*;
    let agl = if (now / 1000) % 80 < 40 { 160.0 } else { 280.0 };
    let (lat, lon) = (39.8721, -75.2407);
    [
        Message::Location(Location {
            status: Status::Airborne,
            height_type: HeightType::AboveTakeoff,
            direction: Some(90.0),
            speed_horizontal: Some(2.0),
            speed_vertical: Some(0.0),
            position: Some(LatLon { lat, lon }),
            altitude_pressure: None,
            altitude_geodetic: Some(2.2 - 32.9 + agl),
            height: Some(agl),
            horizontal_accuracy: HorizontalAccuracy::M3,
            vertical_accuracy: VerticalAccuracy::M3,
            baro_accuracy: VerticalAccuracy::M3,
            speed_accuracy: SpeedAccuracy::Mps1,
            timestamp: None,
            timestamp_accuracy: None,
        }),
        Message::System(System {
            operator_location_type: OperatorLocationType::LiveGnss,
            classification_type: ClassificationType::Undeclared,
            operator_position: Some(LatLon {
                lat: lat + 0.0108,
                lon,
            }),
            area_count: 1,
            area_radius: 0,
            area_ceiling: None,
            area_floor: None,
            category: Category::Undeclared,
            class: UaClass::Undeclared,
            operator_altitude: None,
            timestamp: None,
        }),
    ]
}

/// The ADSBee counts as covering 1090 MHz (aero nodes then leave it out)
/// when heard this recently. Long, so a short gap does not replan nodes.
const ADSBEE_COVERS_MS: u64 = 60_000;

/// GPS and ADS-B status for the log, every 5 s.
fn print_gps_adsb(app: &app::App, gps: &gps::Gps, adsbee: &adsbee::Adsbee, now: u64) {
    use core::sync::atomic::Ordering::Relaxed;
    let st = gps.stats();
    let sky = gps.sky();
    let pps_age = gps::pps_last_us().map(|t| (Instant::now().duration_since_epoch().as_micros().saturating_sub(t)) / 1000);
    let own = app.own_pos(gps, now);
    println!(
        "hub-p4: gps {:?} {:?} module={} ver={} baud={} cfg={}{} frames={} csum_err={} rx={} uart_err={} | sky in_view={} used={} dual_band={} | pps n={} age={:?}ms | utc={:?} pps_anchor={} | own={:?}",
        gps.phase, gps.status.state, gps.module.map_or("-", |m| m.name()), gps.version, gps.baud(), gps.cfg.name(),
        if gps.cfg_failed_step.is_empty() { "" } else { gps.cfg_failed_step }, st.frames_ok, st.checksum_errors,
        uartring::gps_rx::BYTES.load(Relaxed), uartring::gps_rx::UART_ERR.load(Relaxed),
        sky.total_in_view(), sky.total_used(), sky.dual_band(), gps::PPS_COUNT.load(Relaxed), pps_age,
        gps.utc_ms(now), gps.time.is_some_and(|t| t.pps),
        own.map(|p| (p.lat, p.lon, p.alt, p.source == app::OwnSource::Gps))
    );
    let a = adsbee.status(now);
    println!(
        "hub-p4: adsbee alive={} bytes={} lines={} errors={} other={} uart_err={} ring_full={} aircraft={} (active {}) table={:?} valid_ps={:?}",
        a.alive, adsbee.bytes(), adsbee.lines, adsbee.errors, adsbee.other,
        uartring::adsb_rx::UART_ERR.load(Relaxed), uartring::adsb_rx::RING_FULL.load(Relaxed),
        app.tracker.aircraft().len(), app.tracker.active_aircraft_count(now), a.aircraft, a.frames_per_s
    );
    #[cfg(feature = "modes")]
    {
        let Some(t) = adsbee.raw.table.as_deref() else { return };
        let s = &t.stats;
        let b = &adsbee.raw.beast.stats;
        println!(
            "hub-p4: modes beast_frames={} broken={} skipped={} used={} bad_crc={} fixed1={} fixed2={} fix2_refused={} unconfirmed={} pos_global={} pos_local={} pos_refused={} zone_mismatch={} tisb={} adsr={} bds={:?} table={} df={:?}",
            b.frames, b.broken, b.skipped, s.used, s.bad_crc, s.fixed_1, s.fixed_2, s.fix_2_refused, s.unconfirmed,
            s.positions_global, s.positions_local, s.positions_refused, s.cpr_zone_mismatch, s.tis_b, s.ads_r, s.bds,
            t.len(), s.by_df
        );
        // The aircraft with a position, as the decoder has them.
        for p in t.planes() {
            if let Some((lat, lon)) = p.position() {
                println!(
                    "hub-p4: modes ac {:06X} {} alt={:?} pos={:.5},{:.5} global={}",
                    p.icao, p.callsign().as_ref().map_or("-", |c| c.as_str()), p.baro_altitude_ft(), lat, lon, p.position_global()
                );
            }
        }
    }
}

/// Where the tracker (~64 KB) lives in PSRAM. Until 2026-11-17 it was a
/// static in internal RAM, and on this chip the main stack is whatever
/// internal RAM the statics leave: with `main`'s ~65 KB frame that left about
/// 7 KB below it, and the SD mount (plus an interrupt on top) overran the
/// stack guard on every boot with a card in. In PSRAM it gives that 64 KB to
/// the stack instead.
pub const TRACKER_OFFSET: usize = 2 << 20;
const _: () = assert!(rgb::H_RES * rgb::V_RES * 2 <= TRACKER_OFFSET, "framebuffer overlaps the tracker");
const _: () = assert!(
    TRACKER_OFFSET + core::mem::size_of::<app::HubTracker>() <= evlog::QUEUE_OFFSET,
    "tracker overlaps the evidence queue"
);

/// Builds the tracker in PSRAM at [`TRACKER_OFFSET`]. The hub cannot run
/// without PSRAM (it is inside the P4 package; a failed PSRAM test is a
/// hardware fault), so it stops here with a message rather than overrun the
/// stack later.
#[inline(never)]
fn place_tracker(psram: Option<(*mut u32, usize)>) -> &'static mut app::HubTracker {
    let Some((base, size)) = psram else {
        panic!("hub-p4: no PSRAM: the tracker lives there; cannot start");
    };
    assert!(size >= TRACKER_OFFSET + core::mem::size_of::<app::HubTracker>(), "PSRAM too small for the tracker");
    // SAFETY: [TRACKER_OFFSET, + size_of) is inside PSRAM (checked above),
    // used by nothing else (the asserts above: after the framebuffer, before
    // the evidence queue), and 2 MB is aligned for any type. Called once.
    unsafe {
        let p = base.cast::<u8>().add(TRACKER_OFFSET).cast::<app::HubTracker>();
        p.write(app::HubTracker::new(tracker::Config {
            // Project policy: open-link identifiers stay hidden unless built
            // with `--features show-link-ids` (as the S3 hub).
            show_link_ids: cfg!(feature = "show-link-ids"),
            ..tracker::Config::default()
        }));
        &mut *p
    }
}
static mut FAA: core::mem::MaybeUninit<faa::Faa> = core::mem::MaybeUninit::uninit();

/// Where the screen's band buffer (800 x 48 RGB565, 75 KB) lives in PSRAM:
/// after the framebuffer, below the tracker. Until 2026-11-18 a 15 KB band
/// was a static in internal RAM, which on this chip comes out of the main
/// stack (see [`TRACKER_OFFSET`]); a full-width band there would have cost
/// the stack 38 KB more. Drawing into PSRAM goes through the cache, which
/// holds a whole band.
pub const BAND_OFFSET: usize = 1 << 20;
const _: () = assert!(rgb::H_RES * rgb::V_RES * 2 <= BAND_OFFSET, "framebuffer overlaps the band");
const _: () = assert!(BAND_OFFSET + display::BAND_PX * 2 <= TRACKER_OFFSET, "band overlaps the tracker");

/// The band buffer in PSRAM at [`BAND_OFFSET`] (the hub cannot run without
/// PSRAM; `place_tracker` stops first).
#[inline(never)]
fn place_band(psram: Option<(*mut u32, usize)>) -> &'static mut [u16] {
    let Some((base, size)) = psram else {
        panic!("hub-p4: no PSRAM: the screen band lives there; cannot start");
    };
    assert!(size >= BAND_OFFSET + display::BAND_PX * 2, "PSRAM too small for the screen band");
    // SAFETY: [BAND_OFFSET, + BAND_PX * 2) is inside PSRAM (checked above)
    // and used by nothing else (the asserts above: after the framebuffer,
    // before the tracker); 1 MB is aligned for u16. Called once.
    unsafe {
        let p = base.cast::<u8>().add(BAND_OFFSET).cast::<u16>();
        p.write_bytes(0, display::BAND_PX);
        core::slice::from_raw_parts_mut(p, display::BAND_PX)
    }
}
