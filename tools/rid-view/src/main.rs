//! rid-view: show live Remote ID drones from one or more radio nodes.
//!
//! Reads the nodes' USB JSON lines (serial ports, recorded files or stdin)
//! and runs them through the same `tracker` crate the hub uses, so what the
//! laptop shows is what the hub would show. See `--help`.

mod demo;
mod input;
mod parse;
mod view;

use std::collections::HashMap;
use std::fs::File;
use std::io::{IsTerminal, Write};
use std::net::{ToSocketAddrs, UdpSocket};
use std::process::ExitCode;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use input::{Clock, Event};
use parse::Line;
use tracker::{Context, Observation, TrackId, TrackState, Tracker, cot, csv, json};
use view::View;

const USAGE: &str = "\
rid-view: live table of Remote ID drones from drone-detector nodes

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
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            _ => o.inputs.push(a),
        }
    }
    if o.inputs.is_empty() && !o.demo {
        o.inputs.push("-".into());
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
        for (n, what) in [(base, "demo Wi-Fi"), (base + 1, "demo BLE")] {
            view.nodes.entry(n).or_default().source = what.into();
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
                    match line {
                        Line::Rid(r) => {
                            info.rid_lines += 1;
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
                        Line::Hello(v) => info.hello = Some(summarize(&v, &["t"])),
                        Line::Stats(v) => {
                            info.stats = Some(summarize(&v, &["t", "up_us", "win_ms"]))
                        }
                        Line::Other => {}
                        Line::Bad => info.bad_lines += 1,
                    }
                }
                Event::Detection { node, det, at_ms } => {
                    now = now.max(at_ms);
                    view.nodes.entry(node).or_default().rid_lines += 1;
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
        if let Some((lat, lon, alt)) = o.own {
            tr.set_own_position(lat, lon, alt, now);
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
