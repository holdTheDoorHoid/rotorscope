//! Signals that are not (yet) a drone track: raw-capture RF emitters, Wi-Fi
//! drones recognised by maker, and what each node says it is doing.
//!
//! None has a position. Both tables age exactly like open links
//! ([`crate::Config::stale_after_ms`], [`crate::Config::remove_after_ms`])
//! and make room by dropping the entry heard least recently.
//!
//! **Alerts.** [`crate::Tracker::take_new_signal`] hands out each entry
//! once, like `take_new_drone`, but only for what is almost certainly a
//! drone: confirmed analog video, HDZero, LTE-style and narrow Wi-Fi links,
//! the DJI link fingerprints (`crates/djilink`), and Wi-Fi drones. Plain [`RfKind::Energy`] and an unconfirmed
//! [`RfKind::FpvCarrier`] (a Wi-Fi access point on an FPV channel looks the
//! same) never alert.
//!
//! **Privacy.** A Wi-Fi drone's transmitter address is shown only with
//! [`crate::Config::show_link_ids`], as for open links; its network name is
//! what any phone lists, and is shown.

use heapless::Vec;
use link::{NodeRole, RfKind, RfSighting, WifiDroneSighting};

use crate::{Band, Config, TrackState, Tracker};

/// RF emitters held. A band scan rarely finds more than a handful.
pub const MAX_EMITTERS: usize = 16;
/// Wi-Fi drones held.
pub const MAX_WIFI_DRONES: usize = 16;
/// Nodes whose Status is kept.
pub const MAX_NODES: usize = 16;

/// Two sightings of one kind closer than this (100 kHz units, so 2 MHz) are
/// the same emitter, or half the occupied width if that is wider.
pub const SAME_EMITTER: u32 = 20;

/// A hopping controller moves across this much (100 kHz units, 150 MHz:
/// more than DJI's 5.8 GHz hop set, less than the gap between bands) and is
/// still one emitter.
pub const HOP_SPAN: u32 = 1500;

/// Whether a kind is worth a "new signal" alert.
pub const fn rf_alerts(kind: RfKind) -> bool {
    matches!(
        kind,
        RfKind::AnalogVideo
            | RfKind::Hdzero
            | RfKind::LteStyle
            | RfKind::WifiNarrow
            | RfKind::DjiVideo
            | RfKind::DjiO4Video
            | RfKind::DjiFpvAirUnit
            | RfKind::DjiController
    )
}

/// Short display name of a kind.
pub const fn rf_label(kind: RfKind) -> &'static str {
    match kind {
        RfKind::AnalogVideo => "Analog video",
        RfKind::FpvCarrier => "FPV carrier",
        RfKind::Hdzero => "HDZero",
        RfKind::LteStyle => "LTE-style link",
        RfKind::WifiNarrow => "Narrow Wi-Fi",
        RfKind::Energy => "Energy",
        RfKind::DjiVideo => "DJI video",
        RfKind::DjiO4Video => "DJI O4 video",
        RfKind::DjiFpvAirUnit => "DJI FPV air unit",
        RfKind::DjiController => "DJI controller",
    }
}

/// Stable handle for an RF emitter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(transparent)]
pub struct EmitterId(pub u32);

/// One emitter: a kind on one frequency.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Emitter {
    pub id: EmitterId,
    pub kind: RfKind,
    /// Latest centre, 100 kHz units.
    pub freq: u32,
    /// Occupied width, kHz (0 if unknown).
    pub width_khz: u16,
    /// Level above the noise floor, dB (latest).
    pub snr_db: i8,
    /// Strongest level seen.
    pub peak_snr_db: i8,
    /// Fraction of the dwell it was on, percent (latest).
    pub duty_pct: u8,
    /// Latest non-zero analog line rate, Hz (0 if never measured).
    pub line_hz: u16,
    pub node: u8,
    pub first_seen_ms: u64,
    pub last_seen_ms: u64,
    pub sightings: u32,
    pub(crate) announced: bool,
}

impl Emitter {
    pub fn freq_mhz(&self) -> f64 {
        f64::from(self.freq) / 10.0
    }

    fn same(&self, s: &RfSighting) -> bool {
        // A controller hops: every centre in one band is the same one.
        let tol = if s.kind == RfKind::DjiController {
            HOP_SPAN
        } else {
            SAME_EMITTER.max(u32::from(self.width_khz.max(s.width_khz)) / 200)
        };
        self.kind == s.kind && self.freq.abs_diff(s.freq) <= tol
    }
}

/// Stable handle for a Wi-Fi drone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(transparent)]
pub struct WifiDroneId(pub u32);

/// One Wi-Fi network that looks like a drone or its controller.
#[derive(Debug, Clone, PartialEq)]
pub struct WifiDrone {
    pub id: WifiDroneId,
    /// Index into `wifidrone::MAKERS`.
    pub maker: u8,
    /// Transmitter address: the key. Shown only with `show_link_ids`.
    pub mac: [u8; 6],
    pub ssid: heapless::String<32>,
    pub channel: u8,
    pub rssi: i8,
    pub node: u8,
    pub first_seen_ms: u64,
    pub last_seen_ms: u64,
    pub sightings: u32,
    pub(crate) announced: bool,
}

impl WifiDrone {
    /// The maker's name; "Unknown maker" for an index this build lacks.
    pub fn maker_name(&self) -> &'static str {
        wifidrone::maker_name(self.maker).unwrap_or("Unknown maker")
    }

    pub fn band(&self) -> Band {
        Band::of_wifi_channel(self.channel)
    }
}

/// What one node last said about itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeInfo {
    pub node: u8,
    /// Its current role (Status, else Hello).
    pub role: Option<NodeRole>,
    /// Roles it can take, a mask of [`NodeRole::bit`] (Status only).
    pub can: Option<u8>,
    /// The hub plan it has applied, 0 for its built-in one (Status only).
    pub plan: Option<u16>,
    /// Hub time of its latest Hello or Status.
    pub last_ms: u64,
}

impl NodeInfo {
    /// Whether the node can take `role`; `None` before its first Status.
    pub fn can(&self, role: NodeRole) -> Option<bool> {
        self.can.map(|m| m & role.bit() != 0)
    }
}

/// Either kind of non-drone signal, for [`Tracker::take_new_signal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalKey {
    Emitter(EmitterId),
    WifiDrone(WifiDroneId),
}

fn state(last_seen_ms: u64, now_ms: u64, config: &Config) -> TrackState {
    if now_ms.saturating_sub(last_seen_ms) >= config.stale_after_ms {
        TrackState::Stale
    } else {
        TrackState::Active
    }
}

/// Make room in a full table by removing its least recently heard entry.
fn make_room<T, const M: usize>(v: &mut Vec<T, M>, last: impl Fn(&T) -> u64) {
    if v.is_full()
        && let Some(i) = (0..v.len()).min_by_key(|&i| last(&v[i]))
    {
        v.remove(i);
    }
}

#[derive(Debug, Clone, Default)]
pub struct EmitterTable {
    v: Vec<Emitter, MAX_EMITTERS>,
    next_id: u32,
}

impl EmitterTable {
    pub const fn new() -> Self {
        Self {
            v: Vec::new(),
            next_id: 1,
        }
    }

    pub fn all(&self) -> &[Emitter] {
        &self.v
    }

    fn ingest(&mut self, node: u8, s: &RfSighting, now_ms: u64, config: &Config) -> EmitterId {
        self.expire(now_ms, config);
        let i = match self.v.iter().position(|e| e.same(s)) {
            Some(i) => i,
            None => {
                make_room(&mut self.v, |e| e.last_seen_ms);
                let id = EmitterId(self.next_id.max(1));
                self.next_id = id.0.wrapping_add(1).max(1);
                let _ = self.v.push(Emitter {
                    id,
                    kind: s.kind,
                    freq: s.freq,
                    width_khz: 0,
                    snr_db: s.snr_db,
                    peak_snr_db: s.snr_db,
                    duty_pct: 0,
                    line_hz: 0,
                    node,
                    first_seen_ms: now_ms,
                    last_seen_ms: now_ms,
                    sightings: 0,
                    announced: false,
                });
                self.v.len() - 1
            }
        };
        let e = &mut self.v[i];
        e.freq = s.freq;
        if s.width_khz != 0 {
            e.width_khz = s.width_khz;
        }
        e.snr_db = s.snr_db;
        e.peak_snr_db = e.peak_snr_db.max(s.snr_db);
        e.duty_pct = s.duty_pct;
        if s.line_hz != 0 {
            e.line_hz = s.line_hz;
        }
        e.node = node;
        e.last_seen_ms = e.last_seen_ms.max(now_ms);
        e.sightings = e.sightings.saturating_add(1);
        e.id
    }

    pub(crate) fn expire(&mut self, now_ms: u64, config: &Config) {
        self.v
            .retain(|e| now_ms.saturating_sub(e.last_seen_ms) < config.remove_after_ms);
    }
}

#[derive(Debug, Clone, Default)]
pub struct WifiDroneTable {
    v: Vec<WifiDrone, MAX_WIFI_DRONES>,
    next_id: u32,
}

impl WifiDroneTable {
    pub const fn new() -> Self {
        Self {
            v: Vec::new(),
            next_id: 1,
        }
    }

    pub fn all(&self) -> &[WifiDrone] {
        &self.v
    }

    fn ingest(
        &mut self,
        node: u8,
        s: &WifiDroneSighting,
        now_ms: u64,
        config: &Config,
    ) -> WifiDroneId {
        self.expire(now_ms, config);
        let i = match self.v.iter().position(|w| w.mac == s.mac) {
            Some(i) => i,
            None => {
                make_room(&mut self.v, |w| w.last_seen_ms);
                let id = WifiDroneId(self.next_id.max(1));
                self.next_id = id.0.wrapping_add(1).max(1);
                let _ = self.v.push(WifiDrone {
                    id,
                    maker: s.maker,
                    mac: s.mac,
                    ssid: heapless::String::new(),
                    channel: s.channel,
                    rssi: s.rssi,
                    node,
                    first_seen_ms: now_ms,
                    last_seen_ms: now_ms,
                    sightings: 0,
                    announced: false,
                });
                self.v.len() - 1
            }
        };
        let w = &mut self.v[i];
        w.maker = s.maker;
        if !s.ssid.is_empty() {
            w.ssid = s.ssid.clone();
        }
        w.channel = s.channel;
        w.rssi = s.rssi;
        w.node = node;
        w.last_seen_ms = w.last_seen_ms.max(now_ms);
        w.sightings = w.sightings.saturating_add(1);
        w.id
    }

    pub(crate) fn expire(&mut self, now_ms: u64, config: &Config) {
        self.v
            .retain(|w| now_ms.saturating_sub(w.last_seen_ms) < config.remove_after_ms);
    }
}

#[derive(Debug, Clone, Default)]
pub struct NodeTable {
    v: Vec<NodeInfo, MAX_NODES>,
}

impl NodeTable {
    pub const fn new() -> Self {
        Self { v: Vec::new() }
    }

    fn entry(&mut self, node: u8, now_ms: u64) -> &mut NodeInfo {
        let i = match self.v.iter().position(|n| n.node == node) {
            Some(i) => i,
            None => {
                make_room(&mut self.v, |n| n.last_ms);
                let _ = self.v.push(NodeInfo {
                    node,
                    role: None,
                    can: None,
                    plan: None,
                    last_ms: now_ms,
                });
                self.v.len() - 1
            }
        };
        let n = &mut self.v[i];
        n.last_ms = n.last_ms.max(now_ms);
        n
    }
}

impl<const N: usize, const A: usize> Tracker<N, A> {
    /// Feed one raw-capture sighting from `node`.
    pub fn ingest_rf(&mut self, node: u8, s: &RfSighting, now_ms: u64) -> EmitterId {
        self.expire(now_ms);
        self.emitters.ingest(node, s, now_ms, &self.config)
    }

    /// RF emitters, in table order.
    pub fn emitters(&self) -> &[Emitter] {
        self.emitters.all()
    }

    pub fn get_emitter(&self, id: EmitterId) -> Option<&Emitter> {
        self.emitters.all().iter().find(|e| e.id == id)
    }

    /// Feed one Wi-Fi drone sighting from `node`.
    pub fn ingest_wifi_drone(
        &mut self,
        node: u8,
        s: &WifiDroneSighting,
        now_ms: u64,
    ) -> WifiDroneId {
        self.expire(now_ms);
        self.wifi_drones.ingest(node, s, now_ms, &self.config)
    }

    /// Wi-Fi drones, in table order.
    pub fn wifi_drones(&self) -> &[WifiDrone] {
        self.wifi_drones.all()
    }

    pub fn get_wifi_drone(&self, id: WifiDroneId) -> Option<&WifiDrone> {
        self.wifi_drones.all().iter().find(|w| w.id == id)
    }

    /// A node's Status frame: its role, what it can do, the plan it applied.
    pub fn ingest_status(&mut self, node: u8, role: NodeRole, can: u8, plan: u16, now_ms: u64) {
        let n = self.nodes.entry(node, now_ms);
        // What is a normal frame rate depends on what the node listens to.
        if n.role != Some(role) || n.plan != Some(plan) {
            self.deaf.reset(node);
        }
        let n = self.nodes.entry(node, now_ms);
        n.role = Some(role);
        n.can = Some(can);
        n.plan = Some(plan);
    }

    /// A node's Hello frame: its role only.
    pub fn ingest_hello(&mut self, node: u8, role: NodeRole, now_ms: u64) {
        self.nodes.entry(node, now_ms).role = Some(role);
    }

    /// Every node that has sent a Hello or Status, in first-heard order.
    pub fn nodes(&self) -> &[NodeInfo] {
        &self.nodes.v
    }

    pub fn node(&self, node: u8) -> Option<&NodeInfo> {
        self.nodes.v.iter().find(|n| n.node == node)
    }

    /// Feed any frame from `node` to the table it belongs to. Stats and Via
    /// frames are link bookkeeping, not tracker data, and are ignored.
    pub fn ingest_frame(&mut self, node: u8, frame: &link::Frame, now_ms: u64) {
        use link::Frame;
        match frame {
            Frame::Detection(d) => drop(self.ingest(node, d, now_ms)),
            Frame::OpenLink(s) => drop(self.ingest_open_link(node, s, now_ms)),
            Frame::DroneId(s) => drop(self.ingest_droneid(node, s, now_ms)),
            Frame::Rf(s) => drop(self.ingest_rf(node, s, now_ms)),
            Frame::DjiWifi {
                channel,
                rssi,
                mac,
                element,
                ..
            } => drop(self.ingest_dji_wifi(node, *channel, *rssi, *mac, element, now_ms)),
            Frame::WifiDrone(s) => drop(self.ingest_wifi_drone(node, s, now_ms)),
            // Protocol 11: raw aircraft frames are decoded by the hub (it
            // holds the GPS time and the FLARM keys) and arrive here as
            // `ingest_aircraft`; the aero node's status is link bookkeeping.
            Frame::Air(_) | Frame::AeroStatus(_) => {}
            // Protocol 12: drone control links and control-band energy go
            // to a `ctrllinks::ControlTable`, which the caller owns (the P4
            // hub keeps it in PSRAM) and feeds itself.
            Frame::Control(_) | Frame::ControlEnergy(_) => {}
            // Protocol 13: the bytes behind a MavlinkAttack threat, for the
            // evidence log only (the threat itself is `Frame::Threat`).
            Frame::MavlinkEvidence(_) => {}
            Frame::Status { role, can, plan } => {
                self.ingest_status(node, *role, *can, *plan, now_ms)
            }
            Frame::Hello { role, .. } => self.ingest_hello(node, *role, now_ms),
            // Stats feed the hub's deaf-node check (a jammed node).
            Frame::Stats { uptime_us, frames_seen, .. } => {
                let _ = self.ingest_node_stats(node, *uptime_us, *frames_seen, now_ms);
            }
            Frame::Via { .. } => {}
            // Protocol 10: wired-bus bookkeeping, consumed by the bus master.
            Frame::BusJoin { .. } | Frame::BusDone { .. } => {}
            // Protocol 5, hub to web head: the hub firmware handles these
            // (own position, ADS-B), not the drone tables.
            Frame::HubFix(_) | Frame::Csbee(_) | Frame::Flags(_) => {}
            // Protocol 9: attacks and jamming, a table of their own.
            Frame::Threat(s) => drop(self.ingest_threat(node, s, now_ms)),
        }
    }

    /// The next RF emitter or Wi-Fi drone to alert on, once each (see the
    /// module docs for which kinds alert).
    pub fn take_new_signal(&mut self, now_ms: u64) -> Option<SignalKey> {
        let cfg = self.config;
        if let Some(e) = self.emitters.v.iter_mut().find(|e| {
            !e.announced
                && rf_alerts(e.kind)
                && state(e.last_seen_ms, now_ms, &cfg) == TrackState::Active
        }) {
            e.announced = true;
            return Some(SignalKey::Emitter(e.id));
        }
        let w =
            self.wifi_drones.v.iter_mut().find(|w| {
                !w.announced && state(w.last_seen_ms, now_ms, &cfg) == TrackState::Active
            })?;
        w.announced = true;
        Some(SignalKey::WifiDrone(w.id))
    }
}

impl crate::Context {
    pub fn emitter_state(&self, e: &Emitter) -> TrackState {
        state(e.last_seen_ms, self.now_ms, &self.config)
    }

    pub fn wifi_drone_state(&self, w: &WifiDrone) -> TrackState {
        state(w.last_seen_ms, self.now_ms, &self.config)
    }
}
