//! The site file: where each node stands and which alarms are muted.
//!
//! The P4 hub reads `ROTORSCP/SITE.TXT` from its SD card at mount and
//! takes the same lines typed on its console (command `L`). Changes made on
//! the hub (screen or console) are **appended**, never rewritten, so a
//! hand-edited file is never lost: later lines win.
//!
//! One instruction per line; `#` starts a comment; case of the keywords
//! does not matter.
//!
//! ```text
//! # Node positions (the station address the hub's Health page shows):
//! node 10:bd:a3:ce:bd:18 39.95210 -75.16380   roof, north corner
//! node 10:bd:a3:ce:bd:18 clear
//! node all here          # (console/screen only) every node at the hub
//!
//! # Muted alarms (kind names as in the hub's "threat" lines):
//! mute evil_twin source=02:11:22:33:44:55 ssid=Cafe WiFi
//! mute deauth target=ff:ff:ff:ff:ff:ff
//! mute jam_broadband mhz=2437.0
//! unmute all
//!
//! # Drone control links (LR2021 nodes; both off unless a line says on):
//! control ids on          # read link identifiers (ELRS UID bytes, SiK NET ID...)
//! control content on      # decode sticks, telemetry, positions; keep raw packets
//! control content off
//! ```
//!
//! `ssid=` takes the rest of the line (names may hold spaces), so it comes
//! last.

use core::fmt::Write;

use link::ThreatKind;

use crate::threats::Mute;

/// Every kind, in `ThreatKind` order, for reading names back.
const KINDS: [ThreatKind; 16] = [
    ThreatKind::Deauth,
    ThreatKind::Disassoc,
    ThreatKind::BeaconFlood,
    ThreatKind::EvilTwin,
    ThreatKind::Karma,
    ThreatKind::ChannelSwitch,
    ThreatKind::AuthFlood,
    ThreatKind::ProbeFlood,
    ThreatKind::AttackTool,
    ThreatKind::BleSpam,
    ThreatKind::JamBroadband,
    ThreatKind::JamTone,
    ThreatKind::JamSweep,
    ThreatKind::JamPulsed,
    ThreatKind::Blinded,
    ThreatKind::ForgedLink,
];

/// A threat kind from its `name()` (`"evil_twin"`).
pub fn kind_from_name(s: &str) -> Option<ThreatKind> {
    KINDS.into_iter().find(|k| k.name().eq_ignore_ascii_case(s))
}

/// One understood line.
#[derive(Debug, Clone, PartialEq)]
pub enum SiteLine {
    /// Blank or comment.
    Nothing,
    NodeAt { mac: [u8; 6], lat: f64, lon: f64 },
    NodeClear { mac: [u8; 6] },
    /// Every node known now stands at the hub's own position.
    AllNodesHere,
    Mute(Mute),
    UnmuteAll,
    /// `control ids on|off`, `control content on|off` (protocol 12 opt-ins).
    Control { what: ControlSetting, on: bool },
}

/// The two control-link opt-ins (docs/CONTROL-LINKS.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlSetting {
    /// Read link identifiers.
    Ids,
    /// Decode control content (sticks, telemetry, positions).
    Content,
}

impl ControlSetting {
    pub const fn word(self) -> &'static str {
        match self {
            ControlSetting::Ids => "ids",
            ControlSetting::Content => "content",
        }
    }
}

/// Why a line was refused (shown to the operator).
pub type SiteError = &'static str;

/// `aa:bb:cc:dd:ee:ff` (or with `-`).
pub fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let mut out = [0u8; 6];
    let mut parts = s.split([':', '-']);
    for b in out.iter_mut() {
        let p = parts.next()?;
        if p.len() != 2 {
            return None;
        }
        *b = u8::from_str_radix(p, 16).ok()?;
    }
    parts.next().is_none().then_some(out)
}

fn lat_lon(lat: &str, lon: &str) -> Result<(f64, f64), SiteError> {
    let lat: f64 = lat.parse().map_err(|_| "latitude is not a number")?;
    let lon: f64 = lon.parse().map_err(|_| "longitude is not a number")?;
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Err("position out of range (latitude first, decimal degrees)");
    }
    Ok((lat, lon))
}

pub fn parse_line(line: &str) -> Result<SiteLine, SiteError> {
    let line = line.split('#').next().unwrap_or("").trim();
    if line.is_empty() {
        return Ok(SiteLine::Nothing);
    }
    let (word, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    let rest = rest.trim();
    let mut it = rest.split_whitespace();
    if word.eq_ignore_ascii_case("node") {
        let who = it.next().ok_or("node needs an address")?;
        if who.eq_ignore_ascii_case("all") {
            return match it.next() {
                Some(w) if w.eq_ignore_ascii_case("here") => Ok(SiteLine::AllNodesHere),
                _ => Err("say: node all here"),
            };
        }
        let mac = parse_mac(who).ok_or("node address must look like aa:bb:cc:dd:ee:ff")?;
        let a = it.next().ok_or("node needs a position (lat lon) or clear")?;
        if a.eq_ignore_ascii_case("clear") {
            return Ok(SiteLine::NodeClear { mac });
        }
        let b = it.next().ok_or("node needs a longitude after the latitude")?;
        let (lat, lon) = lat_lon(a, b)?;
        return Ok(SiteLine::NodeAt { mac, lat, lon });
    }
    if word.eq_ignore_ascii_case("control") {
        let what = match it.next() {
            Some(w) if w.eq_ignore_ascii_case("ids") => ControlSetting::Ids,
            Some(w) if w.eq_ignore_ascii_case("content") => ControlSetting::Content,
            _ => return Err("say: control ids on|off, or control content on|off"),
        };
        let on = match it.next() {
            Some(w) if w.eq_ignore_ascii_case("on") => true,
            Some(w) if w.eq_ignore_ascii_case("off") => false,
            _ => return Err("say on or off after control ids / control content"),
        };
        if it.next().is_some() {
            return Err("say: control ids on|off, or control content on|off");
        }
        return Ok(SiteLine::Control { what, on });
    }
    if word.eq_ignore_ascii_case("unmute") {
        return match it.next() {
            Some(w) if w.eq_ignore_ascii_case("all") => Ok(SiteLine::UnmuteAll),
            _ => Err("say: unmute all"),
        };
    }
    if word.eq_ignore_ascii_case("mute") {
        let (kind, mut rest) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        let kind = kind_from_name(kind).ok_or("unknown alarm kind (see the hub's threat lines)")?;
        let mut m = Mute { kind, ssid: heapless::String::new(), source: [0; 6], target: [0; 6], freq: 0 };
        loop {
            rest = rest.trim_start();
            if rest.is_empty() {
                break;
            }
            if let Some(name) = rest.strip_prefix("ssid=") {
                m.ssid = heapless::String::try_from(name.trim_end()).map_err(|_| "network name longer than 32")?;
                break;
            }
            let (tok, more) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            rest = more;
            let (k, v) = tok.split_once('=').ok_or("mute options look like key=value")?;
            match k {
                "source" => m.source = parse_mac(v).ok_or("bad source address")?,
                "target" => m.target = parse_mac(v).ok_or("bad target address")?,
                "mhz" => {
                    let f: f64 = v.parse().map_err(|_| "bad mhz")?;
                    m.freq = (f * 10.0 + 0.5) as u32;
                }
                _ => return Err("mute options: source= target= mhz= ssid="),
            }
        }
        return Ok(SiteLine::Mute(m));
    }
    Err("unknown line (node, mute, unmute, control)")
}

/// Node positions held by [`SiteState`].
pub const MAX_PLACED: usize = 16;

/// What a whole site file says, line by line (later lines win). Small and
/// fixed-size, so a file of any length can be read in pieces.
#[derive(Debug, Clone, Default)]
pub struct SiteState {
    /// Station address -> (lat, lon).
    pub nodes: heapless::Vec<([u8; 6], f64, f64), MAX_PLACED>,
    pub mutes: heapless::Vec<Mute, { crate::threats::MAX_MUTES }>,
    /// Control-link opt-ins as the file last set them (`None`: never
    /// mentioned, which means off).
    pub control_ids: Option<bool>,
    pub control_content: Option<bool>,
    pub used: u32,
    pub ignored: u32,
}

impl SiteState {
    pub fn place(&mut self, mac: [u8; 6], lat: f64, lon: f64) {
        self.nodes.retain(|n| n.0 != mac);
        if self.nodes.is_full() {
            self.nodes.remove(0);
        }
        let _ = self.nodes.push((mac, lat, lon));
    }

    /// Apply one line. `AllNodesHere` needs a live position and is ignored
    /// here (the hub handles it).
    pub fn apply(&mut self, l: SiteLine) {
        match l {
            SiteLine::Nothing => return,
            SiteLine::NodeAt { mac, lat, lon } => self.place(mac, lat, lon),
            SiteLine::NodeClear { mac } => self.nodes.retain(|n| n.0 != mac),
            SiteLine::Mute(m) => {
                if !self.mutes.contains(&m) {
                    if self.mutes.is_full() {
                        self.mutes.remove(0);
                    }
                    let _ = self.mutes.push(m);
                }
            }
            SiteLine::UnmuteAll => self.mutes.clear(),
            SiteLine::Control { what: ControlSetting::Ids, on } => self.control_ids = Some(on),
            SiteLine::Control { what: ControlSetting::Content, on } => self.control_content = Some(on),
            SiteLine::AllNodesHere => {
                self.ignored += 1;
                return;
            }
        }
        self.used += 1;
    }
}

/// Longest line kept; a longer one is ignored as a whole.
pub const MAX_LINE: usize = 160;

/// Splits a byte stream fed in pieces into lines.
#[derive(Debug, Default)]
pub struct Lines {
    buf: heapless::Vec<u8, MAX_LINE>,
    too_long: bool,
    /// 1-based number of the line being read.
    pub number: u32,
}

impl Lines {
    /// Feed bytes; `f(line_number, Ok(text) | Err(reason))` per finished line.
    pub fn feed(&mut self, bytes: &[u8], mut f: impl FnMut(u32, Result<&str, SiteError>)) {
        for &b in bytes {
            if b == b'\n' {
                self.end(&mut f);
            } else if self.buf.push(b).is_err() {
                self.too_long = true;
            }
        }
    }

    /// The last line, if the file does not end in a newline.
    pub fn finish(&mut self, mut f: impl FnMut(u32, Result<&str, SiteError>)) {
        if !self.buf.is_empty() || self.too_long {
            self.end(&mut f);
        }
    }

    fn end(&mut self, f: &mut impl FnMut(u32, Result<&str, SiteError>)) {
        self.number += 1;
        let r = if self.too_long {
            Err("line too long")
        } else {
            core::str::from_utf8(&self.buf).map_err(|_| "not text")
        };
        f(self.number, r);
        self.buf.clear();
        self.too_long = false;
    }
}

/// Read a whole site file fed in pieces into a [`SiteState`]; `refused`
/// hears each line that was not understood.
pub fn read_into(state: &mut SiteState, lines: &mut Lines, bytes: &[u8], mut refused: impl FnMut(u32, SiteError)) {
    lines.feed(bytes, |n, r| match r.and_then(parse_line) {
        Ok(l) => state.apply(l),
        Err(e) => {
            state.ignored += 1;
            refused(n, e);
        }
    });
}

struct MacText<'a>(&'a [u8; 6]);

impl core::fmt::Display for MacText<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let m = self.0;
        write!(f, "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5])
    }
}

/// The line that recreates `m` (ends in `\n`).
pub fn write_mute<W: Write>(w: &mut W, m: &Mute) -> core::fmt::Result {
    write!(w, "mute {}", m.kind.name())?;
    if m.source != [0; 6] {
        write!(w, " source={}", MacText(&m.source))?;
    }
    if m.target != [0; 6] {
        write!(w, " target={}", MacText(&m.target))?;
    }
    if m.freq != 0 {
        write!(w, " mhz={}.{}", m.freq / 10, m.freq % 10)?;
    }
    if !m.ssid.is_empty() {
        write!(w, " ssid={}", m.ssid)?;
    }
    w.write_char('\n')
}

/// The line that sets a control-link opt-in (ends in `\n`).
pub fn write_control<W: Write>(w: &mut W, what: ControlSetting, on: bool) -> core::fmt::Result {
    writeln!(w, "control {} {}", what.word(), if on { "on" } else { "off" })
}

/// The line that places a node (ends in `\n`).
pub fn write_node<W: Write>(w: &mut W, mac: &[u8; 6], lat: f64, lon: f64) -> core::fmt::Result {
    writeln!(w, "node {} {:.7} {:.7}", MacText(mac), lat, lon)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn reads_every_kind_of_line() {
        assert_eq!(parse_line("  # just a comment"), Ok(SiteLine::Nothing));
        assert_eq!(parse_line(""), Ok(SiteLine::Nothing));
        let mac = [0x10, 0xbd, 0xa3, 0xce, 0xbd, 0x18];
        match parse_line("NODE 10:BD:a3:ce:bd:18  39.9521 -75.1638  roof # north").unwrap() {
            SiteLine::NodeAt { mac: m, lat, lon } => {
                assert_eq!(m, mac);
                assert!((lat - 39.9521).abs() < 1e-9 && (lon + 75.1638).abs() < 1e-9);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(parse_line("node 10-bd-a3-ce-bd-18 clear"), Ok(SiteLine::NodeClear { mac }));
        assert_eq!(parse_line("node all here"), Ok(SiteLine::AllNodesHere));
        assert_eq!(parse_line("unmute all"), Ok(SiteLine::UnmuteAll));
        let SiteLine::Mute(m) = parse_line("mute evil_twin source=02:11:22:33:44:55 ssid=Cafe WiFi Guest ").unwrap() else {
            panic!()
        };
        assert_eq!(m.kind, ThreatKind::EvilTwin);
        assert_eq!(m.ssid.as_str(), "Cafe WiFi Guest");
        assert_eq!(m.source, [2, 0x11, 0x22, 0x33, 0x44, 0x55]);
        let SiteLine::Mute(j) = parse_line("mute jam_broadband mhz=2437.0").unwrap() else { panic!() };
        assert_eq!(j.freq, 24_370);
        assert_eq!(parse_line("control ids on"), Ok(SiteLine::Control { what: ControlSetting::Ids, on: true }));
        assert_eq!(parse_line("CONTROL Content OFF # not today"), Ok(SiteLine::Control { what: ControlSetting::Content, on: false }));
    }

    #[test]
    fn control_lines_last_one_wins_and_round_trip() {
        let mut st = SiteState::default();
        assert_eq!((st.control_ids, st.control_content), (None, None));
        for l in ["control content on", "control ids on", "control ids off"] {
            st.apply(parse_line(l).unwrap());
        }
        assert_eq!((st.control_ids, st.control_content), (Some(false), Some(true)));
        for what in [ControlSetting::Ids, ControlSetting::Content] {
            for on in [false, true] {
                let mut s: heapless::String<32> = heapless::String::new();
                write_control(&mut s, what, on).unwrap();
                assert_eq!(parse_line(&s), Ok(SiteLine::Control { what, on }));
            }
        }
    }

    #[test]
    fn refuses_bad_lines_with_a_reason() {
        for bad in [
            "node 10:bd:a3 1 2",
            "node 10:bd:a3:ce:bd:18 91 0",
            "node 10:bd:a3:ce:bd:18 39.9",
            "node all there",
            "mute no_such_kind",
            "mute deauth target=zz",
            "mute deauth colour=red",
            "unmute some",
            "control",
            "control ids",
            "control ids maybe",
            "control names on",
            "control ids on please",
            "hello",
        ] {
            assert!(parse_line(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_long_file_read_in_pieces_keeps_the_latest_lines() {
        let mut text = std::string::String::new();
        // 300 appended changes, far more than any buffer: the last ones win.
        for i in 0..300 {
            text += &std::format!("node 10:bd:a3:ce:bd:{:02x} 1.{i} 2.0\n", i % 20);
            text += "mute deauth target=ff:ff:ff:ff:ff:ff\n";
            if i % 7 == 0 {
                text += "unmute all\n";
            }
        }
        text += "node 10:bd:a3:ce:bd:00 clear\nbad line here\n";
        text += &"x".repeat(400);
        text += "\nmute evil_twin ssid=Last One"; // no final newline
        for piece in [1usize, 7, 512] {
            let (mut st, mut lines) = (SiteState::default(), Lines::default());
            let mut refused = std::vec::Vec::new();
            for chunk in text.as_bytes().chunks(piece) {
                read_into(&mut st, &mut lines, chunk, |n, e| refused.push((n, e)));
            }
            lines.finish(|n, r| match r.and_then(parse_line) {
                Ok(l) => st.apply(l),
                Err(e) => refused.push((n, e)),
            });
            assert_eq!(st.nodes.len(), 16, "20 nodes, oldest dropped past 16");
            assert!(st.nodes.iter().all(|n| n.0[5] != 0), "node :00 cleared");
            // The latest position of node :13 is from i = 299.
            assert!(st.nodes.iter().any(|n| n.0[5] == 0x13 && (n.1 - 1.299).abs() < 1e-9));
            assert_eq!(st.mutes.len(), 2, "unmute all at i=294, then deauth + twin");
            assert_eq!(st.mutes[1].ssid.as_str(), "Last One");
            assert_eq!(refused.len(), 2, "{refused:?}");
            assert_eq!(refused[1].1, "line too long");
        }
    }

    #[test]
    fn written_lines_read_back_the_same() {
        let mut m = Mute {
            kind: ThreatKind::Karma,
            ssid: heapless::String::try_from("Free Airport WiFi").unwrap(),
            source: [0xde, 0xad, 0xbe, 0xef, 0, 1],
            target: [0xff; 6],
            freq: 0,
        };
        let mut s: heapless::String<128> = heapless::String::new();
        write_mute(&mut s, &m).unwrap();
        assert_eq!(parse_line(&s), Ok(SiteLine::Mute(m.clone())));
        m.kind = ThreatKind::JamTone;
        m.ssid.clear();
        m.freq = 58_005;
        s.clear();
        write_mute(&mut s, &m).unwrap();
        assert_eq!(parse_line(&s), Ok(SiteLine::Mute(m)));
        for k in KINDS {
            assert_eq!(kind_from_name(k.name()), Some(k));
        }
        s.clear();
        write_node(&mut s, &[1, 2, 3, 4, 5, 6], -33.8688197, 151.2092955).unwrap();
        let SiteLine::NodeAt { lat, lon, .. } = parse_line(&s).unwrap() else { panic!() };
        assert!((lat + 33.8688197).abs() < 1e-7 && (lon - 151.2092955).abs() < 1e-7);
    }
}
