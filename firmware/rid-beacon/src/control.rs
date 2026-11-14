//! Shared state between the radio tasks and the USB console: which transports
//! are on, how many transmissions each made, and the one-letter commands.
//!
//! Plain atomics, because the radio tasks only ever read a flag and bump a
//! counter; nothing here needs a lock.

use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering::Relaxed};

use openlink::System;

/// The five transports, in summary/command order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tx {
    /// 802.11 beacon with the ASTM vendor IE.
    WifiBeacon = 0,
    /// 802.11 NAN service discovery action frame.
    WifiNan = 1,
    /// BLE 4 legacy advertising, one message per advert, rotating.
    BleLegacy = 2,
    /// BLE 5 extended advertising on the 1M PHY, Message Pack.
    BleExt = 3,
    /// BLE 5 Long Range: extended advertising on the coded PHY, Message Pack.
    BleCoded = 4,
}

pub const ALL: [Tx; 5] = [
    Tx::WifiBeacon,
    Tx::WifiNan,
    Tx::BleLegacy,
    Tx::BleExt,
    Tx::BleCoded,
];

impl Tx {
    /// Key in the JSON summary.
    pub const fn key(self) -> &'static str {
        match self {
            Tx::WifiBeacon => "beacon",
            Tx::WifiNan => "nan",
            Tx::BleLegacy => "ble4",
            Tx::BleExt => "ble5",
            Tx::BleCoded => "ble_lr",
        }
    }

    /// The console letter that toggles it.
    pub const fn letter(self) -> u8 {
        match self {
            Tx::WifiBeacon => b'w',
            Tx::WifiNan => b'n',
            Tx::BleLegacy => b'l',
            Tx::BleExt => b'e',
            Tx::BleCoded => b'c',
        }
    }
}

// Compile-time defaults: all five Remote ID transports on. (The fake open
// FPV links have no default: they are always off until typed.) Flip one to `false` to build a beacon
// that starts with that transport silenced (it can still be toggled live).
pub const DEFAULT_WIFI_BEACON: bool = true;
pub const DEFAULT_WIFI_NAN: bool = true;
pub const DEFAULT_BLE_LEGACY: bool = true;
pub const DEFAULT_BLE_EXT: bool = true;
pub const DEFAULT_BLE_CODED: bool = true;

static ENABLED: [AtomicBool; 5] = [
    AtomicBool::new(DEFAULT_WIFI_BEACON),
    AtomicBool::new(DEFAULT_WIFI_NAN),
    AtomicBool::new(DEFAULT_BLE_LEGACY),
    AtomicBool::new(DEFAULT_BLE_EXT),
    AtomicBool::new(DEFAULT_BLE_CODED),
];
/// Successful transmissions since boot (Wi-Fi: frames handed to the radio;
/// BLE: distinct payloads, i.e. counter values, handed to the controller).
static SENT: [AtomicU32; 5] = [const { AtomicU32::new(0) }; 5];
/// Failed transmissions since boot.
static FAILED: [AtomicU32; 5] = [const { AtomicU32::new(0) }; 5];
/// Set at boot for a BLE transport the controller refused (see the
/// `ble_probe` lines); such a transport is never attempted again.
static UNSUPPORTED: [AtomicBool; 5] = [const { AtomicBool::new(false) }; 5];
/// Bumped whenever the BLE set of enabled transports changes, so the BLE task
/// knows to restart advertising with the new set.
static BLE_EPOCH: AtomicU32 = AtomicU32::new(0);
/// The Wi-Fi channel every Wi-Fi transmission uses (one radio, one
/// channel). C5: 149 at boot; S3: always 6.
static WIFI_CH: AtomicU8 = AtomicU8::new(if cfg!(feature = "esp32c5") { 149 } else { 6 });

/// Which fake open-FPV link is on: 0 = none, else 1 + index in
/// [`OPEN_SYSTEMS`]. Off at boot, always: only a typed key turns it on.
static OPEN: AtomicU8 = AtomicU8::new(0);
/// Fake open-link frames handed to the radio / refused by it.
static OPEN_SENT: AtomicU32 = AtomicU32::new(0);
static OPEN_FAILED: AtomicU32 = AtomicU32::new(0);

/// The fake open-FPV systems and their console keys. Never DJI: DroneID of
/// any kind is out of scope for this transmitter (README, "Conduct").
pub const OPEN_SYSTEMS: [(u8, System, &str); 4] = [
    (b'f', System::WfbNg, "wfb-ng"),
    (b'o', System::OpenHd, "openhd"),
    (b'r', System::RubyFpv, "rubyfpv"),
    (b'd', System::DroneBridge, "dronebridge"),
];

/// Channel keys: digit -> channel. 5 GHz ones only on the C5.
pub const CHANNEL_KEYS: [(u8, u8); 4] = [(b'6', 6), (b'7', 149), (b'8', 161), (b'9', 165)];

pub fn enabled(t: Tx) -> bool {
    ENABLED[t as usize].load(Relaxed)
}

pub fn set(t: Tx, on: bool) {
    let was = ENABLED[t as usize].swap(on, Relaxed);
    if was != on && matches!(t, Tx::BleLegacy | Tx::BleExt | Tx::BleCoded) {
        BLE_EPOCH.fetch_add(1, Relaxed);
    }
}

pub fn mark_unsupported(t: Tx) {
    UNSUPPORTED[t as usize].store(true, Relaxed);
}

pub fn supported(t: Tx) -> bool {
    !UNSUPPORTED[t as usize].load(Relaxed)
}

pub fn sent(t: Tx) {
    SENT[t as usize].fetch_add(1, Relaxed);
}

pub fn failed(t: Tx) {
    FAILED[t as usize].fetch_add(1, Relaxed);
}

pub fn sent_total(t: Tx) -> u32 {
    SENT[t as usize].load(Relaxed)
}

pub fn failed_total(t: Tx) -> u32 {
    FAILED[t as usize].load(Relaxed)
}

pub fn ble_epoch() -> u32 {
    BLE_EPOCH.load(Relaxed)
}

pub fn band_5g() -> bool {
    channel() > 14
}

pub fn channel() -> u8 {
    WIFI_CH.load(Relaxed)
}

/// The fake open link that is on, if any: (system, short name).
pub fn open() -> Option<(System, &'static str)> {
    let i = OPEN.load(Relaxed);
    OPEN_SYSTEMS
        .get(usize::from(i).checked_sub(1)?)
        .map(|&(_, s, n)| (s, n))
}

pub fn open_sent() {
    OPEN_SENT.fetch_add(1, Relaxed);
}

pub fn open_failed() {
    OPEN_FAILED.fetch_add(1, Relaxed);
}

pub fn open_sent_total() -> u32 {
    OPEN_SENT.load(Relaxed)
}

pub fn open_failed_total() -> u32 {
    OPEN_FAILED.load(Relaxed)
}

/// What a console byte did, for the reply line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    Toggled(Tx, bool),
    AllOn,
    AllOff,
    Band5g(bool),
    /// A fake open link was switched on (`Some`) or off (`None`).
    Open(Option<&'static str>),
    Channel(u8),
    Help,
    /// Not a command (newline, unknown letter...). No reply.
    Ignored,
}

/// Apply one console byte. Letters are case-insensitive.
pub fn command(byte: u8) -> Reply {
    let b = byte.to_ascii_lowercase();
    if let Some(&t) = ALL.iter().find(|t| t.letter() == b) {
        let on = !enabled(t);
        set(t, on);
        return Reply::Toggled(t, on);
    }
    // One fake open link at a time: its key turns it on (replacing any
    // other) or, typed again, off.
    if let Some(i) = OPEN_SYSTEMS.iter().position(|&(k, _, _)| k == b) {
        let want = i as u8 + 1;
        if OPEN.swap(want, Relaxed) == want {
            OPEN.store(0, Relaxed);
            return Reply::Open(None);
        }
        return Reply::Open(Some(OPEN_SYSTEMS[i].2));
    }
    if let Some(&(_, ch)) = CHANNEL_KEYS
        .iter()
        .find(|&&(k, ch)| k == b && (ch <= 14 || cfg!(feature = "esp32c5")))
    {
        WIFI_CH.store(ch, Relaxed);
        return Reply::Channel(ch);
    }
    match b {
        b'a' => {
            ALL.iter().for_each(|&t| set(t, true));
            Reply::AllOn
        }
        // 'x' silences everything without a reset: the quick "stop
        // transmitting" for the bench.
        b'x' => {
            ALL.iter().for_each(|&t| set(t, false));
            OPEN.store(0, Relaxed);
            Reply::AllOff
        }
        b'b' if cfg!(feature = "esp32c5") => {
            let five = !band_5g();
            WIFI_CH.store(if five { 149 } else { 6 }, Relaxed);
            Reply::Band5g(five)
        }
        b'?' | b'h' => Reply::Help,
        _ => Reply::Ignored,
    }
}

/// The five on/off flags as letters, upper case when on, '-' when the
/// hardware refused it: e.g. "WNLe-".
pub fn flags() -> [u8; 5] {
    let mut out = [0u8; 5];
    for (i, &t) in ALL.iter().enumerate() {
        let l = t.letter();
        out[i] = if !supported(t) {
            b'-'
        } else if enabled(t) {
            l.to_ascii_uppercase()
        } else {
            l
        };
    }
    out
}
