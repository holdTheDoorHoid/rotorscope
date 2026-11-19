//! rid-view: show live Remote ID drones from one or more radio nodes.
//!
//! Reads the nodes' USB JSON lines (serial ports, recorded files or stdin)
//! and runs them through the same `tracker` crate the hub uses, so what the
//! laptop shows is what the hub would show. See `--help`. Open FPV links
//! the Wi-Fi node recognises (wfb-ng, OpenHD, RubyFPV, DroneBridge) are
//! listed below the drones, with their identifiers hidden unless
//! `--show-link-ids` is given (the hub's policy too). DJI DroneID records
//! (OcuSync bursts and the legacy Wi-Fi element) join the drone table,
//! merged with Remote ID by serial; raw-capture RF emitters and Wi-Fi drones
//! get their own tables (node lines `{"t":"frame",...}`, see `parse`).
//! Crewed aircraft from an ADSBee (`--adsbee`) are listed last, apart from
//! drones, and never alert.

mod ble;
#[cfg(all(feature = "ble", target_os = "linux"))]
mod bluez;
mod demo;
mod dragonsync;
mod http;
mod input;
mod parse;
#[cfg(target_os = "linux")]
mod sniffle;
mod view;

use std::collections::HashMap;
use std::fs::File;
use std::io::{IsTerminal, Write};
use std::net::{TcpListener, ToSocketAddrs, UdpSocket};
use std::process::ExitCode;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use input::{Clock, Event};
use link::NodeRole;
use parse::Line;
use rid::Transport;
use tracker::json::{
    AdsbStatus, GpsState, GpsStatus, HubStatus, ManualPos, NodeStatus, OwnPosRequest, OwnSource,
};
use tracker::{
    AircraftKey, Config, Context, LinkTrackId, Observation, TrackId, TrackState, Tracker, cot, csv,
    json,
};
use view::View;

const USAGE: &str = "\
rid-view: live table of Remote ID drones from Rotorscope nodes

USAGE:
    rid-view [OPTIONS] [INPUT...]

INPUT is a node's USB serial port (e.g. /dev/ttyACM0), a recorded file of
its JSON lines, or - for stdin (the default when no input and no --demo).
Each input is one node; give several to merge them.

OPTIONS:
    --own-pos LAT,LON[,ALT]  Your position, for distance and bearing. ALT is
                             metres above the WGS-84 ellipsoid (GPS height).
    --log FILE.csv           Append a CSV row per drone per second (same
                             columns as the hub's SD-card log).
    --cot udp://HOST:PORT    Send TAK Cursor-on-Target for each drone and
                             operator once a second, e.g. udp://239.2.3.1:6969
                             (ATAK's default multicast). Off by default.
    --dragonsync BIND        Publish drones to DragonSync (WarDragon's TAK/MQTT
                             bridge) as ZeroMQ JSON, e.g. tcp://127.0.0.1:4230.
                             Point DragonSync's zmq_host/zmq_port at it. Off
                             by default; no default port, so it never lands
                             on a WarDragon's own 4224.
    --dragonsync-signals BIND
                             Publish signal alerts (analog FPV video, DJI
                             links, open FPV links) on a second port, e.g.
                             tcp://127.0.0.1:4231. Point DragonSync's
                             fpv_zmq_host/fpv_zmq_port at it and set
                             fpv_enabled = true. Must differ from
                             --dragonsync's. Link identifiers stay hidden
                             without --show-link-ids.
    --adsbee PORT            Read crewed aircraft from an ADSBee 1090: its
                             USB console (after AT+PROTOCOL_OUT=CONSOLE,CSBEE),
                             a USB-UART on its COMMS_UART pins, or a recorded
                             file of its CSBee text. Listed apart from drones;
                             never an alert.
    --ble ADAPTER            Scan for Bluetooth Remote ID with a laptop
                             adapter through BlueZ (e.g. hci1; no root
                             needed). Repeat for several adapters. A BT4
                             adapter hears legacy adverts only. Combines
                             with node inputs, --adsbee and --demo.
    --sniffle PORT           Scan for Bluetooth 5 Remote ID with a TI
                             CC26x2 dongle running Sniffle firmware (e.g. a
                             Sonoff Zigbee 3.0 USB Dongle Plus flashed with
                             tools/cc26xx-flash; see docs/SONOFF.md). Hears
                             legacy, extended and Long Range adverts, with
                             RSSI. Passive: the dongle never transmits.
    --sniffle-mode MODE      What the Sniffle dongle listens for on primary
                             channel 37: 1m (legacy + extended, default),
                             coded (Long Range only), or alt (switch
                             between the two every 3 s; hears each half
                             the time).
    --duration SECS          Stop live inputs (--ble, --sniffle) after SECS
                             seconds. With --once, the default is 10.
    --demo                   Add two simulated drones circling --own-pos
                             (or Philadelphia City Hall), a simulated
                             wfb-ng FPV link, three simulated aircraft
                             (ADS-B), and one of each protocol-4 signal: a
                             DJI drone on OcuSync DroneID (also in Remote
                             ID), an encrypted O4 drone, a DJI Wi-Fi
                             DroneID beacon, analog video and HDZero
                             emitters, plain RF energy, a Wi-Fi drone and
                             node Status frames. No hardware needed.
    --show-link-ids          Show open FPV links' identifiers (wfb-ng link
                             ID, RubyFPV vehicle ID, DroneBridge comm ID) and
                             their transmitter addresses, and Wi-Fi drones'
                             addresses. Off by default: they identify
                             someone's equipment.
    --hide-droneid-ids       Keep identity decoded from DJI DroneID (serial,
                             O4 session key ID, flight purpose) out of every
                             output. Positions are still shown, and a serial
                             also sent in Remote ID still is.
    --once                   Read inputs to the end, print the table once and
                             exit (for files; with --demo, simulates
                             --demo-secs seconds instantly).
    --demo-secs N            Simulated seconds for --demo --once (default 10).
    --json                   Print the hub's JSON track lines instead of the
                             table (once a second, or at the end with --once).
    --realtime               Play recorded files back at their captured speed.
                             Without it a recording is read at once but
                             timed by its own up_us, so drones are
                             confirmed and age as in the original run.
    --http ADDR              Serve the hub's phone page and API on ADDR, e.g.
                             127.0.0.1:8080 (port 0 picks a free one). The
                             page can set --own-pos (POST /api/own-pos).
                             Keep it on loopback: the page shows where
                             drones are and takes a position, and other
                             addresses print a warning.
    -h, --help               This text.
";

struct Options {
    inputs: Vec<String>,
    own: Option<(f64, f64, Option<f64>)>,
    log: Option<String>,
    cot: Option<String>,
    dragonsync: Option<String>,
    dragonsync_signals: Option<String>,
    demo: bool,
    once: bool,
    demo_secs: u64,
    json: bool,
    realtime: bool,
    http: Option<String>,
    show_link_ids: bool,
    hide_droneid_ids: bool,
    adsbee: Option<String>,
    ble: Vec<String>,
    sniffle: Vec<String>,
    sniffle_mode: String,
    duration: Option<u64>,
}

fn parse_args() -> Result<Options, String> {
    let mut o = Options {
        inputs: Vec::new(),
        own: None,
        log: None,
        cot: None,
        dragonsync: None,
        dragonsync_signals: None,
        demo: false,
        once: false,
        demo_secs: 10,
        json: false,
        realtime: false,
        http: None,
        show_link_ids: false,
        hide_droneid_ids: false,
        adsbee: None,
        ble: Vec::new(),
        sniffle: Vec::new(),
        sniffle_mode: "1m".into(),
        duration: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match a.as_str() {
            "-h" | "--help" => return Err(String::new()),
            "--own-pos" => {
                let v = value("--own-pos")?;
                let parts: Vec<f64> = v
                    .split(',')
                    .map(|p| p.trim().parse::<f64>())
                    .collect::<Result<_, _>>()
                    .map_err(|_| format!("--own-pos: cannot read {v:?}"))?;
                match parts[..] {
                    [lat, lon] => o.own = Some((lat, lon, None)),
                    [lat, lon, alt] => o.own = Some((lat, lon, Some(alt))),
                    _ => return Err("--own-pos wants LAT,LON or LAT,LON,ALT".into()),
                }
                let (lat, lon, _) = o.own.unwrap();
                if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
                    return Err("--own-pos out of range".into());
                }
            }
            "--log" => o.log = Some(value("--log")?),
            "--cot" => o.cot = Some(value("--cot")?),
            "--dragonsync" => o.dragonsync = Some(value("--dragonsync")?),
            "--dragonsync-signals" => o.dragonsync_signals = Some(value("--dragonsync-signals")?),
            "--demo" => o.demo = true,
            "--once" => o.once = true,
            "--demo-secs" => {
                o.demo_secs = value("--demo-secs")?
                    .parse()
                    .map_err(|_| "--demo-secs: not a number")?
            }
            "--json" => o.json = true,
            "--realtime" => o.realtime = true,
            "--http" => o.http = Some(value("--http")?),
            "--show-link-ids" => o.show_link_ids = true,
            "--hide-droneid-ids" => o.hide_droneid_ids = true,
            "--adsbee" => o.adsbee = Some(value("--adsbee")?),
            "--ble" => o.ble.push(value("--ble")?),
            "--sniffle" => o.sniffle.push(value("--sniffle")?),
            "--sniffle-mode" => o.sniffle_mode = value("--sniffle-mode")?,
            "--duration" => {
                o.duration = Some(
                    value("--duration")?
                        .parse()
                        .map_err(|_| "--duration: not a number")?,
                )
            }
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            _ => o.inputs.push(a),
        }
    }
    if o.inputs.is_empty()
        && !o.demo
        && o.adsbee.is_none()
        && o.ble.is_empty()
        && o.sniffle.is_empty()
    {
        o.inputs.push("-".into());
    }
    if o.http.is_some() && o.once {
        return Err("--http serves a live page; it cannot be used with --once".into());
    }
    if o.once && !(o.ble.is_empty() && o.sniffle.is_empty()) && o.duration.is_none() {
        o.duration = Some(10);
    }
    if (o.dragonsync.is_some() || o.dragonsync_signals.is_some()) && o.once {
        return Err("--dragonsync publishes live; it cannot be used with --once".into());
    }
    if o.dragonsync.is_some() && o.dragonsync == o.dragonsync_signals {
        return Err("--dragonsync and --dragonsync-signals need different ports".into());
    }
    if !["1m", "coded", "alt"].contains(&o.sniffle_mode.to_ascii_lowercase().as_str()) {
        return Err(format!(
            "--sniffle-mode: {:?} is not 1m, coded or alt",
            o.sniffle_mode
        ));
    }
    if o.inputs.len() + o.ble.len() + o.sniffle.len() > 200 {
        return Err("too many inputs".into());
    }
    Ok(o)
}

/// Wall-clock UTC in Unix ms.
fn utc_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn hhmmss(utc_ms: u64) -> String {
    let s = utc_ms / 1000;
    format!("{:02}:{:02}:{:02}Z", s / 3600 % 24, s / 60 % 60, s % 60)
}

/// A compact `k=v` summary of a hello/stats line's scalar fields, whatever
/// they are, so new firmware fields show up without a viewer change.
fn summarize(v: &serde_json::Value, skip: &[&str]) -> String {
    let Some(o) = v.as_object() else {
        return String::new();
    };
    o.iter()
        .filter(|(k, _)| !skip.contains(&k.as_str()))
        .filter_map(|(k, v)| match v {
            serde_json::Value::String(s) => Some(format!("{k}={s}")),
            serde_json::Value::Number(n) => Some(format!("{k}={n}")),
            serde_json::Value::Bool(b) => Some(format!("{k}={b}")),
            _ => None,
        })
        .take(8)
        .collect::<Vec<_>>()
        .join(" ")
}

// ------------------------------------------------- node health (--http) --

/// A node's role from its hello line: the Wi-Fi node sends `"role":"Wifi24"`,
/// the BLE node `"node":"ble"`.
fn role_from_hello(v: &serde_json::Value) -> Option<NodeRole> {
    let s = v.get("role").or_else(|| v.get("node"))?.as_str()?;
    match s.to_ascii_lowercase().as_str() {
        "wifi24" => Some(NodeRole::Wifi24),
        "wifi5" => Some(NodeRole::Wifi5),
        "ble" => Some(NodeRole::Ble),
        "fpv" => Some(NodeRole::Fpv),
        "droneid" => Some(NodeRole::DroneId),
        "gateway" => Some(NodeRole::Gateway),
        _ => None,
    }
}

/// Without a hello line, guess a Wi-Fi node's role from its channel.
fn role_from_channel(channel: u8) -> NodeRole {
    if channel > 14 {
        NodeRole::Wifi5
    } else {
        NodeRole::Wifi24
    }
}

/// Without a hello line, guess the role from what the input hears.
fn role_from_rid(t: Transport, channel: u8) -> NodeRole {
    match t {
        Transport::WifiBeacon | Transport::WifiNan if channel > 14 => NodeRole::Wifi5,
        Transport::WifiBeacon | Transport::WifiNan => NodeRole::Wifi24,
        _ => NodeRole::Ble,
    }
}

/// Frames seen per second and total drops from a stats line. The Wi-Fi node
/// sends `fps` and `drop`; the BLE node `adv` per `win_ms` and `tot_drops`.
fn stats_rates(v: &serde_json::Value) -> (Option<u32>, Option<u32>) {
    let u = |k: &str| v.get(k).and_then(|x| x.as_u64());
    let seen = u("fps").or_else(|| {
        let adv = u("adv")?;
        let win = u("win_ms").filter(|w| *w > 0).unwrap_or(1000);
        Some(adv * 1000 / win)
    });
    let drops = u("drop").or_else(|| u("tot_drops"));
    let clamp = |x: u64| x.min(u64::from(u32::MAX)) as u32;
    (seen.map(clamp), drops.map(clamp))
}

/// A node counts as alive if heard within this long.
const NODE_ALIVE_MS: u64 = 5_000;

/// The ADSBee's health for the phone page (the ADSBee reports once a second).
fn adsb_status(view: &View, now: u64) -> Option<AdsbStatus> {
    let a = view.adsb.as_ref()?;
    let age = a.last_ms.map(|l| now.saturating_sub(l));
    Some(AdsbStatus {
        alive: a.ended.is_none() && age.is_some_and(|x| x <= NODE_ALIVE_MS),
        age_ms: age,
        frames_per_s: a.stats.map(|s| s.valid_per_s()),
        aircraft: a.stats.map(|s| s.aircraft),
        errors: a.errors.min(u64::from(u32::MAX)) as u32,
    })
}
/// Keep what a frame says about its node: role and plan from Hello and
/// Status. (The tracker keeps them too; the view needs them per input.)
fn note_frame(info: &mut view::NodeInfo, f: &link::Frame) {
    match f {
        link::Frame::Hello { role, .. } => info.role = Some(*role),
        link::Frame::Status { role, can, plan } => {
            info.role = Some(*role);
            info.status = Some((*can, *plan));
        }
        link::Frame::Stats { dropped, .. } => info.drops = Some(*dropped),
        _ => {}
    }
}

/// Window for the Remote ID rate.
const RATE_WINDOW_MS: u64 = 5_000;

fn note_rid(info: &mut view::NodeInfo, at_ms: u64) {
    info.last_ms = Some(at_ms);
    info.rid_times.push_back(at_ms);
    while info.rid_times.len() > 10_000 {
        info.rid_times.pop_front();
    }
}

fn node_statuses(view: &mut View, now: u64) -> Vec<NodeStatus> {
    view.nodes
        .values_mut()
        .filter_map(|n| {
            while n
                .rid_times
                .front()
                .is_some_and(|t| now.saturating_sub(*t) > RATE_WINDOW_MS)
            {
                n.rid_times.pop_front();
            }
            let age = n.last_ms.map(|l| now.saturating_sub(l));
            let status = n.status;
            Some(NodeStatus {
                role: n.role?,
                alive: n.ended.is_none() && age.is_some_and(|a| a <= NODE_ALIVE_MS),
                age_ms: age,
                seen_per_s: n.seen_per_s,
                rid_per_s: ((n.rid_times.len() as u64 * 1000 + RATE_WINDOW_MS / 2) / RATE_WINDOW_MS)
                    as u32,
                drops: n.drops,
                can: status.map(|s| s.0),
                plan: status.map(|s| s.1),
            })
        })
        .collect()
}

struct Cot {
    socket: UdpSocket,
    target: std::net::SocketAddr,
}

impl Cot {
    fn open(url: &str) -> Result<Self, String> {
        let hostport = url
            .strip_prefix("udp://")
            .ok_or("--cot: only udp://HOST:PORT is supported")?;
        let target = hostport
            .to_socket_addrs()
            .map_err(|e| format!("--cot {hostport}: {e}"))?
            .next()
            .ok_or("--cot: no address")?;
        let socket = UdpSocket::bind(if target.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        })
        .map_err(|e| format!("--cot bind: {e}"))?;
        // Keep multicast on the local network, where the ATAK devices are.
        let _ = socket.set_multicast_ttl_v4(1);
        Ok(Self { socket, target })
    }
}

/// Everything that happens once a second: own position refresh, expiry,
/// CSV rows for drones and open links heard since the last row, CoT, JSON
/// lines.
struct Outputs {
    log: Option<File>,
    logged: HashMap<TrackId, u64>,
    logged_links: HashMap<LinkTrackId, u64>,
    logged_aircraft: HashMap<AircraftKey, u64>,
    logged_rf: HashMap<tracker::EmitterId, u64>,
    logged_wifi: HashMap<tracker::WifiDroneId, u64>,
    cot: Option<Cot>,
    cot_error_shown: bool,
    dragon: Option<dragonsync::DragonSync>,
}

impl Outputs {
    fn tick(&mut self, tr: &Tracker, ctx: &Context, view: &mut View, json_out: bool) {
        let mut buf = [0u8; 2048];
        if let Some(f) = &mut self.log {
            for t in tr.tracks() {
                if self.logged.get(&t.id).is_some_and(|&l| l >= t.last_seen_ms) {
                    continue;
                }
                if let Ok(n) = csv::row(t, ctx, &mut buf) {
                    let _ = f.write_all(&buf[..n]);
                }
                self.logged.insert(t.id, t.last_seen_ms);
            }
            for l in tr.open_links() {
                if self
                    .logged_links
                    .get(&l.id)
                    .is_some_and(|&t| t >= l.last_seen_ms)
                {
                    continue;
                }
                if let Ok(n) = csv::link_row(l, ctx, &mut buf) {
                    let _ = f.write_all(&buf[..n]);
                }
                self.logged_links.insert(l.id, l.last_seen_ms);
            }
            for a in tr.aircraft() {
                if self
                    .logged_aircraft
                    .get(&a.key)
                    .is_some_and(|&t| t >= a.last_seen_ms)
                {
                    continue;
                }
                if let Ok(n) = csv::aircraft_row(a, ctx, &mut buf) {
                    let _ = f.write_all(&buf[..n]);
                }
                self.logged_aircraft.insert(a.key, a.last_seen_ms);
            }
            for e in tr.emitters() {
                if self
                    .logged_rf
                    .get(&e.id)
                    .is_some_and(|&t| t >= e.last_seen_ms)
                {
                    continue;
                }
                if let Ok(n) = csv::rf_row(e, ctx, &mut buf) {
                    let _ = f.write_all(&buf[..n]);
                }
                self.logged_rf.insert(e.id, e.last_seen_ms);
            }
            for w in tr.wifi_drones() {
                if self
                    .logged_wifi
                    .get(&w.id)
                    .is_some_and(|&t| t >= w.last_seen_ms)
                {
                    continue;
                }
                if let Ok(n) = csv::wifi_drone_row(w, ctx, &mut buf) {
                    let _ = f.write_all(&buf[..n]);
                }
                self.logged_wifi.insert(w.id, w.last_seen_ms);
            }
            let _ = f.flush();
            self.logged_rf.retain(|id, _| tr.get_emitter(*id).is_some());
            self.logged_wifi
                .retain(|id, _| tr.get_wifi_drone(*id).is_some());
            self.logged_aircraft
                .retain(|k, _| tr.get_aircraft(*k).is_some());
            self.logged.retain(|id, _| tr.get(*id).is_some());
            self.logged_links
                .retain(|id, _| tr.get_open_link(*id).is_some());
        }
        if let Some(c) = &self.cot {
            for t in tr
                .tracks()
                .iter()
                .filter(|t| ctx.state(t) == TrackState::Active)
            {
                for event in [cot::drone_event, cot::operator_event] {
                    if let Ok(n) = event(t, ctx, &mut buf)
                        && let Err(e) = c.socket.send_to(&buf[..n], c.target)
                        && !self.cot_error_shown
                    {
                        self.cot_error_shown = true;
                        view.note(&hhmmss(utc_now()), format!("CoT send failed: {e}"));
                    }
                }
            }
            // Aircraft too (an ADS-B "UAV" that is a Remote ID drone is
            // skipped: its drone event already shows it).
            for a in tr
                .aircraft()
                .iter()
                .filter(|a| ctx.aircraft_state(a) == TrackState::Active)
            {
                if let Ok(n) = cot::aircraft_event(a, tr.tracks(), ctx, &mut buf)
                    && let Err(e) = c.socket.send_to(&buf[..n], c.target)
                    && !self.cot_error_shown
                {
                    self.cot_error_shown = true;
                    view.note(&hhmmss(utc_now()), format!("CoT send failed: {e}"));
                }
            }
        }
        if let Some(d) = &mut self.dragon {
            d.tick(tr, ctx);
        }
        if json_out {
            let mut out = std::io::stdout().lock();
            for t in tr.tracks() {
                if let Ok(n) = json::track_line(t, ctx, &mut buf) {
                    let _ = out.write_all(&buf[..n]);
                }
            }
            for l in tr.open_links() {
                if let Ok(n) = json::link_line(l, ctx, &mut buf) {
                    let _ = out.write_all(&buf[..n]);
                }
            }
            for e in tr.emitters() {
                if let Ok(n) = json::rf_line(e, ctx, &mut buf) {
                    let _ = out.write_all(&buf[..n]);
                }
            }
            for w in tr.wifi_drones() {
                if let Ok(n) = json::wifi_drone_line(w, ctx, &mut buf) {
                    let _ = out.write_all(&buf[..n]);
                }
            }
            for a in tr.aircraft() {
                if let Ok(n) = json::aircraft_line(a, tr.tracks(), ctx, &mut buf) {
                    let _ = out.write_all(&buf[..n]);
                }
            }
            let _ = out.flush();
        }
    }
}

#[cfg(all(feature = "ble", target_os = "linux"))]
fn spawn_ble(
    node: u8,
    adapter: &str,
    secs: Option<u64>,
    start: Instant,
    tx: mpsc::Sender<Event>,
) -> Result<String, String> {
    bluez::spawn_scan(node, adapter, secs.map(Duration::from_secs), start, tx)
        .map_err(|e| format!("--ble {adapter}: {e}"))
}

#[cfg(not(all(feature = "ble", target_os = "linux")))]
fn spawn_ble(
    _node: u8,
    adapter: &str,
    _secs: Option<u64>,
    _start: Instant,
    _tx: mpsc::Sender<Event>,
) -> Result<String, String> {
    Err(format!(
        "--ble {adapter}: this build has no Bluetooth support (needs Linux and the `ble` feature)"
    ))
}

#[cfg(target_os = "linux")]
fn spawn_sniffle(
    node: u8,
    path: &str,
    mode: &str,
    secs: Option<u64>,
    start: Instant,
    tx: mpsc::Sender<Event>,
) -> Result<String, String> {
    let mode = sniffle::Primary::parse(mode).ok_or("bad --sniffle-mode")?;
    sniffle::spawn(node, path, mode, secs.map(Duration::from_secs), start, tx)
        .map_err(|e| format!("--sniffle {path}: {e}"))
}

#[cfg(not(target_os = "linux"))]
fn spawn_sniffle(
    _node: u8,
    path: &str,
    _mode: &str,
    _secs: Option<u64>,
    _start: Instant,
    _tx: mpsc::Sender<Event>,
) -> Result<String, String> {
    Err(format!("--sniffle {path}: only built on Linux"))
}

fn run(o: Options) -> Result<(), String> {
    let start = Instant::now();
    let clock = Clock(start);
    let (tx, rx) = mpsc::channel();
    let mut view = View::new(!o.once && !o.json && std::io::stdout().is_terminal());

    for (i, path) in o.inputs.iter().enumerate() {
        let node = i as u8;
        input::spawn_reader(node, path.clone(), o.realtime, start, tx.clone())
            .map_err(|e| format!("{path}: {e}"))?;
        view.nodes.entry(node).or_default().source = if path == "-" {
            "stdin".into()
        } else {
            path.clone()
        };
    }
    let mut live_sources = o.inputs.len();
    for (i, adapter) in o.ble.iter().enumerate() {
        let node = (o.inputs.len() + i) as u8;
        let source = spawn_ble(node, adapter, o.duration, start, tx.clone())?;
        let info = view.nodes.entry(node).or_default();
        info.source = source;
        info.role = Some(NodeRole::Ble);
        live_sources += 1;
    }
    for (i, path) in o.sniffle.iter().enumerate() {
        let node = (o.inputs.len() + o.ble.len() + i) as u8;
        let source = spawn_sniffle(node, path, &o.sniffle_mode, o.duration, start, tx.clone())?;
        let info = view.nodes.entry(node).or_default();
        info.source = source;
        info.role = Some(NodeRole::Ble);
        live_sources += 1;
    }
    if let Some(path) = &o.adsbee {
        input::spawn_adsbee(path.clone(), o.realtime, start, tx.clone())
            .map_err(|e| format!("--adsbee {path}: {e}"))?;
        view.adsb = Some(view::AdsbInfo {
            source: path.clone(),
            ..Default::default()
        });
        live_sources += 1;
    }
    if o.demo {
        let base = (o.inputs.len() + o.ble.len() + o.sniffle.len()) as u8;
        let center = o.own.map_or((39.952_583, -75.165_222), |(a, b, _)| (a, b));
        demo::spawn(center, !o.once, o.demo_secs, start, base, tx.clone());
        for (n, what, role) in [
            (base, "demo Wi-Fi", NodeRole::Wifi24),
            (base + 1, "demo BLE", NodeRole::Ble),
            (base + 2, "demo Wi-Fi 5 GHz", NodeRole::Wifi5),
            (base + 3, "demo DroneID", NodeRole::DroneId),
            (base + 4, "demo FPV scanner", NodeRole::Fpv),
        ] {
            let info = view.nodes.entry(n).or_default();
            info.source = what.into();
            info.role = Some(role);
        }
        view.adsb.get_or_insert_with(|| view::AdsbInfo {
            source: "demo".into(),
            ..Default::default()
        });
        live_sources += 1;
    }
    drop(tx);

    let mut outputs = Outputs {
        log: None,
        logged: HashMap::new(),
        logged_links: HashMap::new(),
        logged_aircraft: HashMap::new(),
        logged_rf: HashMap::new(),
        logged_wifi: HashMap::new(),
        cot: o.cot.as_deref().map(Cot::open).transpose()?,
        cot_error_shown: false,
        dragon: None,
    };
    if o.dragonsync.is_some() || o.dragonsync_signals.is_some() {
        let bind = |what: &str, ep: &Option<String>| {
            ep.as_deref()
                .map(|e| dragonsync::Bus::bind(what, e))
                .transpose()
        };
        let drones = bind("--dragonsync", &o.dragonsync)?;
        let signals = bind("--dragonsync-signals", &o.dragonsync_signals)?;
        for (what, b) in [("drones", &drones), ("signal alerts", &signals)] {
            if let Some(b) = b {
                eprintln!("rid-view: DragonSync {what} published on {}", b.endpoint);
            }
        }
        outputs.dragon = Some(dragonsync::DragonSync::new(drones, signals));
    }
    if let Some(path) = &o.log {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| format!("{path}: {e}"))?;
        if f.metadata().map(|m| m.len() == 0).unwrap_or(true) {
            f.write_all(csv::HEADER.as_bytes())
                .map_err(|e| format!("{path}: {e}"))?;
        }
        outputs.log = Some(f);
    }

    // Own position: from --own-pos, changed by the page (--http). There is
    // no GPS on a laptop, so it is always a "manual" position.
    let mut manual: Option<ManualPos> = o.own.map(|(lat, lon, alt)| ManualPos { lat, lon, alt });
    let (pos_tx, pos_rx) = mpsc::channel::<OwnPosRequest>();
    let http = match &o.http {
        Some(addr) => {
            let listener = TcpListener::bind(addr).map_err(|e| format!("--http {addr}: {e}"))?;
            let at = listener.local_addr().map_err(|e| e.to_string())?;
            eprintln!("rid-view: phone page at http://{at}/");
            if let Some(w) = http::exposure_warning(at) {
                eprintln!("{w}");
            }
            let shared = Arc::new(http::Shared {
                state: Mutex::new(Vec::new()),
                own_pos: Mutex::new(pos_tx),
            });
            http::spawn(listener, Arc::clone(&shared));
            Some(shared)
        }
        None => None,
    };
    let mut last_publish: Option<u64> = None;

    let mut tr: Tracker = Tracker::new(Config {
        show_link_ids: o.show_link_ids,
        hide_droneid_ids: o.hide_droneid_ids,
        ..Config::default()
    });
    // Tracker time: the newest event stamp; while live it also advances with
    // the wall clock so tracks age and go stale when nothing arrives.
    let mut now = 0u64;
    let mut last_tick = 0u64;
    let mut last_draw = Instant::now() - Duration::from_secs(1);
    if view.ansi {
        print!("\x1b[2J");
    }

    loop {
        let ev = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ev) => Some(ev),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if let Some(ev) = ev {
            match ev {
                Event::Line { node, line, at_ms } => {
                    now = now.max(at_ms);
                    let info = view.nodes.entry(node).or_default();
                    info.last_ms = Some(at_ms);
                    match line {
                        Line::Rid(r) => {
                            info.rid_lines += 1;
                            note_rid(info, at_ms);
                            if info.role.is_none() {
                                info.role = Some(role_from_rid(r.transport, r.channel));
                            }
                            let obs = Observation {
                                node,
                                transport: r.transport,
                                channel: r.channel,
                                rssi: r.rssi,
                                mac: r.mac,
                                counter: r.counter,
                                rx_ms: now,
                            };
                            tr.ingest_messages(&obs, &r.msgs, r.errors);
                        }
                        Line::Open(s) => {
                            info.open_lines += 1;
                            if info.role.is_none() {
                                info.role = Some(role_from_channel(s.channel));
                            }
                            tr.ingest_open_link(node, &s, now);
                        }
                        Line::Frame(f) => {
                            info.frame_lines += 1;
                            note_frame(info, &f);
                            if matches!(*f, link::Frame::Detection(_)) {
                                note_rid(info, at_ms);
                            }
                            tr.ingest_frame(node, &f, now);
                        }
                        Line::Hello(v) => {
                            info.role = role_from_hello(&v).or(info.role);
                            info.hello = Some(summarize(&v, &["t"]));
                        }
                        Line::Stats(v) => {
                            (info.seen_per_s, info.drops) = stats_rates(&v);
                            info.stats = Some(summarize(&v, &["t", "up_us", "win_ms"]));
                        }
                        Line::Other => {}
                        Line::Bad => info.bad_lines += 1,
                    }
                }
                Event::Detection { node, det, at_ms } => {
                    now = now.max(at_ms);
                    let info = view.nodes.entry(node).or_default();
                    info.rid_lines += 1;
                    note_rid(info, at_ms);
                    tr.ingest(node, &det, now);
                }
                Event::OpenLink {
                    node,
                    sighting,
                    at_ms,
                } => {
                    now = now.max(at_ms);
                    let info = view.nodes.entry(node).or_default();
                    info.last_ms = Some(at_ms);
                    info.open_lines += 1;
                    tr.ingest_open_link(node, &sighting, now);
                }
                Event::Frame { node, frame, at_ms } => {
                    now = now.max(at_ms);
                    let info = view.nodes.entry(node).or_default();
                    info.last_ms = Some(at_ms);
                    info.frame_lines += 1;
                    note_frame(info, &frame);
                    tr.ingest_frame(node, &frame, now);
                }
                Event::End { node, why } => {
                    live_sources -= 1;
                    if let Some(info) = view.nodes.get_mut(&node) {
                        info.ended = Some(why);
                    }
                }
                Event::Adsb { result, at_ms } => {
                    now = now.max(at_ms);
                    let info = view.adsb.get_or_insert_with(Default::default);
                    match result {
                        Ok(adsb::Report::Aircraft(a)) => {
                            info.last_ms = Some(at_ms);
                            info.aircraft_lines += 1;
                            tr.ingest_aircraft(&a, now);
                        }
                        Ok(adsb::Report::Stats(s)) => {
                            info.last_ms = Some(at_ms);
                            info.stats = Some(s);
                        }
                        Err(adsb::Error::NotCsbee) => info.other_lines += 1,
                        Err(_) => info.errors += 1,
                    }
                }
                Event::AdsbEnd { why } => {
                    live_sources -= 1;
                    if let Some(info) = &mut view.adsb {
                        info.ended = Some(why);
                    }
                }
            }
        }
        if !o.once {
            now = now.max(clock.ms());
        }
        let mut pos_changed = false;
        while let Ok(req) = pos_rx.try_recv() {
            manual = match req {
                OwnPosRequest::Set(p) => Some(p),
                OwnPosRequest::Clear => None,
            };
            pos_changed = true;
            let what = manual.map_or("cleared".to_string(), |p| {
                format!("{:.5},{:.5}", p.lat, p.lon)
            });
            view.note(
                &hhmmss(utc_now()),
                format!("own position from page: {what}"),
            );
        }
        match manual {
            Some(p) => tr.set_own_position(p.lat, p.lon, p.alt, now),
            None => tr.clear_own_position(),
        }
        while let Some(id) = tr.take_new_drone(now) {
            view.announced.insert(id, now);
            let name = tr
                .get(id)
                .map(|t| tracker::cot::uid_base(t).to_string())
                .unwrap_or_default();
            view.note(&hhmmss(utc_now()), format!("new drone: {name}"));
            if view.ansi {
                print!("\x07"); // terminal bell: the laptop's buzzer (O6)
            }
        }
        view.announced.retain(|id, _| tr.get(*id).is_some());
        while let Some(k) = tr.take_new_signal(now) {
            let what = match k {
                tracker::SignalKey::Emitter(id) => tr.get_emitter(id).map(|e| {
                    format!(
                        "new RF signal: {} {:.1} MHz",
                        tracker::signals::rf_label(e.kind),
                        e.freq_mhz()
                    )
                }),
                tracker::SignalKey::WifiDrone(id) => tr
                    .get_wifi_drone(id)
                    .map(|w| format!("new Wi-Fi drone: {} {}", w.maker_name(), w.ssid)),
            };
            if let Some(w) = what {
                view.note(&hhmmss(utc_now()), w);
            }
        }

        if now >= last_tick + 1000 {
            last_tick = now;
            tr.expire(now);
            let ctx = tr.context(now, Some(utc_now()));
            outputs.tick(&tr, &ctx, &mut view, o.json && !o.once);
        }
        // The page polls about once a second; a fresh snapshot 4x a second
        // keeps it current without rendering per request.
        if let Some(h) = &http
            && (pos_changed || last_publish.is_none_or(|l| now >= l + 250))
        {
            last_publish = Some(now);
            let ctx = tr.context(now, Some(utc_now()));
            let nodes = node_statuses(&mut view, now);
            let hub = HubStatus {
                own_source: OwnSource::Manual,
                manual,
                gps: GpsStatus {
                    state: GpsState::Absent,
                    sats: None,
                },
                recording: o.log.is_some(),
                nodes: &nodes,
                adsb: adsb_status(&view, now),
            };
            let mut buf = vec![
                0u8;
                json::state_full_max_len(
                    tr.len(),
                    tr.open_links().len(),
                    tr.aircraft().len(),
                    nodes.len(),
                    tr.emitters().len(),
                    tr.wifi_drones().len(),
                )
            ];
            let signals = json::Signals {
                emitters: tr.emitters(),
                wifi_drones: tr.wifi_drones(),
                threats: tr.signal_threats(),
            };
            if let Ok(n) = json::state_full(
                tr.tracks(),
                tr.open_links(),
                tr.aircraft(),
                &signals,
                &ctx,
                &hub,
                &mut buf,
            ) {
                buf.truncate(n);
                if let Ok(mut s) = h.state.lock() {
                    *s = buf;
                }
            }
        }
        // Redraw in place 4x a second on a terminal; when piped, a fresh
        // table once a second is plenty.
        let every = Duration::from_millis(if view.ansi { 250 } else { 1000 });
        if !o.once && !o.json && last_draw.elapsed() >= every {
            last_draw = Instant::now();
            let ctx = tr.context(now, Some(utc_now()));
            let screen = view.render(&tr, &ctx, &hhmmss(utc_now()));
            let mut out = std::io::stdout().lock();
            if view.ansi {
                let _ = write!(out, "{}{screen}{}", view::HOME, view::CLEAR_TO_END);
            } else {
                let _ = writeln!(out, "{screen}");
            }
            let _ = out.flush();
        }
        if o.once && live_sources == 0 {
            break;
        }
    }

    // Final state: flush the log and print once.
    let ctx = tr.context(now, Some(utc_now()));
    outputs.tick(&tr, &ctx, &mut view, o.json);
    if !o.json {
        let screen = view.render(&tr, &ctx, &hhmmss(utc_now()));
        if view.ansi {
            print!("{}{screen}{}", view::HOME, view::CLEAR_TO_END);
        } else {
            print!("{screen}");
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    match parse_args() {
        Err(e) if e.is_empty() => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("rid-view: {e}\n\n{USAGE}");
            ExitCode::from(2)
        }
        Ok(o) => match run(o) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("rid-view: {e}");
                ExitCode::FAILURE
            }
        },
    }
}
