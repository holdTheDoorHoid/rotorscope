//! ESP-NOW gateway (XIAO ESP32-C5): a relay that holds no secret (link
//! protocol 14, docs/LINK-KEYS-DESIGN.md sections 5, 6.3 and 14).
//!
//! The gateway holds no key. It cannot verify a packet or make one: the hub
//! verifies every node packet and seals everything that goes to a node. A
//! captured gateway can drop, delay, reorder or repeat traffic (bounded,
//! counted, visible at the hub), never forge or alter it undetected. It
//! speaks `link::gw` on the hub UART (TX = XIAO D6 = GPIO11, RX = XIAO D7 =
//! GPIO12), never `link::Frame`s.
//!
//! Up (`airlink::relay::Uplink`, host-tested there): every well-formed
//! version 3 packet from an address in the roster the hub pushed
//! (`GwDown::Roster`, RAM only; before the first one arrives no node packet
//! passes) goes to the hub as `GwUp::Packet`, untouched, with its source
//! address, RSSI and how long it waited here. Non-secret filters only: a
//! per-address budget (40 packets/s, 16 KB/s), a total cap of 50 KB/s, a
//! byte-identical radio retry dropped, round-robin per address so one
//! flooded address cannot starve the others. Version 2 packets are refused
//! as `old_firmware`; unprovisioned announcements pass at most once a second
//! in total; version 1 (open kit) node packets pass, from a roster address in
//! its own lane and from anyone else in one shared lane. Refusals go to the
//! hub as `GwUp::Refused` once a second per address and reason, totals in
//! `GwUp::Stats`.
//!
//! Down (`airlink::relay::TxQueue`, `BeaconSlot`): each `GwDown::Beacon` is
//! broadcast once as it arrives (a newer one replaces one not yet sent; none
//! is repeated). Each `GwDown::Transmit` goes unicast at once, again whenever
//! its node is heard (it is on channel then) and blind every 50 ms, until the
//! radio acknowledges it or its `give_up_ms` runs out; urgent ones (time
//! syncs) before anything else, dropped 300 ms after arrival. `Cancel` drops
//! what is queued for a node. Each ends in one `GwUp::TxDone`. The gateway
//! originates no beacon of its own and rewrites nothing.
//!
//! Hub receive: in the UART interrupt into a 4 KB ring, FIFO threshold 32
//! bytes (`hubuart`); USB output through an async writer that never masks
//! interrupts for longer than a copy (`usb`). Both are what the S0 spike
//! needed to stop losing hub messages.
//!
//! Crash recovery (docs/BENCH.md "Crash recovery"): a panic prints its
//! backtrace on USB and reboots (`custom_halt`); the RTC watchdog
//! ([`WATCHDOG`]) is fed by the receive loop while the transmit task and the
//! hub writer are both alive ([`ALIVE_MS`]), so a stuck radio, a task that
//! never yields or a dead UART writer resets the chip. The `hello` line says
//! why it last reset.
//!
//! Bench (`gwforge`, fenced with `ROTORSCOPE_BENCH=1`): see [`forge`].
#![no_std]
#![no_main]

#[cfg(all(feature = "faulttest", feature = "gwforge"))]
compile_error!("`gwforge` and `faulttest` both read the USB console; build one");

mod hubuart;
mod usb;
#[cfg(feature = "gwforge")]
mod forge;

use core::cell::RefCell;
use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

use airlink::relay::{self, Outcome, TxDone};
use critical_section::Mutex;
use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Ticker, with_timeout};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::uart::Uart;
use esp_radio::esp_now::{BROADCAST_ADDRESS, EspNowManager, EspNowReceiver, EspNowSender, EspNowWifiInterface, PeerInfo};
use esp_radio::wifi::WifiController;
use esp_wifi_sys_esp32c5::include as sys;
use link::gw::{self, GwDown, GwUp};
use usb::usb_line;

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

const _: () = assert!(gw::MAX_PACKET == airlink::MAX_PACKET && gw::MAX_ROSTER == relay::ROSTER_MAX);

/// Unattended relay: after a panic (backtrace already printed to USB),
/// reboot rather than sit halted until someone power-cycles the box.
#[unsafe(no_mangle)]
extern "Rust" fn custom_halt() -> ! {
    node_common::fault::reboot_after_panic()
}

/// Hang watchdog timeout.
const WATCHDOG: esp_hal::time::Duration = esp_hal::time::Duration::from_secs(10);
/// The receive loop feeds the watchdog only while the transmit task and the
/// hub writer have both gone round this recently (each wakes at least every
/// [`IDLE_MS`]).
const ALIVE_MS: u32 = 5_000;
/// Longest any task sleeps without going round.
const IDLE_MS: u64 = 100;
/// When `air_tx` / `hub_out` last went round (uptime ms, wrapping).
static AIR_AT: AtomicU32 = AtomicU32::new(0);
static OUT_AT: AtomicU32 = AtomicU32::new(0);

/// The gateway's channel (shared with the nodes and `coverage`).
const CHANNEL: u8 = airlink::GATEWAY_CHANNEL;
/// Hub UART speed.
const HUB_BAUD: u32 = 921_600;
/// Up: packet buffers waiting for the UART (32 x 1 KB, see `relay`).
const UP_SLOTS: usize = 32;
/// Down: transmits waiting for the radio (one per node, and room for time
/// syncs beside them).
const TX_SLOTS: usize = 20;
/// Our `Hello` to the hub this often (and at boot).
const HELLO_EVERY_MS: u64 = 5000;
/// `Refused` and `Stats` to the hub, and the USB stats lines, this often.
const REPORT_EVERY_MS: u64 = 1000;

static UP: Mutex<RefCell<relay::Uplink<UP_SLOTS>>> = Mutex::new(RefCell::new(relay::Uplink::new()));
static TX: Mutex<RefCell<relay::TxQueue<TX_SLOTS>>> = Mutex::new(RefCell::new(relay::TxQueue::new()));
static BEACON: Mutex<RefCell<relay::BeaconSlot>> = Mutex::new(RefCell::new(relay::BeaconSlot::new()));
/// Something for `hub_out` (a packet queued, a `TxDone`).
static OUT_WAKE: Signal<CriticalSectionRawMutex, ()> = Signal::new();
/// Something for `air_tx` (a beacon, a transmit, a node heard).
static AIR_WAKE: Signal<CriticalSectionRawMutex, ()> = Signal::new();
/// Finished transmits for the hub.
static DONE: Channel<CriticalSectionRawMutex, TxDone, 32> = Channel::new();

/// Beacons from the hub broadcast, and failed sends of them.
static BEACONS_SENT: AtomicU32 = AtomicU32::new(0);
static BEACONS_FAILED: AtomicU32 = AtomicU32::new(0);
/// Hub beacons that were not ours to send (no "RS", too long).
static BEACONS_BAD: AtomicU32 = AtomicU32::new(0);
/// Transmit attempts, and `TxDone`s that found `DONE` full (lost).
static TX_ATTEMPTS: AtomicU32 = AtomicU32::new(0);
static DONE_LOST: AtomicU32 = AtomicU32::new(0);
/// Hub messages decoded, corrupt or too long (`GwError`), rosters taken.
static HUB_MSGS: AtomicU32 = AtomicU32::new(0);
static HUB_CORRUPT: AtomicU32 = AtomicU32::new(0);
static ROSTERS: AtomicU32 = AtomicU32::new(0);
/// Messages written to the hub, and bytes.
static OUT_MSGS: AtomicU32 = AtomicU32::new(0);
static OUT_BYTES: AtomicU32 = AtomicU32::new(0);
/// Packets heard at all (before any filter).
static HEARD: AtomicU32 = AtomicU32::new(0);

fn now_ms() -> u64 {
    Instant::now().as_millis()
}

fn alive_now(at: &AtomicU32) {
    at.store(now_ms() as u32, Relaxed);
}

fn alive(at: &AtomicU32) -> bool {
    (now_ms() as u32).wrapping_sub(at.load(Relaxed)) < ALIVE_MS
}

/// Hand a finished transmit to the hub writer.
fn done(d: TxDone) {
    if DONE.try_send(d).is_err() {
        DONE_LOST.fetch_add(1, Relaxed);
    }
    OUT_WAKE.signal(());
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger(log::LevelFilter::Off);
    let p = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 65_536);
    let timg0 = TimerGroup::new(p.TIMG0);
    esp_rtos::start(timg0.timer0, p.FROM_CPU_INTR0);

    let (usb_rx, usb_tx) = esp_hal::usb::usb_serial_jtag::UsbSerialJtag::new(p.USB_DEVICE).into_async().split();
    spawner.spawn(usb::writer(usb_tx).expect("spawn usb writer"));

    let uart = Uart::new(p.UART1, hubuart::config(HUB_BAUD)).expect("hub UART").with_tx(p.GPIO11).with_rx(p.GPIO12);
    hubuart::start(uart);

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
    usb_line!(
        r#"{{"t":"hello","fw":"gateway","ver":"{}","protocol":{},"ch":{CHANNEL},"espnow":{version},"lr":{},"mac":"{}","key":"none","note":"relay only: holds no key, cannot verify or forge","reset":"{}","faulttest":{},"bench":{}}}"#,
        env!("CARGO_PKG_VERSION"),
        link::PROTOCOL_VERSION,
        lr == 0,
        Mac(mac),
        node_common::fault::reset_reason(),
        cfg!(feature = "faulttest"),
        env!("ROTORSCOPE_BENCH_JSON")
    );
    // Bench features compiled in (build.rs): say so, with the tag
    // tools/dev-board.sh looks for in the ELF.
    if !env!("ROTORSCOPE_BENCH_LIST").is_empty() {
        usb_line!(
            r#"{{"t":"warn","what":"bench_build","tag":"{}","note":"BENCH BUILD: never leave it on a fielded unit"}}"#,
            env!("ROTORSCOPE_BENCH_TAG")
        );
    }
    let (manager, sender, receiver) = esp_now.split();

    spawner.spawn(air_tx(manager, sender).expect("spawn air_tx"));
    spawner.spawn(hub_in().expect("spawn hub_in"));
    spawner.spawn(hub_out(mac).expect("spawn hub_out"));
    spawner.spawn(stats().expect("spawn stats"));
    #[cfg(feature = "faulttest")]
    spawner.spawn(faulttest(usb_rx).expect("spawn faulttest"));
    #[cfg(feature = "gwforge")]
    spawner.spawn(forge::console(usb_rx).expect("spawn gwforge"));
    #[cfg(not(any(feature = "faulttest", feature = "gwforge")))]
    let _ = usb_rx;
    alive_now(&AIR_AT);
    alive_now(&OUT_AT);
    node_common::fault::arm(WATCHDOG);
    relay_up(receiver).await
}

/// Bench only: `fault panic` / `fault hang` typed on USB (docs/BENCH.md).
#[cfg(feature = "faulttest")]
#[embassy_executor::task]
async fn faulttest(mut rx: esp_hal::usb::usb_serial_jtag::UsbSerialJtagRx<'static, esp_hal::Async>) {
    node_common::fault::console(|b| rx.drain_rx_fifo(b), |l| usb_line!("{l}")).await
}

/// Make sure `mac` is an ESP-NOW peer, so unicast to it works.
fn ensure_peer(manager: &EspNowManager, mac: [u8; 6]) -> bool {
    manager.peer_exists(&mac)
        || manager
            .add_peer(PeerInfo { interface: EspNowWifiInterface::Station, peer_address: mac, lmk: None, channel: None, encrypt: false })
            .is_ok()
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

/// Receive forever: every packet heard goes through the relay filters; a
/// node heard is on channel, so its waiting transmits may go now. Feeds the
/// watchdog at least once a second while the other two loops are alive.
async fn relay_up(mut rx: EspNowReceiver) -> ! {
    loop {
        if alive(&AIR_AT) && alive(&OUT_AT) {
            node_common::fault::feed();
        }
        let Ok(got) = with_timeout(Duration::from_secs(1), rx.receive_async()).await else { continue };
        HEARD.fetch_add(1, Relaxed);
        let src = got.info.src_address;
        let rssi = got.info.rx_control.rssi as i8;
        let at = Instant::now();
        // Bench (`gwforge drop`): the radio has already acknowledged it;
        // discard it here (counted there), as a captured gateway could.
        #[cfg(feature = "gwforge")]
        let forged_drop = forge::drop_this(&src);
        #[cfg(not(feature = "gwforge"))]
        let forged_drop = false;
        if !forged_drop {
            let outcome = critical_section::with(|cs| UP.borrow_ref_mut(cs).offer(src, rssi, got.data(), at.as_millis(), at.as_micros()));
            match outcome {
                Outcome::Queued => OUT_WAKE.signal(()),
                // Not a node packet: says nothing about a node being on channel.
                Outcome::Dropped(relay::Drop::Foreign) => continue,
                Outcome::Dropped(_) => {}
            }
        }
        let waiting = critical_section::with(|cs| {
            let mut tx = TX.borrow_ref_mut(cs);
            tx.heard(&src);
            !tx.is_empty()
        });
        if waiting {
            AIR_WAKE.signal(());
        }
    }
}

/// Everything the gateway transmits: the hub's beacons and transmits, in
/// that order of urgency: urgent transmits, then a beacon, then the rest.
#[embassy_executor::task]
async fn air_tx(manager: EspNowManager, mut sender: EspNowSender) {
    let mut buf = [0u8; airlink::MAX_PACKET];
    loop {
        alive_now(&AIR_AT);
        let now = now_ms();
        enum Job {
            Beacon(usize),
            Tx(usize, [u8; 6], usize),
            Wait(u64),
        }
        let job = critical_section::with(|cs| {
            let mut tx = TX.borrow_ref_mut(cs);
            tx.expire(now, done);
            let urgent = tx.urgent_ready(now);
            if !urgent && let Some(n) = BEACON.borrow_ref_mut(cs).take(&mut buf) {
                return Job::Beacon(n);
            }
            match tx.next(now) {
                Some(i) => {
                    let e = tx.begin(i, now);
                    let d = e.data();
                    buf[..d.len()].copy_from_slice(d);
                    Job::Tx(i, e.to, d.len())
                }
                None => Job::Wait(tx.wake_at().map_or(IDLE_MS, |t| t.saturating_sub(now).clamp(1, IDLE_MS))),
            }
        });
        match job {
            Job::Beacon(n) => {
                if sender.send_async(&BROADCAST_ADDRESS, &buf[..n]).await.is_ok() {
                    BEACONS_SENT.fetch_add(1, Relaxed);
                } else {
                    BEACONS_FAILED.fetch_add(1, Relaxed);
                }
            }
            Job::Tx(i, to, n) => {
                TX_ATTEMPTS.fetch_add(1, Relaxed);
                let acked = ensure_peer(&manager, to) && sender.send_async(&to, &buf[..n]).await.is_ok();
                let at = now_ms();
                critical_section::with(|cs| TX.borrow_ref_mut(cs).finish(i, acked, at, done));
            }
            Job::Wait(ms) => {
                let _ = with_timeout(Duration::from_millis(ms), AIR_WAKE.wait()).await;
            }
        }
    }
}

/// Read the hub's messages from the receive ring.
#[embassy_executor::task]
async fn hub_in() {
    let mut store = [0u8; gw::MAX_GW_FRAME];
    let mut dec = gw::GwDecoder::new(&mut store);
    let mut chunk = [0u8; 256];
    #[cfg(feature = "gwforge")]
    let mut scratch = [0u8; airlink::MAX_PACKET];
    loop {
        let n = hubuart::read(&mut chunk).await;
        dec.feed_down(&chunk[..n], |m| match m {
            Ok(m) => {
                HUB_MSGS.fetch_add(1, Relaxed);
                #[cfg(feature = "gwforge")]
                on_down(m, &mut scratch);
                #[cfg(not(feature = "gwforge"))]
                on_down(m);
            }
            Err(_) => {
                HUB_CORRUPT.fetch_add(1, Relaxed);
            }
        });
    }
}

/// One message from the hub.
fn on_down(m: GwDown<'_>, #[cfg(feature = "gwforge")] scratch: &mut [u8; airlink::MAX_PACKET]) {
    let now = now_ms();
    match m {
        GwDown::Roster(macs) => {
            let first = critical_section::with(|cs| {
                let mut up = UP.borrow_ref_mut(cs);
                let first = !up.has_roster();
                up.set_roster(&macs);
                first
            });
            ROSTERS.fetch_add(1, Relaxed);
            if first {
                usb_line!(r#"{{"t":"roster","n":{},"note":"first roster from the hub: node packets now relayed"}}"#, macs.len());
            }
        }
        GwDown::Beacon(b) => {
            // The gateway puts nothing on the air but the kit's own packets.
            let ok = b.starts_with(&airlink::MAGIC) && critical_section::with(|cs| BEACON.borrow_ref_mut(cs).put(b));
            if ok {
                AIR_WAKE.signal(());
            } else {
                BEACONS_BAD.fetch_add(1, Relaxed);
            }
        }
        GwDown::Transmit { id, to, urgent, give_up_ms, data } => {
            #[cfg(feature = "gwforge")]
            let (data, hold) = forge::on_transmit(to, urgent, data, scratch);
            #[cfg(not(feature = "gwforge"))]
            let hold = None;
            let t = relay::Transmit { id, to, urgent, give_up_ms, data };
            let r = critical_section::with(|cs| TX.borrow_ref_mut(cs).push(&t, now, hold));
            match r {
                Ok(()) => AIR_WAKE.signal(()),
                Err((_, d)) => done(d),
            }
        }
        GwDown::Cancel { to } => critical_section::with(|cs| TX.borrow_ref_mut(cs).cancel(to, now, done)),
    }
}

/// Encode `m` into `buf` and send it to the hub.
async fn send_up(m: &GwUp<'_>, buf: &mut [u8; gw::MAX_GW_FRAME]) {
    if let Ok(b) = gw::encode_up(m, buf) {
        let n = b.len();
        hubuart::write_all(&buf[..n]).await;
        OUT_MSGS.fetch_add(1, Relaxed);
        OUT_BYTES.fetch_add(n as u32, Relaxed);
    }
}

/// The only writer on the hub UART: our Hello, `TxDone`s, `Refused` and
/// `Stats` once a second, and the relayed packets round-robin under the
/// total cap.
#[embassy_executor::task]
async fn hub_out(me: [u8; 6]) {
    let mut buf = [0u8; gw::MAX_GW_FRAME];
    let mut next_hello = 0u64;
    let mut next_report = REPORT_EVERY_MS;
    loop {
        alive_now(&OUT_AT);
        let now = now_ms();
        if now >= next_hello {
            next_hello = now + HELLO_EVERY_MS;
            send_up(&GwUp::Hello { protocol: link::PROTOCOL_VERSION, firmware: firmware_name(), mac: me, channel: CHANNEL }, &mut buf).await;
        }
        if now >= next_report {
            next_report = now + REPORT_EVERY_MS;
            report(now, &mut buf).await;
        }
        while let Ok(d) = DONE.try_receive() {
            let m = GwUp::TxDone { id: d.id, to: d.to, radio_acked: d.radio_acked, attempts: d.attempts, held_ms: d.held_ms };
            send_up(&m, &mut buf).await;
        }
        #[cfg(feature = "gwforge")]
        forge::pending(&mut buf).await;
        // One relayed packet, encoded while its buffer is held.
        let now_us = Instant::now().as_micros();
        let n = critical_section::with(|cs| {
            UP.borrow_ref_mut(cs).pop(now_ms(), |q| {
                let held_us = now_us.saturating_sub(q.at_us).min(u64::from(u32::MAX)) as u32;
                let m = GwUp::Packet { src: q.src, rssi: q.rssi, held_us, data: q.data() };
                gw::encode_up(&m, &mut buf).map(|b| b.len()).ok()
            })
        });
        match n {
            Some(Some(n)) => {
                hubuart::write_all(&buf[..n]).await;
                OUT_MSGS.fetch_add(1, Relaxed);
                OUT_BYTES.fetch_add(n as u32, Relaxed);
                #[cfg(feature = "gwforge")]
                forge::relayed(&buf[..n]);
                continue;
            }
            Some(None) => continue,
            None => {}
        }
        // Nothing to send now: sleep until the cap allows the next packet,
        // the next report, or something new arrives.
        let now = now_ms();
        let cap = critical_section::with(|cs| UP.borrow_ref_mut(cs).wait_ms(now));
        let due = next_hello.min(next_report).saturating_sub(now);
        let wait = cap.unwrap_or(IDLE_MS).min(due).clamp(1, IDLE_MS);
        let _ = with_timeout(Duration::from_millis(wait), OUT_WAKE.wait()).await;
    }
}

/// The firmware name in our Hello: says the gateway holds no key, and a
/// bench build says so.
fn firmware_name() -> heapless::String<16> {
    use core::fmt::Write as _;
    let mut s = heapless::String::new();
    let what = if cfg!(feature = "gwforge") {
        "FORGE"
    } else if env!("ROTORSCOPE_BENCH_LIST").is_empty() {
        "nokey"
    } else {
        "bench"
    };
    let _ = write!(s, "gw {what} {}", env!("CARGO_PKG_VERSION"));
    s
}

/// Once a second: refusals per address and reason, then the totals.
async fn report(now: u64, buf: &mut [u8; gw::MAX_GW_FRAME]) {
    let mut out: heapless::Vec<relay::Refusal, 24> = heapless::Vec::new();
    let (counts, totals) = critical_section::with(|cs| {
        let mut up = UP.borrow_ref_mut(cs);
        up.refusals.drain(now, |r| {
            let _ = out.push(r);
        });
        (up.counts, up.refusals.totals)
    });
    for r in &out {
        let reason = gw::RefuseReason::ALL[r.reason.index()];
        send_up(&GwUp::Refused { src: r.src, reason, count: r.count, rssi: r.rssi, window_ms: r.window_ms }, buf).await;
        usb_line!(r#"{{"t":"refused","src":"{}","why":"{}","count":{},"rssi":{}}}"#, Mac(r.src), reason.name(), r.count, r.rssi);
    }
    let stats = gw::GwStats {
        uptime_s: (now / 1000) as u32,
        beacons_sent: BEACONS_SENT.load(Relaxed),
        foreign: counts.foreign,
        uart_drops: counts.uart_drops(),
        refused: totals,
    };
    send_up(&GwUp::Stats(stats), buf).await;
}

/// The bench's view, on USB once a second.
#[embassy_executor::task]
async fn stats() {
    let mut ticker = Ticker::every(Duration::from_secs(1));
    loop {
        ticker.next().await;
        let (c, totals, queued, roster) = critical_section::with(|cs| {
            let up = UP.borrow_ref(cs);
            (up.counts, up.refusals.totals, up.queued(), up.has_roster())
        });
        let (tx_len, taken, refused, given_up, acked, cancelled, superseded) = critical_section::with(|cs| {
            let tx = TX.borrow_ref(cs);
            (tx.len(), tx.taken, tx.refused, tx.given_up, tx.acked, tx.cancelled, BEACON.borrow_ref(cs).superseded)
        });
        usb_line!(
            r#"{{"t":"gw","up_ms":{},"key":"none","roster":{roster},"heard":{},"queued":{},"relayed":{},"foreign":{},"dups":{},"unprov_limited":{},"uart_drops":{},"queue_full":{},"stale":{},"unlisted":{},"not_in_roster":{},"over_budget":{},"old_firmware":{},"malformed":{},"up_waiting":{queued},"out_msgs":{},"out_bytes":{},"hub_bytes":{},"hub_msgs":{},"hub_corrupt":{},"hub_overrun":{},"hub_rx_errors":{},"rosters":{},"beacons":{},"beacon_fail":{},"beacon_bad":{},"beacon_superseded":{superseded},"tx_taken":{taken},"tx_refused":{refused},"tx_acked":{acked},"tx_given_up":{given_up},"tx_cancelled":{cancelled},"tx_attempts":{},"tx_waiting":{tx_len},"done_lost":{},"usb_drop":{}}}"#,
            now_ms(),
            HEARD.load(Relaxed),
            c.queued,
            c.sent,
            c.foreign,
            c.duplicates,
            c.unprovisioned_limited,
            c.uart_drops(),
            c.queue_full,
            c.stale,
            c.unlisted,
            totals[0],
            totals[1],
            totals[2],
            totals[3],
            OUT_MSGS.load(Relaxed),
            OUT_BYTES.load(Relaxed),
            hubuart::BYTES.load(Relaxed),
            HUB_MSGS.load(Relaxed),
            HUB_CORRUPT.load(Relaxed),
            hubuart::OVERRUN.load(Relaxed),
            hubuart::RX_ERRORS.load(Relaxed),
            ROSTERS.load(Relaxed),
            BEACONS_SENT.load(Relaxed),
            BEACONS_FAILED.load(Relaxed),
            BEACONS_BAD.load(Relaxed),
            TX_ATTEMPTS.load(Relaxed),
            DONE_LOST.load(Relaxed),
            usb::DROPPED.load(Relaxed)
        );
        #[cfg(feature = "gwforge")]
        forge::stats_line();
    }
}
