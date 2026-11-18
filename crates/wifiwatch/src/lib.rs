//! Hostile Wi-Fi and Bluetooth activity near a node, from raw frames.
//!
//! A Rotorscope radio node is receive-only. Besides Remote ID it hears every
//! management frame on its channel, which is enough to notice somebody
//! attacking the networks around it (or a drone carrying a Wi-Fi Pineapple,
//! an ESP32 Marauder, a Pwnagotchi or a deauther board). This crate turns
//! those frames into [`link::ThreatSighting`]s for the hub.
//!
//! Two halves, so a node can split them between its receive callback and a
//! task:
//!
//! - [`floods::Floods`] counts deauthentication, disassociation,
//!   authentication / association and probe-request floods from the **frame
//!   header alone** (frame control, three addresses, a reason code). It never
//!   needs the frame body, so a node can feed it straight from the radio's
//!   receive callback and never queue those frames: a flood of thousands of
//!   deauths a second costs one short call each and cannot crowd Remote ID
//!   out of the receive queue. It only counts; [`floods::Floods::poll`]
//!   (a few times a second, from a task) decides and reports.
//! - [`beacons::Beacons`] looks at beacons, probe responses and action
//!   frames: beacon floods, Karma / Pineapple, evil twins, channel-switch
//!   abuse and known tools' signatures ([`tools::TOOLS`]).
//!
//! - [`drones::DroneLinks`] follows the drone networks the node has
//!   recognised (`crates/wifidrone`) and reports someone kicking the pilot
//!   off one or joining it in the pilot's place, also from headers alone
//!   (and data-frame addresses); [`drones::Clones`], inside
//!   [`beacons::Beacons`], reports copies of a drone's network.
//!
//! - [`mavwifi`] runs `crates/mavwatch` (hostile MAVLink telemetry) on
//!   UDP / TCP MAVLink in unprotected data frames, only while the hub's
//!   `control content` opt-in is on: a header-only filter for the receive
//!   callback, a rate limit, and the analysis for a task.
//!
//! [`ble::BleWatch`] does the same for Bluetooth pairing pop-up spam.
//! [`Watch`] bundles the Wi-Fi halves for callers (and tests) that have
//! every frame in one place.
//!
//! Each kind reports at most about once a second per target while it lasts
//! (`count` and `window_ms` say how much was seen). Every table is fixed
//! size; all of it together is about 6 KB ([`Watch`]; [`drones::DroneLinks`]
//! ~440 bytes of it), plus ~360 bytes for [`ble::BleWatch`].
//!
//! # What the numbers mean (beyond `link::ThreatKind`'s doc)
//!
//! - `Deauth` / `Disassoc`: `detail & 0xFFFF` = most common reason code;
//!   bit 16 ([`floods::DETAIL_BROADCAST`]) set if any frame went to the
//!   broadcast address; bit 17 ([`floods::DETAIL_PROTECTED`]) set if every
//!   frame was protected (802.11w; the reason code is then unreadable and
//!   reads 0). `source` = claimed transmitter, `target` = broadcast,
//!   the one station hit, or the BSSID when many stations were hit.
//! - `AuthFlood`: `detail` = distinct source addresses (lower bound),
//!   `target` = the access point.
//! - `ProbeFlood`: `detail` = distinct source addresses (lower bound).
//! - `BeaconFlood`: `count` = new transmitters in the window, `detail` =
//!   distinct names among them.
//! - `Karma`: `source` = the transmitter, `detail` = distinct names it used
//!   within [`beacons::KARMA_WINDOW_MS`], `ssid` = the latest.
//! - `EvilTwin`: `detail` = `(original security << 8) | twin security`
//!   ([`mac::Security`] numbers; the twin is always `Open` = 0).
//! - `ChannelSwitch`: `detail` = announced channel, `source` = the BSSID it
//!   claims to come from.
//! - `AttackTool`: `detail` = index into [`tools::TOOLS`].
//! - `BleSpam`: `detail` = new addresses in the window, `ssid` = the
//!   pairing family with the most ([`ble::Family::name`]).
//! - `DroneDeauth`, `DroneTakeover`, `DroneNetClone`: see [`drones`].
#![cfg_attr(not(test), no_std)]

pub mod beacons;
pub mod ble;
pub mod contentlimit;
pub mod drones;
pub mod floods;
pub mod mac;
pub mod mavwifi;
pub mod shed;
pub mod tools;
pub mod util;

#[cfg(test)]
mod tests;

pub use link::{ThreatKind, ThreatSighting};
pub use tools::TOOLS;

/// One Wi-Fi channel's width, kHz (`ThreatSighting::width_khz`).
pub const CHANNEL_KHZ: u32 = 20_000;

/// Common fields of a sighting heard on a Wi-Fi channel.
#[allow(clippy::too_many_arguments, reason = "plain field list, named at every call site")]
pub(crate) fn sighting(
    kind: ThreatKind,
    channel: u8,
    level: i8,
    count: u32,
    window_ms: u32,
    source: [u8; 6],
    target: [u8; 6],
    detail: u32,
    ssid: &[u8],
    now_us: u64,
) -> ThreatSighting {
    let mhz = util::channel_mhz(channel);
    ThreatSighting {
        kind,
        channel,
        freq: mhz * 10,
        width_khz: if mhz == 0 { 0 } else { CHANNEL_KHZ },
        level,
        count,
        window_ms,
        source,
        target,
        detail,
        ssid: util::ssid_string(ssid),
        uptime_us: now_us,
    }
}

/// Every Wi-Fi part together. A node that splits them (callback / task)
/// uses [`floods::Floods`], [`drones::DroneLinks`] and [`beacons::Beacons`]
/// directly instead.
pub struct Watch {
    pub floods: floods::Floods,
    pub drones: drones::DroneLinks,
    pub beacons: beacons::Beacons,
}

impl Default for Watch {
    fn default() -> Self {
        Self::new()
    }
}

impl Watch {
    pub const fn new() -> Self {
        Self { floods: floods::Floods::new(), drones: drones::DroneLinks::new(), beacons: beacons::Beacons::new() }
    }

    /// One management frame: MPDU without FCS, RSSI (dBm), the channel it
    /// was heard on, node time (µs). Sightings that are due go to `emit`.
    pub fn frame(&mut self, mpdu: &[u8], rssi: i8, channel: u8, now_us: u64, emit: &mut impl FnMut(ThreatSighting)) {
        self.drones.frame(mpdu, rssi, channel, now_us);
        if self.floods.frame(mpdu, rssi, channel, now_us) {
            return;
        }
        if let Some(d) = self.beacons.frame(mpdu, rssi, channel, now_us, emit) {
            self.drones.learn(&d, now_us);
        }
    }

    /// One data frame (MPDU, FCS or not): only its addresses are read, to
    /// see who is attached to a drone's network.
    pub fn data(&mut self, mpdu: &[u8], rssi: i8, channel: u8, now_us: u64) {
        self.drones.data(mpdu, rssi, channel, now_us);
    }

    /// Close windows that are due and report what they held. Call a few
    /// times a second (every frame is fine too).
    pub fn poll(&mut self, now_us: u64, emit: &mut impl FnMut(ThreatSighting)) {
        self.floods.poll(now_us, emit);
        self.drones.poll(now_us, emit);
        self.beacons.poll(now_us, emit);
    }
}

#[cfg(test)]
mod size {
    #[test]
    fn memory_budget() {
        let w = core::mem::size_of::<super::Watch>();
        let f = core::mem::size_of::<super::floods::Floods>();
        let b = core::mem::size_of::<super::ble::BleWatch>();
        let d = core::mem::size_of::<super::drones::DroneLinks>();
        std::println!("Watch {w} B (Floods {f} B, DroneLinks {d} B), BleWatch {b} B");
        assert!(d <= 512, "DroneLinks is {d} bytes");
        assert!(w <= 6 * 1024, "Watch is {w} bytes");
        assert!(f <= 1536, "Floods is {f} bytes");
        assert!(b <= 512, "BleWatch is {b} bytes");
    }
}
