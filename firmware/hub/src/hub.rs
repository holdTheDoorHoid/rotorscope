//! The hub's shared state: the track table, node health, own position.
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
/// (see the `"track_bytes"` field of the boot line for the real cost).
pub const MAX_TRACKS: usize = 64;

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
    pub tracker: RefCell<Tracker<MAX_TRACKS>>,
    pub nodes: RefCell<[NodeStatus; NODE_LINKS]>,
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
            manual_pos: Cell::new(None),
        }
    }

    /// One frame from link `node` (or the demo source).
    pub fn on_frame(&self, node: u8, frame: Frame, now: u64) {
        if let Some(st) = self.nodes.borrow_mut().get_mut(usize::from(node)) {
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
                Frame::Detection(_) => st.detections = st.detections.wrapping_add(1),
                Frame::OpenLink(_) => st.open_sightings = st.open_sightings.wrapping_add(1),
            }
        }
        match frame {
            Frame::Detection(det) => {
                self.tracker.borrow_mut().ingest(node, &det, now);
                self.announce_new(now);
            }
            // Kept apart from drones; no alert (there is no position to act on).
            Frame::OpenLink(s) => {
                self.tracker.borrow_mut().ingest_open_link(node, &s, now);
            }
            Frame::Hello { .. } | Frame::Stats { .. } => {}
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

    /// Total drops on link `i`: what the node lost plus what the hub lost.
    pub fn link_drops(&self, i: usize) -> u32 {
        let node = self.nodes.borrow().get(i).map_or(0, |s| s.node_dropped);
        node.wrapping_add(links::STATS[i].hub_lost())
    }
}
