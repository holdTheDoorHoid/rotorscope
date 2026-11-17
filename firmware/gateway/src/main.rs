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
//! Authentication (docs/ESPNOW.md, "Securing the link"): built with the kit
//! key (`tools/link.key`), every packet both ways is sealed (`airlink::auth`).
//! The gateway then relays only node packets with a good tag and a fresh
//! stamp, commands only nodes it has heard this boot (a command must echo the
//! node's session), and answers a genuine but stale or out-of-order packet
//! with a sync beacon so a node that rebooted, or outlived a gateway reboot,
//! catches up. Rejected packets are counted per claimed address and reported
//! to the hub once a second as `Frame::Threat` / `ThreatKind::ForgedLink`.
//! A keyed gateway accepts keyed nodes only. Built without a key it runs
//! open, as before, and says so at boot and in its `Hello` to the hub.
//!
//! Time (link protocol 11): the hub's `Command::TimeSync`, about once a
//! second, goes to every aero node (one that has said it is `NodeRole::Aero`
//! or that the hub assigned that role) as its own command packet, unicast,
//! with no retry or ACK bookkeeping (the next one is a second away). Its
//! `utc_ms` is advanced by the time it spent here, from UART receipt to the
//! moment each packet is handed to the radio, so queueing here does not
//! skew the node's clock. The `To` + `Assign` path is unchanged.
//!
//! This board transmits (the beacons); see docs/ESPNOW.md.
#![no_std]
#![no_main]

use core::cell::RefCell;

use critical_section::Mutex;
use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use airlink::auth::{self, Reject, Verdict};
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
/// With a key, a node must be in this table to be relayed (its replay state
/// lives here), so it is also the most keyed nodes one gateway serves.
const MAX_NODES: usize = 16;
/// Answer a node's stale or out-of-order packets with a sync at most this often.
const SYNC_EVERY_MS: u64 = 1000;
/// Report rejected packets to the hub at most once per this window.
const THREAT_WINDOW_MS: u32 = 1000;
/// Packets from before our boot (stale epoch) are expected for a few seconds
/// after a gateway restart, until each node has had its sync: not reported
/// as an attack before this uptime.
const STALE_GRACE_MS: u64 = 60_000;
/// Our own Hello to the hub (key state), this often.
const HELLO_EVERY_MS: u64 = 5000;
/// 2437 MHz in the 100 kHz units of `ThreatSighting::freq`.
const CHANNEL_FREQ: u32 = 24_370;

#[derive(Clone, Copy)]
struct Node {
    mac: [u8; 6],
    seq: airlink::SeqTracker,
    frames: u32,
    bytes: u32,
    rssi: i8,
    last_ms: u64,
    /// Replay state and the echo its commands need (keyed builds).
    auth: auth::Peer,
    last_sync_ms: Option<u64>,
    /// It has said it is an aero node (Hello / Status role Aero, or sent
    /// AeroStatus / Air frames): it gets the hub's `TimeSync`.
    aero: bool,
}

impl Node {
    fn new(mac: [u8; 6], rssi: i8) -> Self {
        Node {
            mac,
            seq: Default::default(),
            frames: 0,
            bytes: 0,
            rssi,
            last_ms: 0,
            auth: Default::default(),
            last_sync_ms: None,
            aero: false,
        }
    }
}

/// Index of `mac` in the node table, adding it if there is room.
fn node_index(nodes: &mut heapless::Vec<Node, MAX_NODES>, mac: [u8; 6], rssi: i8) -> Option<usize> {
    if let Some(i) = nodes.iter().position(|n| n.mac == mac) {
        return Some(i);
    }
    nodes.push(Node::new(mac, rssi)).ok()?;
    Some(nodes.len() - 1)
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
/// Authentication (keyed builds): rejected packets per address and reason,
/// duplicates dropped, syncs sent, commands held for want of an echo.
static REJECTS: Mutex<RefCell<auth::RejectLog<8>>> = Mutex::new(RefCell::new(auth::RejectLog::new()));
static AUTH_DUPS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static SYNCS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static THREATS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
/// Open builds: sealed packets relayed without checking (we have no key).
static UNVERIFIED: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

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

/// Work for `air_tx`, the only task that transmits.
#[derive(Clone, Copy)]
enum Job {
    /// Try this node's pending command now.
    Command([u8; 6]),
    /// Send this node a sync beacon echoing `(session, counter)`.
    Sync([u8; 6], u32, u32),
    /// Send the latest [`TIME`] to every aero node.
    TimeSync,
}
static KICK: Channel<CriticalSectionRawMutex, Job, 16> = Channel::new();
static HUB_BYTES: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static HUB_CMDS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static HUB_CORRUPT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
/// `Assign`s with no `To` before them (or for a full node table).
static HUB_ORPHANS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
/// The hub's latest `TimeSync`: UTC ms, when its bytes arrived here, PPS.
static TIME: Mutex<RefCell<Option<(u64, Instant, bool)>>> = Mutex::new(RefCell::new(None));
/// `TimeSync`s from the hub; packets sent to aero nodes (radio-ACKed or
/// not); and syncs that found no aero node to go to.
static HUB_SYNCS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static TIME_SENT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static TIME_UNACKED: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static TIME_NOBODY: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
/// Command-packet sequence numbers for `TimeSync`: bit 15 set, so they
/// never equal a queued command's (which count up from 0 per node). A node
/// drops a command packet whose sequence equals the one before it.
static TIME_SEQ: core::sync::atomic::AtomicU16 = core::sync::atomic::AtomicU16::new(0);

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
    // The radio is on, so the hardware RNG is a true RNG: a fresh epoch.
    let gw_auth = auth::Key::kit().map(|k| auth::Gateway::new(k, esp_hal::rng::Rng::new().random()));
    println!(
        r#"{{"t":"hello","fw":"gateway","ver":"{}","ch":{CHANNEL},"espnow":{version},"lr":{},"mac":"{}","key":"{}"}}"#,
        env!("CARGO_PKG_VERSION"),
        lr == 0,
        Mac(mac),
        auth::KEY_TAG
    );
    if gw_auth.is_none() {
        println!(
            r#"{{"t":"warn","what":"link_key","note":"built WITHOUT a link key: anyone nearby can inject or replay node packets and commands. See docs/ESPNOW.md 'Securing the link'."}}"#
        );
    }
    let (manager, sender, receiver) = esp_now.split();
    let relay_key = gw_auth.as_ref().map(|g| (g.key.clone(), g.epoch()));

    spawner.spawn(air_tx(manager, sender, gw_auth, mac).expect("spawn air_tx"));
    spawner.spawn(hub_commands(hub_rx).expect("spawn hub_commands"));
    spawner.spawn(stats().expect("spawn stats"));
    relay(receiver, hub, relay_key, mac).await
}

/// Next stamp from the gateway's clock; restart for a new epoch once it has
/// run out (after 2^32 packets, decades).
fn stamp(a: &mut auth::Gateway, echo: Option<(u32, u32)>) -> auth::Stamp {
    match a.stamp(echo) {
        Some(s) => s,
        None => esp_hal::system::software_reset(),
    }
}

/// Make sure `mac` is an ESP-NOW peer, so unicast to it works.
fn ensure_peer(manager: &EspNowManager, mac: [u8; 6]) {
    if !manager.peer_exists(&mac) {
        let _ = manager.add_peer(PeerInfo {
            interface: EspNowWifiInterface::Station,
            peer_address: mac,
            lmk: None,
            channel: None,
            encrypt: false,
        });
    }
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
async fn air_tx(manager: EspNowManager, mut sender: EspNowSender, mut auth: Option<auth::Gateway>, me: [u8; 6]) {
    let mut seq: u16 = 0;
    let period = Duration::from_millis(500);
    let mut next_beacon = Instant::now();
    loop {
        let wait = next_beacon.saturating_duration_since(Instant::now());
        match with_timeout(wait, KICK.receive()).await {
            Ok(Job::Command(mac)) => {
                send_command(&manager, &mut sender, &mut auth, me, mac).await;
                continue;
            }
            Ok(Job::TimeSync) => {
                send_time(&manager, &mut sender, &mut auth, me).await;
                continue;
            }
            Ok(Job::Sync(mac, session, counter)) => {
                // Only keyed gateways queue syncs.
                if let Some(a) = auth.as_mut() {
                    ensure_peer(&manager, mac);
                    let mut buf = [0u8; airlink::BEACON_LEN + auth::TRAILER];
                    let len = airlink::beacon(&mut buf, seq, CHANNEL).len();
                    let st = stamp(a, Some((session, counter)));
                    if let Some(n) = auth::seal(&a.key, &me, &mut buf, len, st)
                        && sender.send_async(&mac, &buf[..n]).await.is_ok()
                    {
                        SYNCS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                    }
                }
                continue;
            }
            Err(_) => {}
        }
        let mut buf = [0u8; airlink::BEACON_LEN + auth::TRAILER];
        let mut n = airlink::beacon(&mut buf, seq, CHANNEL).len();
        if let Some(a) = auth.as_mut() {
            let st = stamp(a, None);
            n = auth::seal(&a.key, &me, &mut buf, n, st).unwrap_or(n);
        }
        if sender.send_async(&BROADCAST_ADDRESS, &buf[..n]).await.is_ok() {
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
            send_command(&manager, &mut sender, &mut auth, me, mac).await;
        }
    }
}

/// One attempt at delivering `mac`'s pending command. Keyed: only once the
/// node has been heard this boot (the command must echo its session); until
/// then it stays pending, and hearing the node kicks it.
async fn send_command(manager: &EspNowManager, sender: &mut EspNowSender, auth: &mut Option<auth::Gateway>, me: [u8; 6], mac: [u8; 6]) {
    let now = Instant::now().as_millis();
    let echo = critical_section::with(|cs| NODES.borrow_ref(cs).iter().find(|n| n.mac == mac).and_then(|n| n.auth.echo));
    if auth.is_some() && echo.is_none() {
        return;
    }
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
    ensure_peer(manager, mac);
    let mut buf = [0u8; airlink::HEADER + link::MAX_FRAME + auth::TRAILER];
    let Some(len) = airlink::command(&mut buf, seq, &frames).map(<[u8]>::len) else { return };
    let mut n = len;
    if let Some(a) = auth.as_mut() {
        let st = stamp(a, echo);
        let Some(sealed) = auth::seal(&a.key, &me, &mut buf, len, st) else { return };
        n = sealed;
    }
    let ok = sender.send_async(&mac, &buf[..n]).await.is_ok();
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

/// The hub's latest `TimeSync` to every aero node, once each, unicast (a
/// keyed node needs its session echoed). `utc_ms` is advanced by the time
/// since its bytes reached us, just before each packet is sealed and sent.
async fn send_time(manager: &EspNowManager, sender: &mut EspNowSender, auth: &mut Option<auth::Gateway>, me: [u8; 6]) {
    use core::sync::atomic::Ordering::Relaxed;
    let Some((utc_ms, rx_at, pps)) = critical_section::with(|cs| TIME.borrow_ref_mut(cs).take()) else { return };
    let mut to: heapless::Vec<([u8; 6], Option<(u32, u32)>, u16), MAX_NODES> = heapless::Vec::new();
    critical_section::with(|cs| {
        let down = DOWN.borrow_ref(cs);
        for n in NODES.borrow_ref(cs).iter() {
            let d = down.iter().find(|d| d.mac == n.mac);
            if n.aero || d.is_some_and(|d| d.role == Some(link::NodeRole::Aero)) {
                let _ = to.push((n.mac, n.auth.echo, d.map_or(0, |d| d.seq)));
            }
        }
    });
    if to.is_empty() {
        TIME_NOBODY.fetch_add(1, Relaxed);
        return;
    }
    for (mac, echo, cmd_seq) in to {
        if auth.is_some() && echo.is_none() {
            continue; // not heard this boot: it could not check the packet
        }
        let mut seq = 0x8000 | (TIME_SEQ.fetch_add(1, Relaxed) & 0x7FFF);
        if seq == cmd_seq {
            seq = 0x8000 | (TIME_SEQ.fetch_add(1, Relaxed) & 0x7FFF);
        }
        ensure_peer(manager, mac);
        let utc_ms = utc_ms + Instant::now().saturating_duration_since(rx_at).as_millis();
        let mut enc = [0u8; 24];
        let Ok(frame) = link::encode_command(&link::Command::TimeSync { utc_ms, pps }, &mut enc) else { continue };
        let mut buf = [0u8; airlink::HEADER + 24 + auth::TRAILER];
        let Some(len) = airlink::command(&mut buf, seq, frame).map(<[u8]>::len) else { continue };
        let mut n = len;
        if let Some(a) = auth.as_mut() {
            let st = stamp(a, echo);
            let Some(sealed) = auth::seal(&a.key, &me, &mut buf, len, st) else { continue };
            n = sealed;
        }
        TIME_SENT.fetch_add(1, Relaxed);
        if sender.send_async(&mac, &buf[..n]).await.is_err() {
            TIME_UNACKED.fetch_add(1, Relaxed);
        }
    }
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
        let rx_at = Instant::now();
        dec.feed(&buf[..n], |r| match r {
            Ok(link::Command::To { node }) => to = Some(node),
            // Protocol 11: for every aero node (`send_time`); `to` is left
            // as it is (a TimeSync is never addressed).
            Ok(link::Command::TimeSync { utc_ms, pps }) => {
                HUB_SYNCS.fetch_add(1, Relaxed);
                critical_section::with(|cs| *TIME.borrow_ref_mut(cs) = Some((utc_ms, rx_at, pps)));
                let _ = KICK.try_send(Job::TimeSync);
            }
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
                    let _ = KICK.try_send(Job::Command(mac));
                } else {
                    HUB_ORPHANS.fetch_add(1, Relaxed);
                }
            }
            // Protocol 10 wired-bus commands: not for a gateway (it is never on the bus).
            Ok(_) => {}
            Err(_) => {
                HUB_CORRUPT.fetch_add(1, Relaxed);
                to = None;
            }
        });
    }
}

/// Receive forever; write `Via` + frame for each frame of each packet. Also
/// the gateway's own frames to the hub (written here, so they never land in
/// the middle of a relayed one): `Threat` reports for rejected packets, once
/// a second, and a `Hello` naming the key state every few seconds.
async fn relay(mut rx: EspNowReceiver, mut hub: UartTx<'static, esp_hal::Async>, auth: Option<(auth::Key, u32)>, me: [u8; 6]) -> ! {
    use core::sync::atomic::Ordering::Relaxed;
    let mut next_hello = Instant::now();
    let mut next_report = Instant::now() + Duration::from_millis(u64::from(THREAT_WINDOW_MS));
    loop {
        let now = Instant::now();
        if now >= next_hello {
            next_hello = now + Duration::from_millis(HELLO_EVERY_MS);
            hello(&mut hub).await;
        }
        if now >= next_report {
            next_report = now + Duration::from_millis(u64::from(THREAT_WINDOW_MS));
            report_threats(&mut hub, me).await;
        }
        let wait = next_report.min(next_hello).saturating_duration_since(Instant::now());
        let Ok(got) = with_timeout(wait, rx.receive_async()).await else { continue };
        let Some(pkt) = airlink::parse(got.data()) else {
            FOREIGN.fetch_add(1, Relaxed);
            continue;
        };
        if pkt.kind != airlink::Kind::Frames {
            continue; // another gateway's beacon
        }
        let src = got.info.src_address;
        let rssi = got.info.rx_control.rssi as i8;
        // Keyed: the tag first (no state is touched for a bad one), then
        // freshness against this node's last accepted packet.
        let stamp = match &auth {
            Some((key, _)) => match auth::verify(key, &src, got.data()) {
                Ok(st) => Some(st),
                Err(r) => {
                    reject(src, r, rssi);
                    continue;
                }
            },
            None => {
                if pkt.sealed {
                    UNVERIFIED.fetch_add(1, Relaxed);
                }
                None
            }
        };
        let now_ms = Instant::now().as_millis();
        let fresh = critical_section::with(|cs| {
            let mut nodes = NODES.borrow_ref_mut(cs);
            let Some(i) = node_index(&mut nodes, src, rssi) else {
                // Table full. Open: relay, do not count. Keyed: no replay
                // state for it, so it cannot be relayed safely.
                return if auth.is_some() { Err(None) } else { Ok(true) };
            };
            let n = &mut nodes[i];
            if let (Some((_, epoch)), Some(st)) = (&auth, &stamp) {
                match n.auth.check(*epoch, st) {
                    Verdict::Accept => {}
                    Verdict::Duplicate => return Ok(false),
                    Verdict::Reject(r) => {
                        // Genuine but stale or old: answer with a sync, so a
                        // node that rebooted (or outlived our reboot) catches up.
                        let sync = n.last_sync_ms.is_none_or(|t| now_ms.saturating_sub(t) >= SYNC_EVERY_MS);
                        if sync {
                            n.last_sync_ms = Some(now_ms);
                        }
                        return Err(Some((r, sync.then_some((st.session, st.counter)))));
                    }
                }
            }
            n.rssi = rssi;
            n.last_ms = now_ms;
            let fresh = n.seq.on_packet(pkt.seq);
            if fresh {
                n.bytes = n.bytes.wrapping_add(pkt.payload.len() as u32);
                n.frames = n.frames.wrapping_add(pkt.payload.iter().filter(|&&b| b == 0).count() as u32);
            }
            Ok(fresh)
        });
        let fresh = match fresh {
            Ok(f) => f,
            Err(None) => continue,
            Err(Some((r, sync))) => {
                reject(src, r, rssi);
                if let Some((session, counter)) = sync {
                    let _ = KICK.try_send(Job::Sync(src, session, counter));
                }
                continue;
            }
        };
        if !fresh {
            if stamp.is_some() {
                AUTH_DUPS.fetch_add(1, Relaxed);
            }
            continue;
        }
        // The node is on our channel right now: a good moment for its command.
        let waiting = critical_section::with(|cs| DOWN.borrow_ref(cs).iter().any(|d| d.mac == src && d.pending.is_some()));
        if waiting {
            let _ = KICK.try_send(Job::Command(src));
        }
        let mut via = [0u8; 16];
        let Ok(via) = link::encode(&link::Frame::Via { node: src, rssi }, &mut via) else { continue };
        let via: &[u8] = via;
        // Whether this node says it is an aero node (it then gets time syncs).
        let mut aero = false;
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
                    aero |= matches!(
                        f,
                        link::Frame::Hello { role: link::NodeRole::Aero, .. }
                            | link::Frame::Status { role: link::NodeRole::Aero, .. }
                            | link::Frame::AeroStatus(_)
                            | link::Frame::Air(_)
                            | link::Frame::Control(_)
                            | link::Frame::ControlEnergy(_)
                    );
                }
                Err(_) => {
                    CORRUPT.fetch_add(1, Relaxed);
                }
            });
            if write_all(&mut hub, via).await.is_err() || write_all(&mut hub, frame).await.is_err() {
                UART_DROPPED.fetch_add(1, Relaxed);
            }
        }
        if aero {
            critical_section::with(|cs| {
                if let Some(n) = NODES.borrow_ref_mut(cs).iter_mut().find(|n| n.mac == src) {
                    n.aero = true;
                }
            });
        }
    }
}

/// Count a rejected packet for the next `Threat` report. Stale-epoch packets
/// right after our own boot are expected (nodes still on our previous epoch
/// until their sync) and only counted in the log line.
fn reject(src: [u8; 6], r: Reject, rssi: i8) {
    let quiet = r == Reject::Stale && Instant::now().as_millis() < STALE_GRACE_MS;
    critical_section::with(|cs| {
        let mut log = REJECTS.borrow_ref_mut(cs);
        if quiet {
            let i = r.code() as usize - 1;
            log.totals[i] = log.totals[i].saturating_add(1);
        } else {
            log.record(src, r, rssi);
        }
    });
}

/// One `Threat` / `ForgedLink` frame per claimed address and reason rejected
/// in the last window. `detail` = the reason (`airlink::auth::Reject` code:
/// 1 bad tag, 2 replay, 3 unauthenticated, 4 stale).
async fn report_threats(hub: &mut UartTx<'static, esp_hal::Async>, me: [u8; 6]) {
    let mut out: heapless::Vec<auth::RejectReport, 32> = heapless::Vec::new();
    let now = Instant::now();
    critical_section::with(|cs| {
        REJECTS.borrow_ref_mut(cs).drain(now.as_millis(), THREAT_WINDOW_MS, |r| {
            let _ = out.push(r);
        })
    });
    for r in out {
        let f = link::Frame::Threat(link::ThreatSighting {
            kind: link::ThreatKind::ForgedLink,
            channel: CHANNEL,
            freq: CHANNEL_FREQ,
            width_khz: 0,
            level: r.rssi,
            count: r.count,
            window_ms: r.window_ms,
            source: r.source,
            target: me,
            detail: r.reason.code(),
            ssid: heapless::String::new(),
            uptime_us: now.as_micros(),
        });
        let mut buf = [0u8; link::MAX_FRAME];
        if let Ok(b) = link::encode(&f, &mut buf) {
            let b: &[u8] = b;
            if write_all(hub, b).await.is_ok() {
                THREATS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            }
        }
        println!(
            r#"{{"t":"threat","kind":"forged_link","src":"{}","why":"{}","count":{},"rssi":{}}}"#,
            Mac(r.source),
            r.reason.name(),
            r.count,
            r.rssi
        );
    }
}

/// The gateway's own `Hello` (no `Via` before it, so it is the link's own):
/// role Gateway, and in the firmware string the key state, "gw key <id>" or
/// "gw OPEN no key". Relayed nodes keep their own Hellos.
async fn hello(hub: &mut UartTx<'static, esp_hal::Async>) {
    use core::fmt::Write as _;
    let mut firmware = heapless::String::<16>::new();
    if auth::KIT_KEY.is_some() {
        let _ = write!(firmware, "gw key {}", auth::KEY_ID);
    } else {
        let _ = firmware.push_str("gw OPEN no key");
    }
    let f = link::Frame::Hello { role: link::NodeRole::Gateway, protocol: link::PROTOCOL_VERSION, firmware };
    let mut buf = [0u8; 48];
    if let Ok(b) = link::encode(&f, &mut buf) {
        let b: &[u8] = b;
        let _ = write_all(hub, b).await;
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
        let rej = critical_section::with(|cs| REJECTS.borrow_ref(cs).totals);
        println!(
            r#"{{"t":"gw","up_ms":{now},"key":"{}","beacons":{},"decoded":{},"detections":{},"corrupt":{},"foreign":{},"uart_dropped":{},"nodes":{},"hub_bytes":{},"hub_cmds":{},"hub_corrupt":{},"hub_orphans":{},"bad_tag":{},"replay":{},"unauth":{},"stale":{},"auth_dups":{},"syncs":{},"threats":{},"unverified":{},"hub_syncs":{},"time_sent":{},"time_unacked":{},"time_nobody":{},"aero_nodes":{}}}"#,
            auth::KEY_ID,
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
            HUB_ORPHANS.load(Relaxed),
            rej[0],
            rej[1],
            rej[2],
            rej[3],
            AUTH_DUPS.load(Relaxed),
            SYNCS.load(Relaxed),
            THREATS.load(Relaxed),
            UNVERIFIED.load(Relaxed),
            HUB_SYNCS.load(Relaxed),
            TIME_SENT.load(Relaxed),
            TIME_UNACKED.load(Relaxed),
            TIME_NOBODY.load(Relaxed),
            nodes.iter().filter(|n| n.aero).count()
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
