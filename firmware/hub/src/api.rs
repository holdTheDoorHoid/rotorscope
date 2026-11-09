//! The `/api/state` body, streamed, and the `/api/own-pos` request.
//!
//! `tracker::json::state` writes the whole object into one buffer, which for 64
//! tracks is up to ~99 KB: it does not fit next to Wi-Fi on an S3 without
//! PSRAM. So the hub sends the same bytes in pieces:
//!
//! 1. the envelope: `json::state` called with **no tracks**, which yields
//!    `{...,"tracks":[]}\n`; the trailing `]}\n` is cut off;
//! 2. each track as `json::track_line` without its `\n`, comma-separated;
//! 3. `]}\n`.
//!
//! That is byte-for-byte what `json::state` produces for the same tracks and
//! context (it builds the object the same way), and `check_against_reference`
//! verifies it on the device (USB `s`, and once at boot in `demo` builds).
//! Each piece fits in `json::MAX_LINE`, and the tracker is borrowed only while
//! a piece is formatted, never across a socket write.

use tracker::json::{self, GpsStatus, HubStatus, MAX_LINE, ManualPos, NodeStatus, OwnPosRequest};
use tracker::{Context, TrackId};

use crate::hub::{Hub, MAX_TRACKS, OwnSource};
use crate::pins::NODE_LINKS;
use crate::{gnss, sd};

const TAIL: &[u8] = b"]}\n";

/// Node entries for the state object: one per link that has said Hello.
fn node_statuses(hub: &Hub, now: u64) -> heapless::Vec<NodeStatus, NODE_LINKS> {
    let nodes = hub.nodes.borrow();
    nodes
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            let role = s.role?;
            let alive = s.alive(now);
            Some(NodeStatus {
                role,
                alive,
                age_ms: s.last_heard_ms.map(|t| now.saturating_sub(t)),
                // A dead node's last rate would be a lie.
                seen_per_s: s.seen_per_s.map(|r| if alive { r } else { 0 }),
                rid_per_s: if alive { s.rid_per_s_hub } else { 0 },
                drops: Some(hub.link_drops(i)),
            })
        })
        .collect()
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
    };
    match json::state(&[], ctx, &status, buf) {
        // `...,"tracks":[]}\n` -> `...,"tracks":[`
        Ok(n) if n >= TAIL.len() => n - TAIL.len(),
        // Cannot happen with MAX_LINE (head + 3 nodes is < 1 KB); still valid JSON.
        _ => {
            let fallback = br#"{"t":"state","v":1,"err":"envelope","tracks":["#;
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
    ids: heapless::Vec<TrackId, MAX_TRACKS>,
    next: usize,
    stage: Stage,
    wrote_track: bool,
}

#[derive(PartialEq)]
enum Stage {
    Head,
    Tracks,
    Tail,
    Done,
}

impl StateWriter {
    /// Snapshot the track list (in table order, as `json::state` iterates it)
    /// and the time. A track that disappears before its turn is skipped; one
    /// that appears later waits for the next poll.
    pub fn new(hub: &Hub, now: u64) -> Self {
        let ctx = hub.context(now);
        let ids = hub.tracker.borrow().tracks().iter().map(|t| t.id).collect();
        Self { ctx, ids, next: 0, stage: Stage::Head, wrote_track: false }
    }

    /// Write the next piece into `buf`; `None` when the body is complete.
    pub fn next_piece(&mut self, hub: &Hub, buf: &mut [u8; MAX_LINE]) -> Option<usize> {
        loop {
            match self.stage {
                Stage::Head => {
                    self.stage = Stage::Tracks;
                    return Some(envelope(hub, &self.ctx, buf));
                }
                Stage::Tracks => {
                    let Some(&id) = self.ids.get(self.next) else {
                        self.stage = Stage::Tail;
                        continue;
                    };
                    self.next += 1;
                    let tracker = hub.tracker.borrow();
                    let Some(track) = tracker.get(id) else { continue };
                    let start = usize::from(self.wrote_track);
                    let Ok(n) = json::track_line(track, &self.ctx, &mut buf[start..]) else { continue };
                    if self.wrote_track {
                        buf[0] = b',';
                    }
                    self.wrote_track = true;
                    // Drop the line's '\n' inside the array.
                    return Some(start + n - 1);
                }
                Stage::Tail => {
                    self.stage = Stage::Done;
                    buf[..TAIL.len()].copy_from_slice(TAIL);
                    return Some(TAIL.len());
                }
                Stage::Done => return None,
            }
        }
    }
}

/// Build the body both ways (streamed pieces, and `json::state` into one heap
/// buffer) and compare. Only for small track sets: the reference needs
/// `state_max_len` bytes of heap. Returns `(matches, bytes)`, or `None` if
/// there were too many tracks to check.
pub fn check_against_reference(hub: &Hub, now: u64) -> Option<(bool, usize)> {
    const MAX_CHECKED: usize = 4;
    let tracker = hub.tracker.borrow();
    if tracker.len() > MAX_CHECKED {
        return None;
    }
    let ctx = hub.context(now);
    let nodes = node_statuses(hub, now);
    let mut reference = alloc::vec![0u8; json::state_max_len(tracker.len(), nodes.len())];
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
    };
    let ref_len = json::state(tracker.tracks(), &ctx, &status, &mut reference).ok()?;

    let mut streamed: alloc::vec::Vec<u8> = alloc::vec::Vec::with_capacity(ref_len);
    streamed.extend_from_slice(&env[..env_len]);
    let mut line = [0u8; MAX_LINE];
    for (i, t) in tracker.tracks().iter().enumerate() {
        if i > 0 {
            streamed.push(b',');
        }
        let n = json::track_line(t, &ctx, &mut line).ok()?;
        streamed.extend_from_slice(&line[..n - 1]);
    }
    streamed.extend_from_slice(TAIL);
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
