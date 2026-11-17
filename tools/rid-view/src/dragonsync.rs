//! DragonSync / WarDragon output (`--dragonsync`, `--dragonsync-signals`).
//!
//! DragonSync (<https://github.com/alphafox02/DragonSync>, Apache-2.0, the
//! TAK/MQTT bridge that runs on a WarDragon kit) subscribes over ZeroMQ to
//! JSON messages. This module publishes the two message shapes it reads, so
//! Rotorscope can be one more sensor for it. Only DragonSync's field names
//! and string conventions are used, as read from its `core/telemetry_parser.py`
//! and `ingest/signal_ingest.py`; none of its code is copied.
//!
//! Two sockets, because DragonSync reads two different streams:
//!
//! - **Drones** (WarDragon's port 4224, DragonSync `zmq_host`/`zmq_port`): one
//!   JSON array per drone update, a list of message blocks (`Basic ID`,
//!   `Location/Vector Message`, `System Message`, ...).
//! - **Signal alerts** (WarDragon's port 4226, DragonSync `fpv_zmq_*`): the
//!   same envelope plus a `Signal Info` block, for radio signals that are
//!   not Remote ID: analog FPV video, DJI links, open FPV links.
//!
//! They stay on separate ports on purpose. DragonSync's drone reader would
//! take a signal alert for a drone called `fpv-alert-...`, and its signal
//! reader accepts any message with a frequency in it.
//!
//! What is sent, and what is not:
//! - Remote ID and DJI DroneID tracks that DragonSync can key on: a serial
//!   number (any source) or an O4 session key ID (`drone-alert-<hex>`, as
//!   DragonSDR sends). A track with only a registration, UTM UUID or session
//!   ID is sent honestly typed; DragonSync only tracks drones with a serial,
//!   and attaches a registration to a drone it already knows by MAC.
//!   Tracks with no ID at all are not sent.
//! - Crewed aircraft (ADS-B) are not sent: DragonSync has its own ADS-B path.
//! - Wi-Fi drones recognised by maker (no frequency, no confirmation) are
//!   not sent.
//! - Signals: `source: "confirm"` only for verdicts Rotorscope treats as
//!   confirmed (analog video with a measured line rate, the DJI link
//!   fingerprints, open FPV links). HDZero, LTE-style, narrow Wi-Fi and an
//!   unconfirmed FPV carrier go out as `source: "energy"`, which DragonSync
//!   ignores unless `fpv_confirm_only = false`. Plain energy is not sent.
//! - Identity is not widened: DJI serials and O4 key IDs follow
//!   `--hide-droneid-ids`, and open-link identifiers follow
//!   `--show-link-ids`, exactly as in every other output.
//!
//! At most one message per track per tick (about a second), and only when
//! the track was heard since the last one.

use std::collections::HashMap;
use std::time::Duration;

use rid::Transport;
use rid::message::{
    FixedStr, HeightType, HorizontalAccuracy, IdType, SpeedAccuracy, Status, VerticalAccuracy,
};
use serde_json::{Map, Value, json};
use tracker::{Context, Emitter, EmitterId, LinkTrack, LinkTrackId, Track, TrackId, TrackState};
use zeromq::{PubSocket, Socket, SocketSend, ZmqMessage};

/// DragonSync's exact `id_type` strings (it compares them for equality).
const ID_SERIAL: &str = "Serial Number (ANSI/CTA-2063-A)";
const ID_CAA: &str = "CAA Assigned Registration ID";
const ID_UTM: &str = "UTM (UAS) Assigned ID";
const ID_SESSION: &str = "Specific Session ID";

// ------------------------------------------------------------- helpers --

/// Round for output: honest about the data and keeps lines short.
fn round(v: f64, places: i32) -> f64 {
    let k = 10f64.powi(places);
    (v * k).round() / k
}

fn num(v: f32) -> Value {
    json!(round(f64::from(v), 2))
}

/// A fixed text field as text: the string when it is clean ASCII, else hex.
fn fixed_text<const N: usize>(f: &FixedStr<N>) -> String {
    match f.as_str() {
        Some(s) if f.is_clean_text() => s.to_string(),
        _ => hex(f.as_bytes()),
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn mac_text(m: &[u8; 6]) -> String {
    format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        m[0], m[1], m[2], m[3], m[4], m[5]
    )
}

fn metres_text(m: f32) -> String {
    if m.fract() == 0.0 {
        format!("{m:.0}")
    } else {
        format!("{m:.1}")
    }
}

/// Wi-Fi channel to centre frequency, MHz. `None` for 0 or a channel that
/// is in neither band.
pub fn wifi_channel_mhz(ch: u8) -> Option<f64> {
    match ch {
        1..=13 => Some(2407.0 + 5.0 * f64::from(ch)),
        14 => Some(2484.0),
        32..=177 => Some(5000.0 + 5.0 * f64::from(ch)),
        _ => None,
    }
}

fn is_wifi(t: Transport) -> bool {
    matches!(t, Transport::WifiBeacon | Transport::WifiNan)
}

// -------------------------------------------------------------- drones --

fn status_text(s: Status) -> Option<&'static str> {
    Some(match s {
        Status::Undeclared => "Undeclared",
        Status::Ground => "Ground",
        Status::Airborne => "Airborne",
        Status::Emergency => "Emergency",
        Status::RemoteIdSystemFailure => "Remote ID System Failure",
        Status::Reserved(_) => return None,
    })
}

fn id_type_text(t: IdType) -> Option<&'static str> {
    Some(match t {
        IdType::SerialNumber => ID_SERIAL,
        IdType::CaaRegistration => ID_CAA,
        IdType::UtmAssigned => ID_UTM,
        IdType::SpecificSession => ID_SESSION,
        IdType::None | IdType::Reserved(_) => return None,
    })
}

/// Where the drone's radio was, MHz: a DroneID burst's centre, else the Wi-Fi
/// channel of the latest Wi-Fi frame. Bluetooth has none.
fn drone_freq_mhz(t: &Track) -> Option<f64> {
    t.dji.freq_mhz().or_else(|| {
        let (slot, l) = t.latest_slot()?;
        if is_wifi(slot.transport) {
            wifi_channel_mhz(l.channel)
        } else {
            None
        }
    })
}

/// The list-format message for one drone, or `None` if DragonSync could not
/// use it (no serial, registration or O4 key ID to name it by).
pub fn drone_message(t: &Track, ctx: &Context) -> Option<Value> {
    // IDs, serial last: DragonSync's parser keeps the last Basic ID's
    // `id_type`, and only takes `id` from a serial-typed block.
    let mut ids: Vec<(&'static str, String)> = t
        .uas_ids
        .iter()
        .filter_map(|u| Some((id_type_text(u.id_type)?, fixed_text(&u.id))))
        .collect();
    ids.sort_by_key(|(ty, _)| *ty == ID_SERIAL);
    if ids.is_empty() {
        // An encrypted O4 record: the session key ID is all there is, and it
        // is identity, so `--hide-droneid-ids` keeps it out.
        let key = t.dji.key_id.filter(|_| !ctx.config.hide_droneid_ids)?;
        ids.push((ID_SERIAL, format!("drone-alert-{}", hex(&key))));
    }

    let mac = t
        .latest_link()
        .map(|(_, l)| l.mac)
        .or_else(|| t.dji.wifi.heard().then_some(t.dji.wifi.mac));
    let freq = drone_freq_mhz(t);
    let transport = t.latest_slot().map(|(s, _)| s.name());

    let mut blocks: Vec<Value> = Vec::new();
    // First, so that the Basic ID's `frequency_mhz` (which DragonSync's
    // parser prefers, and which is in MHz) is read after it and wins: the
    // parser keeps whichever it reads last, and the units differ.
    if let Some(f) = freq {
        blocks.push(json!({ "Frequency Message": { "frequency": (f * 1e6).round() as u64 } }));
    }
    for (id_type, id) in &ids {
        // Every block repeats the common fields: a later Basic ID block
        // would otherwise blank `ua_type` in DragonSync's parser.
        let mut b = Map::new();
        b.insert("id_type".into(), json!(id_type));
        b.insert("id".into(), json!(id));
        if let Some(ua) = t.ua_type {
            b.insert("ua_type".into(), json!(ua.code()));
        }
        if let Some(m) = mac {
            b.insert("MAC".into(), json!(mac_text(&m)));
        }
        if let Some(r) = t.latest_rssi() {
            b.insert("RSSI".into(), json!(r));
        }
        if let Some(n) = transport {
            b.insert("transport".into(), json!(n));
        } else if t.dji.ocusync.heard() {
            b.insert("transport".into(), json!("DroneId"));
        }
        if let Some(f) = freq {
            b.insert("frequency_mhz".into(), json!(round(f, 1)));
        }
        blocks.push(json!({ "Basic ID": b }));
    }

    if let Some(l) = &t.location
        && let Some(p) = l.value.position
    {
        let l = &l.value;
        let mut m = Map::new();
        m.insert("latitude".into(), json!(p.lat));
        m.insert("longitude".into(), json!(p.lon));
        if let Some(v) = l.altitude_geodetic {
            m.insert("geodetic_altitude".into(), num(v));
        }
        if let Some(v) = l.altitude_pressure {
            m.insert("pressure_altitude".into(), num(v));
        }
        if let Some(v) = l.height {
            m.insert("height_agl".into(), num(v));
        }
        if let Some(v) = l.speed_horizontal {
            m.insert("speed".into(), num(v));
        }
        if let Some(v) = l.speed_vertical {
            m.insert("vert_speed".into(), num(v));
        }
        if let Some(v) = l.direction {
            m.insert("direction".into(), json!(v.round() as i32));
        }
        if let Some(s) = status_text(l.status) {
            m.insert("op_status".into(), json!(s));
        }
        m.insert(
            "height_type".into(),
            json!(match l.height_type {
                HeightType::AboveTakeoff => "Above Takeoff",
                HeightType::AboveGround => "Above Ground Level",
            }),
        );
        if let Some(v) = HorizontalAccuracy::meters(l.horizontal_accuracy) {
            m.insert(
                "horizontal_accuracy".into(),
                json!(format!("< {}m", metres_text(v))),
            );
        }
        if let Some(v) = VerticalAccuracy::meters(l.vertical_accuracy) {
            m.insert(
                "vertical_accuracy".into(),
                json!(format!("< {}m", metres_text(v))),
            );
        }
        if let Some(v) = VerticalAccuracy::meters(l.baro_accuracy) {
            m.insert(
                "baro_accuracy".into(),
                json!(format!("< {}m", metres_text(v))),
            );
        }
        if let Some(v) = SpeedAccuracy::meters_per_second(l.speed_accuracy) {
            m.insert(
                "speed_accuracy".into(),
                json!(format!("< {} m/s", metres_text(v))),
            );
        }
        if let Some(v) = l.timestamp {
            m.insert("timestamp".into(), num(v));
        }
        if let Some(v) = l.timestamp_accuracy {
            m.insert("timestamp_accuracy".into(), num(v));
        }
        blocks.push(json!({ "Location/Vector Message": m }));
    }

    // System: the operator position (the list format reads it as
    // latitude/longitude, the dict format as operator_lat/lon: both are
    // sent) and DJI's home point.
    let op = t.system.as_ref().and_then(|s| s.value.operator_position);
    let home = t.home.as_ref().map(|h| h.value);
    if op.is_some() || home.is_some() {
        let mut m = Map::new();
        if let Some(p) = op {
            m.insert("latitude".into(), json!(p.lat));
            m.insert("longitude".into(), json!(p.lon));
            m.insert("operator_lat".into(), json!(p.lat));
            m.insert("operator_lon".into(), json!(p.lon));
        }
        if let Some(h) = home {
            m.insert("home_lat".into(), json!(h.lat));
            m.insert("home_lon".into(), json!(h.lon));
        }
        blocks.push(json!({ "System Message": m }));
    }

    if let Some(o) = &t.operator_id {
        blocks.push(json!({ "Operator ID Message": {
            "operator_id_type": "Operator ID",
            "operator_id": fixed_text(&o.value.operator_id),
        }}));
    }

    let text = match &t.self_id {
        Some(s) => Some(fixed_text(&s.value.description)),
        None => t.dji.product_name().map(str::to_string),
    };
    if let Some(text) = text.filter(|s| !s.is_empty()) {
        blocks.push(json!({ "Self-ID Message": { "text": text } }));
    }

    Some(Value::Array(blocks))
}

// ------------------------------------------------------------- signals --

/// One signal alert, before it is written as JSON.
struct Alert<'a> {
    /// Basic ID prefix: `fpv-alert-5945.200MHz`.
    prefix: &'a str,
    description: &'a str,
    /// Self-ID text.
    text: String,
    /// `confirm` or `energy`.
    source: &'a str,
    signal_type: &'a str,
    center_hz: u64,
    bandwidth_hz: Option<u64>,
    snr_db: Option<i8>,
    rssi_dbm: Option<i8>,
    /// Extra Signal Info fields.
    extra: Vec<(&'a str, Value)>,
    /// A suffix for the Basic ID (a shown link ID).
    id_suffix: String,
}

fn alert_message(a: Alert<'_>, ctx: &Context) -> Value {
    let mhz = a.center_hz as f64 / 1e6;
    let id = format!("{}-alert-{mhz:.3}MHz{}", a.prefix, a.id_suffix);
    let mut blocks = vec![json!({ "Basic ID": {
        "id_type": ID_SERIAL,
        "id": id,
        "description": a.description,
    }})];
    // The sensor's own position, if known: DragonSync draws the alert near
    // the sensor (and falls back to the kit's GPS when this is absent).
    if let Some(own) = &ctx.own {
        let mut m = Map::new();
        m.insert("latitude".into(), json!(own.lat));
        m.insert("longitude".into(), json!(own.lon));
        if let Some(h) = own.alt_m {
            m.insert("geodetic_altitude".into(), json!(round(h, 1)));
        }
        blocks.push(json!({ "Location/Vector Message": m }));
    }
    blocks.push(json!({ "Self-ID Message": { "text": a.text } }));
    blocks.push(json!({ "Frequency Message": { "frequency": a.center_hz } }));
    let mut s = Map::new();
    s.insert("source".into(), json!(a.source));
    s.insert("signal_type".into(), json!(a.signal_type));
    s.insert("center_hz".into(), json!(a.center_hz));
    if let Some(b) = a.bandwidth_hz.filter(|b| *b > 0) {
        s.insert("bandwidth_hz".into(), json!(b));
    }
    if let Some(v) = a.snr_db {
        s.insert("snr_db".into(), json!(v));
    }
    if let Some(v) = a.rssi_dbm {
        s.insert("rssi".into(), json!(v));
    }
    for (k, v) in a.extra {
        s.insert(k.into(), v);
    }
    blocks.push(json!({ "Signal Info": s }));
    Value::Array(blocks)
}

/// The alert for one raw-capture RF emitter, or `None` for plain energy.
pub fn emitter_message(e: &Emitter, ctx: &Context) -> Option<Value> {
    use link::RfKind;
    // (prefix, signal_type, confirmed?)
    let (prefix, signal_type, confirmed) = match e.kind {
        RfKind::AnalogVideo => ("fpv", "analog_fpv", e.line_hz > 0),
        RfKind::FpvCarrier => ("fpv", "fpv_carrier", false),
        RfKind::Hdzero => ("fpv", "hdzero", false),
        RfKind::LteStyle => ("rf", "lte_ofdm", false),
        RfKind::WifiNarrow => ("rf", "wifi_narrow", false),
        RfKind::DjiVideo => ("dji", "dji_video", true),
        RfKind::DjiO4Video => ("dji", "dji_o4_video", true),
        RfKind::DjiFpvAirUnit => ("dji", "dji_fpv_air_unit", true),
        RfKind::DjiController => ("dji", "dji_controller", true),
        RfKind::Energy => return None,
    };
    let source = if confirmed { "confirm" } else { "energy" };
    let label = tracker::signals::rf_label(e.kind);
    let mut extra = Vec::new();
    if e.line_hz > 0 {
        extra.push(("line_hz", json!(e.line_hz)));
    }
    Some(alert_message(
        Alert {
            prefix,
            description: label,
            text: format!("{label} ({source})"),
            source,
            signal_type,
            center_hz: u64::from(e.freq) * 100_000,
            bandwidth_hz: Some(u64::from(e.width_khz) * 1000),
            snr_db: Some(e.snr_db),
            rssi_dbm: None,
            extra,
            id_suffix: String::new(),
        },
        ctx,
    ))
}

/// The alert for one open FPV link, or `None` when the node did not say
/// which Wi-Fi channel it heard it on (no frequency, DragonSync drops it).
pub fn open_link_message(l: &LinkTrack, ctx: &Context) -> Option<Value> {
    use openlink::System;
    let s = l.latest()?;
    let mhz = wifi_channel_mhz(s.channel)?;
    let (signal_type, name) = match l.system {
        System::WfbNg => ("wfb_ng", "wfb-ng"),
        System::OpenHd => ("openhd", "OpenHD"),
        System::RubyFpv => ("rubyfpv", "RubyFPV"),
        System::DroneBridge => ("dronebridge", "DroneBridge"),
    };
    // The link ID identifies someone's equipment: only with --show-link-ids.
    let shown = ctx.config.show_link_ids.then_some(l.link_id).flatten();
    let text = match shown {
        Some(id) => format!("{name} FPV link {id:#x} (confirm)"),
        None => format!("{name} FPV link (confirm)"),
    };
    Some(alert_message(
        Alert {
            prefix: "fpv",
            description: "Open FPV link",
            text,
            source: "confirm",
            signal_type,
            center_hz: (mhz * 1e6).round() as u64,
            bandwidth_hz: None,
            snr_db: None,
            rssi_dbm: Some(s.rssi),
            extra: Vec::new(),
            id_suffix: shown.map_or(String::new(), |id| format!("-{id:x}")),
        },
        ctx,
    ))
}

// ----------------------------------------------------------- publishing --

/// A ZeroMQ PUB socket on its own thread. Pure Rust (`zeromq`), no libzmq.
pub struct Bus {
    tx: tokio::sync::mpsc::UnboundedSender<String>,
    /// Where it is listening, as resolved by the bind.
    pub endpoint: String,
}

impl Bus {
    /// Bind `tcp://HOST:PORT` (a bare `HOST:PORT` is read as TCP too).
    pub fn bind(what: &str, endpoint: &str) -> Result<Bus, String> {
        let ep = if endpoint.contains("://") {
            endpoint.to_string()
        } else {
            format!("tcp://{endpoint}")
        };
        if !ep.starts_with("tcp://") {
            return Err(format!("{what}: only tcp://HOST:PORT is supported"));
        }
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<String, String>>();
        let ep2 = ep.clone();
        std::thread::Builder::new()
            .name("dragonsync-pub".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e.to_string()));
                        return;
                    }
                };
                rt.block_on(async move {
                    let mut sock = PubSocket::new();
                    match sock.bind(&ep2).await {
                        Ok(bound) => {
                            let _ = ready_tx.send(Ok(bound.to_string()));
                        }
                        Err(e) => {
                            let _ = ready_tx.send(Err(e.to_string()));
                            return;
                        }
                    }
                    while let Some(m) = rx.recv().await {
                        // A PUB socket never waits for subscribers; the
                        // timeout is only a guard against a stuck peer.
                        let _ = tokio::time::timeout(
                            Duration::from_secs(1),
                            sock.send(ZmqMessage::from(m)),
                        )
                        .await;
                    }
                });
            })
            .map_err(|e| format!("{what}: {e}"))?;
        let endpoint = ready_rx
            .recv()
            .map_err(|_| format!("{what}: publisher thread died"))?
            .map_err(|e| format!("{what} {ep}: {e}"))?;
        Ok(Bus { tx, endpoint })
    }

    pub fn send(&self, json: String) {
        let _ = self.tx.send(json);
    }
}

/// The two outputs and what each track was last sent at.
pub struct DragonSync {
    drones: Option<Bus>,
    signals: Option<Bus>,
    sent: HashMap<TrackId, u64>,
    sent_rf: HashMap<EmitterId, u64>,
    sent_links: HashMap<LinkTrackId, u64>,
}

impl DragonSync {
    pub fn new(drones: Option<Bus>, signals: Option<Bus>) -> Self {
        Self {
            drones,
            signals,
            sent: HashMap::new(),
            sent_rf: HashMap::new(),
            sent_links: HashMap::new(),
        }
    }

    /// What is due now: `(drone messages, signal alerts)`, as JSON text. A
    /// track is due when it has been heard since it was last sent.
    pub fn due(&mut self, tr: &tracker::Tracker, ctx: &Context) -> (Vec<String>, Vec<String>) {
        let mut drones = Vec::new();
        let mut signals = Vec::new();
        if self.drones.is_some() {
            for t in tr.tracks() {
                if ctx.state(t) != TrackState::Active
                    || self.sent.get(&t.id).is_some_and(|&s| s >= t.last_seen_ms)
                {
                    continue;
                }
                self.sent.insert(t.id, t.last_seen_ms);
                if let Some(m) = drone_message(t, ctx) {
                    drones.push(m.to_string());
                }
            }
            self.sent.retain(|id, _| tr.get(*id).is_some());
        }
        if self.signals.is_some() {
            for e in tr.emitters() {
                if self
                    .sent_rf
                    .get(&e.id)
                    .is_some_and(|&s| s >= e.last_seen_ms)
                {
                    continue;
                }
                self.sent_rf.insert(e.id, e.last_seen_ms);
                if let Some(m) = emitter_message(e, ctx) {
                    signals.push(m.to_string());
                }
            }
            for l in tr.open_links() {
                if ctx.link_state(l) != TrackState::Active
                    || self
                        .sent_links
                        .get(&l.id)
                        .is_some_and(|&s| s >= l.last_seen_ms)
                {
                    continue;
                }
                self.sent_links.insert(l.id, l.last_seen_ms);
                if let Some(m) = open_link_message(l, ctx) {
                    signals.push(m.to_string());
                }
            }
            self.sent_rf.retain(|id, _| tr.get_emitter(*id).is_some());
            self.sent_links
                .retain(|id, _| tr.get_open_link(*id).is_some());
        }
        (drones, signals)
    }

    /// Publish what is due. Called once a second.
    pub fn tick(&mut self, tr: &tracker::Tracker, ctx: &Context) {
        let (drones, signals) = self.due(tr, ctx);
        if let Some(b) = &self.drones {
            drones.into_iter().for_each(|m| b.send(m));
        }
        if let Some(b) = &self.signals {
            signals.into_iter().for_each(|m| b.send(m));
        }
    }
}

#[cfg(test)]
mod tests;
