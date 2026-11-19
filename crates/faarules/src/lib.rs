//! Flags flights that **may** be breaking FAA rules (PLAN.md Phase 12).
//!
//! The hub cannot see waivers, LAANC authorizations, public-safety (COA)
//! operations or whether a flight is Part 107 or recreational, so every
//! output says "possible violation", never "illegal", and names its
//! evidence and the data source and date it used.
//!
//! # What is checked (PLAN.md checks 1-7)
//! | Rule | What | Needs pack |
//! |---|---|---|
//! | [`Rule::Height`] | more than 400 ft above ground (107.51(b), 44809(a)(6)) | terrain, unless the drone sends height above ground or above take-off |
//! | [`Rule::FacilityMap`] | above the UAS Facility Map (LAANC) ceiling (107.41) | yes |
//! | [`Rule::Controlled`] | inside Class B/C/D/E surface airspace, a note: often authorized (107.41, 44809(a)(5)) | yes |
//! | [`Rule::Area`] | prohibited / restricted / national-security / stadium / TFR / national defense areas | yes |
//! | [`Rule::NoRemoteId`] | DJI DroneID heard, no Remote ID (89.105), outside FRIAs | FRIAs only |
//! | [`Rule::NoOperator`] | Remote ID without the operator position (89.305(c), 89.315(c)) | no |
//! | [`Rule::SerialChanged`] | Remote ID serial number changed mid-flight (89.305(a)) | no |
//! | [`Rule::LowAircraft`] | crewed aircraft (ADS-B) below 91.119 minimums away from airports | terrain, urban, airports |
//! | [`Rule::Vlos`] | far from the operator (107.31, 44809(a)(7)), inferred | no |
//! | [`Rule::VlosTerrain`] | terrain blocks the operator's view | terrain |
//! | [`Rule::Night`] | night or civil twilight (107.29): a note; raises distance flags | no |
//!
//! # Levels
//! 0 Note, 1 Watch, 2 Warning, 3 Alert ([`Flag::level`]). A condition has
//! an *instant* level and a *sustained* level reached after it has held for
//! that rule's duration ([`settings`]). Confidence rises with:
//! - margin beyond the reported accuracy (e.g. the height minus its 95 %
//!   accuracy, minus the terrain error, still over 450 ft);
//! - duration and repetition (the third separate excursion counts as
//!   sustained);
//! - two sources agreeing: Remote ID and DJI DroneID both heard from the
//!   same serial halve the time to the sustained level;
//! - combined factors: night raises distance flags one level; a height over
//!   600 ft (lower bound) together with a distance, night or no-Remote-ID
//!   flag is an Alert.
//!
//! Lowered by: the operator position being only the takeoff point or a
//! fixed point (distance flags at most Watch below the Alert distance), an
//! urban cell (the 107.51 structure exception, buildings blocking view),
//! and being within the position accuracy of an area's edge.
#![no_std]

mod check;
pub mod settings;
pub mod text;

use airspace::{AreaKind, Pack};
use heapless::{String, Vec};
use tracker::{TrackId, Tracker};

pub use check::{Cond, HeightSource, Inputs, aircraft_conditions, drone_conditions};
pub use settings::Settings;

/// A rule a flag is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Rule {
    Height = 1,
    FacilityMap = 2,
    Controlled = 3,
    Area = 4,
    NoRemoteId = 5,
    NoOperator = 6,
    SerialChanged = 7,
    LowAircraft = 8,
    Vlos = 9,
    VlosTerrain = 10,
    Night = 11,
}

impl Rule {
    /// Short code for CSV and the screen.
    pub fn code(self) -> &'static str {
        match self {
            Rule::Height => "HEIGHT",
            Rule::FacilityMap => "UASFM",
            Rule::Controlled => "CTRL",
            Rule::Area => "AREA",
            Rule::NoRemoteId => "NORID",
            Rule::NoOperator => "NOOP",
            Rule::SerialChanged => "SERIAL",
            Rule::LowAircraft => "LOW",
            Rule::Vlos => "VLOS",
            Rule::VlosTerrain => "VLOS-T",
            Rule::Night => "NIGHT",
        }
    }

    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            1 => Rule::Height,
            2 => Rule::FacilityMap,
            3 => Rule::Controlled,
            4 => Rule::Area,
            5 => Rule::NoRemoteId,
            6 => Rule::NoOperator,
            7 => Rule::SerialChanged,
            8 => Rule::LowAircraft,
            9 => Rule::Vlos,
            10 => Rule::VlosTerrain,
            11 => Rule::Night,
            _ => return None,
        })
    }

    /// The regulation cited.
    pub fn citation(self, area: Option<AreaKind>) -> &'static str {
        match self {
            Rule::Height => "14 CFR 107.51(b) / 49 USC 44809(a)(6)",
            Rule::FacilityMap => "14 CFR 107.41 (facility map)",
            Rule::Controlled => "14 CFR 107.41 / 49 USC 44809(a)(5)",
            Rule::Area => match area {
                Some(AreaKind::Prohibited) => "14 CFR 91.133 / 107.45",
                Some(AreaKind::Restricted) => "14 CFR 91.133",
                Some(AreaKind::NationalSecurity | AreaKind::NationalSecurityPartTime) => {
                    "14 CFR 99.7 (UAS security NOTAM)"
                }
                Some(AreaKind::Stadium) => "14 CFR 99.7 (FDC 4/3621, events only)",
                Some(AreaKind::Tfr) => "14 CFR 91.137-145 / 99.7 / 107.47",
                Some(AreaKind::NationalDefense) => "14 CFR 99.7 / 93.339",
                _ => "14 CFR 107.41",
            },
            Rule::NoRemoteId => "14 CFR 89.105",
            Rule::NoOperator => "14 CFR 89.305(c) / 89.315(c)",
            Rule::SerialChanged => "14 CFR 89.305(a)",
            Rule::LowAircraft => "14 CFR 91.119",
            Rule::Vlos | Rule::VlosTerrain => "14 CFR 107.31 / 49 USC 44809(a)(7)",
            Rule::Night => "14 CFR 107.29",
        }
    }
}

/// One flag on one track, as of the last evaluation.
#[derive(Debug, Clone, PartialEq)]
pub struct Flag {
    pub rule: Rule,
    /// For [`Rule::Area`] and [`Rule::Controlled`]: which kind of area.
    pub area: Option<AreaKind>,
    /// 0 Note, 1 Watch, 2 Warning, 3 Alert.
    pub level: u8,
    /// Evidence numbers; meaning per rule, see [`check::Cond::ev`].
    pub ev: [f32; 3],
    /// Pack source id (or `airspace::format::src::BROADCAST`/`COMPUTED`/`ADSB`).
    pub src: u8,
    /// Area name, when the flag is about an area.
    pub name: String<24>,
    /// How long the condition has held, seconds.
    pub secs: f32,
    /// A dated area evaluated with no clock (shown as "time unknown").
    pub no_clock: bool,
}

/// Per-flight summary (PLAN.md "Outputs").
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Summary {
    pub worst_level: u8,
    pub worst_rule: Option<Rule>,
    /// Highest height above ground seen, metres (NaN-free: 0 before any).
    pub max_agl_m: f32,
    pub has_agl: bool,
    /// Seconds inside each area kind (index: `AreaKind as u8`).
    pub area_s: [u16; 18],
    /// Seconds above the facility-map ceiling.
    pub above_uasfm_s: u16,
    /// Distinct flag kinds raised during the flight (bit `Rule as u8`).
    pub rules_seen: u16,
}

/// What a set of flags is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Drone(TrackId),
    /// ICAO address of an ADS-B aircraft.
    Aircraft(u32),
}

const MAX_FLAGS: usize = 8;
const MAX_RUNNING: usize = 12;
/// A full engine may drop an entry of equal standing only once it has had
/// no fresh data for this long (a gap that also restarts a condition).
pub const IDLE_EVICT_MS: u64 = 5_000;

#[derive(Debug, Clone, Copy)]
struct Running {
    rule: Rule,
    area: Option<AreaKind>,
    since_ms: u64,
    seen_ms: u64,
    onsets: u8,
    present: bool,
}

/// Everything held for one track.
#[derive(Debug, Clone)]
pub struct Entry {
    pub key: Key,
    pub flags: Vec<Flag, MAX_FLAGS>,
    pub summary: Summary,
    running: Vec<Running, MAX_RUNNING>,
    last_eval_ms: u64,
    last_loc_ms: u64,
    serial: Option<rid::message::FixedStr<20>>,
    serial_changes: u8,
    serial_changed_ms: u64,
    pub(crate) seen_ms: u64,
    /// Last update that had fresh data for it.
    fresh_ms: u64,
    /// 1 for a confirmed drone or an established aircraft, 0 otherwise
    /// (`tracker::probation`): who may replace whom when the engine is full.
    standing: u8,
}

impl Entry {
    fn new(key: Key, now: u64) -> Self {
        Self {
            key,
            flags: Vec::new(),
            summary: Summary::default(),
            running: Vec::new(),
            last_eval_ms: 0,
            last_loc_ms: 0,
            serial: None,
            serial_changes: 0,
            serial_changed_ms: 0,
            seen_ms: now,
            fresh_ms: now,
            standing: 0,
        }
    }

    /// The highest current level, if any flag.
    pub fn level(&self) -> Option<u8> {
        self.flags.iter().map(|f| f.level).max()
    }

    /// Flags ordered worst first.
    pub fn worst(&self) -> Option<&Flag> {
        self.flags
            .iter()
            .max_by_key(|f| (f.level, core::cmp::Reverse(f.rule as u8)))
    }

    /// Turn this evaluation's conditions into levelled flags.
    fn apply(&mut self, conds: &[Cond], s: &Settings, now: u64, confirmed: bool) {
        let dt_s = if self.last_eval_ms == 0 {
            0.0
        } else {
            (now.saturating_sub(self.last_eval_ms) as f32 / 1000.0).min(2.0)
        };
        self.last_eval_ms = now;
        for r in self.running.iter_mut() {
            r.present = false;
        }
        self.flags.clear();
        for c in conds.iter().filter(|c| c.show) {
            let r = match self
                .running
                .iter_mut()
                .position(|r| r.rule == c.rule && r.area == c.area)
            {
                Some(i) => {
                    let r = &mut self.running[i];
                    // A gap longer than two evaluations starts a new excursion.
                    if now.saturating_sub(r.seen_ms) > 2_500 {
                        r.since_ms = now;
                        r.onsets = r.onsets.saturating_add(1);
                    }
                    r
                }
                None => {
                    if self.running.is_full() {
                        let oldest = self
                            .running
                            .iter()
                            .enumerate()
                            .min_by_key(|(_, r)| r.seen_ms)
                            .map(|(i, _)| i)
                            .unwrap_or(0);
                        self.running.swap_remove(oldest);
                    }
                    let _ = self.running.push(Running {
                        rule: c.rule,
                        area: c.area,
                        since_ms: now,
                        seen_ms: now,
                        onsets: 1,
                        present: false,
                    });
                    self.running.last_mut().unwrap()
                }
            };
            r.seen_ms = now;
            r.present = true;
            let held_s = now.saturating_sub(r.since_ms) as f32 / 1000.0;
            if c.instant >= 1 && held_s < c.min_s {
                // Not held long enough to show yet.
                continue;
            }
            let need = if confirmed {
                c.sustain_s / 2.0
            } else {
                c.sustain_s
            };
            let mut level = if held_s >= need {
                c.sustained
            } else {
                c.instant
            };
            if r.onsets >= 3 && c.instant >= 1 {
                level = level.max(c.instant.saturating_add(1).min(c.sustained));
            }
            let _ = self.flags.push(Flag {
                rule: c.rule,
                area: c.area,
                level,
                ev: c.ev,
                src: c.src,
                name: c.name.clone(),
                secs: held_s,
                no_clock: c.no_clock,
            });
            if let Some(k) = c.area
                && c.inside_band
            {
                let s = &mut self.summary.area_s[usize::from(k as u8).min(17)];
                *s = s.saturating_add(libm::roundf(dt_s) as u16);
            }
            if c.rule == Rule::FacilityMap && level >= 1 {
                self.summary.above_uasfm_s = self
                    .summary
                    .above_uasfm_s
                    .saturating_add(libm::roundf(dt_s) as u16);
            }
        }
        combine(&mut self.flags, conds, s);
        for f in &self.flags {
            if f.level >= 1 || f.rule == Rule::Area {
                self.summary.rules_seen |= 1 << (f.rule as u8);
            }
            if f.level > self.summary.worst_level
                || self.summary.worst_rule.is_none() && f.level >= self.summary.worst_level
            {
                self.summary.worst_level = f.level;
                self.summary.worst_rule = Some(f.rule);
            }
        }
        for c in conds {
            if c.rule == Rule::Height && c.agl.is_some() {
                let agl = c.agl.unwrap_or(0.0);
                if !self.summary.has_agl || agl > self.summary.max_agl_m {
                    self.summary.max_agl_m = agl;
                    self.summary.has_agl = true;
                }
            }
        }
    }
}

/// Combined factors (PLAN.md: "night raises distance flags one level";
/// Alert = strong, combined evidence).
fn combine(flags: &mut Vec<Flag, MAX_FLAGS>, conds: &[Cond], s: &Settings) {
    let night = conds.iter().any(|c| c.rule == Rule::Night && c.ev[1] > 0.5);
    let level = |flags: &Vec<Flag, MAX_FLAGS>, r: Rule| {
        flags
            .iter()
            .filter(|f| f.rule == r)
            .map(|f| f.level)
            .max()
            .unwrap_or(0)
    };
    let vlos = level(flags, Rule::Vlos).max(level(flags, Rule::VlosTerrain));
    let norid = level(flags, Rule::NoRemoteId);
    let terrain = level(flags, Rule::VlosTerrain);
    let height_lb_ft = conds
        .iter()
        .find(|c| c.rule == Rule::Height)
        .and_then(|c| c.agl.map(|a| (a - c.ev[1]) * airspace::FT_PER_M as f32))
        .unwrap_or(0.0);
    let height_over = level(flags, Rule::Height);
    for f in flags.iter_mut() {
        match f.rule {
            Rule::Vlos if f.level >= 1 => {
                if night {
                    f.level = (f.level + 1).min(3);
                }
                if terrain >= 1 {
                    f.level = (f.level + 1).min(3);
                }
                if f.level >= 2 && height_over >= 2 {
                    f.level = 3;
                }
            }
            Rule::VlosTerrain if f.level >= 1 && night => f.level = (f.level + 1).min(3),
            Rule::Height
                if f.level >= 2
                    && height_lb_ft > s.agl_alert_ft
                    && (vlos >= 1 || night || norid >= 1) =>
            {
                f.level = 3
            }
            _ => {}
        }
    }
}

/// The rule engine: one [`Entry`] per drone or aircraft being evaluated.
pub struct Engine<const N: usize> {
    pub settings: Settings,
    entries: Vec<Entry, N>,
    /// Evaluations run, for the timing report.
    pub evals: u32,
    /// Summaries of flights that ended (track dropped by the tracker) with
    /// at least one flag, for [`Engine::take_finished`]. The oldest goes
    /// when full.
    finished: heapless::Deque<(Key, Summary), 4>,
    /// Tracks that found the engine full and could not be evaluated
    /// (counted per evaluation). Zero when `N` covers every track.
    pub refused: u32,
}

impl<const N: usize> Default for Engine<N> {
    fn default() -> Self {
        Self::new(Settings::default())
    }
}

impl<const N: usize> Engine<N> {
    pub const fn new(settings: Settings) -> Self {
        Self {
            settings,
            entries: Vec::new(),
            finished: heapless::Deque::new(),
            evals: 0,
            refused: 0,
        }
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn get(&self, key: Key) -> Option<&Entry> {
        self.entries.iter().find(|e| e.key == key)
    }

    pub fn drone(&self, id: TrackId) -> Option<&Entry> {
        self.get(Key::Drone(id))
    }

    pub fn aircraft(&self, icao: u32) -> Option<&Entry> {
        self.get(Key::Aircraft(icao))
    }

    /// The entry for `key`, made if new and there is room for it. A full
    /// engine makes room only by dropping an entry of lower standing (an
    /// unconfirmed drone or unproven aircraft for a confirmed or
    /// established one), or one of equal standing that has had no fresh
    /// data for [`IDLE_EVICT_MS`]; never a track still being evaluated at
    /// its own standing. So churn of new (forged) tracks never restarts a
    /// real drone's timers (WP-09).
    fn entry(&mut self, key: Key, standing: u8, now: u64) -> Option<&mut Entry> {
        if let Some(i) = self.entries.iter().position(|e| e.key == key) {
            let e = &mut self.entries[i];
            e.standing = standing;
            return Some(e);
        }
        if self.entries.is_full() {
            let idle = |e: &Entry| now.saturating_sub(e.fresh_ms) >= IDLE_EVICT_MS;
            let i = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| e.standing < standing || (e.standing == standing && idle(e)))
                .min_by_key(|(_, e)| (e.standing, e.fresh_ms))
                .map(|(i, _)| i);
            let Some(i) = i else {
                self.refused = self.refused.saturating_add(1);
                return None;
            };
            self.entries.swap_remove(i);
        }
        let mut e = Entry::new(key, now);
        e.standing = standing;
        self.entries.push(e).ok()?;
        self.entries.last_mut()
    }

    /// Evaluate every drone with a fresh position (or a DroneID-only
    /// presence) and every airborne aircraft. Call about once a second.
    /// Entries for tracks the tracker no longer holds are dropped first.
    ///
    /// Entries are made only for tracks with fresh data, confirmed drones
    /// and established aircraft before the rest, and a full engine never
    /// drops an entry still in use at its own standing (see `entry`): size
    /// `N` for every drone and aircraft the tracker holds (the P4 uses its
    /// drone plus aircraft capacity) and nothing is ever dropped at all.
    pub fn update<const T: usize, const A: usize>(
        &mut self,
        tracker: &Tracker<T, A>,
        pack: Option<&Pack<'_>>,
        now: u64,
        utc_ms: Option<u64>,
    ) {
        // Forget tracks the tracker dropped, keeping a flagged flight's summary.
        let finished = &mut self.finished;
        self.entries.retain(|e| {
            let keep = match e.key {
                Key::Drone(id) => tracker.get(id).is_some(),
                Key::Aircraft(icao) => tracker.aircraft().iter().any(|a| a.key.icao == icao),
            };
            if !keep && e.summary.rules_seen != 0 {
                if finished.is_full() {
                    finished.pop_front();
                }
                let _ = finished.push_back((e.key, e.summary));
            }
            keep
        });
        let s = self.settings;
        // Confirmed drones first, then unconfirmed ones.
        for pass in [true, false] {
            for t in tracker.tracks().iter().filter(|t| t.confirmed() == pass) {
                self.eval_drone(tracker, t, pack, &s, now, utc_ms);
            }
        }
        for pass in [true, false] {
            for a in tracker.aircraft().iter().filter(|a| a.established() == pass) {
                if now.saturating_sub(a.last_seen_ms) > tracker::aircraft::AIRCRAFT_STALE_MS {
                    continue;
                }
                let inputs = Inputs {
                    pack,
                    settings: &s,
                    now_ms: now,
                    utc_ms,
                };
                let conds = check::aircraft_conditions(a, &inputs);
                if conds.is_empty() && self.get(Key::Aircraft(a.key.icao)).is_none() {
                    continue;
                }
                let Some(e) = self.entry(Key::Aircraft(a.key.icao), u8::from(pass), now) else {
                    continue;
                };
                e.seen_ms = now;
                e.fresh_ms = now;
                e.apply(&conds, &s, now, false);
                self.evals += 1;
            }
        }
    }

    /// One drone's evaluation (see [`Engine::update`]).
    fn eval_drone<const T: usize, const A: usize>(
        &mut self,
        tracker: &Tracker<T, A>,
        t: &tracker::Track,
        pack: Option<&Pack<'_>>,
        s: &Settings,
        now: u64,
        utc_ms: Option<u64>,
    ) {
        let inputs = Inputs {
            pack,
            settings: s,
            now_ms: now,
            utc_ms,
        };
        let loc_ms = t.location.as_ref().map_or(0, |l| l.rx_ms);
        let fresh_loc = loc_ms + s.fresh_ms as u64 >= now && loc_ms > 0;
        let dji_recent = [t.dji.ocusync, t.dji.wifi]
            .iter()
            .any(|l| l.heard() && l.last_seen_ms + s.fresh_ms as u64 >= now);
        if !fresh_loc && !dji_recent {
            // Nothing new: no entry is made for it; an existing one keeps
            // its last flags until the track goes stale.
            if let Some(e) = self.entries.iter_mut().find(|e| e.key == Key::Drone(t.id)) {
                e.seen_ms = now;
                if now.saturating_sub(t.last_seen_ms) > tracker.config().stale_after_ms {
                    e.flags.clear();
                }
            }
            return;
        }
        let Some(e) = self.entry(Key::Drone(t.id), u8::from(t.confirmed()), now) else {
            return;
        };
        e.seen_ms = now;
        e.fresh_ms = now;
        // Serial-number continuity (89.305(a)).
        let serial = t
            .uas_ids
            .iter()
            .find(|u| u.id_type == rid::message::IdType::SerialNumber)
            .map(|u| u.id);
        if let Some(sn) = serial {
            // A new serial on the same transmitter makes a new track in
            // the tracker (IDs disagree), so compare with the track that
            // last used this transmitter address.
            if e.serial.is_none() {
                let mac = t.latest_slot().map(|(_, l)| l.mac);
                let prev = tracker
                    .tracks()
                    .iter()
                    .filter(|u| u.id != t.id && u.first_seen_ms <= t.first_seen_ms)
                    .find(|u| {
                        mac.is_some_and(|m| u.slots().any(|(_, l)| l.mac == m))
                            && t.first_seen_ms.saturating_sub(u.last_seen_ms) < 30_000
                            && u.uas_ids.iter().any(|x| {
                                x.id_type == rid::message::IdType::SerialNumber && x.id != sn
                            })
                    });
                if prev.is_some() {
                    e.serial_changes = 1;
                    e.serial_changed_ms = now;
                }
            }
            if e.serial.is_some_and(|old| old != sn) {
                e.serial_changes = e.serial_changes.saturating_add(1);
                e.serial_changed_ms = now;
            }
            e.serial = Some(sn);
        }
        if fresh_loc && loc_ms == e.last_loc_ms && !dji_recent {
            return;
        }
        e.last_loc_ms = loc_ms;
        let changed = (e.serial_changes > 0
            && now.saturating_sub(e.serial_changed_ms) < (s.id_change_hold_s * 1000.0) as u64)
            .then_some(e.serial_changes);
        let (conds, confirmed) = check::drone_conditions(t, &inputs, changed);
        e.apply(&conds, s, now, confirmed);
        self.evals += 1;
    }

    /// The next finished flight's summary (worst flag, highest height above
    /// ground, seconds inside each kind of area), oldest first.
    pub fn take_finished(&mut self) -> Option<(Key, Summary)> {
        self.finished.pop_front()
    }
}

#[cfg(test)]
mod tests;
