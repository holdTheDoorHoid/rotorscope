//! The hub's shared state: the track table, node health, own position, and
//! (feature `adsbee`) the ADS-B receiver's health.
//!
//! Everything here lives on the thread-mode executor only (the UART readers on
//! the interrupt executor hand frames over through `links::FRAMES`), so plain
//! `RefCell`/`Cell` suffice and no lock is ever held across an `.await`. That
//! matters: a critical section would block interrupts (and the node UARTs)
//! for as long as a track takes to format.

use core::cell::{Cell, RefCell};

use embassy_time::Instant;
use link::{Frame, NodeRole};
use tracker::{Context, Tracker};

use crate::pins::NODE_LINKS;
use crate::{clock, gnss, links, panel};

/// Track table size. PLAN.md D7: 32 required, 64 goal. 64 fits without PSRAM
/// (see the `"track_bytes"` field of the boot line for the real cost). The
/// C5 hub variant holds D7's 32: with protocol 4's DJI fields (~130 B a
/// track) and signal tables, 64 overflowed its RAM by 6 KB and 48 left the
/// main stack under 8 KB.
pub const MAX_TRACKS: usize = if cfg!(feature = "esp32c5") { 32 } else { 64 };

/// Aircraft table size: the tracker's default with an ADSBee (feature
/// `adsbee`), else none at all, so a hub without one pays no RAM for it.
pub const MAX_AIRCRAFT: usize = if crate::adsbee::ENABLED { tracker::aircraft::MAX_AIRCRAFT } else { 0 };

/// The hub's tracker type.
pub type HubTracker = Tracker<MAX_TRACKS, MAX_AIRCRAFT>;

/// What the hub knows about the ADSBee besides its aircraft.
#[derive(Clone, Copy, Default)]
pub struct AdsbHealth {
    /// Hub time of the last valid CSBee line.
    pub last_ms: Option<u64>,
    /// The last `#S:` statistics line.
    pub stats: Option<adsb::Stats>,
}

/// A node is alive if any frame arrived within this long (Stats come every 1 s,
/// Hello every 5 s).
pub const ALIVE_MS: u64 = 3_000;

/// Open FPV links' identifiers on the outputs (feature `show-link-ids`).
/// There is no runtime settings store yet, and this is a decision about what
/// the unit discloses, so it is fixed when the firmware is built.
pub const SHOW_LINK_IDS: bool = cfg!(feature = "show-link-ids");

/// Milliseconds since boot: the time base for the tracker and every output.
pub fn now_ms() -> u64 {
    Instant::now().as_millis()
}

/// Health of one node link, from its Hello and Stats frames.
#[derive(Default, Clone)]
pub struct NodeStatus {
    pub role: Option<NodeRole>,
    pub firmware: heapless::String<16>,
    pub protocol: u8,
    pub last_heard_ms: Option<u64>,
    /// Previous Stats totals, to turn running totals into rates.
    last_stats: Option<StatsTotals>,
    /// Frames the node saw per second, from its Stats totals. `None` until
    /// two Stats frames have arrived.
    pub seen_per_s: Option<u32>,
    /// The node's own Remote ID count per second, from Stats.
    pub node_rid_per_s: Option<u32>,
    /// Remote ID detections the hub received from this link in the last
    /// second (the state object's `rid_per_s`).
    pub rid_per_s_hub: u32,
    /// Detections received from this link, total; `tick` turns it into a rate.
    pub detections: u32,
    detections_at_last_tick: u32,
    /// The node's own drop total (frames it lost before sending).
    pub node_dropped: u32,
    /// Open FPV link sightings received from this link, total.
    pub open_sightings: u32,
    /// Protocol 4 `Status`: roles the node can take and the plan it runs.
    pub can: Option<u8>,
    pub plan: Option<u16>,
}

/// Nodes relayed through a link (an ESP-NOW gateway, or a P4 hub feeding
/// this board as its web head), kept apart by station address. Eight: the
/// phone page's node list must fit its `MAX_LINE` envelope next to the
/// three links (~150 bytes a node).
pub const MAX_RELAYED: usize = 8;

/// One relayed node: who it is, the link it came through, its health.
#[derive(Clone)]
pub struct Relayed {
    pub mac: [u8; 6],
    pub link: u8,
    /// RSSI the relay last heard it at.
    pub rssi: i8,
    pub status: NodeStatus,
}

#[derive(Clone, Copy)]
struct StatsTotals {
    uptime_us: u64,
    seen: u32,
    rid: u32,
}

impl NodeStatus {
    /// Called once a second by `tick`: detections in the last second.
    pub fn roll_rate(&mut self) {
        self.rid_per_s_hub = self.detections.wrapping_sub(self.detections_at_last_tick);
        self.detections_at_last_tick = self.detections;
    }

    pub fn alive(&self, now: u64) -> bool {
        self.last_heard_ms.is_some_and(|t| now.saturating_sub(t) <= ALIVE_MS)
    }

    /// Remote ID frames the node has found since it booted (its Stats total).
    pub fn rid_total(&self) -> u32 {
        self.last_stats.map_or(0, |s| s.rid)
    }

    /// The node's own uptime, from its last Stats frame.
    pub fn uptime_s(&self) -> u32 {
        self.last_stats.map_or(0, |s| (s.uptime_us / 1_000_000) as u32)
    }

    fn on_stats(&mut self, uptime_us: u64, seen: u32, rid: u32, dropped: u32) {
        let cur = StatsTotals { uptime_us, seen, rid };
        if let Some(prev) = self.last_stats {
            // Rates over the node's own clock, so a delayed frame does not
            // look like a burst. A smaller total or uptime means the node
            // rebooted: wait for the next pair rather than report garbage.
            let dt_us = cur.uptime_us.saturating_sub(prev.uptime_us);
            if dt_us > 0 && cur.seen >= prev.seen && cur.rid >= prev.rid {
                let per_s = |d: u32| ((u64::from(d) * 1_000_000 + dt_us / 2) / dt_us) as u32;
                self.seen_per_s = Some(per_s(cur.seen - prev.seen));
                self.node_rid_per_s = Some(per_s(cur.rid - prev.rid));
            }
        }
        self.last_stats = Some(cur);
        self.node_dropped = dropped;
    }
}

/// Where the own position came from (the `/api/state` `own.source` field).
#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OwnSource {
    Gps,
    Manual,
}

/// The unit's position as reported by `/api/state`.
#[derive(Clone, Copy)]
pub struct OwnPos {
    pub lat: f64,
    pub lon: f64,
    /// Height above the WGS-84 ellipsoid, metres.
    pub alt: Option<f64>,
    pub source: OwnSource,
    /// Hub time the position was set or measured.
    pub at_ms: u64,
}

pub struct Hub {
    pub tracker: RefCell<HubTracker>,
    pub nodes: RefCell<[NodeStatus; NODE_LINKS]>,
    /// Nodes behind a relay. Tracker node number: `NODE_LINKS + index`.
    pub relayed: RefCell<heapless::Vec<Relayed, MAX_RELAYED>>,
    /// Relayed frames from nodes that did not fit in `relayed` (counted
    /// against their link instead).
    pub relayed_full: Cell<u32>,
    pub adsb: Cell<AdsbHealth>,
    /// Set by `POST /api/own-pos` or the USB `p` command. Kept until cleared:
    /// a person typed it in, so it does not go stale like a GPS fix. Not kept
    /// across reboots yet (would need a flash write; see README).
    pub manual_pos: Cell<Option<OwnPos>>,
}

impl Hub {
    pub fn new() -> Self {
        Self {
            tracker: RefCell::new(Tracker::new(tracker::Config {
                // Project policy: open-link identifiers stay hidden unless
                // the unit was built with `--features show-link-ids`.
                show_link_ids: SHOW_LINK_IDS,
                ..tracker::Config::default()
            })),
            nodes: RefCell::new(core::array::from_fn(|_| NodeStatus::default())),
            relayed: RefCell::new(heapless::Vec::new()),
            relayed_full: Cell::new(0),
            adsb: Cell::new(AdsbHealth::default()),
            manual_pos: Cell::new(None),
        }
    }

    /// One CSBee report from the ADSBee (feature `adsbee`, or its demo).
    /// Aircraft go to their own table: never a drone track, never an alert
    /// (`announce_new` is deliberately not called).
    pub fn on_adsb(&self, report: &adsb::Report, now: u64) {
        let mut h = self.adsb.get();
        h.last_ms = Some(now);
        match report {
            adsb::Report::Aircraft(a) => {
                self.tracker.borrow_mut().ingest_aircraft(a, now);
            }
            adsb::Report::Stats(s) => h.stats = Some(*s),
        }
        self.adsb.set(h);
        crate::screen::mark_dirty();
    }

    /// One frame from link `link`; `via` names the node a relay passed it
    /// on for (see `links.rs`), `None` for the link's own frames (a wired
    /// node, a gateway, a P4 hub, or the demo source).
    pub fn on_link_frame(&self, link: u8, via: crate::links::Via, frame: Frame, now: u64) {
        // Tracker node number and the health entry the frame counts for.
        let node = {
            let mut relayed = self.relayed.borrow_mut();
            let mut nodes = self.nodes.borrow_mut();
            let found = via.and_then(|(mac, rssi)| {
                let i = match relayed.iter().position(|r| r.mac == mac) {
                    Some(i) => i,
                    None => {
                        let r = Relayed { mac, link, rssi, status: NodeStatus::default() };
                        if relayed.push(r).is_err() {
                            self.relayed_full.set(self.relayed_full.get().wrapping_add(1));
                            return None;
                        }
                        relayed.len() - 1
                    }
                };
                relayed[i].rssi = rssi;
                relayed[i].link = link;
                Some(i)
            });
            let (node, st) = match found {
                Some(i) => ((NODE_LINKS + i) as u8, Some(&mut relayed[i].status)),
                None => (link, nodes.get_mut(usize::from(link))),
            };
            if let Some(st) = st {
                st.last_heard_ms = Some(now);
                match &frame {
                    Frame::Hello { role, protocol, firmware } => {
                        st.role = Some(*role);
                        st.protocol = *protocol;
                        st.firmware = firmware.clone();
                    }
                    Frame::Stats { uptime_us, frames_seen, rid_found, dropped, .. } => {
                        st.on_stats(*uptime_us, *frames_seen, *rid_found, *dropped);
                    }
                    Frame::Status { role, can, plan } => {
                        st.role = Some(*role);
                        st.can = Some(*can);
                        st.plan = Some(*plan);
                    }
                    Frame::Detection(_) => st.detections = st.detections.wrapping_add(1),
                    Frame::OpenLink(_) => st.open_sightings = st.open_sightings.wrapping_add(1),
                    _ => {}
                }
            }
            node
        };
        match frame {
            // Protocol 5, from a P4 hub this board is the web head of.
            Frame::HubFix(fix) => gnss::set_upstream(&fix, now),
            Frame::Csbee(line) => crate::adsbee::on_forwarded_line(self, &line, now),
            Frame::Detection(det) => {
                self.tracker.borrow_mut().ingest(node, &det, now);
                self.announce_new(now);
            }
            // Open links (kept apart from drones; no alert), protocol 4
            // signals (DroneID, RF, DJI Wi-Fi, Wi-Fi drones) and node
            // Hello/Status, as the P4 hub's tracker takes them.
            other => self.tracker.borrow_mut().ingest_frame(node, &other, now),
        }
        // The screen shows tracks and node health: rebuild its view soon.
        crate::screen::mark_dirty();
    }

    /// Hand each new drone to the screen's alert (banner + buzzer) and the
    /// USB stream, once per drone (O6). Also called at 1 Hz,
    /// because a MAC-only track becomes due only after its grace period.
    pub fn announce_new(&self, now: u64) {
        loop {
            let id = self.tracker.borrow_mut().take_new_drone(now);
            let Some(id) = id else { break };
            panel::new_drone(id);
            crate::usb::event_new_drone(id.0);
        }
    }

    /// The position in use. Policy (the phone page assumes it, see
    /// crates/tracker/README.md): a usable GPS fix wins; the manual position
    /// is only the fallback while there is none.
    pub fn own_pos(&self, now: u64) -> Option<OwnPos> {
        let max_age = self.tracker.borrow().config().own_fix_max_age_ms;
        gnss::latest_fix()
            .filter(|f| now.saturating_sub(f.at_ms) <= max_age)
            .map(|f| OwnPos { lat: f.lat, lon: f.lon, alt: f.alt_hae_m, source: OwnSource::Gps, at_ms: f.at_ms })
            .or(self.manual_pos.get())
    }

    /// Push the current own position into the tracker. Called at 1 Hz and
    /// whenever the position changes. A manual position is re-stamped each
    /// time so the tracker's GPS staleness rule (P4) does not expire it.
    pub fn refresh_own(&self, now: u64) {
        // Decide first: `own_pos` reads the tracker's config.
        let pos = self.own_pos(now);
        let mut t = self.tracker.borrow_mut();
        match pos {
            Some(p) => {
                let stamp = if p.source == OwnSource::Manual { now } else { p.at_ms };
                t.set_own_position(p.lat, p.lon, p.alt, stamp);
            }
            None => t.clear_own_position(),
        }
    }

    pub fn set_manual(&self, lat: f64, lon: f64, alt: Option<f64>, now: u64) {
        self.manual_pos.set(Some(OwnPos { lat, lon, alt, source: OwnSource::Manual, at_ms: now }));
        self.refresh_own(now);
    }

    pub fn clear_manual(&self, now: u64) {
        self.manual_pos.set(None);
        self.refresh_own(now);
    }

    /// Formatter context for this instant.
    pub fn context(&self, now: u64) -> Context {
        self.tracker.borrow().context(now, clock::utc_ms(now))
    }

    /// Every node with a health entry, links first, then relayed nodes:
    /// (status, total drops). Links that never said Hello are left out.
    pub fn for_each_node(&self, mut f: impl FnMut(&NodeStatus, u32)) {
        for (i, st) in self.nodes.borrow().iter().enumerate() {
            if st.role.is_some() {
                f(st, self.link_drops(i));
            }
        }
        for r in self.relayed.borrow().iter() {
            if r.status.role.is_some() {
                f(&r.status, r.status.node_dropped);
            }
        }
    }

    /// Total drops on link `i`: what the node lost plus what the hub lost.
    pub fn link_drops(&self, i: usize) -> u32 {
        let node = self.nodes.borrow().get(i).map_or(0, |s| s.node_dropped);
        node.wrapping_add(links::STATS[i].hub_lost())
    }
}
