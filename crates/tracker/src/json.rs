//! One JSON object per track, for the phone page and the USB stream (O1, O4).
//! The schema is documented in `crates/tracker/README.md`; change both together.
//!
//! Enum values use the `rid` crate's names (`"WifiBeacon"`,
//! `"SerialNumber"`, ...) so the node stream (`{"t":"rid",...}`) and the
//! track stream (`{"t":"track",...}`) speak the same vocabulary.

use serde::Serialize;

use crate::aircraft::{AircraftTrack, icao_hex, same_drone};
use crate::dji::Source;
use crate::openlinks::{LinkTrack, LinkTrackId, shown_id};
use crate::signals::{Emitter, EmitterId, WifiDrone, WifiDroneId, rf_label};
use crate::text::{FormatError, Mac};
use crate::{Band, Context, Track, TrackId, TrackState};
use openlink::{Direction, System};
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
    /// Where `loc` and `op` came from: `"RemoteId"`, `"DroneId"` or
    /// `"DjiWifi"`; null when absent. Added with protocol 4.
    loc_src: Option<Source>,
    op_src: Option<Source>,
    /// DJI home point (DroneID only).
    home: Option<Home>,
    /// What DJI's own broadcasts said; null when none was heard.
    dji: Option<Dji<'a>>,
    /// Spoofing checks that fired, and authentication pages seen; null
    /// when neither (see `crate::spoof`).
    spoof: Option<SpoofJ<'a>>,
    /// The track this one probably continues under a new identity; null
    /// when none (see `crate::continuity`).
    cont: Option<ContJ>,
}

#[derive(Serialize)]
struct SpoofJ<'a> {
    /// Combined suspicion 0-100 (below 30 when every flag is weak).
    score: u8,
    flags: SpoofFlags<'a>,
    /// Authentication pages heard (count of distinct page numbers). Not
    /// verified: the hub has no keys.
    auth: u32,
}

#[derive(Serialize)]
struct FlagJ {
    check: crate::spoof::Check,
    conf: u8,
    detail: i32,
}

struct SpoofFlags<'a>(&'a [crate::spoof::Flag]);

impl Serialize for SpoofFlags<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(self.0.iter().map(|f| FlagJ {
            check: f.check,
            conf: f.confidence,
            detail: f.detail,
        }))
    }
}

#[derive(Serialize)]
struct ContJ {
    prev: TrackId,
    conf: u8,
    id_changed: bool,
}

fn spoof_json(t: &Track) -> Option<SpoofJ<'_>> {
    let s = &t.spoof;
    (!s.flags().is_empty() || s.auth_pages != 0).then(|| SpoofJ {
        score: s.score(),
        flags: SpoofFlags(s.flags()),
        auth: s.auth_pages.count_ones(),
    })
}

#[derive(Serialize)]
struct Home {
    lat: f64,
    lon: f64,
    age_ms: u64,
}

#[derive(Serialize)]
struct Dji<'a> {
    /// DJI product code and its name, when sent.
    product: Option<u8>,
    model: Option<&'static str>,
    /// Whether this unit shows DroneID identity (`!hide_droneid_ids`).
    ids_shown: bool,
    /// O4 session key ID, 8 hex digits; null when hidden or not O4.
    key_id: Option<heapless::String<8>>,
    /// Encrypted (O4) records only, not decrypted: position and serial
    /// unreadable.
    encrypted: bool,
    /// O4 records decrypted with the owner's key (serial and positions are
    /// from the decrypted records).
    decrypted: bool,
    /// The shared track for bursts that did not decode.
    undecoded: bool,
    /// O4 record types heard (protocol 8 nodes), and what they suggest:
    /// `GpsLocked` (CRYP only) or `ProbablyFlying` (INFP), model-dependent.
    o4: Option<O4J>,
    ocusync: Option<Ocu>,
    wifi: Option<DjiWifiJ<'a>>,
}

#[derive(Serialize)]
struct O4J {
    cryp: u32,
    infp10: u32,
    infp11: u32,
    state: Option<crate::dji::O4State>,
    /// Always true: the state is inferred from one model's behaviour.
    model_dependent: bool,
}

#[derive(Serialize)]
struct Ocu {
    n: u32,
    rssi: i8,
    snr: i8,
    mhz: f64,
    age_ms: u64,
    node: u8,
}

#[derive(Serialize)]
struct DjiWifiJ<'a> {
    n: u32,
    rssi: i8,
    ch: u8,
    band: Band,
    mac: Mac<'a>,
    age_ms: u64,
    node: u8,
}

/// Eight lowercase hex digits.
pub(crate) fn key_hex(k: [u8; 4]) -> heapless::String<8> {
    crate::text::render(KeyHex(k))
}

struct KeyHex([u8; 4]);

impl core::fmt::Display for KeyHex {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

fn dji_json<'a>(t: &'a Track, ctx: &Context) -> Option<Dji<'a>> {
    let d = &t.dji;
    if !d.heard() {
        return None;
    }
    let now = ctx.now_ms;
    let shown = !ctx.config.hide_droneid_ids;
    Some(Dji {
        product: d.product_type,
        model: d.product_name(),
        ids_shown: shown,
        key_id: d.key_id.filter(|_| shown).map(key_hex),
        encrypted: d.key_id.is_some() && !d.o4_decrypted,
        decrypted: d.o4_decrypted,
        undecoded: d.undecoded,
        o4: d.o4.heard().then(|| O4J {
            cryp: d.o4.cryp,
            infp10: d.o4.infp10,
            infp11: d.o4.infp11,
            state: d.o4.state(),
            model_dependent: true,
        }),
        ocusync: d.ocusync.heard().then(|| Ocu {
            n: d.ocusync.count,
            rssi: d.ocusync.rssi,
            snr: d.ocusync.snr_db,
            mhz: f64::from(d.ocusync.freq) / 10.0,
            age_ms: now.saturating_sub(d.ocusync.last_seen_ms),
            node: d.ocusync.node,
        }),
        wifi: d.wifi.heard().then(|| DjiWifiJ {
            n: d.wifi.count,
            rssi: d.wifi.rssi,
            ch: d.wifi.channel,
            band: Band::of_wifi_channel(d.wifi.channel),
            mac: Mac(&d.wifi.mac),
            age_ms: now.saturating_sub(d.wifi.last_seen_ms),
            node: d.wifi.node,
        }),
    })
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
    /// `"2.4"` or `"5"`. Added with the band split; a Wi-Fi transport heard
    /// on both bands has one entry per band. Readers that predate it still
    /// work: they see two entries with the same `tr`.
    band: Band,
    mac: Mac<'a>,
    ctr: u8,
    age_ms: u64,
    node: u8,
}

struct Links<'a>(&'a Track, u64);

impl Serialize for Links<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let now = self.1;
        s.collect_seq(self.0.slots().map(|(slot, l)| LinkEntry {
            tr: slot.transport,
            n: l.count,
            rssi: l.rssi,
            ch: l.channel,
            band: slot.band,
            mac: Mac(&l.mac),
            ctr: l.counter,
            age_ms: now.saturating_sub(l.last_seen_ms),
            node: l.node,
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

/// A buffer this size always holds one line (worst case: all seven slots,
/// i.e. every transport plus 5 GHz beacon and NAN, two IDs, DJI DroneID and
/// Wi-Fi DroneID too, every field at its longest; see the test). Kept tight
/// because the hub holds several of these in static task state without
/// PSRAM. (1792 before protocol 4 added the `dji` object; 2304 before
/// protocol 8 added `dji.o4`; 2432 before the `spoof` and `cont` objects,
/// which add up to 373 bytes; worst case now 2702 bytes.)
pub const MAX_LINE: usize = 2816;

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
        rssi: track.latest_rssi(),
        links: Links(track, now),
        loc,
        op,
        self_id: track.self_id.as_ref().map(|s| &s.value.description),
        op_id: track.operator_id.as_ref().map(|s| &s.value.operator_id),
        own,
        loc_src: track.location.as_ref().map(|_| track.location_src),
        op_src: track.system.as_ref().map(|_| track.system_src),
        home: track.home.as_ref().map(|h| Home {
            lat: h.value.lat,
            lon: h.value.lon,
            age_ms: now.saturating_sub(h.rx_ms),
        }),
        dji: dji_json(track, ctx),
        spoof: spoof_json(track),
        cont: track.continuity.previous.map(|prev| ContJ {
            prev,
            conf: track.continuity.link_confidence,
            id_changed: track.continuity.identity_changed,
        }),
    };
    let n = serde_json_core::to_slice(&line, buf).map_err(|_| FormatError::BufferTooSmall)?;
    let nl = buf.get_mut(n).ok_or(FormatError::BufferTooSmall)?;
    *nl = b'\n';
    Ok(n + 1)
}

// ------------------------------------------------------------- open links --

#[derive(Serialize)]
struct LinkLine<'a> {
    t: &'static str,
    id: LinkTrackId,
    state: TrackState,
    sys: System,
    label: &'static str,
    /// Whether this unit shows identifiers (`Config::show_link_ids`). When
    /// false, `link_id` and every `mac` are null whatever the link sent.
    ids_shown: bool,
    link_id: Option<heapless::String<10>>,
    first_ms: u64,
    last_ms: u64,
    age_ms: u64,
    rssi: Option<i8>,
    ch: Option<u8>,
    band: Option<Band>,
    /// Heard in each direction.
    down: bool,
    up: bool,
    pkts: u32,
    streams: Streams<'a>,
}

#[derive(Serialize)]
struct StreamEntry<'a> {
    port: u8,
    /// wfb-ng only: `"video"`, `"mavlink"` or `"tunnel"`, from the port.
    kind: Option<&'static str>,
    dir: Direction,
    ch: u8,
    band: Band,
    rssi: i8,
    n: u32,
    mac: Option<Mac<'a>>,
    age_ms: u64,
    node: u8,
}

struct Streams<'a>(&'a LinkTrack, &'a Context);

impl Serialize for Streams<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let (t, ctx) = (self.0, self.1);
        let show = ctx.config.show_link_ids;
        s.collect_seq(t.streams.iter().map(|x| StreamEntry {
            port: x.port,
            kind: stream_kind(t.system, x.port),
            dir: x.direction,
            ch: x.channel,
            band: x.band(),
            rssi: x.rssi,
            n: x.packets,
            mac: show.then_some(Mac(&x.mac)),
            age_ms: ctx.now_ms.saturating_sub(x.last_seen_ms),
            node: x.node,
        }))
    }
}

/// wfb-ng's stream class (spec 4.1) as a word, `None` for other systems.
pub(crate) fn stream_kind(system: System, port: u8) -> Option<&'static str> {
    let l = openlink::OpenLink {
        system,
        id: None,
        port,
        direction: Direction::Unknown,
        tx: [0; 6],
    };
    l.wfb_stream().map(|k| match k {
        openlink::WfbStream::Video => "video",
        openlink::WfbStream::Mavlink => "mavlink",
        openlink::WfbStream::Tunnel => "tunnel",
    })
}

/// A buffer this size always holds one link line (worst case: six streams,
/// identifiers shown, every number at its widest; see the test). The hub
/// streams link lines through its `MAX_LINE` buffer, so this must not exceed it.
pub const MAX_LINK_LINE: usize = 1344;
const _: () = assert!(MAX_LINK_LINE <= MAX_LINE);

/// Write one open link as a JSON object followed by `\n` (`"t":"link"`;
/// schema in `crates/tracker/README.md`). Returns bytes written.
pub fn link_line(t: &LinkTrack, ctx: &Context, buf: &mut [u8]) -> Result<usize, FormatError> {
    let latest = t.latest();
    let line = LinkLine {
        t: "link",
        id: t.id,
        state: ctx.link_state(t),
        sys: t.system,
        label: t.system.label(),
        ids_shown: ctx.config.show_link_ids,
        link_id: shown_id(t, ctx),
        first_ms: t.first_seen_ms,
        last_ms: t.last_seen_ms,
        age_ms: ctx.now_ms.saturating_sub(t.last_seen_ms),
        rssi: latest.map(|s| s.rssi),
        ch: latest.map(|s| s.channel),
        band: latest.map(|s| s.band()),
        down: t.heard(Direction::AirToGround),
        up: t.heard(Direction::GroundToAir),
        pkts: t.packets(),
        streams: Streams(t, ctx),
    };
    let n = serde_json_core::to_slice(&line, buf).map_err(|_| FormatError::BufferTooSmall)?;
    let nl = buf.get_mut(n).ok_or(FormatError::BufferTooSmall)?;
    *nl = b'\n';
    Ok(n + 1)
}

// --------------------------------------------------------------- aircraft --

#[derive(Serialize)]
struct AircraftLine<'a> {
    t: &'static str,
    /// Six upper-case hex digits.
    icao: heapless::String<6>,
    /// `"1090"`, `"978"` (UAT), `"OGN"` or `"FLARM"`.
    src: &'static str,
    /// UAT address qualifier (2, 3 = relayed by a TIS-B ground station).
    aq: u8,
    state: TrackState,
    call: Option<&'a str>,
    /// Four octal digits, e.g. `"7700"`.
    squawk: Option<heapless::String<4>>,
    /// `"hijack"`, `"radio failure"` or `"emergency"` for 7500/7600/7700.
    emerg: Option<&'static str>,
    cat: i8,
    kind: &'static str,
    label: &'static str,
    mil: bool,
    air: bool,
    lat: Option<f64>,
    lon: Option<f64>,
    pos_age_ms: Option<u64>,
    alt_baro_ft: Option<i32>,
    alt_geo_ft: Option<i32>,
    spd_kt: Option<i32>,
    trk: Option<f32>,
    vr_fpm: Option<i32>,
    rssi: i16,
    first_ms: u64,
    last_ms: u64,
    age_ms: u64,
    /// The Remote ID drone track this probably is (ADS-B category UAV
    /// within 300 m of it), else null.
    drone: Option<TrackId>,
    own: Option<AircraftOwn>,
}

#[derive(Serialize)]
struct AircraftOwn {
    dist_m: Option<f64>,
    brg: Option<f64>,
    above_m: Option<f64>,
    /// Low aircraft nearby: a quiet heads-up, never an alert.
    low: bool,
}

/// A buffer this size always holds one aircraft line (worst case: see the
/// test). The hub streams these through its `MAX_LINE` buffer.
pub const MAX_AIRCRAFT_LINE: usize = 704;
const _: () = assert!(MAX_AIRCRAFT_LINE <= MAX_LINE);

/// Write one aircraft as a JSON object followed by `\n` (`"t":"aircraft"`;
/// schema in `crates/tracker/README.md`). `drones` is the drone table, for
/// the `drone` field. Returns bytes written.
pub fn aircraft_line(
    a: &AircraftTrack,
    drones: &[Track],
    ctx: &Context,
    buf: &mut [u8],
) -> Result<usize, FormatError> {
    let now = ctx.now_ms;
    let pos = a.position.as_ref();
    let own = ctx.own.map(|_| {
        let g = ctx.aircraft_geometry(a);
        AircraftOwn {
            dist_m: g.range.map(|r| round1(r.distance_m)),
            brg: g.range.map(|r| round_bearing(r.bearing_deg)),
            above_m: g.above_m.map(round1),
            low: g.low_nearby && ctx.aircraft_state(a) == TrackState::Active,
        }
    });
    let line = AircraftLine {
        t: "aircraft",
        icao: icao_hex(a),
        src: match a.key.source {
            adsb::Source::ModeS => "1090",
            adsb::Source::Uat => "978",
            adsb::Source::Ogn => "OGN",
            adsb::Source::Flarm => "FLARM",
        },
        aq: a.address_qualifier,
        state: ctx.aircraft_state(a),
        call: (!a.callsign.is_empty()).then(|| a.callsign.as_str()),
        squawk: a.squawk.map(|s| crate::text::render(Octal4(s))),
        emerg: a.emergency(),
        cat: a.category,
        kind: a.kind().name(),
        label: a.kind().label(),
        mil: a.military,
        air: a.airborne,
        lat: pos.map(|p| p.value.lat),
        lon: pos.map(|p| p.value.lon),
        pos_age_ms: pos.map(|p| now.saturating_sub(p.rx_ms)),
        alt_baro_ft: a.baro_alt_ft,
        alt_geo_ft: a.gnss_alt_ft,
        spd_kt: a.speed_kt,
        trk: a.direction_deg,
        vr_fpm: a.vrate_fpm,
        rssi: a.rssi_dbm,
        first_ms: a.first_seen_ms,
        last_ms: a.last_seen_ms,
        age_ms: now.saturating_sub(a.last_seen_ms),
        drone: same_drone(a, drones, ctx),
        own,
    };
    let n = serde_json_core::to_slice(&line, buf).map_err(|_| FormatError::BufferTooSmall)?;
    let nl = buf.get_mut(n).ok_or(FormatError::BufferTooSmall)?;
    *nl = b'\n';
    Ok(n + 1)
}

/// A squawk as four octal digits.
pub(crate) struct Octal4(pub u16);

impl core::fmt::Display for Octal4 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:04o}", self.0 & 0o7777)
    }
}

// ------------------------------------------------------- RF / Wi-Fi drones --

#[derive(Serialize)]
struct RfLine {
    t: &'static str,
    id: EmitterId,
    state: TrackState,
    /// `link::RfKind` name: `"AnalogVideo"`, `"FpvCarrier"`, `"Hdzero"`,
    /// `"LteStyle"`, `"WifiNarrow"`, `"Energy"`, `"DjiVideo"`,
    /// `"DjiO4Video"`, `"DjiFpvAirUnit"` or `"DjiController"`.
    kind: link::RfKind,
    label: &'static str,
    /// Whether this kind raises the new-signal alert (never for Energy).
    alerts: bool,
    mhz: f64,
    width_khz: Option<u16>,
    snr: i8,
    peak_snr: i8,
    duty: u8,
    line_hz: Option<u16>,
    n: u32,
    node: u8,
    first_ms: u64,
    last_ms: u64,
    age_ms: u64,
}

/// A buffer this size always holds one RF line (see the test).
pub const MAX_RF_LINE: usize = 384;
const _: () = assert!(MAX_RF_LINE <= MAX_LINE);

/// Write one RF emitter as a JSON object followed by `\n` (`"t":"rf"`).
pub fn rf_line(e: &Emitter, ctx: &Context, buf: &mut [u8]) -> Result<usize, FormatError> {
    let line = RfLine {
        t: "rf",
        id: e.id,
        state: ctx.emitter_state(e),
        kind: e.kind,
        label: rf_label(e.kind),
        alerts: crate::signals::rf_alerts(e.kind),
        mhz: f64::from(e.freq) / 10.0,
        width_khz: (e.width_khz != 0).then_some(e.width_khz),
        snr: e.snr_db,
        peak_snr: e.peak_snr_db,
        duty: e.duty_pct,
        line_hz: (e.line_hz != 0).then_some(e.line_hz),
        n: e.sightings,
        node: e.node,
        first_ms: e.first_seen_ms,
        last_ms: e.last_seen_ms,
        age_ms: ctx.now_ms.saturating_sub(e.last_seen_ms),
    };
    finish(&line, buf)
}

#[derive(Serialize)]
struct WifiDroneLine<'a> {
    t: &'static str,
    id: WifiDroneId,
    state: TrackState,
    maker: &'static str,
    maker_id: u8,
    ssid: &'a str,
    /// Whether this unit shows transmitter addresses (`show_link_ids`).
    ids_shown: bool,
    mac: Option<Mac<'a>>,
    ch: u8,
    band: Band,
    rssi: i8,
    n: u32,
    node: u8,
    first_ms: u64,
    last_ms: u64,
    age_ms: u64,
}

/// A buffer this size always holds one Wi-Fi drone line (see the test).
pub const MAX_WIFI_DRONE_LINE: usize = 448;
const _: () = assert!(MAX_WIFI_DRONE_LINE <= MAX_LINE);

/// Write one Wi-Fi drone as a JSON object followed by `\n` (`"t":"wifidrone"`).
pub fn wifi_drone_line(w: &WifiDrone, ctx: &Context, buf: &mut [u8]) -> Result<usize, FormatError> {
    let show = ctx.config.show_link_ids;
    let line = WifiDroneLine {
        t: "wifidrone",
        id: w.id,
        state: ctx.wifi_drone_state(w),
        maker: w.maker_name(),
        maker_id: w.maker,
        ssid: w.ssid.as_str(),
        ids_shown: show,
        mac: show.then_some(Mac(&w.mac)),
        ch: w.channel,
        band: w.band(),
        rssi: w.rssi,
        n: w.sightings,
        node: w.node,
        first_ms: w.first_seen_ms,
        last_ms: w.last_seen_ms,
        age_ms: ctx.now_ms.saturating_sub(w.last_seen_ms),
    };
    finish(&line, buf)
}

fn finish<T: Serialize>(line: &T, buf: &mut [u8]) -> Result<usize, FormatError> {
    let n = serde_json_core::to_slice(line, buf).map_err(|_| FormatError::BufferTooSmall)?;
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
    /// Serialized as `"Wifi24"`, `"Wifi5"`, `"Ble"`, `"Fpv"`, `"DroneId"`
    /// or `"Gateway"`.
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
    /// Protocol 4: roles the node can take (mask of `NodeRole::bit`), from
    /// its Status frame; `None` before one arrives or from older nodes.
    pub can: Option<u8>,
    /// Protocol 4: the hub plan it has applied (0 = its built-in one).
    pub plan: Option<u16>,
}

/// Health of the ADS-B receiver (an ADSBee on a hub UART), for the page's
/// status strip. `None` in [`HubStatus::adsb`] when the hub has none built in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AdsbStatus {
    /// A valid line arrived recently (the ADSBee reports once a second).
    pub alive: bool,
    /// Time since the last valid line; `None` if never.
    pub age_ms: Option<u64>,
    /// Valid frames per second over both bands, from its statistics line.
    pub frames_per_s: Option<u32>,
    /// Aircraft in the ADSBee's own table (may exceed the hub's).
    pub aircraft: Option<u16>,
    /// Lines refused (bad checksum, malformed), total since the hub booted.
    pub errors: u32,
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
    /// The ADS-B receiver, if the hub has one built in.
    pub adsb: Option<AdsbStatus>,
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
    adsb: Option<AdsbStatus>,
}

/// Schema version of the state object (`"v"`). Bump on incompatible change.
pub const STATE_VERSION: u8 = 1;

/// Bytes the state object needs besides its nodes, open links, tracks and
/// aircraft (worst case, with the empty arrays' brackets).
pub const STATE_HEAD_MAX: usize = 576;
/// Bytes one node entry needs (worst case, with its comma).
pub const STATE_NODE_MAX: usize = 160;

/// A buffer this size always holds the state object for `tracks` tracks,
/// `links` open links, `aircraft` aircraft and `nodes` nodes. E.g. 64
/// tracks, 8 links, 32 aircraft and 4 nodes: about 147 KB (put it in PSRAM).
/// A typical object is far smaller (~1 KB per track, ~0.4 KB per link or
/// aircraft); see `state` for what happens when it does not fit.
pub const fn state_max_len(tracks: usize, links: usize, aircraft: usize, nodes: usize) -> usize {
    STATE_HEAD_MAX
        + nodes * STATE_NODE_MAX
        + links * MAX_LINK_LINE
        + tracks * MAX_LINE
        + aircraft * MAX_AIRCRAFT_LINE
}

/// Between the open links and the tracks of the state object. A server that
/// streams the object in pieces (the hub) writes exactly these bytes.
pub const STATE_LINKS_TO_TRACKS: &[u8] = b"],\"tracks\":[";
/// Between the tracks and the aircraft.
pub const STATE_TRACKS_TO_AIRCRAFT: &[u8] = b"],\"aircraft\":[";
/// The end of the state object.
pub const STATE_END: &[u8] = b"]}\n";
/// [`state_full`] only: between the aircraft and the RF emitters.
pub const STATE_AIRCRAFT_TO_RF: &[u8] = b"],\"rf\":[";
/// [`state_full`] only: between the RF emitters and the Wi-Fi drones.
pub const STATE_RF_TO_WIFI: &[u8] = b"],\"wifi_drones\":[";

/// As [`state_max_len`], plus room for `rf` emitters and `wifi` Wi-Fi drones
/// in [`state_full`].
pub const fn state_full_max_len(
    tracks: usize,
    links: usize,
    aircraft: usize,
    nodes: usize,
    rf: usize,
    wifi: usize,
) -> usize {
    state_max_len(tracks, links, aircraft, nodes)
        + STATE_AIRCRAFT_TO_RF.len()
        + STATE_RF_TO_WIFI.len()
        + rf * (MAX_RF_LINE + 1)
        + wifi * (MAX_WIFI_DRONE_LINE + 1)
}

/// The protocol-4 tables [`state_full`] adds after the aircraft.
#[derive(Debug, Clone, Copy, Default)]
pub struct Signals<'a> {
    pub emitters: &'a [Emitter],
    pub wifi_drones: &'a [WifiDrone],
}

/// [`state`], then `"rf":[...]` (each as [`rf_line`]) and
/// `"wifi_drones":[...]` (each as [`wifi_drone_line`]) after the aircraft.
/// `state` itself is unchanged, so a hub that streams it piece by piece
/// keeps working; a reader treats missing `rf` / `wifi_drones` as empty.
pub fn state_full(
    tracks: &[Track],
    links: &[LinkTrack],
    aircraft: &[AircraftTrack],
    signals: &Signals,
    ctx: &Context,
    hub: &HubStatus,
    buf: &mut [u8],
) -> Result<usize, FormatError> {
    let mut n = state(tracks, links, aircraft, ctx, hub, buf)? - STATE_END.len();
    put(buf, &mut n, STATE_AIRCRAFT_TO_RF)?;
    for (i, e) in signals.emitters.iter().enumerate() {
        if i > 0 {
            put(buf, &mut n, b",")?;
        }
        let rest = buf.get_mut(n..).ok_or(FormatError::BufferTooSmall)?;
        n += rf_line(e, ctx, rest)? - 1;
    }
    put(buf, &mut n, STATE_RF_TO_WIFI)?;
    for (i, w) in signals.wifi_drones.iter().enumerate() {
        if i > 0 {
            put(buf, &mut n, b",")?;
        }
        let rest = buf.get_mut(n..).ok_or(FormatError::BufferTooSmall)?;
        n += wifi_drone_line(w, ctx, rest)? - 1;
    }
    put(buf, &mut n, STATE_END)?;
    Ok(n)
}

/// Write the `/api/state` body: one JSON object followed by `\n`. Returns
/// bytes written.
///
/// `tracks` is normally `tracker.tracks()`, `links` is
/// `tracker.open_links()`, `aircraft` is `tracker.aircraft()`, and `ctx` is
/// `tracker.context(now_ms, utc_ms)`, so the tracks agree with `own`. Every
/// track is written exactly as [`track_line`] writes it, every link as
/// [`link_line`], every aircraft as [`aircraft_line`]; the links come first,
/// as `"open_links":[...]`, then `"tracks":[...]`, then `"aircraft":[...]`.
///
/// Returns [`FormatError::BufferTooSmall`] (and nothing usable) if `buf` is
/// too small; [`state_max_len`] gives a size that always fits.
pub fn state(
    tracks: &[Track],
    links: &[LinkTrack],
    aircraft: &[AircraftTrack],
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
        adsb: hub.adsb,
    };
    // `{...}` -> `{...,"open_links":[` : drop the closing brace and carry on.
    let mut n = serde_json_core::to_slice(&head, buf).map_err(|_| FormatError::BufferTooSmall)? - 1;
    put(buf, &mut n, b",\"open_links\":[")?;
    for (i, l) in links.iter().enumerate() {
        if i > 0 {
            put(buf, &mut n, b",")?;
        }
        let rest = buf.get_mut(n..).ok_or(FormatError::BufferTooSmall)?;
        // `link_line` ends with '\n'; drop it inside the array.
        n += link_line(l, ctx, rest)? - 1;
    }
    put(buf, &mut n, STATE_LINKS_TO_TRACKS)?;
    for (i, t) in tracks.iter().enumerate() {
        if i > 0 {
            put(buf, &mut n, b",")?;
        }
        let rest = buf.get_mut(n..).ok_or(FormatError::BufferTooSmall)?;
        // `track_line` ends with '\n'; drop it inside the array.
        n += track_line(t, ctx, rest)? - 1;
    }
    put(buf, &mut n, STATE_TRACKS_TO_AIRCRAFT)?;
    for (i, a) in aircraft.iter().enumerate() {
        if i > 0 {
            put(buf, &mut n, b",")?;
        }
        let rest = buf.get_mut(n..).ok_or(FormatError::BufferTooSmall)?;
        // `aircraft_line` ends with '\n'; drop it inside the array.
        n += aircraft_line(a, tracks, ctx, rest)? - 1;
    }
    put(buf, &mut n, STATE_END)?;
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
