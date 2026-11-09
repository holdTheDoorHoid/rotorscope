//! rid-view: show live Remote ID drones from one or more radio nodes.
//!
//! Reads the nodes' USB JSON lines (serial ports, recorded files or stdin)
//! and runs them through the same `tracker` crate the hub uses, so what the
//! laptop shows is what the hub would show. See `--help`.

mod demo;
mod http;
mod input;
mod parse;
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
    GpsState, GpsStatus, HubStatus, ManualPos, NodeStatus, OwnPosRequest, OwnSource,
};
use tracker::{Context, Observation, TrackId, TrackState, Tracker, cot, csv, json};
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
    --demo                   Add two simulated drones circling --own-pos
                             (or Philadelphia City Hall). No hardware needed.
    --once                   Read inputs to the end, print the table once and
                             exit (for files; with --demo, simulates
                             --demo-secs seconds instantly).
    --demo-secs N            Simulated seconds for --demo --once (default 10).
    --json                   Print the hub's JSON track lines instead of the
                             table (once a second, or at the end with --once).
    --realtime               Play recorded files back at their captured speed.
    --http ADDR              Serve the hub's phone page and API on ADDR, e.g.
                             127.0.0.1:8080 (port 0 picks a free one). The
                             page can set --own-pos (POST /api/own-pos).
    -h, --help               This text.
";

struct Options {
    inputs: Vec<String>,
    own: Option<(f64, f64, Option<f64>)>,
    log: Option<String>,
    cot: Option<String>,
    demo: bool,
    once: bool,
    demo_secs: u64,
    json: bool,
    realtime: bool,
    http: Option<String>,
}

fn parse_args() -> Result<Options, String> {
    let mut o = Options {
        inputs: Vec::new(),
        own: None,
        log: None,
        cot: None,
        demo: false,
        once: false,
        demo_secs: 10,
        json: false,
        realtime: false,
        http: None,
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
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            _ => o.inputs.push(a),
        }
    }
    if o.inputs.is_empty() && !o.demo {
        o.inputs.push("-".into());
    }
    if o.http.is_some() && o.once {
        return Err("--http serves a live page; it cannot be used with --once".into());
    }
    if o.inputs.len() > 200 {
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
        _ => None,
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
            Some(NodeStatus {
                role: n.role?,
                alive: n.ended.is_none() && age.is_some_and(|a| a <= NODE_ALIVE_MS),
                age_ms: age,
                seen_per_s: n.seen_per_s,
                rid_per_s: ((n.rid_times.len() as u64 * 1000 + RATE_WINDOW_MS / 2) / RATE_WINDOW_MS)
                    as u32,
                drops: n.drops,
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
/// CSV rows for drones heard since the last row, CoT, JSON lines.
struct Outputs {
    log: Option<File>,
    logged: HashMap<TrackId, u64>,
    cot: Option<Cot>,
    cot_error_shown: bool,
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
            let _ = f.flush();
            self.logged.retain(|id, _| tr.get(*id).is_some());
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
        }
        if json_out {
            let mut out = std::io::stdout().lock();
            for t in tr.tracks() {
                if let Ok(n) = json::track_line(t, ctx, &mut buf) {
                    let _ = out.write_all(&buf[..n]);
                }
            }
            let _ = out.flush();
        }
    }
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
    if o.demo {
        let base = o.inputs.len() as u8;
        let center = o.own.map_or((39.952_583, -75.165_222), |(a, b, _)| (a, b));
        demo::spawn(center, !o.once, o.demo_secs, start, base, tx.clone());
        for (n, what, role) in [
            (base, "demo Wi-Fi", NodeRole::Wifi24),
            (base + 1, "demo BLE", NodeRole::Ble),
        ] {
            let info = view.nodes.entry(n).or_default();
            info.source = what.into();
            info.role = Some(role);
        }
        live_sources += 1;
    }
    drop(tx);

    let mut outputs = Outputs {
        log: None,
        logged: HashMap::new(),
        cot: o.cot.as_deref().map(Cot::open).transpose()?,
        cot_error_shown: false,
    };
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

    let mut tr: Tracker = Tracker::default();
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
                Event::End { node, why } => {
                    live_sources -= 1;
                    if let Some(info) = view.nodes.get_mut(&node) {
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
            };
            let mut buf = vec![0u8; json::state_max_len(tr.len(), nodes.len())];
            if let Ok(n) = json::state(tr.tracks(), &ctx, &hub, &mut buf) {
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
