//! One JSON object per track, for the phone page and the USB stream (O1, O4).
//! The schema is documented in `crates/tracker/README.md`; change both together.
//!
//! Enum values use the `rid` crate's names (`"WifiBeacon"`,
//! `"SerialNumber"`, ...) so the node stream (`{"t":"rid",...}`) and the
//! track stream (`{"t":"track",...}`) speak the same vocabulary.

use serde::Serialize;

use crate::text::{FormatError, Mac};
use crate::{Context, TRANSPORTS, Track, TrackId, TrackState};
use rid::Transport;
use rid::message::{FixedStr, HeightType, IdType, OperatorLocationType, Status, UaType};

/// Round to 0.1: honest about the data and keeps lines short. Dividing the
/// integer by 10 (rather than multiplying by 0.1) gives the double nearest
/// the decimal, so it prints as `1363.9`, not `1363.9000000000001`.
fn round1(v: f64) -> f64 {
    libm::round(v * 10.0) / 10.0
}

/// As [`round1`], but 359.96 deg becomes 0.0, not 360.0.
pub(crate) fn round_bearing(v: f64) -> f64 {
    let r = round1(v);
    if r >= 360.0 { 0.0 } else { r }
}

#[derive(Serialize)]
struct Line<'a> {
    t: &'static str,
    id: TrackId,
    state: TrackState,
    ids: Ids<'a>,
    ua: Option<UaType>,
    first_ms: u64,
    last_ms: u64,
    age_ms: u64,
    rssi: Option<i8>,
    links: Links<'a>,
    loc: Option<Loc>,
    op: Option<Op>,
    self_id: Option<&'a FixedStr<23>>,
    op_id: Option<&'a FixedStr<20>>,
    own: Option<Own>,
}

#[derive(Serialize)]
struct IdEntry<'a> {
    #[serde(rename = "type")]
    id_type: IdType,
    id: &'a FixedStr<20>,
}

struct Ids<'a>(&'a Track);

impl Serialize for Ids<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(self.0.uas_ids.iter().map(|u| IdEntry {
            id_type: u.id_type,
            id: &u.id,
        }))
    }
}

#[derive(Serialize)]
struct LinkEntry<'a> {
    tr: Transport,
    n: u32,
    rssi: i8,
    ch: u8,
    mac: Mac<'a>,
    ctr: u8,
    age_ms: u64,
    node: u8,
}

struct Links<'a>(&'a Track, u64);

impl Serialize for Links<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let now = self.1;
        s.collect_seq(TRANSPORTS.iter().filter_map(|t| {
            let l = self.0.link(*t);
            l.heard().then(|| LinkEntry {
                tr: *t,
                n: l.count,
                rssi: l.rssi,
                ch: l.channel,
                mac: Mac(&l.mac),
                ctr: l.counter,
                age_ms: now.saturating_sub(l.last_seen_ms),
                node: l.node,
            })
        }))
    }
}

#[derive(Serialize)]
struct Loc {
    lat: Option<f64>,
    lon: Option<f64>,
    alt_geo: Option<f32>,
    alt_baro: Option<f32>,
    height: Option<f32>,
    height_ref: HeightType,
    speed: Option<f32>,
    vspeed: Option<f32>,
    dir: Option<f32>,
    status: Status,
    hacc_m: Option<f32>,
    vacc_m: Option<f32>,
    age_ms: u64,
}

#[derive(Serialize)]
struct Op {
    lat: Option<f64>,
    lon: Option<f64>,
    alt: Option<f32>,
    src: OperatorLocationType,
    age_ms: u64,
}

#[derive(Serialize)]
struct Own {
    dist_m: Option<f64>,
    brg: Option<f64>,
    op_dist_m: Option<f64>,
    op_brg: Option<f64>,
    above_m: Option<f64>,
}

/// A buffer this size always holds one line (worst case: all five
/// transports, two IDs, every field at its longest; see the test).
pub const MAX_LINE: usize = 1536;

/// Write `track` as one JSON object followed by `\n`. Returns bytes written.
pub fn track_line(track: &Track, ctx: &Context, buf: &mut [u8]) -> Result<usize, FormatError> {
    let now = ctx.now_ms;
    let loc = track.location.as_ref().map(|s| {
        let l = &s.value;
        Loc {
            lat: l.position.map(|p| p.lat),
            lon: l.position.map(|p| p.lon),
            alt_geo: l.altitude_geodetic,
            alt_baro: l.altitude_pressure,
            height: l.height,
            height_ref: l.height_type,
            speed: l.speed_horizontal,
            vspeed: l.speed_vertical,
            dir: l.direction,
            status: l.status,
            hacc_m: l.horizontal_accuracy.meters(),
            vacc_m: l.vertical_accuracy.meters(),
            age_ms: now.saturating_sub(s.rx_ms),
        }
    });
    let op = track.system.as_ref().map(|s| {
        let v = &s.value;
        Op {
            lat: v.operator_position.map(|p| p.lat),
            lon: v.operator_position.map(|p| p.lon),
            alt: v.operator_altitude,
            src: v.operator_location_type,
            age_ms: now.saturating_sub(s.rx_ms),
        }
    });
    // `own` is null without a fix, which is how the page knows to show
    // "own position unknown" (P4) rather than a bogus range.
    let own = ctx.own.map(|_| {
        let g = ctx.geometry(track);
        Own {
            dist_m: g.drone.map(|d| round1(d.distance_m)),
            brg: g.drone.map(|d| round_bearing(d.bearing_deg)),
            op_dist_m: g.operator.map(|d| round1(d.distance_m)),
            op_brg: g.operator.map(|d| round_bearing(d.bearing_deg)),
            above_m: g.height_above_m.map(round1),
        }
    });
    let line = Line {
        t: "track",
        id: track.id,
        state: ctx.state(track),
        ids: Ids(track),
        ua: track.ua_type,
        first_ms: track.first_seen_ms,
        last_ms: track.last_seen_ms,
        age_ms: now.saturating_sub(track.last_seen_ms),
        rssi: track.latest_link().map(|(_, l)| l.rssi),
        links: Links(track, now),
        loc,
        op,
        self_id: track.self_id.as_ref().map(|s| &s.value.description),
        op_id: track.operator_id.as_ref().map(|s| &s.value.operator_id),
        own,
    };
    let n = serde_json_core::to_slice(&line, buf).map_err(|_| FormatError::BufferTooSmall)?;
    let nl = buf.get_mut(n).ok_or(FormatError::BufferTooSmall)?;
    *nl = b'\n';
    Ok(n + 1)
}

// ------------------------------------------------------------------ state --
//
// The phone page polls `GET /api/state` about once a second and gets one
// object holding everything it shows: own position, hub GPS and node health,
// recording, and every track (each exactly as `track_line` writes it). The
// schema is in `crates/tracker/README.md` ("Phone page API").

/// Where the unit's own position (the `own` object) came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OwnSource {
    /// The hub's GPS receiver.
    Gps,
    /// Typed in on the phone page (`POST /api/own-pos`).
    Manual,
}

/// What the hub's GPS receiver is doing, independent of which position is in
/// use (a manual position may be set while the GPS is still searching).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum GpsState {
    /// No GPS receiver answering.
    #[serde(rename = "absent")]
    Absent,
    /// Receiver present, no fix yet.
    #[serde(rename = "searching")]
    Searching,
    #[serde(rename = "2d")]
    Fix2d,
    #[serde(rename = "3d")]
    Fix3d,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct GpsStatus {
    pub state: GpsState,
    /// Satellites used in the fix (or tracked while searching), if known.
    pub sats: Option<u8>,
}

/// A position typed in on the phone page.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ManualPos {
    pub lat: f64,
    pub lon: f64,
    /// Metres above the WGS-84 ellipsoid, if given.
    pub alt: Option<f64>,
}

/// Health of one radio node, for the page's status strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct NodeStatus {
    /// Serialized as `"Wifi24"`, `"Wifi5"`, `"Ble"` or `"Fpv"`.
    pub role: link::NodeRole,
    /// The hub has heard from the node recently (any frame, e.g. within 5 s).
    pub alive: bool,
    /// Time since the last frame from this node; `None` if never heard.
    pub age_ms: Option<u64>,
    /// Frames the node saw per second (all frames, not only Remote ID), from
    /// its Stats frames. `None` until the first Stats frame.
    pub seen_per_s: Option<u32>,
    /// Remote ID detections the hub received from it per second.
    pub rid_per_s: u32,
    /// Frames the node dropped (queue full), total since it booted.
    pub drops: Option<u32>,
}

/// Everything `/api/state` reports besides the tracks and the `Context`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HubStatus<'a> {
    /// Where `ctx.own` came from. Ignored when `ctx.own` is `None`.
    pub own_source: OwnSource,
    /// The manual position on file, if any, whether or not it is in use.
    pub manual: Option<ManualPos>,
    pub gps: GpsStatus,
    /// The SD-card log is being written.
    pub recording: bool,
    /// One entry per attached node, in a stable order.
    pub nodes: &'a [NodeStatus],
}

#[derive(Serialize)]
struct StateOwn {
    lat: f64,
    lon: f64,
    alt: Option<f64>,
    source: OwnSource,
    age_ms: u64,
}

/// The part of the state object before the tracks. Serialized, then its
/// closing `}` is replaced so the tracks array can be streamed after it.
#[derive(Serialize)]
struct StateHead<'a> {
    t: &'static str,
    v: u8,
    now_ms: u64,
    utc_ms: Option<u64>,
    own: Option<StateOwn>,
    manual: Option<ManualPos>,
    gps: GpsStatus,
    rec: bool,
    nodes: &'a [NodeStatus],
}

/// Schema version of the state object (`"v"`). Bump on incompatible change.
pub const STATE_VERSION: u8 = 1;

/// Bytes the state object needs before the tracks, with no nodes (worst case).
pub const STATE_HEAD_MAX: usize = 448;
/// Bytes one node entry needs (worst case, with its comma).
pub const STATE_NODE_MAX: usize = 160;

/// A buffer this size always holds the state object for `tracks` tracks and
/// `nodes` nodes. E.g. 64 tracks and 4 nodes: about 99 KB (put it in PSRAM).
/// A typical object is far smaller (~1 KB per track); see `state` for what
/// happens when it does not fit.
pub const fn state_max_len(tracks: usize, nodes: usize) -> usize {
    STATE_HEAD_MAX + nodes * STATE_NODE_MAX + tracks * MAX_LINE
}

/// Write the `/api/state` body: one JSON object followed by `\n`. Returns
/// bytes written.
///
/// `tracks` is normally `tracker.tracks()` and `ctx` is
/// `tracker.context(now_ms, utc_ms)`, so the tracks agree with `own`.
/// Every track is written exactly as [`track_line`] writes it.
///
/// Returns [`FormatError::BufferTooSmall`] (and nothing usable) if `buf` is
/// too small; [`state_max_len`] gives a size that always fits.
pub fn state(
    tracks: &[Track],
    ctx: &Context,
    hub: &HubStatus,
    buf: &mut [u8],
) -> Result<usize, FormatError> {
    let head = StateHead {
        t: "state",
        v: STATE_VERSION,
        now_ms: ctx.now_ms,
        utc_ms: ctx.utc_ms,
        own: ctx.own.map(|o| StateOwn {
            lat: o.lat,
            lon: o.lon,
            alt: o.alt_m,
            source: hub.own_source,
            age_ms: ctx.now_ms.saturating_sub(o.fix_ms),
        }),
        manual: hub.manual,
        gps: hub.gps,
        rec: hub.recording,
        nodes: hub.nodes,
    };
    // `{...}` -> `{...,"tracks":[` : drop the closing brace and carry on.
    let mut n = serde_json_core::to_slice(&head, buf).map_err(|_| FormatError::BufferTooSmall)? - 1;
    put(buf, &mut n, b",\"tracks\":[")?;
    for (i, t) in tracks.iter().enumerate() {
        if i > 0 {
            put(buf, &mut n, b",")?;
        }
        let rest = buf.get_mut(n..).ok_or(FormatError::BufferTooSmall)?;
        // `track_line` ends with '\n'; drop it inside the array.
        n += track_line(t, ctx, rest)? - 1;
    }
    put(buf, &mut n, b"]}\n")?;
    Ok(n)
}

/// Append `s` at `buf[*n..]`.
fn put(buf: &mut [u8], n: &mut usize, s: &[u8]) -> Result<(), FormatError> {
    let dst = buf
        .get_mut(*n..*n + s.len())
        .ok_or(FormatError::BufferTooSmall)?;
    dst.copy_from_slice(s);
    *n += s.len();
    Ok(())
}

// ---------------------------------------------------------------- own-pos --

/// A parsed `POST /api/own-pos` body.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OwnPosRequest {
    /// `{"lat":..,"lon":..}` or `{"lat":..,"lon":..,"alt":..}` (`alt` may be null).
    Set(ManualPos),
    /// `{"clear":true}`: forget the manual position.
    Clear,
}

/// Why a `POST /api/own-pos` body was refused. The hub answers 400 with
/// [`OwnPosError::message`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnPosError {
    /// Not JSON, or not an object of the expected shape.
    Malformed,
    /// `lat` or `lon` missing.
    Missing,
    /// `lat` outside -90..90, `lon` outside -180..180, or `alt` outside
    /// -1000..20000 m, or a value not finite.
    OutOfRange,
}

impl OwnPosError {
    pub fn message(self) -> &'static str {
        match self {
            OwnPosError::Malformed => {
                "body must be {\"lat\":..,\"lon\":..,\"alt\":..} or {\"clear\":true}"
            }
            OwnPosError::Missing => "lat and lon are both required",
            OwnPosError::OutOfRange => "lat must be -90..90, lon -180..180, alt -1000..20000 m",
        }
    }
}

#[derive(serde::Deserialize)]
struct OwnPosBody {
    #[serde(default)]
    lat: Option<f64>,
    #[serde(default)]
    lon: Option<f64>,
    #[serde(default)]
    alt: Option<f64>,
    #[serde(default)]
    clear: Option<bool>,
}

/// Parse and check a `POST /api/own-pos` body. Unknown fields are ignored.
pub fn parse_own_pos(body: &[u8]) -> Result<OwnPosRequest, OwnPosError> {
    let (b, _): (OwnPosBody, _) =
        serde_json_core::from_slice(body).map_err(|_| OwnPosError::Malformed)?;
    if b.clear == Some(true) {
        return Ok(OwnPosRequest::Clear);
    }
    let (Some(lat), Some(lon)) = (b.lat, b.lon) else {
        return Err(OwnPosError::Missing);
    };
    let ok_lat = lat.is_finite() && (-90.0..=90.0).contains(&lat);
    let ok_lon = lon.is_finite() && (-180.0..=180.0).contains(&lon);
    let ok_alt = b
        .alt
        .is_none_or(|a| a.is_finite() && (-1000.0..=20_000.0).contains(&a));
    if !(ok_lat && ok_lon && ok_alt) {
        return Err(OwnPosError::OutOfRange);
    }
    Ok(OwnPosRequest::Set(ManualPos {
        lat,
        lon,
        alt: b.alt,
    }))
}
