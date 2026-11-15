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

mod app;
#[cfg(feature = "benchlog")]
mod bench;
mod bb;
mod display;
mod downlink;
mod links;
mod psram_test;
mod rgb;
mod sd;
mod sdbb;
mod touch;

use esp_backtrace as _;
use esp_hal::{
    gpio::{Flex, Pin},
    main,
    time::{Duration, Instant},
};
use esp_println::println;

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
                esp_println::println!(
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
    // microSD log (1-bit native mode, bit-banged): CLK 43, CMD 44, D0 39.
    let mut sdlog = sd::Logger::new(Flex::new(p.GPIO43), Flex::new(p.GPIO44), Flex::new(p.GPIO39));

    #[cfg(feature = "selftest")]
    let mut selftest = selftest::Sender::new(p.UART2);

    // SAFETY: each static is taken exactly once, here, before the loop.
    let tracker = unsafe {
        (&raw mut TRACKER).write(core::mem::MaybeUninit::new(app::HubTracker::new(tracker::Config::default())));
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
    let mut loops = 0u32;
    let mut max_gap_us = 0u64;
    let mut prev = Instant::now();
    loop {
        let now = Instant::now();
        max_gap_us = max_gap_us.max((now - prev).as_micros());
        loops += 1;
        let ms = app::now_ms();
        gw.poll(ms);
        app.ingest(&mut gw, ms);
        down.tick(&mut gw, ms);
        app.touch(&mut i2c, ms);
        if let Some(panel) = panel.as_mut() {
            panel.poll();
            app.draw(panel, &mut gw, ms, |gw| {
                gw.poll(app::now_ms());
            });
        }
        sdlog.tick(app.tracker, ms);
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
            app.frames_drawn = 0;
            app.max_band_us = 0;
            loops = 0;
            max_gap_us = 0;
        }
    }
}

static mut TRACKER: core::mem::MaybeUninit<app::HubTracker> = core::mem::MaybeUninit::uninit();
static mut BAND: [embedded_graphics::pixelcolor::Rgb565; screen::WIDTH as usize * screen::BAND_ROWS] =
    [embedded_graphics::pixelcolor::Rgb565::new(0, 0, 0); screen::WIDTH as usize * screen::BAND_ROWS];
