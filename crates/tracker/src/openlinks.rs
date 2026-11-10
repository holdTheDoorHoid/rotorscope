//! Open FPV / telemetry links (wfb-ng, OpenHD, RubyFPV, DroneBridge), kept
//! apart from Remote ID drones.
//!
//! A Wi-Fi node classifies the frames with the `openlink` crate and sends a
//! [`link::OpenLinkSighting`] at most about once a second per stream. Here
//! sightings become one [`LinkTrack`] per link: keyed by system and the
//! cleartext identifier (wfb-ng link ID, RubyFPV vehicle ID, DroneBridge comm
//! ID), so a link's video downlink, MAVLink up and down and tunnel streams
//! are one entry. OpenHD sends no identifier, so all OpenHD heard at once is
//! one entry; so is any system's traffic without an ID (e.g. an encrypted
//! RubyFPV header).
//!
//! These tracks have no position and no alert: a sighting says "this kind of
//! link is on the air nearby", nothing more.
//!
//! **Privacy.** The identifiers are kept here because they are the key, but
//! every formatter hides them (and the transmitter address, which for wfb-ng
//! embeds the link ID) unless [`crate::Config::show_link_ids`] is set.

use heapless::Vec;
use openlink::{Direction, System};

use crate::{Band, Config, TrackState};

/// Link tracks the table holds. Each costs about 190 bytes of the hub's
/// static RAM, which has little to spare without PSRAM, and a sensor rarely
/// hears more than one or two such links at once (OpenHD, and anything
/// without a cleartext ID, shares one entry per system). When it is full the
/// link heard least recently makes room.
pub const MAX_LINK_TRACKS: usize = 8;
/// Streams kept per link track. wfb-ng's usual set is five (video down,
/// MAVLink down and up, tunnel down and up); when a sixth new one arrives the
/// stream heard least recently is replaced.
pub const MAX_STREAMS: usize = 6;

/// Stable handle for a link track, separate from drone [`crate::TrackId`]s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(transparent)]
pub struct LinkTrackId(pub u32);

/// One stream of a link as one node hears it: a port in one direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stream {
    pub port: u8,
    pub direction: Direction,
    /// Node that reports it. Two nodes on different bands are two streams.
    pub node: u8,
    pub channel: u8,
    pub rssi: i8,
    /// The node's running frame count for this stream (latest report).
    pub packets: u32,
    /// Latest transmitter address. An identifier for wfb-ng: shown only
    /// with `show_link_ids`.
    pub mac: [u8; 6],
    pub last_seen_ms: u64,
}

impl Stream {
    pub fn band(&self) -> Band {
        Band::of_wifi_channel(self.channel)
    }
}

/// Everything known about one open link.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkTrack {
    pub id: LinkTrackId,
    pub system: System,
    /// The cleartext identifier, if the system sends one. Formatters show it
    /// only with `show_link_ids`.
    pub link_id: Option<u32>,
    pub streams: Vec<Stream, MAX_STREAMS>,
    pub first_seen_ms: u64,
    pub last_seen_ms: u64,
    /// Sightings received (reports, not packets).
    pub sightings: u32,
}

impl LinkTrack {
    /// Frames counted over all streams (each stream's latest running count).
    pub fn packets(&self) -> u32 {
        self.streams
            .iter()
            .fold(0u32, |a, s| a.saturating_add(s.packets))
    }

    /// The stream heard most recently.
    pub fn latest(&self) -> Option<&Stream> {
        self.streams.iter().max_by_key(|s| s.last_seen_ms)
    }

    /// The link has been heard in this direction.
    pub fn heard(&self, d: Direction) -> bool {
        self.streams.iter().any(|s| s.direction == d)
    }

    fn observe(&mut self, node: u8, s: &link::OpenLinkSighting, now_ms: u64) {
        let l = &s.link;
        let stream = Stream {
            port: l.port,
            direction: l.direction,
            node,
            channel: s.channel,
            rssi: s.rssi,
            packets: s.packets,
            mac: l.tx,
            last_seen_ms: now_ms,
        };
        let same = |x: &Stream| x.port == l.port && x.direction == l.direction && x.node == node;
        if let Some(x) = self.streams.iter_mut().find(|x| same(x)) {
            *x = stream;
        } else if let Err(stream) = self.streams.push(stream)
            && let Some(old) = self.streams.iter_mut().min_by_key(|x| x.last_seen_ms)
        {
            *old = stream;
        }
        self.sightings = self.sightings.saturating_add(1);
        self.last_seen_ms = self.last_seen_ms.max(now_ms);
    }
}

/// The link table: at most [`MAX_LINK_TRACKS`] links, expired on the same
/// clock as drones ([`Config::stale_after_ms`], [`Config::remove_after_ms`]).
#[derive(Debug, Clone)]
pub struct LinkTable {
    tracks: Vec<LinkTrack, MAX_LINK_TRACKS>,
    next_id: u32,
}

impl Default for LinkTable {
    fn default() -> Self {
        Self::new()
    }
}

impl LinkTable {
    pub const fn new() -> Self {
        Self {
            tracks: Vec::new(),
            next_id: 1,
        }
    }

    pub fn tracks(&self) -> &[LinkTrack] {
        &self.tracks
    }

    pub fn get(&self, id: LinkTrackId) -> Option<&LinkTrack> {
        self.tracks.iter().find(|t| t.id == id)
    }

    /// Merge one sighting from `node`, received at hub time `now_ms`.
    pub fn ingest(
        &mut self,
        node: u8,
        s: &link::OpenLinkSighting,
        now_ms: u64,
        config: &Config,
    ) -> LinkTrackId {
        self.expire(now_ms, config);
        let key = (s.link.system, s.link.id);
        let i = match self
            .tracks
            .iter()
            .position(|t| (t.system, t.link_id) == key)
        {
            Some(i) => i,
            None => {
                if self.tracks.is_full() {
                    // The link heard least recently makes room.
                    if let Some(i) =
                        (0..self.tracks.len()).min_by_key(|&i| self.tracks[i].last_seen_ms)
                    {
                        self.tracks.remove(i);
                    }
                }
                let id = LinkTrackId(self.next_id);
                self.next_id = self.next_id.wrapping_add(1).max(1);
                // Cannot fail: room was just made.
                let _ = self.tracks.push(LinkTrack {
                    id,
                    system: s.link.system,
                    link_id: s.link.id,
                    streams: Vec::new(),
                    first_seen_ms: now_ms,
                    last_seen_ms: now_ms,
                    sightings: 0,
                });
                self.tracks.len() - 1
            }
        };
        let t = &mut self.tracks[i];
        t.observe(node, s, now_ms);
        t.id
    }

    /// Drop links not heard for `remove_after_ms`. Returns how many went.
    pub fn expire(&mut self, now_ms: u64, config: &Config) -> usize {
        let before = self.tracks.len();
        self.tracks
            .retain(|t| now_ms.saturating_sub(t.last_seen_ms) < config.remove_after_ms);
        before - self.tracks.len()
    }

    /// Links heard within `stale_after_ms`.
    pub fn active_count(&self, now_ms: u64, config: &Config) -> usize {
        self.tracks
            .iter()
            .filter(|t| link_state(t, now_ms, config) == TrackState::Active)
            .count()
    }
}

/// Active or stale, by the same rule as drone tracks.
pub fn link_state(t: &LinkTrack, now_ms: u64, config: &Config) -> TrackState {
    if now_ms.saturating_sub(t.last_seen_ms) >= config.stale_after_ms {
        TrackState::Stale
    } else {
        TrackState::Active
    }
}

/// The link's identifier as display text, **only** when the operator opted
/// in (`Config::show_link_ids`); `None` when hidden or when the system sends
/// none. Every formatter, and the laptop viewer, goes through this, so the
/// policy lives in one place. wfb-ng's link ID is six hex digits (as wfb-ng
/// logs it, `link_id=0x7505d6`); the others are decimal.
pub fn shown_id(t: &LinkTrack, ctx: &crate::Context) -> Option<heapless::String<10>> {
    if !ctx.config.show_link_ids {
        return None;
    }
    t.link_id
        .map(|id| crate::text::render(IdText(t.system, id)))
}

/// See [`shown_id`].
struct IdText(System, u32);

impl core::fmt::Display for IdText {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            System::WfbNg => write!(f, "{:06x}", self.1),
            _ => write!(f, "{}", self.1),
        }
    }
}
