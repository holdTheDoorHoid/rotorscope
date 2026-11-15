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
#[cfg(all(feature = "webheadloop", any(feature = "selftest", feature = "gpsloop")))]
compile_error!("`webheadloop` needs the web-head feed on UART2, which `selftest` and `gpsloop` take");

mod adsbee;
mod app;
#[cfg(feature = "benchlog")]
mod bench;
mod bb;
mod civil;
mod console;
mod display;
mod downlink;
mod evlog;
mod faa;
mod gps;
#[cfg(feature = "gpsloop")]
mod gpsloop;
mod links;
mod psram_test;
mod report;
mod rgb;
mod sd;
mod sdbb;
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

/// STC8 helper MCU on the board I2C (SDA GPIO45, SCL GPIO46): register 0x20
/// takes the backlight brightness, 0..=100.
const STC8_ADDR: u8 = 0x2F;
const STC8_BACKLIGHT: u8 = 0x20;
/// GT911 touch controller (0x5D when INT is low at reset, else 0x14).
const GT911_ADDRS: [u8; 2] = [0x5D, 0x14];


/// GPIO28 for the UART3 receiver. With `selftest` the same pad is also driven
/// by UART2's transmitter, so the pin is handed out twice on purpose.
fn unsafe_pin28() -> esp_hal::peripherals::GPIO28<'static> {
    // SAFETY: bring-up only. Without `selftest`, `main` never uses GPIO28
    // otherwise; with it, a second user drives the pad as output (see below).
    unsafe { esp_hal::peripherals::GPIO28::steal() }
}

#[cfg(feature = "selftest")]
mod selftest {
    //! Sends a fixed sequence (Via, Hello, Via, Status, Via, Detection, a
    //! direct Stats) every 100 ms from UART2 TX on GPIO28, the gateway RX
    //! pin, and reads the hub's commands back from the UART3 TX pad (GPIO27)
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
            // SAFETY: bring-up only: GPIO27 is UART3's TX pad; we only listen to it.
            let pin27 = unsafe { esp_hal::peripherals::GPIO27::steal() };
            let uart = Uart::new(uart, Config::default().with_baudrate(super::links::BAUD))
                .expect("UART2")
                .with_tx(super::unsafe_pin28())
                .with_rx(pin27);
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
    println!("hub-p4: hello, chip revision {:?}", esp_hal::efuse::chip_revision());
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

    let mut gw = links::GatewayLink::new(p.UART3, unsafe_pin28(), p.GPIO27);
    println!("hub-p4: gateway link on UART3 RX GPIO28, TX GPIO27 at {} baud", links::BAUD);
    let mut down = downlink::Downlink::new();
    // The web head (WEBHEAD.md): UART2 TX on GPIO31. The bench self-tests
    // `selftest` and `gpsloop` need UART2 themselves, so they build without it.
    #[cfg(not(any(feature = "selftest", feature = "gpsloop")))]
    {
        gw.feed = Some(webhead::Feed::new(p.UART2, p.GPIO31));
        println!("hub-p4: web head feed on UART2 TX GPIO{} at {} baud", webhead::TX_PIN, webhead::BAUD);
    }

    // GPS on UART1 (header RX GPIO48 / TX GPIO47), PPS on GPIO29; ADSBee on
    // UART4 RX GPIO30. See gps.rs, adsbee.rs and PARITY.md.
    let gps_uart = esp_hal::uart::Uart::new(p.UART1, uartring::gps_rx::config(gps::BAUDS[0]))
        .expect("UART1")
        .with_rx(p.GPIO48)
        .with_tx(p.GPIO47);
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
    #[cfg(feature = "gpsloop")]
    // SAFETY: bench only; GPIO30 is shared with UART4 RX on purpose.
    let adsb_uart = adsb_uart.with_tx(unsafe { esp_hal::peripherals::GPIO30::steal() });
    uartring::adsb_rx::install(adsb_uart);
    let mut adsbee = adsbee::Adsbee::default();
    println!("hub-p4: gps on UART1 RX GPIO48 TX GPIO47 (hunting {:?} baud), PPS GPIO29; adsbee on UART4 RX GPIO30 at {}", gps::BAUDS, adsb::DEFAULT_BAUD);
    #[cfg(feature = "gpsloop")]
    let mut gpsloop = gpsloop::Loop::new(p.UART2);
    // microSD log (1-bit native mode, bit-banged): CLK 43, CMD 44, D0 39.
    let mut sdlog = sd::Logger::new(Flex::new(p.GPIO43), Flex::new(p.GPIO44), Flex::new(p.GPIO39));
    // Phase 12: the airspace pack goes to PSRAM above the framebuffer.
    // SAFETY: taken exactly once, here (a static: the engine is ~27 KB).
    let faa = unsafe {
        (&raw mut FAA).write(core::mem::MaybeUninit::new(faa::Faa::new(psram)));
        (*(&raw mut FAA)).assume_init_mut()
    };

    // Phase 13: the evidence log (evlog.rs). A random boot id names this
    // boot's chain.
    let mut boot_id = [0u8; 8];
    esp_hal::rng::Rng::new().read(&mut boot_id);
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
    ev.boot((chip.major, chip.minor));
    let mut note: Option<heapless::String<120>> = None;

    #[cfg(feature = "selftest")]
    let mut selftest = selftest::Sender::new(p.UART2);
    #[cfg(feature = "webheadloop")]
    let mut whloop = webheadloop::Loop::new();

    // SAFETY: each static is taken exactly once, here, before the loop.
    let tracker = unsafe {
        (&raw mut TRACKER).write(core::mem::MaybeUninit::new(app::HubTracker::new(tracker::Config {
            // Project policy: open-link identifiers stay hidden unless built
            // with `--features show-link-ids` (as the S3 hub).
            show_link_ids: cfg!(feature = "show-link-ids"),
            ..tracker::Config::default()
        })));
        (*(&raw mut TRACKER)).assume_init_mut()
    };
    let band = unsafe { &mut *(&raw mut BAND) };
    let mut app = app::App::new(tracker, band);

    #[cfg(feature = "fakenode")]
    let mut last_fake = Instant::now();
    #[cfg(feature = "fakedrone")]
    let mut last_drone = Instant::now();
    #[cfg(feature = "benchlog")]
    let mut bench = bench::Bench::default();
    let mut last = Instant::now();
    let mut last_report = 0u64;
    let mut loops = 0u32;
    let mut max_gap_us = 0u64;
    let mut prev = Instant::now();
    loop {
        let now = Instant::now();
        max_gap_us = max_gap_us.max((now - prev).as_micros());
        loops += 1;
        let ms = app::now_ms();
        console::pump();
        gw.poll(ms);
        ev.set_context(&app, &gps, ms);
        app.ingest(&mut gw, &mut ev, ms);
        down.tick(&mut gw, ms);
        gps.tick(ms);
        #[cfg(not(feature = "webheadloop"))]
        adsbee.tick(app.tracker, ms, gw.feed.as_mut());
        #[cfg(feature = "webheadloop")]
        whloop.tick(&mut adsbee, app.tracker, &mut gw, ms);
        command(&mut app, &gps, faa, &mut ev, &mut sdlog, &mut note, ms);
        ev.tick(ms);
        if ms.saturating_sub(last_report) >= 1000 {
            last_report = ms;
            app.housekeeping(&gps, ms);
            ev.watch(&gps, &gw.nodes, faa.pack(), ms);
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
            };
            app.draw(panel, &mut gw, &ext, ms, |gw| {
                gw.poll(app::now_ms());
                console::pump();
            });
        }
        sdlog.tick(app.tracker, faa, &mut ev, ms, utc, gps.status.state != tracker::json::GpsState::Absent);
        #[cfg(feature = "gpsloop")]
        gpsloop.tick();
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
                "hub-p4: link bytes={} frames={} corrupt={} uart_err={} ring_full={} direct={} nodes={} out_full={} tx={} tx_full={} cmds={} | tracks={} | lcd vsync={} dma_err={} drawn={} max_band={}us | loops={} max_gap={}us",
                s.bytes, s.frames, s.corrupt, s.uart_err, s.ring_full, s.direct, gw.nodes.len(), gw.out_full,
                gw.tx_bytes, gw.tx_full, down.commands,
                app.tracker.tracks().len(), vs, err, app.frames_drawn, app.max_band_us, loops, max_gap_us
            );
            if ms / 1000 % 5 == 0 {
                print_gps_adsb(&app, &gps, &adsbee, ms);
                if let Some(f) = gw.feed.as_ref() {
                    println!(
                        "hub-p4: webhead tx={} frames={} dropped={} queued={} csbee_err={} encode_err={}",
                        f.tx_bytes, f.fwd.frames, f.fwd.dropped, f.fwd.len(), f.csbee_err, f.fwd.encode_err
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
    screen::Gps { fix, sats: gps.status.sats.unwrap_or(0) }
}

/// One console command byte, if any (see `console.rs`). `n` starts an
/// operator note: the text up to Enter goes into the evidence log.
fn command(
    app: &mut app::App,
    gps: &gps::Gps,
    faa: &faa::Faa,
    ev: &mut evlog::Evidence,
    sdlog: &mut sd::Logger,
    note: &mut Option<heapless::String<120>>,
    now: u64,
) {
    let Some(c) = console::read_command() else { return };
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
        return;
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
        b'h' | b'?' => println!("hub-p4: cmd help: p=set test own position, c=clear it, b=time airspace lookups, n=evidence note, k=evidence checkpoint now, r=replay evidence file, u=evidence stream on/off, e=evidence shutdown checkpoint, h=help. JSON lines start with '{{', evidence lines with '@EV1'."),
        b'\r' | b'\n' | b' ' => {}
        _ => println!("hub-p4: cmd unknown (h for help)"),
    }
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
}

static mut TRACKER: core::mem::MaybeUninit<app::HubTracker> = core::mem::MaybeUninit::uninit();
static mut FAA: core::mem::MaybeUninit<faa::Faa> = core::mem::MaybeUninit::uninit();
static mut BAND: [embedded_graphics::pixelcolor::Rgb565; screen::WIDTH as usize * screen::BAND_ROWS] =
    [embedded_graphics::pixelcolor::Rgb565::new(0, 0, 0); screen::WIDTH as usize * screen::BAND_ROWS];
