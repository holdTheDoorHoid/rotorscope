//! The `/api/state` body, streamed, and the `/api/own-pos` request.
//!
//! `tracker::json::state` writes the whole object into one buffer, which for 64
//! tracks is up to ~99 KB: it does not fit next to Wi-Fi on an S3 without
//! PSRAM. So the hub sends the same bytes in pieces:
//!
//! 1. the envelope: `json::state` called with **no links, tracks or
//!    aircraft**, which yields `{...,"open_links":[],"tracks":[],"aircraft":[]}\n`;
//!    the trailing `],"tracks":[],"aircraft":[]}\n` is cut off;
//! 2. each open link as `json::link_line` without its `\n`, comma-separated;
//! 3. `],"tracks":[` (`json::STATE_LINKS_TO_TRACKS`);
//! 4. each track as `json::track_line` without its `\n`, comma-separated;
//! 5. `],"aircraft":[` (`json::STATE_TRACKS_TO_AIRCRAFT`);
//! 6. each aircraft as `json::aircraft_line` without its `\n` (none unless
//!    built with `adsbee`);
//! 7. `]}\n` (`json::STATE_END`).
//!
//! That is byte-for-byte what `json::state` produces for the same tracks and
//! context (it builds the object the same way), and `check_against_reference`
//! verifies it on the device (USB `s`, and once at boot in `demo` builds).
//! Each piece fits in `json::MAX_LINE`, and the tracker is borrowed only while
//! a piece is formatted, never across a socket write.

use tracker::json::{self, GpsStatus, HubStatus, MAX_LINE, ManualPos, NodeStatus, OwnPosRequest};
use tracker::openlinks::MAX_LINK_TRACKS;
use tracker::{AircraftKey, Context, LinkTrackId, TrackId};

use crate::hub::{Hub, MAX_AIRCRAFT, MAX_RELAYED, MAX_TRACKS, OwnSource};
use crate::pins::NODE_LINKS;
use crate::{adsbee, gnss, sd};

/// What `json::state` puts after the envelope when there are no links,
/// tracks or aircraft: `],"tracks":[],"aircraft":[]}\n`.
const EMPTY_TAIL: usize =
    json::STATE_LINKS_TO_TRACKS.len() + json::STATE_TRACKS_TO_AIRCRAFT.len() + json::STATE_END.len();

/// Node entries for the state object: one per link that has said Hello,
/// then one per relayed node (behind a gateway or a P4 hub) that has.
fn node_statuses(hub: &Hub, now: u64) -> heapless::Vec<NodeStatus, { NODE_LINKS + MAX_RELAYED }> {
    let mut out = heapless::Vec::new();
    hub.for_each_node(|s, drops| {
        let Some(role) = s.role else { return };
        let alive = s.alive(now);
        let _ = out.push(NodeStatus {
            role,
            alive,
            age_ms: s.last_heard_ms.map(|t| now.saturating_sub(t)),
            // A dead node's last rate would be a lie.
            seen_per_s: s.seen_per_s.map(|r| if alive { r } else { 0 }),
            rid_per_s: if alive { s.rid_per_s_hub } else { 0 },
            drops: Some(drops),
            can: s.can,
            plan: s.plan,
        });
    });
    out
}

/// Write the envelope (everything before the first track) into `buf`.
fn envelope(hub: &Hub, ctx: &Context, buf: &mut [u8]) -> usize {
    let now = ctx.now_ms;
    let nodes = node_statuses(hub, now);
    let source = match hub.own_pos(now).map(|p| p.source) {
        Some(OwnSource::Manual) => json::OwnSource::Manual,
        _ => json::OwnSource::Gps,
    };
    let status = HubStatus {
        own_source: source,
        manual: hub.manual_pos.get().map(|m| ManualPos { lat: m.lat, lon: m.lon, alt: m.alt }),
        gps: gnss_status(),
        recording: sd::recording(),
        nodes: &nodes,
        adsb: adsbee::status(hub, now),
    };
    match json::state(&[], &[], &[], ctx, &status, buf) {
        // `...,"open_links":[],"tracks":[],"aircraft":[]}\n` -> `...,"open_links":[`
        Ok(n) if n >= EMPTY_TAIL => n - EMPTY_TAIL,
        // Cannot happen with MAX_LINE (head + 3 nodes is < 1 KB); still valid JSON.
        _ => {
            let fallback = br#"{"t":"state","v":1,"err":"envelope","open_links":["#;
            buf[..fallback.len()].copy_from_slice(fallback);
            fallback.len()
        }
    }
}

fn gnss_status() -> GpsStatus {
    gnss::status()
}

/// Builds the state body piece by piece. Every piece fits in [`MAX_LINE`].
pub struct StateWriter {
    ctx: Context,
    links: heapless::Vec<LinkTrackId, MAX_LINK_TRACKS>,
    ids: heapless::Vec<TrackId, MAX_TRACKS>,
    aircraft: heapless::Vec<AircraftKey, MAX_AIRCRAFT>,
    next: usize,
    stage: Stage,
    /// Whether the current array has an element yet (for the commas).
    wrote_any: bool,
}

#[derive(PartialEq)]
enum Stage {
    Head,
    Links,
    Mid,
    Tracks,
    Mid2,
    Aircraft,
    Tail,
    Done,
}

impl StateWriter {
    /// Snapshot the link and track lists (in table order, as `json::state`
    /// iterates them) and the time. One that disappears before its turn is
    /// skipped; one that appears later waits for the next poll.
    pub fn new(hub: &Hub, now: u64) -> Self {
        let ctx = hub.context(now);
        let tracker = hub.tracker.borrow();
        let links = tracker.open_links().iter().map(|l| l.id).collect();
        let ids = tracker.tracks().iter().map(|t| t.id).collect();
        let aircraft = tracker.aircraft().iter().map(|a| a.key).collect();
        Self { ctx, links, ids, aircraft, next: 0, stage: Stage::Head, wrote_any: false }
    }

    /// Write the next piece into `buf`; `None` when the body is complete.
    pub fn next_piece(&mut self, hub: &Hub, buf: &mut [u8; MAX_LINE]) -> Option<usize> {
        loop {
            match self.stage {
                Stage::Head => {
                    self.stage = Stage::Links;
                    return Some(envelope(hub, &self.ctx, buf));
                }
                Stage::Links => {
                    let Some(&id) = self.links.get(self.next) else {
                        self.stage = Stage::Mid;
                        continue;
                    };
                    self.next += 1;
                    let tracker = hub.tracker.borrow();
                    let Some(link) = tracker.get_open_link(id) else { continue };
                    let start = usize::from(self.wrote_any);
                    let Ok(n) = json::link_line(link, &self.ctx, &mut buf[start..]) else { continue };
                    return Some(self.element(buf, start, n));
                }
                Stage::Mid => {
                    (self.stage, self.next, self.wrote_any) = (Stage::Tracks, 0, false);
                    return Some(put(buf, json::STATE_LINKS_TO_TRACKS));
                }
                Stage::Tracks => {
                    let Some(&id) = self.ids.get(self.next) else {
                        self.stage = Stage::Mid2;
                        continue;
                    };
                    self.next += 1;
                    let tracker = hub.tracker.borrow();
                    let Some(track) = tracker.get(id) else { continue };
                    let start = usize::from(self.wrote_any);
                    let Ok(n) = json::track_line(track, &self.ctx, &mut buf[start..]) else { continue };
                    return Some(self.element(buf, start, n));
                }
                Stage::Mid2 => {
                    (self.stage, self.next, self.wrote_any) = (Stage::Aircraft, 0, false);
                    return Some(put(buf, json::STATE_TRACKS_TO_AIRCRAFT));
                }
                Stage::Aircraft => {
                    let Some(&key) = self.aircraft.get(self.next) else {
                        self.stage = Stage::Tail;
                        continue;
                    };
                    self.next += 1;
                    let tracker = hub.tracker.borrow();
                    let Some(a) = tracker.get_aircraft(key) else { continue };
                    let start = usize::from(self.wrote_any);
                    let Ok(n) = json::aircraft_line(a, tracker.tracks(), &self.ctx, &mut buf[start..]) else {
                        continue;
                    };
                    return Some(self.element(buf, start, n));
                }
                Stage::Tail => {
                    self.stage = Stage::Done;
                    return Some(put(buf, json::STATE_END));
                }
                Stage::Done => return None,
            }
        }
    }
}

impl StateWriter {
    /// An array element formatted at `buf[start..]` (`n` bytes, ending in
    /// '\n'): put the comma in front if it is not the first, drop the '\n'.
    fn element(&mut self, buf: &mut [u8; MAX_LINE], start: usize, n: usize) -> usize {
        if self.wrote_any {
            buf[0] = b',';
        }
        self.wrote_any = true;
        start + n - 1
    }
}

/// Copy `s` to the front of `buf`; returns its length.
fn put(buf: &mut [u8; MAX_LINE], s: &[u8]) -> usize {
    buf[..s.len()].copy_from_slice(s);
    s.len()
}

/// Build the body both ways (streamed pieces, and `json::state` into one heap
/// buffer) and compare. Only for small track sets: the reference needs
/// `state_max_len` bytes of heap. Returns `(matches, bytes)`, or `None` if
/// there were too many tracks to check.
pub fn check_against_reference(hub: &Hub, now: u64) -> Option<(bool, usize)> {
    const MAX_CHECKED: usize = 4;
    let tracker = hub.tracker.borrow();
    let links = tracker.open_links();
    let aircraft = tracker.aircraft();
    if tracker.len() > MAX_CHECKED || links.len() > MAX_CHECKED || aircraft.len() > MAX_CHECKED {
        return None;
    }
    let ctx = hub.context(now);
    let nodes = node_statuses(hub, now);
    let mut reference =
        alloc::vec![0u8; json::state_max_len(tracker.len(), links.len(), aircraft.len(), nodes.len())];
    let mut env = [0u8; MAX_LINE];
    let env_len = envelope(hub, &ctx, &mut env);
    // Rebuild the reference with the same inputs the envelope used.
    let status = HubStatus {
        own_source: match hub.own_pos(now).map(|p| p.source) {
            Some(OwnSource::Manual) => json::OwnSource::Manual,
            _ => json::OwnSource::Gps,
        },
        manual: hub.manual_pos.get().map(|m| ManualPos { lat: m.lat, lon: m.lon, alt: m.alt }),
        gps: gnss_status(),
        recording: sd::recording(),
        nodes: &nodes,
        adsb: adsbee::status(hub, now),
    };
    let ref_len = json::state(tracker.tracks(), links, aircraft, &ctx, &status, &mut reference).ok()?;

    let mut streamed: alloc::vec::Vec<u8> = alloc::vec::Vec::with_capacity(ref_len);
    streamed.extend_from_slice(&env[..env_len]);
    let mut line = [0u8; MAX_LINE];
    for (i, l) in links.iter().enumerate() {
        if i > 0 {
            streamed.push(b',');
        }
        let n = json::link_line(l, &ctx, &mut line).ok()?;
        streamed.extend_from_slice(&line[..n - 1]);
    }
    streamed.extend_from_slice(json::STATE_LINKS_TO_TRACKS);
    for (i, t) in tracker.tracks().iter().enumerate() {
        if i > 0 {
            streamed.push(b',');
        }
        let n = json::track_line(t, &ctx, &mut line).ok()?;
        streamed.extend_from_slice(&line[..n - 1]);
    }
    streamed.extend_from_slice(json::STATE_TRACKS_TO_AIRCRAFT);
    for (i, a) in aircraft.iter().enumerate() {
        if i > 0 {
            streamed.push(b',');
        }
        let n = json::aircraft_line(a, tracker.tracks(), &ctx, &mut line).ok()?;
        streamed.extend_from_slice(&line[..n - 1]);
    }
    streamed.extend_from_slice(json::STATE_END);
    Some((streamed[..] == reference[..ref_len], ref_len))
}

/// Apply a `POST /api/own-pos` body. `Err` holds the message for the 400.
pub fn own_pos_request(hub: &Hub, body: &[u8], now: u64) -> Result<(), &'static str> {
    match json::parse_own_pos(body).map_err(|e| e.message())? {
        OwnPosRequest::Clear => hub.clear_manual(now),
        OwnPosRequest::Set(p) => hub.set_manual(p.lat, p.lon, p.alt, now),
    }
    Ok(())
}

/// `own` for the USB hub line.
#[derive(serde::Serialize)]
pub struct OwnJson {
    lat: f64,
    lon: f64,
    alt: Option<f64>,
    source: OwnSource,
    age_ms: u64,
}

impl OwnJson {
    pub fn of(p: &crate::hub::OwnPos, now: u64) -> Self {
        Self { lat: p.lat, lon: p.lon, alt: p.alt, source: p.source, age_ms: now.saturating_sub(p.at_ms) }
    }
}
