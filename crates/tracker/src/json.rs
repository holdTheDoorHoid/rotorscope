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
