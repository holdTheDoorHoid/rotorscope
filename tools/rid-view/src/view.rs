//! The live table, as plain text with optional ANSI styling. Plain ANSI
//! rather than a TUI crate: the screen is one table that is redrawn in
//! place, which needs three escape codes, not a dependency tree.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt::Write;

use rid::Transport;
use rid::message::UaType;
use tracker::openlinks::shown_id;
use tracker::{Context, LinkTrack, Track, TrackId, TrackState, Tracker};

/// Cursor to top-left; the screen is then overwritten line by line.
pub const HOME: &str = "\x1b[H";
pub const CLEAR_TO_END: &str = "\x1b[J";
const CLEAR_LINE: &str = "\x1b[K";
const BOLD_YELLOW: &str = "\x1b[1;33m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

/// How long a new drone's row stays highlighted.
pub const NEW_HIGHLIGHT_MS: u64 = 10_000;

/// What we know about one input.
#[derive(Default)]
pub struct NodeInfo {
    pub source: String,
    pub hello: Option<String>,
    pub stats: Option<String>,
    pub ended: Option<String>,
    pub rid_lines: u64,
    /// Open FPV link sightings from this input.
    pub open_lines: u64,
    pub bad_lines: u64,
    /// For the phone page's node health (--http).
    pub role: Option<link::NodeRole>,
    /// Viewer time (ms) of the last line or detection from this input.
    pub last_ms: Option<u64>,
    /// Viewer times of recent Remote ID detections, for a per-second rate.
    pub rid_times: VecDeque<u64>,
    /// From the node's stats lines, when it sends them.
    pub seen_per_s: Option<u32>,
    pub drops: Option<u32>,
}

pub struct View {
    pub ansi: bool,
    pub nodes: BTreeMap<u8, NodeInfo>,
    /// When each drone was announced (hub ms), for the highlight.
    pub announced: HashMap<TrackId, u64>,
    /// Recent notable events, newest last.
    pub log: VecDeque<String>,
}

fn short_transport(t: Transport) -> &'static str {
    match t {
        Transport::WifiBeacon => "WB",
        Transport::WifiNan => "WN",
        Transport::BleLegacy => "B4",
        Transport::BleExtended => "B5",
        Transport::BleLongRange => "LR",
    }
}

fn short_ua(ua: Option<UaType>) -> &'static str {
    match ua {
        Some(UaType::HelicopterOrMultirotor) => "multi",
        Some(UaType::Aeroplane) => "plane",
        Some(UaType::HybridLift) => "vtol",
        Some(UaType::Gyroplane) => "gyro",
        Some(UaType::Glider) => "glider",
        Some(UaType::Ornithopter) => "ornith",
        Some(UaType::FreeBalloon | UaType::CaptiveBalloon) => "balloon",
        Some(UaType::Airship) => "airship",
        Some(UaType::Kite) => "kite",
        Some(UaType::FreeFallParachute) => "chute",
        Some(UaType::Rocket) => "rocket",
        Some(UaType::TetheredPoweredAircraft) => "tether",
        Some(UaType::GroundObstacle) => "obstcl",
        Some(UaType::Other) => "other",
        Some(UaType::None) | None => "?",
    }
}

fn compass(deg: f64) -> &'static str {
    const P: [&str; 16] = [
        "N", "NNE", "NE", "ENE", "E", "ESE", "SE", "SSE", "S", "SSW", "SW", "WSW", "W", "WNW",
        "NW", "NNW",
    ];
    P[((deg + 11.25).rem_euclid(360.0) / 22.5) as usize % 16]
}

fn dist(m: f64) -> String {
    if m < 10_000.0 {
        format!("{m:.0} m")
    } else {
        format!("{:.1} km", m / 1000.0)
    }
}

fn age(ms: u64) -> String {
    match ms {
        0..10_000 => format!("{:.1}s", ms as f64 / 1000.0),
        10_000..120_000 => format!("{}s", ms / 1000),
        _ => format!("{}m", ms / 60_000),
    }
}

/// Cut to `n` chars, so one long Self-ID cannot wrap the table.
fn fit(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n - 1).chain(['…']).collect()
    }
}

fn display_id(t: &Track) -> String {
    match t.primary_id() {
        Some(id) => match id.id.as_str() {
            Some(s) if id.id.is_clean_text() => s.to_string(),
            _ => id
                .id
                .as_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
        },
        // Not yet identified: show the transmitter address.
        None => t
            .latest_link()
            .map(|(_, l)| {
                let m = l.mac;
                format!(
                    "({:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x})",
                    m[0], m[1], m[2], m[3], m[4], m[5]
                )
            })
            .unwrap_or_default(),
    }
}

impl View {
    pub fn new(ansi: bool) -> Self {
        Self {
            ansi,
            nodes: BTreeMap::new(),
            announced: HashMap::new(),
            log: VecDeque::new(),
        }
    }

    pub fn note(&mut self, clock: &str, msg: String) {
        self.log.push_back(format!("{clock}  {msg}"));
        while self.log.len() > 6 {
            self.log.pop_front();
        }
    }

    fn style<'a>(&self, s: &'a str) -> &'a str {
        if self.ansi { s } else { "" }
    }

    /// The whole screen as text. Each line ends with "clear to end of line"
    /// in ANSI mode, so redrawing in place leaves no leftovers.
    pub fn render(&self, tr: &Tracker, ctx: &Context, clock: &str) -> String {
        let eol = if self.ansi { CLEAR_LINE } else { "" };
        let mut s = String::new();
        let total = tr.len();
        let active = tr.active_count(ctx.now_ms);
        let own = match ctx.own {
            Some(o) => format!("own {:.5},{:.5}", o.lat, o.lon),
            None => "own position unknown (no --own-pos): no range or bearing".into(),
        };
        let _ = writeln!(
            s,
            "rid-view  {clock}  drones: {active} active, {total} total  |  {own}{eol}"
        );
        for (n, info) in &self.nodes {
            let mut line = format!("  node {n} {}", info.source);
            if let Some(h) = &info.hello {
                let _ = write!(line, "  [{h}]");
            }
            if let Some(st) = &info.stats {
                let _ = write!(line, "  stats: {st}");
            }
            let _ = write!(line, "  rid lines: {}", info.rid_lines);
            if info.open_lines > 0 {
                let _ = write!(line, ", open links: {}", info.open_lines);
            }
            if info.bad_lines > 0 {
                let _ = write!(line, ", unparsed: {}", info.bad_lines);
            }
            if let Some(e) = &info.ended {
                let _ = write!(line, "  ({e})");
            }
            let _ = writeln!(s, "{line}{eol}");
        }
        let _ = writeln!(s, "{eol}");
        let _ = writeln!(
            s,
            "  {:<22} {:<7} {:<11} {:>5} {:>6} {:>8} {:>8} {:>6} {:>6}  {:<16} SELF-ID{eol}",
            "UAS ID", "TYPE", "HEARD", "RSSI", "AGE", "DIST", "BRG", "ALT", "SPD", "OPERATOR",
        );

        // Active first, then nearest (or most recently heard without a fix).
        let mut rows: Vec<(&Track, tracker::Geometry)> =
            tr.tracks().iter().map(|t| (t, ctx.geometry(t))).collect();
        rows.sort_by(|(a, ga), (b, gb)| {
            let st = |t| (ctx.state(t) == TrackState::Stale) as u8;
            st(a).cmp(&st(b)).then_with(|| {
                let d = |g: &tracker::Geometry| g.drone.map_or(f64::MAX, |d| d.distance_m);
                d(ga)
                    .total_cmp(&d(gb))
                    .then(b.last_seen_ms.cmp(&a.last_seen_ms))
            })
        });
        if rows.is_empty() {
            let _ = writeln!(s, "  (no drones yet){eol}");
        }
        for (t, g) in rows {
            let heard: Vec<&str> = t.transports().map(short_transport).collect();
            let loc = t.location.as_ref().map(|l| &l.value);
            let new = self
                .announced
                .get(&t.id)
                .is_some_and(|at| ctx.now_ms.saturating_sub(*at) < NEW_HIGHLIGHT_MS);
            let (pre, post) = if ctx.state(t) == TrackState::Stale {
                (self.style(DIM), self.style(RESET))
            } else if new {
                (self.style(BOLD_YELLOW), self.style(RESET))
            } else {
                ("", "")
            };
            let opr = g
                .operator
                .map(|o| format!("{} {:.0}°", dist(o.distance_m), o.bearing_deg))
                .or_else(|| {
                    t.system
                        .as_ref()
                        .and_then(|s| s.value.operator_position)
                        .map(|p| format!("{:.4},{:.4}", p.lat, p.lon))
                })
                .unwrap_or_else(|| "-".into());
            let self_id = t
                .self_id
                .as_ref()
                .and_then(|s| s.value.description.as_str().map(str::to_string))
                .unwrap_or_default();
            let _ = writeln!(
                s,
                "{pre}{} {:<22} {:<7} {:<11} {:>5} {:>6} {:>8} {:>8} {:>6} {:>6}  {:<16} {}{post}{eol}",
                if new { '*' } else { ' ' },
                fit(&display_id(t), 22),
                short_ua(t.ua_type),
                heard.join("+"),
                t.latest_link()
                    .map_or("-".into(), |(_, l)| l.rssi.to_string()),
                age(ctx.now_ms.saturating_sub(t.last_seen_ms)),
                g.drone.map_or("-".into(), |d| dist(d.distance_m)),
                g.drone.map_or("-".into(), |d| format!(
                    "{:.0}° {}",
                    d.bearing_deg,
                    compass(d.bearing_deg)
                )),
                loc.and_then(|l| l.altitude_geodetic)
                    .map_or("-".into(), |a| format!("{a:.0}m")),
                loc.and_then(|l| l.speed_horizontal)
                    .map_or("-".into(), |v| format!("{v:.1}")),
                fit(&opr, 16),
                fit(&self_id, 24),
            );
        }
        self.render_links(&mut s, tr, ctx, eol);
        if !self.log.is_empty() {
            let _ = writeln!(s, "{eol}");
            for l in &self.log {
                let _ = writeln!(s, "  {l}{eol}");
            }
        }
        s
    }
}

impl View {
    /// The open FPV links, below the drones. Nothing at all when there are none.
    fn render_links(&self, s: &mut String, tr: &Tracker, ctx: &Context, eol: &str) {
        if tr.open_links().is_empty() {
            return;
        }
        let ids = if ctx.config.show_link_ids {
            "identifiers shown"
        } else {
            "identifiers hidden; --show-link-ids to show"
        };
        let _ = writeln!(s, "{eol}");
        let _ = writeln!(s, "  OPEN FPV LINKS ({ids}){eol}");
        let _ = writeln!(
            s,
            "  {:<15} {:<10} {:<5} {:<26} {:>4} {:>5} {:>8} {:>6}{eol}",
            "SYSTEM", "ID", "DIR", "STREAMS", "CH", "RSSI", "PKTS", "AGE",
        );
        let mut links: Vec<&LinkTrack> = tr.open_links().iter().collect();
        links.sort_by_key(|l| {
            (
                ctx.link_state(l) == TrackState::Stale,
                std::cmp::Reverse(l.last_seen_ms),
            )
        });
        for l in links {
            let (pre, post) = if ctx.link_state(l) == TrackState::Stale {
                (self.style(DIM), self.style(RESET))
            } else {
                ("", "")
            };
            let id = match (shown_id(l, ctx), l.system.id_name()) {
                (Some(id), _) => id.to_string(),
                (None, None) => "-".into(),
                (None, Some(_)) if ctx.config.show_link_ids => "-".into(),
                (None, Some(_)) => "hidden".into(),
            };
            let dir = match (
                l.heard(openlink::Direction::AirToGround),
                l.heard(openlink::Direction::GroundToAir),
            ) {
                (true, true) => "dn+up",
                (true, false) => "down",
                (false, true) => "up",
                (false, false) => "?",
            };
            let latest = l.latest();
            let _ = writeln!(
                s,
                "{pre}  {:<15} {:<10} {:<5} {:<26} {:>4} {:>5} {:>8} {:>6}{post}{eol}",
                l.system.label(),
                id,
                dir,
                fit(&streams(l), 26),
                latest.map_or("-".into(), |x| x.channel.to_string()),
                latest.map_or("-".into(), |x| x.rssi.to_string()),
                l.packets(),
                age(ctx.now_ms.saturating_sub(l.last_seen_ms)),
            );
        }
    }
}

/// A link's streams in words: wfb-ng's video / mavlink / tunnel, otherwise
/// the port numbers, each with an arrow for its direction (down to the
/// ground, up to the aircraft).
fn streams(l: &LinkTrack) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut ports: Vec<(u8, openlink::Direction)> =
        l.streams.iter().map(|x| (x.port, x.direction)).collect();
    ports.sort();
    ports.dedup();
    for (port, dir) in ports {
        let probe = openlink::OpenLink {
            system: l.system,
            id: None,
            port,
            direction: dir,
            tx: [0; 6],
        };
        let arrow = match dir {
            openlink::Direction::AirToGround => "↓",
            openlink::Direction::GroundToAir => "↑",
            openlink::Direction::Unknown => "",
        };
        let name = match probe.wfb_stream() {
            Some(openlink::WfbStream::Video) => format!("video{arrow}"),
            Some(openlink::WfbStream::Mavlink) => format!("mavlink{arrow}"),
            Some(openlink::WfbStream::Tunnel) => format!("tunnel{arrow}"),
            None => format!("p{port}{arrow}"),
        };
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(compass(0.0), "N");
        assert_eq!(compass(359.0), "N");
        assert_eq!(compass(107.5), "ESE");
        assert_eq!(dist(1363.9), "1364 m");
        assert_eq!(dist(12_345.0), "12.3 km");
        assert_eq!(age(400), "0.4s");
        assert_eq!(age(65_000), "65s");
        assert_eq!(fit("abcdef", 4), "abc…");
    }

    fn sighting(port: u8, id: Option<u32>) -> link::OpenLinkSighting {
        link::OpenLinkSighting {
            link: openlink::OpenLink {
                system: openlink::System::WfbNg,
                id,
                port,
                direction: if port < 128 {
                    openlink::Direction::AirToGround
                } else {
                    openlink::Direction::GroundToAir
                },
                tx: [0x57, 0x42, 0x75, 0x05, 0xd6, port],
            },
            channel: 161,
            rssi: -61,
            uptime_us: 0,
            packets: 50,
        }
    }

    #[test]
    fn links_section_hides_ids_unless_asked() {
        for show in [false, true] {
            let mut tr: Tracker = Tracker::new(tracker::Config {
                show_link_ids: show,
                ..tracker::Config::default()
            });
            for port in [0, 16, 144] {
                tr.ingest_open_link(2, &sighting(port, Some(0x7505d6)), 1_000);
            }
            let ctx = tr.context(1_500, None);
            let text = View::new(false).render(&tr, &ctx, "00:00:00Z");
            assert!(text.contains("OPEN FPV LINKS"), "{text}");
            assert!(text.contains("wfb-ng/OpenIPC"));
            assert!(text.contains("video↓,mavlink↓,mavlink↑"), "{text}");
            assert!(text.contains("dn+up"));
            assert_eq!(text.contains("7505d6"), show, "{text}");
            assert_eq!(text.contains("hidden"), !show);
        }
        // No links: no section.
        let tr: Tracker = Tracker::default();
        let text = View::new(false).render(&tr, &tr.context(0, None), "00:00:00Z");
        assert!(!text.contains("OPEN FPV LINKS"));
    }
}
