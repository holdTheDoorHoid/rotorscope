//! Profiles (slice 16e): presets of the thresholds and level weights, so
//! nobody tunes nineteen numbers by hand (docs/THREAT-PROFILES.md,
//! "Profiles").
//!
//! A `profile home|facility|event|patrol|default` line in the site file
//! picks one row of [`TUNINGS`]; every rule reads its numbers from that row
//! ([`Tuning`]), and the level weights are applied in one place
//! ([`Tuning::weigh`], called from `behaviour::judge`). There are no
//! `if profile == ..` checks in the rules.
//!
//! | | default | home | facility | event | patrol |
//! |---|---|---|---|---|---|
//! | fast inbound (6): closing faster than | 25 m/s | 25 | 25 | 25 | **18** |
//! | fast inbound (6): arriving sooner than | 30 s | 30 | 30 | 30 | **45** |
//! | pilot "at the fence" (2): within | 30 m | 30 | **100** | 30 | 30 |
//! | loitering (7): Warning when low after | 5 min | **3 min** | 5 | 5 | 5 |
//! | low hover (9): under | 15 m | **20 m** | 15 | 15 | 15 |
//! | low hover (9): for | 10 s | **6 s** | 10 | 10 | 10 |
//! | following (11): the unit travelled | 200 m | 200 | 200 | 200 | **120 m** |
//! | repeat visitor (17): days in two weeks | 3 | **2** | 3 | 3 | 3 |
//! | several at once (15): the count of drones | on | on | on | **off** | on |
//! | level weights | | | circling **+1** | pilot near, probing **-1** | |
//!
//! **Safety floors no profile can loosen** (checked by [`Tuning::floor`],
//! [`Tuning::fast_mps`] and [`Tuning::soon_s`], whatever the table says):
//! beeline and fast inbound (5, 6; a profile may only make fast inbound
//! *more* sensitive), spotter then striker (14) and the spotter's mark,
//! identity changes (19), the "last seen inbound" hold, and the jamming and
//! GPS-interference half of several at once (15). A friend is never lowered
//! on those either (that is not a profile matter: `behaviour::evaluate`).
//!
//! **Precedence.** A `crowd` line is an explicit setting and wins over the
//! profile whatever the order of the lines (the profile is a preset of many
//! numbers; a line naming one setting is a deliberate choice about it, and
//! switching profiles must not silently undo it). `crowd auto` hands the
//! count back to the profile. Nothing else in the site file overlaps a
//! profile: areas, the bubble, friends and `visits` are never set by one.

use super::{FAST_MPS, Level, PILOT_NEAR_M, Rule, SOON_S, follow, linger, visits};

/// A profile (site line `profile NAME`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
#[repr(u8)]
pub enum Profile {
    /// Today's numbers.
    #[default]
    Default = 0,
    /// A fixed circle round a house: stalking and casing (repeat visitor
    /// and low hover weighted up).
    Home = 1,
    /// A fixed polygon round infrastructure or a prison: the pilot outside
    /// the fence and circling weighted up.
    Facility = 2,
    /// Many friendly drones expected: several at once (the count) off, the
    /// pilot nearby and probing weighted down; the approach rules (and
    /// circling) as ever.
    Event = 3,
    /// A handheld unit's bubble: following and fast inbound weighted up.
    Patrol = 4,
}

/// Every profile, in table order.
pub const PROFILES: [Profile; 5] = [Profile::Default, Profile::Home, Profile::Facility, Profile::Event, Profile::Patrol];

impl Profile {
    /// `"default"`, `"home"`, ... (site line, JSON).
    pub const fn name(self) -> &'static str {
        match self {
            Profile::Default => "default",
            Profile::Home => "home",
            Profile::Facility => "facility",
            Profile::Event => "event",
            Profile::Patrol => "patrol",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        PROFILES.into_iter().find(|p| p.name().eq_ignore_ascii_case(s))
    }

    /// Its row of [`TUNINGS`].
    pub const fn tuning(self) -> &'static Tuning {
        &TUNINGS[self as usize]
    }

    /// In a few words, for the evidence log and the console.
    pub const fn summary(self) -> &'static str {
        match self {
            Profile::Default => "the standard numbers",
            Profile::Home => "repeat visitor from 2 days, low hover from 20 m and 6 s, long low loitering a Warning after 3 min",
            Profile::Facility => "pilot at the fence within 100 m, circling a Warning",
            Profile::Event => {
                "several at once by count off (jamming still counts), pilot nearby and probing one level lower"
            }
            Profile::Patrol => "fast inbound from 18 m/s or under 45 s, following from 120 m",
        }
    }
}

/// The numbers one profile sets (one row of [`TUNINGS`]). Read through the
/// methods, which hold the safety floors.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tuning {
    pub profile: Profile,
    fast_mps: f32,
    soon_s: f32,
    pub pilot_near_m: f32,
    pub loiter_warning_ms: u64,
    pub low_hover_m: f32,
    pub low_hover_ms: u64,
    pub follow_travel_m: f32,
    pub repeat_days: u32,
    /// Several at once by count (rule 15's crowd) is on, unless a `crowd`
    /// line says otherwise.
    pub crowd: bool,
    /// Level weights: per rule number, -1 one level lower, +1 one higher.
    shift: [i8; 22],
}

const NO_SHIFT: [i8; 22] = [0; 22];

const fn shifted(pairs: &[(Rule, i8)]) -> [i8; 22] {
    let mut s = NO_SHIFT;
    let mut i = 0;
    while i < pairs.len() {
        s[pairs[i].0 as usize] = pairs[i].1;
        i += 1;
    }
    s
}

const DEFAULT: Tuning = Tuning {
    profile: Profile::Default,
    fast_mps: FAST_MPS,
    soon_s: SOON_S,
    pilot_near_m: PILOT_NEAR_M,
    loiter_warning_ms: linger::LOITER_WARNING_MS,
    low_hover_m: linger::LOW_HOVER_M,
    low_hover_ms: linger::LOW_HOVER_MS,
    follow_travel_m: follow::FOLLOW_TRAVEL_M,
    repeat_days: visits::REPEAT_DAYS,
    crowd: true,
    shift: NO_SHIFT,
};

/// The profiles' numbers, in [`PROFILES`] order (the table in the module
/// notes and in docs/THREAT-PROFILES.md, "Slice 16e as built").
pub const TUNINGS: [Tuning; 5] = [
    DEFAULT,
    Tuning {
        profile: Profile::Home,
        // A drone back over the house on a second day in two weeks.
        repeat_days: 2,
        // A first-floor window from the street is under 20 m; a peek is
        // quick.
        low_hover_m: 20.0,
        low_hover_ms: 6_000,
        loiter_warning_ms: 180_000,
        ..DEFAULT
    },
    Tuning {
        profile: Profile::Facility,
        // Prison drops are flown from just beyond the perimeter.
        pilot_near_m: 100.0,
        shift: shifted(&[(Rule::Circling, 1)]),
        ..DEFAULT
    },
    Tuning {
        profile: Profile::Event,
        crowd: false,
        // Pilots stand inside the event; show and camera drones come and
        // go. (Not circling: lowered, recon circling over an event fell to
        // Caution in the false-alarm matrix, tests_profiles.)
        shift: shifted(&[(Rule::PilotNear, -1), (Rule::Probing, -1)]),
        ..DEFAULT
    },
    Tuning {
        profile: Profile::Patrol,
        // A camera drone at full speed (16-21 m/s) at a person on foot.
        fast_mps: 18.0,
        soon_s: 45.0,
        follow_travel_m: 120.0,
        ..DEFAULT
    },
];

impl Default for Tuning {
    fn default() -> Self {
        DEFAULT
    }
}

impl Tuning {
    /// Fast inbound: closing faster than this. Never above [`FAST_MPS`].
    pub fn fast_mps(&self) -> f32 {
        self.fast_mps.min(FAST_MPS)
    }

    /// Fast inbound: arriving sooner than this (when closing fast enough).
    /// Never below [`SOON_S`].
    pub fn soon_s(&self) -> f32 {
        self.soon_s.max(SOON_S)
    }

    /// A rule no profile may lower: the approach rules, the striker and its
    /// spotter, identity changes, the lost-track hold, and several at once
    /// during jamming or GPS interference (`alarm`).
    pub const fn floor(rule: Rule, alarm: bool) -> bool {
        matches!(
            rule,
            Rule::Beeline | Rule::FastInbound | Rule::SpotterStriker | Rule::Spotter | Rule::IdentityChanged | Rule::LastSeenInbound
        ) || (alarm && matches!(rule, Rule::Several))
    }

    /// The weight this profile gives `rule` (-1, 0, +1; 0 for a floor).
    pub fn shift(&self, rule: Rule) -> i8 {
        self.shift[rule as usize]
    }

    /// The level weights, applied in one place: `level` is what the rule
    /// gave. A Notice (a friend's lowered place rule, a repeat visitor on its
    /// own) is never raised; a raise stops at Warning (Critical is for
    /// something that may arrive within seconds); a lower stops at Notice;
    /// a floor ([`Self::floor`]) is never lowered.
    pub fn weigh(&self, rule: Rule, level: Level, alarm: bool) -> Level {
        let s = self.shift(rule);
        if s == 0 || level == Level::Notice || level == Level::Critical {
            return level;
        }
        if s < 0 && Self::floor(rule, alarm) {
            return level;
        }
        match (s > 0, level) {
            (true, Level::Caution) => Level::Warning,
            (false, Level::Warning) => Level::Caution,
            (false, Level::Caution) => Level::Notice,
            (_, l) => l,
        }
    }

    /// Whether every safety floor holds for this row: nothing in it makes
    /// rules 5, 6, 14, 19 or the jamming half of 15 less sensitive than the
    /// default (checked for every row by a test, and by construction in the
    /// accessors).
    pub fn floors_hold(&self) -> bool {
        let floors = [Rule::Beeline, Rule::FastInbound, Rule::SpotterStriker, Rule::Spotter, Rule::IdentityChanged, Rule::LastSeenInbound];
        self.fast_mps <= FAST_MPS && self.soon_s >= SOON_S && floors.iter().all(|&r| self.shift(r) >= 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_and_rows_are_in_order() {
        for (i, p) in PROFILES.into_iter().enumerate() {
            assert_eq!(Profile::from_name(p.name()), Some(p));
            assert_eq!(Profile::from_name(&p.name().to_ascii_uppercase()), Some(p));
            assert_eq!(TUNINGS[i].profile, p);
            assert_eq!(p as usize, i);
        }
        assert_eq!(Profile::from_name("party"), None);
        assert_eq!(*Profile::Default.tuning(), Tuning::default());
    }

    #[test]
    fn no_profile_loosens_a_safety_floor() {
        for t in &TUNINGS {
            assert!(t.floors_hold(), "{:?}", t.profile);
            assert!(t.fast_mps() <= FAST_MPS && t.soon_s() >= SOON_S);
            for r in [Rule::Beeline, Rule::FastInbound, Rule::SpotterStriker, Rule::Spotter, Rule::IdentityChanged] {
                for l in [Level::Caution, Level::Warning, Level::Critical] {
                    assert!(t.weigh(r, l, false) >= l, "{:?} {r:?}", t.profile);
                }
            }
            // The jamming half of rule 15.
            assert!(t.weigh(Rule::Several, Level::Warning, true) == Level::Warning, "{:?}", t.profile);
        }
        // Even a row that tried would be held by the accessors and weigh().
        let bad = Tuning { fast_mps: 40.0, soon_s: 10.0, shift: shifted(&[(Rule::FastInbound, -1), (Rule::Beeline, -1)]), ..DEFAULT };
        assert!(!bad.floors_hold());
        assert_eq!((bad.fast_mps(), bad.soon_s()), (FAST_MPS, SOON_S));
        assert_eq!(bad.weigh(Rule::Beeline, Level::Warning, false), Level::Warning);
        assert_eq!(bad.weigh(Rule::FastInbound, Level::Critical, false), Level::Critical);
    }

    #[test]
    fn weights_move_one_level_within_notice_and_warning() {
        let f = Profile::Facility.tuning();
        assert_eq!(f.weigh(Rule::Circling, Level::Caution, false), Level::Warning);
        assert_eq!(f.weigh(Rule::Circling, Level::Warning, false), Level::Warning);
        // A friend's circling stays at Notice.
        assert_eq!(f.weigh(Rule::Circling, Level::Notice, false), Level::Notice);
        let e = Profile::Event.tuning();
        assert_eq!(e.weigh(Rule::PilotNear, Level::Caution, false), Level::Notice);
        assert_eq!(e.weigh(Rule::Probing, Level::Caution, false), Level::Notice);
        assert_eq!(e.weigh(Rule::InArea, Level::Caution, false), Level::Caution);
        assert_eq!(e.weigh(Rule::Beeline, Level::Warning, false), Level::Warning);
        assert!(!e.crowd);
    }
}
