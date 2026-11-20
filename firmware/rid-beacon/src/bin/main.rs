//! rid-beacon: bench Remote ID test transmitter.
//!
//! One firmware, two chips:
//! - ESP32-S3: Wi-Fi beacon + NAN on 2.4 GHz channel 6, BLE 4 legacy, BLE 5
//!   extended (1M) and BLE 5 Long Range (coded PHY).
//! - ESP32-C5: the same, with Wi-Fi on 5 GHz channel 149 by default
//!   (country "US" set explicitly) and a console command to drop to 2.4 GHz.
//!
//! Also, off until a console key turns one on: fake frames shaped like the
//! open digital FPV links (wfb-ng/OpenIPC, OpenHD, RubyFPV, DroneBridge),
//! built by `openlink::build` with obviously fake IDs. Never DJI DroneID.
//!
//! Conduct (see README.md): obviously fake identity, minimum TX power,
//! simulated position, bench use only.
#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use bt_hci::controller::ExternalController;
use bt_hci::param::AdvSet;
use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_time::{Duration, Instant, Ticker, Timer};
use esp_backtrace as _;
use esp_hal::Blocking;
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::usb::usb_serial_jtag::{UsbSerialJtag, UsbSerialJtagRx};
use esp_println::println;
use esp_radio::ble::controller::BleConnector;
use esp_radio::wifi::sniffer::Sniffer;
use esp_radio::wifi::{
    ControllerConfig, CountryInfo, PowerSaveMode, SecondaryChannel, WifiController,
};
use log::{error, info, warn};
use trouble_host::prelude::*;

use rid_beacon::control::{self, Reply, Tx};
use rid_beacon::flight::{self, Centre};
use rid_beacon::hci_fix::FixedTransport;
use rid_beacon::rid_tx;
#[cfg(feature = "scripted")]
use rid_beacon::script;

extern crate alloc;

// --- radio power -----------------------------------------------------------
//
// Lowest practical power: this is for a bench, metres not kilometres.

/// Wi-Fi max TX power in esp-radio's 0.25 dBm units. 8 = 2 dBm, the lowest
/// value `esp_wifi_set_max_tx_power` accepts (range 8..=84). esp-radio's own
/// default is 20 (5 dBm).
const WIFI_TX_POWER_QDBM: i8 = 8;
/// BLE controller default TX power: -15 dBm, the lowest level esp-radio
/// exposes on both chips (its default is +9 dBm).
const BLE_TX_POWER: esp_radio::ble::TxPower = esp_radio::ble::TxPower::N15;
/// Per-advertising-set TX power we ask for. The controller picks the nearest
/// level it supports at or below this (HCI semantics), i.e. its floor.
const BLE_ADV_TX_POWER: TxPower = TxPower::Minus16dBm;

// --- identity on air ---------------------------------------------------------

/// Last address byte, so an S3 and a C5 on the same bench are distinguishable.
#[cfg(feature = "esp32s3")]
const CHIP_BYTE: u8 = 0x53; // 'S'
#[cfg(feature = "esp32c5")]
const CHIP_BYTE: u8 = 0x43; // 'C'

/// Wi-Fi source address / BSSID: locally administered (0x02 bit set), so it
/// cannot collide with any manufacturer's OUI. "RID" in ASCII.
const WIFI_SRC: [u8; 6] = [0x02, 0x52, 0x49, 0x44, 0x00, CHIP_BYTE];
/// BLE static random address (top two bits of the MSB set), shown by
/// scanners as C2:52:49:44:00:<chip>. Byte order is little-endian (BdAddr).
const BLE_ADDR: [u8; 6] = [CHIP_BYTE, 0x00, 0x44, 0x49, 0x52, 0xC2];

// --- Wi-Fi ------------------------------------------------------------------

/// Raw TX goes out through the station interface. The Wi-Fi driver must be
/// started (it is: the controller starts in station mode) but need not be
/// associated; esp_wifi_80211_tx accepts beacons and action frames while
/// unconnected, on whatever channel the radio is parked on.
const USE_STA_INTERFACE: bool = true;
/// The S3's only channel (the C5 takes its channel from `control`).
#[cfg(not(feature = "esp32c5"))]
const WIFI_CH_24: u8 = 6;
/// Fake open-link frames per second while one is on. Real video links send
/// hundreds; 20 is plenty for a hopping listener (it spends 50 ms on 161 and
/// 165 every 800 ms) and keeps air time negligible.
const OPEN_HZ: u32 = 20;
/// Send core-c's NAN synchronisation beacon just before each NAN action
/// frame, as opendroneid-core-c's transmitter does. Some NAN receivers need
/// it to treat the action frame as part of a cluster; sniffers do not care.
const SEND_NAN_SYNC_BEACON: bool = true;

// --- BLE --------------------------------------------------------------------

/// How often the BLE 4 legacy payload changes. With five message types that
/// is each type once per second, the F3411 minimum for Location.
const BLE_LEGACY_ROTATE: Duration = Duration::from_millis(200);
/// Legacy advertising interval: each rotated payload goes out ~2 times.
const BLE_LEGACY_INTERVAL: Duration = Duration::from_millis(100);
/// Extended (1M) and coded advertising intervals; the pack changes every 1 s.
const BLE_EXT_INTERVAL: Duration = Duration::from_millis(200);
const BLE_CODED_INTERVAL: Duration = Duration::from_millis(300);

esp_bootloader_esp_idf::esp_app_desc!();

/// After a panic (backtrace already printed to USB), reboot rather than sit
/// halted, as every other Rotorscope board does (docs/BENCH.md "Crash
/// recovery"); 500 ms first so the host can read the backtrace.
#[unsafe(no_mangle)]
extern "Rust" fn custom_halt() -> ! {
    esp_hal::delay::Delay::new().delay_millis(500);
    esp_hal::system::software_reset()
}

fn uptime_s() -> f64 {
    Instant::now().as_micros() as f64 / 1e6
}

/// The five Remote ID messages for this instant: the circle, or in a bench
/// build with `scripted` the scripted path in use (src/script.rs).
fn messages_now(centre: &Centre) -> Result<[rid::RawMessage; rid_tx::N_MESSAGES], rid::Error> {
    let t = uptime_s();
    #[cfg(feature = "scripted")]
    if let Some(s) = scripted_now(centre) {
        return rid_tx::messages_scripted(&s);
    }
    rid_tx::messages(centre, &flight::at(centre, t), None)
}

/// Bench builds with `scripted`: where the scripted path in use is now
/// (`None` while the circle is flown).
#[cfg(feature = "scripted")]
fn scripted_now(centre: &Centre) -> Option<script::Sample> {
    let (p, secs) = script::current(Instant::now().as_millis() as u32);
    script::at(p, centre.lat, centre.lon, secs)
}

/// Bench builds with `scripted`: the image's bench tag, which
/// tools/dev-board.sh finds in the ELF (it refuses to flash it without
/// ROTORSCOPE_BENCH=1), printed at boot.
#[cfg(feature = "scripted")]
#[used]
static BENCH_TAG: &str = concat!(env!("ROTORSCOPE_BENCH_TAG"), "\0");

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    #[cfg(feature = "esp32s3")]
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 73744);
    #[cfg(feature = "esp32c5")]
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 65536);
    // Wi-Fi + BLE coexistence needs more heap than either alone.
    esp_alloc::heap_allocator!(size: 64 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    let centre = flight::centre();
    if !centre.from_env_ok {
        warn!("RID_TEST_LAT/RID_TEST_LON unparseable or out of range; using the default centre");
    }

    // Prove the constants encode and decode before anything is transmitted.
    let msgs = rid_tx::messages(&centre, &flight::at(&centre, 0.0), None).expect("encode");
    if let Err(e) = rid_tx::self_check(&msgs) {
        panic!("Remote ID self-check failed: {e:?}");
    }

    // ---- Wi-Fi: started in station mode (never associates), country US.
    // "US" must be explicit: a recent esp-hal change moves the default
    // country to "01", which rejects 5 GHz channel 149.
    let wifi_cfg = ControllerConfig::default().with_country_info(CountryInfo::from(*b"US"));
    let wifi = WifiController::new(peripherals.WIFI, wifi_cfg).expect("Wi-Fi init");
    let sniffer = wifi.sniffer();

    // ---- BLE.
    let ble_cfg = esp_radio::ble::Config::default().with_default_tx_power(BLE_TX_POWER);
    // The C5's controller defaults to one advertising set and 31-byte
    // extended data; we need three sets (four with the BlueZ helper) and a
    // 134-byte pack.
    #[cfg(feature = "esp32c5")]
    let ble_cfg = ble_cfg
        .with_multi_adv_instances(4)
        .with_ext_adv_max_size(251);
    let transport = BleConnector::new(peripherals.BT, ble_cfg).expect("BLE init");
    let ble_controller: ExternalController<_, 20> =
        ExternalController::new(FixedTransport::new(transport));
    // Three Remote ID sets plus the optional BlueZ helper set.
    let mut resources: HostResources<DefaultPacketPool, 1, 1, 4> = HostResources::new();
    let ble_address = Address::random(BLE_ADDR);
    let stack = trouble_host::new(ble_controller, &mut resources)
        .set_random_address(ble_address)
        .build();

    println!(
        r#"{{"t":"boot","fw":"rid-beacon","chip":"{}","uas_id":"{}","operator_id":"{}","self_id":"{}","ssid":"{}","wifi_src":"{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}","ble_addr":"{}","wifi_tx_qdbm":{},"ble_tx_dbm":-15,"centre":[{:.7},{:.7}],"radius_m":{},"period_s":{},"open_ids":{{"wfb_link_id":"{:06x}","ruby_vehicle_id":"{:08x}","dronebridge_comm_id":"{:02x}"}},"cmds":"w n l e c a x b f o r d 6 7 8 9 ?"}}"#,
        if cfg!(feature = "esp32c5") {
            "esp32c5"
        } else {
            "esp32s3"
        },
        core::str::from_utf8(rid_tx::UAS_ID).unwrap_or("?"),
        core::str::from_utf8(rid_tx::OPERATOR_ID).unwrap_or("?"),
        core::str::from_utf8(rid_tx::SELF_ID).unwrap_or("?"),
        core::str::from_utf8(rid_tx::SSID).unwrap_or("?"),
        WIFI_SRC[0],
        WIFI_SRC[1],
        WIFI_SRC[2],
        WIFI_SRC[3],
        WIFI_SRC[4],
        WIFI_SRC[5],
        ble_address,
        WIFI_TX_POWER_QDBM,
        centre.lat,
        centre.lon,
        flight::RADIUS_M,
        flight::PERIOD_S,
        openlink::build::FAKE_WFB_LINK_ID,
        openlink::build::FAKE_RUBY_VEHICLE_ID,
        openlink::build::FAKE_DRONEBRIDGE_COMM_ID,
    );

    #[cfg(feature = "scripted")]
    println!(
        r#"{{"t":"warn","what":"bench_build","tag":"{}","note":"BENCH BUILD: scripted flights on keys 0-5 (0 circle, 1 beeline, 2 fast inbound, 3 straight pass, 4 low hover, 5 drop); the circle until a key is pressed"}}"#,
        BENCH_TAG.trim_end_matches('\0')
    );

    spawner.spawn(wifi_task(wifi, sniffer, centre).unwrap());

    let usb = UsbSerialJtag::new(peripherals.USB_DEVICE);
    let (rx, _tx) = usb.split();
    spawner.spawn(console_task(rx).unwrap());
    spawner.spawn(summary_task(centre).unwrap());

    // BLE runs here: the host runner and the advertiser are two halves of the
    // same stack borrow, so they share this task.
    let mut runner = stack.runner();
    let mut peripheral = stack.peripheral();
    join(
        async {
            loop {
                if let Err(e) = runner.run().await {
                    error!("BLE host runner stopped: {e:?}; restarting");
                    Timer::after(Duration::from_secs(1)).await;
                }
            }
        },
        ble_advertise(&stack, &mut peripheral, centre),
    )
    .await;
    unreachable!()
}

// ---------------------------------------------------------------------------
// Wi-Fi: beacon and NAN through raw 802.11 TX.

/// Park the radio on the channel the console asked for.
fn tune(wifi: &mut WifiController<'static>, ch: u8) -> u8 {
    #[cfg(feature = "esp32c5")]
    {
        use esp_radio::wifi::BandMode;
        let band = if ch > 14 {
            BandMode::_5G
        } else {
            BandMode::_2_4G
        };
        if let Err(e) = wifi.set_band_mode(band) {
            error!("set_band_mode: {e:?}");
        }
        if let Err(e) = wifi.set_channel(ch, SecondaryChannel::None) {
            error!("set_channel({ch}): {e:?}");
        }
        // Re-assert the power cap after a band switch, in case the driver
        // recomputes limits per band.
        if let Err(e) = wifi.set_max_tx_power(WIFI_TX_POWER_QDBM) {
            error!("set_max_tx_power: {e:?}");
        }
        ch
    }
    #[cfg(not(feature = "esp32c5"))]
    {
        let _ = ch;
        if let Err(e) = wifi.set_channel(WIFI_CH_24, SecondaryChannel::None) {
            error!("set_channel({WIFI_CH_24}): {e:?}");
        }
        WIFI_CH_24
    }
}

#[embassy_executor::task]
#[allow(clippy::large_stack_frames, reason = "~1.5 KB of frame and message buffers; the executor stack has room and keeping them local avoids shared statics")]
async fn wifi_task(mut wifi: WifiController<'static>, mut sniffer: Sniffer, centre: Centre) {
    if let Err(e) = wifi.set_max_tx_power(WIFI_TX_POWER_QDBM) {
        error!("set_max_tx_power({WIFI_TX_POWER_QDBM}): {e:?}");
    }
    // Modem sleep, not esp-radio's default "none": ESP-IDF refuses
    // WIFI_PS_NONE while Bluetooth is enabled, because the coexistence
    // arbiter needs Wi-Fi idle windows to give the radio to BLE.
    if let Err(e) = wifi.set_power_saving(PowerSaveMode::Minimum) {
        error!("set_power_saving: {e:?}");
    }
    let mut channel = tune(&mut wifi, control::channel());
    info!("Wi-Fi raw TX on channel {channel}");

    let beacon_hz = beacon_hz();
    info!("Wi-Fi beacon rate {beacon_hz} Hz, NAN 1 Hz");
    let mut counters = [0u8; 2]; // F3411 message counters: beacon, NAN
    let mut frame = [0u8; rid_tx::FRAME_MAX];
    let mut open_seq = 0u32;
    // 50 ms ticks, 20 a second. Remote ID uses the even ticks as ten 100 ms
    // slots: beacons in slots 0, 10/hz, ...; NAN in slot 5, so at the default
    // 1 Hz the two are half a second apart and never queue behind each
    // other. A fake open link, when on, sends on every tick.
    let mut ticker = Ticker::every(Duration::from_millis(1000 / OPEN_HZ as u64));
    let mut tick = 0u32;
    loop {
        ticker.next().await;
        let this_tick = tick;
        tick = (tick + 1) % 20;
        if control::channel() != channel {
            channel = tune(&mut wifi, control::channel());
            info!("Wi-Fi retuned to channel {channel}");
        }
        if let Some((system, _)) = control::open() {
            open_send(&mut sniffer, system, open_seq, &mut frame);
            open_seq = open_seq.wrapping_add(1);
        }
        if !this_tick.is_multiple_of(2) {
            continue;
        }
        let this = this_tick / 2;
        let beacon_due = this.is_multiple_of(10 / beacon_hz);
        let nan_due = this == 5;
        for kind in [Tx::WifiBeacon, Tx::WifiNan] {
            let due = if kind == Tx::WifiBeacon {
                beacon_due
            } else {
                nan_due
            };
            if due && control::enabled(kind) {
                wifi_send(
                    &mut sniffer,
                    kind,
                    channel,
                    &centre,
                    &mut counters,
                    &mut frame,
                );
            }
        }
    }
}

/// Build and transmit one fake open-FPV frame (`openlink::build`).
fn open_send(
    sniffer: &mut Sniffer,
    system: openlink::System,
    seq: u32,
    frame: &mut [u8; rid_tx::FRAME_MAX],
) {
    let Some(len) = openlink::build::fake_frame(system, seq, frame) else {
        control::open_failed();
        return;
    };
    // use_internal_seq_num = false: the builder wrote the sequence field,
    // and for DroneBridge those two bytes are payload, not a header.
    match sniffer.send_raw_frame(USE_STA_INTERFACE, &frame[..len], false) {
        Ok(()) => control::open_sent(),
        Err(e) => {
            control::open_failed();
            let n = control::open_failed_total();
            if n == 1 || n.is_multiple_of(100) {
                error!("open-link raw TX failed ({n} so far): {e:?}");
            }
        }
    }
}

/// Build and transmit one beacon or NAN frame.
#[allow(clippy::large_stack_frames, reason = "~1.5 KB of frame and message buffers; the executor stack has room and keeping them local avoids shared statics")]
fn wifi_send(
    sniffer: &mut Sniffer,
    kind: Tx,
    channel: u8,
    centre: &Centre,
    counters: &mut [u8; 2],
    frame: &mut [u8; rid_tx::FRAME_MAX],
) {
    let built = messages_now(centre).and_then(|m| rid_tx::pack(&m));
    let pack = match built {
        Ok(p) => p,
        Err(e) => {
            error!("encode: {e:?}");
            control::failed(kind);
            return;
        }
    };
    let idx = if kind == Tx::WifiBeacon { 0 } else { 1 };
    let counter = counters[idx];
    if kind == Tx::WifiNan && SEND_NAN_SYNC_BEACON {
        // Not counted as a Remote ID transmission: it carries no messages.
        if let Ok(len) = rid_tx::nan_sync(WIFI_SRC, frame) {
            let _ = sniffer.send_raw_frame(USE_STA_INTERFACE, &frame[..len], true);
        }
    }
    let len = match kind {
        Tx::WifiBeacon => rid_tx::beacon(WIFI_SRC, channel, counter, &pack, frame),
        _ => rid_tx::nan(WIFI_SRC, counter, &pack, frame),
    };
    let Ok(len) = len else {
        control::failed(kind);
        return;
    };
    // use_internal_seq_num = true: the driver fills the sequence number.
    match sniffer.send_raw_frame(USE_STA_INTERFACE, &frame[..len], true) {
        Ok(()) => {
            control::sent(kind);
            counters[idx] = counter.wrapping_add(1);
        }
        Err(e) => {
            control::failed(kind);
            let n = control::failed_total(kind);
            // Log the first failure and then every 60th, not every second.
            if n == 1 || n.is_multiple_of(60) {
                error!("{} raw TX failed ({n} so far): {e:?}", kind.key());
            }
        }
    }
}

/// Beacons per second: build-time `RID_BEACON_HZ`, one of 1, 2, 5, 10
/// (divisors of the 10 slots per second). Default 1, F3411's minimum.
/// Higher rates help scanners with short channel dwell (laptops, some
/// phones) notice the beacon sooner; the power stays at the bench minimum.
fn beacon_hz() -> u32 {
    match option_env!("RID_BEACON_HZ").map(|s| s.trim().parse::<u32>()) {
        Some(Ok(h @ (1 | 2 | 5 | 10))) => h,
        Some(_) => {
            warn!("RID_BEACON_HZ must be 1, 2, 5 or 10; using 1");
            1
        }
        None => 1,
    }
}

// ---------------------------------------------------------------------------
// BLE: three advertising sets driven by one controller.

type BleCtl = ExternalController<FixedTransport, 20>;

fn adv_params(kind: Tx) -> AdvertisementParameters {
    let (phy, interval) = match kind {
        Tx::BleLegacy => (PhyKind::Le1M, BLE_LEGACY_INTERVAL),
        Tx::BleExt => (PhyKind::Le1M, BLE_EXT_INTERVAL),
        // Long Range: coded PHY on both the primary (ADV_EXT_IND) and the
        // secondary (AUX_ADV_IND) channels, which is what F3411 means by
        // "Bluetooth 5 Long Range" and what a coded-PHY scanner listens for.
        _ => (PhyKind::LeCoded, BLE_CODED_INTERVAL),
    };
    AdvertisementParameters {
        primary_phy: phy,
        secondary_phy: phy,
        tx_power: BLE_ADV_TX_POWER,
        interval_min: interval,
        interval_max: interval,
        ..Default::default()
    }
}

fn adv_set(kind: Tx, data: &[u8]) -> AdvertisementSet<'_> {
    let data = match kind {
        // Non-connectable, non-scannable: Remote ID is broadcast only.
        Tx::BleLegacy => Advertisement::NonconnectableNonscannableUndirected { adv_data: data },
        _ => Advertisement::ExtNonconnectableNonscannableUndirected {
            anonymous: false,
            adv_data: data,
        },
    };
    AdvertisementSet {
        params: adv_params(kind),
        data,
        address: None,
    }
}

/// The payload buffers for all three sets, rebuilt in place.
struct BlePayloads {
    legacy: [u8; 31],
    ext: [u8; rid_tx::BLE_EXT_LEN],
    coded: [u8; rid_tx::BLE_EXT_LEN],
    /// F3411 counters per transport (legacy, ext, coded).
    counters: [u8; 3],
    /// Which message type the legacy advert carries next.
    rotate: usize,
}

impl BlePayloads {
    fn data(&self, kind: Tx) -> &[u8] {
        match kind {
            Tx::BleLegacy => &self.legacy,
            Tx::BleExt => &self.ext,
            _ => &self.coded,
        }
    }

    /// Next legacy payload: the next message type in rotation.
    fn next_legacy(&mut self, centre: &Centre) -> bool {
        let Ok(msgs) = messages_now(centre) else {
            return false;
        };
        let msg = msgs[self.rotate];
        self.rotate = (self.rotate + 1) % rid_tx::N_MESSAGES;
        match rid_tx::ble_legacy(self.counters[0], &msg) {
            Ok(d) => {
                self.legacy = d;
                self.counters[0] = self.counters[0].wrapping_add(1);
                true
            }
            Err(_) => false,
        }
    }

    /// Next pack for the extended and coded sets (each has its own counter).
    #[allow(clippy::large_stack_frames, reason = "~1.5 KB of frame and message buffers; the executor stack has room and keeping them local avoids shared statics")]
    fn next_packs(&mut self, centre: &Centre) -> bool {
        let Ok(pack) = messages_now(centre).and_then(|m| rid_tx::pack(&m)) else {
            return false;
        };
        match (
            rid_tx::ble_ext(self.counters[1], &pack),
            rid_tx::ble_ext(self.counters[2], &pack),
        ) {
            (Ok(e), Ok(c)) => {
                self.ext = e;
                self.coded = c;
                self.counters[1] = self.counters[1].wrapping_add(1);
                self.counters[2] = self.counters[2].wrapping_add(1);
                true
            }
            _ => false,
        }
    }
}

/// Linux/BlueZ only lists devices whose advert says "discoverable" (the
/// Flags AD), and Remote ID adverts carry no Flags (a BT4 one has no room).
/// With `RID_BLE_DISCOVERABLE=1` at build time the beacon adds a fourth,
/// plain legacy advert from the same address with Flags and the name
/// "RID-TEST". BlueZ then creates the device, and the Remote ID adverts from
/// that address show up as its ServiceData (`bluetoothctl info`). Off by
/// default: it is a verification aid, not part of F3411.
fn ble_helper_enabled() -> bool {
    matches!(option_env!("RID_BLE_DISCOVERABLE"), Some("1"))
}

/// Flags (LE General Discoverable, BR/EDR not supported) + complete name.
static HELPER_ADV: [u8; 13] = [
    2, 0x01, 0x06, 9, 0x09, b'R', b'I', b'D', b'-', b'T', b'E', b'S', b'T',
];

fn helper_set() -> AdvertisementSet<'static> {
    AdvertisementSet {
        params: AdvertisementParameters {
            tx_power: BLE_ADV_TX_POWER,
            interval_min: Duration::from_millis(500),
            interval_max: Duration::from_millis(500),
            ..Default::default()
        },
        data: Advertisement::NonconnectableNonscannableUndirected {
            adv_data: &HELPER_ADV,
        },
        address: None,
    }
}

/// Delete every advertising set from the controller before configuring new ones.
///
/// trouble-host's `advertise_ext` reuses handles 0.. and stopping (dropping the
/// `Advertiser`) only disables the sets: their old parameters and data stay. The
/// ESP32-C5 controller then rejects turning a set that still holds 134 bytes of
/// extended data into a legacy (31-byte) advert with "Invalid HCI Command
/// Parameters", as the Core spec requires (Vol 4 Part E 7.8.53). Seen on the
/// bench 2026-11-12: every C5 BLE test failed after the start-up probe left the
/// coded set's data on handle 0. Clearing first makes each start a clean slate.
async fn clear_adv_sets(stack: &Stack<'_, BleCtl, DefaultPacketPool>) {
    if let Err(e) = stack.command(bt_hci::cmd::le::LeClearAdvSets::new()).await {
        warn!("LE Clear Advertising Sets failed: {e:?}");
    }
}

const NO_SET: AdvSet = AdvSet {
    adv_handle: AdvHandle::new(0),
    duration: bt_hci::param::Duration::from_u16(0),
    max_ext_adv_events: 0,
};

#[allow(clippy::large_stack_frames, reason = "the future lives inside the main task's static storage, not on a stack")]
async fn ble_advertise(
    stack: &Stack<'_, BleCtl, DefaultPacketPool>,
    peripheral: &mut Peripheral<'_, BleCtl, DefaultPacketPool>,
    centre: Centre,
) {
    let mut p = BlePayloads {
        legacy: [0; 31],
        ext: [0; rid_tx::BLE_EXT_LEN],
        coded: [0; rid_tx::BLE_EXT_LEN],
        counters: [0; 3],
        rotate: 0,
    };
    let ble_kinds = [Tx::BleLegacy, Tx::BleExt, Tx::BleCoded];

    // Probe each advertising set alone once, so a transport this controller
    // cannot do (e.g. coded PHY) is reported clearly and does not take the
    // other two down with it: advertise_ext is all-or-nothing.
    if !(p.next_legacy(&centre) && p.next_packs(&centre)) {
        error!("BLE encode failed");
    }
    for k in ble_kinds {
        let mut h = [NO_SET; 1];
        clear_adv_sets(stack).await;
        let r = peripheral
            .advertise_ext(&[adv_set(k, p.data(k))], &mut h)
            .await;
        match r {
            Ok(a) => {
                drop(a);
                println!(r#"{{"t":"ble_probe","tx":"{}","ok":true}}"#, k.key());
            }
            Err(e) => {
                control::mark_unsupported(k);
                println!(
                    r#"{{"t":"ble_probe","tx":"{}","ok":false,"err":"{:?}"}}"#,
                    k.key(),
                    e
                );
            }
        }
        // Let the host process the cancel before the next advertise.
        Timer::after(Duration::from_millis(100)).await;
    }

    loop {
        // (Re)start advertising with whichever BLE transports are enabled.
        // Toggling one from the console bumps the epoch and lands back here.
        let epoch = control::ble_epoch();
        let mut kinds = [Tx::BleLegacy; 3];
        let mut n = 0;
        for k in ble_kinds {
            if control::enabled(k) && control::supported(k) {
                kinds[n] = k;
                n += 1;
            }
        }
        if n == 0 {
            while control::ble_epoch() == epoch {
                Timer::after(Duration::from_millis(100)).await;
            }
            continue;
        }

        let ok = p.next_legacy(&centre) && p.next_packs(&centre);
        if !ok {
            error!("BLE encode failed");
            Timer::after(Duration::from_secs(1)).await;
            continue;
        }
        let mut handles = [NO_SET; 4];
        let advertiser = {
            let sets = [
                adv_set(kinds[0], p.data(kinds[0])),
                adv_set(kinds[1], p.data(kinds[1])),
                adv_set(kinds[2], p.data(kinds[2])),
                helper_set(),
            ];
            // The helper goes right after the n Remote ID sets so indices
            // 0..n still line up with `kinds` for the payload updates below.
            let mut sets = sets;
            sets.swap(n, 3);
            let m = if ble_helper_enabled() { n + 1 } else { n };
            clear_adv_sets(stack).await;
            peripheral
                .advertise_ext(&sets[..m], &mut handles[..m])
                .await
        };
        let _advertiser = match advertiser {
            Ok(a) => a,
            Err(e) => {
                error!("BLE advertise_ext({} sets) failed: {e:?}", n);
                for &k in &kinds[..n] {
                    control::failed(k);
                }
                Timer::after(Duration::from_secs(2)).await;
                continue;
            }
        };
        for &k in &kinds[..n] {
            control::sent(k);
        }
        info!("BLE advertising {} set(s)", n);

        // While the set of transports is unchanged, swap payloads in place.
        // Dropping `_advertiser` (on leaving this block) stops advertising.
        let mut ticker = Ticker::every(BLE_LEGACY_ROTATE);
        let mut tick = 0u32;
        while control::ble_epoch() == epoch {
            ticker.next().await;
            tick += 1;
            let packs_due = tick.is_multiple_of(5); // every 1 s
            if packs_due && !p.next_packs(&centre) {
                error!("BLE pack encode failed");
            }
            if !p.next_legacy(&centre) {
                error!("BLE legacy encode failed");
            }
            for (i, &k) in kinds[..n].iter().enumerate() {
                if k != Tx::BleLegacy && !packs_due {
                    continue;
                }
                let set = [adv_set(k, p.data(k))];
                match peripheral
                    .update_adv_data_ext(&set, &mut handles[i..i + 1])
                    .await
                {
                    Ok(()) => control::sent(k),
                    Err(e) => {
                        control::failed(k);
                        let f = control::failed_total(k);
                        if f == 1 || f.is_multiple_of(50) {
                            error!("BLE {} data update failed ({f} so far): {e:?}", k.key());
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// USB console: single-letter commands in, one JSON line per second out.

#[embassy_executor::task]
async fn console_task(mut rx: UsbSerialJtagRx<'static, Blocking>) {
    // Polled rather than interrupt-driven: esp-println owns the TX side of the
    // same peripheral, and 50 ms is instant for a human typing a letter.
    let mut buf = [0u8; 32];
    loop {
        Timer::after(Duration::from_millis(50)).await;
        let n = rx.drain_rx_fifo(&mut buf);
        for &b in &buf[..n] {
            #[cfg(feature = "scripted")]
            if let Some(p) = script::command(b, Instant::now().as_millis() as u32) {
                println!(r#"{{"t":"cmd","path":"{}"}}"#, p.name());
                continue;
            }
            match control::command(b) {
                Reply::Ignored => {}
                Reply::Toggled(t, on) => {
                    println!(
                        r#"{{"t":"cmd","tx":"{}","on":{},"flags":"{}"}}"#,
                        t.key(),
                        on,
                        flags_str(&control::flags())
                    )
                }
                r @ (Reply::AllOn | Reply::AllOff) => {
                    println!(
                        r#"{{"t":"cmd","all":{},"flags":"{}"}}"#,
                        r == Reply::AllOn,
                        flags_str(&control::flags())
                    )
                }
                Reply::Band5g(five) => println!(
                    r#"{{"t":"cmd","band":"{}"}}"#,
                    if five { "5" } else { "2.4" }
                ),
                Reply::Open(sys) => println!(
                    r#"{{"t":"cmd","open":"{}","ch":{}}}"#,
                    sys.unwrap_or("off"),
                    control::channel()
                ),
                Reply::Channel(ch) => println!(r#"{{"t":"cmd","ch":{ch}}}"#),
                Reply::Help => println!(
                    r#"{{"t":"help","w":"wifi beacon","n":"wifi nan","l":"ble4 legacy","e":"ble5 extended 1M","c":"ble5 long range (coded)","a":"all remote id on","x":"all off (remote id and fake fpv)","b":"c5 only: toggle 5 GHz ch149 / 2.4 GHz ch6","f":"fake wfb-ng/openipc","o":"fake openhd","r":"fake rubyfpv","d":"fake dronebridge","6":"wifi ch 6","7":"c5 only: wifi ch 149","8":"c5 only: wifi ch 161","9":"c5 only: wifi ch 165","flags":"{}"}}"#,
                    flags_str(&control::flags())
                ),
            }
        }
    }
}

fn flags_str(f: &[u8; 5]) -> &str {
    core::str::from_utf8(f).unwrap_or("?????")
}

#[embassy_executor::task]
async fn summary_task(centre: Centre) {
    let mut last = [0u32; 5];
    let mut last_fail = 0u32;
    let mut last_open = 0u32;
    let mut ticker = Ticker::every(Duration::from_secs(1));
    loop {
        ticker.next().await;
        let mut now = [0u32; 5];
        let mut fail = 0u32;
        for (i, &t) in control::ALL.iter().enumerate() {
            now[i] = control::sent_total(t);
            fail += control::failed_total(t);
        }
        fail += control::open_failed_total();
        let open_now = control::open_sent_total();
        let d = |i: usize| now[i].wrapping_sub(last[i]);
        let t = uptime_s();
        let fix = flight::at(&centre, t);
        #[cfg(feature = "scripted")]
        let fix = match scripted_now(&centre) {
            Some(s) => {
                let (p, secs) = script::current(Instant::now().as_millis() as u32);
                println!(
                    r#"{{"t":"path","path":"{}","s":{:.0},"lat":{:.7},"lon":{:.7},"h":{:.1},"v":{:.1},"hdg":{:.0},"vs":{:.1}}}"#,
                    p.name(),
                    secs,
                    s.lat,
                    s.lon,
                    s.height_m,
                    s.speed_mps,
                    s.heading_deg,
                    s.vspeed_mps
                );
                flight::Fix { lat: s.lat, lon: s.lon, heading_deg: s.heading_deg, speed_mps: s.speed_mps }
            }
            None => fix,
        };
        let five = control::band_5g();
        println!(
            r#"{{"t":"tx","up":{},"band":"{}","ch":{},"beacon":{},"nan":{},"ble4":{},"ble5":{},"ble_lr":{},"open_sys":"{}","open":{},"fail":{},"on":"{}","lat":{:.7},"lon":{:.7},"hdg":{:.0}}}"#,
            t as u32,
            if five { "5" } else { "2.4" },
            control::channel(),
            d(0),
            d(1),
            d(2),
            d(3),
            d(4),
            control::open().map_or("off", |(_, n)| n),
            open_now.wrapping_sub(last_open),
            fail.wrapping_sub(last_fail),
            flags_str(&control::flags()),
            fix.lat,
            fix.lon,
            fix.heading_deg,
        );
        last = now;
        last_fail = fail;
        last_open = open_now;
    }
}
