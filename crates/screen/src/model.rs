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

/// One of the six ways a drone can be heard. The letter is what the list
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
    /// DJI's OcuSync DroneID burst (any band, plaintext or O4).
    DroneId,
}

impl Radio {
    /// Display order of the letters: W N B L 5 D. (DJI's legacy Wi-Fi
    /// DroneID is a 2.4 GHz beacon and shows as W.)
    pub const ALL: [Radio; 6] = [
        Radio::WifiBeacon,
        Radio::WifiNan,
        Radio::Ble,
        Radio::BleLongRange,
        Radio::Wifi5,
        Radio::DroneId,
    ];

    pub const fn letter(self) -> char {
        match self {
            Radio::WifiBeacon => 'W',
            Radio::WifiNan => 'N',
            Radio::Ble => 'B',
            Radio::BleLongRange => 'L',
            Radio::Wifi5 => '5',
            Radio::DroneId => 'D',
        }
    }

    pub const fn index(self) -> usize {
        match self {
            Radio::WifiBeacon => 0,
            Radio::WifiNan => 1,
            Radio::Ble => 2,
            Radio::BleLongRange => 3,
            Radio::Wifi5 => 4,
            Radio::DroneId => 5,
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
    /// DJI DroneID with no readable identity (encrypted O4, a burst that did
    /// not decode, or identity hidden by the unit's settings): the text says
    /// which and on what frequency.
    Presence,
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
            IdKind::Presence => "DJI DroneID, no ID",
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
    /// Heard only once so far, or for under a second (tracker
    /// `probation`): not announced yet, and the cheapest thing to forge.
    /// Listed after every confirmed drone, its number shown as `?`, drawn
    /// dim and hollow on the radar.
    pub unconfirmed: bool,
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
    pub rssi: [Option<i8>; 6],
    /// Model name from DJI DroneID ("Mavic 3"); empty when unknown.
    pub model: String<20>,
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
    /// Receiver answering, no fix yet.
    #[default]
    None,
    Fix2D,
    Fix3D,
    /// No GPS fix, but a position typed in on the phone page (or over USB)
    /// is in use, so distances and bearings are real. Shown as "MAN" so
    /// nobody mistakes it for a live fix: it does not move with the unit.
    Manual,
    /// No GPS receiver answering at all (not fitted, unplugged, or wired
    /// wrong). Distinct from [`Fix::None`] so the health page can say which.
    Absent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Gps {
    pub fix: Fix,
    /// Satellites used in the solution.
    pub sats: u8,
    /// Signs that the hub's own GPS is being jammed or faked.
    pub integrity: GpsIntegrity,
}

/// The hub's verdict on its own GPS (`gnss::integrity` on the P4). While
/// spoofing is suspected the hub holds its last trusted position and time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GpsIntegrity {
    #[default]
    Ok,
    SuspectJamming,
    Jamming,
    SuspectSpoofing,
    Spoofing,
}

impl GpsIntegrity {
    /// Status-bar text, `None` when all is well.
    pub fn badge(self) -> Option<&'static str> {
        match self {
            GpsIntegrity::Ok => None,
            GpsIntegrity::SuspectJamming => Some("GPS JAM?"),
            GpsIntegrity::Jamming => Some("GPS JAM"),
            GpsIntegrity::SuspectSpoofing => Some("GPS FAKE?"),
            GpsIntegrity::Spoofing => Some("GPS FAKE"),
        }
    }
}

impl Gps {
    /// True when the unit knows where it is, so distances can be shown.
    pub fn has_position(&self) -> bool {
        matches!(self.fix, Fix::Fix2D | Fix::Fix3D | Fix::Manual)
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
    /// A raw-capture node decoding DJI DroneID bursts.
    DroneId,
    /// An ESP-NOW gateway relaying other nodes.
    Gateway,
    /// An LR2021 radio node (aircraft signals, control links).
    Lr,
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
            NodeKind::DroneId => "DID",
            NodeKind::Gateway => "GW",
            NodeKind::Lr => "LR",
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
            NodeKind::DroneId => "DroneID",
            NodeKind::Gateway => "Gateway",
            NodeKind::Lr => "LR radio",
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

/// What the hub's coverage coordinator asked one node to do (health screen).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Assigned {
    /// Role asked for, as a health-screen label (e.g. [`NodeKind::long`]).
    pub role: &'static str,
    /// Plan id asked for.
    pub plan: u16,
    /// The node reports running exactly this role and plan.
    pub applied: bool,
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
    /// No card in the slot (or none answering). Not an alarm: a unit
    /// without a card is a normal configuration. The hub keeps checking.
    NoCard,
    /// Logging was asked for but is failing (card full, write error).
    Error,
}

/// Facts about this unit that must stay on screen for as long as they hold
/// (P4 hub): what its evidence signatures are worth, a bench build, an open
/// (unauthenticated) node link, evidence not being written. Drawn as a strip
/// along the bottom edge on every page. The default shows nothing, so hubs
/// that do not fill it (the S3 hub) are unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Caveats {
    /// The key the evidence log is signed with.
    pub key: EvidenceKey,
    /// The firmware was built with bench-only features (fake data, debug
    /// doors).
    pub bench: bool,
    /// The gateway reports an open link: no ESP-NOW key, so anyone nearby
    /// can inject node reports and commands.
    pub link_open: bool,
    /// Evidence records are not reaching the card; `None` while they are.
    pub evidence: Option<EvidenceStop>,
}

/// The evidence signing key's class, as far as the screen is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EvidenceKey {
    /// Nothing to say (no evidence log, or a hardware key).
    #[default]
    NotShown,
    /// The published test key: anyone can sign with it.
    Test,
    /// A development key in the firmware image: not evidence-grade.
    Development,
}

/// Why evidence is not being written, and how much is waiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvidenceStop {
    pub why: EvidenceStopWhy,
    /// Records queued in RAM, not yet safely on the card.
    pub waiting: u32,
    /// Records that could not even be queued (counted since boot).
    pub dropped: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceStopWhy {
    /// No card answering.
    NoCard,
    /// The card answered but failed (full, write error, not FAT).
    CardError,
    /// The card works (the CSV log may be recording) but the evidence
    /// folder or file could not be opened; retried.
    FileError,
}

impl EvidenceStopWhy {
    pub fn text(self) -> &'static str {
        match self {
            EvidenceStopWhy::NoCard => "no SD",
            EvidenceStopWhy::CardError => "SD error",
            EvidenceStopWhy::FileError => "file error",
        }
    }
}

impl Caveats {
    /// The bottom strip's lines, top to bottom (`None`: no line). The first
    /// is the evidence state (only while evidence is not being written),
    /// the second the build marks (key class, bench build, open link).
    pub fn lines(&self) -> [Option<String<64>>; 2] {
        use core::fmt::Write;
        let ev = self.evidence.map(|e| {
            let mut s = String::new();
            let _ = write!(s, "NO EVIDENCE ({}): {} waiting", e.why.text(), e.waiting);
            if e.dropped > 0 {
                let _ = write!(s, ", {} dropped", e.dropped);
            }
            s
        });
        let mut marks: String<64> = String::new();
        let mut add = |t: &str| {
            if !marks.is_empty() {
                let _ = marks.push_str("  ");
            }
            let _ = marks.push_str(t);
        };
        match self.key {
            EvidenceKey::NotShown => {}
            EvidenceKey::Test => add("TEST KEY: log not evidence"),
            EvidenceKey::Development => add("DEV KEY: not evidence-grade"),
        }
        if self.bench {
            add("BENCH BUILD");
        }
        if self.link_open {
            add("OPEN LINK");
        }
        [ev, (!marks.is_empty()).then_some(marks)]
    }
}

/// The nearest aircraft, for the health page.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NearestAircraft {
    /// Callsign, or the ICAO address in hex when it has sent none.
    pub name: String<8>,
    pub distance_m: f32,
    /// Pressure altitude, feet.
    pub alt_ft: Option<i32>,
}

/// Crewed aircraft from the ADS-B receiver, summarised. Aircraft are not
/// drones: they never appear in the drone list, on the radar or in the
/// new-drone alert, only as this line on the health page.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AircraftSummary {
    /// Aircraft the hub holds.
    pub total: u16,
    /// Of those, heard recently.
    pub active: u16,
    /// Active, airborne, within 5 km and about 2000 ft of the unit.
    pub low_nearby: u16,
    /// The nearest active aircraft with a position (needs an own position).
    pub nearest: Option<NearestAircraft>,
}

/// RF emitters and Wi-Fi drones the screen is given per frame (each).
pub const MAX_SIGNALS: usize = 8;

/// What a raw-capture node recognised (mirrors `link::RfKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EmitterKind {
    AnalogVideo,
    FpvCarrier,
    Hdzero,
    LteStyle,
    WifiNarrow,
    #[default]
    Energy,
    DjiVideo,
    DjiO4Video,
    DjiFpvAirUnit,
    DjiController,
}

impl EmitterKind {
    /// Signals-page label, eleven characters at most.
    pub const fn label(self) -> &'static str {
        match self {
            EmitterKind::AnalogVideo => "Analog vid",
            EmitterKind::FpvCarrier => "FPV carr.",
            EmitterKind::Hdzero => "HDZero",
            EmitterKind::LteStyle => "LTE-style",
            EmitterKind::WifiNarrow => "Narrow WiFi",
            EmitterKind::Energy => "Energy",
            EmitterKind::DjiVideo => "DJI video",
            EmitterKind::DjiO4Video => "DJI O4 vid",
            EmitterKind::DjiFpvAirUnit => "DJI FPV air",
            EmitterKind::DjiController => "DJI control",
        }
    }
}

/// One RF emitter (a kind on one frequency), no position.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct EmitterView {
    pub key: u32,
    pub kind: EmitterKind,
    pub freq_mhz: f32,
    /// Level above the noise floor, dB.
    pub snr_db: i8,
    pub duty_pct: u8,
    /// Analog line rate, Hz (0 if none measured).
    pub line_hz: u16,
    pub stale: bool,
    pub last_seen_ms: u64,
}

/// One Wi-Fi network that looks like a drone or its controller.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WifiDroneView {
    pub key: u32,
    pub maker: String<16>,
    /// Network name, printable ASCII only ('?' for anything else).
    pub ssid: String<32>,
    pub channel: u8,
    pub rssi: i8,
    pub stale: bool,
    pub last_seen_ms: u64,
}

/// Control-link rows the screen is given per frame.
pub const MAX_CONTROL_ROWS: usize = 3;

/// How sure a control-link row is (mirrors `ctrlwatch::Confidence`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ControlSure {
    /// Energy and timing only (closed systems, or a hopper).
    #[default]
    Energy,
    /// Right settings and size, nothing checkable.
    Likely,
    /// The packet passed the system's own checksum.
    Confirmed,
}

impl ControlSure {
    pub const fn label(self) -> &'static str {
        match self {
            ControlSure::Energy => "energy",
            ControlSure::Likely => "likely",
            ControlSure::Confirmed => "sure",
        }
    }
}

/// One drone control link (ExpressLRS, mLRS, SiK...) or a hopping
/// transmitter in a control band, for the SIGNALS page.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ControlView {
    pub key: u32,
    /// Short system name ("ELRS", "mLRS", "SiK", "ELRS?" for an energy
    /// candidate).
    pub system: String<8>,
    /// "915", "868", "433", "2.4".
    pub band: &'static str,
    pub sure: ControlSure,
    pub rssi: i16,
    /// Stick positions, telemetry, rate or hop pattern, 19 characters at most.
    pub detail: String<19>,
    pub stale: bool,
    pub last_seen_ms: u64,
}

/// Most flag rows the FLAGS page lists.
pub const MAX_FLAG_ROWS: usize = 12;

/// One "possible violation" flag (PLAN.md Phase 12), for the FLAGS page.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FlagRow {
    /// Drone ID (or aircraft callsign / ICAO), shortened.
    pub who: String<14>,
    /// 0 Note, 1 Watch, 2 Warning, 3 Alert.
    pub level: u8,
    /// Short rule code ("HEIGHT", "UASFM", "AREA"...).
    pub rule: &'static str,
    /// Evidence in a few words ("520ft AGL", "Class B").
    pub text: String<24>,
    /// How long it has held, seconds.
    pub secs: u16,
}

/// The FLAGS page's content. `UiState::flags` is `None` on a hub without
/// the rule engine, and the page is then left out of the page cycle.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FlagsView {
    /// Airspace pack status: "pack 2026-11-15", "no airspace pack", ...
    pub pack: String<32>,
    /// Worst first.
    pub rows: Vec<FlagRow, MAX_FLAG_ROWS>,
    /// Flags the hub holds (may exceed `rows.len()`).
    pub total: u16,
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
    /// Parallel to `nodes`, for hubs that assign roles: what each node was
    /// asked to do. Empty (the default) shows nothing extra.
    pub assigned: Vec<Option<Assigned>, MAX_NODES>,
    pub power: Power,
    pub sd: SdLog,
    /// In any order; the screen sorts.
    pub drones: Vec<DroneView, MAX_DRONES>,
    /// Drones the tracker holds, which may exceed `drones.len()`.
    pub total_drones: u16,
    /// Of those, unconfirmed (tracker `probation`). The big drone count
    /// leaves them out and says "+N unconfirmed" instead.
    pub total_unconfirmed: u16,
    pub hub_uptime_s: u32,
    /// `None` when the hub has no ADS-B receiver built in (no line shown).
    pub aircraft: Option<AircraftSummary>,
    /// RF emitters, active first then by level (the hub's choice of
    /// [`MAX_SIGNALS`]), and how many the hub holds.
    pub emitters: Vec<EmitterView, MAX_SIGNALS>,
    pub total_emitters: u16,
    /// Wi-Fi drones, active first then strongest, and how many in all.
    pub wifi_drones: Vec<WifiDroneView, MAX_SIGNALS>,
    pub total_wifi_drones: u16,
    /// Drone control links, then control-band hoppers: active first, then
    /// strongest; and how many the hub holds.
    pub control: Vec<ControlView, MAX_CONTROL_ROWS>,
    pub total_control: u16,
    /// Possible-violation flags (P4 hub); `None` hides the FLAGS page.
    pub flags: Option<FlagsView>,
    /// Attacks and jamming the hub sees right now (link protocol 9).
    pub threats: Threats,
    /// The most serious drone / crewed-aircraft pair (tracker `conflict`).
    pub conflict: Option<ConflictView>,
    /// GPS jamming or spoofing in the area, seen through aircraft and
    /// drones (tracker `area_gnss`); `None` when neither is raised.
    pub area_gnss: Option<AreaGnssView>,
    /// Build and evidence facts kept on screen (P4 hub); the default
    /// shows nothing.
    pub caveats: Caveats,
}

/// GPS interference in the area (tracker `area_gnss`), for the status bar
/// and health page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AreaGnssView {
    pub jamming: bool,
    pub spoofing: bool,
    /// Jamming: aircraft reporting degraded GPS, and how many are counted.
    pub degraded: u8,
    pub counted: u8,
    /// Spoofing: drones and aircraft whose positions jumped together.
    pub drones_moved: u8,
    pub aircraft_moved: u8,
    /// The hub's own GPS check agrees.
    pub own_agrees: bool,
}

/// A drone close to a crewed aircraft, for the status bar and health page.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ConflictView {
    /// 1 advisory, 2 caution, 3 warning.
    pub level: u8,
    /// The drone (its `DroneView::key`).
    pub drone: u32,
    /// Callsign, or the ICAO address in hex.
    pub aircraft: String<8>,
    pub range_m: f32,
    /// Aircraft height minus drone height, metres.
    pub dz_m: Option<f32>,
    /// Pairs at caution or worse (this one included).
    pub count: u8,
}

/// Attacks and jamming, summarised for the status bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Threats {
    /// Active attacks of any kind (deauth floods, fake networks, jamming...).
    pub active: u8,
    /// A node is jammed or deaf: the detector may be missing drones, so an
    /// empty list must not read as "all clear".
    pub jammed: bool,
    /// Alarms the operator muted (still logged).
    pub muted: u8,
    /// A flood of forged drone identities (tracker `probation`): new IDs a
    /// second while its alarm is raised, 0 otherwise. Unconfirmed drones in
    /// the list are then mostly fakes.
    pub flood_per_s: u16,
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

    /// As [`Self::active_count`], confirmed drones only (unconfirmed ones
    /// are never stale: the tracker drops them first).
    pub fn confirmed_active_count(&self) -> usize {
        self.active_count().saturating_sub(usize::from(self.total_unconfirmed))
    }

    /// Drone indices in screen order: confirmed before unconfirmed (WP-09:
    /// a forged drone can claim any distance, so standing comes first), and
    /// within each, with a position: active drones by distance (those with
    /// no position yet after them), then stale ones. Without: active drones
    /// newest first, then stale ones by last heard.
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
            (d.stale, d.unconfirmed, dist, u64::MAX - recency, d.key)
        };
        idx.sort_unstable_by_key(key);
        idx
    }
}
