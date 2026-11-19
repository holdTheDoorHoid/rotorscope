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
//! # Alarms muted for good: a venue's known false alarm, one specific
//! # thing per line (kind names as in the hub's "threat" lines):
//! mute evil_twin source=02:11:22:33:44:55 ssid=Cafe WiFi
//! mute deauth target=ff:ff:ff:ff:ff:ff
//! unmute all
//!
//! # Drone control links (LR2021 nodes; both off unless a line says on):
//! control ids on          # read link identifiers (ELRS UID bytes, SiK NET ID...)
//! control content on      # decode sticks, telemetry, positions; keep raw packets
//! control content off
//!
//! # The screen's colour theme (P4 hub; also set on the Settings page):
//! theme auto              # auto | glass-cockpit | field-day | red-night
//! theme-day field-day     # Auto's theme while the sun is up
//! theme-night red-night   # Auto's theme at night (and before it knows)
//! theme-switch twilight   # twilight (civil dawn and dusk) | sunrise (sunrise and sunset)
//! ```
//!
//! `ssid=` takes the rest of the line (names may hold spaces), so it comes
//! last.
//!
//! **Mutes are specific and never for safety alarms** (WP-11b). A `mute`
//! line must name every field of its kind's key
//! ([`crate::threats::key_of`]): a network copy its transmitter and name,
//! a flood its victim, a contested control link its identifier and band.
//! A line that names less (`mute deauth`, `mute evil_twin ssid=..`), and
//! any line for a kind that can mean the hub is missing drones (jamming,
//! `blinded`, `area_gnss`, `id_flood`, `forged_link`: those can only be
//! snoozed on the hub, for five minutes), is **not honoured**: refused with
//! a reason starting [`crate::threats::NOT_HONOURED`], counted in
//! [`SiteState::mutes_dropped`]. Older hubs saved exactly such catch-all
//! lines from the screen's "Mute" action; they are dropped this way on
//! load. The operator's mutes from the screen are timed and kept in RAM
//! only: they are not written here.

use core::fmt::Write;

use link::ThreatKind;

use crate::threats::{Mute, NOT_HONOURED};

/// Every kind, in `ThreatKind` order, for reading names back.
const KINDS: [ThreatKind; 23] = [
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
    ThreatKind::DroneDeauth,
    ThreatKind::DroneTakeover,
    ThreatKind::DroneNetClone,
    ThreatKind::ControlContested,
    ThreatKind::MavlinkAttack,
    ThreatKind::AreaGnss,
    ThreatKind::IdFlood,
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
    /// `home LAT LON [HEIGHT]`: where the hub itself stands, placed by the operator (WP-12).
    /// The strongest prior for the GPS spoofing checks: it replaces the home the hub learns.
    /// Height in metres above the WGS-84 ellipsoid, optional.
    Home { lat: f64, lon: f64, alt: Option<f64> },
    /// `home clear`: no placed home (the hub learns one again).
    HomeClear,
    /// `theme ...`, `theme-day ...`, `theme-night ...`, `theme-switch ...`:
    /// the screen's colours. Not a security setting; kept here so the
    /// operator's choice survives a reboot.
    Theme(ThemeSetting),
}

/// The screen themes a site file can name (the screen crate's
/// `screen::Theme`; the tracker does not depend on it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeName {
    GlassCockpit,
    FieldDay,
    RedNight,
}

impl ThemeName {
    pub const ALL: [ThemeName; 3] = [ThemeName::GlassCockpit, ThemeName::FieldDay, ThemeName::RedNight];
    pub const fn word(self) -> &'static str {
        match self {
            ThemeName::GlassCockpit => "glass-cockpit",
            ThemeName::FieldDay => "field-day",
            ThemeName::RedNight => "red-night",
        }
    }
    pub fn from_word(w: &str) -> Option<ThemeName> {
        Self::ALL.into_iter().find(|t| t.word().eq_ignore_ascii_case(w))
    }
}

/// One theme line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeSetting {
    /// `theme auto` (`None`) or one theme always.
    Theme(Option<ThemeName>),
    Day(ThemeName),
    Night(ThemeName),
    /// `theme-switch twilight` (`true`: civil dawn and dusk) or `sunrise`.
    SwitchTwilight(bool),
}

/// The theme lines' state: each `None` until a line sets it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ThemeSite {
    /// `Some(None)`: auto.
    pub theme: Option<Option<ThemeName>>,
    pub day: Option<ThemeName>,
    pub night: Option<ThemeName>,
    pub switch_twilight: Option<bool>,
}

impl ThemeSite {
    pub fn apply(&mut self, t: ThemeSetting) {
        match t {
            ThemeSetting::Theme(x) => self.theme = Some(x),
            ThemeSetting::Day(x) => self.day = Some(x),
            ThemeSetting::Night(x) => self.night = Some(x),
            ThemeSetting::SwitchTwilight(x) => self.switch_twilight = Some(x),
        }
    }

    /// Nothing said.
    pub fn is_empty(&self) -> bool {
        *self == ThemeSite::default()
    }
}

impl core::fmt::Display for ThemeSite {
    /// `theme auto (day field-day, night red-night, switch twilight)`; the
    /// parts the file never set say `default`.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let name = |t: Option<ThemeName>| t.map_or("default", |t| t.word());
        f.write_str("theme ")?;
        match self.theme {
            None => f.write_str("default")?,
            Some(None) => f.write_str("auto")?,
            Some(Some(t)) => f.write_str(t.word())?,
        }
        write!(
            f,
            " (day {}, night {}, switch {})",
            name(self.day),
            name(self.night),
            match self.switch_twilight {
                None => "default",
                Some(true) => "twilight",
                Some(false) => "sunrise",
            }
        )
    }
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
    if word.eq_ignore_ascii_case("home") {
        let a = it.next().ok_or("home needs a position (lat lon [height]) or clear")?;
        if a.eq_ignore_ascii_case("clear") {
            return if it.next().is_none() { Ok(SiteLine::HomeClear) } else { Err("say: home clear") };
        }
        let b = it.next().ok_or("home needs a longitude after the latitude")?;
        let (lat, lon) = lat_lon(a, b)?;
        let alt = match it.next() {
            None => None,
            Some(h) => {
                let h: f64 = h.parse().map_err(|_| "home height is not a number (metres)")?;
                if !(-500.0..=9_000.0).contains(&h) {
                    return Err("home height out of range (metres above the ellipsoid)");
                }
                Some(h)
            }
        };
        if it.next().is_some() {
            return Err("say: home LAT LON [HEIGHT]");
        }
        return Ok(SiteLine::Home { lat, lon, alt });
    }
    if let Some(kind) = word.get(..5).filter(|h| h.eq_ignore_ascii_case("theme")).and_then(|_| word.get(5..)) {
        let v = it.next().ok_or("say: theme auto|glass-cockpit|field-day|red-night")?;
        if it.next().is_some() {
            return Err("one word after theme, theme-day, theme-night or theme-switch");
        }
        let name = || ThemeName::from_word(v).ok_or("theme names: glass-cockpit, field-day, red-night");
        let t = match kind {
            "" if v.eq_ignore_ascii_case("auto") => ThemeSetting::Theme(None),
            "" => ThemeSetting::Theme(Some(name()?)),
            w if w.eq_ignore_ascii_case("-day") => ThemeSetting::Day(name()?),
            w if w.eq_ignore_ascii_case("-night") => ThemeSetting::Night(name()?),
            w if w.eq_ignore_ascii_case("-switch") => match v {
                v if v.eq_ignore_ascii_case("twilight") => ThemeSetting::SwitchTwilight(true),
                v if v.eq_ignore_ascii_case("sunrise") => ThemeSetting::SwitchTwilight(false),
                _ => return Err("say: theme-switch twilight|sunrise"),
            },
            _ => return Err("say: theme, theme-day, theme-night or theme-switch"),
        };
        return Ok(SiteLine::Theme(t));
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
        let mut m = Mute::empty(kind);
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
        // For good, so specific and not a safety-critical kind.
        m.check_permanent()?;
        return Ok(SiteLine::Mute(m));
    }
    Err("unknown line (node, home, mute, unmute, control, theme)")
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
    /// The hub's own placed position (`home` lines), `(lat, lon, height)`.
    pub home: Option<(f64, f64, Option<f64>)>,
    /// The screen theme lines.
    pub theme: ThemeSite,
    pub used: u32,
    pub ignored: u32,
    /// `mute` lines not honoured: too broad, or for a kind that can only be
    /// snoozed (also counted in `ignored`).
    pub mutes_dropped: u32,
}

/// Whether a refusal from [`parse_line`] is a `mute` line that was
/// understood but not honoured (too broad, or a safety-critical kind).
pub fn is_dropped_mute(e: SiteError) -> bool {
    e.starts_with(NOT_HONOURED)
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
            SiteLine::Home { lat, lon, alt } => self.home = Some((lat, lon, alt)),
            SiteLine::HomeClear => self.home = None,
            SiteLine::Theme(t) => self.theme.apply(t),
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
    lines.feed(bytes, |n, r| state.line(n, r, &mut refused));
}

/// The last line of a file fed with [`read_into`] (one without a final
/// newline).
pub fn finish_into(state: &mut SiteState, lines: &mut Lines, mut refused: impl FnMut(u32, SiteError)) {
    lines.finish(|n, r| state.line(n, r, &mut refused));
}

impl SiteState {
    fn line(&mut self, n: u32, r: Result<&str, SiteError>, refused: &mut impl FnMut(u32, SiteError)) {
        match r.and_then(parse_line) {
            Ok(l) => self.apply(l),
            Err(e) => {
                self.ignored += 1;
                if is_dropped_mute(e) {
                    self.mutes_dropped += 1;
                }
                refused(n, e);
            }
        }
    }
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

/// The line that recreates one theme setting (ends in `\n`).
pub fn write_theme<W: Write>(w: &mut W, t: ThemeSetting) -> core::fmt::Result {
    match t {
        ThemeSetting::Theme(None) => writeln!(w, "theme auto"),
        ThemeSetting::Theme(Some(n)) => writeln!(w, "theme {}", n.word()),
        ThemeSetting::Day(n) => writeln!(w, "theme-day {}", n.word()),
        ThemeSetting::Night(n) => writeln!(w, "theme-night {}", n.word()),
        ThemeSetting::SwitchTwilight(tw) => writeln!(w, "theme-switch {}", if tw { "twilight" } else { "sunrise" }),
    }
}

/// The line that places a node (ends in `\n`).
pub fn write_node<W: Write>(w: &mut W, mac: &[u8; 6], lat: f64, lon: f64) -> core::fmt::Result {
    writeln!(w, "node {} {:.7} {:.7}", MacText(mac), lat, lon)
}

// ------------------------------------------------------ evidence wording --

/// Evidence and console text for one rule: what it silences and for how
/// long. Addresses and network names are what the alarm's frames claimed
/// (anyone can put any address in a frame), and are said to be.
/// `mute 2 h: Evil twin network; claimed source 02:11:22:33:44:55, claimed
/// name "Cafe WiFi"; ends at hub uptime 7201 s`.
pub fn write_mute_text<W: Write>(w: &mut W, m: &Mute, now_ms: u64) -> core::fmt::Result {
    use crate::threats::key;
    match m.ends_ms() {
        None => w.write_str("mute for good (site file)")?,
        Some(e) => {
            let left = e.saturating_sub(now_ms);
            let what = if m.snooze() { "snooze" } else { "mute" };
            if left >= 3_600_000 && left % 3_600_000 < 1_000 {
                write!(w, "{what} {} h", left / 3_600_000)?
            } else {
                write!(w, "{what} {} min", left.div_ceil(60_000))?
            }
        }
    }
    write!(w, ": {}", crate::threats::threat_label(m.kind))?;
    let k = m.key();
    if k & key::NODE != 0 {
        write!(w, "; node {}", m.node)?;
    }
    if k & key::SOURCE != 0 {
        write!(w, "; claimed source {}", MacText(&m.source))?;
    }
    if k & key::TARGET != 0 {
        if m.kind == ThreatKind::AreaGnss {
            w.write_str(if m.target[0] == 1 { "; jamming" } else { "; spoofing" })?;
        } else {
            write!(w, "; claimed target {}", MacText(&m.target))?;
        }
    }
    if k & key::SSID != 0 {
        write!(w, "; claimed name \"{}\"", link::printable::Printable(&m.ssid))?;
    }
    if k & (key::FREQ | key::BAND) != 0 {
        write!(w, "; {}.{} MHz", m.freq / 10, m.freq % 10)?;
    }
    if k & key::SUB != 0 {
        write!(w, "; sub {}", m.sub)?;
    }
    if let Some(e) = m.ends_ms() {
        write!(w, "; ends at hub uptime {} s", e / 1000)?;
    }
    Ok(())
}

// ------------------------------------------------- the record of a load --

/// One load of the site file at most logs this many change events; the
/// rest are counted in one more event. (A file of any length holds at most
/// [`crate::threats::MAX_MUTES`] mutes and [`MAX_PLACED`] positions, so a
/// load can change at most 80 things; a huge file costs one hash, one
/// summary and at most this many events.)
pub const MAX_CHANGE_EVENTS: usize = 48;

/// What changed from one load of the site file to the next.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Change<'a> {
    MuteAdded(&'a Mute),
    MuteRemoved(&'a Mute),
    /// Placed, or moved (`from`: the old position).
    NodeSet { mac: [u8; 6], lat: f64, lon: f64, from: Option<(f64, f64)> },
    NodeCleared { mac: [u8; 6] },
}

/// Every change from `old` to `new`, in order (mutes added, mutes
/// removed, positions set, positions cleared). Returns how many.
pub fn changes<'a>(old: &'a SiteState, new: &'a SiteState, f: impl FnMut(Change<'a>)) -> usize {
    changes_since(&old.mutes, &old.nodes, new, f)
}

/// As [`changes`], from what is in force rather than a whole old state:
/// the hub passes the tracker's rules (only those for good are the file's)
/// and the node positions it uses, so nothing is copied.
pub fn changes_since<'a>(
    old_mutes: &'a [Mute],
    old_nodes: &[([u8; 6], f64, f64)],
    new: &'a SiteState,
    mut f: impl FnMut(Change<'a>),
) -> usize {
    let mut n = 0;
    let mut out = |c| {
        n += 1;
        f(c)
    };
    let old = || old_mutes.iter().filter(|m| m.permanent());
    for m in new.mutes.iter().filter(|m| !old().any(|o| o == *m)) {
        out(Change::MuteAdded(m));
    }
    for m in old().filter(|m| !new.mutes.contains(m)) {
        out(Change::MuteRemoved(m));
    }
    for &(mac, lat, lon) in &new.nodes {
        let was = old_nodes.iter().find(|o| o.0 == mac).map(|o| (o.1, o.2));
        if was != Some((lat, lon)) {
            out(Change::NodeSet { mac, lat, lon, from: was });
        }
    }
    for &(mac, ..) in old_nodes.iter().filter(|o| !new.nodes.iter().any(|n| n.0 == o.0)) {
        out(Change::NodeCleared { mac });
    }
    n
}

/// Evidence text for one change.
pub fn write_change<W: Write>(w: &mut W, c: &Change<'_>) -> core::fmt::Result {
    w.write_str("site file changed: ")?;
    match c {
        Change::MuteAdded(m) => {
            w.write_str("added ")?;
            write_mute_text(w, m, 0)
        }
        Change::MuteRemoved(m) => {
            w.write_str("removed ")?;
            write_mute_text(w, m, 0)
        }
        Change::NodeSet { mac, lat, lon, from } => {
            write!(w, "node {} at {lat:.7} {lon:.7}", MacText(mac))?;
            match from {
                Some((a, b)) => write!(w, " (was {a:.7} {b:.7}); spoof checks use it"),
                None => w.write_str(" (new); spoof checks use it"),
            }
        }
        Change::NodeCleared { mac } => write!(w, "node {} position removed", MacText(mac)),
    }
}

/// The summary of one load, for an evidence Setting event: the file's
/// SHA-256, its size, and what it sets. `lines`: lines read.
pub struct LoadSummary<'a> {
    pub st: &'a SiteState,
    pub sha256: &'a [u8; 32],
    pub bytes: u32,
    pub lines: u32,
}

impl core::fmt::Display for LoadSummary<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let st = self.st;
        f.write_str("site file loaded: sha256 ")?;
        for b in self.sha256 {
            write!(f, "{b:02x}")?;
        }
        let onoff = |v: Option<bool>| if v == Some(true) { "on" } else { "off" };
        write!(
            f,
            ", {} bytes, {} lines ({} used, {} ignored); {} mutes for good, {} mute lines not honoured; {} node positions; control ids {}, control content {}",
            self.bytes,
            self.lines,
            st.used,
            st.ignored,
            st.mutes.len(),
            st.mutes_dropped,
            st.nodes.len(),
            onoff(st.control_ids),
            onoff(st.control_content),
        )?;
        write!(f, "; {}", st.theme)
    }
}

impl LoadSummary<'_> {
    /// The event's data: the SHA-256, then mutes, mute lines not honoured,
    /// node positions (one byte each, saturating), control ids and content
    /// (0 off, 1 on).
    pub fn data(&self) -> [u8; 37] {
        let mut d = [0u8; 37];
        d[..32].copy_from_slice(self.sha256);
        let b = |n: usize| n.min(255) as u8;
        d[32] = b(self.st.mutes.len());
        d[33] = b(self.st.mutes_dropped as usize);
        d[34] = b(self.st.nodes.len());
        d[35] = u8::from(self.st.control_ids == Some(true));
        d[36] = u8::from(self.st.control_content == Some(true));
        d
    }
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
        let SiteLine::Mute(j) = parse_line("mute control_contested target=00:00:12:34:56:78 mhz=915.5").unwrap() else {
            panic!()
        };
        assert_eq!(j.freq, 9_155);
        assert_eq!(parse_line("control ids on"), Ok(SiteLine::Control { what: ControlSetting::Ids, on: true }));
        // WP-12: the hub's own placed position.
        assert_eq!(parse_line("home 39.95210 -75.16380 12.5 # roof"), Ok(SiteLine::Home { lat: 39.9521, lon: -75.1638, alt: Some(12.5) }));
        assert_eq!(parse_line("HOME 39.9521 -75.1638"), Ok(SiteLine::Home { lat: 39.9521, lon: -75.1638, alt: None }));
        assert_eq!(parse_line("home clear"), Ok(SiteLine::HomeClear));
        assert!(parse_line("home 91 0").is_err());
        assert!(parse_line("home 39.9 -75.1 99999").is_err());
        assert!(parse_line("home 39.9").is_err());
        let mut st = SiteState::default();
        st.apply(parse_line("home 1.5 2.5").unwrap());
        assert_eq!(st.home, Some((1.5, 2.5, None)));
        st.apply(parse_line("home clear").unwrap());
        assert_eq!(st.home, None);
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
    fn theme_lines_read_back_and_last_one_wins() {
        use ThemeName::*;
        assert_eq!(parse_line("theme auto"), Ok(SiteLine::Theme(ThemeSetting::Theme(None))));
        assert_eq!(parse_line("THEME Red-Night # dark site"), Ok(SiteLine::Theme(ThemeSetting::Theme(Some(RedNight)))));
        assert_eq!(parse_line("theme-day glass-cockpit"), Ok(SiteLine::Theme(ThemeSetting::Day(GlassCockpit))));
        assert_eq!(parse_line("Theme-Night field-day"), Ok(SiteLine::Theme(ThemeSetting::Night(FieldDay))));
        assert_eq!(parse_line("theme-switch sunrise"), Ok(SiteLine::Theme(ThemeSetting::SwitchTwilight(false))));
        assert_eq!(parse_line("theme-switch twilight"), Ok(SiteLine::Theme(ThemeSetting::SwitchTwilight(true))));
        for bad in ["theme", "theme night", "theme auto please", "theme-day auto", "theme-switch dusk", "theme-colour red-night", "themé x"] {
            assert!(parse_line(bad).is_err(), "{bad}");
        }
        // Every line the hub writes reads back as itself.
        let mut all = std::vec![ThemeSetting::Theme(None), ThemeSetting::SwitchTwilight(true), ThemeSetting::SwitchTwilight(false)];
        for n in ThemeName::ALL {
            all.extend([ThemeSetting::Theme(Some(n)), ThemeSetting::Day(n), ThemeSetting::Night(n)]);
        }
        for t in all {
            let mut s: heapless::String<40> = heapless::String::new();
            write_theme(&mut s, t).unwrap();
            assert!(s.ends_with('\n'));
            assert_eq!(parse_line(&s), Ok(SiteLine::Theme(t)), "{s}");
        }
        // A file: later lines win, and the summary says what it set.
        let st = load("theme red-night\ntheme-night glass-cockpit\ntheme auto\ntheme-switch sunrise\n");
        assert_eq!(
            st.theme,
            ThemeSite { theme: Some(None), day: None, night: Some(GlassCockpit), switch_twilight: Some(false) }
        );
        assert_eq!(st.used, 4);
        let sum = LoadSummary { st: &st, sha256: &[0; 32], bytes: 1, lines: 4 };
        let text = std::format!("{sum}");
        assert!(text.ends_with("; theme auto (day default, night glass-cockpit, switch sunrise)"), "{text}");
        assert!(std::format!("{}", ThemeSite::default()).starts_with("theme default"));
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
            "mute deauth",
            "mute jam_broadband mhz=2437.0",
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
        text += "\nmute evil_twin source=02:00:00:00:00:01 ssid=Last One"; // no final newline
        for piece in [1usize, 7, 512] {
            let (mut st, mut lines) = (SiteState::default(), Lines::default());
            let mut refused = std::vec::Vec::new();
            for chunk in text.as_bytes().chunks(piece) {
                read_into(&mut st, &mut lines, chunk, |n, e| refused.push((n, e)));
            }
            finish_into(&mut st, &mut lines, |n, e| refused.push((n, e)));
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

    fn load(text: &str) -> SiteState {
        let (mut st, mut lines) = (SiteState::default(), Lines::default());
        read_into(&mut st, &mut lines, text.as_bytes(), |_, _| {});
        finish_into(&mut st, &mut lines, |_, _| {});
        st
    }

    #[test]
    fn a_load_is_summarised_and_only_changes_are_listed() {
        let text = "node 10:bd:a3:ce:bd:18 39.9521 -75.1638\n\
                    mute evil_twin source=02:11:22:33:44:55 ssid=Cafe WiFi\n\
                    mute deauth\n\
                    control ids on\n";
        let st = load(text);
        let sha = [0xab; 32];
        let sum = LoadSummary { st: &st, sha256: &sha, bytes: text.len() as u32, lines: 4 };
        let s = std::format!("{sum}");
        assert!(s.starts_with("site file loaded: sha256 abababab"), "{s}");
        assert!(s.contains("1 mutes for good, 1 mute lines not honoured; 1 node positions; control ids on, control content off"), "{s}");
        assert_eq!(&sum.data()[32..], &[1, 1, 1, 1, 0]);
        // The first load after boot: everything is a change.
        let empty = SiteState::default();
        let mut seen = std::vec::Vec::new();
        assert_eq!(changes(&empty, &st, |c| seen.push(std::format!("{}", Text(c)))), 2);
        assert_eq!(
            seen[0],
            "site file changed: added mute for good (site file): Evil twin network; claimed source 02:11:22:33:44:55; claimed name \"Cafe WiFi\""
        );
        assert!(seen[1].contains("node 10:bd:a3:ce:bd:18 at 39.9521000 -75.1638000 (new)"), "{seen:?}");
        // The same file again: nothing changed.
        assert_eq!(changes(&st, &load(text), |_| panic!("no change")), 0);
        // An edited card: the node moved and the mute is gone.
        let edited = load("node 10:bd:a3:ce:bd:18 39.9600 -75.1638\ncontrol ids on\n");
        let mut seen = std::vec::Vec::new();
        assert_eq!(changes(&st, &edited, |c| seen.push(std::format!("{}", Text(c)))), 2);
        assert!(seen[0].starts_with("site file changed: removed mute for good"), "{seen:?}");
        assert!(seen[1].contains("(was 39.9521000 -75.1638000)"), "{seen:?}");
    }

    struct Text<'a>(Change<'a>);
    impl core::fmt::Display for Text<'_> {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write_change(f, &self.0)
        }
    }

    #[test]
    fn mute_text_says_what_and_how_long() {
        let mut s = std::string::String::new();
        let m = Mute { node: 3, until_s: 301, ..Mute::empty(ThreatKind::Blinded) };
        write_mute_text(&mut s, &m, 1_000).unwrap();
        assert_eq!(s, "snooze 5 min: Node blinded; node 3; ends at hub uptime 301 s");
        s.clear();
        let m = Mute {
            ssid: heapless::String::try_from("Cafe\x1b[2J").unwrap(),
            source: [2, 0, 0, 0, 0, 7],
            until_s: 7_201,
            ..Mute::empty(ThreatKind::EvilTwin)
        };
        write_mute_text(&mut s, &m, 1_000).unwrap();
        assert!(s.starts_with("mute 2 h: Evil twin network; claimed source 02:00:00:00:00:07; claimed name \"Cafe"), "{s}");
        assert!(!s.contains('\x1b'), "names from frames are never printed as received: {s}");
    }

    #[test]
    fn written_lines_read_back_the_same() {
        let mut m = Mute {
            ssid: heapless::String::try_from("Free Airport WiFi").unwrap(),
            source: [0xde, 0xad, 0xbe, 0xef, 0, 1],
            ..Mute::empty(ThreatKind::AttackTool)
        };
        let mut s: heapless::String<128> = heapless::String::new();
        write_mute(&mut s, &m).unwrap();
        assert_eq!(parse_line(&s), Ok(SiteLine::Mute(m.clone())));
        m = Mute { target: [0x12; 6], freq: 58_005, ..Mute::empty(ThreatKind::ControlContested) };
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
