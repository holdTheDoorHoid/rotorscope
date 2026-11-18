//! Attacks and jamming: what nodes report as [`link::Frame::Threat`]
//! (protocol 9), plus one check the hub makes itself.
//!
//! These are not drones. A deauth flood, beacon spam, an evil twin or a
//! jammer is something attacking the radios around the detector (or the
//! detector), or drowning out the bands drones use. They are kept in their
//! own table with their own ids and handed out once each by
//! [`Tracker::take_new_threat`] for an alert.
//!
//! **Same threat.** One kind from one node against one target is one threat
//! while it lasts; a jamming kind is also keyed by band (a jammer seen at
//! 2.4 and 5.8 GHz is two entries, which is itself telling).
//!
//! **Deaf nodes (hub-side check).** A node that suddenly hears almost
//! nothing it normally hears, while it still reports in, has probably been
//! drowned out: the hub raises [`ThreatKind::Blinded`] for it itself, so a
//! jammer the node cannot characterise still shows. See [`DeafWatch`].
//!
//! **Drone Wi-Fi hijacking** (`DroneDeauth`, `DroneTakeover`,
//! `DroneNetClone`, docs/DRONE-ATTACKS.md): one threat per drone network
//! (its BSSID) for the first two, whoever the attacker is, so a takeover by
//! a second newcomer updates the same entry (`source` follows the latest);
//! a copy of a drone's network is one per name and copy, like an evil twin.
//! The maker travels in `detail >> 24` and is shown as `maker`
//! ([`drone_maker`]).
//!
//! **Privacy.** Transmitter and target addresses (often a neighbour's access
//! point) are shown only with [`crate::Config::show_link_ids`], as for open
//! links. Network names are what any phone lists, and are shown. The
//! evidence log keeps the raw frames either way.

use heapless::Vec;
use link::{ThreatKind, ThreatSighting};
use serde::Serialize;

use crate::text::{FormatError, Mac};
use crate::{Context, Tracker};

/// Threats held at once.
pub const MAX_THREATS: usize = 16;
/// A threat not reported for this long has ended (it stays listed, as
/// "ended", until [`crate::Config::remove_after_ms`]).
pub const THREAT_ACTIVE_MS: u64 = 10_000;
/// Two jamming reports this close (100 kHz units, 300 MHz) are one band.
pub const SAME_JAM_BAND: u32 = 3_000;

/// Stable handle for a threat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct ThreatId(pub u32);

/// Short plain-English name of a kind, for the screen and alerts.
pub const fn threat_label(kind: ThreatKind) -> &'static str {
    match kind {
        ThreatKind::Deauth => "Wi-Fi deauth attack",
        ThreatKind::Disassoc => "Wi-Fi disassociation attack",
        ThreatKind::BeaconFlood => "Fake network flood",
        ThreatKind::EvilTwin => "Evil twin network",
        ThreatKind::Karma => "Rogue access point (Karma)",
        ThreatKind::ChannelSwitch => "Forced channel switch",
        ThreatKind::AuthFlood => "Wi-Fi login flood",
        ThreatKind::ProbeFlood => "Wi-Fi probe flood",
        ThreatKind::AttackTool => "Wi-Fi attack tool",
        ThreatKind::BleSpam => "Bluetooth spam",
        ThreatKind::JamBroadband => "Jamming (wideband)",
        ThreatKind::JamTone => "Jamming (tone)",
        ThreatKind::JamSweep => "Jamming (sweep)",
        ThreatKind::JamPulsed => "Jamming (pulsed)",
        ThreatKind::Blinded => "Node blinded",
        ThreatKind::ForgedLink => "Forged node traffic",
        ThreatKind::DroneDeauth => "Drone kicked off its Wi-Fi",
        ThreatKind::DroneTakeover => "Drone Wi-Fi taken over",
        ThreatKind::DroneNetClone => "Copy of a drone's network",
        ThreatKind::ControlContested => "Second controller on a link",
        ThreatKind::MavlinkAttack => "Hostile drone telemetry",
        ThreatKind::AreaGnss => "GPS interference nearby",
    }
}

/// Drone kinds carry the `wifidrone` maker index in `detail >> 24`.
pub const DRONE_MAKER_SHIFT: u32 = 24;

/// Whether `kind` is about a drone's own Wi-Fi network (maker in `detail`).
pub const fn is_drone_wifi(kind: ThreatKind) -> bool {
    matches!(kind, ThreatKind::DroneDeauth | ThreatKind::DroneTakeover | ThreatKind::DroneNetClone)
}

/// The drone maker a drone Wi-Fi threat is about (`wifidrone` name),
/// `None` for other kinds or a maker this build does not know.
pub fn drone_maker(t: &Threat) -> Option<&'static str> {
    if !is_drone_wifi(t.kind) {
        return None;
    }
    wifidrone::maker_name((t.detail >> DRONE_MAKER_SHIFT) as u8)
}

/// One threat.
#[derive(Debug, Clone, PartialEq)]
pub struct Threat {
    pub id: ThreatId,
    pub kind: ThreatKind,
    /// The node that reported it.
    pub node: u8,
    /// Raised by the hub itself (a deaf node), not reported by a node.
    pub by_hub: bool,
    pub channel: u8,
    /// Latest centre, 100 kHz units (0 if unknown).
    pub freq: u32,
    pub width_khz: u32,
    /// Latest level: RSSI dBm for frame kinds, dB above floor for jamming.
    pub level: i8,
    /// Strongest level seen.
    pub peak_level: i8,
    /// Frames (bursts, dwells) summed over every report.
    pub count: u64,
    /// Latest kind-specific number (see [`ThreatKind`]).
    pub detail: u32,
    pub source: [u8; 6],
    pub target: [u8; 6],
    pub ssid: heapless::String<32>,
    pub first_seen_ms: u64,
    pub last_seen_ms: u64,
    pub reports: u32,
    /// Matches one of the operator's mute rules: kept and logged, but no
    /// alert, no badge and not counted as jamming.
    pub muted: bool,
    pub(crate) announced: bool,
}

impl Threat {
    pub fn label(&self) -> &'static str {
        threat_label(self.kind)
    }

    pub fn freq_mhz(&self) -> f64 {
        f64::from(self.freq) / 10.0
    }

    /// Still being reported.
    pub fn active(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.last_seen_ms) < THREAT_ACTIVE_MS
    }

    fn same(&self, node: u8, by_hub: bool, s: &ThreatSighting) -> bool {
        if self.kind != s.kind || self.node != node || self.by_hub != by_hub {
            return false;
        }
        if s.kind.is_jamming() {
            return self.freq.abs_diff(s.freq) <= SAME_JAM_BAND;
        }
        match s.kind {
            // Name-centred kinds: one per network name (and newcomer).
            ThreatKind::EvilTwin | ThreatKind::DroneNetClone => self.ssid == s.ssid && self.source == s.source,
            ThreatKind::Karma => self.source == s.source,
            ThreatKind::ForgedLink => self.source == s.source,
            // One per link: the node's handle for it (`source`), its
            // identifier when sent (`target`), and the system (low byte of
            // `detail`). Without identifiers, two contested links on one
            // node still stay apart by handle.
            ThreatKind::ControlContested => {
                self.source == s.source && self.target == s.target && self.detail & 0xFF == s.detail & 0xFF
            }
            // One per claimed MAVLink sender and finding (`detail` is the
            // `mavwatch::Kind`; `source` the sender's system/component).
            ThreatKind::MavlinkAttack => self.source == s.source && self.detail == s.detail,
            _ => self.target == s.target,
        }
    }

    /// For [`ThreatKind::ControlContested`]: the system, transmitters and
    /// rules its `detail` encodes (`ctrlwatch::contested::Report::detail`).
    pub fn contested(&self) -> Option<ctrlwatch::contested::Detail> {
        (self.kind == ThreatKind::ControlContested).then(|| ctrlwatch::contested::decode_detail(self.detail))
    }
}

// ----------------------------------------------------------------- mutes --

/// Mute rules held. Each muted alarm takes one.
pub const MAX_MUTES: usize = 24;

/// An operator's "this alarm is a known false one" rule (for example a
/// venue that really runs an open network named like its secured one).
/// Zero / empty fields match anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mute {
    pub kind: ThreatKind,
    pub ssid: heapless::String<32>,
    pub source: [u8; 6],
    pub target: [u8; 6],
    /// Jamming only: centre, 100 kHz units; matches within
    /// [`SAME_JAM_BAND`]. 0 = any.
    pub freq: u32,
}

impl Mute {
    pub fn matches(&self, t: &Threat) -> bool {
        let mac = |rule: &[u8; 6], v: &[u8; 6]| *rule == [0; 6] || rule == v;
        self.kind == t.kind
            && (self.ssid.is_empty() || self.ssid == t.ssid)
            && mac(&self.source, &t.source)
            && mac(&self.target, &t.target)
            && (self.freq == 0 || self.freq.abs_diff(t.freq) <= SAME_JAM_BAND)
    }

    /// The narrowest rule that silences `t` and things exactly like it.
    pub fn for_threat(t: &Threat) -> Self {
        let mut m = Mute { kind: t.kind, ssid: heapless::String::new(), source: [0; 6], target: [0; 6], freq: 0 };
        if t.kind.is_jamming() {
            m.freq = t.freq;
            return m;
        }
        match t.kind {
            // Named things: the name and the transmitter.
            ThreatKind::EvilTwin | ThreatKind::Karma | ThreatKind::AttackTool | ThreatKind::DroneNetClone => {
                m.ssid = t.ssid.clone();
                m.source = t.source;
            }
            ThreatKind::ForgedLink => m.source = t.source,
            // A control link: its identifier when known (all zero without
            // `control ids`, so then every contested link) in that band
            // (900 MHz or 2.4 GHz). Not the node's handle: it changes.
            ThreatKind::ControlContested => {
                m.target = t.target;
                m.freq = t.freq;
            }
            // Hostile MAVLink: the sender (its system/component ID). The
            // rule has no `detail`, so it silences every finding from that
            // sender (the owner's own ground station using its console).
            ThreatKind::MavlinkAttack => m.source = t.source,
            // Floods: the victim (random sources change all the time).
            _ => m.target = t.target,
        }
        m
    }
}

#[derive(Debug, Clone, Default)]
pub struct ThreatTable {
    v: Vec<Threat, MAX_THREATS>,
    next_id: u32,
    mutes: Vec<Mute, MAX_MUTES>,
}

impl ThreatTable {
    pub const fn new() -> Self {
        Self { v: Vec::new(), next_id: 1, mutes: Vec::new() }
    }

    fn remute(&mut self) {
        for t in self.v.iter_mut() {
            t.muted = self.mutes.iter().any(|m| m.matches(t));
        }
    }

    pub fn all(&self) -> &[Threat] {
        &self.v
    }

    fn ingest(&mut self, node: u8, by_hub: bool, s: &ThreatSighting, now_ms: u64) -> ThreatId {
        let i = match self.v.iter().position(|t| t.same(node, by_hub, s)) {
            Some(i) => i,
            None => {
                // Make room: drop an ended threat first, else the oldest.
                if self.v.is_full() {
                    let ended = (0..self.v.len()).filter(|&i| !self.v[i].active(now_ms));
                    let i = ended
                        .min_by_key(|&i| self.v[i].last_seen_ms)
                        .or_else(|| (0..self.v.len()).min_by_key(|&i| self.v[i].last_seen_ms));
                    if let Some(i) = i {
                        self.v.remove(i);
                    }
                }
                let id = ThreatId(self.next_id.max(1));
                self.next_id = id.0.wrapping_add(1).max(1);
                let _ = self.v.push(Threat {
                    id,
                    kind: s.kind,
                    node,
                    by_hub,
                    channel: s.channel,
                    freq: s.freq,
                    width_khz: s.width_khz,
                    level: s.level,
                    peak_level: s.level,
                    count: 0,
                    detail: s.detail,
                    source: s.source,
                    target: s.target,
                    ssid: s.ssid.clone(),
                    first_seen_ms: now_ms,
                    last_seen_ms: now_ms,
                    reports: 0,
                    muted: false,
                    announced: false,
                });
                self.v.len() - 1
            }
        };
        let t = &mut self.v[i];
        t.channel = s.channel;
        if s.freq != 0 {
            t.freq = s.freq;
        }
        if s.width_khz != 0 {
            t.width_khz = s.width_khz;
        }
        t.level = s.level;
        t.peak_level = t.peak_level.max(s.level);
        t.count = t.count.saturating_add(u64::from(s.count));
        t.detail = s.detail;
        // Keyed by the drone, not the attacker: show the latest one.
        if matches!(s.kind, ThreatKind::DroneDeauth | ThreatKind::DroneTakeover) && s.source != [0; 6] {
            t.source = s.source;
        }
        if !s.ssid.is_empty() {
            t.ssid = s.ssid.clone();
        }
        t.last_seen_ms = t.last_seen_ms.max(now_ms);
        t.reports = t.reports.saturating_add(1);
        t.muted = self.mutes.iter().any(|m| m.matches(t));
        t.id
    }

    pub(crate) fn expire(&mut self, now_ms: u64, remove_after_ms: u64) {
        self.v.retain(|t| now_ms.saturating_sub(t.last_seen_ms) < remove_after_ms);
    }
}

// ------------------------------------------------------------ deaf nodes --

/// Nodes watched for going deaf.
pub const MAX_WATCHED: usize = 16;
/// A node must have been heard this long before its normal rate is trusted.
pub const DEAF_LEARN_MS: u64 = 60_000;
/// Below this normal rate (frames/s) a node is too quiet to judge: a quiet
/// place, not a jammer, can explain a drop.
pub const DEAF_MIN_NORMAL: f32 = 20.0;
/// Deaf: hearing less than this fraction of its normal rate...
pub const DEAF_FRACTION: f32 = 0.10;
/// ...for at least this long.
pub const DEAF_HOLD_MS: u64 = 10_000;
/// The normal rate follows the measured one with this weight per Stats
/// report (about a 30 s memory at one report a second), and only while the
/// node is not deaf, so a jammer is never learned as normal.
const DEAF_ALPHA: f32 = 1.0 / 30.0;

#[derive(Debug, Clone, Copy, PartialEq)]
struct Watched {
    node: u8,
    /// Node uptime and running frame count at the previous Stats report.
    last_uptime_us: u64,
    last_frames: u32,
    /// Hub time the current baseline started learning.
    since_ms: u64,
    normal: f32,
    /// Latest measured rate.
    rate: f32,
    /// Hub time the rate first fell below the deaf line (cleared on recovery).
    low_since: Option<u64>,
}

/// Watches each node's own "frames seen" counter (from its Stats reports)
/// and notices when it collapses: the hub-side [`ThreatKind::Blinded`].
///
/// A role or plan change, or a node reboot, restarts the learning, since
/// what is normal depends on what the node listens to.
#[derive(Debug, Clone, Default)]
pub struct DeafWatch {
    v: Vec<Watched, MAX_WATCHED>,
}

impl DeafWatch {
    pub const fn new() -> Self {
        Self { v: Vec::new() }
    }

    /// Forget what is normal for `node` (it changed role or plan).
    pub fn reset(&mut self, node: u8) {
        self.v.retain(|w| w.node != node);
    }

    /// Normal and current rate of `node`, frames/s, once learned.
    pub fn rates(&self, node: u8, now_ms: u64) -> Option<(f32, f32)> {
        let w = self.v.iter().find(|w| w.node == node)?;
        (now_ms.saturating_sub(w.since_ms) >= DEAF_LEARN_MS).then_some((w.normal, w.rate))
    }

    /// One Stats report. Returns the sighting to raise when the node has
    /// been deaf long enough (again every report while it stays deaf).
    pub fn stats(&mut self, node: u8, uptime_us: u64, frames_seen: u32, now_ms: u64) -> Option<ThreatSighting> {
        let Some(i) = self.v.iter().position(|w| w.node == node) else {
            if self.v.is_full() {
                self.v.remove(0);
            }
            let _ = self.v.push(Watched {
                node,
                last_uptime_us: uptime_us,
                last_frames: frames_seen,
                since_ms: now_ms,
                normal: 0.0,
                rate: 0.0,
                low_since: None,
            });
            return None;
        };
        let w = &mut self.v[i];
        // Rebooted (uptime went back) or counter reset: start again.
        if uptime_us <= w.last_uptime_us || frames_seen < w.last_frames {
            *w = Watched {
                node,
                last_uptime_us: uptime_us,
                last_frames: frames_seen,
                since_ms: now_ms,
                normal: 0.0,
                rate: 0.0,
                low_since: None,
            };
            return None;
        }
        let dt_s = (uptime_us - w.last_uptime_us) as f32 / 1e6;
        let rate = (frames_seen - w.last_frames) as f32 / dt_s.max(0.001);
        w.last_uptime_us = uptime_us;
        w.last_frames = frames_seen;
        w.rate = rate;
        let learned = now_ms.saturating_sub(w.since_ms) >= DEAF_LEARN_MS;
        let deaf_line = w.normal * DEAF_FRACTION;
        if learned && w.normal >= DEAF_MIN_NORMAL && rate < deaf_line {
            let since = *w.low_since.get_or_insert(now_ms);
            if now_ms.saturating_sub(since) >= DEAF_HOLD_MS {
                let pct = ((rate / w.normal) * 100.0).clamp(0.0, 100.0) as u32;
                return Some(ThreatSighting {
                    kind: ThreatKind::Blinded,
                    channel: 0,
                    freq: 0,
                    width_khz: 0,
                    level: 0,
                    count: 0,
                    window_ms: DEAF_HOLD_MS as u32,
                    source: [0; 6],
                    target: [0; 6],
                    detail: pct,
                    ssid: heapless::String::new(),
                    uptime_us,
                });
            }
            return None;
        }
        w.low_since = None;
        // Learn: quickly at first (plain average), then slowly.
        if w.normal == 0.0 {
            w.normal = rate;
        } else {
            w.normal += (rate - w.normal) * DEAF_ALPHA;
        }
        None
    }
}

// --------------------------------------------------------------- tracker --

impl<const N: usize, const A: usize> Tracker<N, A> {
    /// Feed one threat reported by `node`.
    pub fn ingest_threat(&mut self, node: u8, s: &ThreatSighting, now_ms: u64) -> ThreatId {
        self.expire(now_ms);
        self.threats.ingest(node, false, s, now_ms)
    }

    /// Feed a node's Stats report to the deaf-node check; a deaf node
    /// becomes a hub-raised [`ThreatKind::Blinded`] threat.
    pub fn ingest_node_stats(&mut self, node: u8, uptime_us: u64, frames_seen: u32, now_ms: u64) -> Option<ThreatId> {
        let s = self.deaf.stats(node, uptime_us, frames_seen, now_ms)?;
        self.expire(now_ms);
        Some(self.threats.ingest(node, true, &s, now_ms))
    }

    /// Raise (or refresh) a threat the hub found itself from what it holds,
    /// rather than one a node reported: `by_hub`, node 0. Used by checks
    /// that live outside the tracker (`area_gnss::AreaWatch`).
    pub fn ingest_hub_threat(&mut self, s: &ThreatSighting, now_ms: u64) -> ThreatId {
        self.expire(now_ms);
        self.threats.ingest(0, true, s, now_ms)
    }

    /// Threats, in table order.
    pub fn threats(&self) -> &[Threat] {
        self.threats.all()
    }

    pub fn get_threat(&self, id: ThreatId) -> Option<&Threat> {
        self.threats.all().iter().find(|t| t.id == id)
    }

    /// Threats still being reported, muted ones left out.
    pub fn active_threats(&self, now_ms: u64) -> impl Iterator<Item = &Threat> {
        self.threats.all().iter().filter(move |t| t.active(now_ms) && !t.muted)
    }

    /// Add a mute rule (ignored if already there or the list is full;
    /// returns whether it was added).
    pub fn mute(&mut self, m: Mute) -> bool {
        let tt = &mut self.threats;
        let added = !tt.mutes.contains(&m) && tt.mutes.push(m).is_ok();
        tt.remute();
        added
    }

    /// Mute every alarm active right now; returns the rules added (for the
    /// hub to save).
    pub fn mute_active(&mut self, now_ms: u64) -> Vec<Mute, MAX_THREATS> {
        let rules: Vec<Mute, MAX_THREATS> = self.active_threats(now_ms).map(Mute::for_threat).collect();
        let mut added = Vec::new();
        for m in rules {
            if self.mute(m.clone()) {
                let _ = added.push(m);
            }
        }
        added
    }

    /// Drop every mute rule.
    pub fn unmute_all(&mut self) {
        self.threats.mutes.clear();
        self.threats.remute();
    }

    pub fn mutes(&self) -> &[Mute] {
        &self.threats.mutes
    }

    /// Whether any node is jammed or blinded right now: the detector may be
    /// missing drones, so "nothing seen" must not read as "all clear".
    pub fn partly_blind(&self, now_ms: u64) -> bool {
        self.active_threats(now_ms).any(|t| t.kind.is_jamming())
    }

    /// The next threat to alert on, exactly once each.
    /// Muted threats are never handed out.
    pub fn take_new_threat(&mut self, _now_ms: u64) -> Option<ThreatId> {
        loop {
            let t = self.threats.v.iter_mut().find(|t| !t.announced)?;
            t.announced = true;
            if !t.muted {
                return Some(t.id);
            }
        }
    }

    /// Normal and current frame rate of a node (frames/s), once learned.
    pub fn node_rates(&self, node: u8, now_ms: u64) -> Option<(f32, f32)> {
        self.deaf.rates(node, now_ms)
    }
}

// ------------------------------------------------------------------ json --

#[derive(Serialize)]
struct ThreatLine<'a> {
    t: &'static str,
    id: ThreatId,
    kind: &'static str,
    label: &'static str,
    jamming: bool,
    active: bool,
    muted: bool,
    by_hub: bool,
    node: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    ch: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mhz: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    width_khz: Option<u32>,
    level: i8,
    peak_level: i8,
    count: u64,
    detail: u32,
    /// Whether this unit shows transmitter addresses (`show_link_ids`).
    ids_shown: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    src: Option<Mac<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<Mac<'a>>,
    #[serde(skip_serializing_if = "str::is_empty")]
    ssid: &'a str,
    /// Contested control link: the system ("ExpressLRS"), how many
    /// transmitters the evidence needs, and the rule that fired.
    #[serde(skip_serializing_if = "Option::is_none")]
    ctrl_system: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transmitters: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rule: Option<&'static str>,
    /// Drone Wi-Fi kinds: the drone's maker (`wifidrone`).
    #[serde(skip_serializing_if = "Option::is_none")]
    maker: Option<&'static str>,
    n: u32,
    first_ms: u64,
    last_ms: u64,
    age_ms: u64,
}

/// A buffer this size always holds one threat line (see the test).
pub const MAX_THREAT_LINE: usize = 576;
const _: () = assert!(MAX_THREAT_LINE <= crate::json::MAX_LINE);

/// Write one threat as a JSON object followed by `\n` (`"t":"threat"`).
pub fn threat_line(t: &Threat, ctx: &Context, buf: &mut [u8]) -> Result<usize, FormatError> {
    let show = ctx.config.show_link_ids;
    let known = |m: &[u8; 6]| *m != [0; 6];
    let line = ThreatLine {
        t: "threat",
        id: t.id,
        kind: t.kind.name(),
        label: t.label(),
        jamming: t.kind.is_jamming(),
        active: t.active(ctx.now_ms),
        muted: t.muted,
        by_hub: t.by_hub,
        node: t.node,
        ch: (t.channel != 0).then_some(t.channel),
        mhz: (t.freq != 0).then_some(t.freq_mhz()),
        width_khz: (t.width_khz != 0).then_some(t.width_khz),
        level: t.level,
        peak_level: t.peak_level,
        count: t.count,
        detail: t.detail,
        ids_shown: show,
        src: (show && known(&t.source)).then_some(Mac(&t.source)),
        target: (show && known(&t.target)).then_some(Mac(&t.target)),
        ssid: t.ssid.as_str(),
        ctrl_system: t.contested().and_then(|d| d.system).map(|s| s.label()),
        transmitters: t.contested().map(|d| d.transmitters),
        rule: t.contested().map(|d| ctrlwatch::contested::rule_name(d.rules)),
        maker: drone_maker(t),
        n: t.reports,
        first_ms: t.first_seen_ms,
        last_ms: t.last_seen_ms,
        age_ms: ctx.now_ms.saturating_sub(t.last_seen_ms),
    };
    let n = serde_json_core::to_slice(&line, buf).map_err(|_| FormatError::BufferTooSmall)?;
    let nl = buf.get_mut(n).ok_or(FormatError::BufferTooSmall)?;
    *nl = b'\n';
    Ok(n + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;

    fn sighting(kind: ThreatKind, target: u8, freq: u32) -> ThreatSighting {
        ThreatSighting {
            kind,
            channel: 6,
            freq,
            width_khz: 0,
            level: -40,
            count: 50,
            window_ms: 1000,
            source: [0x02, 1, 2, 3, 4, 5],
            target: [target; 6],
            detail: 7,
            ssid: heapless::String::try_from("x".repeat(32).as_str()).unwrap(),
            uptime_us: 1,
        }
    }

    #[test]
    fn drone_wifi_threats_are_per_drone() {
        let mut tr: Tracker = Tracker::default();
        let drone = [0x60, 0x60, 0x1F, 1, 2, 3];
        let tello = |kind, source: [u8; 6], ssid: &str| ThreatSighting {
            kind,
            channel: 6,
            freq: 24370,
            width_khz: 20_000,
            level: -50,
            count: 3,
            window_ms: 1000,
            source,
            target: drone,
            detail: (1 << DRONE_MAKER_SHIFT) | 7,
            ssid: heapless::String::try_from(ssid).unwrap(),
            uptime_us: 1,
        };
        let a = [0x00, 0xC0, 0xCA, 1, 1, 1];
        let b = [0x00, 0xC0, 0xCA, 2, 2, 2];
        // Deauth and takeover: one each per drone, the latest attacker shown.
        let d1 = tr.ingest_threat(1, &tello(ThreatKind::DroneDeauth, a, "TELLO-010203"), 1_000);
        let d2 = tr.ingest_threat(1, &tello(ThreatKind::DroneDeauth, b, "TELLO-010203"), 2_000);
        assert_eq!(d1, d2);
        let t1 = tr.ingest_threat(1, &tello(ThreatKind::DroneTakeover, a, "TELLO-010203"), 2_000);
        let t2 = tr.ingest_threat(1, &tello(ThreatKind::DroneTakeover, b, "TELLO-010203"), 3_000);
        assert_eq!(t1, t2);
        assert_ne!(d1, t1);
        let t = tr.get_threat(t1).unwrap();
        assert_eq!(t.source, b);
        assert_eq!(t.count, 6);
        assert_eq!(drone_maker(t), Some("DJI Tello"));
        assert_eq!(t.label(), "Drone Wi-Fi taken over");
        // Copies: one per name and copy.
        let c1 = tr.ingest_threat(1, &tello(ThreatKind::DroneNetClone, a, "TELLO-010203"), 3_000);
        let c2 = tr.ingest_threat(1, &tello(ThreatKind::DroneNetClone, b, "TELLO-010203"), 3_000);
        let c3 = tr.ingest_threat(1, &tello(ThreatKind::DroneNetClone, a, "TELLO-010203"), 4_000);
        assert_ne!(c1, c2);
        assert_eq!(c1, c3);
        assert_eq!(tr.get_threat(c1).unwrap().source, a);
        // Every one is announced once.
        let mut new = Vec::<ThreatId, 8>::new();
        while let Some(id) = tr.take_new_threat(4_000) {
            new.push(id).unwrap();
        }
        assert_eq!(new.len(), 4);
        // Muting a drone's takeover silences that drone only.
        let m = Mute::for_threat(tr.get_threat(t1).unwrap());
        assert_eq!((m.target, m.source), (drone, [0; 6]));
        // JSON line names the maker.
        let ctx = Context { now_ms: 4_000, utc_ms: None, own: None, config: crate::Config::default() };
        let mut buf = [0u8; MAX_THREAT_LINE];
        let n = threat_line(tr.get_threat(t1).unwrap(), &ctx, &mut buf).unwrap();
        let text = core::str::from_utf8(&buf[..n]).unwrap();
        assert!(text.contains("\"maker\":\"DJI Tello\""), "{text}");
        assert!(text.contains("\"kind\":\"drone_takeover\""), "{text}");
        // Other kinds have no maker, whatever their detail says.
        let mut plain = tello(ThreatKind::Deauth, a, "");
        plain.target = [0xFF; 6];
        let p = tr.ingest_threat(1, &plain, 4_000);
        let n = threat_line(tr.get_threat(p).unwrap(), &ctx, &mut buf).unwrap();
        assert!(!core::str::from_utf8(&buf[..n]).unwrap().contains("maker"));
        // The longest drone line still fits.
        let mut big = tello(ThreatKind::DroneNetClone, [0x02, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF], "");
        big.ssid = heapless::String::try_from("\"".repeat(32).as_str()).unwrap();
        big.detail = (6 << DRONE_MAKER_SHIFT) | 0xFF_FFFF;
        big.target = [0xFF; 6];
        big.channel = u8::MAX;
        big.freq = u32::MAX;
        big.width_khz = u32::MAX;
        big.count = u32::MAX;
        big.level = i8::MIN;
        let id = tr.ingest_threat(u8::MAX, &big, u64::MAX / 4);
        let config = crate::Config { show_link_ids: true, ..Default::default() };
        let ctx = Context { now_ms: u64::MAX / 2, utc_ms: None, own: None, config };
        let n = threat_line(tr.get_threat(id).unwrap(), &ctx, &mut buf).unwrap();
        assert!(n > 512, "{n}: the reason MAX_THREAT_LINE grew from 512");
        assert!(core::str::from_utf8(&buf[..n]).unwrap().contains("\"maker\":\"Holy Stone\""));
    }

    #[test]
    fn same_threat_merges_and_alerts_once() {
        let mut tr: Tracker = Tracker::default();
        let a = tr.ingest_threat(1, &sighting(ThreatKind::Deauth, 0xFF, 0), 1_000);
        let b = tr.ingest_threat(1, &sighting(ThreatKind::Deauth, 0xFF, 0), 2_000);
        assert_eq!(a, b);
        assert_eq!(tr.threats()[0].count, 100);
        // Another target, another node, another kind: separate.
        tr.ingest_threat(1, &sighting(ThreatKind::Deauth, 0xAA, 0), 2_000);
        tr.ingest_threat(2, &sighting(ThreatKind::Deauth, 0xFF, 0), 2_000);
        tr.ingest_threat(1, &sighting(ThreatKind::Disassoc, 0xFF, 0), 2_000);
        assert_eq!(tr.threats().len(), 4);
        let mut alerts = 0;
        while tr.take_new_threat(3_000).is_some() {
            alerts += 1;
        }
        assert_eq!(alerts, 4);
        tr.ingest_threat(1, &sighting(ThreatKind::Deauth, 0xFF, 0), 4_000);
        assert!(tr.take_new_threat(4_000).is_none());
    }

    #[test]
    fn jamming_is_keyed_by_band_and_marks_partly_blind() {
        let mut tr: Tracker = Tracker::default();
        assert!(!tr.partly_blind(0));
        tr.ingest_threat(3, &sighting(ThreatKind::JamBroadband, 0, 24_370), 1_000);
        tr.ingest_threat(3, &sighting(ThreatKind::JamBroadband, 0, 24_620), 1_500);
        tr.ingest_threat(3, &sighting(ThreatKind::JamBroadband, 0, 57_800), 1_500);
        assert_eq!(tr.threats().len(), 2);
        assert!(tr.partly_blind(2_000));
        assert!(!tr.partly_blind(1_500 + THREAT_ACTIVE_MS));
        // A deauth flood alone does not make the detector blind.
        let mut tr: Tracker = Tracker::default();
        tr.ingest_threat(3, &sighting(ThreatKind::Deauth, 0xFF, 0), 1_000);
        assert!(!tr.partly_blind(1_000));
    }

    #[test]
    fn table_full_drops_ended_first_and_expires() {
        let mut tr: Tracker = Tracker::default();
        tr.ingest_threat(1, &sighting(ThreatKind::Deauth, 0, 0), 0);
        for i in 1..MAX_THREATS as u8 {
            tr.ingest_threat(1, &sighting(ThreatKind::Deauth, i, 0), 20_000);
        }
        tr.ingest_threat(1, &sighting(ThreatKind::Deauth, 0xEE, 0), 20_000);
        assert_eq!(tr.threats().len(), MAX_THREATS);
        assert!(tr.threats().iter().all(|t| t.target != [0; 6]));
        let remove = Config::default().remove_after_ms;
        tr.ingest_threat(9, &sighting(ThreatKind::JamTone, 0, 58_000), 20_000 + remove);
        assert_eq!(tr.threats().len(), 1);
    }

    #[test]
    fn deaf_node_raises_blinded_only_after_learning() {
        let mut tr: Tracker = Tracker::default();
        let mut frames = 0u32;
        let mut t = 0u64;
        // 90 s at 200 frames/s: learned.
        for _ in 0..90 {
            t += 1_000;
            frames += 200;
            assert!(tr.ingest_node_stats(4, t * 1_000, frames, t).is_none());
        }
        let (normal, _) = tr.node_rates(4, t).unwrap();
        assert!((normal - 200.0).abs() < 1.0);
        // Collapse to 5 frames/s: nothing for 10 s, then Blinded.
        let mut raised = None;
        for s in 0..15 {
            t += 1_000;
            frames += 5;
            if let Some(id) = tr.ingest_node_stats(4, t * 1_000, frames, t) {
                raised.get_or_insert((s, id));
            }
        }
        let (after, id) = raised.unwrap();
        assert_eq!(after, 10);
        let th = tr.get_threat(id).unwrap();
        assert_eq!(th.kind, ThreatKind::Blinded);
        assert!(th.by_hub && th.detail <= 3);
        assert!(tr.partly_blind(t));
        // The collapse was not learned as normal.
        assert!(tr.node_rates(4, t).unwrap().0 > 190.0);
    }

    #[test]
    fn quiet_or_new_or_rebooted_nodes_are_not_deaf() {
        let mut tr: Tracker = Tracker::default();
        // A quiet place: 5 frames/s normally, then 0.
        let (mut f, mut t) = (0u32, 0u64);
        for i in 0..200 {
            t += 1_000;
            f += if i < 100 { 5 } else { 0 };
            assert!(tr.ingest_node_stats(1, t * 1_000, f, t).is_none());
        }
        // A busy node that collapses before 60 s of learning.
        let (mut f, mut t) = (0u32, 0u64);
        for i in 0..60 {
            t += 1_000;
            f += if i < 30 { 300 } else { 0 };
            assert!(tr.ingest_node_stats(2, t * 1_000, f, t).is_none());
        }
        // A node that reboots: uptime and counters restart, no alarm.
        let (mut f, mut t) = (0u32, 0u64);
        for _ in 0..90 {
            t += 1_000;
            f += 300;
            tr.ingest_node_stats(3, t * 1_000, f, t);
        }
        for i in 0..30u64 {
            t += 1_000;
            assert!(tr.ingest_node_stats(3, (i + 1) * 1_000_000, 0, t).is_none());
        }
        assert!(tr.threats().is_empty());
    }

    /// Hostile MAVLink (protocol 13, `crates/mavwatch`): one threat per
    /// claimed sender and finding kind; a mute silences that sender.
    #[test]
    fn mavlink_attack_per_sender_and_finding() {
        let mut tr: Tracker = Tracker::default();
        let mav = |sys: u8, comp: u8, detail: u32| {
            let mut s = sighting(ThreatKind::MavlinkAttack, 0, 9_150);
            s.source = [0, 0, 0, 0, sys, comp];
            s.target = [0, 0, 0, 0, 1, 1];
            s.detail = detail;
            s.ssid = heapless::String::try_from("dev 10 n 7: reboot?").unwrap();
            s
        };
        let a = tr.ingest_threat(4, &mav(255, 190, 13), 1_000);
        assert_eq!(tr.ingest_threat(4, &mav(255, 190, 13), 3_000), a);
        let b = tr.ingest_threat(4, &mav(255, 190, 16), 3_000);
        let c = tr.ingest_threat(4, &mav(1, 100, 1), 3_000);
        assert!(a != b && b != c && a != c);
        assert_eq!(tr.get_threat(a).unwrap().label(), "Hostile drone telemetry");
        let ctx = Context { now_ms: 3_000, utc_ms: None, own: None, config: Config::default() };
        let mut buf = [0u8; MAX_THREAT_LINE];
        let n = threat_line(tr.get_threat(a).unwrap(), &ctx, &mut buf).unwrap();
        let text = core::str::from_utf8(&buf[..n]).unwrap();
        assert!(text.contains("\"kind\":\"mavlink_attack\"") && text.contains("\"detail\":13"), "{text}");
        // Muting the owner's own console silences that sender only.
        assert!(tr.mute(Mute::for_threat(tr.get_threat(a).unwrap())));
        assert_eq!(tr.active_threats(3_000).count(), 1);
    }

    #[test]
    fn muted_alarms_stay_listed_but_quiet() {
        let mut tr: Tracker = Tracker::default();
        let mut twin = sighting(ThreatKind::EvilTwin, 0, 0);
        twin.ssid = heapless::String::try_from("CafeWiFi").unwrap();
        tr.ingest_threat(1, &twin, 1_000);
        tr.ingest_threat(2, &sighting(ThreatKind::JamBroadband, 0, 24_370), 1_000);
        assert!(tr.take_new_threat(1_000).is_some());
        assert!(tr.take_new_threat(1_000).is_some());
        assert!(tr.partly_blind(1_000));
        let added = tr.mute_active(1_000);
        assert_eq!(added.len(), 2);
        assert_eq!(tr.active_threats(1_000).count(), 0);
        assert!(!tr.partly_blind(1_000));
        assert_eq!(tr.threats().len(), 2, "kept for the record");
        // The same twin again: still muted, no new alert. A different name
        // or jamming on the other band is not.
        tr.ingest_threat(3, &twin, 2_000);
        assert!(tr.take_new_threat(2_000).is_none());
        let mut other = twin.clone();
        other.ssid = heapless::String::try_from("Other").unwrap();
        tr.ingest_threat(1, &other, 2_000);
        tr.ingest_threat(2, &sighting(ThreatKind::JamBroadband, 0, 58_000), 2_000);
        assert_eq!(tr.active_threats(2_000).count(), 2);
        assert!(tr.mute_active(2_000).len() == 2 && tr.mute_active(2_000).is_empty(), "no duplicate rules");
        tr.unmute_all();
        assert_eq!(tr.active_threats(2_000).count(), 5, "the twin heard by node 3 too");
        assert!(tr.mutes().is_empty());
    }

    #[test]
    fn contested_links_stay_apart_and_mute_by_band() {
        use ctrlwatch::contested::{RULE_PHASE, encode_detail};
        use ctrlwatch::{Region, System};
        let mut tr: Tracker = Tracker::default();
        let mut s = sighting(ThreatKind::ControlContested, 0, 9_155);
        s.target = [0; 6];
        s.source = [0, 0, 0, 0, 0, 1];
        s.detail = encode_detail(System::ExpressLrs, 2, RULE_PHASE, Region::Fcc915);
        tr.ingest_threat(4, &s, 1_000);
        tr.ingest_threat(4, &s, 2_000);
        // Another link on the same node: another handle.
        let mut s2 = s.clone();
        s2.source[5] = 2;
        tr.ingest_threat(4, &s2, 2_000);
        // Same handle, other system (the handle was reused): apart too.
        let mut s3 = s.clone();
        s3.detail = encode_detail(System::Sik, 3, RULE_PHASE, Region::Fcc915);
        tr.ingest_threat(4, &s3, 2_000);
        assert_eq!(tr.threats().len(), 3);
        let t = &tr.threats()[0];
        let d = t.contested().unwrap();
        assert_eq!((d.system, d.transmitters), (Some(System::ExpressLrs), 2));
        let ctx = Context { now_ms: 2_000, utc_ms: None, own: None, config: Config::default() };
        let mut buf = [0u8; MAX_THREAT_LINE];
        let n = threat_line(t, &ctx, &mut buf).unwrap();
        let text = core::str::from_utf8(&buf[..n]).unwrap();
        assert!(text.contains("\"ctrl_system\":\"ExpressLRS\",\"transmitters\":2,\"rule\":\"interleaved timing\""), "{text}");
        // Mute: the band (and the identifier, all zero here); a 2.4 GHz
        // contested link is not muted by it.
        let m = Mute::for_threat(t);
        assert_eq!((m.freq, m.source, m.target), (9_155, [0; 6], [0; 6]));
        let mut far = t.clone();
        far.freq = 24_404;
        assert!(m.matches(&tr.threats()[1]) && !m.matches(&far));
        // Other kinds carry no contested fields.
        let n = threat_line(&{
            let mut x = t.clone();
            x.kind = ThreatKind::Deauth;
            x
        }, &ctx, &mut buf).unwrap();
        assert!(!core::str::from_utf8(&buf[..n]).unwrap().contains("transmitters"));
    }

    #[test]
    fn longest_line_fits_and_hides_addresses() {
        let mut tr: Tracker = Tracker::default();
        let mut s = sighting(ThreatKind::EvilTwin, 0xFF, u32::MAX);
        s.width_khz = u32::MAX;
        s.detail = u32::MAX;
        s.count = u32::MAX;
        s.level = i8::MIN;
        tr.ingest_threat(u8::MAX, &s, u64::MAX / 4);
        let config = Config { show_link_ids: true, ..Default::default() };
        let ctx = Context { now_ms: u64::MAX / 2, utc_ms: None, own: None, config };
        let mut buf = [0u8; MAX_THREAT_LINE];
        let n = threat_line(&tr.threats()[0], &ctx, &mut buf).unwrap();
        let text = core::str::from_utf8(&buf[..n]).unwrap();
        assert!(text.contains("\"src\":\"02:01:02:03:04:05\""), "{text}");
        assert!(text.contains("\"kind\":\"evil_twin\""));
        let ctx = Context { config: Config::default(), ..ctx };
        let n = threat_line(&tr.threats()[0], &ctx, &mut buf).unwrap();
        let text = core::str::from_utf8(&buf[..n]).unwrap();
        assert!(!text.contains("src") && !text.contains("\"target\""), "{text}");
        assert!(text.contains("\"ids_shown\":false"));
    }
}
