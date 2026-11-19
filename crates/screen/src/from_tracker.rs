//! Fill the drone part of a [`UiState`] from a [`tracker::Tracker`]
//! (feature `tracker`). The hub still sets the GPS, node, power and SD
//! fields itself, because those do not live in the tracker.

use core::fmt::Write;

use heapless::{String, Vec};
use link::RfKind;
use rid::Transport;
use rid::message::{FixedStr, HeightType, IdType, UaType};
use tracker::{Band, Context, Track, TrackState, Tracker, geo};

use crate::model::*;

/// Replace `state.drones` with the tracker's tracks, choosing the
/// [`MAX_DRONES`] most relevant when there are more (active before stale,
/// confirmed before unconfirmed, then nearest, then newest: a forged drone
/// can claim to be at the hub, so standing comes before claimed distance).
/// Also sets `now_ms`, `utc_ms`, `threats.flood_per_s` and
/// `total_drones`, and downgrades a 2D/3D/manual `gps.fix` to `None` when
/// the tracker considers the own position too old, so the layout and the
/// numbers agree. (`Fix::Absent` is left alone: it says more.)
pub fn fill_from_tracker<const N: usize, const A: usize, const P: usize>(
    tracker: &Tracker<N, A, P>,
    ctx: &Context,
    state: &mut UiState,
) {
    state.now_ms = ctx.now_ms;
    state.utc_ms = ctx.utc_ms;
    if ctx.own.is_none() && state.gps.has_position() {
        state.gps.fix = Fix::None;
    }
    let tracks = tracker.tracks();
    state.total_drones = tracks.len().min(usize::from(u16::MAX)) as u16;
    // At least 1 while the alarm is raised: its slow rules (a minute's new
    // IDs, IDs active at once) can hold under one new ID a second.
    state.threats.flood_per_s = ctx.flood.map_or(0, |f| f.new_per_s.clamp(1, u32::from(u16::MAX)) as u16);
    state.total_unconfirmed = tracks.iter().filter(|t| !t.confirmed()).count().min(usize::from(u16::MAX)) as u16;
    // The most serious drone's threat level, for the strip (not snoozed).
    state.threat_strip = tracker.top_threat(ctx.now_ms).and_then(|t| {
        Some(crate::ThreatStrip { level: t.threat.level? as u8, text: tracker::behaviour::strip_text(t)? })
    });

    // Sort keys computed once per track: the distance is a range solve in
    // f64, which is soft-float on the hub's cores. A sort that called it
    // from its comparator would do ~n log n of them per frame. Only the
    // drone's own distance (one bounded solve, `tracker::geo`), not the
    // operator's.
    let mut keyed: Vec<((bool, bool, u64, u64), usize), N> = tracks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let dist = ctx
                .own
                .zip(t.location.as_ref().and_then(|l| l.value.position))
                .map_or(u64::MAX, |(o, p)| geo::range_bearing(o.lat, o.lon, p.lat, p.lon).distance_m.max(0.0) as u64);
            let key = (
                ctx.state(t) == TrackState::Stale,
                !t.confirmed(),
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
    fill_signals(tracker.emitters(), tracker.wifi_drones(), ctx, state);
}

/// Replace `state.control` (and its total) from the control-link table
/// (kept apart from the tracker; call after [`fill_from_tracker`]):
/// control links first (active, then strongest), then bands with a recent
/// hopper. A drone known only from a link's telemetry is named after the
/// link ("ExpressLRS link 915").
#[inline(never)]
pub fn fill_control(table: &tracker::ctrllinks::ControlTable, ctx: &Context, state: &mut UiState) {
    use tracker::ctrllinks::{band_name, hopper_state, short_name};
    let (links, bands) = (table.links(), table.bands());
    for v in state.drones.iter_mut().filter(|v| v.id_kind == IdKind::Presence) {
        if let Some(l) = table.link_of(tracker::TrackId(v.key)) {
            v.id.clear();
            let _ = write!(v.id, "{} link {}", l.system.label(), l.band());
        }
    }
    let stale_after = ctx.config.stale_after_ms;
    let is_stale = |t: u64| ctx.now_ms.saturating_sub(t) >= stale_after;
    let hoppers = bands.iter().filter(|b| hopper_state(b, ctx.now_ms, &ctx.config).is_some());
    state.total_control = (links.len() + hoppers.clone().count()).min(usize::from(u16::MAX)) as u16;
    state.control.clear();
    let mut order: Vec<(bool, i16, usize), { tracker::ctrllinks::MAX_CONTROL_LINKS }> =
        links.iter().enumerate().map(|(i, l)| (is_stale(l.last_seen_ms), -l.rssi, i)).collect();
    order.sort_unstable();
    for &(stale, _, i) in &order {
        let l = &links[i];
        let mut v = ControlView {
            key: l.id.0,
            band: l.band(),
            sure: match l.best {
                ctrlwatch::Confidence::Energy => ControlSure::Energy,
                ctrlwatch::Confidence::Likely => ControlSure::Likely,
                ctrlwatch::Confidence::Confirmed => ControlSure::Confirmed,
            },
            rssi: l.rssi,
            stale,
            last_seen_ms: l.last_seen_ms,
            ..ControlView::default()
        };
        let _ = v.system.push_str(short_name(l.system));
        // Fresh sticks, else telemetry, else the rate.
        let fresh = |t: u64| ctx.now_ms.saturating_sub(t) < 10_000;
        match (l.sticks.filter(|s| fresh(s.rx_ms)), l.telemetry.as_ref().filter(|t| fresh(t.rx_ms))) {
            (Some(st), _) => {
                for (k, us) in st.us.iter().enumerate() {
                    let _ = write!(v.detail, "{}{}", if k == 0 { "" } else { " " }, us);
                }
            }
            (None, Some(t)) => {
                if let Some(mv) = t.battery_mv {
                    let _ = write!(v.detail, "{}.{}V ", mv / 1000, mv / 100 % 10);
                }
                if let Some(lq) = t.lq {
                    let _ = write!(v.detail, "LQ{lq} ");
                }
                let _ = v.detail.push_str(t.mode.get(..t.mode.len().min(8)).unwrap_or(""));
            }
            (None, None) if l.encrypted => {
                let _ = v.detail.push_str("encrypted");
            }
            (None, None) => {
                let _ = write!(v.detail, "{}", tracker::json::ProbeText(l.probe));
            }
        }
        if state.control.push(v).is_err() {
            break;
        }
    }
    for b in hoppers {
        let Some(h) = &b.hopper else { continue };
        let at = &h.at_ms;
        let mut v = ControlView {
            key: 0x8000_0000 | u32::from(b.node) << 8 | b.region as u32,
            band: band_name(b.region),
            sure: ControlSure::Energy,
            rssi: i16::from(h.rssi),
            stale: is_stale(*at),
            last_seen_ms: *at,
            ..ControlView::default()
        };
        match h.candidates().next() {
            Some((sys, _)) => {
                let _ = v.system.push_str(short_name(sys).trim_end_matches('?'));
                let _ = v.system.push('?');
            }
            None => {
                let _ = v.system.push_str("hopper");
            }
        }
        let _ = write!(v.detail, "{}ch", h.channels);
        if let Some(sp) = h.spacing_khz {
            let _ = write!(v.detail, " {sp}k");
        }
        if let Some(iv) = h.interval_us {
            let _ = write!(v.detail, " {}.{}ms", iv / 1000, iv / 100 % 10);
        }
        if state.control.push(v).is_err() {
            break;
        }
    }
}

/// Replace `state.emitters` and `state.wifi_drones` (and their totals):
/// active before stale, then strongest. Called by [`fill_from_tracker`].
pub fn fill_signals(
    emitters: &[tracker::Emitter],
    wifi: &[tracker::WifiDrone],
    ctx: &Context,
    state: &mut UiState,
) {
    let clamp = |n: usize| n.min(usize::from(u16::MAX)) as u16;
    state.total_emitters = clamp(emitters.len());
    state.total_wifi_drones = clamp(wifi.len());

    let mut order: Vec<(bool, i16, usize), { tracker::signals::MAX_EMITTERS }> = emitters
        .iter()
        .enumerate()
        .map(|(i, e)| {
            (
                ctx.emitter_state(e) == TrackState::Stale,
                -i16::from(e.snr_db),
                i,
            )
        })
        .collect();
    order.sort_unstable();
    state.emitters.clear();
    for &(stale, _, i) in order.iter().take(MAX_SIGNALS) {
        let e = &emitters[i];
        let _ = state.emitters.push(EmitterView {
            key: e.id.0,
            kind: match e.kind {
                RfKind::AnalogVideo => EmitterKind::AnalogVideo,
                RfKind::FpvCarrier => EmitterKind::FpvCarrier,
                RfKind::Hdzero => EmitterKind::Hdzero,
                RfKind::LteStyle => EmitterKind::LteStyle,
                RfKind::WifiNarrow => EmitterKind::WifiNarrow,
                RfKind::Energy => EmitterKind::Energy,
                RfKind::DjiVideo => EmitterKind::DjiVideo,
                RfKind::DjiO4Video => EmitterKind::DjiO4Video,
                RfKind::DjiFpvAirUnit => EmitterKind::DjiFpvAirUnit,
                RfKind::DjiController => EmitterKind::DjiController,
            },
            freq_mhz: e.freq_mhz() as f32,
            snr_db: e.snr_db,
            duty_pct: e.duty_pct,
            line_hz: e.line_hz,
            stale,
            last_seen_ms: e.last_seen_ms,
        });
    }

    let mut order: Vec<(bool, i16, usize), { tracker::signals::MAX_WIFI_DRONES }> = wifi
        .iter()
        .enumerate()
        .map(|(i, w)| {
            (
                ctx.wifi_drone_state(w) == TrackState::Stale,
                -i16::from(w.rssi),
                i,
            )
        })
        .collect();
    order.sort_unstable();
    state.wifi_drones.clear();
    for &(stale, _, i) in order.iter().take(MAX_SIGNALS) {
        let w = &wifi[i];
        let mut v = WifiDroneView {
            key: w.id.0,
            channel: w.channel,
            rssi: w.rssi,
            stale,
            last_seen_ms: w.last_seen_ms,
            ..WifiDroneView::default()
        };
        let _ = v.maker.push_str(w.maker_name());
        for c in w.ssid.chars() {
            let c = if (' '..='~').contains(&c) { c } else { '?' };
            if v.ssid.push(c).is_err() {
                break;
            }
        }
        let _ = state.wifi_drones.push(v);
    }
}

/// Summarise the tracker's aircraft for the health page. The hub calls this
/// only when it has an ADS-B receiver built in (otherwise `state.aircraft`
/// stays `None` and no line is drawn).
pub fn aircraft_summary(aircraft: &[tracker::AircraftTrack], ctx: &Context) -> AircraftSummary {
    let mut out = AircraftSummary {
        total: aircraft.len().min(usize::from(u16::MAX)) as u16,
        ..AircraftSummary::default()
    };
    for a in aircraft {
        if ctx.aircraft_state(a) != TrackState::Active {
            continue;
        }
        out.active = out.active.saturating_add(1);
        let g = ctx.aircraft_geometry(a);
        out.low_nearby = out.low_nearby.saturating_add(u16::from(g.low_nearby));
        let Some(r) = g.range else { continue };
        if out
            .nearest
            .as_ref()
            .is_some_and(|n| n.distance_m <= r.distance_m as f32)
        {
            continue;
        }
        let mut name: String<8> = String::new();
        if a.callsign.is_empty() {
            let _ = name.push_str(tracker::aircraft::icao_hex(a).as_str());
        } else {
            let _ = name.push_str(a.callsign.as_str());
        }
        out.nearest = Some(NearestAircraft {
            name,
            distance_m: r.distance_m as f32,
            alt_ft: a.baro_alt_ft,
        });
    }
    out
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
        unconfirmed: !t.confirmed(),
        first_seen_ms: t.first_seen_ms,
        last_seen_ms: t.last_seen_ms,
        drone: g.drone.map(polar),
        operator: g.operator.map(polar),
        // For display only: the sphere (0.5 % off, a fixed cost) rather
        // than Vincenty, since both ends come from the air (WP-09).
        drone_to_operator_m: loc
            .and_then(|l| l.position.as_ref())
            .zip(op_pos)
            .map(|(d, o)| geo::haversine(d.lat, d.lon, o.lat, o.lon).distance_m as f32),
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
        None => match t.latest_link() {
            Some((_, l)) => {
                v.id_kind = IdKind::Mac;
                for (n, b) in l.mac.iter().enumerate() {
                    let sep = if n == 0 { "" } else { ":" };
                    let _ = write!(v.id, "{sep}{b:02x}");
                }
            }
            // DJI DroneID with no readable (or shown) identity.
            None => {
                v.id_kind = IdKind::Presence;
                let what = match t.dji.key_id {
                    Some(k) if !ctx.config.hide_droneid_ids => {
                        let _ = write!(v.id, "DJI O4 {:02x}{:02x}{:02x}{:02x}", k[0], k[1], k[2], k[3]);
                        None
                    }
                    Some(_) => Some("DJI O4"),
                    None if t.dji.undecoded => Some("DroneID"),
                    None => Some("DJI"),
                };
                if let Some(w) = what {
                    let _ = v.id.push_str(w);
                    if let Some(f) = t.dji.freq_mhz() {
                        let _ = write!(v.id, " {f:.1}");
                    }
                }
            }
        },
    }
    // Claims: the dispute, and where the other claim is (one more range
    // solve, for disputed drones only).
    v.disputed = t.claims.disputed(ctx.now_ms);
    if v.disputed && let Some(r) = t.claims.rival(ctx.now_ms) {
        v.other_claim = ctx.own.map(|o| polar(geo::range_bearing(o.lat, o.lon, r.position.lat, r.position.lon)));
        v.claims_apart_m = tracker::claims::apart_m(t, ctx.now_ms).map(|m| m as f32);
    }
    v.auth_seen = t.spoof.auth_pages != 0;
    if let Some(m) = t.dji.product_name() {
        let _ = v.model.push_str(m);
    } else if let Some(st) = t.dji.o4.state() {
        // O4 has no readable model; show what the record types suggest
        // (model-dependent, hence the question mark).
        let _ = v.model.push_str(st.short());
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
    let mut latest = [0u64; 6];
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
    // DJI: the OcuSync burst is its own letter; the legacy Wi-Fi element
    // is a 2.4 GHz beacon, so it counts as W.
    for (r, l) in [
        (Radio::DroneId, &t.dji.ocusync),
        (Radio::WifiBeacon, &t.dji.wifi),
    ] {
        let i = r.index();
        if !l.heard() {
            continue;
        }
        v.radios.insert(r);
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

/// The most serious drone / crewed-aircraft pair, for the status bar and
/// health page (`conflicts` from `Tracker::conflicts`, most serious first).
pub fn conflict_view(conflicts: &[tracker::conflict::Conflict], aircraft: &[tracker::AircraftTrack]) -> Option<ConflictView> {
    let c = conflicts.first()?;
    let mut name: String<8> = String::new();
    match aircraft.iter().find(|a| a.key == c.aircraft) {
        Some(a) if !a.callsign.is_empty() => {
            let _ = name.push_str(a.callsign.as_str());
        }
        Some(a) => {
            let _ = name.push_str(tracker::aircraft::icao_hex(a).as_str());
        }
        None => {
            let _ = core::fmt::Write::write_fmt(&mut name, format_args!("{:06X}", c.aircraft.icao));
        }
    }
    Some(ConflictView {
        level: c.level as u8,
        drone: c.drone.0,
        aircraft: name,
        range_m: c.range_m,
        dz_m: c.dz_m,
        count: conflicts.iter().filter(|c| c.level >= tracker::conflict::Level::Caution).count().min(255) as u8,
    })
}

/// GPS interference in the area, for the status bar and health page
/// (`status` from `tracker::area_gnss::AreaWatch::status`); `None` when
/// neither jamming nor spoofing is raised.
pub fn area_gnss_view(status: &tracker::area_gnss::AreaStatus) -> Option<AreaGnssView> {
    use tracker::area_gnss::detail;
    if !status.jamming && !status.spoofing {
        return None;
    }
    let own = |d: u32| d & detail::OWN_AGREES != 0;
    let sd = status.spoof_detail;
    Some(AreaGnssView {
        jamming: status.jamming,
        spoofing: status.spoofing,
        degraded: detail::byte(status.jam_detail, 1),
        counted: detail::byte(status.jam_detail, 2),
        drones_moved: if status.spoofing { detail::byte(sd, 2) } else { 0 },
        aircraft_moved: if status.spoofing { detail::byte(sd, 1) } else { 0 },
        own_agrees: (status.jamming && own(status.jam_detail)) || (status.spoofing && own(sd)),
    })
}
