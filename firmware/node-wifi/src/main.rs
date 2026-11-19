//! Wi-Fi Remote ID listener: Node A (ESP32-S3, 2.4 GHz) and Node C (ESP32-C5,
//! 5 GHz) from one source tree.
//!
//! Shape: the driver's promiscuous callback copies management frames into a
//! queue (`rx`); `drain` pulls them out and runs the `rid` crate; `report`
//! prints JSON lines on USB and sends `link` frames to the hub; `main` ends up
//! as the channel hopper. Data frames are classified in the callback by the
//! `openlink` crate (open FPV / telemetry links) and only counted there;
//! `open_links` reports each stream at most once a second. Attack detection
//! (`wifiwatch`, Frame::Threat) is split the same way: deauth / auth / probe
//! floods are counted in the callback and judged by `threats`; beacons,
//! probe responses and action frames are checked in `drain`. Drone
//! hijacking (`wifiwatch::drones`) is split the same way: `drain` learns
//! drones' networks from their beacons, the callback follows deauths,
//! joins and data addresses on them, `threats` reports. With the hub's
//! `control content` opt-in on, the callback also queues MAVLink found in
//! open data frames (`wifiwatch::mavwifi`, own small pool, rate-limited)
//! and `mavlink` runs the hostile-MAVLink scan on it. The hopper
//! also leaves its plan now and then to scout a channel the plan skips
//! (`coverage::plans::scout_channel`). Receive-only:
//! nothing here transmits on the air, except with the `espnow` feature, where
//! `uplink` sends hub frames to an ESP-NOW gateway instead of the UART. With
//! `wirebus` the hub frames go on the shared wired bus instead
//! (`node_common::bus`, docs/WIREBUS.md), still receive-only on the air.
//!
//! Crash recovery (docs/BENCH.md "Crash recovery"): a panic prints its
//! backtrace and reboots (`custom_halt`); the hopper feeds the RTC watchdog
//! at every hop ([`WATCHDOG`]), so anything that stops the hopper (a task
//! that never yields, a stuck driver call) resets the chip. The `hello` line
//! says why it last reset.
#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

#[cfg(all(feature = "wirebus", feature = "espnow"))]
compile_error!("`wirebus` and `espnow` are two ways to reach the hub; build one");
#[cfg(all(feature = "wirebus", not(feature = "esp32c5")))]
compile_error!("`wirebus` pins are set for the XIAO ESP32-C5 only");
#[cfg(all(feature = "forgetest", feature = "faulttest"))]
compile_error!("`forgetest` and `faulttest` both read the USB console; build one");
#[cfg(all(feature = "floodtest", any(feature = "faulttest", feature = "forgetest")))]
compile_error!("`floodtest` reads the USB console, as `faulttest` and `forgetest` do; build one");

mod chip;
mod detect;
#[cfg(feature = "floodtest")]
mod flood;
#[cfg(feature = "forgetest")]
mod forge;
mod report;
mod role;
mod rx;
#[cfg(feature = "espnow")]
mod uplink;
mod usb;

use usb::usb_line;

use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

use embassy_executor::Spawner;
use embassy_time::{Duration, Instant, Ticker, with_timeout};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
#[cfg(not(any(feature = "espnow", feature = "wirebus")))]
use esp_hal::uart::{Config as UartConfig, Uart};
use esp_radio::wifi::{SecondaryChannel, WifiController};
use rid::wifi::{BEACON_OUI, BEACON_OUI_TYPE, NAN_DEST};

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

/// Unattended node: after a panic (backtrace already printed to USB),
/// reboot rather than sit halted until someone power-cycles the box.
#[unsafe(no_mangle)]
extern "Rust" fn custom_halt() -> ! {
    node_common::fault::reboot_after_panic()
}

/// Hang watchdog timeout. The hopper feeds it at every hop; the longest
/// legitimate gap is one dwell, which a hub plan may stretch to 10 s
/// (`role::MAX_DWELL_MS`), plus a scout visit and a retune.
const WATCHDOG: esp_hal::time::Duration = esp_hal::time::Duration::from_secs(30);
/// The drain task yields after this long of back-to-back frames, so a frame
/// flood cannot starve the hopper (and with it the watchdog).
const DRAIN_SLICE: Duration = Duration::from_millis(2);

/// Diagnostic log level. Off by default because USB carries machine-read JSON
/// lines; raise it (e.g. `Warn`) when debugging esp-radio itself. Those log
/// lines go straight to the port through `esp_println`, beside the `usb`
/// writer, so they can land inside a JSON line: a debugging aid only.
const LOG_LEVEL: log::LevelFilter = log::LevelFilter::Off;

/// Frames that carried Remote ID.
static RID_FOUND: AtomicU32 = AtomicU32::new(0);
/// Frames longer than a slot.
static TRUNCATED: AtomicU32 = AtomicU32::new(0);
/// Failed `set_channel` calls.
static CHANNEL_ERRORS: AtomicU32 = AtomicU32::new(0);
/// Beacon element-walk fit, see `report::StatsLine::iefit`.
static IE_FIT: [AtomicU32; 3] = [const { AtomicU32::new(0) }; 3];
/// Management frames heard, and hopper visits, per channel number (1-177).
/// Shows the hop plan is really being followed.
pub(crate) static PER_CHANNEL: [AtomicU32; CHANNELS] = [const { AtomicU32::new(0) }; CHANNELS];
static VISITS: [AtomicU32; CHANNELS] = [const { AtomicU32::new(0) }; CHANNELS];
const CHANNELS: usize = 178;
/// Failed band changes (C5).
static BAND_ERRORS: AtomicU32 = AtomicU32::new(0);
/// Scouting visits off the plan, and the last channel scouted.
static SCOUT_VISITS: AtomicU32 = AtomicU32::new(0);
static SCOUT_LAST: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);
/// Beacon-side attack detection totals (`wifiwatch::beacons::Totals`),
/// copied out of the drain task for the stats line.
static BEACON_CSA: AtomicU32 = AtomicU32::new(0);
static BEACON_THREATS: AtomicU32 = AtomicU32::new(0);

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger(LOG_LEVEL);

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: chip::HEAP_BYTES);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    // USB: JSON lines out through an async writer (`usb`); the receive side
    // is the bench console in bench builds.
    let (_usb_rx, usb_tx) = esp_hal::usb::usb_serial_jtag::UsbSerialJtag::new(peripherals.USB_DEVICE).into_async().split();
    spawner.spawn(usb::writer(usb_tx).expect("spawn usb writer"));

    // Which images this board holds (OTA slots) and the job, if this image
    // was started by a role switch.
    let mut images = node_common::Images::scan(peripherals.FLASH, chip::OWN_ROLES);
    role::init(images.stored_job(), images.can);
    let names: heapless::Vec<Option<&'static str>, 8> =
        images.slots.iter().map(|s| s.image.map(nodeimage::Image::project)).collect();
    report::hello_json(&names);

    #[cfg(not(any(feature = "espnow", feature = "wirebus")))]
    {
        // TX to the hub, RX for the hub's commands (`chip::hub_rx_pin!`).
        let (hub_rx, hub_tx) = Uart::new(peripherals.UART1, UartConfig::default().with_baudrate(chip::HUB_BAUD))
            .expect("hub UART config")
            .with_tx(chip::hub_tx_pin!(peripherals))
            .with_rx(chip::hub_rx_pin!(peripherals))
            .into_async()
            .split();
        spawner.spawn(report::hub_writer(hub_tx).expect("spawn hub_writer"));
        spawner.spawn(report::hub_reader(hub_rx).expect("spawn hub_reader"));
    }

    // The wired bus: TX open-drain on D9 (GPIO9), RX on D7 (GPIO12), served
    // from an interrupt-mode executor so a poll is answered within the hub's
    // window even while the drain task parses a frame.
    #[cfg(feature = "wirebus")]
    {
        let exec = alloc::boxed::Box::leak(alloc::boxed::Box::new(esp_rtos::embassy::InterruptExecutor::new(
            peripherals.FROM_CPU_INTR2,
        )));
        let bus = exec.start(esp_hal::interrupt::Priority::Priority2);
        bus.spawn(bus_task(peripherals.UART1, peripherals.GPIO9, peripherals.GPIO12).expect("spawn bus_task"));
    }

    let mut wifi = WifiController::new(peripherals.WIFI, Default::default()).expect("Wi-Fi init");
    if let Err(what) = chip::configure_radio(&mut wifi) {
        // Keep going: on the S3 a failed country/filter call costs efficiency,
        // not reception. The error line says which call to look at.
        usb_line!(r#"{{"t":"err","what":"{what}"}}"#);
    }
    // With the radio on, the hardware RNG is truly random: the salt that
    // makes the Remote ID source table's slots unpredictable.
    report::init_shed(esp_hal::rng::Rng::new().random());

    rx::init();
    let mut sniffer = wifi.sniffer();
    sniffer.set_receive_cb(rx::on_frame);
    sniffer.set_promiscuous_mode(true).expect("promiscuous mode");

    #[cfg(feature = "espnow")]
    spawner.spawn(uplink::run(wifi.esp_now()).expect("spawn uplink"));
    spawner.spawn(drain().expect("spawn drain"));
    spawner.spawn(stats().expect("spawn stats"));
    spawner.spawn(role::control(images).expect("spawn control"));
    spawner.spawn(threats().expect("spawn threats"));
    if chip::OPEN_LINKS {
        spawner.spawn(open_links().expect("spawn open_links"));
        spawner.spawn(mavlink().expect("spawn mavlink"));
    }

    #[cfg(feature = "faulttest")]
    spawner.spawn(faulttest(_usb_rx).expect("spawn faulttest"));
    #[cfg(feature = "forgetest")]
    spawner.spawn(forgetest(_usb_rx).expect("spawn forgetest"));
    #[cfg(feature = "floodtest")]
    spawner.spawn(floodtest(_usb_rx).expect("spawn floodtest"));

    node_common::fault::arm(WATCHDOG);
    // `sniffer` must stay alive: dropping it turns promiscuous mode off.
    hop_forever(&mut wifi).await
}

/// Bench only: `fault panic` / `fault hang` typed on USB (docs/BENCH.md).
#[cfg(feature = "faulttest")]
#[embassy_executor::task]
async fn faulttest(mut rx: esp_hal::usb::usb_serial_jtag::UsbSerialJtagRx<'static, esp_hal::Async>) {
    // The console waits 200 ms after its `panic` / `hang` line, so the USB
    // writer sends it before the fault.
    node_common::fault::console(|b| rx.drain_rx_fifo(b), |l| usb_line!("{l}")).await
}

/// Bench only: `forge <what>` typed on USB sends a frame this node may not
/// originate (docs/BENCH.md "Origin checks").
#[cfg(feature = "forgetest")]
#[embassy_executor::task]
async fn forgetest(mut rx: esp_hal::usb::usb_serial_jtag::UsbSerialJtagRx<'static, esp_hal::Async>) {
    forge::console(|b| rx.drain_rx_fifo(b)).await
}

/// Bench only: `flood <kind> <rate>` typed on USB injects synthetic frames
/// into the receive path (docs/BENCH.md "Frame flood").
#[cfg(feature = "floodtest")]
#[embassy_executor::task]
async fn floodtest(mut rx: esp_hal::usb::usb_serial_jtag::UsbSerialJtagRx<'static, esp_hal::Async>) {
    flood::console(|b| rx.drain_rx_fifo(b)).await
}

/// The wired-bus node: hub frames from `report::HUB_QUEUE`, `Assign`s to
/// `role` (docs/WIREBUS.md).
#[cfg(feature = "wirebus")]
#[embassy_executor::task]
async fn bus_task(
    uart: esp_hal::peripherals::UART1<'static>,
    tx: esp_hal::peripherals::GPIO9<'static>,
    rx: esp_hal::peripherals::GPIO12<'static>,
) -> ! {
    // Made here: an async UART is bound to the executor that polls it.
    let uart = node_common::bus::uart(uart, tx, rx);
    let mut out = node_common::bus::Staged::new(|v: &mut node_common::bus::Unit| match report::HUB_QUEUE.try_receive() {
        Ok(f) => v.extend_from_slice(&f).is_ok(),
        Err(_) => false,
    });
    node_common::bus::run(uart, &mut out, &mut |a| {
        // Full: an older command is still queued; the hub resends.
        let _ = role::COMMANDS.try_send(a);
    })
    .await
}

/// Walk the current channel plan forever; start over at once on a new plan
/// (`role::CHANGED`). Every `SCOUT_EVERY_MS`, between two hops, visit one
/// channel the plan skips for `SCOUT_MS`. Runs in `main`, which owns the
/// controller.
async fn hop_forever(wifi: &mut WifiController<'_>) -> ! {
    use coverage::plans::{SCOUT_EVERY_MS, SCOUT_MS, scout_channel};
    let mut tuner = Tuner { reported: [false; CHANNELS], band: None };
    let mut last_scout = Instant::now();
    let mut scout_n: usize = 0;
    loop {
        let plan = role::current().plan;
        'plan: loop {
            for hop in &plan {
                // The hopper going round is what keeps the node seeing.
                node_common::fault::feed();
                tuner.tune(wifi, hop.channel()).await;
                if with_timeout(Duration::from_millis(hop.dwell_ms()), role::CHANGED.wait()).await.is_ok() {
                    break 'plan;
                }
                if last_scout.elapsed() < Duration::from_millis(u64::from(SCOUT_EVERY_MS)) {
                    continue;
                }
                last_scout = Instant::now();
                let Some(ch) = scout_channel(plan.iter().map(|h| h.channel()), scout_n) else { continue };
                scout_n = scout_n.wrapping_add(1);
                // Under ESP-NOW this can stretch one absence from channel 6
                // by SCOUT_MS; the uplink only sends while tuned to 6 anyway.
                tuner.tune(wifi, ch).await;
                SCOUT_VISITS.fetch_add(1, Relaxed);
                SCOUT_LAST.store(ch, Relaxed);
                if with_timeout(Duration::from_millis(u64::from(SCOUT_MS)), role::CHANGED.wait()).await.is_ok() {
                    break 'plan;
                }
            }
        }
    }
}

/// Channel changes, with the band switched as needed (C5).
struct Tuner {
    /// One error line per channel, then only the counter, so a channel the
    /// driver rejects (e.g. 149 under the wrong country) is visible without
    /// flooding the stream.
    reported: [bool; CHANNELS],
    band: Option<bool>, // Some(true) = 5 GHz
}

impl Tuner {
    async fn tune(&mut self, wifi: &mut WifiController<'_>, ch: u8) {
        #[cfg(feature = "espnow")]
        uplink::wait_idle().await;
        let five = ch > 14;
        if self.band != Some(five) {
            match chip::set_band(wifi, ch) {
                Ok(()) => self.band = Some(five),
                Err(e) => {
                    BAND_ERRORS.fetch_add(1, Relaxed);
                    usb_line!(r#"{{"t":"err","what":"set_band","ch":{ch},"e":"{e:?}"}}"#);
                }
            }
        }
        match wifi.set_channel(ch, SecondaryChannel::None) {
            Ok(()) => {
                rx::TUNED_CHANNEL.store(ch, Relaxed);
                if let Some(v) = VISITS.get(usize::from(ch)) {
                    v.fetch_add(1, Relaxed);
                }
            }
            Err(e) => {
                CHANNEL_ERRORS.fetch_add(1, Relaxed);
                if let Some(seen @ false) = self.reported.get_mut(usize::from(ch)) {
                    *seen = true;
                    usb_line!(r#"{{"t":"err","what":"set_channel","ch":{ch},"e":"{e:?}"}}"#);
                }
            }
        }
    }
}

/// Cheap test for "could this be Remote ID?", so `rid` only parses likely
/// frames: a NAN action frame to the Remote ID address, or a beacon containing
/// the ASD-STAN vendor OUI+type anywhere in its body.
fn maybe_rid(frame: &[u8]) -> bool {
    match frame[0] >> 4 {
        8 => {
            let needle = [BEACON_OUI[0], BEACON_OUI[1], BEACON_OUI[2], BEACON_OUI_TYPE];
            frame.get(36..).is_some_and(|body| body.windows(4).any(|w| w == needle))
        }
        13 => frame.get(4..10) == Some(&NAN_DEST[..]),
        _ => false,
    }
}

/// Field check of `chip::FCS_LEN` on every untruncated beacon: does the element
/// list end exactly at `sig_len - 4`, or at `sig_len`?
fn check_ie_fit(f: &rx::RxFrame) {
    // Beacon: 24-byte header + 12 bytes fixed fields, then elements.
    if f.data[0] != 0x80 || f.truncated() || f.raw_len < 40 {
        return;
    }
    let ends_at = |end: usize| {
        let mut ies = &f.data[36..end];
        while let [_, len, rest @ ..] = ies {
            match rest.get(usize::from(*len)..) {
                Some(after) => ies = after,
                None => return false,
            }
        }
        ies.is_empty()
    };
    let raw = usize::from(f.raw_len);
    let idx = match (ends_at(raw - 4), ends_at(raw)) {
        (true, false) => 0,
        (false, true) => 1,
        _ => 2,
    };
    IE_FIT[idx].fetch_add(1, Relaxed);
}

#[embassy_executor::task]
async fn drain() {
    let mut detect = detect::Detect::new();
    // Beacon-side attack detection (~4.3 KB, in the task's static state).
    let mut watch = wifiwatch::beacons::Beacons::new();
    let mut slice = Instant::now();
    loop {
        // `rx::next` does not yield while frames are queued: under a flood
        // this task would hold the executor and starve the hopper.
        if slice.elapsed() >= DRAIN_SLICE {
            embassy_futures::yield_now().await;
            slice = Instant::now();
        }
        let f = rx::next().await;
        if f.truncated() {
            TRUNCATED.fetch_add(1, Relaxed);
        }
        check_ie_fit(&f);
        if let Some(c) = PER_CHANNEL.get(usize::from(f.channel)) {
            c.fetch_add(1, Relaxed);
        }

        let frame = f.mpdu();
        if frame.len() < 24 {
            continue;
        }
        detect.frame(&f, frame);
        // A drone's own beacon: let the callback's drone watch know it.
        if let Some(d) = watch.frame(frame, f.rssi, f.channel, f.up_us, &mut |s| {
            report::threat(&s);
        }) {
            critical_section::with(|cs| rx::DRONES.borrow_ref_mut(cs).learn(&d, f.up_us));
            rx::DRONES_ON.store(true, Relaxed);
        }
        BEACON_CSA.store(watch.totals.csa, Relaxed);
        BEACON_THREATS.store(watch.totals.threats, Relaxed);
        if !maybe_rid(frame) {
            continue;
        }
        let Some(found) = rid::wifi::extract(frame) else { continue };
        RID_FOUND.fetch_add(1, Relaxed);
        // Known source within its rate, or new source within the budget;
        // else shed (counted), on both outputs.
        let Some(kind) = report::rid_admit(found.src) else {
            #[cfg(feature = "floodtest")]
            flood::note_rid(found.src, false, false);
            continue;
        };

        let mut messages = report::Messages::new();
        let error = match rid::message::raw_messages(found.payload) {
            Ok(iter) => {
                for raw in iter {
                    // Cannot overflow: a pack holds at most MAX_PACK messages.
                    let _ = messages.push(*raw);
                }
                None
            }
            Err(e) => Some(e),
        };
        let hit = report::Hit {
            transport: found.transport,
            channel: f.channel,
            rssi: f.rssi,
            mac: found.src,
            counter: found.counter,
            up_us: f.up_us,
            messages: &messages,
            error,
        };
        report::detection_json(&hit);
        let _queued = report::detection_hub(&hit, kind);
        #[cfg(feature = "floodtest")]
        flood::note_rid(found.src, true, _queued);
    }
}

/// Shortest gap between two reports of one open-link stream. A video
/// downlink is hundreds of frames a second; one line a second, carrying the
/// running count, says everything the hub shows.
const OPEN_REPORT_GAP_US: u64 = 1_000_000;

/// Report open-link streams that have new frames, each at most once per
/// `OPEN_REPORT_GAP_US`. Checked four times a second, so a new link is
/// reported within 250 ms of its first frame. Never more at once than the
/// hub queue has room for open links and the open-link budget allows
/// (`wifiwatch::shed`): with the table full (16 streams, e.g. injected
/// wfb-ng frames with many link ids) they go out in turns, the
/// longest-waiting first, instead of 16 at once into an 8-frame queue.
#[embassy_executor::task]
async fn open_links() {
    use wifiwatch::shed::Kind;
    let mut ticker = Ticker::every(Duration::from_millis(250));
    loop {
        ticker.next().await;
        let now = Instant::now().as_micros();
        let room = report::hub_room(Kind::OpenLink);
        // Copy out under the lock (the callback writes the table); format
        // and send after releasing it.
        let mut due: heapless::Vec<openlink::Sighting, { rx::OPEN_STREAMS }> = heapless::Vec::new();
        critical_section::with(|cs| {
            rx::SIGHTINGS.borrow_ref_mut(cs).take_due_max(now, OPEN_REPORT_GAP_US, room, |s| {
                // Cannot overflow: the table holds OPEN_STREAMS entries.
                let _ = due.push(*s);
            })
        });
        for s in &due {
            // Over budget: shed (counted). It was taken, so it waits a
            // second like a reported one; its count keeps running.
            if !report::admit(Kind::OpenLink) {
                continue;
            }
            report::open_json(s);
            report::open_hub(s);
        }
    }
}

/// MAVLink datagrams analysed and findings sent (`mavlink` task).
static MAV_DATAGRAMS: AtomicU32 = AtomicU32::new(0);
static MAV_FINDINGS: AtomicU32 = AtomicU32::new(0);
/// `mavwatch::Pressure` (WP-11a): new alarms held back, sources / name sets
/// holding evidence given up, newcomers turned away, streams turned away.
static MAV_PRESSURE: [AtomicU32; 4] = [const { AtomicU32::new(0) }; 4];

/// Hostile MAVLink over open Wi-Fi (protocol 13, `wifiwatch::mavwifi`,
/// docs/DRONE-ATTACKS.md): the datagrams the receive callback queued while
/// the hub's `control content` opt-in is on, through `mavwatch`. Each
/// finding goes to the hub as node-lr sends it: a `Threat`
/// (`MavlinkAttack`, with the Wi-Fi channel) then the `MavlinkEvidence`
/// frame. Idle (never woken) while the opt-in is off.
#[embassy_executor::task]
async fn mavlink() {
    // ~8 KB (`mavwatch::Watch`), in the task's static state.
    let mut scan = wifiwatch::mavwifi::MavWifi::new();
    loop {
        let d = rx::next_mavlink().await;
        // Turned off since it was queued: dropped unread.
        if !rx::MAV_ON.load(Relaxed) {
            continue;
        }
        scan.feed(&d.meta, d.payload(), &mut |s, e| {
            // The evidence only with its threat (both shed together).
            if report::threat(s) {
                report::mavlink_evidence(e);
            }
        });
        MAV_DATAGRAMS.store(scan.totals.datagrams, Relaxed);
        MAV_FINDINGS.store(scan.totals.findings, Relaxed);
        let p = scan.pressure();
        for (a, v) in MAV_PRESSURE.iter().zip([p.throttled, p.evicted_evidence, p.refused, p.streams_refused]) {
            a.store(v, Relaxed);
        }
    }
}

/// Judge the flood counters the receive callback fills (`rx::FLOODS`) and
/// the drone-link watch (`rx::DRONES`) four times a second; each flood is
/// reported at most once per one-second window, each drone kind at most
/// once a second per drone.
#[embassy_executor::task]
async fn threats() {
    let mut ticker = Ticker::every(Duration::from_millis(250));
    // Copied out under the lock, reported after releasing it.
    const DUE: usize = wifiwatch::floods::TABLE + 1 + 2 * wifiwatch::drones::NETS;
    let mut due: heapless::Vec<link::ThreatSighting, DUE> = heapless::Vec::new();
    loop {
        ticker.next().await;
        let now = Instant::now().as_micros();
        due.clear();
        critical_section::with(|cs| {
            rx::FLOODS.borrow_ref_mut(cs).poll(now, &mut |s| {
                // Cannot overflow: one per table entry plus the probe counter.
                let _ = due.push(s);
            });
            let mut drones = rx::DRONES.borrow_ref_mut(cs);
            drones.poll(now, &mut |s| {
                // Cannot overflow: at most two per drone network.
                let _ = due.push(s);
            });
            rx::DRONES_ON.store(!drones.is_empty(), Relaxed);
        });
        for s in &due {
            report::threat(s);
        }
    }
}

/// Stats every second on both outputs; the hub also gets Hello + Status every
/// 5 s so a hub that restarts learns who is attached and what it can do.
#[embassy_executor::task]
async fn stats() {
    let mut ticker = Ticker::every(Duration::from_secs(1));
    let mut last_seen = 0;
    report::hello_hub();
    let mut tick: u32 = 0;
    loop {
        ticker.next().await;
        tick = tick.wrapping_add(1);
        let seen = rx::MGMT_SEEN.load(Relaxed);
        let job = role::current();
        let mut per_ch: heapless::Vec<(u8, u32, u32), 24> = heapless::Vec::new();
        for h in &job.plan {
            let ch = h.channel();
            if per_ch.iter().all(|p| p.0 != ch) {
                let i = usize::from(ch);
                let _ = per_ch.push((ch, PER_CHANNEL[i].load(Relaxed), VISITS[i].load(Relaxed)));
            }
        }
        let floods = critical_section::with(|cs| rx::FLOODS.borrow_ref(cs).totals);
        let (shed, rid_src, shed_total) = report::shed_counts();
        let drones = critical_section::with(|cs| rx::DRONES.borrow_ref(cs).totals);
        let line = report::StatsLine {
            t: "stats",
            up_us: Instant::now().as_micros(),
            role: job.role,
            plan_id: job.id,
            cmds: [role::APPLIED.load(Relaxed), role::REPEATS.load(Relaxed), role::REJECTED.load(Relaxed)],
            ch: rx::TUNED_CHANNEL.load(Relaxed),
            seen,
            fps: seen.wrapping_sub(last_seen),
            data: rx::DATA_SEEN.load(Relaxed),
            open: rx::OPEN_SEEN.load(Relaxed),
            open_ev: critical_section::with(|cs| rx::SIGHTINGS.borrow_ref(cs).evicted),
            rid: RID_FOUND.load(Relaxed),
            drop: rx::DROPPED.load(Relaxed),
            qmax: rx::HIGH_WATER.swap(0, Relaxed),
            other: rx::OTHER.load(Relaxed),
            trunc: TRUNCATED.load(Relaxed),
            ch_err: CHANNEL_ERRORS.load(Relaxed),
            hub_drop: report::HUB_DROPPED.load(Relaxed).wrapping_add(shed_total),
            shed,
            rid_src,
            usb_drop: usb::DROPPED.load(Relaxed),
            alt: [detect::DJI_ALT.load(Relaxed), detect::WIFI_DRONE_ALT.load(Relaxed)],
            clamp: rx::CLAMPED.load(Relaxed),
            iefit: IE_FIT.each_ref().map(|c| c.load(Relaxed)),
            per_ch,
            band_err: BAND_ERRORS.load(Relaxed),
            dji: detect::DJI.load(Relaxed),
            wdrone: detect::WIFI_DRONES.load(Relaxed),
            kill: floods.deauth.wrapping_add(floods.disassoc),
            auth: floods.auth,
            probe: floods.probe,
            csa: BEACON_CSA.load(Relaxed),
            threat: floods.threats.wrapping_add(BEACON_THREATS.load(Relaxed)).wrapping_add(drones.threats),
            scout: (SCOUT_VISITS.load(Relaxed), SCOUT_LAST.load(Relaxed)),
            mav: [
                u32::from(rx::MAV_ON.load(Relaxed)),
                rx::MAV_CANDIDATES.load(Relaxed),
                rx::MAV_LIMITED.load(Relaxed),
                rx::MAV_DROPPED.load(Relaxed),
                MAV_DATAGRAMS.load(Relaxed),
                MAV_FINDINGS.load(Relaxed),
            ],
            mavp: MAV_PRESSURE.each_ref().map(|c| c.load(Relaxed)),
            dnet: [drones.learned, drones.evicted, drones.refused],
            robust: {
                let h = node_common::health();
                [
                    h.plans_refused,
                    h.deaf_fallbacks,
                    h.deaf_blamed,
                    h.deaf_outages,
                    h.deaf_refused,
                    h.switches_refused,
                    h.switch_wait_s,
                ]
            },
        };
        last_seen = seen;
        report::stats_json(&line);
        report::stats_hub(&line);
        #[cfg(feature = "wirebus")]
        usb::line(node_common::bus::json_line().as_bytes());
        if tick.is_multiple_of(5) {
            report::hello_hub();
        }
    }
}
