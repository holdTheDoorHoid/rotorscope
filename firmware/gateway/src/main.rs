//! ESP-NOW gateway (XIAO ESP32-C5).
//!
//! Sits on one 2.4 GHz channel ([`CHANNEL`]), beacons twice a second so nodes
//! learn its address, receives `airlink` packets, and writes each relayed
//! frame to the hub UART preceded by a `link::Frame::Via` marker naming the
//! node. To the hub this is one more wired node that happens to speak for
//! many. USB carries a once-a-second JSON line per node for the bench.
//!
//! This board transmits (the beacons); see docs/ESPNOW.md.
#![no_std]
#![no_main]

use core::cell::RefCell;

use critical_section::Mutex;
use embassy_executor::Spawner;
use embassy_time::{Duration, Instant, Ticker};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::uart::{Config as UartConfig, UartTx};
use esp_println::println;
use esp_radio::esp_now::{BROADCAST_ADDRESS, EspNowReceiver, EspNowSender};
use esp_radio::wifi::WifiController;
use esp_wifi_sys_esp32c5::include as sys;

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

/// The gateway's channel. The 2.4 GHz node dwells on 6 for 300 of every
/// 400 ms of its hop plan, so it can send without leaving its plan.
pub const CHANNEL: u8 = 6;
/// Hub UART, same speed and pin as a node's (XIAO D6 = GPIO11).
const HUB_BAUD: u32 = 921_600;
/// Nodes tracked for stats. More can still relay; they just are not counted.
const MAX_NODES: usize = 16;

#[derive(Clone, Copy)]
struct Node {
    mac: [u8; 6],
    seq: airlink::SeqTracker,
    frames: u32,
    bytes: u32,
    rssi: i8,
    last_ms: u64,
}

static NODES: Mutex<RefCell<heapless::Vec<Node, MAX_NODES>>> = Mutex::new(RefCell::new(heapless::Vec::new()));
/// Frames the UART could not take in time.
static UART_DROPPED: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static BEACONS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static FOREIGN: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
/// Relayed frames checked by decoding them here too (bench proof that what
/// goes to the hub is intact): whole frames, then the Detection subset.
static DECODED: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static DETECTIONS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static CORRUPT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger(log::LevelFilter::Off);
    let p = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 65_536);
    let timg0 = TimerGroup::new(p.TIMG0);
    esp_rtos::start(timg0.timer0, p.FROM_CPU_INTR0);

    let hub = UartTx::new(p.UART1, UartConfig::default().with_baudrate(HUB_BAUD))
        .expect("hub UART")
        .with_tx(p.GPIO11)
        .into_async();

    let mut wifi = WifiController::new(p.WIFI, Default::default()).expect("Wi-Fi init");
    let code = *b"US\0";
    // SAFETY: NUL-terminated, outlives the call; Wi-Fi is initialised.
    unsafe { sys::esp_wifi_set_country_code(code.as_ptr().cast(), false) };
    wifi.set_band_mode(esp_radio::wifi::BandMode::_2_4G).expect("2.4 GHz band");
    let esp_now = wifi.esp_now();
    esp_now.set_channel(CHANNEL).expect("set channel");
    let version = esp_now.version().unwrap_or(0);
    let mut mac = [0u8; 6];
    // SAFETY: 6-byte buffer; Wi-Fi is initialised.
    unsafe { sys::esp_wifi_get_mac(sys::wifi_interface_t_WIFI_IF_STA, mac.as_mut_ptr()) };
    println!(
        r#"{{"t":"hello","fw":"gateway","ver":"{}","ch":{CHANNEL},"espnow":{version},"mac":"{}"}}"#,
        env!("CARGO_PKG_VERSION"),
        Mac(mac)
    );
    let (manager, sender, receiver) = esp_now.split();
    // Keep the manager alive (dropping the last part deinitialises ESP-NOW).
    core::mem::forget(manager);

    spawner.spawn(beacon(sender).expect("spawn beacon"));
    spawner.spawn(stats().expect("spawn stats"));
    relay(receiver, hub).await
}

struct Mac([u8; 6]);
impl core::fmt::Display for Mac {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let m = self.0;
        write!(f, "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5])
    }
}

#[embassy_executor::task]
async fn beacon(mut sender: EspNowSender) {
    let mut seq: u16 = 0;
    let mut ticker = Ticker::every(Duration::from_millis(500));
    loop {
        let mut buf = [0u8; airlink::HEADER + 1];
        let pkt = airlink::beacon(&mut buf, seq, CHANNEL);
        if sender.send_async(&BROADCAST_ADDRESS, pkt).await.is_ok() {
            BEACONS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        }
        seq = seq.wrapping_add(1);
        ticker.next().await;
    }
}

/// Receive forever; write `Via` + frame for each frame of each packet.
async fn relay(mut rx: EspNowReceiver, mut hub: UartTx<'static, esp_hal::Async>) -> ! {
    use core::sync::atomic::Ordering::Relaxed;
    loop {
        let got = rx.receive_async().await;
        let Some(pkt) = airlink::parse(got.data()) else {
            FOREIGN.fetch_add(1, Relaxed);
            continue;
        };
        if pkt.kind != airlink::Kind::Frames {
            continue; // another gateway's beacon
        }
        let src = got.info.src_address;
        let rssi = got.info.rx_control.rssi as i8;
        let fresh = critical_section::with(|cs| {
            let mut nodes = NODES.borrow_ref_mut(cs);
            let i = match nodes.iter().position(|n| n.mac == src) {
                Some(i) => i,
                None => {
                    let n = Node { mac: src, seq: Default::default(), frames: 0, bytes: 0, rssi, last_ms: 0 };
                    if nodes.push(n).is_err() {
                        return true; // table full: relay, do not count
                    }
                    nodes.len() - 1
                }
            };
            let n = &mut nodes[i];
            n.rssi = rssi;
            n.last_ms = Instant::now().as_millis();
            let fresh = n.seq.on_packet(pkt.seq);
            if fresh {
                n.bytes = n.bytes.wrapping_add(pkt.payload.len() as u32);
                n.frames = n.frames.wrapping_add(pkt.payload.iter().filter(|&&b| b == 0).count() as u32);
            }
            fresh
        });
        if !fresh {
            continue;
        }
        let mut via = [0u8; 16];
        let Ok(via) = link::encode(&link::Frame::Via { node: src, rssi }, &mut via) else { continue };
        let via: &[u8] = via;
        for frame in pkt.payload.split_inclusive(|&b| b == 0) {
            if frame.last() != Some(&0) {
                break; // a truncated tail: never relay half a frame
            }
            link::Decoder::new().feed(frame, |r| match r {
                Ok(f) => {
                    DECODED.fetch_add(1, Relaxed);
                    if matches!(f, link::Frame::Detection(_)) {
                        DETECTIONS.fetch_add(1, Relaxed);
                    }
                }
                Err(_) => {
                    CORRUPT.fetch_add(1, Relaxed);
                }
            });
            if write_all(&mut hub, via).await.is_err() || write_all(&mut hub, frame).await.is_err() {
                UART_DROPPED.fetch_add(1, Relaxed);
            }
        }
    }
}

async fn write_all(uart: &mut UartTx<'static, esp_hal::Async>, mut b: &[u8]) -> Result<(), ()> {
    while !b.is_empty() {
        let n = uart.write_async(b).await.map_err(|_| ())?;
        b = &b[n..];
    }
    Ok(())
}

#[embassy_executor::task]
async fn stats() {
    use core::sync::atomic::Ordering::Relaxed;
    let mut ticker = Ticker::every(Duration::from_secs(1));
    loop {
        ticker.next().await;
        let now = Instant::now().as_millis();
        let nodes = critical_section::with(|cs| NODES.borrow_ref(cs).clone());
        println!(
            r#"{{"t":"gw","up_ms":{now},"beacons":{},"decoded":{},"detections":{},"corrupt":{},"foreign":{},"uart_dropped":{},"nodes":{}}}"#,
            BEACONS.load(Relaxed),
            DECODED.load(Relaxed),
            DETECTIONS.load(Relaxed),
            CORRUPT.load(Relaxed),
            FOREIGN.load(Relaxed),
            UART_DROPPED.load(Relaxed),
            nodes.len()
        );
        for n in &nodes {
            println!(
                r#"{{"t":"node","mac":"{}","pkts":{},"lost":{},"dups":{},"frames":{},"bytes":{},"rssi":{},"age_ms":{}}}"#,
                Mac(n.mac),
                n.seq.received,
                n.seq.lost,
                n.seq.duplicates,
                n.frames,
                n.bytes,
                n.rssi,
                now.saturating_sub(n.last_ms)
            );
        }
    }
}
