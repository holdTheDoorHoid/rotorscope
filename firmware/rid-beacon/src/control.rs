//! Shared state between the radio tasks and the USB console: which transports
//! are on, how many transmissions each made, and the one-letter commands.
//!
//! Plain atomics, because the radio tasks only ever read a flag and bump a
//! counter; nothing here needs a lock.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};

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

// Compile-time defaults: all five on. Flip one to `false` to build a beacon
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
/// C5 only: true = 5 GHz (channel 149), false = 2.4 GHz (channel 6).
static BAND_5G: AtomicBool = AtomicBool::new(cfg!(feature = "esp32c5"));

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
    BAND_5G.load(Relaxed)
}

/// What a console byte did, for the reply line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    Toggled(Tx, bool),
    AllOn,
    AllOff,
    Band5g(bool),
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
    match b {
        b'a' => {
            ALL.iter().for_each(|&t| set(t, true));
            Reply::AllOn
        }
        // 'x' silences everything without a reset: the quick "stop
        // transmitting" for the bench.
        b'x' => {
            ALL.iter().for_each(|&t| set(t, false));
            Reply::AllOff
        }
        b'b' if cfg!(feature = "esp32c5") => {
            let on = !BAND_5G.load(Relaxed);
            BAND_5G.store(on, Relaxed);
            Reply::Band5g(on)
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
