//! Threat levels: what each drone is *doing*, judged against the places
//! the owner protects (docs/THREAT-PROFILES.md, slices 16a, 16b and 16d).
//!
//! Every drone that reports its position (Remote ID, DJI DroneID, MAVLink
//! telemetry) gets one level, the highest any rule gives it, and up to
//! [`MAX_REASONS`] reasons with their numbers:
//!
//! | Level | Meaning |
//! |---|---|
//! | Notice | a drone is around, nothing about it is unusual |
//! | Caution | something worth watching |
//! | Warning | behaviour that matches a known hostile pattern |
//! | Critical | something may arrive within seconds |
//!
//! Rules in this slice (numbered as in the design):
//! 1. **In the area** (Caution): inside a fixed area or the bubble.
//! 2. **Pilot in or near the area** (Caution): the broadcast pilot position
//!    is inside, or within [`PILOT_NEAR_M`] of the edge.
//! 3. **Drone in, pilot outside** (Caution).
//! 4. **Above the ceiling** (Caution): inside an area and higher above the
//!    unit than the area's ceiling (120 m unless the site file says).
//! 5. **Beeline** (Warning): heading for the place on a fairly straight
//!    path, predicted to reach it within [`LOOKAHEAD_S`]. "Fairly straight"
//!    comes from the path history ([`path`]), so a drone flying straight
//!    *past* (its closest approach is outside) or circling does not count.
//! 6. **Fast inbound** (Critical): as 5, closing faster than [`FAST_MPS`]
//!    or arriving in under [`SOON_S`].
//!
//! Slice 16b adds the lingering rules, worked out from the path history
//! ([`linger`], [`follow`]; drones without a history get none of them):
//! 7. **Loitering** (Caution; Warning when long and low): staying in one
//!    spot for minutes, in or just outside a place.
//! 8. **Circling** (Caution): a full circle round a point in a place, or a
//!    circle that reaches into one.
//! 9. **Low hover** (Warning): inside a place, low and nearly still.
//! 10. **Drop pattern** (Warning): inside a place, down, a pause, up again.
//! 11. **Following** (Warning): keeping near the unit while it travels (the
//!     bubble only).
//!
//! Slice 16d adds the rules across drones and days ([`across`], with the
//! repeat-visitor history in [`visits`] and the pilot's trail in [`watch`]):
//! 14. **Spotter then striker** (Critical): a different drone heads for a
//!     place within 10 min of another lingering there.
//! 15. **Several at once** (Warning): 3 or more drones at the places, or a
//!     drone at a place during jamming or GPS interference.
//! 16. **Probing** (Caution): back in an area it left 30 s to 30 min ago.
//! 17. **Repeat visitor**: the same drone or pilot spot on 3 or more days
//!     in two weeks; raises the other reasons by one level (to Warning at
//!     most), a reason at Notice on its own.
//! 18. **Pilot on the move** (Caution): the pilot moving over 5 m/s.
//! 19. **Identity changes** (Warning): a new ID mid-flight near a place.
//!
//! Slice 16c adds the rules without a position ([`trend`]), for signals
//! that are not drone tracks (FPV video, control-link downlinks, drone
//! Wi-Fi, open FPV links) and rotor sound; each gets its own level
//! ([`trend::SignalThreat`]), moved by the same policy:
//! 12. **Getting louder fast** (Caution; Warning on two nodes at once or
//!     with the microphone agreeing): a steady rise of 6 dB in 5-12 s.
//! 13. **Dark drone** (Warning): rotor sound with no drone heard by radio.
//!
//! A rising signal at a place a spotter lingered at is a striker (14).
//!
//! Two or more *different* Caution rules at once make a Warning (one sign
//! rarely proves anything; two together are worth more), with two
//! exceptions: loitering is the same sign as being in the area (it is
//! being there, for longer), and in the bubble a pilot near you does not
//! count towards the pair (a pilot nearby is normal in a park).
//!
//! **How the level moves** (human-factors choices, see the design):
//! - It goes up at once and comes down only after [`DECAY_MS`] without the
//!   rule that raised it, so it does not flicker.
//! - A drone at Warning or Critical that stops being heard while it was
//!   heading in keeps its level for [`DECAY_MS`], with the reason "last
//!   seen inbound" (losing a low drone behind trees is when it matters).
//! - A `friend` is lowered, never hidden: rules 1-4 and 7-11 give a friend
//!   only Notice (a follow-me selfie drone is the classic friendly case),
//!   but rules 5 and 6 are never lowered (serials can be faked).
//! - A drone's level can be snoozed for [`SNOOZE_MS`] (the alert stops;
//!   the level is still shown and logged), never switched off.
//!
//! **Positions are rough** (Remote ID promises 30 m at 95 %): the area
//! checks use the average of the last [`SMOOTH_POINTS`] positions (moved on
//! along the drone's velocity to now), a drone counted inside stays inside
//! until it is [`EDGE_MARGIN_M`] beyond the edge, and the approach rules
//! need the path to have been straight for 10 s (5 s when fast).
//!
//! Only confirmed tracks ([`crate::probation`]) with a position heard in
//! the last [`FRESH_MS`] are judged; a drone with no position gets no level.

pub mod across;
pub mod follow;
pub mod linger;
pub mod path;
pub mod places;
pub mod profile;
pub mod scenario;
pub mod trend;
pub mod visits;
pub mod watch;

use core::fmt::Write;

use heapless::{String, Vec};
use libm::{acosf, sqrtf};

pub use across::{Across, Cross};
pub use path::{OwnTrail, PathPoint, PathRing, PathStore};
pub use trend::{RotorSound, SignalKind, SignalRise, SignalSrc, SignalThreat, SignalThreatId, Trends};
pub use visits::Visits;
pub use places::{Area, Frame, Friend, Local, Name, Places, Shape, Vertex};
pub use profile::{Profile, Tuning};

use crate::{OwnFix, Track, TrackId};

/// A position older than this is not judged (Remote ID repeats its
/// position every second).
pub const FRESH_MS: u64 = 5_000;
/// A level comes down only after this long without the rule that raised
/// it; a drone lost while heading in keeps its level this long.
pub const DECAY_MS: u64 = 30_000;
/// How far ahead the approach rules look.
pub const LOOKAHEAD_S: f32 = 60.0;
/// Closing faster than this is "fast inbound" (camera drones top out near
/// 16-21 m/s; FPV attack drones fly 40 m/s and more).
pub const FAST_MPS: f32 = 25.0;
/// Arriving sooner than this is "fast inbound", when also closing at
/// [`SOON_MIN_MPS`] or more.
pub const SOON_S: f32 = 30.0;
/// A drone arriving soon but slower than this (a survey drone passing over
/// at walking or cycling pace) stays at Warning: Critical is for things
/// that may hit within seconds.
pub const SOON_MIN_MPS: f32 = 10.0;
/// Slower than this relative to the place is not an approach.
pub const MIN_APPROACH_MPS: f32 = 3.0;
/// A pilot this close outside an area counts as "at the fence".
pub const PILOT_NEAR_M: f32 = 30.0;
/// A pilot position older than this is not used.
pub const PILOT_FRESH_MS: u64 = 60_000;
/// The straightness check looks at this much of the path.
pub const STRAIGHT_WINDOW_MS: u64 = 20_000;
/// It needs at least this much path (less for a fast drone).
pub const STRAIGHT_SPAN_MS: u64 = 10_000;
pub const STRAIGHT_SPAN_FAST_MS: u64 = 5_000;
/// Faster than this (m/s), the shorter span is enough.
pub const STRAIGHT_FAST_MPS: f32 = 15.0;
/// The two halves of the path may differ in direction by this much.
pub const MAX_TURN_DEG: f32 = 25.0;
/// The path and the reported direction of travel may differ by this much.
pub const MAX_COURSE_OFF_DEG: f32 = 30.0;
/// Each half of the path must cover at least this much ground.
pub const MIN_LEG_M: f32 = 15.0;
/// The unit is standing still below this speed.
pub const OWN_STATIONARY_MPS: f32 = 0.5;
/// How long a snooze lasts (as the attack alarms' snooze).
pub const SNOOZE_MS: u64 = crate::threats::SNOOZE_MS;
/// Reasons kept per drone.
pub const MAX_REASONS: usize = 4;
/// Positions averaged for the area checks (one a second).
pub const SMOOTH_POINTS: usize = 5;
/// A drone counted inside a place stays inside until it is this far beyond
/// its edge, so one hovering at the fence does not go in and out.
pub const EDGE_MARGIN_M: f32 = 10.0;

/// How worrying a drone is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Level {
    Notice = 1,
    Caution = 2,
    Warning = 3,
    Critical = 4,
}

impl Level {
    /// `"notice"`, ... (JSON).
    pub const fn name(self) -> &'static str {
        match self {
            Level::Notice => "notice",
            Level::Caution => "caution",
            Level::Warning => "warning",
            Level::Critical => "critical",
        }
    }

    /// `"Notice"`, ... (text).
    pub const fn label(self) -> &'static str {
        match self {
            Level::Notice => "Notice",
            Level::Caution => "Caution",
            Level::Warning => "Warning",
            Level::Critical => "Critical",
        }
    }

    /// `"NOTICE"`, ... (the screen).
    pub const fn upper(self) -> &'static str {
        match self {
            Level::Notice => "NOTICE",
            Level::Caution => "CAUTION",
            Level::Warning => "WARNING",
            Level::Critical => "CRITICAL",
        }
    }
}

/// Which rule gave a reason (numbered as in docs/THREAT-PROFILES.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Rule {
    InArea = 1,
    PilotNear = 2,
    PilotOutside = 3,
    AboveCeiling = 4,
    Beeline = 5,
    FastInbound = 6,
    Loitering = 7,
    Circling = 8,
    LowHover = 9,
    Drop = 10,
    Following = 11,
    /// Slice 16c ([`trend`]): a signal without a position getting louder
    /// fast, or rotor sound getting louder (on a signal, not a track).
    GettingLouder = 12,
    /// Slice 16c: rotor sound with no drone heard by radio (on a sound).
    DarkDrone = 13,
    /// Slice 16d ([`across`]): a different drone heads for a place soon
    /// after a spotter lingered there (on the striker).
    SpotterStriker = 14,
    Several = 15,
    Probing = 16,
    RepeatVisitor = 17,
    PilotMoving = 18,
    IdentityChanged = 19,
    /// Not a rule of its own: a Warning or Critical drone lost while it
    /// was heading in, held for [`DECAY_MS`].
    LastSeenInbound = 20,
    /// Part of rule 14, on the spotter: a striker followed it.
    Spotter = 21,
}

impl Rule {
    pub const fn name(self) -> &'static str {
        match self {
            Rule::InArea => "in_area",
            Rule::PilotNear => "pilot_near",
            Rule::PilotOutside => "pilot_outside",
            Rule::AboveCeiling => "above_ceiling",
            Rule::Beeline => "beeline",
            Rule::FastInbound => "fast_inbound",
            Rule::Loitering => "loitering",
            Rule::Circling => "circling",
            Rule::LowHover => "low_hover",
            Rule::Drop => "drop",
            Rule::Following => "following",
            Rule::GettingLouder => "getting_louder",
            Rule::DarkDrone => "dark_drone",
            Rule::SpotterStriker => "spotter_striker",
            Rule::Several => "several_at_once",
            Rule::Probing => "probing",
            Rule::RepeatVisitor => "repeat_visitor",
            Rule::PilotMoving => "pilot_moving",
            Rule::IdentityChanged => "identity_changed",
            Rule::LastSeenInbound => "last_seen_inbound",
            Rule::Spotter => "spotter",
        }
    }

    /// The level the rule gives (loitering: at first; it rises to Warning
    /// when long and low; a repeat visitor: on its own, else it raises).
    pub const fn level(self) -> Level {
        match self {
            Rule::InArea | Rule::PilotNear | Rule::PilotOutside | Rule::AboveCeiling => Level::Caution,
            Rule::Loitering | Rule::Circling | Rule::Probing | Rule::PilotMoving => Level::Caution,
            Rule::GettingLouder => Level::Caution,
            Rule::DarkDrone => Level::Warning,
            Rule::LowHover | Rule::Drop | Rule::Following => Level::Warning,
            Rule::Beeline | Rule::Several | Rule::IdentityChanged | Rule::Spotter => Level::Warning,
            Rule::FastInbound | Rule::SpotterStriker => Level::Critical,
            Rule::LastSeenInbound => Level::Warning,
            Rule::RepeatVisitor => Level::Notice,
        }
    }

    /// Rules about places and lingering (1-4, 7-11; from 16d also several
    /// at once, probing and pilot on the move): a friend gets only Notice
    /// from them. The approach rules (5, 6, 14 on the striker) and identity
    /// changes (19) are never lowered. Rules 12 and 13 are about signals no
    /// `friend` line can name, so they are never lowered either.
    pub const fn is_place_rule(self) -> bool {
        matches!(
            self,
            Rule::InArea
                | Rule::PilotNear
                | Rule::PilotOutside
                | Rule::AboveCeiling
                | Rule::Loitering
                | Rule::Circling
                | Rule::LowHover
                | Rule::Drop
                | Rule::Following
                | Rule::Several
                | Rule::Probing
                | Rule::PilotMoving
        )
    }
}

/// One reason for a level, with its numbers. Which numbers mean what:
/// - in area: `range_m` from the place's middle (from the unit, for the
///   bubble); `height_m` above the unit when known.
/// - pilot near / pilot outside: `range_m` the pilot is outside the edge
///   (0 inside).
/// - above ceiling: `height_m` above the unit; `range_m` from the middle.
/// - beeline / fast inbound: `range_m` from the middle, `eta_s` until it
///   reaches the place (the edge, or the middle when already inside),
///   `speed_mps` how fast it is closing.
/// - last seen inbound: the beeline's numbers when last heard, and
///   `ago_s` since then.
/// - loitering: `range_m` from the middle, `height_m` above the unit,
///   `for_s` how long it has stayed.
/// - circling: `range_m` the circle's radius, `height_m`, `for_s` how long
///   the last full turn took.
/// - low hover: `range_m` from the middle, `height_m`, `for_s` how long it
///   has been low and still.
/// - drop: `range_m` how far it came down, `height_m` the bottom's height
///   above the unit, `ago_s` since it left the bottom.
/// - following: `range_m` how far the unit has travelled with it near,
///   `for_s` for how long.
/// - slice 16d (all but the striker's: `range_m` from the middle of the
///   place it is at or near):
///   - spotter then striker: the striker's distance, `ago_s` since the
///     spotter last lingered, [`Reason::other`] the spotter;
///   - spotter: [`Reason::other`] the striker;
///   - several at once: [`Reason::count`] drones, or [`Reason::alarm`];
///   - probing: `for_s` how long it was away;
///   - repeat visitor: [`Reason::count`] days, [`Reason::by_pilot_spot`];
///   - pilot on the move: `speed_mps` the pilot's speed, `for_s` over how
///     long;
///   - identity changes: `ago_s` since the change, [`Reason::other`] the
///     earlier track (none when the serial changed within this track).
///
/// Kept compact (26 bytes: whole metres, tenths of seconds and of m/s),
/// because every track holds up to [`MAX_REASONS`] and the S3 hub keeps
/// its tracker in internal RAM.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reason {
    pub rule: Rule,
    /// What this rule gave this drone (Notice for a friend's place rules).
    pub level: Level,
    /// The area's name, zero-padded; all zero for the bubble.
    place: [u8; places::NAME_LEN],
    range_m: u16,
    /// Tenths of a second; [`NONE_U16`] unknown.
    eta_ds: u16,
    /// Tenths of a m/s; [`NONE_U16`] unknown.
    speed_dms: u16,
    /// Whole metres; `i16::MIN` unknown.
    height_m: i16,
    /// Seconds: `ago_s` or `for_s` (see the list above); [`NONE_U16`]
    /// unknown.
    secs: u16,
}

const NONE_U16: u16 = u16::MAX;

fn tenths(v: f32) -> u16 {
    libm::roundf(v * 10.0).clamp(0.0, f32::from(NONE_U16 - 1)) as u16
}

impl Reason {
    fn new(rule: Rule, level: Level, place: &Name, range_m: f32) -> Self {
        let mut p = [0u8; places::NAME_LEN];
        p[..place.len()].copy_from_slice(place.as_bytes());
        Self {
            rule,
            level,
            place: p,
            // 65535 is "no distance" (rules 12 and 13, a striker without a
            // position).
            range_m: libm::roundf(range_m).clamp(0.0, f32::from(NONE_U16 - 1)) as u16,
            eta_ds: NONE_U16,
            speed_dms: NONE_U16,
            height_m: i16::MIN,
            secs: NONE_U16,
        }
    }

    fn with_secs(mut self, ms: u64) -> Self {
        self.secs = (ms / 1000).min(u64::from(NONE_U16 - 1)) as u16;
        self
    }

    fn with_eta(mut self, s: f32) -> Self {
        self.eta_ds = tenths(s);
        self
    }

    fn with_speed(mut self, mps: f32) -> Self {
        self.speed_dms = tenths(mps);
        self
    }

    fn with_height(mut self, m: Option<f32>) -> Self {
        self.height_m = m.map_or(i16::MIN, |m| libm::roundf(m).clamp(-32_767.0, 32_767.0) as i16);
        self
    }

    /// The area's name; empty for the bubble.
    pub fn place(&self) -> &str {
        let n = self.place.iter().position(|&b| b == 0).unwrap_or(self.place.len());
        core::str::from_utf8(&self.place[..n]).unwrap_or("")
    }

    /// The bubble round the unit (not a named area).
    pub fn is_bubble(&self) -> bool {
        self.place[0] == 0
    }

    /// Metres (see the list above for what it measures).
    pub fn range_m(&self) -> f32 {
        f32::from(self.range_m)
    }

    pub fn eta_s(&self) -> Option<f32> {
        (self.eta_ds != NONE_U16).then(|| f32::from(self.eta_ds) / 10.0)
    }

    pub fn speed_mps(&self) -> Option<f32> {
        (self.speed_dms != NONE_U16).then(|| f32::from(self.speed_dms) / 10.0)
    }

    pub fn height_m(&self) -> Option<f32> {
        (self.height_m != i16::MIN).then(|| f32::from(self.height_m))
    }

    /// Seconds since (last seen inbound, drop).
    pub fn ago_s(&self) -> Option<u16> {
        (self.secs != NONE_U16).then_some(self.secs)
    }

    /// Seconds for (loitering, circling, low hover, following).
    pub fn for_s(&self) -> Option<u16> {
        self.ago_s()
    }
}

/// What the rules said about one drone at one moment.
#[derive(Debug, Clone, PartialEq)]
pub struct Eval {
    pub level: Level,
    pub reasons: Vec<Reason, MAX_REASONS>,
    /// Warning reached by two Caution rules together.
    pub combined: bool,
    pub friend: bool,
    /// Hub time the position judged was heard.
    pub heard_ms: u64,
    /// Which places it counts as inside (bit 0 the bubble, bit 1 + n area
    /// n), for the edge margin next time.
    pub inside: u16,
}

/// One drone's threat level, as shown, and what it needs to move slowly.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TrackThreat {
    /// `None`: not judged (no position, or not confirmed yet).
    pub level: Option<Level>,
    /// Why, most serious first.
    pub reasons: Vec<Reason, MAX_REASONS>,
    pub combined: bool,
    /// Matched a `friend` line.
    pub friend: bool,
    /// Held after the drone was lost while heading in.
    pub held: bool,
    /// Hub time the level was reached.
    pub since_ms: u64,
    last_hit_ms: u64,
    /// When the position last judged was heard.
    last_heard_ms: u64,
    /// Places it counts as inside ([`Eval::inside`]).
    inside: u16,
    /// The latest approach reason while the drone was heard.
    inbound: Option<Reason>,
    snoozed_until_ms: u64,
    /// The level rose (to Caution or above) since the last
    /// [`crate::Tracker::take_level_rise`].
    rise: bool,
}

impl TrackThreat {
    /// The operator snoozed this drone's alerts, until then.
    pub fn snoozed(&self, now_ms: u64) -> bool {
        now_ms < self.snoozed_until_ms
    }

    /// Seconds of snooze left.
    pub fn snooze_left_s(&self, now_ms: u64) -> Option<u32> {
        self.snoozed(now_ms).then(|| self.snoozed_until_ms.saturating_sub(now_ms).div_ceil(1000) as u32)
    }

    pub(crate) fn snooze(&mut self, now_ms: u64) {
        self.snoozed_until_ms = now_ms + SNOOZE_MS;
    }

    pub(crate) fn take_rise(&mut self) -> bool {
        core::mem::take(&mut self.rise)
    }

    pub(crate) fn rise_pending(&self) -> bool {
        self.rise
    }

    /// One reason of `rule` with every field at its widest.
    #[cfg(test)]
    pub(crate) fn wide_for_tests(rule: Rule) -> Reason {
        let name = Name::try_from("abcdefghijkl").unwrap();
        let mut r = Reason::new(rule, Level::Critical, &name, 1.0e9).with_eta(1.0e9).with_speed(1.0e9).with_height(Some(-1.0e9));
        r.secs = NONE_U16 - 1;
        if matches!(rule, Rule::Several | Rule::RepeatVisitor) {
            // The alarm's name, the pilot spot: wider than the numbers.
            r.eta_ds = 1;
        }
        if matches!(rule, Rule::SpotterStriker | Rule::Spotter | Rule::IdentityChanged) {
            // The widest other drone: a track id has ten digits at most
            // (from 2^31 the ids are signals', slice 16c, written as `sig`).
            r = r.with_other(TrackId(u32::MAX >> 1));
        }
        r
    }

    /// The four widest reasons (checked in `tests_across`).
    #[cfg(test)]
    pub(crate) const WORST_RULES: [Rule; 4] = [Rule::IdentityChanged, Rule::SpotterStriker, Rule::RepeatVisitor, Rule::FastInbound];

    /// The longest JSON and CoT this can produce: every field at its widest.
    #[cfg(test)]
    pub(crate) fn worst_for_tests(now_ms: u64) -> Self {
        let mut reasons = Vec::new();
        // The four widest reasons (wider than any set a drone can have at
        // once; a held drone has only its one "last seen inbound" reason).
        for rule in Self::WORST_RULES {
            let _ = reasons.push(Self::wide_for_tests(rule));
        }
        Self {
            level: Some(Level::Critical),
            reasons,
            combined: true,
            friend: true,
            held: true,
            since_ms: now_ms,
            snoozed_until_ms: now_ms + SNOOZE_MS,
            ..Self::default()
        }
    }

    /// Two records of one drone merged: keep the more serious picture.
    pub(crate) fn absorb(&mut self, o: &TrackThreat) {
        if o.level > self.level {
            let mine = (self.snoozed_until_ms, self.rise);
            *self = o.clone();
            self.snoozed_until_ms = self.snoozed_until_ms.max(mine.0);
            self.rise |= mine.1;
        } else {
            self.snoozed_until_ms = self.snoozed_until_ms.max(o.snoozed_until_ms);
        }
    }

    /// One evaluation: `e` is what the rules said (`None`: no fresh
    /// position, or not judged), `has_position` whether the drone has ever
    /// sent one.
    pub fn update(&mut self, e: Option<Eval>, has_position: bool, now_ms: u64) {
        let Some(e) = e else {
            return self.unheard(has_position, now_ms);
        };
        self.held = false;
        self.last_heard_ms = e.heard_ms;
        self.inside = e.inside;
        self.friend = e.friend;
        let inbound = e.reasons.iter().find(|r| matches!(r.rule, Rule::Beeline | Rule::FastInbound)).copied();
        match self.level {
            Some(l) if e.level < l && now_ms.saturating_sub(self.last_hit_ms) < DECAY_MS => {
                // Coming down slowly: keep the level and the reasons that
                // raised it, and the approach among them, so a drone lost
                // now is still held as "last seen inbound". (Bench
                // 2026-11-19: the path keeps one point a second, so its
                // newest point can be up to a second older than the last
                // report; the approach rules then lapse for one judgement
                // before the position goes stale, and clearing the approach
                // here lost the hold.)
                if inbound.is_some() {
                    self.inbound = inbound;
                }
            }
            Some(l) if e.level <= l => {
                if e.level < l {
                    self.since_ms = now_ms;
                }
                self.inbound = inbound;
                self.set(e, now_ms);
            }
            _ => {
                self.since_ms = now_ms;
                self.rise |= e.level >= Level::Caution;
                self.inbound = inbound;
                self.set(e, now_ms);
            }
        }
    }

    fn set(&mut self, e: Eval, now_ms: u64) {
        self.level = Some(e.level);
        self.reasons = e.reasons;
        self.combined = e.combined;
        self.last_hit_ms = now_ms;
    }

    /// Places it counted as inside at the last judgement.
    pub fn inside(&self) -> u16 {
        self.inside
    }

    fn unheard(&mut self, has_position: bool, now_ms: u64) {
        self.inside = 0;
        let Some(level) = self.level else { return };
        if level == Level::Notice && self.reasons.is_empty() {
            return;
        }
        // Held for DECAY_MS from the last rule hit, or from when the drone
        // was last heard if that was earlier (a position up to FRESH_MS old
        // is still judged).
        if now_ms.saturating_sub(self.last_hit_ms.min(self.last_heard_ms)) >= DECAY_MS {
            *self = TrackThreat {
                level: has_position.then_some(Level::Notice),
                friend: self.friend,
                snoozed_until_ms: self.snoozed_until_ms,
                since_ms: now_ms,
                ..TrackThreat::default()
            };
            return;
        }
        if level >= Level::Warning
            && let Some(r) = &self.inbound
        {
            let mut held = *r;
            held.rule = Rule::LastSeenInbound;
            held.level = level;
            held.secs = now_ms.saturating_sub(self.last_heard_ms).div_ceil(1000).min(u64::from(NONE_U16 - 1)) as u16;
            self.held = true;
            self.reasons.clear();
            let _ = self.reasons.push(held);
        }
    }
}

// ------------------------------------------------------------ own motion --

/// The unit's own speed and course, worked out from consecutive fixes when
/// the receiver does not say (smoothed: each new estimate counts half).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct OwnMotion {
    anchor: Option<(f64, f64, u64)>,
    vel: Option<(f32, f32)>,
}

/// Fixes this far apart give a speed estimate.
const MOTION_MIN_DT_MS: u64 = 2_000;
/// Fixes further apart than this start the estimate again.
const MOTION_MAX_DT_MS: u64 = 30_000;

impl OwnMotion {
    pub const fn new() -> Self {
        Self { anchor: None, vel: None }
    }

    /// The receiver said (or the position is manual: speed 0).
    pub fn reported(&mut self, lat: f64, lon: f64, ms: u64, speed: f32, course: Option<f32>) -> (Option<f32>, Option<f32>) {
        self.anchor = Some((lat, lon, ms));
        self.vel = course.map(|c| {
            let (e, n) = crate::conflict::east_north(f64::from(speed), f64::from(c));
            (e as f32, n as f32)
        });
        speed_course(speed, course)
    }

    /// A fix with no speed from the receiver.
    pub fn derived(&mut self, lat: f64, lon: f64, ms: u64) -> (Option<f32>, Option<f32>) {
        let Some((alat, alon, ams)) = self.anchor else {
            self.anchor = Some((lat, lon, ms));
            return (None, None);
        };
        let dt = ms.saturating_sub(ams);
        if dt > MOTION_MAX_DT_MS || ms < ams {
            self.anchor = Some((lat, lon, ms));
            self.vel = None;
            return (None, None);
        }
        if dt >= MOTION_MIN_DT_MS {
            let (x, y) = Frame::at(alat, alon).xy(lat, lon);
            let s = dt as f32 / 1000.0;
            let v = (x / s, y / s);
            self.vel = Some(match self.vel {
                Some(o) => ((o.0 + v.0) / 2.0, (o.1 + v.1) / 2.0),
                None => v,
            });
            self.anchor = Some((lat, lon, ms));
        }
        match self.vel {
            Some((e, n)) => {
                let speed = sqrtf(e * e + n * n);
                speed_course(speed, Some(libm::atan2f(e, n).to_degrees()))
            }
            None => (None, None),
        }
    }
}

/// Speed, and the course only while moving ([`OWN_STATIONARY_MPS`]).
fn speed_course(speed: f32, course: Option<f32>) -> (Option<f32>, Option<f32>) {
    if speed < OWN_STATIONARY_MPS {
        return (Some(0.0), None);
    }
    (Some(speed), course.map(|c| if c < 0.0 { c + 360.0 } else { c } % 360.0))
}

impl OwnFix {
    /// Velocity east and north, m/s; zero when standing still or unknown.
    pub fn velocity(&self) -> (f32, f32) {
        match (self.speed_mps, self.course_deg) {
            (Some(s), Some(c)) if s >= OWN_STATIONARY_MPS => {
                let (e, n) = crate::conflict::east_north(f64::from(s), f64::from(c));
                (e as f32, n as f32)
            }
            _ => (0.0, 0.0),
        }
    }
}

// ------------------------------------------------------------ the rules --

/// A drone's IDs as text (for `friend` matching).
fn ids_of(t: &Track) -> impl Iterator<Item = &str> {
    t.uas_ids
        .iter()
        .filter_map(|u| u.id.as_str())
        .chain(t.dji.serial.as_ref().and_then(|s| s.as_str()))
}

/// The friend entry this track matches, if any.
pub fn friend_of<'a>(t: &Track, places: &'a Places) -> Option<&'a Friend> {
    places.friend(ids_of(t))
}

/// The average of the last [`SMOOTH_POINTS`] positions heard within
/// [`FRESH_MS`], and their mean time.
fn smoothed(ring: Option<&PathRing>, now_ms: u64) -> Option<(f64, f64, u64)> {
    let r = ring?;
    let mut n = 0u32;
    let (mut lat, mut lon, mut t) = (0.0f64, 0.0f64, 0u64);
    for (pt, p) in r.fine().rev().take(SMOOTH_POINTS) {
        if now_ms.saturating_sub(pt) > FRESH_MS {
            break;
        }
        lat += p.lat();
        lon += p.lon();
        t += pt;
        n += 1;
    }
    (n > 0).then(|| (lat / f64::from(n), lon / f64::from(n), t / u64::from(n)))
}

fn mean(pts: &[(u64, f32, f32)]) -> (f32, f32, f32) {
    let n = pts.len().max(1) as f32;
    let (mut x, mut y, mut t) = (0.0, 0.0, 0.0);
    for &(pt, px, py) in pts {
        x += px;
        y += py;
        t += pt as f32 / 1000.0;
    }
    (x / n, y / n, t / n)
}

fn angle_deg(a: (f32, f32), b: (f32, f32)) -> f32 {
    let (la, lb) = (sqrtf(a.0 * a.0 + a.1 * a.1), sqrtf(b.0 * b.0 + b.1 * b.1));
    if la <= 0.0 || lb <= 0.0 {
        return 180.0;
    }
    acosf(((a.0 * b.0 + a.1 * b.1) / (la * lb)).clamp(-1.0, 1.0)).to_degrees()
}

/// Whether the recent path is fairly straight, and the velocity it shows
/// (east, north, m/s). The path is split in two halves; each half's
/// direction is taken between averages of three points (so one bad
/// position moves it little), and the halves must agree within
/// [`MAX_TURN_DEG`] and with the reported direction within
/// [`MAX_COURSE_OFF_DEG`]. A circling drone turns too much; a hovering one
/// covers too little ground.
pub fn straight_path(ring: &PathRing, now_ms: u64, course: Option<f32>, speed: Option<f32>) -> Option<(f32, f32)> {
    let last = ring.last_ms()?;
    if now_ms.saturating_sub(last) > FRESH_MS {
        return None;
    }
    let mut pts: Vec<(u64, f32, f32), 24> = Vec::new();
    let (_, newest) = ring.fine().next_back()?;
    let frame = Frame::at(newest.lat(), newest.lon());
    for (t, p) in ring.fine().rev() {
        if last.saturating_sub(t) > STRAIGHT_WINDOW_MS || pts.is_full() {
            break;
        }
        let (x, y) = frame.xy(p.lat(), p.lon());
        let _ = pts.push((t, x, y));
    }
    pts.reverse();
    let n = pts.len();
    if n < 5 {
        return None;
    }
    let span = pts[n - 1].0 - pts[0].0;
    let need = if speed.is_some_and(|s| s > STRAIGHT_FAST_MPS) { STRAIGHT_SPAN_FAST_MS } else { STRAIGHT_SPAN_MS };
    if span < need {
        return None;
    }
    let a = mean(&pts[..3]);
    let m = mean(&pts[n / 2 - 1..n / 2 + 2]);
    let b = mean(&pts[n - 3..]);
    let d1 = (m.0 - a.0, m.1 - a.1);
    let d2 = (b.0 - m.0, b.1 - m.1);
    let len = |d: (f32, f32)| sqrtf(d.0 * d.0 + d.1 * d.1);
    if len(d1) < MIN_LEG_M || len(d2) < MIN_LEG_M || angle_deg(d1, d2) > MAX_TURN_DEG {
        return None;
    }
    let whole = (b.0 - a.0, b.1 - a.1);
    if let Some(c) = course {
        let r = c.to_radians();
        if angle_deg(whole, (libm::sinf(r), libm::cosf(r))) > MAX_COURSE_OFF_DEG {
            return None;
        }
    }
    let dt = (b.2 - a.2).max(0.5);
    Some((whole.0 / dt, whole.1 / dt))
}

/// One place being judged against: its outline in its own flat map, how
/// it moves (the bubble moves with the unit), its name and ceiling.
struct Target {
    local: Local,
    vel: (f32, f32),
    name: Name,
    ceiling_m: f32,
}

/// The height above the ellipsoid that counts as the ground for a drone's
/// heights: the unit's own (when both it and the drone have a GPS height),
/// else the drone's take-off point or the ground under it (its altitude
/// minus the height it broadcasts above them). Path heights minus this are
/// heights above the unit, as rule 4 measures.
fn ground_alt(l: &rid::message::Location, own: Option<&OwnFix>) -> Option<f32> {
    let g = l.altitude_geodetic?;
    match own.and_then(|o| o.alt_m) {
        Some(o) => Some(o as f32),
        None => l
            .height
            .filter(|_| matches!(l.height_type, rid::message::HeightType::AboveTakeoff | rid::message::HeightType::AboveGround))
            .map(|h| g - h),
    }
}

/// Judge one drone against every place. `None` when it is not judged: not
/// confirmed, no position, or the position is older than [`FRESH_MS`].
/// `trail` is the unit's own trail (for "following"); `cross` what the
/// rules across drones and days keep (slice 16d; `None`: those rules are
/// not run).
pub fn evaluate<const P: usize>(
    t: &Track,
    ring: Option<&PathRing>,
    places: &Places,
    own: Option<&OwnFix>,
    trail: Option<&OwnTrail<P>>,
    cross: Option<&mut Cross<'_, P>>,
    now_ms: u64,
) -> Option<Eval> {
    let was_inside = t.threat.inside;
    if !t.confirmed() {
        return None;
    }
    let loc = t.location.as_ref()?;
    let pos = loc.value.position?;
    if now_ms.saturating_sub(loc.rx_ms) > FRESH_MS {
        return None;
    }
    let friend = friend_of(t, places).is_some();
    // The profile's numbers (slice 16e).
    let tu = places.tuning();
    let (lat, lon, at_ms) = smoothed(ring, now_ms).unwrap_or((pos.lat, pos.lon, loc.rx_ms));
    let l = &loc.value;
    let reported = match (l.speed_horizontal, l.direction) {
        (Some(s), Some(d)) => {
            let (e, n) = crate::conflict::east_north(f64::from(s), f64::from(d));
            Some((e as f32, n as f32))
        }
        _ => None,
    };
    let path_vel = ring.and_then(|r| straight_path(r, now_ms, l.direction, l.speed_horizontal));
    // The approach rules need a straight path; the velocity is the
    // drone's own report when it sends one, else what the path shows.
    let approach_vel = path_vel.map(|p| reported.unwrap_or(p));
    let height = match (l.altitude_geodetic, own.and_then(|o| o.alt_m)) {
        (Some(d), Some(o)) => Some(d - o as f32),
        _ => l.height.filter(|_| {
            matches!(l.height_type, rid::message::HeightType::AboveTakeoff | rid::message::HeightType::AboveGround)
        }),
    };
    let pilot = t
        .system
        .as_ref()
        .filter(|s| now_ms.saturating_sub(s.rx_ms) <= PILOT_FRESH_MS)
        .and_then(|s| s.value.operator_position);
    // The lingering rules' view of the path (only when there is a place
    // to judge against).
    let ground = ground_alt(l, own);
    let linger = ring.filter(|_| places.any()).map(|r| linger::Linger::of(r, (lat, lon), height, ground, tu.low_hover_m, now_ms));
    // In the bubble, a drone hovering low beside its own pilot is taking
    // off or landing.
    let by_pilot = pilot.is_some_and(|pp| {
        let (x, y) = Frame::at(lat, lon).xy(pp.lat, pp.lon);
        sqrtf(x * x + y * y) <= linger::TAKEOFF_PILOT_M
    });
    // One hit per rule for the fixed areas and one for the bubble (the
    // most serious, then the nearest), so the list never overflows.
    let mut hits: Vec<Reason, 32> = Vec::new();
    let mut add = |r: Reason| match hits.iter_mut().find(|o| o.rule == r.rule && o.is_bubble() == r.is_bubble()) {
        Some(o) => {
            if r.level > o.level || (r.level == o.level && r.range_m < o.range_m) {
                *o = r;
            }
        }
        None => {
            let _ = hits.push(r);
        }
    };
    let bubble = places.bubble_m.zip(own).map(|(r, o)| Target {
        local: Local::circle(o.lat, o.lon, r),
        vel: o.velocity(),
        name: Name::new(),
        ceiling_m: places::DEFAULT_CEILING_M,
    });
    let areas = places.areas.iter().filter_map(|a| {
        Some(Target { local: Local::of(a.shape.as_ref()?), vel: (0.0, 0.0), name: a.name.clone(), ceiling_m: a.ceiling() })
    });
    let place_level = if friend { Level::Notice } else { Level::Caution };
    let mut inside_now = 0u16;
    // The nearest place it is at or near (slice 16d).
    let mut near: Option<across::Near> = None;
    let mut area_out = [f32::MAX; places::MAX_AREAS];
    // Bit 0 the bubble (when on), bit 1 + n area n.
    let bits = (places.bubble_m.is_some() && own.is_some()).then_some(1u16).into_iter().chain(
        places.areas.iter().enumerate().filter(|(_, a)| a.shape.is_some()).map(|(i, _)| 2u16 << i),
    );
    for (tg, bit) in bubble.into_iter().chain(areas).zip(bits) {
        let f = &tg.local.frame;
        let age_s = (now_ms.saturating_sub(at_ms) as f32 / 1000.0).min(crate::conflict::MAX_EXTRAPOLATE_S as f32);
        let mut p = f.xy(lat, lon);
        if let Some(v) = approach_vel.or(reported) {
            p = (p.0 + v.0 * age_s, p.1 + v.1 * age_s);
        }
        let from_middle = sqrtf(p.0 * p.0 + p.1 * p.1);
        let inside = tg.local.contains(p) || (was_inside & bit != 0 && tg.local.outside_by(p) <= EDGE_MARGIN_M);
        let out = if inside { 0.0 } else { tg.local.outside_by(p) };
        if bit > 1 {
            area_out[(bit.trailing_zeros() - 1) as usize] = out;
        }
        if out <= tg.local.size_m.max(across::NEAR_MIN_M) && near.as_ref().is_none_or(|n| out < n.out_m) {
            near = Some(across::Near { name: tg.name.clone(), from_middle, out_m: out });
        }
        if inside {
            inside_now |= bit;
            add(Reason::new(Rule::InArea, place_level, &tg.name, from_middle).with_height(height));
            if let Some(h) = height.filter(|h| *h > tg.ceiling_m) {
                add(Reason::new(Rule::AboveCeiling, place_level, &tg.name, from_middle).with_height(Some(h)));
            }
        }
        if let Some(pp) = pilot {
            let q = f.xy(pp.lat, pp.lon);
            let out = tg.local.outside_by(q);
            if out <= tu.pilot_near_m {
                add(Reason::new(Rule::PilotNear, place_level, &tg.name, out));
            }
            if inside && out > 0.0 {
                add(Reason::new(Rule::PilotOutside, place_level, &tg.name, out));
            }
        }
        if let Some(lg) = &linger {
            let warning = if friend { Level::Notice } else { Level::Warning };
            // 7: in one spot for minutes, in the place or just outside. The
            // bubble moves: there only the time the unit itself stood about
            // counts.
            let stay_ms = match (tg.name.is_empty(), own, trail) {
                (true, Some(o), Some(tr)) if lg.stay_ms >= linger::LOITER_CAUTION_MS => {
                    lg.stay_ms.min(follow::own_still_ms(tr, o, linger::LOITER_RADIUS_M))
                }
                _ => lg.stay_ms,
            };
            if stay_ms >= linger::LOITER_CAUTION_MS && tg.local.outside_by(p) <= linger::LOITER_NEAR_M {
                let long_low = stay_ms >= tu.loiter_warning_ms && height.is_some_and(|h| h < linger::LOITER_LOW_M);
                let level = if long_low { warning } else { place_level };
                add(Reason::new(Rule::Loitering, level, &tg.name, from_middle).with_height(height).with_secs(stay_ms));
            }
            // 8: a circle whose middle is in the place, or that reaches it.
            if let Some(o) = &lg.orbit
                && tg.local.outside_by(f.xy(o.lat, o.lon)) < o.radius_m
            {
                add(Reason::new(Rule::Circling, place_level, &tg.name, o.radius_m).with_height(height).with_secs(o.turn_ms));
            }
            // 9: low and still inside (not beside its pilot in the bubble).
            if inside && lg.low_ms >= tu.low_hover_ms && !(tg.name.is_empty() && by_pilot) {
                add(Reason::new(Rule::LowHover, warning, &tg.name, from_middle).with_height(height).with_secs(lg.low_ms));
            }
            // 10: down, a pause, up, with the bottom inside.
            if let Some(d) = &lg.dip
                && tg.local.outside_by(f.xy(d.lat, d.lon)) <= EDGE_MARGIN_M
            {
                add(Reason::new(Rule::Drop, warning, &tg.name, d.depth_m)
                    .with_height(ground.map(|g| d.bottom_alt_m - g))
                    .with_secs(d.ago_ms));
            }
        }
        // Approach: relative to the place (the bubble moves with the unit).
        let Some(v) = approach_vel else { continue };
        let rel = (v.0 - tg.vel.0, v.1 - tg.vel.1);
        let rel_speed = sqrtf(rel.0 * rel.0 + rel.1 * rel.1);
        if rel_speed < MIN_APPROACH_MPS || from_middle < 1.0 {
            continue;
        }
        let closing = -(p.0 * rel.0 + p.1 * rel.1) / from_middle;
        if closing <= 0.0 {
            continue;
        }
        let eta = if inside {
            crate::conflict::closest_approach(
                (f64::from(p.0), f64::from(p.1)),
                (f64::from(rel.0), f64::from(rel.1)),
                f64::from(LOOKAHEAD_S),
            )
            .filter(|(_, d)| (*d as f32) < tg.local.core_m)
            .map(|(t, _)| t as f32)
        } else {
            tg.local.entry_s(p, rel, LOOKAHEAD_S)
        };
        let Some(eta) = eta else { continue };
        let r = Reason::new(Rule::Beeline, Level::Warning, &tg.name, from_middle).with_eta(eta).with_speed(closing);
        if closing > tu.fast_mps() || (eta < tu.soon_s() && closing >= SOON_MIN_MPS) {
            let mut c = r;
            c.rule = Rule::FastInbound;
            c.level = Level::Critical;
            add(c);
        }
        add(r);
    }
    // 11: following the unit (the bubble only).
    if let (Some(r), Some(o), Some(tr), Some(radius)) = (ring, own, trail, places.bubble_m)
        && let Some(fl) = follow::following(r, tr, (lat, lon), o, radius.max(follow::FOLLOW_MIN_RANGE_M), tu.follow_travel_m, now_ms)
    {
        let level = if friend { Level::Notice } else { Level::Warning };
        add(Reason::new(Rule::Following, level, &Name::new(), fl.travelled_m).with_secs(fl.ms));
    }
    // 14-19: across drones and days.
    if let Some(c) = cross {
        let (key, id_key) = across::keys(t);
        let facts = across::Facts { key, id_key, friend, near: near.as_ref(), was_inside, inside_now, area_out, pilot };
        across::judge(&mut hits, t, ring, places, &facts, c, now_ms);
    }
    let mut e = judge(hits, friend, tu);
    e.heard_ms = loc.rx_ms;
    e.inside = inside_now;
    Some(e)
}

/// The level from the rules' hits: the highest, raised to Warning when two
/// different signs each gave Caution. Loitering is the same sign as being
/// in the area (it is being there, for longer); a pilot near the bubble is
/// no sign towards the pair (a pilot nearby is normal in a park).
///
/// The profile's level weights ([`Tuning::weigh`]) are applied here, to
/// every hit, before the pair is counted (slice 16e); the rules across
/// drones have already run on the unweighted levels, so a weight never
/// hides a spotter from rule 14. A repeat visitor's raise is worked out
/// again on the weighted levels.
fn judge<const H: usize>(mut hits: Vec<Reason, H>, friend: bool, tu: &Tuning) -> Eval {
    for h in hits.iter_mut() {
        h.level = tu.weigh(h.rule, h.level, h.alarm().is_some());
    }
    let others = hits.iter().filter(|h| h.rule != Rule::RepeatVisitor).map(|h| h.level).max().unwrap_or(Level::Notice);
    for h in hits.iter_mut().filter(|h| h.rule == Rule::RepeatVisitor) {
        h.level = if others >= Level::Caution { Level::Warning } else { Level::Notice };
    }
    let mut cautions: Vec<Rule, 12> = Vec::new();
    for h in hits.iter().filter(|h| h.level == Level::Caution) {
        let sign = match h.rule {
            Rule::PilotNear if h.is_bubble() => continue,
            Rule::Loitering => Rule::InArea,
            r => r,
        };
        if !cautions.contains(&sign) {
            let _ = cautions.push(sign);
        }
    }
    let top = hits.iter().map(|h| h.level).max().unwrap_or(Level::Notice);
    let combined = cautions.len() >= 2 && top < Level::Warning;
    let level = if combined { Level::Warning } else { top };
    // Most serious first; within a level the later rule (approach before
    // place), then the nearer.
    hits.sort_unstable_by(|a, b| {
        b.level.cmp(&a.level).then(b.rule.cmp(&a.rule)).then(a.range_m.cmp(&b.range_m))
    });
    let mut reasons: Vec<Reason, MAX_REASONS> = Vec::new();
    for h in hits {
        // One reason per rule (the most serious place for it).
        if reasons.iter().any(|r| r.rule == h.rule) {
            continue;
        }
        if reasons.push(h).is_err() {
            break;
        }
    }
    Eval { level, reasons, combined, friend, heard_ms: 0, inside: 0 }
}

// ------------------------------------------------------------- wording --

/// A level that rose, for the evidence log and the console.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rise {
    pub track: TrackId,
    pub level: Level,
    pub snoozed: bool,
}

/// Evidence `threat_level` data: level (1), track id (u32 LE), the rule
/// numbers of up to four reasons (4, 0 none), flags (1: bit 0 snoozed,
/// bit 1 friend, bit 2 combination, bit 3 held after the drone was lost).
pub fn rise_data(t: &Track, r: &Rise) -> [u8; 10] {
    let mut d = [0u8; 10];
    d[0] = r.level as u8;
    d[1..5].copy_from_slice(&r.track.0.to_le_bytes());
    for (i, reason) in t.threat.reasons.iter().take(4).enumerate() {
        d[5 + i] = reason.rule as u8;
    }
    let th = &t.threat;
    d[9] = u8::from(r.snoozed) | u8::from(th.friend) << 1 | u8::from(th.combined) << 2 | u8::from(th.held) << 3;
    d
}

fn place_text(w: &mut impl Write, r: &Reason) -> core::fmt::Result {
    if r.is_bubble() { w.write_str("you") } else { w.write_str(r.place()) }
}

fn area_text(w: &mut impl Write, r: &Reason) -> core::fmt::Result {
    if r.is_bubble() { w.write_str("your bubble") } else { w.write_str(r.place()) }
}

/// `at house`, `near you`.
fn at_text(w: &mut impl Write, r: &Reason) -> core::fmt::Result {
    if r.is_bubble() { w.write_str("near you") } else { write!(w, "at {}", r.place()) }
}

/// The other drone of a rule-14 or rule-19 reason: its ID when the caller
/// knows it (`name`), else `drone #12` (its track number).
fn other_text(w: &mut impl Write, r: &Reason, name: Option<&str>) -> core::fmt::Result {
    if let Some(s) = r.other_signal() {
        // A striker without a position (slice 16c).
        return write!(w, "a signal getting louder (S{})", s.0);
    }
    match (name.filter(|n| !n.is_empty()), r.other()) {
        (Some(n), _) => write!(w, "drone {n}"),
        (None, Some(id)) => write!(w, "drone #{}", id.0),
        (None, None) => w.write_str("another drone"),
    }
}

/// `3rd`, `4th`.
fn ordinal(w: &mut impl Write, n: u16) -> core::fmt::Result {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    write!(w, "{n}{suffix}")
}

/// One reason in words: `heading for house, 180 m away at 12 m/s, there in
/// about 14 s`.
pub fn write_reason(w: &mut impl Write, r: &Reason) -> core::fmt::Result {
    write_reason_named(w, r, None)
}

/// As [`write_reason`], naming the other drone of a rule-14 or rule-19
/// reason by `other` (its ID, cleaned by the caller) when given.
pub fn write_reason_named(w: &mut impl Write, r: &Reason, other: Option<&str>) -> core::fmt::Result {
    match r.rule {
        Rule::InArea if r.is_bubble() => write!(w, "within your bubble, {:.0} m from you", r.range_m()),
        Rule::InArea => write!(w, "over {}", r.place()),
        Rule::PilotNear if r.range_m == 0 => {
            w.write_str("pilot inside ")?;
            area_text(w, r)
        }
        Rule::PilotNear => {
            w.write_str("pilot at the edge of ")?;
            area_text(w, r)?;
            write!(w, " ({} m outside)", r.range_m)
        }
        Rule::PilotOutside => {
            write!(w, "pilot outside, {} m beyond the edge of ", r.range_m)?;
            area_text(w, r)
        }
        Rule::AboveCeiling => {
            write!(w, "{:.0} m up over ", r.height_m().unwrap_or(0.0))?;
            place_text(w, r)?;
            w.write_str(", above the ceiling")
        }
        Rule::Beeline | Rule::FastInbound => {
            w.write_str(if r.rule == Rule::FastInbound { "FAST, heading for " } else { "heading for " })?;
            place_text(w, r)?;
            write!(w, ", {} m away at {:.0} m/s", r.range_m, r.speed_mps().unwrap_or(0.0))?;
            if let Some(t) = r.eta_s() {
                write!(w, ", there in about {:.0} s", libm::ceilf(t))?;
            }
            Ok(())
        }
        Rule::LastSeenInbound => {
            w.write_str("last seen heading for ")?;
            place_text(w, r)?;
            write!(w, ", {} m away, {} s ago", r.range_m, r.ago_s().unwrap_or(0))
        }
        Rule::Loitering => {
            w.write_str(if r.is_bubble() { "lingering near you for " } else { "lingering at " })?;
            if !r.is_bubble() {
                write!(w, "{} for ", r.place())?;
            }
            duration_text(w, r.for_s())?;
            height_text(w, r)
        }
        Rule::Circling => {
            w.write_str("circling ")?;
            place_text(w, r)?;
            write!(w, " ({} m radius), a full turn in ", r.range_m)?;
            duration_text(w, r.for_s())?;
            height_text(w, r)
        }
        Rule::LowHover => {
            w.write_str(if r.is_bubble() { "hovering low near you" } else { "hovering low over " })?;
            if !r.is_bubble() {
                w.write_str(r.place())?;
            }
            height_text(w, r)?;
            w.write_str(", for ")?;
            duration_text(w, r.for_s())
        }
        Rule::Drop => {
            write!(w, "dropped {} m ", r.range_m)?;
            w.write_str(if r.is_bubble() { "near you" } else { "over " })?;
            if !r.is_bubble() {
                w.write_str(r.place())?;
            }
            if let Some(h) = r.height_m() {
                write!(w, " (down to {h:.0} m up)")?;
            }
            write!(w, " and climbed again, {} s ago", r.ago_s().unwrap_or(0))
        }
        Rule::Following => {
            write!(w, "following you for {} m (", r.range_m)?;
            duration_text(w, r.for_s())?;
            w.write_str(")")
        }
        Rule::SpotterStriker => {
            w.write_str("follows spotter ")?;
            other_text(w, r, other)?;
            w.write_str(" (lingered ")?;
            at_text(w, r)?;
            w.write_str(" ")?;
            duration_text(w, r.ago_s())?;
            w.write_str(" ago)")
        }
        Rule::Spotter => {
            w.write_str("spotter: ")?;
            other_text(w, r, other)?;
            w.write_str(if r.other_signal().is_some() { " is closing on " } else { " is rushing to " })?;
            place_text(w, r)
        }
        Rule::GettingLouder | Rule::DarkDrone => trend::write_trend(w, r),
        Rule::Several => match r.alarm() {
            Some(a) => {
                at_text(w, r)?;
                w.write_str(if a == across::ALARM_GNSS { " during GPS interference" } else { " while jamming is heard" })
            }
            None => {
                write!(w, "one of {} drones ", r.count().unwrap_or(0))?;
                at_text(w, r)?;
                w.write_str(" at once")
            }
        },
        Rule::Probing => {
            write!(w, "back over {} after ", r.place())?;
            duration_text(w, r.for_s())?;
            w.write_str(" away (probing)")
        }
        Rule::RepeatVisitor => {
            ordinal(w, r.count().unwrap_or(0))?;
            w.write_str(" day in 2 weeks ")?;
            at_text(w, r)?;
            w.write_str(if r.by_pilot_spot() { " (same pilot spot)" } else { " (same drone)" })
        }
        Rule::PilotMoving => {
            write!(w, "pilot on the move at {:.0} m/s while it flies ", r.speed_mps().unwrap_or(0.0))?;
            at_text(w, r)
        }
        Rule::IdentityChanged => match r.other() {
            Some(_) => {
                w.write_str("new identity mid-flight ")?;
                at_text(w, r)?;
                w.write_str(" (was ")?;
                other_text(w, r, other)?;
                write!(w, ", {} s ago)", r.ago_s().unwrap_or(0))
            }
            None => {
                w.write_str("serial changed mid-flight ")?;
                at_text(w, r)?;
                write!(w, ", {} s ago", r.ago_s().unwrap_or(0))
            }
        },
    }
}

/// `45 s`, `4 min`.
fn duration_text(w: &mut impl Write, s: Option<u16>) -> core::fmt::Result {
    match s.unwrap_or(0) {
        s if s < 120 => write!(w, "{s} s"),
        s => write!(w, "{} min", s / 60),
    }
}

/// `, 30 m up` when the height is known.
fn height_text(w: &mut impl Write, r: &Reason) -> core::fmt::Result {
    match r.height_m() {
        Some(h) => write!(w, ", {h:.0} m up"),
        None => Ok(()),
    }
}

/// The evidence and console text for a drone's level: `Warning: drone
/// 1581F5FKD229400B4WW1 heading for house, 180 m away at 12 m/s, there in
/// about 14 s; over yard`. The drone's ID arrived over the air: the hub
/// cleans it before printing or logging.
pub fn describe(t: &Track, now_ms: u64) -> String<256> {
    describe_with(t, now_ms, |_| None)
}

/// As [`describe`], naming the other drones of rules 14 and 19 by
/// `name_of` (their IDs; the hub cleans the whole text before printing).
pub fn describe_with<'a>(t: &Track, now_ms: u64, name_of: impl Fn(TrackId) -> Option<&'a str>) -> String<256> {
    let mut s: String<256> = String::new();
    let th = &t.threat;
    let _ = write!(s, "{}: drone ", th.level.map_or("No level", |l| l.label()));
    match t.primary_id().and_then(|u| u.id.as_str()).filter(|x| !x.is_empty()) {
        Some(id) => {
            let _ = s.push_str(id);
        }
        None => {
            let _ = write!(s, "#{}", t.id.0);
        }
    }
    if th.friend {
        let _ = s.push_str(" (friend)");
    }
    for (i, r) in th.reasons.iter().enumerate() {
        let _ = s.push_str(if i == 0 { " " } else { "; " });
        let _ = write_reason_named(&mut s, r, r.other().and_then(&name_of));
    }
    if th.combined {
        let _ = s.push_str("; two signs at once");
    }
    if th.snoozed(now_ms) {
        let _ = s.push_str(" (snoozed)");
    }
    s
}

/// The short form for the screen strip: `WARNING 229400B4 to house 180m
/// 14s`. The drone is named by the last eight characters of its ID (the
/// part that differs between drones; anything but printable ASCII shown as
/// `?`, since it came over the air), or `#` and its track number.
pub fn strip_text(t: &Track) -> Option<String<64>> {
    let th = &t.threat;
    let level = th.level?;
    let mut s: String<64> = String::new();
    let _ = write!(s, "{} ", level.upper());
    match t.primary_id().map(|u| u.id.as_bytes()).filter(|b| !b.is_empty()) {
        Some(id) => {
            for &b in &id[id.len().saturating_sub(8)..] {
                let _ = s.push(if (0x21..0x7f).contains(&b) { b as char } else { '?' });
            }
        }
        None => {
            let _ = write!(s, "#{}", t.id.0);
        }
    }
    if let Some(r) = th.reasons.first() {
        let _ = s.push(' ');
        let mut place: String<16> = String::new();
        let _ = place_text(&mut place, r);
        let _ = match r.rule {
            Rule::InArea => write!(s, "over {place}"),
            Rule::PilotNear => write!(s, "pilot at {place}"),
            Rule::PilotOutside => write!(s, "over {place}, pilot outside"),
            Rule::AboveCeiling => write!(s, "{:.0}m up over {place}", r.height_m().unwrap_or(0.0)),
            Rule::Beeline | Rule::FastInbound => {
                write!(s, "{}to {place} {}m", if r.rule == Rule::FastInbound { "FAST " } else { "" }, r.range_m)
                    .and_then(|_| match r.eta_s() {
                        Some(e) => write!(s, " {:.0}s", libm::ceilf(e)),
                        None => Ok(()),
                    })
            }
            Rule::LastSeenInbound => write!(s, "lost inbound {place} {}s ago", r.ago_s().unwrap_or(0)),
            Rule::Loitering => write!(s, "lingering {place} {}min", r.for_s().unwrap_or(0) / 60),
            Rule::Circling => write!(s, "circling {place} {}m", r.range_m),
            Rule::LowHover => write!(s, "low over {place} {:.0}m", r.height_m().unwrap_or(0.0)),
            Rule::Drop => write!(s, "drop at {place} {}m", r.range_m),
            Rule::Following => write!(s, "following you {}m", r.range_m),
            Rule::SpotterStriker => write!(s, "after spotter to {place}"),
            Rule::Spotter => match r.other_signal() {
                Some(sig) => write!(s, "spotter for signal S{}", sig.0),
                None => write!(s, "spotter for #{}", r.other().map_or(0, |o| o.0)),
            },
            // Signals have a strip of their own (`trend::strip_text_signal`).
            Rule::GettingLouder | Rule::DarkDrone => write!(s, "{}", r.rule.name()),
            Rule::Several => match r.alarm() {
                Some(a) => write!(s, "at {place} {}", if a == across::ALARM_GNSS { "GPS jam" } else { "jamming" }),
                None => write!(s, "{} at once {place}", r.count().unwrap_or(0)),
            },
            Rule::Probing => write!(s, "probing {place}"),
            Rule::RepeatVisitor => ordinal(&mut s, r.count().unwrap_or(0)).and_then(|_| write!(s, " day {place}")),
            Rule::PilotMoving => write!(s, "pilot moving {:.0}m/s", r.speed_mps().unwrap_or(0.0)),
            Rule::IdentityChanged => write!(s, "new identity {place}"),
        };
    }
    Some(s)
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_across;
#[cfg(test)]
mod tests_bench;
#[cfg(test)]
mod tests_linger;
#[cfg(test)]
mod tests_profiles;
#[cfg(test)]
mod tests_trend;
