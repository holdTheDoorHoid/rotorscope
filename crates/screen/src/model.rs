//! The view-model: everything the screen shows, as plain data.
//!
//! The hub fills a [`UiState`] once per frame from the tracker, the GPS and
//! the node links (see [`crate::from_tracker`] for the tracker part). The
//! renderer reads nothing else, which keeps it testable on a laptop and keeps
//! this crate independent of how the tracker stores things.

use heapless::{String, Vec};

/// Drones the screen is given per frame. PLAN.md D7 asks the hub to track at
/// least 32; when it holds more, the hub passes the 32 most relevant (nearest,
/// then newest) and sets [`UiState::total_drones`] so the screen can say so.
pub const MAX_DRONES: usize = 32;
/// Radio nodes shown in the status bar and on the health screen: Wi-Fi
/// 2.4 GHz, Wi-Fi 5 GHz and BLE today, FPV and ADS-B later.
pub const MAX_NODES: usize = 5;

/// One of the five ways a drone can be heard. The letter is what the list
/// and detail screens print, so it never depends on colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Radio {
    /// Wi-Fi beacon, 2.4 GHz.
    WifiBeacon,
    /// Wi-Fi NAN (Neighbour Awareness Networking), 2.4 GHz.
    WifiNan,
    /// Bluetooth legacy or extended advertising on the 1M PHY.
    Ble,
    /// Bluetooth 5 Long Range (coded PHY).
    BleLongRange,
    /// Wi-Fi beacon or NAN on 5 GHz.
    Wifi5,
}

impl Radio {
    /// Display order of the letters: W N B L 5.
    pub const ALL: [Radio; 5] = [
        Radio::WifiBeacon,
        Radio::WifiNan,
        Radio::Ble,
        Radio::BleLongRange,
        Radio::Wifi5,
    ];

    pub const fn letter(self) -> char {
        match self {
            Radio::WifiBeacon => 'W',
            Radio::WifiNan => 'N',
            Radio::Ble => 'B',
            Radio::BleLongRange => 'L',
            Radio::Wifi5 => '5',
        }
    }

    pub const fn index(self) -> usize {
        match self {
            Radio::WifiBeacon => 0,
            Radio::WifiNan => 1,
            Radio::Ble => 2,
            Radio::BleLongRange => 3,
            Radio::Wifi5 => 4,
        }
    }
}

/// A set of [`Radio`]s, one bit each by [`Radio::index`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Radios(pub u8);

impl Radios {
    pub fn with(mut self, r: Radio) -> Self {
        self.insert(r);
        self
    }
    pub fn insert(&mut self, r: Radio) {
        self.0 |= 1 << r.index();
    }
    pub fn contains(self, r: Radio) -> bool {
        self.0 & (1 << r.index()) != 0
    }
}

/// What kind of identifier [`DroneView::id`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IdKind {
    /// Manufacturer serial number (ANSI/CTA-2063-A). Stable across flights.
    Serial,
    /// Civil aviation authority registration.
    Registration,
    /// UTM-assigned UUID.
    Utm,
    /// Session ID; changes between flights.
    Session,
    /// No Basic ID heard yet: the text is the transmitter address.
    Mac,
    #[default]
    Unknown,
}

impl IdKind {
    pub const fn label(self) -> &'static str {
        match self {
            IdKind::Serial => "Serial number",
            IdKind::Registration => "Registration",
            IdKind::Utm => "UTM ID",
            IdKind::Session => "Session ID",
            IdKind::Mac => "No ID yet (radio address)",
            IdKind::Unknown => "ID type unknown",
        }
    }
}

/// Aircraft type, grouped to what fits on a small screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UaKind {
    Multirotor,
    FixedWing,
    Balloon,
    Other,
    #[default]
    Unknown,
}

impl UaKind {
    pub const fn label(self) -> &'static str {
        match self {
            UaKind::Multirotor => "Multirotor",
            UaKind::FixedWing => "Fixed wing",
            UaKind::Balloon => "Balloon/airship",
            UaKind::Other => "Other aircraft",
            UaKind::Unknown => "Type unknown",
        }
    }
}

/// What a drone's `height_m` is measured from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HeightRef {
    #[default]
    Takeoff,
    Ground,
}

/// Range and bearing from the unit (or from the drone, for its operator).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Polar {
    pub distance_m: f32,
    /// Degrees clockwise from **true** north, 0 to <360.
    pub bearing_deg: f32,
}

/// One drone, as the screen needs it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DroneView {
    /// Stable handle (the tracker's `TrackId`). The cursor follows this, so
    /// a selected drone stays selected while the list re-sorts.
    pub key: u32,
    /// UAS ID as text, or lowercase hex for binary IDs (a UTM UUID is 32
    /// hex digits). For [`IdKind::Mac`], the MAC as `aa:bb:cc:dd:ee:ff`.
    pub id: String<40>,
    pub id_kind: IdKind,
    pub ua: UaKind,
    /// Not heard for a while (tracker: 60 s). Drawn dimmed and hollow.
    pub stale: bool,
    /// Hub uptime when first and last heard.
    pub first_seen_ms: u64,
    pub last_seen_ms: u64,
    /// From the unit to the drone. `None` without an own GPS fix (P4) or
    /// before the drone has sent its position.
    pub drone: Option<Polar>,
    /// From the unit to the operator. `None` as above.
    pub operator: Option<Polar>,
    /// Drone to its operator. Needs no own fix: both positions come from
    /// the drone's broadcast, so this still works under P4.
    pub drone_to_operator_m: Option<f32>,
    /// Height above takeoff or ground, as the drone reports it.
    pub height_m: Option<f32>,
    pub height_ref: HeightRef,
    /// Geodetic altitude (WGS-84), metres.
    pub alt_m: Option<f32>,
    /// Drone altitude minus own altitude. Needs a 3D own fix.
    pub above_you_m: Option<f32>,
    pub speed_mps: Option<f32>,
    /// Up positive.
    pub vspeed_mps: Option<f32>,
    /// Direction of travel, degrees true.
    pub track_deg: Option<f32>,
    /// Radios this drone has been heard on.
    pub radios: Radios,
    /// Latest RSSI per radio, dBm, indexed by [`Radio::index`].
    pub rssi: [Option<i8>; 5],
    /// Self-ID description (free text from the operator).
    pub self_id: String<23>,
    pub operator_id: String<20>,
}

impl DroneView {
    /// Strongest current signal across radios, for the no-position list.
    pub fn best_rssi(&self) -> Option<i8> {
        self.rssi.iter().flatten().copied().max()
    }
}

/// GPS fix quality, as the status bar shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Fix {
    #[default]
    None,
    Fix2D,
    Fix3D,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Gps {
    pub fix: Fix,
    /// Satellites used in the solution.
    pub sats: u8,
}

impl Gps {
    /// True when the unit knows where it is, so distances can be shown.
    pub fn has_position(&self) -> bool {
        self.fix != Fix::None
    }
}

/// Which radio node a status entry is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Wifi24,
    Wifi5,
    Ble,
    Fpv,
    Adsb,
}

impl NodeKind {
    /// Status-bar label (three characters at most).
    pub const fn short(self) -> &'static str {
        match self {
            NodeKind::Wifi24 => "2.4",
            NodeKind::Wifi5 => "5",
            NodeKind::Ble => "BT",
            NodeKind::Fpv => "FPV",
            NodeKind::Adsb => "ADS",
        }
    }
    /// Health-screen label.
    pub const fn long(self) -> &'static str {
        match self {
            NodeKind::Wifi24 => "Wi-Fi 2.4",
            NodeKind::Wifi5 => "Wi-Fi 5",
            NodeKind::Ble => "Bluetooth",
            NodeKind::Fpv => "FPV 5.8",
            NodeKind::Adsb => "ADS-B",
        }
    }
}

/// One radio node's health, as reported over its link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeStatus {
    pub kind: NodeKind,
    /// Heard from recently (the hub decides the timeout).
    pub alive: bool,
    /// Frames per second the node is sniffing (all frames, not only RID).
    pub frames_per_s: u16,
    /// Remote ID frames found since the node started.
    pub rid_found: u32,
    /// Frames lost (node queue overflow + link errors).
    pub drops: u32,
    /// Node uptime, seconds.
    pub uptime_s: u32,
}

/// How the unit is powered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Power {
    /// On USB. `battery_pct` is the charge if a battery is fitted.
    Usb {
        battery_pct: Option<u8>,
    },
    Battery {
        pct: u8,
    },
}

impl Default for Power {
    fn default() -> Self {
        Power::Usb { battery_pct: None }
    }
}

/// SD-card log state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SdLog {
    #[default]
    Off,
    Recording,
    /// Logging was asked for but is failing (no card, full, write error).
    Error,
}

/// Everything one frame shows. The hub owns one (e.g. in a `static`) and
/// refills it each frame; nothing in it is retained by the screen.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UiState {
    /// Hub uptime, milliseconds. Ages are computed against this.
    pub now_ms: u64,
    /// UTC as Unix milliseconds, from GPS time. `None` until GPS time is known.
    pub utc_ms: Option<u64>,
    pub gps: Gps,
    pub nodes: Vec<NodeStatus, MAX_NODES>,
    pub power: Power,
    pub sd: SdLog,
    /// In any order; the screen sorts.
    pub drones: Vec<DroneView, MAX_DRONES>,
    /// Drones the tracker holds, which may exceed `drones.len()`.
    pub total_drones: u16,
    pub hub_uptime_s: u32,
}

impl UiState {
    pub fn drone(&self, key: u32) -> Option<&DroneView> {
        self.drones.iter().find(|d| d.key == key)
    }

    /// Drones heard recently (not stale), counting ones not passed in.
    pub fn active_count(&self) -> usize {
        let hidden = usize::from(self.total_drones).saturating_sub(self.drones.len());
        self.drones.iter().filter(|d| !d.stale).count() + hidden
    }

    /// Drone indices in screen order. With a position: active drones by
    /// distance (those with no position yet after them), then stale ones.
    /// Without: active drones newest first, then stale ones by last heard.
    ///
    /// "Newest first" rather than "last heard first" because every active
    /// drone is heard about once a second: ordering by last-heard would
    /// reshuffle the rows on every frame.
    pub fn order(&self) -> Vec<u8, MAX_DRONES> {
        let mut idx: Vec<u8, MAX_DRONES> = (0..self.drones.len() as u8).collect();
        let fix = self.gps.has_position();
        let key = |i: &u8| {
            let d = &self.drones[usize::from(*i)];
            let dist = if fix {
                d.drone.map_or(u32::MAX, |p| p.distance_m.max(0.0) as u32)
            } else {
                u32::MAX
            };
            let recency = if d.stale {
                d.last_seen_ms
            } else {
                d.first_seen_ms
            };
            (d.stale, dist, u64::MAX - recency, d.key)
        };
        idx.sort_unstable_by_key(key);
        idx
    }
}
