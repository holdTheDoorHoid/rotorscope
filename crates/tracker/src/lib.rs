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
//! # Time
//! All `*_ms` times are hub monotonic milliseconds (uptime), supplied by the
//! caller. Wall-clock UTC is only needed by the CoT and CSV formatters and is
//! passed in separately, because the hub may not have GPS time.
#![no_std]

pub mod cot;
pub mod csv;
pub mod geo;
pub mod json;
mod text;

use heapless::Vec;
use rid::Transport;
use rid::message::{
    BasicId, FixedStr, IdType, Location, Message, OperatorId, SelfId, System, UaType,
};

pub use geo::RangeBearing;
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

/// Index of `t` in [`TRANSPORTS`] and [`Track::links`].
pub fn transport_index(t: Transport) -> usize {
    match t {
        Transport::WifiBeacon => 0,
        Transport::WifiNan => 1,
        Transport::BleLegacy => 2,
        Transport::BleExtended => 3,
        Transport::BleLongRange => 4,
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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            stale_after_ms: 60_000,
            remove_after_ms: 600_000,
            announce_grace_ms: 1_000,
            own_fix_max_age_ms: 10_000,
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

/// Reception statistics for one transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LinkStats {
    /// Frames/adverts heard. Zero means this transport was never heard.
    pub count: u32,
    pub last_seen_ms: u64,
    pub rssi: i8,
    pub counter: u8,
    pub channel: u8,
    /// Most recent transmitter address on this transport.
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
    /// Per transport, indexed by [`transport_index`].
    pub links: [LinkStats; 5],
    /// Bit `n` set when node `n` (0-7) has heard this drone.
    pub nodes: u8,
    pub first_seen_ms: u64,
    pub last_seen_ms: u64,
    /// Messages in this drone's frames that failed to decode.
    pub decode_errors: u32,
    announced: bool,
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
            links: [LinkStats::default(); 5],
            nodes: 0,
            first_seen_ms: now_ms,
            last_seen_ms: now_ms,
            decode_errors: 0,
            announced: false,
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

    pub fn link(&self, t: Transport) -> &LinkStats {
        &self.links[transport_index(t)]
    }

    /// Transports this drone has ever been heard on.
    pub fn transports(&self) -> impl Iterator<Item = Transport> + '_ {
        TRANSPORTS.into_iter().filter(|t| self.link(*t).heard())
    }

    /// The most recently heard transport's stats (for "the" RSSI and MAC).
    pub fn latest_link(&self) -> Option<(Transport, &LinkStats)> {
        TRANSPORTS
            .iter()
            .map(|t| (*t, self.link(*t)))
            .filter(|(_, l)| l.heard())
            .max_by_key(|(_, l)| l.last_seen_ms)
    }

    /// True once the "new drone" event has been handed out for this track.
    pub fn announced(&self) -> bool {
        self.announced
    }

    /// No two IDs of the same type disagree, so `other` may be the same drone.
    fn compatible_with(&self, ids: &[UasId]) -> bool {
        ids.iter().all(|n| {
            self.uas_ids
                .iter()
                .all(|o| o.id_type != n.id_type || o.id == n.id)
        })
    }

    fn has_id(&self, id: &UasId) -> bool {
        self.uas_ids.contains(id)
    }

    /// The most recent MAC this track used on `t`'s medium, if it matches.
    fn heard_mac(&self, t: Transport, mac: &[u8; 6]) -> bool {
        TRANSPORTS
            .iter()
            .any(|o| same_medium(*o, t) && self.link(*o).heard() && self.link(*o).mac == *mac)
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
        let l = &mut self.links[transport_index(obs.transport)];
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

    fn apply(&mut self, msg: &Message, rx_ms: u64) {
        fn put<T: Clone>(slot: &mut Option<Stamped<T>>, v: &T, rx_ms: u64) {
            *slot = Some(Stamped {
                value: v.clone(),
                rx_ms,
            });
        }
        match msg {
            Message::BasicId(b) => {
                self.add_id(UasId::of(b));
                if b.ua_type != UaType::None || self.ua_type.is_none() {
                    self.ua_type = Some(b.ua_type);
                }
            }
            Message::Location(l) => put(&mut self.location, l, rx_ms),
            Message::System(s) => put(&mut self.system, s, rx_ms),
            Message::SelfId(s) => put(&mut self.self_id, s, rx_ms),
            Message::OperatorId(o) => put(&mut self.operator_id, o, rx_ms),
            // Signatures are not verified on the hub; nothing to show yet.
            Message::Auth(_) => {}
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
        newer(&mut self.location, other.location);
        newer(&mut self.system, other.system);
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
        // Whoever was alerted on already, the drone has been announced.
        self.announced |= other.announced;
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
}

impl Context {
    pub fn state(&self, t: &Track) -> TrackState {
        if self.now_ms.saturating_sub(t.last_seen_ms) >= self.config.stale_after_ms {
            TrackState::Stale
        } else {
            TrackState::Active
        }
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

/// The track table. `N` is the capacity (PLAN.md D7: 32 required, 64 goal).
pub struct Tracker<const N: usize = 64> {
    tracks: Vec<Track, N>,
    next_id: u32,
    own: Option<OwnFix>,
    config: Config,
}

impl<const N: usize> Default for Tracker<N> {
    fn default() -> Self {
        Self::new(Config::default())
    }
}

impl<const N: usize> Tracker<N> {
    pub const fn new(config: Config) -> Self {
        Self {
            tracks: Vec::new(),
            next_id: 1,
            own: None,
            config,
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Record the unit's GPS fix. `alt_m` is height above the WGS-84
    /// ellipsoid, `None` for a 2D fix.
    pub fn set_own_position(&mut self, lat: f64, lon: f64, alt_m: Option<f64>, time_ms: u64) {
        self.own = Some(OwnFix {
            lat,
            lon,
            alt_m,
            fix_ms: time_ms,
        });
    }

    /// Forget the fix (GPS lost lock). Geometry becomes `None` (P4).
    pub fn clear_own_position(&mut self) {
        self.own = None;
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
        }
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

    /// The closest active drone with a known position (for the screen, O2).
    /// `None` without an own fix.
    pub fn nearest(&self, now_ms: u64) -> Option<(&Track, Geometry)> {
        let ctx = self.context(now_ms, None);
        self.tracks
            .iter()
            .filter(|t| ctx.state(t) == TrackState::Active)
            .filter_map(|t| {
                let g = ctx.geometry(t);
                g.drone.map(|d| (t, g, d.distance_m))
            })
            .min_by(|a, b| a.2.total_cmp(&b.2))
            .map(|(t, g, _)| (t, g))
    }

    /// Feed one node detection: decode its raw messages with `rid` and merge.
    pub fn ingest(&mut self, node: u8, det: &link::Detection, now_ms: u64) -> Ingested {
        let obs = Observation::from_detection(det, node, now_ms);
        let mut msgs: Vec<Message, { rid::MAX_PACK }> = Vec::new();
        let mut errors = 0u32;
        for raw in &det.messages {
            match rid::message::decode(raw) {
                // Capacity equals the detection's, so this cannot overflow.
                Ok(m) => drop(msgs.push(m)),
                Err(_) => errors += 1,
            }
        }
        self.ingest_decoded(&obs, &msgs, errors)
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

        let by_mac = self.find_by_mac(obs.transport, &obs.mac);
        if msgs.is_empty() {
            // Nothing decodable: count it against the transmitter if known,
            // but never start a track (and an alert) on garbage.
            return match by_mac {
                Some(i) => {
                    let t = &mut self.tracks[i];
                    t.observe(obs);
                    t.decode_errors = t.decode_errors.saturating_add(errors);
                    Ingested::Track(t.id)
                }
                None => Ingested::Ignored,
            };
        }

        // Tracks that share an ID with this frame. The most established
        // (earliest first seen) becomes the survivor.
        let mut by_id: Vec<usize, N> = Vec::new();
        for (i, t) in self.tracks.iter().enumerate() {
            if ids.iter().any(|id| t.has_id(id)) {
                let _ = by_id.push(i);
            }
        }
        let primary = by_id
            .iter()
            .copied()
            .min_by_key(|&i| (self.tracks[i].first_seen_ms, self.tracks[i].id))
            .or_else(|| by_mac.filter(|&i| self.tracks[i].compatible_with(&ids)));

        let primary = match primary {
            Some(p) => self.merge_into(p, by_id.iter().copied().chain(by_mac), &ids),
            None => self.create(now),
        };

        let t = &mut self.tracks[primary];
        t.observe(obs);
        t.decode_errors = t.decode_errors.saturating_add(errors);
        for m in msgs {
            t.apply(m, now);
        }
        Ingested::Track(t.id)
    }

    /// The most recently heard track whose last MAC on this medium is `mac`.
    fn find_by_mac(&self, transport: Transport, mac: &[u8; 6]) -> Option<usize> {
        self.tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.heard_mac(transport, mac))
            .max_by_key(|(_, t)| t.last_seen_ms)
            .map(|(i, _)| i)
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

    fn create(&mut self, now_ms: u64) -> usize {
        if self.tracks.is_full() {
            self.evict(now_ms);
        }
        let id = TrackId(self.next_id);
        self.next_id = self.next_id.wrapping_add(1).max(1);
        // Cannot fail: evict() just made room (N > 0).
        let _ = self.tracks.push(Track::new(id, now_ms));
        self.tracks.len() - 1
    }

    /// Make room: the oldest stale track if there is one, else the
    /// least-recently-heard. A full table of active drones is the worst
    /// case, and then the quietest one is the least useful to keep.
    fn evict(&mut self, now_ms: u64) {
        let ctx = self.context(now_ms, None);
        let victim = self
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| ctx.state(t) == TrackState::Stale)
            .min_by_key(|(_, t)| t.last_seen_ms)
            .or_else(|| {
                self.tracks
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, t)| t.last_seen_ms)
            })
            .map(|(i, _)| i);
        if let Some(i) = victim {
            self.tracks.remove(i);
        }
    }

    /// Drop tracks not heard for [`Config::remove_after_ms`]. Called by
    /// `ingest` too; call it periodically so an idle table still empties.
    pub fn expire(&mut self, now_ms: u64) -> usize {
        let before = self.tracks.len();
        let limit = self.config.remove_after_ms;
        self.tracks
            .retain(|t| now_ms.saturating_sub(t.last_seen_ms) < limit);
        before - self.tracks.len()
    }

    /// The next drone to alert on (buzzer, screen), exactly once per track.
    /// Call after ingesting, and periodically (a track without a Basic ID
    /// becomes due [`Config::announce_grace_ms`] after it was first heard).
    pub fn take_new_drone(&mut self, now_ms: u64) -> Option<TrackId> {
        let grace = self.config.announce_grace_ms;
        let t = self.tracks.iter_mut().find(|t| {
            !t.announced
                && (!t.uas_ids.is_empty() || now_ms.saturating_sub(t.first_seen_ms) >= grace)
        })?;
        t.announced = true;
        Some(t.id)
    }
}

#[cfg(test)]
mod tests;
