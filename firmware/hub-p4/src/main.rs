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

mod bb;
mod links;
mod psram_test;

use esp_backtrace as _;
use esp_hal::{
    gpio::Flex,
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

fn wait(ms: u64) {
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(ms) {}
}

/// GPIO28 for the UART3 receiver. With `selftest` the same pad is also driven
/// by UART2's transmitter, so the pin is handed out twice on purpose.
fn unsafe_pin28() -> esp_hal::peripherals::GPIO28<'static> {
    // SAFETY: bring-up only. Without `selftest`, `main` never uses GPIO28
    // otherwise; with it, a second user drives the pad as output (see below).
    unsafe { esp_hal::peripherals::GPIO28::steal() }
}

#[cfg(feature = "selftest")]
mod selftest {
    //! Sends a fixed sequence (Via, Hello, Via, Detection, a direct Stats)
    //! every 100 ms from UART2 TX on GPIO28, the gateway RX pin.
    use esp_hal::time::{Duration, Instant};
    use esp_hal::uart::{Config, UartTx};
    use esp_hal::Blocking;

    pub struct Sender {
        tx: UartTx<'static, Blocking>,
        last: Instant,
    }

    impl Sender {
        pub fn new(uart: esp_hal::peripherals::UART2<'static>) -> Self {
            let tx = UartTx::new(uart, Config::default().with_baudrate(super::links::BAUD))
                .expect("UART2")
                .with_tx(super::unsafe_pin28());
            Self { tx, last: Instant::now() }
        }

        pub fn tick(&mut self) {
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
                    let _ = self.tx.write(b);
                }
            }
        }
    }
}

#[main]
fn main() -> ! {
    let p = esp_hal::init(esp_hal::Config::default());
    println!("hub-p4: hello, chip revision {:?}", esp_hal::efuse::chip_revision());

    // Order matters on this board (found 2026-11-14): if the PSRAM driver is
    // the first to raise LDO VO2, the board I2C on GPIO45/46 stops working
    // (lines read low / cannot pull low). Raising VO2 to the PSRAM driver's
    // own setting first, then configuring the I2C pins, then PSRAM, works.
    psram_test::vo2_on();
    let mut i2c = bb::BbI2c::new(Flex::new(p.GPIO45), Flex::new(p.GPIO46));

    let psram = psram_test::run(p.PSRAM, esp_hal::psram::SpiRamFreq::Mhz80);
    println!("hub-p4: psram ok = {}", psram.is_some());

    println!("hub-p4: i2c idle sda/scl high = {:?}", i2c.levels());
    println!("hub-p4: i2c pads pull low sda/scl = {:?}", i2c.drive_test());
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
    for level in [10u8, 100] {
        println!("hub-p4: backlight {level}: ack {}", i2c.write(STC8_ADDR, &[STC8_BACKLIGHT, level]));
        wait(1000);
    }

    let mut gw = links::GatewayLink::new(p.UART3, unsafe_pin28());
    println!("hub-p4: gateway link on UART3 RX GPIO28 at {} baud", links::BAUD);

    #[cfg(feature = "selftest")]
    let mut selftest = selftest::Sender::new(p.UART2);

    let mut last = Instant::now();
    loop {
        gw.poll();
        #[cfg(feature = "selftest")]
        selftest.tick();
        if last.elapsed() >= Duration::from_secs(1) {
            last = Instant::now();
            let s = &gw.stats;
            println!(
                "hub-p4: link bytes={} frames={} corrupt={} overflow={} uart_err={} direct={} nodes={}",
                s.bytes, s.frames, s.corrupt, s.overflow, s.uart_err, s.direct, gw.nodes.len()
            );
            for n in &gw.nodes {
                let m = n.mac;
                println!(
                    "hub-p4:   node {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} rssi={} frames={} detections={} hello={}",
                    m[0], m[1], m[2], m[3], m[4], m[5], n.rssi, n.frames, n.detections, n.hello
                );
            }
        }
    }
}
