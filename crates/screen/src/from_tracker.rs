//! Fill the drone part of a [`UiState`] from a [`tracker::Tracker`]
//! (feature `tracker`). The hub still sets the GPS, node, power and SD
//! fields itself, because those do not live in the tracker.

use core::fmt::Write;

use heapless::{String, Vec};
use rid::Transport;
use rid::message::{FixedStr, HeightType, IdType, UaType};
use tracker::{Band, Context, Track, TrackState, Tracker, geo};

use crate::model::*;

/// Replace `state.drones` with the tracker's tracks, choosing the
/// [`MAX_DRONES`] most relevant when there are more (active before stale,
/// then nearest, then newest). Also sets `now_ms`, `utc_ms` and
/// `total_drones`, and downgrades a 2D/3D/manual `gps.fix` to `None` when
/// the tracker considers the own position too old, so the layout and the
/// numbers agree. (`Fix::Absent` is left alone: it says more.)
pub fn fill_from_tracker<const N: usize>(tracker: &Tracker<N>, ctx: &Context, state: &mut UiState) {
    state.now_ms = ctx.now_ms;
    state.utc_ms = ctx.utc_ms;
    if ctx.own.is_none() && state.gps.has_position() {
        state.gps.fix = Fix::None;
    }
    let tracks = tracker.tracks();
    state.total_drones = tracks.len().min(usize::from(u16::MAX)) as u16;

    // Sort keys computed once per track: the distance is a Vincenty solve in
    // f64, which is soft-float on the hub's Xtensa core. A sort that called
    // it from its comparator would do ~n log n of them per frame.
    let mut keyed: Vec<((bool, u64, u64), usize), N> = tracks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let dist = ctx
                .geometry(t)
                .drone
                .map_or(u64::MAX, |g| g.distance_m.max(0.0) as u64);
            let key = (
                ctx.state(t) == TrackState::Stale,
                dist,
                u64::MAX - t.last_seen_ms,
            );
            (key, i)
        })
        .collect();
    keyed.sort_unstable_by_key(|(k, _)| *k);

    state.drones.clear();
    for &(_, i) in keyed.iter().take(MAX_DRONES) {
        let _ = state.drones.push(view(&tracks[i], ctx));
    }
}

/// One track as the screen sees it.
pub fn view(t: &Track, ctx: &Context) -> DroneView {
    let g = ctx.geometry(t);
    let polar = |rb: tracker::RangeBearing| Polar {
        distance_m: rb.distance_m as f32,
        bearing_deg: rb.bearing_deg as f32,
    };
    let loc = t.location.as_ref().map(|l| &l.value);
    let op_pos = t
        .system
        .as_ref()
        .and_then(|s| s.value.operator_position.as_ref());

    let mut v = DroneView {
        key: t.id.0,
        stale: ctx.state(t) == TrackState::Stale,
        first_seen_ms: t.first_seen_ms,
        last_seen_ms: t.last_seen_ms,
        drone: g.drone.map(polar),
        operator: g.operator.map(polar),
        drone_to_operator_m: loc
            .and_then(|l| l.position.as_ref())
            .zip(op_pos)
            .map(|(d, o)| geo::range_bearing(d.lat, d.lon, o.lat, o.lon).distance_m as f32),
        height_m: loc.and_then(|l| l.height),
        height_ref: match loc.map(|l| l.height_type) {
            Some(HeightType::AboveGround) => HeightRef::Ground,
            _ => HeightRef::Takeoff,
        },
        alt_m: loc.and_then(|l| l.altitude_geodetic),
        above_you_m: g.height_above_m.map(|h| h as f32),
        speed_mps: loc.and_then(|l| l.speed_horizontal),
        vspeed_mps: loc.and_then(|l| l.speed_vertical),
        track_deg: loc.and_then(|l| l.direction),
        ua: match t.ua_type {
            Some(UaType::HelicopterOrMultirotor | UaType::Gyroplane) => UaKind::Multirotor,
            Some(UaType::Aeroplane | UaType::HybridLift | UaType::Glider | UaType::Ornithopter) => {
                UaKind::FixedWing
            }
            Some(UaType::FreeBalloon | UaType::CaptiveBalloon | UaType::Airship) => UaKind::Balloon,
            Some(UaType::None) | None => UaKind::Unknown,
            Some(_) => UaKind::Other,
        },
        ..DroneView::default()
    };

    match t.primary_id() {
        Some(u) => {
            v.id_kind = match u.id_type {
                IdType::SerialNumber => IdKind::Serial,
                IdType::CaaRegistration => IdKind::Registration,
                IdType::UtmAssigned => IdKind::Utm,
                IdType::SpecificSession => IdKind::Session,
                _ => IdKind::Unknown,
            };
            put_text_or_hex(&mut v.id, &u.id);
        }
        None => {
            v.id_kind = IdKind::Mac;
            if let Some((_, l)) = t.latest_link() {
                for (n, b) in l.mac.iter().enumerate() {
                    let sep = if n == 0 { "" } else { ":" };
                    let _ = write!(v.id, "{sep}{b:02x}");
                }
            }
        }
    }
    if let Some(s) = &t.self_id {
        put_text(&mut v.self_id, &s.value.description);
    }
    if let Some(o) = &t.operator_id {
        put_text(&mut v.operator_id, &o.value.operator_id);
    }

    // Radios: one letter per tracker slot. The tracker keeps 5 GHz beacon
    // and NAN in their own slots, so a drone heard on both bands shows both
    // "W" and "5", each with its own signal.
    let mut latest = [0u64; 5];
    for (slot, l) in t.slots() {
        let r = match (slot.transport, slot.band) {
            (Transport::WifiBeacon | Transport::WifiNan, Band::Ghz5) => Radio::Wifi5,
            (Transport::WifiBeacon, _) => Radio::WifiBeacon,
            (Transport::WifiNan, _) => Radio::WifiNan,
            (Transport::BleLegacy | Transport::BleExtended, _) => Radio::Ble,
            (Transport::BleLongRange, _) => Radio::BleLongRange,
        };
        v.radios.insert(r);
        let i = r.index();
        if v.rssi[i].is_none() || l.last_seen_ms >= latest[i] {
            v.rssi[i] = Some(l.rssi);
            latest[i] = l.last_seen_ms;
        }
    }
    v
}

/// Printable ASCII as-is, anything else as lowercase hex (as the JSON does).
fn put_text_or_hex<const N: usize, const M: usize>(out: &mut String<M>, f: &FixedStr<N>) {
    if f.is_clean_text() {
        let _ = out.push_str(f.as_str().unwrap_or(""));
    } else {
        for b in f.as_bytes() {
            if write!(out, "{b:02x}").is_err() {
                break;
            }
        }
    }
}

/// Printable text only; control bytes and non-ASCII become '?'.
fn put_text<const N: usize, const M: usize>(out: &mut String<M>, f: &FixedStr<N>) {
    for &b in f.as_bytes() {
        let c = if (0x20..0x7F).contains(&b) {
            b as char
        } else {
            '?'
        };
        if out.push(c).is_err() {
            break;
        }
    }
}
