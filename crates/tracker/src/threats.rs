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
//! 2.4 and 5.8 GHz is two entries, which is itself telling). **Flood kinds**
//! ([`is_flood_kind`]: deauth, disassociation, beacon, login, probe and
//! Bluetooth floods, forced channel switches) are one threat per kind and
//! reporting node, whatever the target ([`Threat::per_node`]; `target` is
//! the latest): an attacker who aims at 20 victims is one attack, not 20
//! table entries. A report that matches a mute rule stays apart, per target
//! as before, so mutes keep working.
//!
//! **Room** (WP-09). The table holds [`MAX_THREATS`]. When it is full a new
//! threat makes room by removing, in order: an ended muted one, an ended
//! one, an active muted one, an active flood (oldest first). Floods may
//! hold at most [`MAX_FLOOD_THREATS`] entries, so there is always room for
//! the rest. An active jamming, Blinded, AreaGnss or ControlContested
//! threat ([`is_reserved_kind`]) is never removed for anything else, and the
//! last active jamming one never at all: [`Tracker::partly_blind`] cannot go
//! false because the table was busy. A newcomer that finds nothing it may
//! remove is dropped (and counted in [`Tracker::threats_dropped`]).
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
/// Of those, at most this many floods ([`is_flood_kind`]).
pub const MAX_FLOOD_THREATS: usize = 10;

/// Kinds that are floods of frames at some victim: aggregated per reporting
/// node, and the first to make room.
pub const fn is_flood_kind(kind: ThreatKind) -> bool {
    matches!(
        kind,
        ThreatKind::Deauth
            | ThreatKind::Disassoc
            | ThreatKind::BeaconFlood
            | ThreatKind::AuthFlood
            | ThreatKind::ProbeFlood
            | ThreatKind::BleSpam
            | ThreatKind::ChannelSwitch
    )
}

/// Kinds whose active entries are never removed to make room for anything
/// of another kind: jamming and Blinded (the detector may be missing drones),
/// area GPS interference and a contested control link.
pub fn is_reserved_kind(kind: ThreatKind) -> bool {
    kind.is_jamming() || matches!(kind, ThreatKind::AreaGnss | ThreatKind::ControlContested)
}
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
        ThreatKind::IdFlood => "Flood of fake IDs",
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
    /// Silenced: matches a mute rule (an operator's timed mute, a snooze,
    /// or a site-file mute). Kept and logged, but no alert and no badge. It
    /// still counts as jamming: [`Tracker::partly_blind`] ignores mutes.
    pub muted: bool,
    /// Silenced by a snooze (a safety-critical kind, [`SNOOZE_MS`]).
    pub snoozed: bool,
    /// Hub uptime seconds the silence ends (0: not silenced, or for good).
    pub silenced_until_s: u32,
    /// A flood kind kept as one entry per reporting node: `target` is the
    /// latest victim, `count` and `reports` add up every victim's.
    pub per_node: bool,
    pub(crate) announced: bool,
}

impl Threat {
    pub fn label(&self) -> &'static str {
        if self.suspect() {
            return "Possible hostile drone telemetry (one frame)";
        }
        if self.mavlink_over_wifi() {
            return "Hostile drone telemetry over Wi-Fi";
        }
        if let Some(t) = crate::probation::flood_table(self) {
            return t.label();
        }
        threat_label(self.kind)
    }

    /// A `MavlinkAttack` that is still one uncorroborated frame
    /// ([`link::MAVLINK_SUSPECT`]): listed, but not alerted on until the
    /// node reports it again without the bit.
    pub fn suspect(&self) -> bool {
        self.kind == ThreatKind::MavlinkAttack && self.detail & link::MAVLINK_SUSPECT != 0
    }

    /// A `MavlinkAttack` found on open Wi-Fi (protocol 13: a Wi-Fi node
    /// sets `channel`; an LR2021 node's SiK / mLRS finding has 0).
    pub fn mavlink_over_wifi(&self) -> bool {
        self.kind == ThreatKind::MavlinkAttack && self.channel != 0
    }

    /// Which radio a `MavlinkAttack` came over: `"wifi"` or `"radio"`
    /// (an LR2021 node: SiK, mLRS). `None` for other kinds.
    pub fn via(&self) -> Option<&'static str> {
        (self.kind == ThreatKind::MavlinkAttack).then_some(if self.mavlink_over_wifi() { "wifi" } else { "radio" })
    }

    pub fn freq_mhz(&self) -> f64 {
        f64::from(self.freq) / 10.0
    }

    /// Still being reported.
    pub fn active(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.last_seen_ms) < THREAT_ACTIVE_MS
    }

    /// Hub ms the silence on it ends (`None`: not silenced, or for good).
    pub fn silenced_until_ms(&self) -> Option<u64> {
        (self.muted && self.silenced_until_s != 0).then(|| u64::from(self.silenced_until_s) * 1000)
    }

    fn same(&self, node: u8, by_hub: bool, per_node: bool, s: &ThreatSighting) -> bool {
        if self.kind != s.kind || self.node != node || self.by_hub != by_hub || self.per_node != per_node {
            return false;
        }
        if per_node {
            return true;
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
            // One per claimed MAVLink sender and finding (`detail` low byte
            // is the `mavwatch::Kind`, bit 24 the suspicion flag; `source`
            // the sender's system/component).
            ThreatKind::MavlinkAttack => self.source == s.source && self.detail & 0xFFFF == s.detail & 0xFFFF,
            // One per table (drones, aircraft, control links).
            ThreatKind::IdFlood => self.detail >> 24 == s.detail >> 24,
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

/// Mute rules held: the operator's timed mutes and snoozes, and the site
/// file's mutes for good.
pub const MAX_MUTES: usize = 24;
/// How long an operator's mute of a nuisance alarm lasts: two hours, about
/// one session at a venue. Long enough that a nuisance silenced once stays
/// quiet through the event; short enough that a mute tapped in a hurry (or
/// under a burst of nuisance alarms raised to provoke it) does not outlive
/// the shift. A mute never survives a reboot either. A false alarm the
/// venue always has belongs in the site file instead (one explicit line).
pub const OPERATOR_MUTE_MS: u64 = 2 * 60 * 60 * 1000;
/// How long a snooze of a safety-critical alarm ([`is_safety_kind`]) lasts:
/// five minutes, time to deal with it, short enough that a jammer that is
/// still on is announced again. A snooze only stops the alert: the status
/// bar still says JAMMED (and AREA / FLOOD), and [`Tracker::partly_blind`]
/// still holds.
pub const SNOOZE_MS: u64 = 5 * 60 * 1000;
/// A snoozed jammer is the same one within this much of its centre
/// frequency (100 kHz units: 10 MHz), not the whole band.
pub const MUTE_JAM_TOL: u32 = 100;
/// [`Mute::node`] of a rule that names no node.
pub const NODE_ANY: u8 = 0xFF;

/// Kinds that say the hub may be missing drones, or that its picture is
/// being forged: jamming and Blinded (a node is deaf), GPS interference in
/// the area, a flood of fake IDs, forged node traffic. They can only be
/// snoozed for [`SNOOZE_MS`], never muted, and never from the site file.
pub fn is_safety_kind(kind: ThreatKind) -> bool {
    kind.is_jamming() || matches!(kind, ThreatKind::AreaGnss | ThreatKind::IdFlood | ThreatKind::ForgedLink)
}

/// Which fields of a rule must equal the threat's (see [`key_of`]).
pub mod key {
    pub const NODE: u8 = 1;
    pub const SSID: u8 = 2;
    pub const SOURCE: u8 = 4;
    pub const TARGET: u8 = 8;
    /// Centre within [`super::MUTE_JAM_TOL`].
    pub const FREQ: u8 = 16;
    /// Same band (within [`super::SAME_JAM_BAND`]: 900 MHz vs 2.4 GHz).
    pub const BAND: u8 = 32;
    /// [`super::Mute::sub`]: the flood's table, the contested link's system.
    pub const SUB: u8 = 64;
}

/// The fields that make one alarm of `kind` "this one" (`no_target`: the
/// rule's target is all zero). Every field in the key must match exactly;
/// zero means zero, never "any".
///
/// | kind | key |
/// |---|---|
/// | Blinded | node |
/// | jamming | node, centre +/- 10 MHz |
/// | AreaGnss | target (1 jamming, 2 spoofing) |
/// | IdFlood | table, node (0 for the hub-wide tables; the shedding node) |
/// | ForgedLink | node, source |
/// | EvilTwin, DroneNetClone, AttackTool | network name, source |
/// | Karma, MavlinkAttack | source |
/// | DroneDeauth, DroneTakeover | target (the drone's network) |
/// | ControlContested | band, link ID; without IDs also node, handle, system |
/// | floods | target; or, for a node's aggregate entry, the node |
pub fn key_of(kind: ThreatKind, no_target: bool) -> u8 {
    use key::*;
    if kind == ThreatKind::Blinded {
        return NODE;
    }
    if kind.is_jamming() {
        return NODE | FREQ;
    }
    match kind {
        ThreatKind::AreaGnss => TARGET,
        ThreatKind::IdFlood => SUB | NODE,
        ThreatKind::ForgedLink => NODE | SOURCE,
        ThreatKind::EvilTwin | ThreatKind::DroneNetClone | ThreatKind::AttackTool => SSID | SOURCE,
        ThreatKind::Karma | ThreatKind::MavlinkAttack => SOURCE,
        ThreatKind::DroneDeauth | ThreatKind::DroneTakeover => TARGET,
        ThreatKind::ControlContested if no_target => BAND | TARGET | NODE | SOURCE | SUB,
        ThreatKind::ControlContested => BAND | TARGET,
        _ if no_target => NODE,
        _ => TARGET,
    }
}

/// [`Mute::sub`] of a threat.
fn sub_of(t: &Threat) -> u8 {
    match t.kind {
        ThreatKind::IdFlood => (t.detail >> 24) as u8,
        ThreatKind::ControlContested => t.detail as u8,
        _ => 0,
    }
}

/// One silenced alarm: an operator's timed mute (nuisance kinds,
/// [`OPERATOR_MUTE_MS`]) or snooze (safety kinds, [`SNOOZE_MS`]), or a
/// site-file mute for good (a venue's known false alarm, one explicit line).
/// It names one specific thing ([`key_of`]); there are no wildcards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mute {
    pub kind: ThreatKind,
    pub ssid: heapless::String<32>,
    pub source: [u8; 6],
    pub target: [u8; 6],
    /// Centre, 100 kHz units.
    pub freq: u32,
    /// The reporting node, for kinds keyed by node; [`NODE_ANY`] otherwise.
    /// Site-file rules never name one (node numbers follow the gateway's
    /// order and are not stable across boots).
    pub node: u8,
    /// `IdFlood`: the table; `ControlContested`: the system.
    pub sub: u8,
    /// Hub uptime seconds at which the rule ends; 0: for good (site file).
    pub until_s: u32,
}

/// Why a site-file mute line is not honoured (every such reason starts
/// with this, see [`crate::site::is_dropped_mute`]).
pub const NOT_HONOURED: &str = "mute not honoured: ";

impl Mute {
    /// A rule for `kind` with every field empty (a starting point; on its
    /// own it is too broad for [`Tracker::mute`] to take).
    pub const fn empty(kind: ThreatKind) -> Self {
        Mute { kind, ssid: heapless::String::new(), source: [0; 6], target: [0; 6], freq: 0, node: NODE_ANY, sub: 0, until_s: 0 }
    }

    /// Which fields this rule must match ([`key_of`]).
    pub fn key(&self) -> u8 {
        key_of(self.kind, self.target == [0; 6])
    }

    /// For good: a site-file rule.
    pub fn permanent(&self) -> bool {
        self.until_s == 0
    }

    /// A snooze (a safety-critical kind, short).
    pub fn snooze(&self) -> bool {
        is_safety_kind(self.kind)
    }

    /// Hub ms the rule ends, `None` for good.
    pub fn ends_ms(&self) -> Option<u64> {
        (self.until_s != 0).then(|| u64::from(self.until_s) * 1000)
    }

    /// Still in force at `now_ms`.
    pub fn live(&self, now_ms: u64) -> bool {
        self.ends_ms().is_none_or(|e| now_ms < e)
    }

    /// Whether this rule names threat `t` (time not considered).
    pub fn matches(&self, t: &Threat) -> bool {
        use key::*;
        if self.kind != t.kind {
            return false;
        }
        let k = self.key();
        (k & NODE == 0 || self.node == t.node)
            && (k & SSID == 0 || self.ssid == t.ssid)
            && (k & SOURCE == 0 || self.source == t.source)
            && (k & TARGET == 0 || self.target == t.target)
            && (k & FREQ == 0 || self.freq.abs_diff(t.freq) <= MUTE_JAM_TOL)
            && (k & BAND == 0 || self.freq.abs_diff(t.freq) <= SAME_JAM_BAND)
            && (k & SUB == 0 || self.sub == sub_of(t))
            // A flood rule is either for a node's aggregate entry (no
            // target) or for one victim's own entry.
            && (!is_flood_kind(self.kind) || t.per_node == (self.target == [0; 6]))
    }

    /// The rule that silences `t` and nothing else, from `now_ms`: a snooze
    /// for a safety-critical kind, otherwise an operator mute.
    pub fn for_threat(t: &Threat, now_ms: u64) -> Self {
        use key::*;
        let mut m = Mute::empty(t.kind);
        let k = key_of(t.kind, t.target == [0; 6] || (is_flood_kind(t.kind) && t.per_node));
        if k & NODE != 0 {
            m.node = t.node;
        }
        if k & SSID != 0 {
            m.ssid = t.ssid.clone();
        }
        if k & SOURCE != 0 {
            m.source = t.source;
        }
        if k & TARGET != 0 && !(is_flood_kind(t.kind) && t.per_node) {
            m.target = t.target;
        }
        if k & (FREQ | BAND) != 0 {
            m.freq = t.freq;
        }
        if k & SUB != 0 {
            m.sub = sub_of(t);
        }
        let dur = if is_safety_kind(t.kind) { SNOOZE_MS } else { OPERATOR_MUTE_MS };
        m.until_s = until_s(now_ms, dur);
        m
    }

    /// Whether this rule may be kept for good (a site-file line): not a
    /// safety-critical kind, no node, and every field of its key given.
    pub fn check_permanent(&self) -> Result<(), &'static str> {
        use key::*;
        if is_safety_kind(self.kind) {
            return Err(concat!(
                "mute not honoured: ",
                "this alarm can mean the hub is missing drones; it can only be snoozed on the hub (5 min)"
            ));
        }
        let k = self.key();
        let missing = (k & NODE != 0)
            || self.node != NODE_ANY
            || (k & SSID != 0 && self.ssid.is_empty())
            || (k & SOURCE != 0 && self.source == [0; 6])
            || (k & TARGET != 0 && self.target == [0; 6])
            || (k & (FREQ | BAND) != 0 && self.freq == 0);
        if missing {
            return Err(site_needs(self.kind));
        }
        Ok(())
    }
}

/// What a site-file mute line of `kind` must name (the refusal text).
pub fn site_needs(kind: ThreatKind) -> &'static str {
    match kind {
        ThreatKind::EvilTwin | ThreatKind::DroneNetClone | ThreatKind::AttackTool => {
            "mute not honoured: too broad; name the transmitter and the network: source=.. ssid=.."
        }
        ThreatKind::Karma | ThreatKind::MavlinkAttack => "mute not honoured: too broad; name the transmitter: source=..",
        ThreatKind::ControlContested => {
            "mute not honoured: too broad; name the link and band: target=.. mhz=.. (needs control ids on)"
        }
        _ => "mute not honoured: too broad; name the victim: target=..",
    }
}

/// Hub uptime seconds `dur_ms` after `now_ms`, rounded up (never 0).
fn until_s(now_ms: u64, dur_ms: u64) -> u32 {
    (now_ms.saturating_add(dur_ms).div_ceil(1000)).clamp(1, u64::from(u32::MAX)) as u32
}

/// Silenced alarms and rules in force, for the screen and the status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MuteCounts {
    /// Operator mutes in force (rules), and ms until the first one ends.
    pub muted: u8,
    pub mute_left_ms: u64,
    /// Snoozes in force (rules), and ms until the first one ends.
    pub snoozed: u8,
    pub snooze_left_ms: u64,
    /// Site-file mutes (for good).
    pub site: u8,
}

#[derive(Debug, Clone, Default)]
pub struct ThreatTable {
    v: Vec<Threat, MAX_THREATS>,
    next_id: u32,
    mutes: Vec<Mute, MAX_MUTES>,
    /// New threats dropped: the table was full of ones it may not remove.
    dropped: u32,
}

impl ThreatTable {
    pub const fn new() -> Self {
        Self { v: Vec::new(), next_id: 1, mutes: Vec::new(), dropped: 0 }
    }

    /// Whether a flood sighting's victim has a rule of its own (then its
    /// reports are kept apart from the node's aggregate entry, so the rule
    /// silences that victim and nothing else).
    fn sighting_muted(&self, s: &ThreatSighting, now_ms: u64) -> bool {
        self.mutes
            .iter()
            .any(|m| m.kind == s.kind && m.target != [0; 6] && m.target == s.target && m.live(now_ms))
    }

    /// The rule silencing `t` now, if any (a snooze or timed mute before a
    /// rule for good, so the screen shows when it ends).
    fn rule_for(&self, t: &Threat, now_ms: u64) -> Option<&Mute> {
        self.mutes.iter().filter(|m| m.live(now_ms) && m.matches(t)).max_by_key(|m| m.until_s != 0)
    }

    /// The entry a newcomer of `kind` may replace in a full table (see the
    /// module notes), or `None`.
    fn victim(&self, kind: ThreatKind, now_ms: u64) -> Option<usize> {
        let v = &self.v;
        let oldest = |f: &dyn Fn(&Threat) -> bool| {
            (0..v.len()).filter(|&i| f(&v[i])).min_by_key(|&i| v[i].last_seen_ms)
        };
        // A snoozed jammer is still a live jammer.
        let jamming_active = |t: &Threat| t.active(now_ms) && t.kind.is_jamming();
        let live_jammers = v.iter().filter(|t| jamming_active(t)).count();
        oldest(&|t| !t.active(now_ms) && t.muted)
            .or_else(|| oldest(&|t| !t.active(now_ms)))
            .or_else(|| oldest(&|t| t.muted && !is_reserved_kind(t.kind)))
            .or_else(|| oldest(&|t| is_flood_kind(t.kind)))
            .or_else(|| {
                // Anything else only for a newcomer that is not a flood,
                // and a reserved kind only for another reserved newcomer.
                if is_flood_kind(kind) {
                    return None;
                }
                oldest(&|t| !is_reserved_kind(t.kind)).or_else(|| {
                    if !is_reserved_kind(kind) {
                        return None;
                    }
                    // Reserved for reserved: never the last live jammer,
                    // and a jammer only for another jamming report.
                    oldest(&|t| {
                        (!t.kind.is_jamming() || kind.is_jamming())
                            && !(jamming_active(t) && live_jammers <= 1)
                    })
                })
            })
    }

    /// Drop ended rules and recompute every threat's silence. One that
    /// was silenced and no longer is, while still active, is announced
    /// again ([`Tracker::take_new_threat`]): a snooze ends with an alarm.
    fn remute(&mut self, now_ms: u64) {
        self.mutes.retain(|m| m.live(now_ms));
        for i in 0..self.v.len() {
            let was = self.v[i].muted;
            let rule = self.rule_for(&self.v[i], now_ms).map(|m| (m.snooze(), m.until_s));
            let t = &mut self.v[i];
            t.muted = rule.is_some();
            t.snoozed = rule.is_some_and(|r| r.0);
            t.silenced_until_s = rule.map_or(0, |r| r.1);
            if was && !t.muted && t.active(now_ms) {
                t.announced = false;
            }
        }
    }

    pub fn all(&self) -> &[Threat] {
        &self.v
    }

    pub(crate) fn ingest(&mut self, node: u8, by_hub: bool, s: &ThreatSighting, now_ms: u64) -> Option<ThreatId> {
        let per_node = is_flood_kind(s.kind) && !self.sighting_muted(s, now_ms);
        let i = match self.v.iter().position(|t| t.same(node, by_hub, per_node, s)) {
            Some(i) => i,
            None => {
                // Floods hold at most MAX_FLOOD_THREATS entries: a new one
                // over that replaces the oldest flood (ended ones first).
                let floods = self.v.iter().filter(|t| is_flood_kind(t.kind)).count();
                let victim = if is_flood_kind(s.kind) && floods >= MAX_FLOOD_THREATS {
                    (0..self.v.len())
                        .filter(|&i| is_flood_kind(self.v[i].kind))
                        .min_by_key(|&i| (self.v[i].active(now_ms), self.v[i].last_seen_ms))
                } else if self.v.is_full() {
                    self.victim(s.kind, now_ms)
                } else {
                    None
                };
                match victim {
                    Some(i) => {
                        self.v.remove(i);
                    }
                    None if self.v.is_full() => {
                        self.dropped = self.dropped.saturating_add(1);
                        return None;
                    }
                    None => {}
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
                    snoozed: false,
                    silenced_until_s: 0,
                    per_node,
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
        // A MAVLink finding once backed stays backed (a later report from
        // a re-created node entry may carry the suspicion bit again).
        t.detail = if t.kind == ThreatKind::MavlinkAttack && t.reports > 0 && !t.suspect() {
            s.detail & !link::MAVLINK_SUSPECT
        } else {
            s.detail
        };
        // Keyed by the drone, not the attacker: show the latest one.
        if matches!(s.kind, ThreatKind::DroneDeauth | ThreatKind::DroneTakeover) && s.source != [0; 6] {
            t.source = s.source;
        }
        // One entry per node for a flood: the latest victim and attacker.
        if t.per_node {
            t.target = s.target;
            t.source = s.source;
        }
        if !s.ssid.is_empty() {
            t.ssid = s.ssid.clone();
        }
        t.last_seen_ms = t.last_seen_ms.max(now_ms);
        t.reports = t.reports.saturating_add(1);
        // An aggregate is muted only by its node's rule (a targeted rule
        // keeps its reports apart instead, above).
        let (id, was) = (t.id, t.muted);
        let rule = self.rule_for(&self.v[i], now_ms).map(|m| (m.snooze(), m.until_s));
        let t = &mut self.v[i];
        if was && rule.is_none() {
            // Its silence ended: announced again.
            t.announced = false;
        }
        t.muted = rule.is_some();
        t.snoozed = rule.is_some_and(|r| r.0);
        t.silenced_until_s = rule.map_or(0, |r| r.1);
        Some(id)
    }

    pub(crate) fn expire(&mut self, now_ms: u64, remove_after_ms: u64) {
        self.v.retain(|t| now_ms.saturating_sub(t.last_seen_ms) < remove_after_ms);
        if self.mutes.iter().any(|m| !m.live(now_ms)) {
            self.remute(now_ms);
        }
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

/// Announced, raised by `seen_ms`, sounding at `now_ms`, not silenced.
fn seen_alarm(t: &Threat, seen_ms: u64, now_ms: u64) -> bool {
    t.announced && !t.muted && t.first_seen_ms <= seen_ms && t.active(now_ms)
}

// --------------------------------------------------------------- tracker --

impl<const N: usize, const A: usize> Tracker<N, A> {
    /// Feed one threat reported by `node`. `None` when it is new and the
    /// table is full of threats it may not replace (see the module notes).
    pub fn ingest_threat(&mut self, node: u8, s: &ThreatSighting, now_ms: u64) -> Option<ThreatId> {
        self.expire(now_ms);
        self.threats.ingest(node, false, s, now_ms)
    }

    /// New threats dropped because the table was full of ones it may not
    /// replace (counted since boot).
    pub fn threats_dropped(&self) -> u32 {
        self.threats.dropped
    }

    /// Feed a node's Stats report to the deaf-node check (a deaf node
    /// becomes a hub-raised [`ThreatKind::Blinded`] threat) and its drop
    /// total to the node-shed check ([`crate::probation::ShedWatch`], raised
    /// from [`Tracker::expire`]).
    pub fn ingest_node_stats(
        &mut self,
        node: u8,
        uptime_us: u64,
        frames_seen: u32,
        dropped: u32,
        now_ms: u64,
    ) -> Option<ThreatId> {
        self.shed.stats(node, uptime_us, dropped, now_ms);
        let s = self.deaf.stats(node, uptime_us, frames_seen, now_ms)?;
        self.expire(now_ms);
        self.threats.ingest(node, true, &s, now_ms)
    }

    /// Raise (or refresh) a threat the hub found itself from what it holds,
    /// rather than one a node reported: `by_hub`, node 0. Used by checks
    /// that live outside the tracker (`area_gnss::AreaWatch`).
    pub fn ingest_hub_threat(&mut self, s: &ThreatSighting, now_ms: u64) -> Option<ThreatId> {
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

    /// Threats still being reported, silenced ones and MAVLink suspicions
    /// (one uncorroborated frame, [`Threat::suspect`]: listed, never
    /// alerted on) left out. This is the screen's ATTACK count.
    pub fn active_threats(&self, now_ms: u64) -> impl Iterator<Item = &Threat> {
        self.threats.all().iter().filter(move |t| t.active(now_ms) && !t.muted && !t.suspect())
    }

    /// Add a rule. A rule for good (a site-file line) must name one
    /// specific thing and may not be for a safety-critical kind
    /// ([`Mute::check_permanent`]); a timed one is held to at most its
    /// kind's length ([`SNOOZE_MS`], [`OPERATOR_MUTE_MS`]) from `now_ms`.
    /// `Ok(false)`: already there, or the list is full.
    pub fn mute(&mut self, mut m: Mute, now_ms: u64) -> Result<bool, &'static str> {
        if m.permanent() {
            m.check_permanent()?;
        } else {
            let most = until_s(now_ms, if m.snooze() { SNOOZE_MS } else { OPERATOR_MUTE_MS });
            m.until_s = m.until_s.min(most);
            if !m.live(now_ms) {
                return Ok(false);
            }
        }
        let tt = &mut self.threats;
        let added = !tt.mutes.contains(&m) && tt.mutes.push(m).is_ok();
        tt.remute(now_ms);
        Ok(added)
    }

    /// Silence one alarm: snooze it ([`SNOOZE_MS`]) when it is a
    /// safety-critical kind, else mute it ([`OPERATOR_MUTE_MS`]). The rule
    /// added (for the evidence log), or `None` when there is no such threat
    /// or no room.
    pub fn silence(&mut self, id: ThreatId, now_ms: u64) -> Option<Mute> {
        let m = Mute::for_threat(self.get_threat(id)?, now_ms);
        matches!(self.mute(m.clone(), now_ms), Ok(true)).then_some(m)
    }

    /// Alarms the operator has seen at `seen_ms` (announced, raised by
    /// then, sounding now, not silenced): how many nuisance ones the
    /// "Mute" action would silence and how many safety-critical ones the
    /// "Snooze" action would.
    pub fn alarm_counts(&self, seen_ms: u64, now_ms: u64) -> (u8, u8) {
        let seen = self.threats.all().iter().filter(|t| seen_alarm(t, seen_ms, now_ms));
        let (mut m, mut s) = (0u8, 0u8);
        for t in seen {
            if is_safety_kind(t.kind) {
                s = s.saturating_add(1);
            } else {
                m = m.saturating_add(1);
            }
        }
        (m, s)
    }

    /// Settings "Mute": mute every nuisance alarm the operator has seen
    /// ([`Tracker::alarm_counts`]), each on its own rule, for
    /// [`OPERATOR_MUTE_MS`]. Never a safety-critical one, never one raised
    /// after `seen_ms`. Returns the rules added (for the evidence log).
    pub fn mute_seen(&mut self, seen_ms: u64, now_ms: u64) -> Vec<Mute, MAX_THREATS> {
        self.silence_seen(seen_ms, now_ms, false)
    }

    /// Settings "Snooze": snooze every safety-critical alarm the operator
    /// has seen, for [`SNOOZE_MS`].
    pub fn snooze_seen(&mut self, seen_ms: u64, now_ms: u64) -> Vec<Mute, MAX_THREATS> {
        self.silence_seen(seen_ms, now_ms, true)
    }

    fn silence_seen(&mut self, seen_ms: u64, now_ms: u64, safety: bool) -> Vec<Mute, MAX_THREATS> {
        let rules: Vec<Mute, MAX_THREATS> = self
            .threats
            .all()
            .iter()
            .filter(|t| seen_alarm(t, seen_ms, now_ms) && is_safety_kind(t.kind) == safety)
            .map(|t| Mute::for_threat(t, now_ms))
            .collect();
        let mut added = Vec::new();
        for m in rules {
            if matches!(self.mute(m.clone(), now_ms), Ok(true)) {
                let _ = added.push(m);
            }
        }
        added
    }

    /// Drop every rule (operator and site file); alarms still sounding are
    /// announced again.
    pub fn unmute_all(&mut self, now_ms: u64) {
        self.threats.mutes.clear();
        self.threats.remute(now_ms);
    }

    /// The site file's rules (for good) replace those of its last load;
    /// the operator's timed mutes and snoozes stay. A rule that is not
    /// specific, or is for a safety-critical kind, is refused
    /// ([`Mute::check_permanent`]). Returns how many were refused.
    pub fn set_site_mutes(&mut self, rules: &[Mute], now_ms: u64) -> usize {
        self.threats.mutes.retain(|m| !m.permanent());
        let mut refused = 0;
        for m in rules {
            if m.check_permanent().is_err() || !matches!(self.mute(m.clone(), now_ms), Ok(true) | Ok(false)) {
                refused += 1;
            }
        }
        self.threats.remute(now_ms);
        refused
    }

    /// Drop the operator's mutes and snoozes, keep the site file's.
    pub fn unmute_operator(&mut self, now_ms: u64) {
        self.threats.mutes.retain(|m| m.permanent());
        self.threats.remute(now_ms);
    }

    pub fn mutes(&self) -> &[Mute] {
        &self.threats.mutes
    }

    /// Rules in force by type, and when the first timed ones end.
    pub fn mute_counts(&self, now_ms: u64) -> MuteCounts {
        let mut c = MuteCounts::default();
        for m in self.threats.mutes.iter().filter(|m| m.live(now_ms)) {
            match m.ends_ms() {
                None => c.site = c.site.saturating_add(1),
                Some(e) => {
                    let left = e.saturating_sub(now_ms);
                    let (n, first) = if m.snooze() { (&mut c.snoozed, &mut c.snooze_left_ms) } else { (&mut c.muted, &mut c.mute_left_ms) };
                    *first = if *n == 0 { left } else { (*first).min(left) };
                    *n = n.saturating_add(1);
                }
            }
        }
        c
    }

    /// Whether any node is jammed or blinded right now: the detector may be
    /// missing drones, so "nothing seen" must not read as "all clear".
    /// Mutes and snoozes do not change it.
    pub fn partly_blind(&self, now_ms: u64) -> bool {
        self.threats.all().iter().any(|t| t.active(now_ms) && t.kind.is_jamming())
    }

    /// The next threat to alert on, exactly once each (and once more when
    /// a mute or snooze on it ends while it is still sounding). Silenced
    /// threats are not handed out.
    pub fn take_new_threat(&mut self, _now_ms: u64) -> Option<ThreatId> {
        loop {
            // A MAVLink suspicion waits until it is backed.
            let t = self.threats.v.iter_mut().find(|t| !t.announced && !t.suspect())?;
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
    /// How it is silenced: `"snooze"` (a safety-critical kind, minutes),
    /// `"timed"` (an operator's mute, hours) or `"site"` (the site file,
    /// for good); absent when not silenced. Added with WP-11b.
    #[serde(skip_serializing_if = "Option::is_none")]
    mute: Option<&'static str>,
    /// Hub ms the snooze or timed mute ends. Added with WP-11b.
    #[serde(skip_serializing_if = "Option::is_none")]
    mute_until_ms: Option<u64>,
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
    /// Present (true) when `src`, `target` or `ssid` is shown: they are what
    /// the attack frames say, not verified, and can name an innocent
    /// device (docs/WIFI-ATTACKS.md "What a forged frame can make an alarm
    /// say"). Added with WP-11b.
    #[serde(skip_serializing_if = "core::ops::Not::not")]
    claimed: bool,
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
    /// Hostile MAVLink: `"wifi"` (open Wi-Fi) or `"radio"` (SiK / mLRS).
    #[serde(skip_serializing_if = "Option::is_none")]
    via: Option<&'static str>,
    /// A flood kept as one entry per reporting node (`target` is the
    /// latest victim). Only present when true. Added with WP-09.
    #[serde(skip_serializing_if = "core::ops::Not::not")]
    per_node: bool,
    /// `IdFlood` only: which table (`"drones"`, `"aircraft"`, `"control"`,
    /// or `"node"`: one node shedding, `node` names it) and the new
    /// identities a second (`"node"`: new drone IDs a second from it that
    /// never confirmed). Added with WP-09.
    #[serde(skip_serializing_if = "Option::is_none")]
    table: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    per_s: Option<u32>,
    /// `IdFlood` table `"node"` only: frames a second the node discarded.
    #[serde(skip_serializing_if = "Option::is_none")]
    dropped_per_s: Option<u32>,
    n: u32,
    first_ms: u64,
    last_ms: u64,
    age_ms: u64,
}

/// A buffer this size always holds one threat line (see the test).
pub const MAX_THREAT_LINE: usize = 640;
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
        mute: t.muted.then_some(if t.snoozed {
            "snooze"
        } else if t.silenced_until_s != 0 {
            "timed"
        } else {
            "site"
        }),
        mute_until_ms: t.silenced_until_ms(),
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
        claimed: (show && (known(&t.source) || known(&t.target))) || !t.ssid.is_empty(),
        ctrl_system: t.contested().and_then(|d| d.system).map(|s| s.label()),
        transmitters: t.contested().map(|d| d.transmitters),
        rule: t.contested().map(|d| ctrlwatch::contested::rule_name(d.rules)),
        maker: drone_maker(t),
        via: t.via(),
        per_node: t.per_node,
        table: crate::probation::flood_table(t).map(|f| f.name()),
        per_s: crate::probation::flood_table(t).map(|_| crate::probation::flood_rate(t)),
        dropped_per_s: crate::probation::shed_rate(t),
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
        let d1 = tr.ingest_threat(1, &tello(ThreatKind::DroneDeauth, a, "TELLO-010203"), 1_000).unwrap();
        let d2 = tr.ingest_threat(1, &tello(ThreatKind::DroneDeauth, b, "TELLO-010203"), 2_000).unwrap();
        assert_eq!(d1, d2);
        let t1 = tr.ingest_threat(1, &tello(ThreatKind::DroneTakeover, a, "TELLO-010203"), 2_000).unwrap();
        let t2 = tr.ingest_threat(1, &tello(ThreatKind::DroneTakeover, b, "TELLO-010203"), 3_000).unwrap();
        assert_eq!(t1, t2);
        assert_ne!(d1, t1);
        let t = tr.get_threat(t1).unwrap();
        assert_eq!(t.source, b);
        assert_eq!(t.count, 6);
        assert_eq!(drone_maker(t), Some("DJI Tello"));
        assert_eq!(t.label(), "Drone Wi-Fi taken over");
        // Copies: one per name and copy.
        let c1 = tr.ingest_threat(1, &tello(ThreatKind::DroneNetClone, a, "TELLO-010203"), 3_000).unwrap();
        let c2 = tr.ingest_threat(1, &tello(ThreatKind::DroneNetClone, b, "TELLO-010203"), 3_000).unwrap();
        let c3 = tr.ingest_threat(1, &tello(ThreatKind::DroneNetClone, a, "TELLO-010203"), 4_000).unwrap();
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
        let m = Mute::for_threat(tr.get_threat(t1).unwrap(), 4_000);
        assert_eq!((m.target, m.source), (drone, [0; 6]));
        // JSON line names the maker.
        let ctx = Context { now_ms: 4_000, utc_ms: None, own: None, config: crate::Config::default(), flood: None };
        let mut buf = [0u8; MAX_THREAT_LINE];
        let n = threat_line(tr.get_threat(t1).unwrap(), &ctx, &mut buf).unwrap();
        let text = core::str::from_utf8(&buf[..n]).unwrap();
        assert!(text.contains("\"maker\":\"DJI Tello\""), "{text}");
        assert!(text.contains("\"kind\":\"drone_takeover\""), "{text}");
        // Other kinds have no maker, whatever their detail says.
        let mut plain = tello(ThreatKind::Deauth, a, "");
        plain.target = [0xFF; 6];
        let p = tr.ingest_threat(1, &plain, 4_000).unwrap();
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
        let id = tr.ingest_threat(u8::MAX, &big, u64::MAX / 4).unwrap();
        let config = crate::Config { show_link_ids: true, ..Default::default() };
        let ctx = Context { now_ms: u64::MAX / 2, utc_ms: None, own: None, config, flood: None };
        let n = threat_line(tr.get_threat(id).unwrap(), &ctx, &mut buf).unwrap();
        assert!(n > 512, "{n}: the reason MAX_THREAT_LINE grew from 512");
        // Muted until as late as a hub uptime can say (u32 seconds).
        let late = u64::from(u32::MAX - 10_000) * 1000;
        let id = tr.ingest_threat(u8::MAX, &big, late).unwrap();
        assert!(tr.silence(id, late).is_some());
        let ctx = Context { now_ms: late, ..ctx };
        let n = threat_line(tr.get_threat(id).unwrap(), &ctx, &mut buf).unwrap();
        assert!(n >= 576, "{n}: at the old limit, the reason it grew again, to 640 (WP-11b)");
        assert!(core::str::from_utf8(&buf[..n]).unwrap().contains("\"mute\":\"timed\",\"mute_until_ms\":"));
        assert!(core::str::from_utf8(&buf[..n]).unwrap().contains("\"maker\":\"Holy Stone\""));
    }

    #[test]
    fn same_threat_merges_and_alerts_once() {
        let mut tr: Tracker = Tracker::default();
        let a = tr.ingest_threat(1, &sighting(ThreatKind::Deauth, 0xFF, 0), 1_000).unwrap();
        let b = tr.ingest_threat(1, &sighting(ThreatKind::Deauth, 0xFF, 0), 2_000).unwrap();
        assert_eq!(a, b);
        assert_eq!(tr.threats()[0].count, 100);
        // Another target from the same node: a flood is one entry per node
        // (the latest target shown).
        assert_eq!(tr.ingest_threat(1, &sighting(ThreatKind::Deauth, 0xAA, 0), 2_000), Some(a));
        assert_eq!(tr.threats()[0].count, 150);
        assert_eq!(tr.threats()[0].target, [0xAA; 6]);
        assert!(tr.threats()[0].per_node);
        // Another node, another kind: separate.
        tr.ingest_threat(2, &sighting(ThreatKind::Deauth, 0xFF, 0), 2_000);
        tr.ingest_threat(1, &sighting(ThreatKind::Disassoc, 0xFF, 0), 2_000);
        assert_eq!(tr.threats().len(), 3);
        let mut alerts = 0;
        while tr.take_new_threat(3_000).is_some() {
            alerts += 1;
        }
        assert_eq!(alerts, 3);
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
        // Evil twins are one per name and copy: 16 copies fill the table.
        let twin = |i: u8| {
            let mut s = sighting(ThreatKind::EvilTwin, 0, 0);
            s.source[5] = i;
            s
        };
        tr.ingest_threat(1, &twin(0), 0);
        for i in 1..MAX_THREATS as u8 {
            tr.ingest_threat(1, &twin(i), 20_000);
        }
        tr.ingest_threat(1, &twin(0xEE), 20_000);
        assert_eq!(tr.threats().len(), MAX_THREATS);
        assert!(tr.threats().iter().all(|t| t.source[5] != 0), "the ended one went first");
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
            assert!(tr.ingest_node_stats(4, t * 1_000, frames, 0, t).is_none());
        }
        let (normal, _) = tr.node_rates(4, t).unwrap();
        assert!((normal - 200.0).abs() < 1.0);
        // Collapse to 5 frames/s: nothing for 10 s, then Blinded.
        let mut raised = None;
        for s in 0..15 {
            t += 1_000;
            frames += 5;
            if let Some(id) = tr.ingest_node_stats(4, t * 1_000, frames, 0, t) {
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
            assert!(tr.ingest_node_stats(1, t * 1_000, f, 0, t).is_none());
        }
        // A busy node that collapses before 60 s of learning.
        let (mut f, mut t) = (0u32, 0u64);
        for i in 0..60 {
            t += 1_000;
            f += if i < 30 { 300 } else { 0 };
            assert!(tr.ingest_node_stats(2, t * 1_000, f, 0, t).is_none());
        }
        // A node that reboots: uptime and counters restart, no alarm.
        let (mut f, mut t) = (0u32, 0u64);
        for _ in 0..90 {
            t += 1_000;
            f += 300;
            tr.ingest_node_stats(3, t * 1_000, f, 0, t);
        }
        for i in 0..30u64 {
            t += 1_000;
            assert!(tr.ingest_node_stats(3, (i + 1) * 1_000_000, 0, 0, t).is_none());
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
            s.channel = 0; // an LR2021 node's (SiK) finding
            s.source = [0, 0, 0, 0, sys, comp];
            s.target = [0, 0, 0, 0, 1, 1];
            s.detail = detail;
            s.ssid = heapless::String::try_from("dev 10 n 7: reboot?").unwrap();
            s
        };
        let a = tr.ingest_threat(4, &mav(255, 190, 13), 1_000).unwrap();
        assert_eq!(tr.ingest_threat(4, &mav(255, 190, 13), 3_000), Some(a));
        let b = tr.ingest_threat(4, &mav(255, 190, 16), 3_000).unwrap();
        let c = tr.ingest_threat(4, &mav(1, 100, 1), 3_000).unwrap();
        assert!(a != b && b != c && a != c);
        assert_eq!(tr.get_threat(a).unwrap().label(), "Hostile drone telemetry");
        let ctx = Context { now_ms: 3_000, utc_ms: None, own: None, config: Config::default(), flood: None };
        let mut buf = [0u8; MAX_THREAT_LINE];
        let n = threat_line(tr.get_threat(a).unwrap(), &ctx, &mut buf).unwrap();
        let text = core::str::from_utf8(&buf[..n]).unwrap();
        assert!(text.contains("\"kind\":\"mavlink_attack\"") && text.contains("\"detail\":13"), "{text}");
        assert!(text.contains("\"via\":\"radio\""), "{text}");
        // Muting the owner's own console silences that sender only.
        assert_eq!(tr.mute(Mute::for_threat(tr.get_threat(a).unwrap(), 3_000), 3_000), Ok(true));
        assert_eq!(tr.active_threats(3_000).count(), 1);
    }

    /// A single uncorroborated MAVLink frame is listed but not alerted on;
    /// the same finding reported again without the suspicion bit is the
    /// same threat, alerted on then.
    #[test]
    fn mavlink_suspicion_waits_for_backing() {
        let mut tr: Tracker = Tracker::default();
        let mut s = sighting(ThreatKind::MavlinkAttack, 0, 9_150);
        s.source = [0, 0, 0, 0, 255, 190];
        s.detail = 13 | link::MAVLINK_SUSPECT;
        let a = tr.ingest_threat(4, &s, 1_000).unwrap();
        assert!(tr.get_threat(a).unwrap().suspect());
        assert_eq!(tr.get_threat(a).unwrap().label(), "Possible hostile drone telemetry (one frame)");
        assert_eq!(tr.take_new_threat(1_000), None, "no alert for a suspicion");
        assert_eq!(tr.active_threats(1_000).count(), 0, "nor the ATTACK count");
        s.detail = 13;
        assert_eq!(tr.ingest_threat(4, &s, 1_500), Some(a), "escalation updates the same threat");
        assert!(!tr.get_threat(a).unwrap().suspect());
        assert_eq!(tr.take_new_threat(1_500), Some(a));
        assert_eq!(tr.active_threats(1_500).count(), 1);
        // A later suspicion report does not demote it.
        s.detail = 13 | link::MAVLINK_SUSPECT;
        assert_eq!(tr.ingest_threat(4, &s, 2_000), Some(a));
        assert!(!tr.get_threat(a).unwrap().suspect());
    }

    /// Protocol 13: the same finding from a Wi-Fi node (`channel` set) is
    /// tabled the same way, labelled as over Wi-Fi, and its JSON says so.
    #[test]
    fn mavlink_attack_over_wifi() {
        let mut tr: Tracker = Tracker::default();
        let mut s = sighting(ThreatKind::MavlinkAttack, 0, 24_370);
        s.channel = 6;
        s.source = [0, 0, 0, 0, 1, 100];
        s.detail = 3;
        s.ssid = heapless::String::try_from("../../../../../tmp").unwrap();
        let a = tr.ingest_threat(2, &s, 1_000).unwrap();
        assert_eq!(tr.ingest_threat(2, &s, 2_000), Some(a), "one per sender and finding");
        let t = tr.get_threat(a).unwrap();
        assert!(t.mavlink_over_wifi());
        assert_eq!(t.label(), "Hostile drone telemetry over Wi-Fi");
        let ctx = Context { now_ms: 2_000, utc_ms: None, own: None, config: Config::default(), flood: None };
        let mut buf = [0u8; MAX_THREAT_LINE];
        let n = threat_line(t, &ctx, &mut buf).unwrap();
        let text = core::str::from_utf8(&buf[..n]).unwrap();
        for want in [
            "\"kind\":\"mavlink_attack\"",
            "\"label\":\"Hostile drone telemetry over Wi-Fi\"",
            "\"via\":\"wifi\"",
            "\"ch\":6",
            "\"mhz\":2437",
            "\"detail\":3",
        ] {
            assert!(text.contains(want), "{want} in {text}");
        }
        // Other kinds carry no `via`.
        let d = tr.ingest_threat(2, &sighting(ThreatKind::Deauth, 1, 0), 2_000).unwrap();
        let n = threat_line(tr.get_threat(d).unwrap(), &ctx, &mut buf).unwrap();
        assert!(!core::str::from_utf8(&buf[..n]).unwrap().contains("via"));
        // The longest such line still fits.
        let mut big = s.clone();
        big.ssid = heapless::String::try_from("\"".repeat(32).as_str()).unwrap();
        big.source = [0xFF; 6];
        big.target = [0xFF; 6];
        big.channel = u8::MAX;
        big.freq = u32::MAX;
        big.width_khz = u32::MAX;
        big.count = u32::MAX;
        big.detail = u32::MAX;
        big.level = i8::MIN;
        let id = tr.ingest_threat(u8::MAX, &big, u64::MAX / 4).unwrap();
        let config = crate::Config { show_link_ids: true, ..Default::default() };
        let ctx = Context { now_ms: u64::MAX / 2, utc_ms: None, own: None, config, flood: None };
        assert!(threat_line(tr.get_threat(id).unwrap(), &ctx, &mut buf).is_ok());
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
        let added = tr.mute_seen(1_000, 1_000);
        assert_eq!(added.len(), 1, "the twin; the jammer is never in Mute");
        assert_eq!(tr.snooze_seen(1_000, 1_000).len(), 1);
        assert_eq!(tr.active_threats(1_000).count(), 0);
        assert!(tr.partly_blind(1_000), "a snoozed jammer still blinds");
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
        while tr.take_new_threat(2_000).is_some() {}
        assert!(tr.mute_seen(2_000, 2_000).len() == 1 && tr.mute_seen(2_000, 2_000).is_empty(), "no duplicate rules");
        tr.unmute_all(2_000);
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
        let ctx = Context { now_ms: 2_000, utc_ms: None, own: None, config: Config::default(), flood: None };
        let mut buf = [0u8; MAX_THREAT_LINE];
        let n = threat_line(t, &ctx, &mut buf).unwrap();
        let text = core::str::from_utf8(&buf[..n]).unwrap();
        assert!(text.contains("\"ctrl_system\":\"ExpressLRS\",\"transmitters\":2,\"rule\":\"interleaved timing\""), "{text}");
        // Mute: the band, and without an identifier the node's handle and
        // the system too: that link only. A 2.4 GHz one, another handle or
        // another system is not muted by it.
        let m = Mute::for_threat(t, 2_000);
        assert_eq!((m.freq, m.source, m.target, m.node), (9_155, [0, 0, 0, 0, 0, 1], [0; 6], 4));
        let mut far = t.clone();
        far.freq = 24_404;
        assert!(m.matches(t) && !m.matches(&far));
        assert!(!m.matches(&tr.threats()[1]) && !m.matches(&tr.threats()[2]));
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
        let ctx = Context { now_ms: u64::MAX / 2, utc_ms: None, own: None, config, flood: None };
        let mut buf = [0u8; MAX_THREAT_LINE];
        let n = threat_line(&tr.threats()[0], &ctx, &mut buf).unwrap();
        let text = core::str::from_utf8(&buf[..n]).unwrap();
        assert!(text.contains("\"src\":\"02:01:02:03:04:05\""), "{text}");
        assert!(text.contains("\"kind\":\"evil_twin\""));
        assert!(text.contains("\"claimed\":true"), "addresses are claims: {text}");
        let ctx = Context { config: Config::default(), ..ctx };
        let n = threat_line(&tr.threats()[0], &ctx, &mut buf).unwrap();
        let text = core::str::from_utf8(&buf[..n]).unwrap();
        assert!(!text.contains("src") && !text.contains("\"target\""), "{text}");
        assert!(text.contains("\"ids_shown\":false"));
    }
}
