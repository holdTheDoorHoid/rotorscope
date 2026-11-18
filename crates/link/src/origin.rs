//! Who may originate which [`Frame`] (docs/ESPNOW.md "Who may send what").
//!
//! The node link carries frames from several kinds of sender, and the hub
//! re-sends the stream to its web head together with frames of its own. A
//! frame type that only the hub (or only the link layer) ever writes must
//! never be taken from a node: a node, or anyone who can forge node traffic
//! (on an open kit anyone in radio range, on a keyed kit anyone who has read
//! the kit key out of one board), could otherwise move the kit's own position
//! and clock (`HubFix`), show made-up aircraft (`Csbee`) or flags (`Flags`),
//! or name another node as the sender of its frames (`Via`).
//!
//! [`Head::origin`] is the one table of who may originate each variant
//! ([`origin`] for a decoded frame, [`origin_of_encoded`] for one still in
//! bytes). It and [`head`] are exhaustive `match`es with no wildcard arm, so
//! a new variant does not compile until someone decides who may send it.
//! Every hop that can check, checks:
//!
//! - the ESP-NOW gateway relays only frames a node may originate
//!   ([`node_batch`]), and never a node's own `Via`;
//! - the hub sorts its node-link stream with a [`Sorter`]: a `Via` names the
//!   sender of exactly the one frame after it, that frame must be one a node
//!   may originate, and a frame with no `Via` (the link's own: the gateway,
//!   or one wired node) must be one a node or a gateway may originate;
//! - the forwarder to the web head (`forward`) runs the same sort, so a
//!   frame the hub refused is never passed on;
//! - the wired bus (`wirebus::Attributor`) applies the node rule to every
//!   bus node's frame and takes a node-supplied `Via` only from a bus address
//!   registered as a gateway;
//! - the S3 web head sorts each of its links with a [`Sorter`] too, the one
//!   from the P4 hub made with [`Sorter::hub_link`], which also takes the
//!   hub's own (`Origin::Hub`) frames when no `Via` came before them.
//!
//! Refusals are counted where they happen and reported as a
//! [`ThreatKind::ForgedLink`] threat with `detail` [`FORGED_ORIGIN`], at most
//! once a second per claimed sender ([`RefusalLog`]).

use crate::{Frame, MAX_FRAME, NodeRole, ThreatKind, ThreatSighting};

/// Who may originate a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// A radio node's own report. May reach the hub relayed (after a `Via`)
    /// or straight from a node wired to the link.
    Node,
    /// The node link's own end: an ESP-NOW gateway's own frames (written
    /// with no `Via` before them), or the `Via` markers a gateway (or the
    /// hub's bus attributor, standing in for one) puts before relayed
    /// frames. Never relayed from a node.
    Gateway,
    /// Only the hub, on its feed to the web head. Never on the node link.
    Hub,
    /// Wired-bus control, consumed by the bus master and node
    /// (`crates/wirebus`). Never inside a relayed stream.
    Bus,
}

impl Origin {
    pub const fn name(self) -> &'static str {
        match self {
            Origin::Node => "node",
            Origin::Gateway => "gateway",
            Origin::Hub => "hub",
            Origin::Bus => "bus",
        }
    }
}

/// `ThreatKind::ForgedLink` `detail` for a frame refused because its sender
/// may not originate that frame type (codes 1-4 are `airlink::auth::Reject`; 7, `Late`, is
/// counted by nodes only and never reported by the gateway).
pub const FORGED_ORIGIN: u32 = 5;
/// `ThreatKind::ForgedLink` `detail` for a genuine node packet a keyed
/// gateway could not relay because its node table was full.
pub const FORGED_TABLE_FULL: u32 = 6;

/// Who may claim a role in a `Hello` or `Status`: the gateway role only the
/// link's own end (the gateway, or the hub introducing itself to its web
/// head); every other role is a node's.
pub const fn role_origin(role: NodeRole) -> Origin {
    match role {
        NodeRole::Wifi24
        | NodeRole::Wifi5
        | NodeRole::Ble
        | NodeRole::Fpv
        | NodeRole::DroneId
        | NodeRole::Aero => Origin::Node,
        NodeRole::Gateway => Origin::Gateway,
    }
}

/// Who may raise a threat of this kind.
pub const fn threat_origin(kind: ThreatKind) -> Origin {
    use ThreatKind::*;
    match kind {
        Deauth | Disassoc | BeaconFlood | EvilTwin | Karma | ChannelSwitch | AuthFlood | ProbeFlood | AttackTool
        | BleSpam | JamBroadband | JamTone | JamSweep | JamPulsed | Blinded | DroneDeauth | DroneTakeover
        | DroneNetClone | ControlContested | MavlinkAttack => Origin::Node,
        // The gateway's report of packets it refused (and, on its own
        // stream to the web head, the hub's).
        ForgedLink => Origin::Gateway,
        // Raised by the hub from aircraft and drone tracks.
        AreaGnss | IdFlood => Origin::Hub,
    }
}

/// What decides a frame's origin: its variant and, for `Hello` / `Status`,
/// the role, for `Threat` the kind. Variants in [`Frame`]'s order, so the
/// start of an encoded frame decodes as its `Head` ([`head_of_encoded`]):
/// the wired bus classifies frames in its interrupt handler without
/// decoding them whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
pub enum Head {
    Hello(NodeRole),
    Detection,
    Stats,
    OpenLink,
    Via,
    Status(NodeRole),
    DroneId,
    Rf,
    DjiWifi,
    WifiDrone,
    HubFix,
    Csbee,
    Flags,
    Threat(ThreatKind),
    BusJoin,
    BusDone,
    Air,
    AeroStatus,
    Control,
    ControlEnergy,
    MavlinkEvidence,
}

/// The [`Head`] of a frame. Exhaustive: a new `Frame` variant needs a
/// `Head` of its own, and so a line in [`Head::origin`].
pub fn head(f: &Frame) -> Head {
    match f {
        Frame::Hello { role, .. } => Head::Hello(*role),
        Frame::Detection(_) => Head::Detection,
        Frame::Stats { .. } => Head::Stats,
        Frame::OpenLink(_) => Head::OpenLink,
        Frame::Via { .. } => Head::Via,
        Frame::Status { role, .. } => Head::Status(*role),
        Frame::DroneId(_) => Head::DroneId,
        Frame::Rf(_) => Head::Rf,
        Frame::DjiWifi { .. } => Head::DjiWifi,
        Frame::WifiDrone(_) => Head::WifiDrone,
        Frame::HubFix(_) => Head::HubFix,
        Frame::Csbee(_) => Head::Csbee,
        Frame::Flags(_) => Head::Flags,
        Frame::Threat(t) => Head::Threat(t.kind),
        Frame::BusJoin { .. } => Head::BusJoin,
        Frame::BusDone { .. } => Head::BusDone,
        Frame::Air(_) => Head::Air,
        Frame::AeroStatus(_) => Head::AeroStatus,
        Frame::Control(_) => Head::Control,
        Frame::ControlEnergy(_) => Head::ControlEnergy,
        Frame::MavlinkEvidence(_) => Head::MavlinkEvidence,
    }
}

impl Head {
    /// **The table.** Who may originate each frame type. No wildcard arm,
    /// on purpose: a new variant must be decided here.
    pub const fn origin(self) -> Origin {
        match self {
            // A node introduces itself; the gateway role is the link's own.
            Head::Hello(role) => role_origin(role),
            // Radio nodes' reports.
            Head::Detection => Origin::Node,
            Head::Stats => Origin::Node,
            Head::OpenLink => Origin::Node,
            // Written by a gateway (or the hub's bus attributor) before each
            // relayed frame; a node's own would name someone else.
            Head::Via => Origin::Gateway,
            Head::Status(role) => role_origin(role),
            Head::DroneId => Origin::Node,
            Head::Rf => Origin::Node,
            Head::DjiWifi => Origin::Node,
            Head::WifiDrone => Origin::Node,
            // Hub to web head only: the kit's GPS and clock, the hub's
            // ADS-B receiver, its FAA flags.
            Head::HubFix => Origin::Hub,
            Head::Csbee => Origin::Hub,
            Head::Flags => Origin::Hub,
            Head::Threat(kind) => threat_origin(kind),
            // Consumed by the bus master and node; never in a relayed stream.
            Head::BusJoin => Origin::Bus,
            Head::BusDone => Origin::Bus,
            // Aero (LR2021) nodes' reports.
            Head::Air => Origin::Node,
            Head::AeroStatus => Origin::Node,
            Head::Control => Origin::Node,
            Head::ControlEnergy => Origin::Node,
            Head::MavlinkEvidence => Origin::Node,
        }
    }
}

/// Who may originate `f` ([`Head::origin`]).
pub fn origin(f: &Frame) -> Origin {
    head(f).origin()
}

/// The [`Head`] of an encoded frame (COBS, delimiter included or not),
/// from its first few bytes, with no buffer the size of a frame. `None` if
/// it does not start like any frame.
pub fn head_of_encoded(bytes: &[u8]) -> Option<Head> {
    let mut prefix = [0u8; 4];
    let n = cobs_prefix(bytes, &mut prefix);
    postcard::from_bytes(&prefix[..n]).ok()
}

/// [`Head::origin`] of an encoded frame; `None` if it does not start like
/// any frame.
pub fn origin_of_encoded(bytes: &[u8]) -> Option<Origin> {
    head_of_encoded(bytes).map(Head::origin)
}

/// COBS-decode the first `out.len()` bytes of `src` (or fewer, up to its
/// delimiter); returns how many.
fn cobs_prefix(src: &[u8], out: &mut [u8]) -> usize {
    let (mut i, mut n) = (0, 0);
    while n < out.len() {
        let code = match src.get(i) {
            Some(&c) if c != 0 => c,
            _ => break,
        };
        i += 1;
        for _ in 1..code {
            match src.get(i) {
                Some(&b) if b != 0 && n < out.len() => {
                    out[n] = b;
                    n += 1;
                    i += 1;
                }
                _ => return n,
            }
        }
        // A block shorter than 254 ends in a zero, unless it is the last.
        if code != 0xFF && n < out.len() && src.get(i).is_some_and(|&b| b != 0) {
            out[n] = 0;
            n += 1;
        }
    }
    n
}

/// A node may send `f` (relayed by a gateway, or on the bus).
pub fn node_may_send(f: &Frame) -> bool {
    origin(f) == Origin::Node
}

/// The node link's own end may write `f` with no `Via` before it: a
/// gateway's own frame, or a frame from one node wired to the link.
pub fn link_may_send(f: &Frame) -> bool {
    matches!(origin(f), Origin::Node | Origin::Gateway)
}

/// Decode one COBS frame (zero delimiter included or not). `None` if it is
/// longer than [`MAX_FRAME`] or does not decode.
pub fn decode_chunk(bytes: &[u8]) -> Option<Frame> {
    let mut buf = [0u8; MAX_FRAME];
    let b = buf.get_mut(..bytes.len())?;
    b.copy_from_slice(bytes);
    postcard::from_bytes_cobs(b).ok()
}

/// One frame of a node's batch, as [`node_batch`] sorts it.
#[derive(Debug, PartialEq)]
pub enum NodeChunk<'a> {
    /// A whole frame a node may send: relay `bytes` (COBS, delimiter
    /// included) as they are.
    Relay { bytes: &'a [u8], frame: Frame },
    /// A whole frame of a type a node may not originate: drop it.
    Refused { frame: Frame, origin: Origin },
    /// Does not decode, is too long, or is a tail with no delimiter: drop
    /// it (it cannot be checked, and the hub could not read it either).
    Corrupt,
}

/// Split a node's batch (COBS frames back to back, as an `airlink` packet
/// carries them) and sort each frame for the gateway: only frames a node may
/// originate are relayed, a node's own `Via` never.
pub fn node_batch<'a>(payload: &'a [u8], mut on: impl FnMut(NodeChunk<'a>)) {
    for bytes in payload.split_inclusive(|&b| b == 0) {
        if bytes.last() != Some(&0) {
            on(NodeChunk::Corrupt); // a truncated tail: never relay half a frame
            break;
        }
        if bytes.len() == 1 {
            continue; // a lone zero: padding
        }
        on(match decode_chunk(bytes) {
            Some(frame) if node_may_send(&frame) => NodeChunk::Relay { bytes, frame },
            Some(frame) => {
                let origin = origin(&frame);
                NodeChunk::Refused { frame, origin }
            }
            None => NodeChunk::Corrupt,
        });
    }
}

/// What the hub makes of one frame of its node-link stream ([`Sorter`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sorted {
    /// A `Via`: it names the sender of the next frame. Nothing to do yet.
    Marker,
    /// A node's frame, relayed by the gateway (or the bus) from `node`,
    /// heard at `rssi`.
    Relayed { node: [u8; 6], rssi: i8 },
    /// The link's own frame (no `Via`): the gateway's, or one wired node's.
    Own,
    /// Refused: `from` (the node a `Via` named; `None` for the link's own
    /// stream) may not originate a frame of `origin`. Drop it.
    Refused { from: Option<([u8; 6], i8)>, origin: Origin },
}

/// The hub's reading of a node-link stream: which sender each frame is
/// from, and whether that sender may originate it.
///
/// A `Via` applies to exactly the one frame after it, whatever that frame
/// is, and never lingers past it. A `Via` right after a `Via` is a node's
/// frame claiming to be a marker (a gateway writes each `Via` together with
/// its frame): it is refused, and the held `Via` is dropped with it, so a
/// node batch that ends in `Via { X }` cannot lend `X` to whatever the
/// gateway writes next (its own `Hello` or threat report).
///
/// [`Sorter::hub_link`] is the web head's reading of its link from a P4 hub:
/// the same rules, except that the link's own end (the hub) may also write
/// hub-origin frames (`HubFix`, `Csbee`, `Flags`, `AreaGnss`). Relayed, they
/// are refused there too.
#[derive(Debug, Default, Clone)]
pub struct Sorter {
    via: Option<([u8; 6], i8)>,
    /// The link's own end is a hub ([`Sorter::hub_link`]).
    hub: bool,
}

impl Sorter {
    /// A node link: its own end is a gateway or one wired node.
    pub const fn new() -> Self {
        Self { via: None, hub: false }
    }

    /// A link whose own end is a hub feeding its web head.
    pub const fn hub_link() -> Self {
        Self { via: None, hub: true }
    }

    /// One decoded frame, in stream order.
    pub fn sort(&mut self, f: &Frame) -> Sorted {
        if let Frame::Via { node, rssi } = *f {
            return match self.via.take() {
                None => {
                    self.via = Some((node, rssi));
                    Sorted::Marker
                }
                Some(from) => Sorted::Refused { from: Some(from), origin: Origin::Gateway },
            };
        }
        match self.via.take() {
            Some((node, rssi)) if node_may_send(f) => Sorted::Relayed { node, rssi },
            Some(from) => Sorted::Refused { from: Some(from), origin: origin(f) },
            None if link_may_send(f) => Sorted::Own,
            None if self.hub && origin(f) == Origin::Hub => Sorted::Own,
            None => Sorted::Refused { from: None, origin: origin(f) },
        }
    }

    /// The stream had an error (a corrupt or overlong chunk): a held `Via`
    /// names nothing now. True if one was held.
    pub fn reset(&mut self) -> bool {
        self.via.take().is_some()
    }

    /// A `Via` is waiting for its frame.
    pub fn holding(&self) -> bool {
        self.via.is_some()
    }
}

/// Refused frames from one claimed sender with one reason, over a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refusal {
    /// The claimed sender; all zero for the link's own stream, or when the
    /// log was full (many senders).
    pub source: [u8; 6],
    /// `ForgedLink` detail code ([`FORGED_ORIGIN`], [`FORGED_TABLE_FULL`]).
    pub detail: u32,
    pub count: u32,
    /// Strongest RSSI among them, dBm (0 on a wire).
    pub rssi: i8,
    pub window_ms: u32,
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    source: [u8; 6],
    detail: u32,
    count: u32,
    rssi: i8,
}

/// Refusals per claimed sender and reason, reported once a window (the
/// same rate limit as the gateway's authentication rejects). Fixed size:
/// when it is full, further senders share one all-zero entry, so an
/// attacker cycling addresses cannot grow it.
#[derive(Debug, Clone)]
pub struct RefusalLog<const N: usize> {
    entries: [Option<Entry>; N],
    since_ms: u64,
    /// Refusals recorded since boot.
    pub total: u32,
}

impl<const N: usize> Default for RefusalLog<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> RefusalLog<N> {
    pub const fn new() -> Self {
        assert!(N >= 2);
        RefusalLog { entries: [None; N], since_ms: 0, total: 0 }
    }

    pub fn record(&mut self, source: [u8; 6], detail: u32, rssi: i8) {
        self.total = self.total.saturating_add(1);
        let pos = |entries: &[Option<Entry>], s: [u8; 6]| {
            entries.iter().position(|e| e.is_some_and(|e| e.source == s && e.detail == detail))
        };
        let zero = [0u8; 6];
        let (i, source) = match pos(&self.entries, source) {
            Some(i) => (i, source),
            // The last slot is kept for the shared overflow entry.
            None => match self.entries[..N - 1].iter().position(Option::is_none) {
                Some(i) => (i, source),
                None => match pos(&self.entries, zero) {
                    Some(i) => (i, zero),
                    // The overflow slot holds another reason: share it anyway
                    // (counted under the reason it already has).
                    None if self.entries[N - 1].is_some() => (N - 1, self.entries[N - 1].unwrap().source),
                    None => (N - 1, zero),
                },
            },
        };
        let e = self.entries[i].get_or_insert(Entry { source, detail, count: 0, rssi: i8::MIN });
        e.count = e.count.saturating_add(1);
        e.rssi = e.rssi.max(rssi);
    }

    /// If at least `window_ms` passed since the last call that reported,
    /// hand out one report per sender and reason seen, and start a new
    /// window. Returns how many were reported.
    pub fn drain(&mut self, now_ms: u64, window_ms: u32, mut report: impl FnMut(Refusal)) -> usize {
        if now_ms.saturating_sub(self.since_ms) < u64::from(window_ms) {
            return 0;
        }
        let window = now_ms.saturating_sub(self.since_ms).min(u64::from(u32::MAX)) as u32;
        self.since_ms = now_ms;
        let mut n = 0;
        for slot in &mut self.entries {
            let Some(e) = slot.take() else { continue };
            report(Refusal { source: e.source, detail: e.detail, count: e.count, rssi: e.rssi, window_ms: window });
            n += 1;
        }
        n
    }
}

/// The `Threat` a refusal is reported as: `ForgedLink`, `detail` the
/// refusal's code, `source` the claimed sender, `target` the reporter
/// (all zero when it does not know its own address).
pub fn forged_link(r: &Refusal, target: [u8; 6], channel: u8, freq: u32, uptime_us: u64) -> ThreatSighting {
    ThreatSighting {
        kind: ThreatKind::ForgedLink,
        channel,
        freq,
        width_khz: 0,
        level: r.rssi,
        count: r.count,
        window_ms: r.window_ms,
        source: r.source,
        target,
        detail: r.detail,
        ssid: heapless::String::new(),
        uptime_us,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;

    fn threat(kind: ThreatKind) -> Frame {
        Frame::Threat(ThreatSighting {
            kind,
            channel: 6,
            freq: 24370,
            width_khz: 0,
            level: -50,
            count: 1,
            window_ms: 1000,
            source: [2, 0, 0, 0, 0, 7],
            target: [0; 6],
            detail: 1,
            ssid: heapless::String::new(),
            uptime_us: 1,
        })
    }

    fn hello(role: NodeRole) -> Frame {
        Frame::Hello { role, protocol: PROTOCOL_VERSION, firmware: "x".try_into().unwrap() }
    }

    fn stats(n: u64) -> Frame {
        Frame::Stats { uptime_us: n, frames_seen: 1, rid_found: 0, dropped: 0, by_transport: heapless::Vec::new() }
    }

    fn hub_fix() -> Frame {
        Frame::HubFix(HubFix {
            state: GnssState::Fix3d,
            sats: Some(12),
            position: Some(HubPosition { lat_e7: 399_526_000, lon_e7: -751_652_000, alt_hae_mm: Some(1), age_ms: 0 }),
            utc_ms: Some(1_800_000_000_000),
            utc_pps: true,
        })
    }

    fn csbee() -> Frame {
        Frame::Csbee(b"#A:A12345,,,,,,,,,,,,,,,,ABCD\r\n".iter().copied().collect())
    }

    fn flags() -> Frame {
        Frame::Flags(FlagReport { who: "ICAO-A12345".try_into().unwrap(), level: 3, items: heapless::Vec::new(), pack_date: 0 })
    }

    fn via(n: u8) -> Frame {
        Frame::Via { node: [2, 0, 0, 0, 0, n], rssi: -40 }
    }

    /// One frame of every variant (and every role and threat kind), with
    /// the origin the table must give it.
    fn table() -> std::vec::Vec<(Frame, Origin)> {
        use Origin::*;
        let mut t = std::vec![
            (Frame::Detection(Detection {
                transport: rid::Transport::WifiBeacon,
                channel: 6,
                rssi: -50,
                mac: [1; 6],
                counter: 0,
                uptime_us: 1,
                messages: heapless::Vec::new(),
            }), Node),
            (stats(1), Node),
            (Frame::OpenLink(OpenLinkSighting {
                link: openlink::OpenLink {
                    system: openlink::System::DroneBridge,
                    id: None,
                    port: 0,
                    direction: openlink::Direction::Unknown,
                    tx: [1; 6],
                },
                channel: 1,
                rssi: -60,
                uptime_us: 1,
                packets: 1,
            }), Node),
            (via(1), Gateway),
            (Frame::DroneId(DroneIdSighting { freq: 57565, rssi: -70, snr_db: 10, uptime_us: 1, record: DroneIdRecord::Detected }), Node),
            (Frame::Rf(RfSighting { kind: RfKind::Energy, freq: 58000, width_khz: 0, snr_db: 5, duty_pct: 1, line_hz: 0, uptime_us: 1 }), Node),
            (Frame::DjiWifi { channel: 6, rssi: -50, mac: [1; 6], uptime_us: 1, element: heapless::Vec::new() }, Node),
            (Frame::WifiDrone(WifiDroneSighting { maker: 0, mac: [1; 6], ssid: "TELLO".try_into().unwrap(), channel: 6, rssi: -50, uptime_us: 1 }), Node),
            (hub_fix(), Hub),
            (csbee(), Hub),
            (flags(), Hub),
            (Frame::BusJoin { mac: [1; 6], want: 0, check: 0 }, Bus),
            (Frame::BusDone { addr: 1, seq: 1, more: false, crc: 0 }, Bus),
            (Frame::Air(AirFrame { proto: AirProto::Es1090, freq_khz: 1_090_000, rssi: -80, utc_ms: None, uptime_us: 1, raw: heapless::Vec::new() }), Node),
            (Frame::AeroStatus(AeroStatus {
                mode: AeroMode::ES1090_CODE,
                freq_khz: 1_090_000,
                time: AeroTime::Unknown,
                parked: false,
                radio_ok: true,
                rx_restarts: 0,
                rejected: 0,
                rate_dropped: 0,
            }), Node),
            (Frame::Control(ControlReport {
                sighting: ctrlwatch::ControlSighting {
                    system: ctrlwatch::System::FlyskyAfhds2a,
                    region: ctrlwatch::Region::Ism2g4,
                    probe: ctrlwatch::Probe::Elrs { rate: ctrlwatch::elrs::ElrsRate::L900_25, version: ctrlwatch::elrs::OtaVersion::V3 },
                    freq_hz: 2_440_000_000,
                    rssi_dbm: -80,
                    snr_db: 5,
                    t_us: 1,
                    confidence: ctrlwatch::Confidence::Confirmed,
                    direction: ctrlwatch::Direction::Downlink,
                    id: None,
                    version: None,
                },
                best: ctrlwatch::Confidence::Confirmed,
                packets: 1,
                confirmed: 1,
                peak_rssi: -80,
                part: 0,
                content: heapless::Vec::new(),
                raw: None,
            }), Node),
            (Frame::ControlEnergy(ControlEnergy {
                region: ctrlwatch::Region::Fcc915,
                windows: 1,
                activity: EnergyActivity::Quiet,
                steady: heapless::Vec::new(),
                uptime_us: 1,
            }), Node),
            (Frame::MavlinkEvidence(MavlinkEvidence { detail: 1, sysid: 1, compid: 1, msgid: 0, truncated: false, uptime_us: 1, frame: heapless::Vec::new() }), Node),
        ];
        for role in [NodeRole::Wifi24, NodeRole::Wifi5, NodeRole::Ble, NodeRole::Fpv, NodeRole::DroneId, NodeRole::Aero] {
            t.push((hello(role), Node));
            t.push((Frame::Status { role, can: role.bit(), plan: 0 }, Node));
        }
        t.push((hello(NodeRole::Gateway), Gateway));
        t.push((Frame::Status { role: NodeRole::Gateway, can: NodeRole::Gateway.bit(), plan: 0 }, Gateway));
        use ThreatKind::*;
        for k in [
            Deauth, Disassoc, BeaconFlood, EvilTwin, Karma, ChannelSwitch, AuthFlood, ProbeFlood, AttackTool, BleSpam,
            JamBroadband, JamTone, JamSweep, JamPulsed, Blinded, DroneDeauth, DroneTakeover, DroneNetClone,
            ControlContested, MavlinkAttack,
        ] {
            t.push((threat(k), Node));
        }
        t.push((threat(ForgedLink), Gateway));
        t.push((threat(AreaGnss), Hub));
        t.push((threat(IdFlood), Hub));
        t
    }

    /// The start of every encoded frame decodes as its head, with no full
    /// decode (what the wired bus relies on).
    #[test]
    fn head_of_encoded_matches_head() {
        let mut buf = [0u8; MAX_FRAME];
        for (f, want) in table() {
            let wire = encode(&f, &mut buf).unwrap();
            assert_eq!(head_of_encoded(wire), Some(head(&f)), "{f:?}");
            assert_eq!(origin_of_encoded(wire), Some(want), "{f:?}");
            // Without the delimiter too, and a truncated prefix never panics.
            let n = wire.len();
            assert_eq!(head_of_encoded(&wire[..n - 1]), Some(head(&f)));
            for k in 0..n {
                let _ = head_of_encoded(&wire[..k]);
            }
        }
        assert_eq!(head_of_encoded(&[]), None);
        assert_eq!(head_of_encoded(&[0]), None);
        assert_eq!(head_of_encoded(&[2, 21, 0]), None, "no such variant");
    }

    /// Every postcard variant index of `Frame` appears in the table.
    #[test]
    fn table_covers_every_variant() {
        let mut buf = [0u8; MAX_FRAME];
        let mut tags: std::vec::Vec<u8> = table().iter().map(|(f, _)| postcard::to_slice(f, &mut buf).unwrap()[0]).collect();
        tags.sort();
        tags.dedup();
        assert_eq!(tags, (0..=20).collect::<std::vec::Vec<u8>>());
        // One past the last variant does not decode: nothing is missing.
        assert!(postcard::from_bytes::<Frame>(&[21]).is_err());
    }

    #[test]
    fn origin_table() {
        for (f, want) in table() {
            assert_eq!(origin(&f), want, "{f:?}");
            assert_eq!(node_may_send(&f), want == Origin::Node, "{f:?}");
            assert_eq!(link_may_send(&f), matches!(want, Origin::Node | Origin::Gateway), "{f:?}");
        }
    }

    fn batch(frames: &[Frame]) -> std::vec::Vec<u8> {
        let mut out = std::vec::Vec::new();
        let mut buf = [0u8; MAX_FRAME];
        for f in frames {
            out.extend_from_slice(encode(f, &mut buf).unwrap());
        }
        out
    }

    /// What `node_batch` lets through, decoded, and what it refused.
    fn run(payload: &[u8]) -> (std::vec::Vec<Frame>, std::vec::Vec<(Frame, Origin)>, usize) {
        let (mut relay, mut refused, mut corrupt) = (std::vec::Vec::new(), std::vec::Vec::new(), 0);
        node_batch(payload, |c| match c {
            NodeChunk::Relay { bytes, frame } => {
                assert_eq!(decode_chunk(bytes).as_ref(), Some(&frame));
                relay.push(frame)
            }
            NodeChunk::Refused { frame, origin } => refused.push((frame, origin)),
            NodeChunk::Corrupt => corrupt += 1,
        });
        (relay, refused, corrupt)
    }

    #[test]
    fn node_batch_refuses_everything_a_node_may_not_originate() {
        for (f, want) in table() {
            let (relay, refused, corrupt) = run(&batch(&[stats(1), f.clone(), stats(2)]));
            assert_eq!(corrupt, 0);
            if want == Origin::Node {
                assert_eq!(relay, [stats(1), f, stats(2)]);
                assert!(refused.is_empty());
            } else {
                assert_eq!(relay, [stats(1), stats(2)], "{f:?} must not be relayed");
                assert_eq!(refused, [(f, want)]);
            }
        }
    }

    #[test]
    fn node_batch_refuses_hub_only_frames() {
        let (relay, refused, _) = run(&batch(&[hub_fix(), csbee(), flags(), threat(ThreatKind::AreaGnss), threat(ThreatKind::IdFlood)]));
        assert!(relay.is_empty());
        assert_eq!(refused.iter().map(|r| r.1).collect::<std::vec::Vec<_>>(), [Origin::Hub; 5]);
    }

    #[test]
    fn node_batch_refuses_a_nodes_own_via() {
        let (relay, refused, _) = run(&batch(&[via(9), stats(1), via(9)]));
        assert_eq!(relay, [stats(1)]);
        assert_eq!(refused, [(via(9), Origin::Gateway), (via(9), Origin::Gateway)]);
        // Nor a node claiming to be the gateway, or its threat reports.
        let (relay, _, _) = run(&batch(&[hello(NodeRole::Gateway), threat(ThreatKind::ForgedLink)]));
        assert!(relay.is_empty());
    }

    #[test]
    fn node_batch_drops_what_it_cannot_check() {
        let mut p = batch(&[stats(1)]);
        p.extend_from_slice(&[3, 0xFF, 0xFF, 0]); // a tag no protocol has
        p.extend_from_slice(&batch(&[stats(2)]));
        p.extend_from_slice(&[5, 1, 2]); // a tail with no delimiter
        let (relay, refused, corrupt) = run(&p);
        assert_eq!(relay, [stats(1), stats(2)]);
        assert!(refused.is_empty());
        assert_eq!(corrupt, 2);
        // Longer than any frame: not checkable, not relayed.
        let mut long = std::vec![1u8; MAX_FRAME + 4];
        long.push(0);
        assert_eq!(run(&long).2, 1);
    }

    #[test]
    fn sorter_attributes_one_frame_per_via() {
        let mut s = Sorter::new();
        let n1 = ([2, 0, 0, 0, 0, 1], -40);
        assert_eq!(s.sort(&via(1)), Sorted::Marker);
        assert_eq!(s.sort(&stats(1)), Sorted::Relayed { node: n1.0, rssi: n1.1 });
        // The Via does not linger: the next frame is the link's own.
        assert_eq!(s.sort(&stats(2)), Sorted::Own);
        assert_eq!(s.sort(&hello(NodeRole::Gateway)), Sorted::Own);
        assert_eq!(s.sort(&threat(ThreatKind::ForgedLink)), Sorted::Own);
    }

    /// The lingering bug: a node batch that ends with its own `Via { X }`
    /// (relayed by an old gateway, which puts its own `Via` before it) must
    /// not make the gateway's next own frame look like X's.
    #[test]
    fn a_batch_ending_in_via_cannot_lend_it_to_the_gateway() {
        let mut s = Sorter::new();
        let victim = Frame::Via { node: [0xAA; 6], rssi: -30 };
        // Old gateway: Via{n1} Detection, Via{n1} Via{victim}, then its own Hello.
        assert_eq!(s.sort(&via(1)), Sorted::Marker);
        assert!(matches!(s.sort(&stats(1)), Sorted::Relayed { .. }));
        assert_eq!(s.sort(&via(1)), Sorted::Marker);
        assert_eq!(
            s.sort(&victim),
            Sorted::Refused { from: Some(([2, 0, 0, 0, 0, 1], -40)), origin: Origin::Gateway },
            "a Via straight after a Via is the node's frame, refused"
        );
        assert!(!s.holding(), "and neither Via is held");
        assert_eq!(s.sort(&hello(NodeRole::Gateway)), Sorted::Own, "the gateway's Hello is the gateway's");
        assert_eq!(s.sort(&threat(ThreatKind::ForgedLink)), Sorted::Own);
        // The node's next relayed frame is still its own.
        assert_eq!(s.sort(&via(1)), Sorted::Marker);
        assert_eq!(s.sort(&stats(3)), Sorted::Relayed { node: [2, 0, 0, 0, 0, 1], rssi: -40 });
    }

    #[test]
    fn sorter_refuses_relayed_frames_a_node_may_not_send() {
        for (f, want) in table() {
            if matches!(f, Frame::Via { .. }) {
                continue;
            }
            let mut s = Sorter::new();
            s.sort(&via(3));
            let got = s.sort(&f);
            if want == Origin::Node {
                assert_eq!(got, Sorted::Relayed { node: [2, 0, 0, 0, 0, 3], rssi: -40 });
            } else {
                assert_eq!(got, Sorted::Refused { from: Some(([2, 0, 0, 0, 0, 3], -40)), origin: want }, "{f:?}");
            }
            assert!(!s.holding());
            // With no Via: the link's own end may send node and gateway frames only.
            let got = Sorter::new().sort(&f);
            match want {
                Origin::Node | Origin::Gateway => assert_eq!(got, Sorted::Own),
                _ => assert_eq!(got, Sorted::Refused { from: None, origin: want }, "{f:?}"),
            }
        }
    }

    /// The web head's link from the P4: the hub's own frames are taken
    /// unrelayed, never behind a `Via`; everything else as on a node link.
    #[test]
    fn hub_link_takes_hub_frames_only_unrelayed() {
        for (f, want) in table() {
            if matches!(f, Frame::Via { .. }) {
                continue;
            }
            let mut s = Sorter::hub_link();
            s.sort(&via(3));
            let got = s.sort(&f);
            if want == Origin::Node {
                assert_eq!(got, Sorted::Relayed { node: [2, 0, 0, 0, 0, 3], rssi: -40 });
            } else {
                assert_eq!(got, Sorted::Refused { from: Some(([2, 0, 0, 0, 0, 3], -40)), origin: want }, "{f:?}");
            }
            let got = Sorter::hub_link().sort(&f);
            match want {
                Origin::Node | Origin::Gateway | Origin::Hub => assert_eq!(got, Sorted::Own, "{f:?}"),
                Origin::Bus => assert_eq!(got, Sorted::Refused { from: None, origin: want }, "{f:?}"),
            }
        }
        // A Via after a Via is refused there too, and lends nothing.
        let mut s = Sorter::hub_link();
        s.sort(&via(1));
        assert!(matches!(s.sort(&via(2)), Sorted::Refused { .. }));
        assert_eq!(s.sort(&hub_fix()), Sorted::Own);
    }

    #[test]
    fn sorter_reset_drops_a_held_via() {
        let mut s = Sorter::new();
        s.sort(&via(1));
        assert!(s.reset());
        assert_eq!(s.sort(&stats(1)), Sorted::Own);
        assert!(!s.reset());
    }

    #[test]
    fn refusal_log_reports_once_a_window_per_sender_and_reason() {
        let mut log = RefusalLog::<3>::new();
        let (a, b, c) = ([1; 6], [2; 6], [3; 6]);
        log.record(a, FORGED_ORIGIN, -50);
        log.record(a, FORGED_ORIGIN, -40);
        log.record(a, FORGED_TABLE_FULL, -45);
        log.record(b, FORGED_ORIGIN, -60); // log full: shares the overflow slot
        log.record(c, FORGED_ORIGIN, -70);
        assert_eq!(log.total, 5);
        assert_eq!(log.drain(500, 1000, |_| panic!("too early")), 0);
        let mut got = std::vec::Vec::new();
        assert_eq!(log.drain(1000, 1000, |r| got.push(r)), 3);
        assert_eq!(got[0], Refusal { source: a, detail: FORGED_ORIGIN, count: 2, rssi: -40, window_ms: 1000 });
        assert_eq!(got[1], Refusal { source: a, detail: FORGED_TABLE_FULL, count: 1, rssi: -45, window_ms: 1000 });
        assert_eq!(got[2], Refusal { source: [0; 6], detail: FORGED_ORIGIN, count: 2, rssi: -60, window_ms: 1000 });
        // A new window starts empty.
        assert_eq!(log.drain(2500, 1000, |_| panic!("nothing new")), 0);
        let t = forged_link(&got[0], [9; 6], 6, 24370, 7);
        assert_eq!((t.kind, t.detail, t.source, t.target, t.count), (ThreatKind::ForgedLink, FORGED_ORIGIN, a, [9; 6], 2));
    }
}
