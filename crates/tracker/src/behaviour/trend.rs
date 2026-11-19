//! Rules without a position (slice 16c, docs/THREAT-PROFILES.md rules 12
//! and 13): signal-strength and sound trends, and the dark drone.
//!
//! An FPV video downlink, a control link's downlink, drone Wi-Fi or an open
//! FPV link says only "it exists" and how loud it is at each node; the
//! microphone says only "rotor sound" and how loud. So these rules judge a
//! **trend**, not a position, and their words say so ("FPV video signal
//! getting louder fast on 2 nodes", "rotor sound, no radio heard").
//!
//! - **12 Getting louder fast** (Caution): a signal whose level rises
//!   steadily by at least [`RISE_DB`] over [`MIN_SPAN_S`] to [`WINDOW_S`]
//!   seconds, to a level louder than anything it was heard at in the half
//!   minute before ([`HIST_S`]), and not falling on another node at the
//!   same time (a rise on one node and a fall on another is a crossing,
//!   not an approach: THREAT-TACTICS.md §4). **Warning** when it rises on
//!   two or more nodes at once, or when the microphone agrees (rotor sound
//!   getting louder, or a dark drone, at the same time). Rotor sound
//!   getting louder is rule 12 too (Caution; Warning with a rising signal).
//! - **13 Dark drone** (Warning): rotor sound heard for at least
//!   [`DARK_AFTER_MS`], not a low hum, not explained by a crewed aircraft,
//!   and no drone heard by radio from [`RADIO_RECENT_MS`] before the sound
//!   began until now ([`last_radio_drone_ms`]).
//! - **14 Spotter then striker**: a signal getting louder fast (not a
//!   sound: the spotter's own rotors may be what is getting louder) at a
//!   place where a spotter lingered in the last 10 minutes calls
//!   [`Across::striker`] with that place; Critical at a fixed area,
//!   Warning at the moving bubble (as a slow positional striker there).
//!
//! **"Steadily"**, so that banking and antenna nulls (swings of 10 dB and
//! more that come back), a drone switched on beside a node (it appears at
//! full strength), a pilot's own goggles standing still and one noisy
//! sample do not count ([`change`]):
//! - at least [`MIN_SAMPLES`] seconds with a sample in the last
//!   [`WINDOW_S`], spanning at least [`MIN_SPAN_S`], the newest at most
//!   [`FRESH_S`] old;
//! - the change is taken between medians of three (so one spike moves
//!   nothing), from the first three samples to the last three;
//! - most pairs of samples agree on the direction (Kendall's tau at least
//!   one half);
//! - no single step between the medians makes more than two thirds of it
//!   (a video transmitter leaving pit mode is a step, not an approach);
//! - and, for a rise, the new level is above everything heard from that
//!   signal on that node in the [`HIST_S`] seconds before the window (a
//!   drone coming out of a null is back where it was, not closer).
//!
//! **Which place it is closing on.** A signal has no position, so the place
//! is that of the node(s) hearing it rise ([`places_for`]): the hub's own
//! receiver (node 0), the microphone, and every node whose position is not
//! set in the site file count as at the hub, and give the bubble when it is
//! on, else the area(s) the hub stands in or is nearest to (within the
//! area's own size, at least 50 m), else the only area. A placed node gives
//! the area(s) it stands in or is nearest to, else the bubble when the
//! node is inside it. No place: the rule still fires, worded without one.
//!
//! **Friends** cannot cover these signals: a `friend` line names a serial,
//! and none of them carries one. Nothing here is lowered for a friend.
//!
//! **Memory.** One slot per path ring (64 on the P4 hub, about 23 KB; none
//! on hubs without path history), each with [`NODE_ROWS`] nodes of
//! [`HIST_S`] one-second levels (`i8`, the loudest that second).

use core::fmt::Write;

use heapless::{String, Vec};
use link::RfKind;

use super::across::{Across, NEAR_MIN_M};
use super::places::{Frame, Local, Name, Places};
use super::{Eval, Level, NONE_U16, Reason, Rule, TrackThreat};
use crate::{EmitterId, LinkTrackId, OwnFix, Track, TrackId, Tracker, WifiDroneId};
use crate::ctrllinks::ControlLinkId;

/// Seconds of level history kept per node.
pub const HIST_S: usize = 32;
/// The trend is judged over this many seconds (the newest).
pub const WINDOW_S: u32 = 12;
/// Seconds with a sample needed in the window.
pub const MIN_SAMPLES: usize = 5;
/// The samples must span at least this many seconds.
pub const MIN_SPAN_S: u32 = 5;
/// The newest sample may be this many seconds old.
pub const FRESH_S: u32 = 2;
/// A rise (or fall) of at least this many dB (halving the distance, in free
/// space).
pub const RISE_DB: i16 = 6;
/// Nodes whose levels are kept per signal.
pub const NODE_ROWS: usize = 3;
/// A slot is judged while heard this recently.
pub const HEARD_MS: u64 = 3_000;
/// A slot not heard for this long, and with no level left, is dropped.
pub const FORGET_MS: u64 = 60_000;
/// Rule 13: "no radio" means no drone heard by radio from this long before
/// the sound began.
pub const RADIO_RECENT_MS: u64 = 15_000;
/// Rule 13: the sound must have lasted this long.
pub const DARK_AFTER_MS: u64 = 5_000;
/// Rule 13: a drone track whose position is further than this from the hub
/// cannot be what the microphone hears (rotor sound carries 50-200 m;
/// Remote ID positions are good to about 30 m).
pub const SOUND_EXPLAINS_M: f32 = 600.0;
/// The microphone's "node" in a sound slot.
pub const MIC_NODE: u8 = u8::MAX;
/// A striker without a position goes to [`Across::striker`] as this bit
/// plus its [`SignalThreatId`], so it is never taken for a drone track.
pub const SIGNAL_TRACK_BIT: u32 = 1 << 31;

/// What kind of signal a rule-12 or rule-13 reason is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SignalKind {
    /// FPV video (analog, HDZero, digital links, DJI video).
    Video = 1,
    /// A control link's downlink (ELRS, mLRS, SiK...).
    Control = 2,
    /// A drone's Wi-Fi (by maker).
    Wifi = 3,
    /// An open FPV link (wfb-ng / OpenIPC, OpenHD...), air to ground.
    OpenLink = 4,
    /// Rotor sound (the microphone).
    Sound = 5,
}

impl SignalKind {
    const ALL: [SignalKind; 5] = [SignalKind::Video, SignalKind::Control, SignalKind::Wifi, SignalKind::OpenLink, SignalKind::Sound];

    fn of_code(c: u16) -> Option<Self> {
        Self::ALL.into_iter().find(|k| *k as u16 == c)
    }

    /// `"video"`, ... (JSON).
    pub const fn name(self) -> &'static str {
        match self {
            SignalKind::Video => "video",
            SignalKind::Control => "control",
            SignalKind::Wifi => "wifi",
            SignalKind::OpenLink => "open_link",
            SignalKind::Sound => "sound",
        }
    }

    /// In a sentence: `FPV video signal`.
    pub const fn words(self) -> &'static str {
        match self {
            SignalKind::Video => "FPV video signal",
            SignalKind::Control => "control signal",
            SignalKind::Wifi => "drone Wi-Fi signal",
            SignalKind::OpenLink => "FPV link signal",
            SignalKind::Sound => "rotor sound",
        }
    }

    /// On the screen strip: `FPV video`.
    pub const fn short(self) -> &'static str {
        match self {
            SignalKind::Video => "FPV video",
            SignalKind::Control => "control",
            SignalKind::Wifi => "drone Wi-Fi",
            SignalKind::OpenLink => "FPV link",
            SignalKind::Sound => "rotor sound",
        }
    }
}

/// The video kinds rule 12 follows: what the drone itself sends. Not the
/// DJI controller (the pilot's), an unconfirmed FPV carrier (a Wi-Fi access
/// point looks the same) or plain energy.
pub const fn rising_kind(k: RfKind) -> bool {
    matches!(
        k,
        RfKind::AnalogVideo
            | RfKind::Hdzero
            | RfKind::LteStyle
            | RfKind::WifiNarrow
            | RfKind::DjiVideo
            | RfKind::DjiO4Video
            | RfKind::DjiFpvAirUnit
    )
}

/// Where a slot's levels come from (the table and entry they belong to).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SignalSrc {
    Emitter { id: EmitterId, kind: RfKind, freq: u32 },
    Wifi { id: WifiDroneId, maker: u8, channel: u8 },
    OpenLink { id: LinkTrackId, system: openlink::System, channel: u8 },
    Control { id: ControlLinkId, system: ctrlwatch::System, region: ctrlwatch::Region },
    /// A rotor-sound contact ([`RotorSound::id`]).
    Sound { id: u32 },
}

impl SignalSrc {
    fn same(&self, o: &SignalSrc) -> bool {
        match (self, o) {
            (SignalSrc::Emitter { id: a, .. }, SignalSrc::Emitter { id: b, .. }) => a == b,
            (SignalSrc::Wifi { id: a, .. }, SignalSrc::Wifi { id: b, .. }) => a == b,
            (SignalSrc::OpenLink { id: a, .. }, SignalSrc::OpenLink { id: b, .. }) => a == b,
            (SignalSrc::Control { id: a, .. }, SignalSrc::Control { id: b, .. }) => a == b,
            (SignalSrc::Sound { id: a }, SignalSrc::Sound { id: b }) => a == b,
            _ => false,
        }
    }

    pub const fn kind(&self) -> SignalKind {
        match self {
            SignalSrc::Emitter { .. } => SignalKind::Video,
            SignalSrc::Wifi { .. } => SignalKind::Wifi,
            SignalSrc::OpenLink { .. } => SignalKind::OpenLink,
            SignalSrc::Control { .. } => SignalKind::Control,
            SignalSrc::Sound { .. } => SignalKind::Sound,
        }
    }

    /// The table it comes from, as its JSON line names itself: `"rf"`,
    /// `"wifidrone"`, `"link"`, `"control"`, `"sound"`.
    pub const fn table(&self) -> &'static str {
        match self {
            SignalSrc::Emitter { .. } => "rf",
            SignalSrc::Wifi { .. } => "wifidrone",
            SignalSrc::OpenLink { .. } => "link",
            SignalSrc::Control { .. } => "control",
            SignalSrc::Sound { .. } => "sound",
        }
    }

    /// Its id in that table.
    pub const fn table_id(&self) -> u32 {
        match self {
            SignalSrc::Emitter { id, .. } => id.0,
            SignalSrc::Wifi { id, .. } => id.0,
            SignalSrc::OpenLink { id, .. } => id.0,
            SignalSrc::Control { id, .. } => id.0,
            SignalSrc::Sound { id } => *id,
        }
    }
}

/// What it is, in a few words, from tables built into the firmware (never
/// from text heard over the air): `Analog video 5740.0 MHz`, `ExpressLRS
/// 915 link`, `DJI Tello Wi-Fi ch 6`, `OpenHD link ch 149`, `rotor sound`.
impl core::fmt::Display for SignalSrc {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SignalSrc::Emitter { kind, freq, .. } => {
                write!(f, "{} {}.{} MHz", crate::signals::rf_label(*kind), freq / 10, freq % 10)
            }
            SignalSrc::Wifi { maker, channel, .. } => {
                write!(f, "{} Wi-Fi ch {}", wifidrone::maker_name(*maker).unwrap_or("Unknown maker"), channel)
            }
            SignalSrc::OpenLink { system, channel, .. } => write!(f, "{} link ch {}", system.label(), channel),
            SignalSrc::Control { system, region, .. } => {
                write!(f, "{} {} link", system.label(), crate::ctrllinks::band_name(*region))
            }
            SignalSrc::Sound { .. } => f.write_str("rotor sound"),
        }
    }
}

/// One rotor-sound contact, as the hub's sound detector sees it, handed in
/// about once a second while it is heard ([`Tracker::hear_sound`]). The
/// trend of its loudness is worked out here from the levels handed in, as
/// for radio signals.
///
/// Shaped after the acoustic branch's `SoundContact`: `id` is its
/// `SoundId`, `first_ms` / `last_ms` its `first_seen_ms` / `last_seen_ms`,
/// `level_db` its `level_dbfs`, `low_hum` its kind `LowHum` (heard mostly
/// below 140 Hz: fans, engines, compressors) and `aircraft` its kind
/// `Aircraft` (a crewed aircraft we track is audible).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RotorSound {
    pub id: u32,
    pub first_ms: u64,
    pub last_ms: u64,
    /// Loudness now, dB (dB below full scale on the hub's microphone; only
    /// the change matters).
    pub level_db: i8,
    pub low_hum: bool,
    pub aircraft: bool,
}

/// Stable handle for a signal's threat entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(transparent)]
pub struct SignalThreatId(pub u32);

const EMPTY: i8 = i8::MIN;

/// One node's levels of one signal: the loudest each second, the last
/// [`HIST_S`] seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Row {
    node: u8,
    used: bool,
    /// The newest second written.
    last_s: u32,
    v: [i8; HIST_S],
}

impl Row {
    const NEW: Row = Row { node: 0, used: false, last_s: 0, v: [EMPTY; HIST_S] };

    fn put(&mut self, s: u32, db: i8) {
        let db = db.max(EMPTY + 1);
        if !self.used {
            *self = Row { node: self.node, used: true, last_s: s, ..Row::NEW };
        } else if s > self.last_s {
            for k in (self.last_s + 1..=s).take(HIST_S) {
                self.v[k as usize % HIST_S] = EMPTY;
            }
            self.last_s = s;
        } else if self.last_s - s >= HIST_S as u32 {
            return;
        }
        let x = &mut self.v[s as usize % HIST_S];
        *x = (*x).max(db);
    }

    /// The level heard in second `s`.
    fn at(&self, s: u32) -> Option<i8> {
        if !self.used || s > self.last_s || self.last_s - s >= HIST_S as u32 {
            return None;
        }
        Some(self.v[s as usize % HIST_S]).filter(|&x| x != EMPTY)
    }
}

/// A steady change in one node's level ([`change`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Change {
    /// dB, positive louder.
    pub db: i16,
    /// Over how many seconds.
    pub over_s: u16,
    /// Louder than anything heard in the [`HIST_S`] before the window.
    pub new_high: bool,
}

fn med3(a: i16, b: i16, c: i16) -> i16 {
    a.max(b).min(a.min(b).max(c))
}

/// The steady change of a row's level at second `now_s`, either way, if
/// there is one (see the module notes for what "steady" means).
fn change(row: &Row, now_s: u32) -> Option<Change> {
    let mut pts: Vec<(u32, i16), { WINDOW_S as usize }> = Vec::new();
    for age in (0..WINDOW_S).rev() {
        if let Some(v) = now_s.checked_sub(age).and_then(|s| Some(s).zip(row.at(s))) {
            let _ = pts.push((v.0, i16::from(v.1)));
        }
    }
    let n = pts.len();
    if n < MIN_SAMPLES {
        return None;
    }
    let (first, last) = (pts[0].0, pts[n - 1].0);
    if now_s.saturating_sub(last) > FRESH_S || last - first < MIN_SPAN_S {
        return None;
    }
    // Medians of three: the first is the median of the first three samples,
    // the last that of the last three.
    let mut sm: Vec<i16, { WINDOW_S as usize }> = Vec::new();
    for i in 1..n - 1 {
        let _ = sm.push(med3(pts[i - 1].1, pts[i].1, pts[i + 1].1));
    }
    let (head, tail) = (sm[0], sm[sm.len() - 1]);
    let d = tail - head;
    if d.abs() < RISE_DB {
        return None;
    }
    let sign = d.signum();
    // Most pairs agree on the direction: tau = (agree - disagree) / pairs.
    let (mut agree, mut disagree) = (0i32, 0i32);
    for i in 0..n {
        for j in i + 1..n {
            match ((pts[j].1 - pts[i].1) * sign).signum() {
                1 => agree += 1,
                -1 => disagree += 1,
                _ => {}
            }
        }
    }
    let pairs = (n * (n - 1) / 2) as i32;
    if 2 * (agree - disagree) < pairs {
        return None;
    }
    // No one step makes most of it.
    let step = sm.windows(2).map(|w| (w[1] - w[0]) * sign).max().unwrap_or(0);
    if step * 3 > d.abs() * 2 {
        return None;
    }
    let older = (WINDOW_S..HIST_S as u32).filter_map(|age| now_s.checked_sub(age).and_then(|s| row.at(s))).max();
    Some(Change { db: d, over_s: (last - first) as u16, new_high: older.is_none_or(|o| tail > i16::from(o)) })
}

/// What a sound slot was last told.
#[derive(Debug, Clone, Copy, PartialEq)]
struct SoundFacts {
    first_ms: u64,
    low_hum: bool,
    aircraft: bool,
}

/// This second's findings for one slot.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct Work {
    /// The largest steady rise (on a node where it is a new high).
    rise: Option<Change>,
    /// Nodes rising, a mask of row indexes.
    rising: u8,
    /// Some node's level is falling steadily.
    falling: bool,
    /// Rule 13 holds.
    dark: bool,
}

/// One signal without a position, its levels per node and its threat level.
#[derive(Debug, Clone, PartialEq)]
pub struct SignalThreat {
    pub id: SignalThreatId,
    pub src: SignalSrc,
    pub first_ms: u64,
    pub last_ms: u64,
    /// Its level and reasons, moved as a drone's (rises at once, comes down
    /// [`super::DECAY_MS`] after the rule stops; snoozed as a drone's).
    pub threat: TrackThreat,
    rows: [Row; NODE_ROWS],
    sound: Option<SoundFacts>,
    work: Work,
}

impl SignalThreat {
    /// The nodes it was last heard rising on (at most [`NODE_ROWS`]).
    pub fn rising_nodes(&self) -> impl Iterator<Item = u8> + '_ {
        self.rows.iter().enumerate().filter(|(i, _)| self.work.rising & (1 << i) != 0).map(|(_, r)| r.node)
    }

    fn row_mut(&mut self, node: u8) -> &mut Row {
        let i = self
            .rows
            .iter()
            .position(|r| r.used && r.node == node)
            .or_else(|| self.rows.iter().position(|r| !r.used))
            .unwrap_or_else(|| (0..NODE_ROWS).min_by_key(|&i| self.rows[i].last_s).unwrap_or(0));
        let r = &mut self.rows[i];
        if !r.used || r.node != node {
            *r = Row { node, ..Row::NEW };
        }
        r
    }

    /// A sound slot that can count at all (not a low hum, not an aircraft).
    fn rotor(&self) -> bool {
        self.sound.is_some_and(|s| !s.low_hum && !s.aircraft)
    }
}

/// A signal's level rose (to Caution or above), for the evidence log and
/// the console.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalRise {
    pub id: SignalThreatId,
    pub level: Level,
    pub snoozed: bool,
}

/// What the rules without a position keep, in the tracker.
#[derive(Debug, Clone)]
pub struct Trends<const P: usize> {
    slots: Vec<SignalThreat, P>,
    next_id: u32,
    /// When a drone control link was last heard (any direction): rule 13's
    /// "radio heard" (the control table lives outside the tracker).
    last_control_ms: Option<u64>,
}

impl<const P: usize> Default for Trends<P> {
    fn default() -> Self {
        Self::new()
    }
}

/// What the judging needs from the rest of the tracker.
pub(crate) struct World<'a> {
    pub places: &'a Places,
    pub own: Option<&'a OwnFix>,
    /// Node positions (1e-7 degrees), by node number.
    pub nodes: &'a [Option<(i32, i32)>],
    pub last_radio_ms: Option<u64>,
    pub tracks: &'a [Track],
}

impl<const P: usize> Trends<P> {
    pub const fn new() -> Self {
        Self { slots: Vec::new(), next_id: 1, last_control_ms: None }
    }

    pub fn all(&self) -> &[SignalThreat] {
        &self.slots
    }

    pub fn get(&self, id: SignalThreatId) -> Option<&SignalThreat> {
        self.slots.iter().find(|s| s.id == id)
    }

    /// One level of `src` heard by `node`.
    pub(crate) fn note(&mut self, src: SignalSrc, node: u8, db: i8, now_ms: u64) -> Option<&mut SignalThreat> {
        if P == 0 {
            return None;
        }
        let i = match self.slots.iter().position(|s| s.src.same(&src)) {
            Some(i) => i,
            None => {
                if self.slots.is_full() {
                    // A slot with no level, heard longest ago, makes room.
                    let j = (0..self.slots.len()).min_by_key(|&j| (self.slots[j].threat.level.is_some(), self.slots[j].last_ms))?;
                    self.slots.swap_remove(j);
                }
                let id = SignalThreatId((self.next_id & !SIGNAL_TRACK_BIT).max(1));
                self.next_id = id.0 + 1;
                let _ = self.slots.push(SignalThreat {
                    id,
                    src,
                    first_ms: now_ms,
                    last_ms: now_ms,
                    threat: TrackThreat::default(),
                    rows: [Row::NEW; NODE_ROWS],
                    sound: None,
                    work: Work::default(),
                });
                self.slots.len() - 1
            }
        };
        let s = &mut self.slots[i];
        s.src = src;
        s.last_ms = s.last_ms.max(now_ms);
        s.row_mut(node).put((now_ms / 1000) as u32, db);
        Some(s)
    }

    /// Once a second, after the drones: rules 12, 13 and 14 (for a striker
    /// without a position) for every slot, and each slot's level moved.
    #[inline(never)]
    pub(crate) fn judge(&mut self, w: &World<'_>, across: &mut Across<P>, now_ms: u64) {
        let now_s = (now_ms / 1000) as u32;
        // First what each slot shows by itself...
        for s in self.slots.iter_mut() {
            s.work = Work::default();
            let heard = now_ms.saturating_sub(s.last_ms) <= HEARD_MS;
            if !heard || (s.sound.is_some() && !s.rotor()) {
                continue;
            }
            for (i, r) in s.rows.iter().enumerate() {
                let Some(c) = change(r, now_s) else { continue };
                if c.db < 0 {
                    s.work.falling = true;
                } else if c.new_high {
                    s.work.rising |= 1 << i;
                    if s.work.rise.is_none_or(|b| c.db > b.db) {
                        s.work.rise = Some(c);
                    }
                }
            }
            if let Some(f) = s.sound.filter(|_| s.rotor()) {
                s.work.dark = s.last_ms.saturating_sub(f.first_ms) >= DARK_AFTER_MS
                    && w.last_radio_ms.is_none_or(|r| r.saturating_add(RADIO_RECENT_MS) < f.first_ms);
            }
        }
        let louder = |s: &SignalThreat| s.work.rise.is_some() && !s.work.falling;
        // ...then what they say together: the microphone and the radio agree.
        let radio_louder = self.slots.iter().any(|s| s.sound.is_none() && louder(s));
        // A sound still heard whose "dark drone" level is held counts too:
        // the radio signal that appeared is what ended rule 13 (rule 12's
        // "Warning with 13").
        let held_dark = |s: &SignalThreat| {
            now_ms.saturating_sub(s.last_ms) <= HEARD_MS && s.threat.reasons.iter().any(|r| r.rule == Rule::DarkDrone)
        };
        let sound_agrees = self.slots.iter().any(|s| s.rotor() && (louder(s) || s.work.dark || held_dark(s)));
        for s in self.slots.iter_mut() {
            let mut hits: Vec<Reason, 4> = Vec::new();
            let mut where_: Vec<Name, 4> = Vec::new();
            if s.sound.is_some() && (s.work.dark || s.work.rise.is_some()) {
                places_for(w, MIC_NODE, &mut where_);
            } else {
                for node in s.rising_nodes() {
                    places_for(w, node, &mut where_);
                }
            }
            let at = |rule: Rule, level: Level| {
                let mut r = Reason::new(rule, level, where_.first().unwrap_or(&Name::new()), 0.0);
                r.eta_ds = s.src.kind() as u16 | if where_.is_empty() { NO_PLACE } else { 0 };
                r
            };
            if let Some(c) = s.work.rise.filter(|_| louder(s)) {
                let nodes = s.work.rising.count_ones() as u16;
                let agrees = if s.sound.is_some() { radio_louder } else { sound_agrees };
                let level = if agrees || nodes >= 2 { Level::Warning } else { Level::Caution };
                let mut r = at(Rule::GettingLouder, level);
                r.range_m = c.db.unsigned_abs();
                r.secs = c.over_s;
                r.speed_dms = nodes;
                if agrees {
                    r.eta_ds |= WITH_OTHER;
                }
                let _ = hits.push(r);
                // 14: a striker without a position (not a sound: the
                // spotter's own rotors may be what is getting louder).
                if s.sound.is_none() {
                    let me = TrackId(SIGNAL_TRACK_BIT | s.id.0);
                    let key = 1u64 << 62 | u64::from(s.id.0) << 1;
                    let ok = |spotter: TrackId| !closing_itself(w.tracks, spotter, now_ms);
                    for name in where_.iter() {
                        if let Some((spotter, ago)) = across.striker_if(me, key, None, name, now_ms, &ok) {
                            // At the moving bubble, as a slow positional striker
                            // there: Warning; at a fixed area, Critical.
                            let level = if name.is_empty() { Level::Warning } else { Level::Critical };
                            let mut r = Reason::new(Rule::SpotterStriker, level, name, 0.0).with_secs(ago).with_other(spotter);
                            r.range_m = NONE_U16;
                            let _ = hits.push(r);
                            break;
                        }
                    }
                }
            }
            if s.work.dark {
                let since = s.sound.map_or(s.first_ms, |f| f.first_ms);
                let _ = hits.push(at(Rule::DarkDrone, Level::Warning).with_secs(s.last_ms.saturating_sub(since)));
            }
            let e = (!hits.is_empty()).then(|| {
                let mut e: Eval = super::judge(hits, false);
                e.heard_ms = s.last_ms;
                e
            });
            s.threat.update(e, false, now_ms);
        }
        self.slots.retain(|s| s.threat.level.is_some() || now_ms.saturating_sub(s.last_ms) < FORGET_MS);
    }

    /// When a control link was last heard (rule 13).
    pub(crate) fn control_heard(&mut self, now_ms: u64) {
        self.last_control_ms = Some(self.last_control_ms.map_or(now_ms, |m| m.max(now_ms)));
    }
}

/// A rule-12 or 13 reason has no place (none fitted the node).
const NO_PLACE: u16 = 0x20;
/// A rule-12 reason the other sense agrees with (the microphone for a radio
/// signal, a radio signal for a sound).
const WITH_OTHER: u16 = 0x10;

/// The spotter is still heard and itself heading in (a beeline or fast
/// inbound now): the signal getting louder may well be its own.
fn closing_itself(tracks: &[Track], spotter: TrackId, now_ms: u64) -> bool {
    tracks.iter().any(|t| {
        t.id == spotter
            && now_ms.saturating_sub(t.last_seen_ms) <= super::FRESH_MS
            && t.threat.reasons.iter().any(|r| matches!(r.rule, Rule::Beeline | Rule::FastInbound))
    })
}

/// The place(s) a signal rising at `node` closes on (see the module notes),
/// added to `out` (no repeats). The bubble is the empty name.
fn places_for(w: &World<'_>, node: u8, out: &mut Vec<Name, 4>) {
    let mut add = |n: &Name| {
        if !out.contains(n) {
            let _ = out.push(n.clone());
        }
    };
    let placed = (node != 0 && node != MIC_NODE)
        .then(|| w.nodes.get(usize::from(node)).copied().flatten())
        .flatten()
        .map(|(a, b)| (f64::from(a) / 1e7, f64::from(b) / 1e7));
    let bubble = w.places.bubble_m.zip(w.own);
    if placed.is_none() && bubble.is_some() {
        return add(&Name::new());
    }
    let mut found = false;
    if let Some(p) = placed.or(w.own.map(|o| (o.lat, o.lon))) {
        let mut nearest: Option<(f32, &Name)> = None;
        for a in w.places.areas.iter() {
            let Some(shape) = &a.shape else { continue };
            let l = Local::of(shape);
            let xy = l.frame.xy(p.0, p.1);
            if l.contains(xy) {
                add(&a.name);
                found = true;
            } else {
                let o = l.outside_by(xy);
                if o <= l.size_m.max(NEAR_MIN_M) && nearest.is_none_or(|n| o < n.0) {
                    nearest = Some((o, &a.name));
                }
            }
        }
        if !found && let Some((_, n)) = nearest {
            add(n);
            found = true;
        }
        if !found
            && let (Some(_), Some((r, o))) = (placed, bubble)
        {
            let (x, y) = Frame::at(o.lat, o.lon).xy(p.0, p.1);
            if libm::sqrtf(x * x + y * y) <= r {
                add(&Name::new());
                found = true;
            }
        }
    }
    // At the hub, with no position: the only area, if there is one.
    let mut shaped = w.places.areas.iter().filter(|a| a.shape.is_some());
    if !found
        && placed.is_none()
        && let (Some(a), None) = (shaped.next(), shaped.next())
    {
        add(&a.name);
    }
}

impl Reason {
    /// Rules 12 and 13: what kind of signal.
    pub fn signal_kind(&self) -> Option<SignalKind> {
        matches!(self.rule, Rule::GettingLouder | Rule::DarkDrone).then(|| SignalKind::of_code(self.eta_ds & 0xF)).flatten()
    }

    /// Rule 12: how many dB it rose.
    pub fn db(&self) -> Option<u16> {
        (self.rule == Rule::GettingLouder).then_some(self.range_m)
    }

    /// Rule 12, a radio signal: on how many nodes at once.
    pub fn nodes(&self) -> Option<u16> {
        (self.rule == Rule::GettingLouder && self.signal_kind() != Some(SignalKind::Sound)).then_some(self.speed_dms)
    }

    /// Rule 12: the other sense agrees (the microphone for a radio signal,
    /// a radio signal getting louder for a sound).
    pub fn agreed(&self) -> bool {
        self.rule == Rule::GettingLouder && self.eta_ds & WITH_OTHER != 0
    }

    /// Rules 12 and 13: no place fitted the node that heard it.
    pub fn no_place(&self) -> bool {
        matches!(self.rule, Rule::GettingLouder | Rule::DarkDrone) && self.eta_ds & NO_PLACE != 0
    }

    /// Whether `range_m` is a distance (rules 12 and 13 have none, nor has
    /// a striker without a position).
    pub fn has_range(&self) -> bool {
        !matches!(self.rule, Rule::GettingLouder | Rule::DarkDrone) && self.range_m != NONE_U16
    }
}

// ------------------------------------------------------------- wording --

fn plural(n: u16) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Rules 12 and 13 in words (called from [`super::write_reason_named`]).
pub(crate) fn write_trend(w: &mut impl Write, r: &Reason) -> core::fmt::Result {
    let kind = r.signal_kind().unwrap_or(SignalKind::Video);
    let place = |w: &mut dyn Write, verb: &str| -> core::fmt::Result {
        if r.no_place() {
            return Ok(());
        }
        w.write_str(verb)?;
        if r.is_bubble() { w.write_str("you") } else { w.write_str(r.place()) }
    };
    match r.rule {
        Rule::GettingLouder => {
            w.write_str(kind.words())?;
            w.write_str(" getting louder")?;
            match r.nodes() {
                Some(n) => write!(w, " fast on {n} node{}", plural(n))?,
                None => w.write_str(" fast")?,
            }
            write!(w, " (+{} dB in {} s)", r.range_m.min(999), r.secs.min(999))?;
            place(w, ", nearing ")?;
            if r.agreed() {
                w.write_str(if kind == SignalKind::Sound { ", a radio signal too" } else { ", rotor sound too" })?;
            }
            Ok(())
        }
        _ => {
            w.write_str("rotor sound, no radio heard, for ")?;
            super::duration_text(w, r.for_s())?;
            place(w, ", near ")
        }
    }
}

/// The evidence and console text for a signal's level: `Warning: Analog
/// video 5740.0 MHz (no position): FPV video signal getting louder fast on
/// 2 nodes (+8 dB in 7 s), nearing house`. `name_of` names a spotter (its
/// ID as heard over the air: the hub cleans the text).
pub fn describe_signal<'a>(s: &SignalThreat, now_ms: u64, name_of: impl Fn(TrackId) -> Option<&'a str>) -> String<256> {
    let mut o: String<256> = String::new();
    let th = &s.threat;
    let _ = write!(o, "{}: {} (no position)", th.level.map_or("No level", |l| l.label()), s.src);
    for (i, r) in th.reasons.iter().enumerate() {
        let _ = o.push_str(if i == 0 { ": " } else { "; " });
        let _ = super::write_reason_named(&mut o, r, r.other().and_then(&name_of));
    }
    if th.snoozed(now_ms) {
        let _ = o.push_str(" (snoozed)");
    }
    o
}

/// The screen strip for a signal: `WARNING FPV video louder fast, 2 nodes,
/// house`, `WARNING rotor sound, no radio heard`, `CRITICAL FPV video after
/// spotter to house`.
pub fn strip_text_signal(s: &SignalThreat) -> Option<String<64>> {
    let th = &s.threat;
    let level = th.level?;
    let r = th.reasons.first()?;
    let mut o: String<64> = String::new();
    let _ = write!(o, "{} ", level.upper());
    let kind = s.src.kind();
    let place = |o: &mut String<64>| {
        let _ = match (r.no_place(), r.is_bubble()) {
            (true, _) => Ok(()),
            (false, true) => o.push_str(", you"),
            (false, false) => o.push_str(", ").and_then(|_| o.push_str(r.place())),
        };
    };
    match r.rule {
        Rule::GettingLouder => {
            let _ = write!(o, "{} louder fast", kind.short());
            if let Some(n) = r.nodes().filter(|&n| n > 1) {
                let _ = write!(o, ", {n} nodes");
            }
            place(&mut o);
        }
        Rule::DarkDrone => {
            let _ = o.push_str("rotor sound, no radio heard");
        }
        Rule::SpotterStriker => {
            let _ = write!(o, "{} after spotter to ", kind.short());
            let _ = if r.is_bubble() { o.push_str("you") } else { o.push_str(r.place()) };
        }
        _ => {
            let _ = o.push_str(kind.short());
        }
    }
    Some(o)
}

/// Evidence `threat_level` data for a signal: as [`super::rise_data`], the
/// id being the signal's [`SignalThreatId`] and flag bit 4 set ("no
/// position: a signal, not a drone track").
pub fn signal_rise_data(s: &SignalThreat, r: &SignalRise) -> [u8; 10] {
    let mut d = [0u8; 10];
    d[0] = r.level as u8;
    d[1..5].copy_from_slice(&r.id.0.to_le_bytes());
    for (i, reason) in s.threat.reasons.iter().take(4).enumerate() {
        d[5 + i] = reason.rule as u8;
    }
    d[9] = u8::from(r.snoozed) | u8::from(s.threat.combined) << 2 | 1 << 4;
    d
}

// ------------------------------------------------------------ the tracker --

impl<const N: usize, const A: usize, const P: usize> Tracker<N, A, P> {
    /// Signals without a position and their threat levels (rules 12, 13 and
    /// 14 for a striker without a position). Empty on hubs without path
    /// history.
    pub fn signal_threats(&self) -> &[SignalThreat] {
        self.trends.all()
    }

    pub fn get_signal_threat(&self, id: SignalThreatId) -> Option<&SignalThreat> {
        self.trends.get(id)
    }

    /// A rotor-sound contact heard now (rule 13 and the sound half of rule
    /// 12): call about once a second for each contact the sound detector
    /// hears, with its latest level.
    pub fn hear_sound(&mut self, c: &RotorSound, now_ms: u64) {
        if let Some(s) = self.trends.note(SignalSrc::Sound { id: c.id }, MIC_NODE, c.level_db, now_ms) {
            s.first_ms = s.first_ms.min(c.first_ms);
            s.last_ms = s.last_ms.max(c.last_ms);
            s.sound = Some(SoundFacts { first_ms: c.first_ms, low_hum: c.low_hum, aircraft: c.aircraft });
        }
    }

    /// A control-link packet's level (from [`crate::ctrllinks::ControlTable`],
    /// which the hub keeps outside the tracker). Only the drone's own
    /// packets (downlink, or direction unknown) of a link at least "likely"
    /// count for rule 12; every link heard counts as radio for rule 13.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn note_control_level(
        &mut self,
        id: ControlLinkId,
        system: ctrlwatch::System,
        region: ctrlwatch::Region,
        node: u8,
        rssi_dbm: i16,
        direction: ctrlwatch::Direction,
        best: ctrlwatch::Confidence,
        now_ms: u64,
    ) {
        if best < ctrlwatch::Confidence::Likely {
            return;
        }
        self.trends.control_heard(now_ms);
        if direction != ctrlwatch::Direction::Uplink {
            let db = rssi_dbm.clamp(i16::from(i8::MIN), i16::from(i8::MAX)) as i8;
            let _ = self.trends.note(SignalSrc::Control { id, system, region }, node, db, now_ms);
        }
    }

    /// When a drone was last heard by radio (rule 13): a confirmed drone
    /// track (not one whose position is over [`SOUND_EXPLAINS_M`] from the
    /// hub), an open FPV link, an RF emitter of a kind that alerts, a Wi-Fi
    /// drone or a control link.
    pub fn last_radio_drone_ms(&self, own: Option<&OwnFix>) -> Option<u64> {
        let mut last = self.trends.last_control_ms;
        let mut see = |ms: u64| last = Some(last.map_or(ms, |l| l.max(ms)));
        for t in self.tracks.iter().filter(|t| t.confirmed()) {
            let far = own.zip(t.location.as_ref().and_then(|l| l.value.position)).is_some_and(|(o, p)| {
                let (x, y) = Frame::at(o.lat, o.lon).xy(p.lat, p.lon);
                libm::sqrtf(x * x + y * y) > SOUND_EXPLAINS_M
            });
            if !far {
                see(t.last_seen_ms);
            }
        }
        self.links.tracks().iter().for_each(|l| see(l.last_seen_ms));
        self.emitters.all().iter().filter(|e| crate::signals::rf_alerts(e.kind)).for_each(|e| see(e.last_seen_ms));
        self.wifi_drones.all().iter().for_each(|w| see(w.last_seen_ms));
        last
    }

    /// The next signal whose level rose to Caution or above, once per rise.
    pub fn take_signal_rise(&mut self, now_ms: u64) -> Option<SignalRise> {
        let s = self.trends.slots.iter_mut().find(|s| s.threat.rise_pending())?;
        s.threat.take_rise();
        Some(SignalRise { id: s.id, level: s.threat.level.unwrap_or(Level::Notice), snoozed: s.threat.snoozed(now_ms) })
    }

    /// The evidence and console text for a signal's level
    /// ([`describe_signal`]).
    pub fn describe_signal_level(&self, id: SignalThreatId, now_ms: u64) -> Option<String<256>> {
        let s = self.trends.get(id)?;
        Some(describe_signal(s, now_ms, |o| self.get(o).and_then(|t| t.primary_id()).and_then(|u| u.id.as_str())))
    }

    /// The most serious signal not snoozed, at Caution or above (the screen
    /// strip), the most recent rise first among equals.
    pub fn top_signal_threat(&self, now_ms: u64) -> Option<&SignalThreat> {
        self.trends
            .all()
            .iter()
            .filter(|s| s.threat.level >= Some(Level::Caution) && !s.threat.snoozed(now_ms))
            .max_by_key(|s| (s.threat.level, s.threat.since_ms))
    }

    /// The screen's Snooze for signals: every one at Warning or above.
    pub fn snooze_signal_levels(&mut self, now_ms: u64) -> Vec<SignalThreatId, P> {
        let mut out = Vec::new();
        for s in self.trends.slots.iter_mut() {
            if s.threat.level >= Some(Level::Warning) && !s.threat.snoozed(now_ms) {
                s.threat.snooze(now_ms);
                let _ = out.push(s.id);
            }
        }
        out
    }
}
