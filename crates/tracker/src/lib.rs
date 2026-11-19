//! Hub-side track table: merge Remote ID detections from every radio node
//! into one track per drone, work out range and bearing from the unit, and
//! format tracks for the phone page / USB stream (JSON), TAK (Cursor-on-Target)
//! and the SD-card log (CSV).
//!
//! `no_std`, no allocator: the table is a fixed array sized by a const
//! generic, and every formatter writes into a caller-provided buffer. The
//! laptop viewer (`tools/rid-view`) runs this same code, so hub and laptop
//! merge and expire tracks identically.
//!
//! # Identity
//! A drone is keyed by its Basic ID UAS ID. A drone may send two Basic IDs
//! (e.g. a serial number and a session ID); a track keeps one per ID type.
//! Until a Basic ID arrives (a BLE 4 advert carries one message, so the first
//! one heard may be a Location), the track is keyed by the transmitter MAC.
//! When an ID later arrives on that MAC the track simply gains the ID; when an
//! ID arrives that already belongs to another track, the MAC-keyed track is
//! folded into it. That is how a drone's Wi-Fi beacon (one MAC), its BLE
//! adverts (another MAC, which may rotate) and its NAN frames end up as one
//! track: the UAS ID is the only thing they share.
//!
//! None of this is authenticated, so a frame is evidence about a
//! transmitter, not a command to the track ([`claims`]): a position report
//! off a track's established path is held as a rival claim (a dispute when
//! both continue), a changed text field or ID is adopted only when it
//! persists, a frame with two IDs of one type is applied to nothing, and
//! two tracks with their own IDs are merged only after repeated agreement.
//!
//! # Open FPV links
//! Separately from drones, the tracker keeps a small table of open digital
//! FPV / telemetry links (wfb-ng, OpenHD, RubyFPV, DroneBridge) that the
//! Wi-Fi nodes recognise with the `openlink` crate: [`Tracker::ingest_open_link`],
//! [`Tracker::open_links`], module [`openlinks`]. Their identifiers stay out of
//! every output unless [`Config::show_link_ids`] is set.
//!
//! # Aircraft
//! Crewed aircraft from an ADSBee (ADS-B), a third table with its own
//! expiry, never alerted on: [`Tracker::ingest_aircraft`],
//! [`Tracker::aircraft`], module [`aircraft`]. Its capacity is the second
//! const generic of [`Tracker`] (0 on a hub built without ADS-B).
//!
//! # DJI DroneID
//! DJI's own broadcasts become ordinary drone tracks ([`Tracker::ingest_droneid`],
//! [`Tracker::ingest_dji_wifi`], module [`dji`]): the OcuSync DroneID burst
//! from a raw-capture node and the legacy Wi-Fi DroneID element from a Wi-Fi
//! node. A plaintext record carries the serial number, which on DJI aircraft
//! is the same as their Remote ID serial, so both end up on one track. An
//! encrypted (O4) record gives a presence-only track keyed by its session
//! key ID; a burst that did not decode joins one presence-only track.
//!
//! # Drone control links
//! ExpressLRS, mLRS, SiK and OpenLRSng links and control-band energy from
//! LR2021 nodes (link protocol 12): a table of their own, kept outside the
//! tracker (module [`ctrllinks`], [`ctrllinks::ControlTable`]). A position decoded from a link's telemetry (only with
//! the owner's content opt-in) becomes an ordinary drone track, source
//! [`Source::ControlLink`].
//!
//! # Other signals
//! Raw-capture RF emitters (analog video, HDZero, LTE-style links...) and
//! Wi-Fi drones recognised by network name and address prefix are two more
//! tables ([`Tracker::ingest_rf`], [`Tracker::ingest_wifi_drone`], module
//! [`signals`]), with no position. Node `Status` frames are kept per node
//! for the health view ([`Tracker::nodes`]).
//!
//! # Time
//! All `*_ms` times are hub monotonic milliseconds (uptime), supplied by the
//! caller. Wall-clock UTC is only needed by the CoT and CSV formatters and is
//! passed in separately, because the hub may not have GPS time.
#![no_std]

pub mod aircraft;
pub mod area_gnss;
pub mod behaviour;
pub mod claims;
pub mod continuity;
pub mod ctrllinks;
pub mod cot;
pub mod csv;
pub mod dji;
pub mod geo;
pub mod json;
pub mod openlinks;
pub mod probation;
pub mod signals;
pub mod spoof;
mod text;
pub mod site;
pub mod threats;
pub mod conflict;

use heapless::Vec;
use rid::Transport;
use rid::message::{
    BasicId, FixedStr, IdType, LatLon, Location, Message, OperatorId, SelfId, System, UaType,
};

pub use aircraft::{AircraftGeometry, AircraftKey, AircraftTrack};
pub use claims::{ClaimSrc, Claims, Dispute, DisputeEvent, Rival};
pub use continuity::Continuity;
pub use dji::{DjiInfo, DjiLink, O4Decrypted, O4Seen, O4State, Source};
pub use geo::RangeBearing;
pub use openlinks::{LinkTrack, LinkTrackId};
pub use signals::{Emitter, EmitterId, NodeInfo, SignalKey, WifiDrone, WifiDroneId};
pub use spoof::{Check, Flag, SpoofEvent, SpoofState};
pub use text::FormatError;

/// Basic IDs kept per track: F3411 allows a UA to send up to two (one per
/// ID type), e.g. a serial number and a CAA registration or session ID.
pub const MAX_IDS: usize = 2;

/// Every transport, in the order [`Track::links`] is indexed.
pub const TRANSPORTS: [Transport; 5] = [
    Transport::WifiBeacon,
    Transport::WifiNan,
    Transport::BleLegacy,
    Transport::BleExtended,
    Transport::BleLongRange,
];

/// Index of `t` in [`TRANSPORTS`]. (Not an index into [`Track::links`], which
/// is per [`Slot`]: use [`Slot::index`] for that.)
pub fn transport_index(t: Transport) -> usize {
    match t {
        Transport::WifiBeacon => 0,
        Transport::WifiNan => 1,
        Transport::BleLegacy => 2,
        Transport::BleExtended => 3,
        Transport::BleLongRange => 4,
    }
}

/// Which radio band a frame was heard on.
///
/// Only Wi-Fi has a choice: Remote ID is sent on 2.4 GHz and, by some drones
/// (DJI Avata, Mini 4), on 5 GHz. The 2.4 GHz node (S3) and the 5 GHz node
/// (C5) hear different frames, so a drone heard on both must keep both,
/// otherwise each band's frames overwrite the other's RSSI, channel and
/// counter and the screen can only guess "5 GHz" from whichever came last.
/// Bluetooth is always 2.4 GHz.
///
/// Serialized as `"2.4"` and `"5"` (the `band` field of a JSON link entry).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
pub enum Band {
    #[serde(rename = "2.4")]
    Ghz24,
    #[serde(rename = "5")]
    Ghz5,
}

impl Band {
    /// The band of an 802.11 channel number: 1-14 are 2.4 GHz, 32-177 are
    /// 5 GHz (no node listens on 6 GHz). Channel 0 means "unknown" in a
    /// detection and counts as 2.4 GHz, which is what every node meant by it
    /// before the 5 GHz node existed.
    pub const fn of_wifi_channel(channel: u8) -> Band {
        if channel > 14 {
            Band::Ghz5
        } else {
            Band::Ghz24
        }
    }
}

/// One way a drone can be heard: a transport, and for Wi-Fi also the band.
/// A track keeps one [`LinkStats`] per slot ([`Track::links`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Slot {
    pub transport: Transport,
    pub band: Band,
}

/// Every slot, in the order [`Track::links`] is indexed and JSON lists them.
/// The first two and last three are the five slots the tracker had before
/// the band split, in their old relative order.
pub const SLOTS: [Slot; 7] = [
    Slot {
        transport: Transport::WifiBeacon,
        band: Band::Ghz24,
    },
    Slot {
        transport: Transport::WifiNan,
        band: Band::Ghz24,
    },
    Slot {
        transport: Transport::WifiBeacon,
        band: Band::Ghz5,
    },
    Slot {
        transport: Transport::WifiNan,
        band: Band::Ghz5,
    },
    Slot {
        transport: Transport::BleLegacy,
        band: Band::Ghz24,
    },
    Slot {
        transport: Transport::BleExtended,
        band: Band::Ghz24,
    },
    Slot {
        transport: Transport::BleLongRange,
        band: Band::Ghz24,
    },
];

impl Slot {
    /// The slot a frame on `transport` and `channel` belongs to. For
    /// Bluetooth the channel is an advertising channel (37-39 or a data
    /// channel), never a 5 GHz one, so it is ignored.
    pub const fn of(transport: Transport, channel: u8) -> Slot {
        let band = match transport {
            Transport::WifiBeacon | Transport::WifiNan => Band::of_wifi_channel(channel),
            _ => Band::Ghz24,
        };
        Slot { transport, band }
    }

    /// Index of this slot in [`SLOTS`] and [`Track::links`].
    pub const fn index(self) -> usize {
        match (self.transport, self.band) {
            (Transport::WifiBeacon, Band::Ghz24) => 0,
            (Transport::WifiNan, Band::Ghz24) => 1,
            (Transport::WifiBeacon, Band::Ghz5) => 2,
            (Transport::WifiNan, Band::Ghz5) => 3,
            // Bluetooth has no 5 GHz slot; a hand-built Slot saying so is
            // folded onto the only one there is.
            (Transport::BleLegacy, _) => 4,
            (Transport::BleExtended, _) => 5,
            (Transport::BleLongRange, _) => 6,
        }
    }

    /// Name for CSV and CoT remarks: the transport name, with `5G` appended
    /// for the 5 GHz Wi-Fi slots (`WifiBeacon5G`, `WifiNan5G`).
    pub const fn name(self) -> &'static str {
        match (self.transport, self.band) {
            (Transport::WifiBeacon, Band::Ghz24) => "WifiBeacon",
            (Transport::WifiNan, Band::Ghz24) => "WifiNan",
            (Transport::WifiBeacon, Band::Ghz5) => "WifiBeacon5G",
            (Transport::WifiNan, Band::Ghz5) => "WifiNan5G",
            (Transport::BleLegacy, _) => "BleLegacy",
            (Transport::BleExtended, _) => "BleExtended",
            (Transport::BleLongRange, _) => "BleLongRange",
        }
    }
}

/// Wi-Fi and Bluetooth addresses are separate namespaces: a drone's beacon and
/// NAN frames come from one Wi-Fi MAC, its legacy and Long Range adverts may
/// share one BLE address, but a Wi-Fi MAC says nothing about a BLE address.
fn same_medium(a: Transport, b: Transport) -> bool {
    let wifi = |t| matches!(t, Transport::WifiBeacon | Transport::WifiNan);
    wifi(a) == wifi(b)
}

/// Tunables. The defaults follow PLAN.md (D6: alert within 3 s).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// A track not heard for this long is shown as stale.
    pub stale_after_ms: u64,
    /// A track not heard for this long is removed.
    pub remove_after_ms: u64,
    /// A track with no Basic ID yet waits this long before its "new drone"
    /// alert, so a Location-only BLE advert that is about to be merged into
    /// an existing drone by its ID does not beep twice. A track that has an
    /// ID is announced at once. Basic IDs repeat at least every second, so
    /// 1 s keeps the alert inside D6's 3 s budget.
    pub announce_grace_ms: u64,
    /// Own GPS fix older than this is treated as "position unknown" (P4).
    pub own_fix_max_age_ms: u64,
    /// Show open-link identifiers (wfb-ng link ID, RubyFPV vehicle ID,
    /// DroneBridge comm ID) and those links' transmitter addresses in every
    /// output. **Off by default** (project policy): recognising that a wfb-ng
    /// or OpenHD link is on the air is fine to show anyone; the identifiers
    /// single out a person's equipment, so they appear only when whoever runs
    /// the unit opts in. Drone Remote ID is unaffected (it is broadcast to be
    /// read).
    pub show_link_ids: bool,
    /// Keep identity decoded from DJI's DroneID (OcuSync bursts and the
    /// legacy Wi-Fi element) out of every output: the serial number, the
    /// O4 session key ID and the flight-purpose text. **Off by default**
    /// (owner decision: DroneID plaintext is shown). Positions are still
    /// shown. A serial the drone also broadcasts in Remote ID is shown
    /// regardless, since Remote ID is meant to be read. Applied as records
    /// arrive, so turning it on does not scrub tracks already held.
    pub hide_droneid_ids: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            stale_after_ms: 60_000,
            remove_after_ms: 600_000,
            announce_grace_ms: 1_000,
            own_fix_max_age_ms: 10_000,
            show_link_ids: false,
            hide_droneid_ids: false,
        }
    }
}

/// What one received frame or advertisement looked like, apart from its
/// Remote ID content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Observation {
    /// Which radio node heard it (hub-assigned, e.g. its UART index).
    pub node: u8,
    pub transport: Transport,
    pub channel: u8,
    pub rssi: i8,
    pub mac: [u8; 6],
    /// F3411 message counter.
    pub counter: u8,
    /// Hub time of arrival.
    pub rx_ms: u64,
}

impl Observation {
    /// The metadata of a node's [`link::Detection`], stamped with hub time.
    /// (The node's own `uptime_us` is on a different clock and is not used.)
    pub fn from_detection(d: &link::Detection, node: u8, rx_ms: u64) -> Self {
        Self {
            node,
            transport: d.transport,
            channel: d.channel,
            rssi: d.rssi,
            mac: d.mac,
            counter: d.counter,
            rx_ms,
        }
    }
}

/// Stable handle for a track: unique for the life of the [`Tracker`], never
/// reused, unlike a slot index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(transparent)]
pub struct TrackId(pub u32);

/// A value and the hub time it was received.
#[derive(Debug, Clone, PartialEq)]
pub struct Stamped<T> {
    pub value: T,
    pub rx_ms: u64,
}

/// One Basic ID the drone sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UasId {
    pub id_type: IdType,
    /// As transmitted: text for serial numbers and registrations, binary for
    /// UTM UUIDs and session IDs.
    pub id: FixedStr<20>,
}

impl UasId {
    fn of(b: &BasicId) -> Self {
        Self {
            id_type: b.id_type,
            id: b.uas_id,
        }
    }
}

/// Reception statistics for one [`Slot`] (transport and band).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LinkStats {
    /// Frames/adverts heard. Zero means this slot was never heard.
    pub count: u32,
    pub last_seen_ms: u64,
    pub rssi: i8,
    pub counter: u8,
    pub channel: u8,
    /// Most recent transmitter address in this slot.
    pub mac: [u8; 6],
    /// Node that heard the most recent one.
    pub node: u8,
}

impl LinkStats {
    pub fn heard(&self) -> bool {
        self.count > 0
    }
}

/// Everything known about one drone.
#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub id: TrackId,
    /// Basic IDs heard, at most one per ID type. Empty for a MAC-keyed track.
    pub uas_ids: Vec<UasId, MAX_IDS>,
    pub ua_type: Option<UaType>,
    pub location: Option<Stamped<Location>>,
    /// Operator position and classification.
    pub system: Option<Stamped<System>>,
    pub self_id: Option<Stamped<SelfId>>,
    pub operator_id: Option<Stamped<OperatorId>>,
    /// Per [`Slot`] (transport and band), indexed by [`Slot::index`].
    pub links: [LinkStats; SLOTS.len()],
    /// Bit `n` set when node `n` (0-7) has heard this drone.
    pub nodes: u8,
    pub first_seen_ms: u64,
    pub last_seen_ms: u64,
    /// Messages in this drone's frames that failed to decode.
    pub decode_errors: u32,
    /// Where `location` came from.
    pub location_src: Source,
    /// Where `system` (the operator position) came from.
    pub system_src: Source,
    /// Home point, from DJI DroneID only (Remote ID has none).
    pub home: Option<Stamped<rid::message::LatLon>>,
    /// What DJI's own broadcasts said (default when never heard).
    pub dji: DjiInfo,
    /// Spoofing / consistency flags (see [`spoof`]).
    pub spoof: SpoofState,
    /// Link to the track this one probably continues (see [`continuity`]).
    pub continuity: Continuity,
    /// Rival claims on this track's ID and disputes (see [`claims`]).
    pub claims: Claims,
    announced: bool,
    /// Frames counted towards standing ([`probation`]): every decoded frame
    /// once it has a track, and the sightings that earned it one.
    heard: u32,
    /// When it was confirmed ([`probation`]); `None` while unconfirmed.
    confirmed_ms: Option<u64>,
    /// The node whose frame made the track ([`probation::ShedWatch`]).
    first_node: u8,
    /// Its threat level and the reasons ([`behaviour`]), set once a second
    /// by [`Tracker::tick_behaviour`].
    pub threat: behaviour::TrackThreat,
}

impl Track {
    fn new(id: TrackId, now_ms: u64) -> Self {
        Self {
            id,
            uas_ids: Vec::new(),
            ua_type: None,
            location: None,
            system: None,
            self_id: None,
            operator_id: None,
            links: [LinkStats::default(); SLOTS.len()],
            nodes: 0,
            first_seen_ms: now_ms,
            last_seen_ms: now_ms,
            decode_errors: 0,
            location_src: Source::RemoteId,
            system_src: Source::RemoteId,
            home: None,
            dji: DjiInfo::default(),
            spoof: SpoofState::default(),
            continuity: Continuity::default(),
            claims: Claims::default(),
            announced: false,
            heard: 0,
            confirmed_ms: None,
            first_node: threats::NODE_ANY,
            threat: behaviour::TrackThreat::default(),
        }
    }

    /// The ID to show and to key external systems on: a serial number if
    /// known (it is stable across flights), then a registration, then a UTM
    /// UUID, then a session ID.
    pub fn primary_id(&self) -> Option<&UasId> {
        let rank = |t: IdType| match t {
            IdType::SerialNumber => 0,
            IdType::CaaRegistration => 1,
            IdType::UtmAssigned => 2,
            IdType::SpecificSession => 3,
            _ => 4,
        };
        self.uas_ids.iter().min_by_key(|u| rank(u.id_type))
    }

    /// Stats for one slot (transport and band).
    pub fn slot(&self, s: Slot) -> &LinkStats {
        &self.links[s.index()]
    }

    /// Slots this drone has been heard in, in [`SLOTS`] order.
    pub fn slots(&self) -> impl Iterator<Item = (Slot, &LinkStats)> + '_ {
        SLOTS
            .iter()
            .map(|s| (*s, self.slot(*s)))
            .filter(|(_, l)| l.heard())
    }

    /// Stats for a transport on any band: the frame counts added up, every
    /// other field from the band heard most recently. For Bluetooth this is
    /// simply its one slot.
    pub fn link(&self, t: Transport) -> LinkStats {
        let mut out = LinkStats::default();
        let mut count = 0u32;
        for (_, l) in self.slots().filter(|(s, _)| s.transport == t) {
            count = count.saturating_add(l.count);
            if out.count == 0 || l.last_seen_ms >= out.last_seen_ms {
                out = *l;
            }
        }
        out.count = count;
        out
    }

    /// Transports this drone has ever been heard on (on any band), in
    /// [`TRANSPORTS`] order.
    pub fn transports(&self) -> impl Iterator<Item = Transport> + '_ {
        TRANSPORTS
            .into_iter()
            .filter(|t| self.slots().any(|(s, _)| s.transport == *t))
    }

    /// The most recently heard slot's stats (for "the" RSSI and MAC).
    pub fn latest_slot(&self) -> Option<(Slot, &LinkStats)> {
        self.slots().max_by_key(|(_, l)| l.last_seen_ms)
    }

    /// As [`Track::latest_slot`], naming only the transport.
    pub fn latest_link(&self) -> Option<(Transport, &LinkStats)> {
        self.latest_slot().map(|(s, l)| (s.transport, l))
    }

    /// True once the "new drone" event has been handed out for this track.
    pub fn announced(&self) -> bool {
        self.announced
    }

    /// Heard again at least [`probation::CONFIRM_SPAN_MS`] after it was
    /// first heard ([`probation`]). Unconfirmed tracks are shown quietly,
    /// never announced, never sent to TAK and the first to make room.
    pub fn confirmed(&self) -> bool {
        self.confirmed_ms.is_some()
    }

    /// Hub time it was confirmed.
    pub fn confirmed_ms(&self) -> Option<u64> {
        self.confirmed_ms
    }

    /// Frames (or, before it had a track, sightings) counted towards its
    /// standing.
    pub fn frames_heard(&self) -> u32 {
        self.heard
    }

    /// One more frame from this drone: count it, and confirm the track
    /// once it has been heard long enough.
    /// Count one frame; true when that confirmed the track.
    fn heard_at(&mut self, now_ms: u64) -> bool {
        self.heard = self.heard.saturating_add(1);
        if self.confirmed_ms.is_none()
            && self.heard >= probation::CONFIRM_MIN_FRAMES
            && now_ms.saturating_sub(self.first_seen_ms) >= probation::CONFIRM_SPAN_MS
        {
            self.confirmed_ms = Some(now_ms);
            return true;
        }
        false
    }

    /// No two IDs of the same type disagree, so `other` may be the same drone.
    fn compatible_with(&self, ids: &[UasId]) -> bool {
        ids.iter().all(|n| {
            self.uas_ids
                .iter()
                .all(|o| o.id_type != n.id_type || o.id == n.id)
        })
    }

    /// Whether the track carries this ID, counting a DJI serial kept out of
    /// `uas_ids` by [`Config::hide_droneid_ids`].
    fn has_id(&self, id: &UasId) -> bool {
        self.uas_ids.contains(id)
            || (id.id_type == IdType::SerialNumber && self.dji.serial == Some(id.id))
    }

    /// Latest RSSI from any way this drone was heard (Remote ID slots and
    /// DJI's own broadcasts).
    pub fn latest_rssi(&self) -> Option<i8> {
        let rid = self.latest_slot().map(|(_, l)| (l.last_seen_ms, l.rssi));
        let dji = [&self.dji.ocusync, &self.dji.wifi]
            .into_iter()
            .filter(|l| l.heard())
            .map(|l| (l.last_seen_ms, l.rssi));
        rid.into_iter()
            .chain(dji)
            .max_by_key(|(t, _)| *t)
            .map(|(_, r)| r)
    }

    /// One name per way this drone was heard: the [`Slot::name`]s, then
    /// `DroneId` (or `DroneIdO4` for encrypted records) and `DjiWifi`.
    pub fn source_names(&self) -> impl Iterator<Item = &'static str> + '_ {
        let ocu = self
            .dji
            .ocusync
            .heard()
            .then_some(if self.dji.key_id.is_some() {
                "DroneIdO4"
            } else {
                "DroneId"
            });
        let wifi = self.dji.wifi.heard().then_some("DjiWifi");
        self.slots().map(|(s, _)| s.name()).chain(ocu).chain(wifi)
    }

    /// The most recent MAC this track used on `t`'s medium (either band),
    /// if it matches.
    fn heard_mac(&self, t: Transport, mac: &[u8; 6]) -> bool {
        self.slots()
            .any(|(s, l)| same_medium(s.transport, t) && l.mac == *mac)
    }

    fn add_id(&mut self, id: UasId) {
        if let Some(slot) = self.uas_ids.iter_mut().find(|o| o.id_type == id.id_type) {
            // Same type, new value: a session ID that changed mid-flight.
            *slot = id;
        } else if self.uas_ids.push(id).is_err() {
            // A third ID type. The standard allows two; keep the best-ranked
            // one (primary_id) and replace the other with the newest.
            let keep = self.primary_id().copied();
            let at = usize::from(Some(self.uas_ids[0]) == keep);
            self.uas_ids[at] = id;
        }
    }

    fn observe(&mut self, obs: &Observation) {
        let l = &mut self.links[Slot::of(obs.transport, obs.channel).index()];
        l.count = l.count.saturating_add(1);
        l.last_seen_ms = obs.rx_ms;
        l.rssi = obs.rssi;
        l.counter = obs.counter;
        l.channel = obs.channel;
        l.mac = obs.mac;
        l.node = obs.node;
        if obs.node < 8 {
            self.nodes |= 1 << obs.node;
        }
        self.last_seen_ms = self.last_seen_ms.max(obs.rx_ms);
    }

    /// Apply one message of a frame that is this track's own (on its path,
    /// see [`claims`]). IDs in `foreign` belong to another track and are
    /// not taken. A changed text field or ID is adopted only once it
    /// persists ([`Claims::text_change`]).
    fn apply(&mut self, msg: &Message, rx_ms: u64, foreign: &[UasId]) {
        fn put<T: Clone>(slot: &mut Option<Stamped<T>>, v: &T, rx_ms: u64) {
            *slot = Some(Stamped {
                value: v.clone(),
                rx_ms,
            });
        }
        let h = claims::hash_bytes;
        match msg {
            Message::BasicId(b) => {
                let id = UasId::of(b);
                if !foreign.contains(&id) {
                    self.offer_id(id, rx_ms);
                }
                if b.ua_type != UaType::None || self.ua_type.is_none() {
                    self.ua_type = Some(b.ua_type);
                }
            }
            Message::Location(l) => {
                put(&mut self.location, l, rx_ms);
                self.location_src = Source::RemoteId;
            }
            Message::System(s) => {
                put(&mut self.system, s, rx_ms);
                self.system_src = Source::RemoteId;
            }
            Message::SelfId(s) => {
                let est = self.self_id.as_ref().map(|e| h(e.value.description.as_bytes()));
                let new = h(s.description.as_bytes());
                if self.claims.text_change(claims::KIND_SELF_ID, est, new, rx_ms) || est == Some(new) {
                    put(&mut self.self_id, s, rx_ms);
                }
            }
            Message::OperatorId(o) => {
                let est = self.operator_id.as_ref().map(|e| h(e.value.operator_id.as_bytes()));
                let new = h(o.operator_id.as_bytes());
                if self.claims.text_change(claims::KIND_OPERATOR_ID, est, new, rx_ms) || est == Some(new) {
                    put(&mut self.operator_id, o, rx_ms);
                }
            }
            // Signatures are not verified on the hub: "auth message
            // present, not checked" is all anything may say about them.
            Message::Auth(_) => {}
        }
    }

    /// A Basic ID from one of this track's own frames: a new ID type is
    /// added; a different value of a type it has (a new session ID), or a
    /// third type, only once it persists ([`Claims::text_change`]).
    fn offer_id(&mut self, id: UasId, now: u64) {
        let h = claims::hash_bytes;
        let kind = claims::kind_id(id.id_type.code());
        match self.uas_ids.iter().position(|o| o.id_type == id.id_type) {
            Some(i) => {
                let est = h(self.uas_ids[i].id.as_bytes());
                if self.claims.text_change(kind, Some(est), h(id.id.as_bytes()), now) {
                    self.uas_ids[i] = id;
                }
            }
            None if !self.uas_ids.is_full() => {
                let _ = self.uas_ids.push(id);
            }
            None => {
                // A third ID type (F3411 allows two): it replaces the
                // non-primary one only once it persists.
                if self.claims.text_change(kind, Some(0), h(id.id.as_bytes()), now) {
                    self.add_id(id);
                }
            }
        }
    }

    /// The track followed its rival claim to `p` ([`claims`]): the old
    /// path ends, and a single transmitter's jump is flagged.
    pub(crate) fn follow(&mut self, p: LatLon, now: u64) {
        let from = self.location.as_ref().and_then(|l| l.value.position);
        if self.claims.followed(now)
            && let Some(q) = from
        {
            self.spoof.followed_jump(spoof::dist_m(q, p));
        }
        self.location = None;
        self.spoof.rebase();
    }

    /// A position report off this track's path ([`claims`]): held as the
    /// rival claim. A report that starts a new rival is a jump in this ID's
    /// reports; when nothing has been heard on the old path for
    /// [`claims::FOLLOW_AFTER_MS`] and there is no dispute, it is one
    /// transmitter jumping, and flagged as such.
    pub(crate) fn rival_claim(
        &mut self,
        p: LatLon,
        hacc: Option<f32>,
        src: ClaimSrc,
        mac: [u8; 6],
        now: u64,
    ) -> claims::Outcome {
        let orig = self.location.as_ref().map(|l| (l.rx_ms, l.value.position));
        let (out, jumped) = self.claims.rival_report(p, hacc, src, mac, orig.map_or(0, |o| o.0), now);
        // From the address this track's own frames come from (held rival
        // frames never update it): possibly this transmitter's path moving.
        let own_addr = mac != [0; 6] && self.slots().any(|(_, l)| l.mac == mac);
        self.claims.path_jump(own_addr, now);
        if jumped {
            self.spoof.note_jump();
            if let Some((ms, Some(q))) = orig
                && now.saturating_sub(ms) >= claims::FOLLOW_AFTER_MS
                && !self.claims.disputed(now)
            {
                self.spoof.followed_jump(spoof::dist_m(q, p));
            }
        }
        out
    }

    /// The newest position claimed for this ID: the rival claim when it was
    /// heard after the shown position, else the shown one.
    pub fn claimed_position(&self, now: u64) -> Option<LatLon> {
        let shown = self.location.as_ref().and_then(|l| l.value.position.map(|p| (p, l.rx_ms)));
        match (self.claims.rival(now), shown) {
            (Some(r), Some((_, ms))) if r.last_ms > ms => Some(r.position),
            (Some(r), None) => Some(r.position),
            (_, s) => s.map(|(p, _)| p),
        }
    }

    /// Fold `other` (the same drone, under another key) into `self`, keeping
    /// the newer of each piece of data.
    fn absorb(&mut self, other: Track) {
        fn newer<T>(a: &mut Option<Stamped<T>>, b: Option<Stamped<T>>) {
            if let Some(b) = b
                && a.as_ref().is_none_or(|a| b.rx_ms > a.rx_ms)
            {
                *a = Some(b);
            }
        }
        for id in other.uas_ids {
            if !self.has_id(&id) {
                self.add_id(id);
            }
        }
        if self.ua_type.is_none_or(|t| t == UaType::None) {
            self.ua_type = other.ua_type.or(self.ua_type);
        }
        if other
            .location
            .as_ref()
            .is_some_and(|b| self.location.as_ref().is_none_or(|a| b.rx_ms > a.rx_ms))
        {
            self.location_src = other.location_src;
        }
        if other
            .system
            .as_ref()
            .is_some_and(|b| self.system.as_ref().is_none_or(|a| b.rx_ms > a.rx_ms))
        {
            self.system_src = other.system_src;
        }
        newer(&mut self.location, other.location);
        newer(&mut self.system, other.system);
        newer(&mut self.home, other.home);
        self.dji.absorb(&other.dji);
        newer(&mut self.self_id, other.self_id);
        newer(&mut self.operator_id, other.operator_id);
        for (mine, theirs) in self.links.iter_mut().zip(other.links) {
            let count = mine.count.saturating_add(theirs.count);
            if theirs.heard() && (!mine.heard() || theirs.last_seen_ms > mine.last_seen_ms) {
                *mine = theirs;
            }
            mine.count = count;
        }
        self.nodes |= other.nodes;
        self.first_seen_ms = self.first_seen_ms.min(other.first_seen_ms);
        self.last_seen_ms = self.last_seen_ms.max(other.last_seen_ms);
        self.decode_errors = self.decode_errors.saturating_add(other.decode_errors);
        self.spoof.absorb(&other.spoof);
        self.continuity.absorb(&other.continuity);
        self.claims.absorb(&other.claims);
        // Whoever was alerted on already, the drone has been announced.
        self.announced |= other.announced;
        self.heard = self.heard.saturating_add(other.heard);
        self.confirmed_ms = match (self.confirmed_ms, other.confirmed_ms) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        self.threat.absorb(&other.threat);
    }
}

/// Whether a track is current.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackState {
    Active,
    /// Not heard for [`Config::stale_after_ms`]; kept until `remove_after_ms`.
    Stale,
}

/// The unit's own GPS fix.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OwnFix {
    pub lat: f64,
    pub lon: f64,
    /// WGS-84 height above ellipsoid (u-blox NAV-PVT `height`, not `hMSL`),
    /// so it compares with Remote ID's geodetic altitude. `None` for a 2D fix.
    pub alt_m: Option<f64>,
    pub fix_ms: u64,
    /// Ground speed, m/s: from the receiver when it says (u-blox NAV-PVT
    /// `gSpeed`, NMEA RMC), else worked out from consecutive fixes; 0 for a
    /// manual or held position, `None` until known. Below
    /// [`behaviour::OWN_STATIONARY_MPS`] it is 0 (standing still).
    pub speed_mps: Option<f32>,
    /// Direction of travel, degrees true; `None` while standing still or
    /// unknown.
    pub course_deg: Option<f32>,
}

/// Range, bearing and relative height of a drone and its operator from the
/// unit. Every field is `None` when the unit has no recent fix (P4) or the
/// drone has not sent the position involved.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Geometry {
    pub drone: Option<RangeBearing>,
    pub operator: Option<RangeBearing>,
    /// Drone geodetic altitude minus own altitude, metres (positive = above you).
    pub height_above_m: Option<f64>,
}

impl Geometry {
    pub fn of(track: &Track, own: Option<&OwnFix>) -> Self {
        let Some(own) = own else {
            return Self::default();
        };
        let loc = track.location.as_ref().map(|l| &l.value);
        let rb = |p: &rid::message::LatLon| geo::range_bearing(own.lat, own.lon, p.lat, p.lon);
        Self {
            drone: loc.and_then(|l| l.position.as_ref()).map(rb),
            operator: track
                .system
                .as_ref()
                .and_then(|s| s.value.operator_position.as_ref())
                .map(rb),
            height_above_m: loc
                .and_then(|l| l.altitude_geodetic)
                .zip(own.alt_m)
                .map(|(d, o)| f64::from(d) - o),
        }
    }
}

/// Everything a formatter needs besides the track: a snapshot of time and
/// own position, so formatting does not borrow the tracker.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Context {
    pub now_ms: u64,
    /// UTC (Unix milliseconds) corresponding to `now_ms`, when known.
    pub utc_ms: Option<u64>,
    /// `None` when there is no recent fix.
    pub own: Option<OwnFix>,
    pub config: Config,
    /// A flood of new drone identities, while its alarm is raised
    /// ([`probation`]); `"flood"` in `/api/state`.
    pub flood: Option<probation::FloodStatus>,
}

impl Context {
    pub fn state(&self, t: &Track) -> TrackState {
        if self.now_ms.saturating_sub(t.last_seen_ms) >= self.config.stale_after_ms {
            TrackState::Stale
        } else {
            TrackState::Active
        }
    }

    /// Active or stale, for an open link (same thresholds as drones).
    pub fn link_state(&self, t: &LinkTrack) -> TrackState {
        openlinks::link_state(t, self.now_ms, &self.config)
    }

    /// Active or stale, for an aircraft (its own, shorter thresholds).
    pub fn aircraft_state(&self, a: &AircraftTrack) -> TrackState {
        aircraft::aircraft_state(a, self.now_ms)
    }

    pub fn aircraft_geometry(&self, a: &AircraftTrack) -> AircraftGeometry {
        AircraftGeometry::of(a, self.own.as_ref())
    }

    pub fn geometry(&self, t: &Track) -> Geometry {
        Geometry::of(t, self.own.as_ref())
    }

    /// UTC (Unix ms) of hub time `ms`, when UTC is known.
    pub fn utc_of(&self, ms: u64) -> Option<u64> {
        self.utc_ms
            .map(|u| u.saturating_sub(self.now_ms.saturating_sub(ms)))
    }
}

/// What [`Tracker::ingest`] did with a detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ingested {
    /// The detection updated (or created) this track.
    Track(TrackId),
    /// Nothing decoded and no track uses that MAC; nothing to attach it to.
    Ignored,
}

impl Ingested {
    pub fn track(self) -> Option<TrackId> {
        match self {
            Ingested::Track(id) => Some(id),
            Ingested::Ignored => None,
        }
    }
}

/// The track table. `N` is the drone capacity (PLAN.md D7: 32 required, 64
/// goal); `A` the aircraft capacity (see [`aircraft`]); `P` how many drones
/// keep a path history for the threat levels ([`behaviour::path`], about
/// 4.4 KB each). The P4 hub, with the tracker in PSRAM, keeps one per
/// track (`P = N`); the default 0 (the S3 hub, the laptop viewer, most
/// tests) keeps none, and then only the place rules (1-4) can fire, on the
/// latest position alone.
pub struct Tracker<const N: usize = 64, const A: usize = { aircraft::MAX_AIRCRAFT }, const P: usize = 0> {
    tracks: Vec<Track, N>,
    /// Open FPV links, a separate table with its own ids.
    links: openlinks::LinkTable,
    /// ADS-B aircraft, a separate table keyed by ICAO address.
    aircraft: aircraft::AircraftTable<A>,
    /// Raw-capture RF emitters.
    emitters: signals::EmitterTable,
    /// Wi-Fi drones by maker table.
    wifi_drones: signals::WifiDroneTable,
    /// Per-node role and plan, from Hello and Status frames.
    nodes: signals::NodeTable,
    /// Attacks and jamming (protocol 9) and the hub's deaf-node check.
    threats: threats::ThreatTable,
    deaf: threats::DeafWatch,
    /// Drone / crewed-aircraft pairs already announced.
    conflict_watch: conflict::ConflictWatch,
    /// UTC and node positions for the spoofing checks.
    spoof_cfg: spoof::SpoofConfig,
    /// Identities heard while the table was full ([`probation`]).
    candidates: probation::Candidates<N>,
    /// New drone identities a second, for the flood alarm.
    flood: probation::FloodWatch,
    /// New drone identities a minute and drone identities active at once,
    /// for the flood alarm's slow rules.
    slow_flood: probation::SlowWatch,
    /// Unproven aircraft replaced a second, for the aircraft flood alarm.
    aircraft_flood: probation::FloodWatch,
    /// Per node: frames discarded and new IDs that never confirm, for the
    /// node-shed alarm ([`probation::ShedWatch`]).
    shed: probation::ShedWatch,
    /// Pairs of tracks frames said were one drone, waiting for more
    /// evidence ([`claims`]).
    merges: heapless::Vec<claims::PendingMerge, { claims::MAX_PENDING_MERGES }>,
    /// Frames with two different IDs of one type, applied to nothing.
    contradictory: u32,
    next_id: u32,
    own: Option<OwnFix>,
    /// The unit's own speed and course from consecutive fixes, when the
    /// receiver does not give them ([`behaviour::OwnMotion`]).
    own_motion: behaviour::OwnMotion,
    /// Areas, the bubble and friendly drones (site file; [`behaviour`]).
    places: behaviour::Places,
    /// Where the unit itself has been, one point every five seconds (the
    /// "following" rule; [`behaviour::OwnTrail`], 48 bytes a path ring).
    own_trail: behaviour::OwnTrail<P>,
    /// What the rules across drones and days keep (slice 16d,
    /// [`behaviour::across`]): spotters, drones that left an area, the
    /// repeat-visitor history. About 10 KB with path rings, nothing without.
    across: behaviour::Across<P>,
    /// Where each drone has been ([`behaviour::path`]): about 4.4 KB a
    /// track, the largest part of the tracker. Kept last.
    paths: behaviour::PathStore<P>,
    config: Config,
}

impl<const N: usize, const A: usize, const P: usize> Default for Tracker<N, A, P> {
    fn default() -> Self {
        Self::new(Config::default())
    }
}

impl<const N: usize, const A: usize, const P: usize> Tracker<N, A, P> {
    pub const fn new(config: Config) -> Self {
        Self {
            tracks: Vec::new(),
            links: openlinks::LinkTable::new(),
            aircraft: aircraft::AircraftTable::new(),
            emitters: signals::EmitterTable::new(),
            wifi_drones: signals::WifiDroneTable::new(),
            nodes: signals::NodeTable::new(),
            threats: threats::ThreatTable::new(),
            deaf: threats::DeafWatch::new(),
            conflict_watch: conflict::ConflictWatch::new(),
            spoof_cfg: spoof::SpoofConfig {
                utc_offset_ms: None,
                nodes: [None; spoof::MAX_NODE_POS],
            },
            candidates: probation::Candidates::new(),
            flood: probation::FloodWatch::new(),
            slow_flood: probation::SlowWatch::new(),
            aircraft_flood: probation::FloodWatch::new(),
            shed: probation::ShedWatch::new(),
            merges: heapless::Vec::new(),
            contradictory: 0,
            next_id: 1,
            own: None,
            own_motion: behaviour::OwnMotion::new(),
            places: behaviour::Places::new(),
            own_trail: behaviour::OwnTrail::new(),
            across: behaviour::Across::new(),
            paths: behaviour::PathStore::new(),
            config,
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Record the unit's GPS fix. `alt_m` is height above the WGS-84
    /// ellipsoid, `None` for a 2D fix. Its speed and direction are worked
    /// out from consecutive fixes (use [`Tracker::set_own_fix`] when the
    /// receiver gives them).
    pub fn set_own_position(&mut self, lat: f64, lon: f64, alt_m: Option<f64>, time_ms: u64) {
        self.set_own_fix(lat, lon, alt_m, time_ms, None);
    }

    /// As [`Tracker::set_own_position`], with the receiver's ground speed
    /// (m/s) and direction of travel (degrees true) when it gives them.
    /// A manual or held position passes `Some((0.0, None))`: not moving.
    pub fn set_own_fix(&mut self, lat: f64, lon: f64, alt_m: Option<f64>, time_ms: u64, motion: Option<(f32, Option<f32>)>) {
        let (speed_mps, course_deg) = match motion {
            Some((speed, course)) => self.own_motion.reported(lat, lon, time_ms, speed, course),
            None => self.own_motion.derived(lat, lon, time_ms),
        };
        self.own_trail.record(lat, lon, time_ms);
        self.own = Some(OwnFix {
            lat,
            lon,
            alt_m,
            fix_ms: time_ms,
            speed_mps,
            course_deg,
        });
    }

    /// Forget the fix (GPS lost lock). Geometry becomes `None` (P4).
    pub fn clear_own_position(&mut self) {
        self.own = None;
        self.own_motion = behaviour::OwnMotion::new();
    }

    /// The own fix, if not older than [`Config::own_fix_max_age_ms`].
    pub fn own_position(&self, now_ms: u64) -> Option<OwnFix> {
        self.own
            .filter(|f| now_ms.saturating_sub(f.fix_ms) <= self.config.own_fix_max_age_ms)
    }

    /// Snapshot for the formatters and geometry.
    pub fn context(&self, now_ms: u64, utc_ms: Option<u64>) -> Context {
        Context {
            now_ms,
            utc_ms,
            own: self.own_position(now_ms),
            config: self.config,
            flood: self.flood.status_drones(&self.slow_flood, now_ms),
        }
    }

    /// A flood of new drone identities, while its alarm is raised.
    pub fn flood_status(&self, now_ms: u64) -> Option<probation::FloodStatus> {
        self.flood.status_drones(&self.slow_flood, now_ms)
    }

    /// Tracks confirmed ([`Track::confirmed`]).
    pub fn confirmed_count(&self) -> usize {
        self.tracks.iter().filter(|t| t.confirmed()).count()
    }

    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    /// All tracks, in no particular order.
    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    pub fn get(&self, id: TrackId) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    /// Tracks heard within [`Config::stale_after_ms`].
    pub fn active_count(&self, now_ms: u64) -> usize {
        let ctx = self.context(now_ms, None);
        self.tracks
            .iter()
            .filter(|t| ctx.state(t) == TrackState::Active)
            .count()
    }

    /// The closest active drone with a known position (for the screen, O2),
    /// confirmed ones before unconfirmed ones (an unconfirmed track's
    /// position is the cheapest thing to forge). `None` without an own fix.
    pub fn nearest(&self, now_ms: u64) -> Option<(&Track, Geometry)> {
        let ctx = self.context(now_ms, None);
        self.tracks
            .iter()
            .filter(|t| ctx.state(t) == TrackState::Active)
            .filter_map(|t| {
                let g = ctx.geometry(t);
                g.drone.map(|d| (t, g, d.distance_m))
            })
            .min_by(|a, b| (!a.0.confirmed()).cmp(&!b.0.confirmed()).then(a.2.total_cmp(&b.2)))
            .map(|(t, g, _)| (t, g))
    }

    /// Feed one node detection: decode its raw messages with `rid` and merge.
    pub fn ingest(&mut self, node: u8, det: &link::Detection, now_ms: u64) -> Ingested {
        let obs = Observation::from_detection(det, node, now_ms);
        let mut msgs: Vec<Message, { rid::MAX_PACK }> = Vec::new();
        let mut errors = 0u32;
        let mut auth_errors = 0u8;
        for raw in &det.messages {
            match rid::message::decode(raw) {
                // Capacity equals the detection's, so this cannot overflow.
                Ok(m) => drop(msgs.push(m)),
                Err(_) => {
                    errors += 1;
                    if rid::message::message_kind(raw) == Some(rid::message::kind::AUTH) {
                        auth_errors += 1;
                    }
                }
            }
        }
        let r = self.ingest_decoded(&obs, &msgs, errors);
        if let Some(t) = r.track().and_then(|id| self.tracks.iter_mut().find(|t| t.id == id)) {
            spoof::auth_decode_errors(t, auth_errors);
        }
        r
    }

    /// Feed already-decoded messages, for sources that do not carry raw
    /// bytes (the laptop viewer reading a node's JSON lines, which hold
    /// `rid`-decoded messages). Merging is identical to [`Tracker::ingest`].
    /// `decode_errors` counts messages in the same frame that the node (or
    /// the caller) could not decode.
    pub fn ingest_messages(
        &mut self,
        obs: &Observation,
        msgs: &[Message],
        decode_errors: u32,
    ) -> Ingested {
        self.ingest_decoded(obs, msgs, decode_errors)
    }

    fn ingest_decoded(&mut self, obs: &Observation, msgs: &[Message], errors: u32) -> Ingested {
        use claims::{Outcome, Verdict};
        let now = obs.rx_ms;
        self.expire(now);

        // Every message in one frame is from the same transmitter.
        let mut ids: Vec<UasId, { rid::MAX_PACK }> = Vec::new();
        for m in msgs {
            if let Message::BasicId(b) = m {
                let id = UasId::of(b);
                if !ids.contains(&id) {
                    let _ = ids.push(id);
                }
            }
        }
        // Two different IDs of one type: F3411 allows one per type and no
        // honest drone sends two. Counted, applied to nothing (it would
        // rename or merge whatever it named).
        if ids.iter().enumerate().any(|(i, a)| ids[..i].iter().any(|b| b.id_type == a.id_type)) {
            self.contradictory = self.contradictory.saturating_add(1);
            for t in self.tracks.iter_mut().filter(|t| ids.iter().any(|id| t.has_id(id))) {
                t.claims.contradictory_frame();
            }
            return Ingested::Ignored;
        }
        let frame_loc = msgs.iter().find_map(|m| match m {
            Message::Location(l) => Some(l),
            _ => None,
        });
        let fpos = frame_loc.and_then(|l| l.position);
        let fhacc = frame_loc.and_then(|l| l.horizontal_accuracy.meters());

        let by_mac = self.find_by_mac(obs.transport, &obs.mac, fpos, fhacc, now);
        if msgs.is_empty() {
            // Nothing decodable: counted against the transmitter if known,
            // but it refreshes nothing (anyone can send garbage from that
            // address) and never starts a track (or an alert).
            return match by_mac {
                Some(i) => {
                    let t = &mut self.tracks[i];
                    t.decode_errors = t.decode_errors.saturating_add(errors);
                    Ingested::Track(t.id)
                }
                None => Ingested::Ignored,
            };
        }

        // Tracks that share an ID with this frame. It goes to the most
        // established (earliest first seen) whose path it fits, else the
        // most established.
        let mut by_id: Vec<usize, N> = Vec::new();
        for (i, t) in self.tracks.iter().enumerate() {
            if ids.iter().any(|id| t.has_id(id)) {
                let _ = by_id.push(i);
            }
        }
        let rank = |t: &Track| (t.first_seen_ms, t.id);
        let primary = by_id
            .iter()
            .copied()
            .filter(|&i| claims::judge(&self.tracks[i], fpos, fhacc, now) == Verdict::Apply)
            .min_by_key(|&i| rank(&self.tracks[i]))
            .or_else(|| by_id.iter().copied().min_by_key(|&i| rank(&self.tracks[i])))
            .or_else(|| by_mac.filter(|&i| self.tracks[i].compatible_with(&ids)));
        // A recently heard track on this address whose ID contradicts the
        // frame's: one transmitter, several IDs (see `spoof`).
        let mac_conflict = by_mac
            .filter(|&i| {
                !ids.is_empty()
                    && !self.tracks[i].compatible_with(&ids)
                    && now.saturating_sub(self.tracks[i].last_seen_ms) < spoof::SHARED_MAC_WINDOW_MS
            })
            .map(|i| self.tracks[i].id);

        let created = primary.is_none();
        let mut primary = match primary {
            Some(p) => p,
            None => {
                // A new identity: the transmitter address and the frame's
                // first ID (a fake with a fixed address and random IDs is
                // still one identity per ID).
                let medium = [u8::from(same_medium(obs.transport, Transport::WifiBeacon))];
                let key = match ids.first() {
                    Some(id) => probation::hash(&[&medium, &obs.mac, &[id.id_type.code()], id.id.as_bytes()]),
                    None => probation::hash(&[&medium, &obs.mac]),
                };
                match self.admit(key, obs.node, now) {
                    Some(i) => i,
                    // The table is busy and this identity has not earned a
                    // slot yet (see `probation`): remembered, not tracked.
                    None => return Ingested::Ignored,
                }
            }
        };
        let mut pid = self.tracks[primary].id;
        // The other tracks this frame names, and an ID-less track on its
        // address: never merged on this frame alone (`claims`).
        let mut others: Vec<(TrackId, bool), N> = Vec::new();
        for &i in by_id.iter().filter(|&&i| i != primary) {
            let fits = claims::judge(&self.tracks[i], fpos, fhacc, now) == Verdict::Apply;
            let _ = others.push((self.tracks[i].id, fits));
        }
        let mac_other = self
            .tracks
            .iter()
            .enumerate()
            .filter(|(i, t)| *i != primary && !by_id.contains(i) && t.heard_mac(obs.transport, &obs.mac))
            .max_by_key(|(_, t)| t.last_seen_ms)
            .map(|(_, t)| t.id);
        // IDs another track holds are not given to this one.
        let mut foreign: Vec<UasId, { rid::MAX_PACK }> = Vec::new();
        for id in &ids {
            if self.tracks.iter().any(|t| t.id != pid && t.has_id(id)) {
                let _ = foreign.push(*id);
            }
        }

        // Judge the frame against the track's established path.
        let mut verdict = if created {
            Verdict::Apply
        } else {
            claims::judge(&self.tracks[primary], fpos, fhacc, now)
        };
        if fpos.is_none() && !created {
            // No position: from the rival's own address, it is the rival's.
            let t = &self.tracks[primary];
            if t.claims.from_rival_address(&obs.mac, now) && !t.heard_mac(obs.transport, &obs.mac) {
                verdict = Verdict::Rival;
            }
        }
        // A claim on every other track it names whose path it does not fit.
        for &(o, fits) in &others {
            if !fits && let Some(i) = self.index_of(o) {
                let _ = self.rival_frame(i, obs, msgs, fpos, fhacc, now);
            }
        }
        if verdict == Verdict::Rival {
            match self.rival_frame(primary, obs, msgs, fpos, fhacc, now) {
                Outcome::Held => return Ingested::Track(pid),
                Outcome::Follow => {
                    if let Some(p) = fpos {
                        self.tracks[primary].follow(p, now);
                    }
                }
            }
        } else if fpos.is_some() && !created {
            self.tracks[primary].claims.original_report(now);
        }

        let env = self.spoof_env(now);
        spoof::before_frame(&mut self.tracks, primary, obs, msgs, mac_conflict, created, &env);
        let t = &mut self.tracks[primary];
        t.observe(obs);
        t.decode_errors = t.decode_errors.saturating_add(errors);
        for m in msgs {
            t.apply(m, now, &foreign);
        }
        if t.heard_at(now) {
            let (node, first) = (t.first_node, t.first_seen_ms);
            self.shed.confirmed(node, first, now);
        }
        spoof::after_frame(&mut self.tracks, primary, &env, now);

        // Evidence that the other tracks are this drone.
        for &(o, fits) in &others {
            if fits {
                pid = self.note_merge(pid, o, now);
            }
        }
        if let Some(m) = mac_other
            && let (Some(mi), Some(pi)) = (self.index_of(m), self.index_of(pid))
        {
            let (mt, pt) = (&self.tracks[mi], &self.tracks[pi]);
            let id_less = mt.uas_ids.is_empty() && mt.dji.serial.is_none() && mt.dji.key_id.is_none();
            if id_less && mt.compatible_with(&ids) && claims::same_place(mt, pt) {
                // An ID-less track (a Bluetooth 4 advert carries one message
                // at a time) whose own address now sends this ID, from
                // where it is: the same drone.
                primary = self.merge_into(pi, core::iter::once(mi), &ids);
                pid = self.tracks[primary].id;
            } else if mt.compatible_with(&pt.uas_ids) && mt.compatible_with(&ids) {
                pid = self.note_merge(pid, m, now);
            }
        }
        Ingested::Track(pid)
    }

    fn index_of(&self, id: TrackId) -> Option<usize> {
        self.tracks.iter().position(|t| t.id == id)
    }

    /// A frame (or report) that is a rival claim on `tracks[i]`: held, its
    /// differing text fields noted. Returns whether the track should
    /// follow it ([`claims`]).
    fn rival_frame(
        &mut self,
        i: usize,
        obs: &Observation,
        msgs: &[Message],
        fpos: Option<LatLon>,
        fhacc: Option<f32>,
        now: u64,
    ) -> claims::Outcome {
        let t = &mut self.tracks[i];
        let mut out = claims::Outcome::Held;
        if let Some(p) = fpos {
            out = t.rival_claim(p, fhacc, ClaimSrc::RemoteId, obs.mac, now);
        }
        for m in msgs {
            match m {
                Message::SelfId(s) if t.self_id.as_ref().is_some_and(|e| e.value.description != s.description) => {
                    t.claims.rival_text_differs(claims::KIND_SELF_ID, now)
                }
                Message::OperatorId(o) if t.operator_id.as_ref().is_some_and(|e| e.value.operator_id != o.operator_id) => {
                    t.claims.rival_text_differs(claims::KIND_OPERATOR_ID, now)
                }
                Message::BasicId(b) if t.uas_ids.iter().any(|u| u.id_type == b.id_type && u.id != b.uas_id) => {
                    t.claims.rival_text_differs(claims::kind_id(b.id_type.code()), now)
                }
                _ => {}
            }
        }
        out
    }

    /// One more frame saying tracks `a` and `b` are one drone. Merges them
    /// (into the more established) once [`claims::merge_evidence`] has
    /// enough, their positions agree and neither is disputed. Returns the
    /// id `a`'s data now lives under.
    pub(crate) fn note_merge(&mut self, a: TrackId, b: TrackId, now: u64) -> TrackId {
        let tracks = &self.tracks;
        let live = |id: TrackId| tracks.iter().any(|t| t.id == id);
        self.merges.retain(|m| live(m.a) && live(m.b));
        if !claims::merge_evidence(&mut self.merges, a, b, now) {
            return a;
        }
        let (Some(i), Some(j)) = (self.index_of(a), self.index_of(b)) else {
            return a;
        };
        let (ti, tj) = (&self.tracks[i], &self.tracks[j]);
        if !claims::same_place(ti, tj)
            || ti.claims.disputed(now)
            || tj.claims.disputed(now)
            || !ti.compatible_with(&tj.uas_ids)
        {
            return a;
        }
        self.merges.retain(|m| !((m.a == a && m.b == b) || (m.a == b && m.b == a)));
        let (keep, fold) = if (ti.first_seen_ms, ti.id) <= (tj.first_seen_ms, tj.id) { (i, j) } else { (j, i) };
        let k = self.merge_into(keep, core::iter::once(fold), &[]);
        self.tracks[k].id
    }

    /// The next dispute to log (evidence event `IdDisputed`), once per
    /// dispute and again when it grows (a text field joins it). Call after
    /// ingesting, and ~1 Hz.
    pub fn take_dispute(&mut self, now_ms: u64) -> Option<DisputeEvent> {
        for t in self.tracks.iter_mut() {
            if t.claims.take_new(now_ms).is_some()
                && let Some(d) = t.claims.dispute(now_ms)
            {
                return Some(DisputeEvent { track: t.id, dispute: d, apart_m: claims::apart_m(t, now_ms) });
            }
        }
        None
    }

    /// Tracks disputed right now.
    pub fn disputed_count(&self, now_ms: u64) -> usize {
        self.tracks.iter().filter(|t| t.claims.disputed(now_ms)).count()
    }

    /// Frames with two different IDs of one type since boot (applied to
    /// nothing).
    pub fn contradictory_frames(&self) -> u32 {
        self.contradictory
    }

    /// The most recently heard track whose last MAC on this medium is `mac`;
    /// when several are and the frame has a position, the one whose path
    /// it fits (another drone's address in one forged frame must not pull
    /// that drone's later frames onto the forged track).
    fn find_by_mac(
        &self,
        transport: Transport,
        mac: &[u8; 6],
        pos: Option<LatLon>,
        hacc: Option<f32>,
        now: u64,
    ) -> Option<usize> {
        let cands = || self.tracks.iter().enumerate().filter(|(_, t)| t.heard_mac(transport, mac));
        if pos.is_some()
            && let Some((i, _)) = cands()
                .filter(|(_, t)| {
                    claims::established(t, now).is_some() && claims::judge(t, pos, hacc, now) == claims::Verdict::Apply
                })
                .max_by_key(|(_, t)| t.last_seen_ms)
        {
            return Some(i);
        }
        cands().max_by_key(|(_, t)| t.last_seen_ms).map(|(i, _)| i)
    }

    /// Fold every candidate that is the same drone into `primary`. A
    /// candidate whose IDs contradict the primary's or the frame's (a MAC
    /// reused by a different drone) is left alone. Returns primary's new index.
    fn merge_into(
        &mut self,
        primary: usize,
        candidates: impl Iterator<Item = usize>,
        ids: &[UasId],
    ) -> usize {
        let mut doomed: Vec<usize, N> = Vec::new();
        for c in candidates {
            if c == primary || doomed.contains(&c) {
                continue;
            }
            let cand = &self.tracks[c];
            if cand.compatible_with(&self.tracks[primary].uas_ids) && cand.compatible_with(ids) {
                let _ = doomed.push(c);
            }
        }
        // Remove from the highest index down so the lower ones stay valid.
        doomed.sort_unstable_by(|a, b| b.cmp(a));
        let mut primary = primary;
        for c in doomed {
            let other = self.tracks.remove(c);
            if c < primary {
                primary -= 1;
            }
            self.tracks[primary].absorb(other);
        }
        primary
    }

    /// A track for a new identity `key` (a [`probation::hash`]), or `None`
    /// when the table is busy and it has not earned a slot yet. See
    /// [`probation`] for the rule:
    /// - a free slot, or a stale track (not heard for `stale_after_ms`):
    ///   taken at once;
    /// - otherwise the identity must have been heard again (the bar rises
    ///   as the admission bucket drains), and then it replaces the weakest
    ///   unconfirmed track (fewest frames, then heard longest ago), or a
    ///   confirmed one not heard for [`probation::QUIET_EVICT_MS`], never
    ///   another confirmed one.
    ///
    /// The new track's history starts at the identity's first sighting.
    ///
    /// `node` delivered the frame (for [`probation::ShedWatch`];
    /// [`threats::NODE_ANY`] when no node did).
    pub(crate) fn admit(&mut self, key: u32, node: u8, now_ms: u64) -> Option<usize> {
        let s = self.candidates.sight(key, now_ms);
        if s.new {
            self.flood.note(now_ms);
            self.slow_flood.note(now_ms);
            self.shed.new_id(node, now_ms);
        }
        let stale_after = self.config.stale_after_ms;
        let victim = if !self.tracks.is_full() {
            None
        } else if let Some((i, _)) = self
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| now_ms.saturating_sub(t.last_seen_ms) >= stale_after)
            .min_by_key(|(_, t)| t.last_seen_ms)
        {
            Some(i)
        } else {
            let earned = self.candidates.bar(now_ms).is_some_and(|bar| s.count >= bar);
            let weakest = || {
                self.tracks
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| !t.confirmed())
                    .min_by_key(|(_, t)| (t.heard, t.last_seen_ms))
                    .or_else(|| {
                        self.tracks
                            .iter()
                            .enumerate()
                            .filter(|(_, t)| now_ms.saturating_sub(t.last_seen_ms) >= probation::QUIET_EVICT_MS)
                            .min_by_key(|(_, t)| t.last_seen_ms)
                    })
                    .map(|(i, _)| i)
            };
            match earned.then(weakest).flatten() {
                Some(i) => {
                    self.candidates.spend();
                    Some(i)
                }
                None => {
                    if s.new {
                        self.flood.refused = self.flood.refused.saturating_add(1);
                    }
                    return None;
                }
            }
        };
        if let Some(i) = victim {
            self.tracks.remove(i);
        }
        self.candidates.forget(key);
        let id = TrackId(self.next_id);
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let mut t = Track::new(id, s.first_ms.min(now_ms));
        t.last_seen_ms = now_ms;
        // The sightings that earned it the slot count towards its standing;
        // the caller counts this frame.
        t.heard = s.count.saturating_sub(1);
        t.first_node = node;
        // Cannot fail: there was room, or a victim was just removed.
        self.tracks.push(t).ok()?;
        Some(self.tracks.len() - 1)
    }

    /// Tests about something other than standing: every drone track held
    /// is confirmed and every aircraft established, as if heard for long
    /// enough.
    #[cfg(test)]
    pub(crate) fn vouch_for_tests(&mut self, now_ms: u64) {
        for t in self.tracks.iter_mut() {
            t.confirmed_ms.get_or_insert(now_ms);
        }
        self.aircraft.vouch_for_tests();
    }

    /// Count one frame (or DJI / control-link report) for track `i` towards
    /// its standing ([`probation`]).
    pub(crate) fn heard(&mut self, i: usize, now_ms: u64) {
        if let Some(t) = self.tracks.get_mut(i)
            && t.heard_at(now_ms)
        {
            let (node, first) = (t.first_node, t.first_seen_ms);
            self.shed.confirmed(node, first, now_ms);
        }
    }

    /// Feed one open-link sighting from `node` (see [`openlinks`]). Kept
    /// apart from drones: it never creates, merges or alerts on a drone track.
    pub fn ingest_open_link(
        &mut self,
        node: u8,
        sighting: &link::OpenLinkSighting,
        now_ms: u64,
    ) -> LinkTrackId {
        self.links.ingest(node, sighting, now_ms, &self.config)
    }

    /// Open links, in table order (the order `json::state` lists them).
    pub fn open_links(&self) -> &[LinkTrack] {
        self.links.tracks()
    }

    pub fn get_open_link(&self, id: LinkTrackId) -> Option<&LinkTrack> {
        self.links.get(id)
    }

    /// Open links heard within [`Config::stale_after_ms`].
    pub fn active_link_count(&self, now_ms: u64) -> usize {
        self.links.active_count(now_ms, &self.config)
    }

    /// Feed one aircraft report from the ADSBee (see [`aircraft`]). Kept
    /// apart from drones: it never creates, merges or alerts on a drone
    /// track. `None` if the aircraft was not added (not currently heard, or
    /// the table is full of nearer ones).
    pub fn ingest_aircraft(&mut self, report: &adsb::Aircraft, now_ms: u64) -> Option<AircraftKey> {
        let own = self.own_position(now_ms);
        let before = self.aircraft.replaced();
        let r = self.aircraft.ingest(report, now_ms, own.as_ref());
        for _ in before..self.aircraft.replaced() {
            self.aircraft_flood.note(now_ms);
        }
        r
    }

    /// Aircraft, in table order (the order `json::state` lists them).
    pub fn aircraft(&self) -> &[AircraftTrack] {
        self.aircraft.all()
    }

    pub fn get_aircraft(&self, key: AircraftKey) -> Option<&AircraftTrack> {
        self.aircraft.get(key)
    }

    /// Drones close to crewed aircraft, most serious first (see
    /// [`conflict`]). Only confirmed drone tracks and established aircraft
    /// take part ([`probation`]): a single forged frame of either cannot
    /// raise the warning.
    pub fn conflicts(&self, now_ms: u64) -> heapless::Vec<conflict::Conflict, { conflict::MAX_CONFLICTS }> {
        let ctx = self.context(now_ms, None);
        conflict::find(self.tracks.iter(), self.aircraft.all(), &ctx)
    }

    /// The next drone / aircraft pair to announce (buzzer, screen, log):
    /// once per pair and level, again after [`conflict::REALERT_MS`].
    pub fn take_new_conflict(&mut self, now_ms: u64) -> Option<conflict::Conflict> {
        let current = self.conflicts(now_ms);
        self.conflict_watch.take_new(&current, now_ms)
    }

    /// Aircraft heard within [`aircraft::AIRCRAFT_STALE_MS`].
    pub fn active_aircraft_count(&self, now_ms: u64) -> usize {
        self.aircraft.active_count(now_ms)
    }

    /// Drop drone tracks and open links not heard for
    /// [`Config::remove_after_ms`], and aircraft not heard for
    /// [`aircraft::AIRCRAFT_REMOVE_MS`]. Called by `ingest` too; call it
    /// periodically so an idle table still empties. Returns the number of
    /// *drone* tracks removed.
    pub fn expire(&mut self, now_ms: u64) -> usize {
        self.links.expire(now_ms, &self.config);
        self.aircraft.expire(now_ms);
        self.emitters.expire(now_ms, &self.config);
        self.wifi_drones.expire(now_ms, &self.config);
        self.threats.expire(now_ms, self.config.remove_after_ms);
        if let Some(s) =
            self.aircraft_flood.check(now_ms, probation::AIRCRAFT_FLOOD_PER_S, probation::FloodTable::Aircraft)
        {
            let _ = self.threats.ingest(0, true, &s, now_ms);
        }
        let threats = &mut self.threats;
        self.shed.check(now_ms, |node, s| {
            let _ = threats.ingest(node, true, &s, now_ms);
        });
        let before = self.tracks.len();
        let limit = self.config.remove_after_ms;
        // Unconfirmed tracks leave sooner (`probation`).
        let unconfirmed = probation::UNCONFIRMED_REMOVE_MS.min(limit);
        self.tracks.retain(|t| {
            now_ms.saturating_sub(t.last_seen_ms) < if t.confirmed() { limit } else { unconfirmed }
        });
        // The drone flood alarm, after the table is pruned: its population
        // rule counts what is left (one track is one drone identity,
        // however many addresses or media it uses).
        let tracks = &self.tracks;
        let active = || {
            tracks.iter().filter(|t| now_ms.saturating_sub(t.last_seen_ms) < probation::ACTIVE_HEARD_MS).count() as u32
        };
        if let Some(s) = self.flood.check_drones(&mut self.slow_flood, now_ms, active) {
            let _ = self.threats.ingest(0, true, &s, now_ms);
        }
        before - self.tracks.len()
    }

    // ------------------------------------------------- threat levels --

    /// Areas, the bubble and friendly drones in force ([`behaviour`]).
    pub fn places(&self) -> &behaviour::Places {
        &self.places
    }

    /// Change them (a site-file line typed on the hub).
    pub fn places_mut(&mut self) -> &mut behaviour::Places {
        &mut self.places
    }

    /// Replace them (the site file read at mount), copied in place (no
    /// second ~2 KB copy on the stack).
    pub fn set_places(&mut self, p: &behaviour::Places) {
        self.places.copy_from(p);
    }

    /// The path history of a track ([`behaviour::path`]).
    pub fn path(&self, id: TrackId) -> Option<&behaviour::PathRing> {
        self.paths.get(id)
    }

    /// Once a second: add each drone's newest position to its path
    /// history, then judge every drone against the places
    /// ([`behaviour::evaluate`]) and move its level ([`behaviour::TrackThreat`]).
    pub fn tick_behaviour(&mut self, now_ms: u64) {
        let own = self.own_position(now_ms);
        let tracks = &self.tracks;
        self.paths.retain(|id| tracks.iter().any(|t| t.id == id));
        for t in self.tracks.iter() {
            if let Some(l) = &t.location
                && let Some(p) = l.value.position
                && now_ms.saturating_sub(l.rx_ms) <= behaviour::FRESH_MS
            {
                self.paths.record(t.id, p.lat, p.lon, l.value.altitude_geodetic, l.rx_ms);
            }
            // The pilot's trail and the serials (slice 16d).
            if let Some(w) = self.paths.watch_mut(t.id) {
                w.note(t, now_ms);
            }
        }
        // UTC day (repeat visitors) and the alarms that make any drone at a
        // place a warning (several at once), for the rules across drones.
        let today = self
            .spoof_cfg
            .utc_offset_ms
            .map(|o| behaviour::visits::day_of((now_ms as i64).saturating_add(o).max(0) as u64));
        let alarm = self.level_alarm(now_ms);
        self.across.begin_tick(&self.places, today, now_ms);
        let mut cross = behaviour::Cross { across: &mut self.across, alarm, today };
        for t in self.tracks.iter_mut() {
            let e = behaviour::evaluate(
                t,
                self.paths.get(t.id),
                &self.places,
                own.as_ref(),
                Some(&self.own_trail),
                Some(&mut cross),
                now_ms,
            );
            let has_position = t.location.as_ref().is_some_and(|l| l.value.position.is_some());
            t.threat.update(e, has_position, now_ms);
        }
    }

    /// Rule 15's alarms, read from the threat table (never changed here):
    /// jamming or a blinded node ([`behaviour::across::ALARM_JAMMING`]), else
    /// GPS interference in the area ([`behaviour::across::ALARM_GNSS`]); 0
    /// none. Muted or snoozed alarms still count: the jamming is still there.
    fn level_alarm(&self, now_ms: u64) -> u8 {
        let mut alarm = 0;
        for t in self.threats.all().iter().filter(|t| t.active(now_ms)) {
            if t.kind.is_jamming() {
                return behaviour::across::ALARM_JAMMING;
            }
            if t.kind == link::ThreatKind::AreaGnss {
                alarm = behaviour::across::ALARM_GNSS;
            }
        }
        alarm
    }

    /// The repeat-visitor history (rule 17, [`behaviour::visits`]).
    pub fn visits(&self) -> &behaviour::Visits<P> {
        self.across.visits()
    }

    /// Change it: the hub reads it from its card, wipes it, and writes it
    /// when it changed ([`behaviour::Visits::take_dirty`]).
    pub fn visits_mut(&mut self) -> &mut behaviour::Visits<P> {
        &mut self.across.visits
    }

    /// The evidence and console text for a drone's level
    /// ([`behaviour::describe`]), naming the other drones of rules 14 and
    /// 19 by their IDs (as heard over the air: the hub cleans the text).
    pub fn describe_level(&self, id: TrackId, now_ms: u64) -> Option<heapless::String<256>> {
        let t = self.get(id)?;
        Some(behaviour::describe_with(t, now_ms, |o| self.get(o).and_then(|o| o.primary_id()).and_then(|u| u.id.as_str())))
    }

    /// The next drone whose level rose to Caution or above, for the
    /// evidence log (code `threat_level`) and the console; once per rise.
    pub fn take_level_rise(&mut self, now_ms: u64) -> Option<behaviour::Rise> {
        let t = self.tracks.iter_mut().find(|t| t.threat.rise_pending())?;
        t.threat.take_rise();
        Some(behaviour::Rise {
            track: t.id,
            level: t.threat.level.unwrap_or(behaviour::Level::Notice),
            snoozed: t.threat.snoozed(now_ms),
        })
    }

    /// The most serious drone that is not snoozed, at Caution or above
    /// (the screen's strip), nearest first among equals.
    pub fn top_threat(&self, now_ms: u64) -> Option<&Track> {
        self.tracks
            .iter()
            .filter(|t| t.threat.level >= Some(behaviour::Level::Caution) && !t.threat.snoozed(now_ms))
            .max_by(|a, b| {
                let range = |t: &Track| t.threat.reasons.first().map_or(f32::MAX, |r| r.range_m());
                a.threat.level.cmp(&b.threat.level).then(range(b).total_cmp(&range(a)))
            })
    }

    /// Drones at Warning or above whose alerts are not snoozed (what the
    /// screen's Snooze would act on).
    pub fn snoozable_levels(&self, now_ms: u64) -> usize {
        self.tracks
            .iter()
            .filter(|t| t.threat.level >= Some(behaviour::Level::Warning) && !t.threat.snoozed(now_ms))
            .count()
    }

    /// Snooze one drone's level alerts for [`behaviour::SNOOZE_MS`] (RAM
    /// only; the level is still shown and logged). False when there is no
    /// such drone or it is below Caution.
    pub fn snooze_drone(&mut self, id: TrackId, now_ms: u64) -> bool {
        match self.tracks.iter_mut().find(|t| t.id == id) {
            Some(t) if t.threat.level >= Some(behaviour::Level::Caution) => {
                t.threat.snooze(now_ms);
                true
            }
            _ => false,
        }
    }

    /// The screen's Snooze: every drone at Warning or above. Returns them
    /// (for the evidence log).
    pub fn snooze_levels(&mut self, now_ms: u64) -> Vec<TrackId, N> {
        let mut out = Vec::new();
        for t in self.tracks.iter_mut() {
            if t.threat.level >= Some(behaviour::Level::Warning) && !t.threat.snoozed(now_ms) {
                t.threat.snooze(now_ms);
                let _ = out.push(t.id);
            }
        }
        out
    }

    /// The next drone to alert on (buzzer, screen), exactly once per track,
    /// when it is **confirmed** ([`probation`]: heard again at least a
    /// second after it was first heard). Call after ingesting, and
    /// periodically (a track without a Basic ID also waits
    /// [`Config::announce_grace_ms`] after it was first heard).
    pub fn take_new_drone(&mut self, now_ms: u64) -> Option<TrackId> {
        let grace = self.config.announce_grace_ms;
        let t = self.tracks.iter_mut().find(|t| {
            !t.announced
                && t.confirmed()
                && (!t.uas_ids.is_empty() || now_ms.saturating_sub(t.first_seen_ms) >= grace)
        })?;
        t.announced = true;
        Some(t.id)
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_aircraft;
#[cfg(test)]
mod tests_area_gnss;
#[cfg(test)]
mod tests_claims;
#[cfg(test)]
mod tests_conflict;
#[cfg(test)]
mod tests_deaf;
#[cfg(test)]
mod tests_control;
#[cfg(test)]
mod tests_flood;
#[cfg(test)]
mod tests_mutes;
#[cfg(test)]
mod tests_openlinks;
#[cfg(test)]
mod tests_signals;
#[cfg(test)]
mod tests_spoof;
