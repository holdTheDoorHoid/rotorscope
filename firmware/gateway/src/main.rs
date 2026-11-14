//! ESP-NOW gateway (XIAO ESP32-C5).
//!
//! Sits on one 2.4 GHz channel ([`CHANNEL`]), beacons twice a second so nodes
//! learn its address, receives `airlink` packets, and writes each relayed
//! frame to the hub UART preceded by a `link::Frame::Via` marker naming the
//! node. To the hub this is one more wired node that happens to speak for
//! many. USB carries a once-a-second JSON line per node for the bench.
//!
//! Downlink: the hub writes `link::Command`s back on the same UART (gateway
//! RX = XIAO D7, GPIO12). Each `To { node }` + `Assign` pair goes to that
//! node as an `airlink` command packet, unicast so the radio ACKs it. A node
//! only hears us while it is on channel 6, so a command that is not ACKed is
//! retried when the node next sends us something (it is on 6 right then) and
//! at each beacon, up to [`CMD_ATTEMPTS`] times. The hub resends every few
//! seconds anyway.
//!
//! This board transmits (the beacons); see docs/ESPNOW.md.
#![no_std]
#![no_main]

use core::cell::RefCell;

use critical_section::Mutex;
use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Instant, Ticker, with_timeout};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::uart::{Config as UartConfig, Uart, UartRx, UartTx};
use esp_println::println;
use esp_radio::esp_now::{BROADCAST_ADDRESS, EspNowManager, EspNowReceiver, EspNowSender, EspNowWifiInterface, PeerInfo};
use esp_radio::wifi::WifiController;
use esp_wifi_sys_esp32c5::include as sys;

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

/// The gateway's channel. The 2.4 GHz node dwells on 6 for 300 of every
/// 400 ms of its hop plan, so it can send without leaving its plan.
pub const CHANNEL: u8 = 6;
/// Hub UART, same speed and pin as a node's (XIAO D6 = GPIO11 TX), plus the
/// hub's commands on XIAO D7 = GPIO12 RX.
const HUB_BAUD: u32 = 921_600;
/// Tries per command before giving up (the hub resends anyway).
const CMD_ATTEMPTS: u8 = 4;
/// Do not retry one node's command more often than this.
const CMD_RETRY_MS: u64 = 20;
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

/// One node's downlink: the latest command from the hub not yet delivered.
#[derive(Clone)]
struct Down {
    mac: [u8; 6],
    /// COBS command frame(s), as the hub sent them (re-encoded, same bytes).
    pending: Option<heapless::Vec<u8, { link::MAX_FRAME }>>,
    /// What the latest command asked for, for the log.
    role: Option<link::NodeRole>,
    plan: u16,
    seq: u16,
    attempts: u8,
    last_try_ms: u64,
    /// Commands from the hub for this node.
    cmds: u32,
    /// Packets sent (attempts), ACKed, and commands given up on.
    sent: u32,
    acked: u32,
    failed: u32,
    /// Commands replaced by a newer one before they were delivered.
    replaced: u32,
}

static DOWN: Mutex<RefCell<heapless::Vec<Down, MAX_NODES>>> = Mutex::new(RefCell::new(heapless::Vec::new()));
/// Nodes whose pending command should be tried now.
static KICK: Channel<CriticalSectionRawMutex, [u8; 6], 16> = Channel::new();
static HUB_BYTES: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static HUB_CMDS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static HUB_CORRUPT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
/// `Assign`s with no `To` before them (or for a full node table).
static HUB_ORPHANS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger(log::LevelFilter::Off);
    let p = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 65_536);
    let timg0 = TimerGroup::new(p.TIMG0);
    esp_rtos::start(timg0.timer0, p.FROM_CPU_INTR0);

    let (hub_rx, hub) = Uart::new(p.UART1, UartConfig::default().with_baudrate(HUB_BAUD))
        .expect("hub UART")
        .with_tx(p.GPIO11)
        .with_rx(p.GPIO12)
        .into_async()
        .split();

    let mut wifi = WifiController::new(p.WIFI, Default::default()).expect("Wi-Fi init");
    let code = *b"US\0";
    // SAFETY: NUL-terminated, outlives the call; Wi-Fi is initialised.
    unsafe { sys::esp_wifi_set_country_code(code.as_ptr().cast(), false) };
    wifi.set_band_mode(esp_radio::wifi::BandMode::_2_4G).expect("2.4 GHz band");
    let lr = enable_lr();
    let esp_now = wifi.esp_now();
    esp_now.set_channel(CHANNEL).expect("set channel");
    let version = esp_now.version().unwrap_or(0);
    let mut mac = [0u8; 6];
    // SAFETY: 6-byte buffer; Wi-Fi is initialised.
    unsafe { sys::esp_wifi_get_mac(sys::wifi_interface_t_WIFI_IF_STA, mac.as_mut_ptr()) };
    println!(
        r#"{{"t":"hello","fw":"gateway","ver":"{}","ch":{CHANNEL},"espnow":{version},"lr":{},"mac":"{}"}}"#,
        env!("CARGO_PKG_VERSION"),
        lr == 0,
        Mac(mac)
    );
    let (manager, sender, receiver) = esp_now.split();

    spawner.spawn(air_tx(manager, sender).expect("spawn air_tx"));
    spawner.spawn(hub_commands(hub_rx).expect("spawn hub_commands"));
    spawner.spawn(stats().expect("spawn stats"));
    relay(receiver, hub).await
}

/// Add Espressif Long Range to the station's 2.4 GHz protocols (keeping b/g/n),
/// so LR packets are understood; ordinary rates are unaffected.
fn enable_lr() -> i32 {
    let bits = (sys::WIFI_PROTOCOL_11B | sys::WIFI_PROTOCOL_11G | sys::WIFI_PROTOCOL_11N | sys::WIFI_PROTOCOL_LR) as u8;
    // SAFETY: Wi-Fi is initialised; plain value arguments.
    unsafe { sys::esp_wifi_set_protocol(sys::wifi_interface_t_WIFI_IF_STA, bits) }
}

struct Mac([u8; 6]);
impl core::fmt::Display for Mac {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let m = self.0;
        write!(f, "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5])
    }
}

/// Everything the gateway transmits: beacons every 500 ms, and hub commands
/// to nodes when kicked (new command, or the node just spoke) or at a beacon.
#[embassy_executor::task]
async fn air_tx(manager: EspNowManager, mut sender: EspNowSender) {
    let mut seq: u16 = 0;
    let period = Duration::from_millis(500);
    let mut next_beacon = Instant::now();
    loop {
        let wait = next_beacon.saturating_duration_since(Instant::now());
        if let Ok(mac) = with_timeout(wait, KICK.receive()).await {
            send_command(&manager, &mut sender, mac).await;
            continue;
        }
        let mut buf = [0u8; airlink::HEADER + 1];
        let pkt = airlink::beacon(&mut buf, seq, CHANNEL);
        if sender.send_async(&BROADCAST_ADDRESS, pkt).await.is_ok() {
            BEACONS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        }
        seq = seq.wrapping_add(1);
        next_beacon += period;
        if next_beacon < Instant::now() {
            next_beacon = Instant::now() + period;
        }
        // Retry whatever is still pending.
        let mut due: heapless::Vec<[u8; 6], MAX_NODES> = heapless::Vec::new();
        critical_section::with(|cs| {
            for d in DOWN.borrow_ref(cs).iter().filter(|d| d.pending.is_some()) {
                let _ = due.push(d.mac);
            }
        });
        for mac in due {
            send_command(&manager, &mut sender, mac).await;
        }
    }
}

/// One attempt at delivering `mac`'s pending command.
async fn send_command(manager: &EspNowManager, sender: &mut EspNowSender, mac: [u8; 6]) {
    let now = Instant::now().as_millis();
    let job = critical_section::with(|cs| {
        let mut down = DOWN.borrow_ref_mut(cs);
        let d = down.iter_mut().find(|d| d.mac == mac)?;
        let frames = d.pending.clone()?;
        if d.attempts > 0 && now.saturating_sub(d.last_try_ms) < CMD_RETRY_MS {
            return None;
        }
        d.last_try_ms = now;
        Some((frames, d.seq))
    });
    let Some((frames, seq)) = job else { return };
    if !manager.peer_exists(&mac) {
        let _ = manager.add_peer(PeerInfo {
            interface: EspNowWifiInterface::Station,
            peer_address: mac,
            lmk: None,
            channel: None,
            encrypt: false,
        });
    }
    let mut buf = [0u8; airlink::HEADER + link::MAX_FRAME];
    let Some(pkt) = airlink::command(&mut buf, seq, &frames) else { return };
    let ok = sender.send_async(&mac, pkt).await.is_ok();
    critical_section::with(|cs| {
        let mut down = DOWN.borrow_ref_mut(cs);
        let Some(d) = down.iter_mut().find(|d| d.mac == mac) else { return };
        if d.seq != seq {
            return; // replaced while in flight; the new one starts afresh
        }
        d.sent += 1;
        d.attempts += 1;
        if ok {
            d.acked += 1;
        } else if d.attempts >= CMD_ATTEMPTS {
            d.failed += 1;
        } else {
            return;
        }
        d.pending = None;
        d.seq = d.seq.wrapping_add(1);
    });
}

/// Read the hub's command stream; queue each `To` + `Assign` for its node.
#[embassy_executor::task]
async fn hub_commands(mut rx: UartRx<'static, esp_hal::Async>) {
    use core::sync::atomic::Ordering::Relaxed;
    let mut dec = link::CommandDecoder::new();
    let mut to: Option<[u8; 6]> = None;
    let mut buf = [0u8; 128];
    loop {
        let n = match rx.read_async(&mut buf).await {
            Ok(n) => n,
            Err(_) => {
                HUB_CORRUPT.fetch_add(1, Relaxed);
                to = None;
                continue;
            }
        };
        HUB_BYTES.fetch_add(n as u32, Relaxed);
        dec.feed(&buf[..n], |r| match r {
            Ok(link::Command::To { node }) => to = Some(node),
            Ok(cmd @ link::Command::Assign(_)) => {
                HUB_CMDS.fetch_add(1, Relaxed);
                let Some(mac) = to.take() else {
                    HUB_ORPHANS.fetch_add(1, Relaxed);
                    return;
                };
                let mut enc = [0u8; link::MAX_FRAME];
                let Ok(wire) = link::encode_command(&cmd, &mut enc) else { return };
                let Ok(frames) = heapless::Vec::from_slice(wire) else { return };
                let link::Command::Assign(a) = &cmd else { return };
                let queued = critical_section::with(|cs| {
                    let mut down = DOWN.borrow_ref_mut(cs);
                    let i = match down.iter().position(|d| d.mac == mac) {
                        Some(i) => i,
                        None => {
                            let d = Down {
                                mac,
                                pending: None,
                                role: None,
                                plan: 0,
                                seq: 0,
                                attempts: 0,
                                last_try_ms: 0,
                                cmds: 0,
                                sent: 0,
                                acked: 0,
                                failed: 0,
                                replaced: 0,
                            };
                            if down.push(d).is_err() {
                                return false;
                            }
                            down.len() - 1
                        }
                    };
                    let d = &mut down[i];
                    if d.pending.is_some() {
                        d.replaced += 1;
                        d.seq = d.seq.wrapping_add(1);
                    }
                    d.pending = Some(frames);
                    d.role = Some(a.role);
                    d.plan = a.id;
                    d.attempts = 0;
                    d.cmds += 1;
                    true
                });
                if queued {
                    let _ = KICK.try_send(mac);
                } else {
                    HUB_ORPHANS.fetch_add(1, Relaxed);
                }
            }
            Err(_) => {
                HUB_CORRUPT.fetch_add(1, Relaxed);
                to = None;
            }
        });
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
        // The node is on our channel right now: a good moment for its command.
        let waiting = critical_section::with(|cs| DOWN.borrow_ref(cs).iter().any(|d| d.mac == src && d.pending.is_some()));
        if waiting {
            let _ = KICK.try_send(src);
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
            r#"{{"t":"gw","up_ms":{now},"beacons":{},"decoded":{},"detections":{},"corrupt":{},"foreign":{},"uart_dropped":{},"nodes":{},"hub_bytes":{},"hub_cmds":{},"hub_corrupt":{},"hub_orphans":{}}}"#,
            BEACONS.load(Relaxed),
            DECODED.load(Relaxed),
            DETECTIONS.load(Relaxed),
            CORRUPT.load(Relaxed),
            FOREIGN.load(Relaxed),
            UART_DROPPED.load(Relaxed),
            nodes.len(),
            HUB_BYTES.load(Relaxed),
            HUB_CMDS.load(Relaxed),
            HUB_CORRUPT.load(Relaxed),
            HUB_ORPHANS.load(Relaxed)
        );
        let down = critical_section::with(|cs| DOWN.borrow_ref(cs).clone());
        for d in &down {
            println!(
                r#"{{"t":"down","mac":"{}","role":"{:?}","plan":{},"cmds":{},"sent":{},"acked":{},"failed":{},"replaced":{},"pending":{}}}"#,
                Mac(d.mac),
                d.role,
                d.plan,
                d.cmds,
                d.sent,
                d.acked,
                d.failed,
                d.replaced,
                d.pending.is_some()
            );
        }
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
