//! The hub's end of the gateway wire, protocol 14 (docs/LINK-KEYS-DESIGN.md
//! 5, 6.3, 13 "Hub (13e) must", 14): everything the hub verifies, seals and
//! sends, with no hardware in it. The firmware (`firmware/hub-p4/src/links.rs`)
//! feeds it decoded [`GwUp`] messages, gives it a [`Sink`] (the UART queue),
//! and calls [`HubLink::tick`] often (the main loop and the screen-band
//! callback, so a long pass cannot open a beacon gap).
//!
//! # Three modes, from the kit records
//!
//! | Mode | Packets from nodes | Beacons, commands, time syncs | Roster to the gateway |
//! |---|---|---|---|
//! | [`Mode::Keyed`] | version 3 Frames verified per roster slot ([`airlink::v3::Hub::open_frames`]); everything else refused | sealed per slot; beacons carry one tag per slot | live (not revoked) roster MACs |
//! | [`Mode::Open`] (provisioned open) | version 1 Frames from anyone; version 3 refused (`not_in_roster`: a keyed node at an open hub) | version 1, unsealed | the addresses heard in the last minute (per-address budgets at the gateway; others share its open lane) |
//! | [`Mode::Closed`] (records missing or damaged, no boot counter) | none: all refused | none | empty |
//!
//! Unprovisioned announcements are counted in every mode (distinct sender
//! addresses over the last minute), and version 2 packets (and the gateway's
//! `old_firmware` refusals) are counted as old node firmware.
//!
//! # Order of messages, and who sends first
//!
//! 1. The gateway sends `Hello` (protocol 14 and up). An older protocol is
//!    refused: no beacons, no commands, `OLD FIRMWARE: gateway` on screen.
//!    Nothing is sealed before a `Hello`: every tag covers the gateway's MAC.
//! 2. On every `Hello` (the gateway sends one at boot and every 5 s, and
//!    relays no keyed node packet until it has a roster) and every
//!    [`ROSTER_MS`] the hub sends `Roster`.
//! 3. Once in every [`BEACON_MS`], at a place in it that varies
//!    ([`beacon_place`]), the hub seals a fresh `Beacon` (the gateway
//!    never repeats one). An empty roster on a keyed hub makes no beacon.
//! 4. A node's first verified batch of a new session gets a `Sync` echoing
//!    it (once; again after [`SYNC_RETRY_MS`] if the gateway reports it not
//!    radio-acknowledged). A genuine batch refused for its epoch or age
//!    ([`airlink::v3::HubVerdict::wants_sync`]) gets a `Sync` echoing it, at
//!    most one per slot per [`SYNC_ANSWER_MS`].
//! 5. `TimeSync` goes only to a slot whose current session got a
//!    radio-acknowledged `Sync`, alone in its own Command, sealed the moment
//!    it is handed to the UART queue ([`HubLink::time_sync_step`], one per
//!    call, called only while the queue is empty), as an `urgent` Transmit
//!    with [`TIMESYNC_GIVE_UP_MS`].
//! 6. Commands (`Assign`) are sealed per node; a newer one sends `Cancel`
//!    first while the previous one is still in flight.

use airlink::v3::{self, Hub, HubVerdict, Kind, Reject};
use heapless::{String, Vec};
use kitkeys::{Mac, SLOTS};
use link::gw::{self, GwDown, GwStats, GwUp, MAX_GW_FRAME, RefuseReason};
use link::origin::{Refusal, RefusalLog};

/// One beacon in every stretch of this long on the hub's clock: two a
/// second.
pub const BEACON_MS: u64 = 500;

/// Where in its [`BEACON_MS`] stretch beacon number `seq` goes: a place that
/// differs from one beacon to the next, from the number alone (no random
/// source needed; the order only has to be unlike a clock).
///
/// A node in the Bluetooth role lends the radio to the link for 200 ms in
/// every second. Beacons exactly 500 ms apart keep two fixed places in that
/// second; a node whose 200 ms held neither heard none for over a minute
/// (bench, 2026-11-19: 74 s after an image switch, and 2 restarts in 5 found
/// no gateway in 29 s). Placed afresh each time, every beacon has an even
/// chance of landing in any listening window.
pub fn beacon_place(seq: u16) -> u64 {
    let mut h = u32::from(seq).wrapping_mul(0x9E37_79B1);
    h ^= h >> 15;
    h = h.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 13;
    u64::from(h) % BEACON_MS
}
/// The roster again this often (and at once on a gateway `Hello`).
pub const ROSTER_MS: u64 = 5_000;
/// At most one answering `Sync` per slot this often.
pub const SYNC_ANSWER_MS: u64 = 1_000;
/// For this long after the hub's link starts, a node may still seal under
/// the hub's previous boot: it relinks within 60 s at worst. A genuine
/// old-boot batch from a slot not yet accepted in this boot is, in that
/// time, a node catching up and not forged traffic ([`Counters::catching_up`]).
pub const STALE_GRACE_MS: u64 = 90_000;
/// A first `Sync` not radio-acknowledged is sent again after this long.
pub const SYNC_RETRY_MS: u64 = 2_000;
/// Commands and syncs: the gateway retries this long (inside the node's 3 s).
pub const COMMAND_GIVE_UP_MS: u16 = 2_000;
/// Time syncs: retried this long (inside the node's 0.5 s).
pub const TIMESYNC_GIVE_UP_MS: u16 = 300;
/// Unprovisioned and old-firmware senders count for this long.
pub const SEEN_MS: u64 = 60_000;
/// The hub's own refusals are reported at most once per this window per
/// sender and reason.
pub const REFUSAL_WINDOW_MS: u32 = 1_000;
/// The first protocol a keyed kit's gateway may speak.
pub const MIN_GATEWAY_PROTOCOL: u8 = link::PROTOCOL_VERSION;

const _: () = assert!(gw::MAX_PACKET == airlink::MAX_PACKET);
const _: () = assert!(gw::MAX_BEACON >= v3::BEACON_MAX);

/// Where the hub's messages to the gateway go: the UART queue. All or
/// nothing; false when there is no room (counted, nothing half-sent).
pub trait Sink {
    fn send(&mut self, bytes: &[u8]) -> bool;
}

impl<F: FnMut(&[u8]) -> bool> Sink for F {
    fn send(&mut self, bytes: &[u8]) -> bool {
        self(bytes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Keyed,
    Open,
    Closed,
}

/// What the firmware does with a message ([`HubLink::on_up`]).
#[derive(Debug)]
pub enum Up<'a> {
    /// A batch to hand on: verified for roster slot `slot` (keyed), or an
    /// open kit's unsealed batch (`slot` `None`). `payload` is COBS frames;
    /// sort them with `link::origin::node_batch` (a verified node may still
    /// only send node frames).
    Batch { slot: Option<u8>, mac: Mac, rssi: i8, payload: &'a [u8] },
    /// The gateway's `Hello`: accepted (protocol 14 or later) or refused.
    Gateway { accepted: bool, protocol: u8 },
    /// The gateway dropped packets: report as a `ForgedLink` threat (the
    /// gateway already sends these at most once a second per sender).
    Refused(Refusal),
}

/// Running totals, for the console.
#[derive(Debug, Clone, Copy, Default)]
pub struct Counters {
    pub packets: u32,
    pub accepted: u32,
    pub duplicates: u32,
    /// The hub's own refusals, per `airlink::v3::Reject` (`Reject::index`).
    pub rejects: [u32; v3::N_REJECT],
    pub open_batches: u32,
    pub unprovisioned: u32,
    pub beacons: u32,
    pub rosters: u32,
    pub syncs: u32,
    pub sync_answers: u32,
    /// Of `rejects[Stale]`: genuine batches from a node still following the
    /// hub's previous boot, soon after this boot started. Answered with a
    /// Sync and not reported as forged traffic.
    pub catching_up: u32,
    pub commands: u32,
    /// Commands not sent: no such node, not heard yet this hub boot, too
    /// big, not whole frames, closed kit.
    pub command_refused: u32,
    pub cancels: u32,
    pub time_syncs: u32,
    pub tx_acked: u32,
    pub tx_given_up: u32,
    /// Longest a time sync waited in the gateway, ms (`TxDone::held_ms`).
    pub time_sync_held_max_ms: u16,
    /// Messages the UART queue had no room for.
    pub sink_full: u32,
    /// Sealing failed (no gateway yet, clock exhausted).
    pub seal_errors: u32,
    pub hellos: u32,
    /// Packets the gateway refused, per `RefuseReason` (summed counts).
    pub gw_refused: [u32; RefuseReason::COUNT],
}

/// The gateway as its `Hello` describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gateway {
    pub mac: Mac,
    pub channel: u8,
    pub protocol: u8,
    pub firmware: String<16>,
}

#[derive(Debug, Clone, Copy, Default)]
struct SlotLink {
    mac: Mac,
    /// The node's newest session heard (from a verified batch).
    session: Option<u32>,
    /// The last Sync sent for a session: (session, radio-acked, at ms).
    synced: Option<(u32, bool, u64)>,
    /// When the last Sync went (answers and first syncs share the limit).
    sync_at: Option<u64>,
    seq: u16,
    /// A Command for this node still queued at the gateway (its id).
    command: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TxKind {
    Sync { session: u32 },
    Command,
    TimeSync,
}

#[derive(Debug, Clone, Copy)]
struct Inflight {
    id: u16,
    slot: Option<u8>,
    kind: TxKind,
}

/// Distinct sender addresses heard over the last [`SEEN_MS`].
#[derive(Debug, Clone, Default)]
pub struct Seen<const N: usize> {
    v: Vec<(Mac, u64), N>,
}

impl<const N: usize> Seen<N> {
    pub const fn new() -> Self {
        Self { v: Vec::new() }
    }

    pub fn note(&mut self, mac: Mac, now: u64) {
        if let Some(e) = self.v.iter_mut().find(|e| e.0 == mac) {
            e.1 = now;
            return;
        }
        if self.v.is_full() {
            // The one heard longest ago makes room.
            if let Some(i) = (0..self.v.len()).min_by_key(|&i| self.v[i].1) {
                self.v.swap_remove(i);
            }
        }
        let _ = self.v.push((mac, now));
    }

    /// How many were heard within [`SEEN_MS`] of `now`.
    pub fn count(&self, now: u64) -> u8 {
        self.v.iter().filter(|e| now.saturating_sub(e.1) < SEEN_MS).count() as u8
    }

    pub fn recent(&self, now: u64) -> impl Iterator<Item = Mac> + '_ {
        self.v.iter().filter(move |e| now.saturating_sub(e.1) < SEEN_MS).map(|e| e.0)
    }
}

/// What the screen needs ([`HubLink::view`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct View {
    /// The gateway's `Hello` named an older protocol.
    pub old_gateway: bool,
    /// Nodes heard with version 2 (old) firmware in the last minute.
    pub old_nodes: u8,
    /// Distinct unprovisioned nodes announced in the last minute.
    pub unprovisioned: u8,
    /// The gateway is a bench build that forges traffic on purpose
    /// (`gwforge`: its firmware string starts "gw FORGE").
    pub bench_gateway: bool,
}

/// The firmware string a `gwforge` (bench) gateway sends in its Hello.
pub const BENCH_GATEWAY_PREFIX: &str = "gw FORGE";

pub struct HubLink {
    mode: Mode,
    hub: Hub,
    slots: [Option<SlotLink>; SLOTS],
    gateway: Option<Gateway>,
    old_gateway: Option<u8>,
    beacon_seq: u16,
    last_beacon: Option<u64>,
    /// When the next beacon is due ([`beacon_place`]); `None`: now.
    next_beacon: Option<u64>,
    /// The [`BEACON_MS`] stretch of the clock the last beacon belonged to.
    beacon_stretch: u64,
    roster_at: Option<u64>,
    /// When this link first ran (hub uptime, ms): the start of
    /// [`STALE_GRACE_MS`].
    born: Option<u64>,
    next_id: u16,
    inflight: Vec<Inflight, 32>,
    refusals: RefusalLog<16>,
    unprov: Seen<16>,
    old_nodes: Seen<16>,
    /// Open kit: who sent batches (time syncs go to them).
    open_nodes: Seen<16>,
    open_seq: u16,
    /// A time-sync round: slots (keyed) or open-node addresses still to do.
    ts_slots: u32,
    ts_open: Vec<Mac, 16>,
    pub stats: Counters,
    /// The gateway's latest `Stats`.
    pub gw_stats: Option<GwStats>,
    pkt: [u8; airlink::MAX_PACKET],
    enc: [u8; MAX_GW_FRAME],
}

impl HubLink {
    /// A link that takes nothing and sends nothing (records missing or
    /// damaged). [`HubLink::open`] / [`HubLink::start_keyed`] change it.
    pub fn closed() -> Self {
        HubLink {
            mode: Mode::Closed,
            hub: Hub::new(0),
            slots: [None; SLOTS],
            gateway: None,
            old_gateway: None,
            beacon_seq: 0,
            last_beacon: None,
            next_beacon: None,
            beacon_stretch: 0,
            roster_at: None,
            born: None,
            next_id: 0,
            inflight: Vec::new(),
            refusals: RefusalLog::new(),
            unprov: Seen::new(),
            old_nodes: Seen::new(),
            open_nodes: Seen::new(),
            open_seq: 0,
            ts_slots: 0,
            ts_open: Vec::new(),
            stats: Counters::default(),
            gw_stats: None,
            pkt: [0; airlink::MAX_PACKET],
            enc: [0; MAX_GW_FRAME],
        }
    }

    /// A kit provisioned open: unauthenticated link, on purpose.
    pub fn open(&mut self) {
        self.mode = Mode::Open;
    }

    /// Keyed, with this boot's epoch (the `rsboot` value). Returns the hub
    /// to load the roster's slots into (`kit::load_slots`); then call
    /// [`HubLink::slots_loaded`].
    pub fn start_keyed(&mut self, epoch: u32) -> &mut Hub {
        self.mode = Mode::Keyed;
        self.hub = Hub::new(epoch);
        &mut self.hub
    }

    /// After loading: the live slots and their MACs (only those the hub
    /// took are kept).
    pub fn slots_loaded(&mut self, slots: impl IntoIterator<Item = (u8, Mac)>) {
        for (slot, mac) in slots {
            if self.hub.slot_of(&mac) == Some(slot)
                && let Some(s) = self.slots.get_mut(usize::from(slot))
            {
                *s = Some(SlotLink { mac, ..Default::default() });
            }
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn epoch(&self) -> Option<u32> {
        (self.mode == Mode::Keyed).then(|| self.hub.epoch())
    }

    pub fn gateway(&self) -> Option<&Gateway> {
        self.gateway.as_ref()
    }

    /// The protocol of a refused (older) gateway.
    pub fn old_gateway(&self) -> Option<u8> {
        self.old_gateway
    }

    /// An older gateway seen another way (one that speaks `link::Frame`s
    /// and says protocol 13 or less in its `Hello`).
    pub fn note_old_gateway(&mut self, protocol: u8) {
        self.old_gateway = Some(protocol);
    }

    /// The node number of a roster slot on the hub (`slot + 1`; 0 is the
    /// link itself, as for a wired node).
    pub const fn node_number(slot: u8) -> u8 {
        slot + 1
    }

    /// The MAC in a live slot.
    pub fn slot_mac(&self, slot: u8) -> Option<Mac> {
        self.slots.get(usize::from(slot)).and_then(|s| s.map(|s| s.mac))
    }

    pub fn view(&self, now: u64) -> View {
        View {
            old_gateway: self.old_gateway.is_some(),
            old_nodes: self.old_nodes.count(now),
            unprovisioned: self.unprov.count(now),
            bench_gateway: self.gateway.as_ref().is_some_and(|g| g.firmware.starts_with(BENCH_GATEWAY_PREFIX)),
        }
    }

    // ------------------------------------------------------- from the gateway --

    /// One message from the gateway, at hub uptime `now` ms.
    pub fn on_up<'a>(&mut self, m: GwUp<'a>, now: u64, sink: &mut impl Sink, out: &mut impl FnMut(Up<'a>)) {
        self.born.get_or_insert(now);
        match m {
            GwUp::Hello { protocol, firmware, mac, channel } => {
                self.stats.hellos += 1;
                if protocol < MIN_GATEWAY_PROTOCOL {
                    self.old_gateway = Some(protocol);
                    self.gateway = None;
                    out(Up::Gateway { accepted: false, protocol });
                    return;
                }
                self.old_gateway = None;
                let new = self.gateway.as_ref().is_none_or(|g| g.mac != mac || g.channel != channel);
                self.gateway = Some(Gateway { mac, channel, protocol, firmware });
                self.hub.set_gateway(mac);
                if new {
                    self.last_beacon = None;
                    self.next_beacon = None;
                }
                // The roster at every Hello: a gateway that rebooted (same
                // address) relays no node packet until it has one.
                self.roster_at = None;
                out(Up::Gateway { accepted: true, protocol });
                self.tick_roster(now, sink);
            }
            GwUp::Packet { src, rssi, held_us: _, data } => self.packet(src, rssi, data, now, sink, out),
            GwUp::TxDone { id, to: _, radio_acked, attempts: _, held_ms } => {
                if radio_acked {
                    self.stats.tx_acked += 1;
                } else {
                    self.stats.tx_given_up += 1;
                }
                let Some(i) = self.inflight.iter().position(|f| f.id == id) else { return };
                let f = self.inflight.swap_remove(i);
                let Some(s) = f.slot.and_then(|s| self.slots.get_mut(usize::from(s))).and_then(Option::as_mut) else { return };
                match f.kind {
                    TxKind::Sync { session } => {
                        if let Some((ss, acked, _)) = s.synced.as_mut()
                            && *ss == session
                        {
                            *acked |= radio_acked;
                        }
                    }
                    TxKind::Command => {
                        if s.command == Some(id) {
                            s.command = None;
                        }
                    }
                    TxKind::TimeSync => self.stats.time_sync_held_max_ms = self.stats.time_sync_held_max_ms.max(held_ms),
                }
            }
            GwUp::Refused { src, reason, count, rssi, window_ms } => {
                let r = &mut self.stats.gw_refused[reason.index()];
                *r = r.saturating_add(count);
                if reason == RefuseReason::OldFirmware && src != [0; 6] {
                    self.old_nodes.note(src, now);
                }
                out(Up::Refused(Refusal { source: src, detail: reason.detail(), count, rssi, window_ms }));
            }
            GwUp::Stats(s) => self.gw_stats = Some(s),
        }
    }

    fn refuse(&mut self, src: Mac, reason: Reject, rssi: i8) {
        let c = &mut self.stats.rejects[reason.index()];
        *c = c.saturating_add(1);
        if reason != Reject::Foreign {
            self.refusals.record(src, reason.code(), rssi);
        }
    }

    /// Whether a genuine old-boot batch for `slot` at `now` is a node
    /// catching up after a hub restart: nothing from that slot has been
    /// accepted in this boot yet, and the boot is younger than
    /// [`STALE_GRACE_MS`]. After either, old-boot traffic is a replay.
    fn catching_up(&self, slot: Option<u8>, now: u64) -> bool {
        let fresh_boot = self.born.is_some_and(|b| now.saturating_sub(b) < STALE_GRACE_MS);
        let unheard = slot.and_then(|s| self.slots.get(usize::from(s))).and_then(Option::as_ref).is_some_and(|s| s.session.is_none());
        fresh_boot && unheard
    }

    fn packet<'a>(&mut self, src: Mac, rssi: i8, data: &'a [u8], now: u64, sink: &mut impl Sink, out: &mut impl FnMut(Up<'a>)) {
        self.stats.packets += 1;
        let shape = v3::inspect(data);
        if let Ok(s) = shape
            && s.kind == Kind::Unprovisioned
        {
            self.stats.unprovisioned += 1;
            self.unprov.note(src, now);
            return;
        }
        if shape == Err(Reject::OldFirmware) {
            self.old_nodes.note(src, now);
        }
        match self.mode {
            Mode::Keyed => {}
            Mode::Open => {
                match (shape, airlink::parse(data)) {
                    (_, Some(p)) if p.kind == airlink::Kind::Frames => {
                        self.stats.open_batches += 1;
                        self.open_nodes.note(src, now);
                        out(Up::Batch { slot: None, mac: src, rssi, payload: p.payload });
                    }
                    (Err(Reject::Unauthenticated), _) => self.refuse(src, Reject::Malformed, rssi),
                    (Err(r), _) => self.refuse(src, r, rssi),
                    // A keyed (version 3) node at an open hub.
                    (Ok(_), _) => self.refuse(src, Reject::NotInRoster, rssi),
                }
                return;
            }
            Mode::Closed => {
                let r = match shape {
                    Err(r @ (Reject::OldFirmware | Reject::Foreign | Reject::Malformed)) => r,
                    _ => Reject::NotInRoster,
                };
                self.refuse(src, r, rssi);
                return;
            }
        }
        let v = self.hub.open_frames(&src, data, now);
        match v {
            HubVerdict::Accept(u) => {
                self.stats.accepted += 1;
                if let Some(s) = self.slots.get_mut(usize::from(u.slot)).and_then(Option::as_mut) {
                    s.session = Some(u.stamp.session);
                }
                out(Up::Batch { slot: Some(u.slot), mac: src, rssi, payload: u.payload });
                self.first_sync(u.slot, now, sink);
            }
            HubVerdict::Duplicate { .. } => self.stats.duplicates += 1,
            HubVerdict::Reject { reason, slot, genuine } => {
                if let Some((slot, echo)) = v.wants_sync() {
                    self.answer_sync(slot, echo, now, sink);
                }
                if reason == Reject::Stale && genuine.is_some() && self.catching_up(slot, now) {
                    // A node still following the hub's previous boot: the
                    // Sync above moves it. Counted, not reported.
                    let c = &mut self.stats.rejects[reason.index()];
                    *c = c.saturating_add(1);
                    self.stats.catching_up += 1;
                } else {
                    self.refuse(src, reason, rssi);
                }
            }
        }
    }

    // ------------------------------------------------------------- to the gateway --

    fn id(&mut self) -> u16 {
        self.next_id = self.next_id.wrapping_add(1);
        self.next_id
    }

    fn track(&mut self, id: u16, slot: Option<u8>, kind: TxKind) {
        if self.inflight.is_full() {
            self.inflight.remove(0);
        }
        let _ = self.inflight.push(Inflight { id, slot, kind });
    }

    /// Encode and queue one message.
    fn put(enc: &mut [u8; MAX_GW_FRAME], stats: &mut Counters, sink: &mut impl Sink, m: &GwDown<'_>) -> bool {
        let Ok(b) = gw::encode_down(m, enc) else {
            stats.seal_errors += 1;
            return false;
        };
        if sink.send(b) {
            true
        } else {
            stats.sink_full += 1;
            false
        }
    }

    /// Queue `pkt[..n]` as a Transmit to `to`.
    fn transmit(&mut self, to: Mac, n: usize, urgent: bool, give_up_ms: u16, sink: &mut impl Sink) -> Option<u16> {
        let id = self.id();
        let m = GwDown::Transmit { id, to, urgent, give_up_ms, data: &self.pkt[..n] };
        Self::put(&mut self.enc, &mut self.stats, sink, &m).then_some(id)
    }

    fn channel(&self) -> u8 {
        self.gateway.as_ref().map_or(airlink::GATEWAY_CHANNEL, |g| g.channel)
    }

    /// Send slot `slot` a Sync echoing `echo`, if its rate allows.
    fn sync(&mut self, slot: u8, echo: (u32, u32), now: u64, sink: &mut impl Sink) -> bool {
        let channel = self.channel();
        let Some(s) = self.slots.get_mut(usize::from(slot)).and_then(Option::as_mut) else { return false };
        if s.sync_at.is_some_and(|t| now.saturating_sub(t) < SYNC_ANSWER_MS) {
            return false;
        }
        s.seq = s.seq.wrapping_add(1);
        let (seq, mac) = (s.seq, s.mac);
        let n = match self.hub.seal_sync(slot, echo, &mut self.pkt, seq, channel, now) {
            Ok(n) => n,
            Err(_) => {
                self.stats.seal_errors += 1;
                return false;
            }
        };
        let Some(id) = self.transmit(mac, n, false, COMMAND_GIVE_UP_MS, sink) else { return false };
        self.track(id, Some(slot), TxKind::Sync { session: echo.0 });
        let s = self.slots[usize::from(slot)].as_mut().expect("checked");
        s.sync_at = Some(now);
        s.synced = Some((echo.0, false, now));
        self.stats.syncs += 1;
        true
    }

    fn answer_sync(&mut self, slot: u8, echo: (u32, u32), now: u64, sink: &mut impl Sink) {
        if self.sync(slot, echo, now, sink) {
            self.stats.sync_answers += 1;
        }
    }

    /// A Sync for a slot whose current session has none acknowledged yet.
    fn first_sync(&mut self, slot: u8, now: u64, sink: &mut impl Sink) {
        let Some(s) = self.slots.get(usize::from(slot)).and_then(Option::as_ref) else { return };
        let Some(session) = s.session else { return };
        let due = match s.synced {
            Some((ss, acked, at)) if ss == session => !acked && now.saturating_sub(at) >= SYNC_RETRY_MS,
            _ => true,
        };
        if due && let Some(echo) = self.hub.echo(slot) {
            self.sync(slot, echo, now, sink);
        }
    }

    /// Timers: beacon, roster, first syncs still owed, refusal reports.
    /// Call from the main loop and between screen bands.
    pub fn tick(&mut self, now: u64, sink: &mut impl Sink, mut on_refusal: impl FnMut(Refusal)) {
        self.born.get_or_insert(now);
        self.tick_beacon(now, sink);
        self.tick_roster(now, sink);
        for slot in 0..SLOTS as u8 {
            self.first_sync(slot, now, sink);
        }
        self.refusals.drain(now, REFUSAL_WINDOW_MS, &mut on_refusal);
    }

    /// Whether a beacon is due at `now`.
    pub fn beacon_due(&self, now: u64) -> bool {
        self.next_beacon.is_none_or(|t| now >= t)
    }

    /// Send a beacon now if the last one is at least `min_ms` old: before a
    /// step known to block the loop (the SD card mount), so the gap is that
    /// step plus `min_ms`, not that step plus a whole period.
    pub fn beacon_if_older(&mut self, now: u64, min_ms: u64, sink: &mut impl Sink) {
        if self.last_beacon.is_none_or(|t| now.saturating_sub(t) >= min_ms) {
            self.next_beacon = None;
            self.tick_beacon(now, sink);
        }
    }

    fn tick_beacon(&mut self, now: u64, sink: &mut impl Sink) {
        if !self.beacon_due(now) || self.gateway.is_none() {
            return;
        }
        self.beacon_seq = self.beacon_seq.wrapping_add(1);
        let (seq, channel) = (self.beacon_seq, self.channel());
        let n = match self.mode {
            Mode::Keyed => match self.hub.seal_beacon(&mut self.pkt, seq, channel, now) {
                Ok(n) => n,
                Err(_) => {
                    // No slots (nothing to tag) or no clock: nothing to send.
                    self.beacon_sent(now);
                    return;
                }
            },
            Mode::Open => airlink::beacon(&mut self.pkt, seq, channel).len(),
            Mode::Closed => return,
        };
        let m = GwDown::Beacon(&self.pkt[..n]);
        if Self::put(&mut self.enc, &mut self.stats, sink, &m) {
            self.stats.beacons += 1;
            self.beacon_sent(now);
        }
    }

    /// A beacon went (or there was none to make) at `now`: the next goes in
    /// the next [`BEACON_MS`] stretch of the clock, at its own place in it.
    fn beacon_sent(&mut self, now: u64) {
        self.last_beacon = Some(now);
        // The stretch after the one this beacon belonged to; if the loop was
        // held up past that one's place too, the first whose place is ahead.
        let place = beacon_place(self.beacon_seq);
        let mut k = (self.beacon_stretch + 1).max(now / BEACON_MS);
        if k * BEACON_MS + place <= now {
            k += 1;
        }
        self.beacon_stretch = k;
        self.next_beacon = Some(k * BEACON_MS + place);
    }

    fn tick_roster(&mut self, now: u64, sink: &mut impl Sink) {
        if self.gateway.is_none() || self.roster_at.is_some_and(|t| now.saturating_sub(t) < ROSTER_MS) {
            return;
        }
        let mut macs: Vec<Mac, { gw::MAX_ROSTER }> = Vec::new();
        match self.mode {
            Mode::Keyed => {
                for s in self.slots.iter().flatten() {
                    let _ = macs.push(s.mac);
                }
            }
            // An open kit has no roster; the nodes heard lately get the
            // gateway's per-address budgets instead of its shared lane.
            Mode::Open => {
                for m in self.open_nodes.recent(now) {
                    let _ = macs.push(m);
                }
            }
            Mode::Closed => {}
        }
        if Self::put(&mut self.enc, &mut self.stats, sink, &GwDown::Roster(macs)) {
            self.stats.rosters += 1;
            self.roster_at = Some(now);
        }
    }

    /// Seal `frames` (whole COBS command frames, without the `To`) for the
    /// node `to` and queue it. A command still in flight for that node is
    /// cancelled first.
    pub fn command(&mut self, to: Mac, frames: &[u8], now: u64, sink: &mut impl Sink) -> Result<(), CommandError> {
        if self.gateway.is_none() {
            self.stats.command_refused += 1;
            return Err(CommandError::NoGateway);
        }
        let (slot, n) = match self.mode {
            Mode::Closed => {
                self.stats.command_refused += 1;
                return Err(CommandError::Closed);
            }
            Mode::Open => {
                self.open_seq = self.open_seq.wrapping_add(1);
                match airlink::command(&mut self.pkt, self.open_seq, frames) {
                    Some(p) => (None, p.len()),
                    None => {
                        self.stats.command_refused += 1;
                        return Err(CommandError::Seal(v3::SealError::TooBig));
                    }
                }
            }
            Mode::Keyed => {
                let Some(slot) = self.hub.slot_of(&to) else {
                    self.stats.command_refused += 1;
                    return Err(CommandError::NotInRoster);
                };
                let s = self.slots[usize::from(slot)].as_mut().expect("hub and slots agree");
                s.seq = s.seq.wrapping_add(1);
                let seq = s.seq;
                match self.hub.seal_command(slot, &mut self.pkt, seq, frames, now) {
                    Ok(n) => (Some(slot), n),
                    Err(e) => {
                        self.stats.command_refused += 1;
                        return Err(CommandError::Seal(e));
                    }
                }
            }
        };
        if let Some(s) = slot.and_then(|s| self.slots[usize::from(s)].as_mut())
            && s.command.take().is_some()
            && Self::put(&mut self.enc, &mut self.stats, sink, &GwDown::Cancel { to })
        {
            self.stats.cancels += 1;
        }
        let id = self.transmit(to, n, false, COMMAND_GIVE_UP_MS, sink).ok_or(CommandError::QueueFull)?;
        self.track(id, slot, TxKind::Command);
        if let Some(s) = slot.and_then(|s| self.slots[usize::from(s)].as_mut()) {
            s.command = Some(id);
        }
        self.stats.commands += 1;
        Ok(())
    }

    /// Start a time-sync round (once a second, from the GPS time): every
    /// slot whose current session has an acknowledged Sync, or (open kit)
    /// every node heard in the last minute. A round not finished is replaced.
    pub fn time_sync_round(&mut self, now: u64) {
        self.ts_slots = 0;
        self.ts_open.clear();
        if self.gateway.is_none() {
            return;
        }
        match self.mode {
            Mode::Keyed => {
                for (i, s) in self.slots.iter().enumerate() {
                    if let Some(s) = s
                        && let (Some(sess), Some((ss, true, _))) = (s.session, s.synced)
                        && sess == ss
                    {
                        self.ts_slots |= 1 << i;
                    }
                }
            }
            Mode::Open => {
                for m in self.open_nodes.recent(now) {
                    let _ = self.ts_open.push(m);
                }
            }
            Mode::Closed => {}
        }
    }

    /// Whether a time-sync round still has nodes to do.
    pub fn time_sync_pending(&self) -> bool {
        self.ts_slots != 0 || !self.ts_open.is_empty()
    }

    /// One `TimeSync` of the round, alone in its own Command, sealed now:
    /// call only when the UART queue is empty, so it goes on the wire at
    /// once (`utc_ms` is UTC at this moment). True if one was queued.
    pub fn time_sync_step(&mut self, now: u64, utc_ms: u64, pps: bool, sink: &mut impl Sink) -> bool {
        let mut frame = [0u8; 24];
        let Ok(f) = link::encode_command(&link::Command::TimeSync { utc_ms, pps }, &mut frame) else { return false };
        let flen = f.len();
        let (slot, to, n) = if self.ts_slots != 0 {
            let slot = self.ts_slots.trailing_zeros() as u8;
            self.ts_slots &= !(1 << slot);
            let Some(s) = self.slots[usize::from(slot)].as_mut() else { return false };
            s.seq = s.seq.wrapping_add(1);
            let (seq, mac) = (s.seq, s.mac);
            match self.hub.seal_command(slot, &mut self.pkt, seq, &frame[..flen], now) {
                Ok(n) => (Some(slot), mac, n),
                Err(_) => {
                    self.stats.seal_errors += 1;
                    return false;
                }
            }
        } else if let Some(mac) = self.ts_open.pop() {
            self.open_seq = self.open_seq.wrapping_add(1);
            match airlink::command(&mut self.pkt, self.open_seq, &frame[..flen]) {
                Some(p) => (None, mac, p.len()),
                None => return false,
            }
        } else {
            return false;
        };
        let Some(id) = self.transmit(to, n, true, TIMESYNC_GIVE_UP_MS, sink) else { return false };
        self.track(id, slot, TxKind::TimeSync);
        self.stats.time_syncs += 1;
        true
    }
}

/// Split what the coverage coordinator writes for one node (`To { node }`
/// then whole command frames, as for the bus or an old gateway) into the
/// node and the frames to seal for it. `None` if it does not start with a
/// whole `To`.
pub fn split_to(bytes: &[u8]) -> Option<(Mac, &[u8])> {
    let end = bytes.iter().position(|&b| b == 0)?;
    let mut to = [0u8; 16];
    let t = to.get_mut(..=end)?;
    t.copy_from_slice(&bytes[..=end]);
    match postcard::from_bytes_cobs::<link::Command>(t) {
        Ok(link::Command::To { node }) => Some((node, &bytes[end + 1..])),
        _ => None,
    }
}

/// Why a command was not sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandError {
    NoGateway,
    Closed,
    NotInRoster,
    Seal(v3::SealError),
    QueueFull,
}

#[cfg(test)]
#[path = "link_tests.rs"]
mod tests;
