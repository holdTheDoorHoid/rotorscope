//! GNSS integrity monitor: is the hub's own receiver being jammed or spoofed?
//!
//! Fed with what the parser already produces, once per navigation epoch (1 Hz): the [`Fix`],
//! the satellites of that epoch ([`SatTable`], from UBX-NAV-SAT or NMEA GSV), and whatever
//! receiver-specific indicators arrived (u-blox UBX-MON-RF, UBX-NAV-STATUS, UBX-NAV-CLOCK;
//! Quectel `$PAIRSPF`). It answers with a [`Status`]: one of five [`State`]s, the check that
//! raised it ([`Cause`]) with its number, and the figures behind it ([`Metrics`]).
//!
//! Every threshold is a documented constant below; `docs/GNSS-INTEGRITY.md` explains each
//! check, its false alarms, and its sources.
//!
//! ## How checks combine
//!
//! Each check produces, per epoch, nothing, *suspect* or *strong*. A check must hold for its
//! own number of consecutive epochs before it counts (so one bad second never alarms), and
//! the one-shot checks (a position jump, a time jump, a clock step) stay counted for
//! [`LATCH_EPOCHS`] after they fire. Some checks are *corroborating only*: alone they never
//! raise an alarm, they only add weight to another. Per category (jamming, spoofing):
//!
//! * the level is the highest level among the counted primary checks;
//! * two or more counted checks at once (any kind) raise it one step: suspect becomes
//!   strong, and two corroborating checks together become suspect.
//!
//! Spoofing outranks jamming in the single [`State`]. Going up is immediate; coming down
//! needs [`CLEAR_EPOCHS`] epochs in a row at the lower level.
//!
//! ## What is learned
//!
//! Only while the state is [`State::Ok`] and no spoofing check fired that epoch:
//! * **home**: the hub is normally static. After [`HOME_LEARN_EPOCHS`] usable fixes that all
//!   stay within [`HOME_LEARN_RADIUS_M`] (or 3x the receiver's accuracy) of their mean, that
//!   mean becomes home, and the position and altitude checks start. [`Monitor::forget_home`]
//!   starts over (call it when the hub is moved on purpose).
//! * the C/N0 and AGC baselines (slow averages), for the jamming checks;
//! * the offset between GPS UTC and the hub's own millisecond clock, for the time check.
//!
//! `#![no_std]`, no alloc, no floating-point library (the square root and cosine are local).

use core::fmt::{self, Write};

use crate::fix::{Fix, Source};
use crate::parser::Event;
use crate::quectel::Reply;
use crate::sats::{Cn0Stats, SatTable};
use crate::sky::Band;
use crate::ubx::{MonRf, NavClock, NavStatus};

// ------------------------------------------------------------- thresholds --

/// Receiver indicators (MON-RF, NAV-STATUS, `$PAIRSPF`, NAV-CLOCK) older than this are ignored.
pub const AUX_FRESH_MS: u64 = 3_000;
/// One-shot checks stay counted this many epochs after they fire.
pub const LATCH_EPOCHS: u16 = 60;
/// Epochs in a row at a lower level before the state steps down.
pub const CLEAR_EPOCHS: u16 = 30;

/// Receiver jamming flag (MON-RF `jammingState`, `$PAIRSPF`): 2 = suspect, 3 = strong, for
/// this many epochs.
pub const SUSTAIN_RX_FLAG: u16 = 3;
/// u-blox CW jamming indicator (MON-RF `cwSuppression`, 0..255) at or above this: suspect
/// (corroborating only).
pub const CW_SUSPECT: u8 = 100;
/// ... for this many epochs.
pub const SUSTAIN_CW: u16 = 5;
/// AGC count (MON-RF `agcCnt`) this fraction below its learned baseline: suspect. Strong
/// in-band power makes the receiver turn its gain down.
pub const AGC_DROP_FRAC: f32 = 0.30;
/// ... for this many epochs.
pub const SUSTAIN_AGC: u16 = 5;
/// Mean C/N0 of the four strongest L1 satellites this many dB below its learned baseline,
/// with the receiver still listing satellites: suspect jamming. Never strong on its own: a
/// hand over the antenna does the same, only the receiver's own flags or AGC can tell.
pub const CN0_DROP_DB: f32 = 10.0;
/// ... for this many epochs.
pub const SUSTAIN_CN0_DROP: u16 = 30;
/// A C/N0 collapse that lasts this long with nothing else pointing at jamming is accepted as
/// the new normal (the hub was moved indoors) and the baseline is learned again.
pub const CN0_REBASE_EPOCHS: u16 = 600;
/// Baselines are averages over about this many epochs.
pub const BASELINE_EPOCHS: f32 = 120.0;
/// ... and need this many samples before they are used.
pub const BASELINE_MIN: u16 = 60;

/// u-blox spoofing detector (NAV-STATUS `spoofDetState`): 2 = suspect, 3 = strong, for this
/// many epochs.
pub const SUSTAIN_RX_SPOOF: u16 = 2;
/// Uniform C/N0: at least this many L1 satellites tracked ...
pub const UNIFORM_MIN_SATS: u8 = 6;
/// ... with a standard deviation under this (dB) ...
pub const UNIFORM_MAX_STD_DB: f32 = 1.5;
/// ... and strongest minus weakest at most this (dB) ...
pub const UNIFORM_MAX_RANGE_DB: u8 = 4;
/// ... and a mean of at least this (dB-Hz): suspect. A weak indoor sky can be flat too, so
/// flat-and-weak is not counted.
pub const UNIFORM_MIN_MEAN: f32 = 38.0;
/// ... a mean of at least this: strong (one nearby transmitter).
pub const UNIFORM_STRONG_MEAN: f32 = 45.0;
/// ... for this many epochs.
pub const SUSTAIN_UNIFORM: u16 = 5;
/// An L1 signal at or above this (dB-Hz) is stronger than a satellite gives at the ground.
pub const CN0_TOO_STRONG: u8 = 55;
/// This many such satellites: suspect.
pub const TOO_STRONG_SATS: u8 = 3;
/// ... for this many epochs.
pub const SUSTAIN_TOO_STRONG: u16 = 5;
/// Horizontal distance from home above `max(this, 4 x accuracy)`: suspect.
pub const HOME_SUSPECT_M: f32 = 75.0;
/// Above `max(this, 6 x accuracy)`: strong.
pub const HOME_STRONG_M: f32 = 300.0;
/// ... for this many epochs.
pub const SUSTAIN_HOME: u16 = 10;
/// Height off home above `max(this, 4 x vertical accuracy)`: suspect.
pub const ALT_SUSPECT_M: f32 = 60.0;
/// Above `max(this, 6 x vertical accuracy)`: strong.
pub const ALT_STRONG_M: f32 = 250.0;
/// ... for this many epochs.
pub const SUSTAIN_ALT: u16 = 10;
/// Two usable fixes further apart than this ...
pub const JUMP_M: f32 = 300.0;
/// ... and faster than this (m/s): a jump (one-shot, suspect).
pub const JUMP_MPS: f32 = 150.0;
/// GPS UTC off the hub's own clock (see [`crate::timeguard`]: 150 ms on the message, 5 ms on
/// the PPS edge, plus the crystal's drift budget) for this many epochs in a row: a time jump
/// (suspect). Two, so one late epoch (a main-loop stall) is not one.
pub const SUSTAIN_TIME: u16 = 2;
/// Epochs in a row with no spoofing check firing at all before GPS position and time are
/// trusted (published, and anchor the hub's clock), at boot and after any doubt.
pub const TRUST_EPOCHS: u16 = 10;
/// u-blox NAV-CLOCK: drift changing by more than this (ns/s) between two 1 s epochs with a
/// fix (corroborating only). A TCXO drifts by a few ns/s per second with temperature.
pub const CLK_DRIFT_STEP_NS_S: i32 = 100;
/// u-blox NAV-CLOCK: bias off its prediction (previous bias + drift x dt), after removing
/// whole-millisecond clock jumps, by more than this (ns) (corroborating only).
pub const CLK_BIAS_STEP_NS: i64 = 1_000;
/// Satellites with an impossible number, elevation or azimuth (corroborating only) ...
pub const SUSTAIN_IMPOSSIBLE: u16 = 3;

/// Home: usable fixes needed, all close together (about 15 min at 1 Hz).
pub const HOME_LEARN_EPOCHS: u32 = 900;
/// Home: fixes must stay within `max(this, 3 x accuracy)` of the running mean.
pub const HOME_LEARN_RADIUS_M: f32 = 40.0;
/// Home: fixes with a worse accuracy estimate than this are not used.
pub const HOME_MAX_ACC_M: f32 = 50.0;
/// NMEA gives no accuracy estimate: use HDOP times this (m, a typical user range error).
pub const UERE_M: f32 = 6.0;

// ------------------------------------------------------------------ types --

/// The overall verdict, worst first in severity order: spoofing outranks jamming.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum State {
    /// Nothing wrong seen.
    Ok,
    /// Some sign of jamming.
    SuspectJamming,
    /// Jamming: the receiver says so, or several signs agree.
    Jamming,
    /// Some sign of spoofing. The hub holds its last trusted position and time.
    SuspectSpoofing,
    /// Spoofing: the receiver says so, a strong sign, or several signs agree.
    Spoofing,
}

impl State {
    /// Short name for logs and JSON.
    pub fn name(self) -> &'static str {
        match self {
            State::Ok => "ok",
            State::SuspectJamming => "suspect_jamming",
            State::Jamming => "jamming",
            State::SuspectSpoofing => "suspect_spoofing",
            State::Spoofing => "spoofing",
        }
    }

    /// Plain English.
    pub fn text(self) -> &'static str {
        match self {
            State::Ok => "GPS looks normal",
            State::SuspectJamming => "possible GPS jamming",
            State::Jamming => "GPS jamming",
            State::SuspectSpoofing => "possible GPS spoofing",
            State::Spoofing => "GPS spoofing",
        }
    }

    /// Suspect spoofing or spoofing: do not trust GPS position or time.
    pub fn is_spoofing(self) -> bool {
        matches!(self, State::SuspectSpoofing | State::Spoofing)
    }

    /// Suspect jamming or jamming.
    pub fn is_jamming(self) -> bool {
        matches!(self, State::SuspectJamming | State::Jamming)
    }
}

/// How strongly a check (or a category) points at trouble.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Level {
    /// Nothing.
    #[default]
    None,
    /// A sign.
    Suspect,
    /// A strong sign.
    Strong,
}

/// Which check. [`Cause::kind`] is the `kind` byte of the evidence event, numbered
/// separately for jamming (`GnssJamming`, code 18) and spoofing (`GnssSpoofing`, code 19).
/// [`Cause::unit`] says what the event's `level` number means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Cause {
    /// Jamming 1: the receiver's own jamming flag (u-blox MON-RF `jammingState`, Quectel
    /// `$PAIRSPF`). Level: the state, 2 warning or 3 critical.
    RxJamFlag,
    /// Jamming 2: u-blox CW jamming indicator. Level: 0..255.
    CwJam,
    /// Jamming 3: the RF gain (AGC) turned down. Level: percent below baseline.
    AgcDrop,
    /// Jamming 4: C/N0 fell on all satellites at once. Level: drop in 0.1 dB.
    Cn0Collapse,
    /// Spoofing 1: the u-blox spoofing detector (NAV-STATUS). Level: `spoofDetState` 2 or 3.
    RxSpoofFlag,
    /// Spoofing 2: all satellites at the same, high C/N0. Level: standard deviation in 0.1 dB.
    UniformCn0,
    /// Spoofing 3: signals stronger than satellites can give. Level: strongest, dB-Hz.
    StrongCn0,
    /// Spoofing 4: position away from the learned home. Level: metres.
    OffHome,
    /// Spoofing 5: position jumped between two fixes. Level: metres.
    PositionJump,
    /// Spoofing 6: altitude away from home's. Level: metres (signed: + above).
    AltitudeOffHome,
    /// Spoofing 7: GPS UTC jumped against the hub's clock. Level: milliseconds (signed).
    TimeJump,
    /// Spoofing 8: receiver clock bias stepped (u-blox NAV-CLOCK). Level: nanoseconds.
    ClockBiasStep,
    /// Spoofing 9: receiver clock drift stepped (u-blox NAV-CLOCK). Level: ns/s.
    ClockDriftStep,
    /// Spoofing 10: satellites with impossible numbers or positions. Level: count.
    ImpossibleSat,
    /// Spoofing 11: GPS time before this firmware's build date, or before the latest time the
    /// hub remembers trusting. Level: seconds before it (negative).
    TimeFloor,
}

const N_CAUSES: usize = 15;

impl Cause {
    const ALL: [Cause; N_CAUSES] = [
        Cause::RxJamFlag,
        Cause::CwJam,
        Cause::AgcDrop,
        Cause::Cn0Collapse,
        Cause::RxSpoofFlag,
        Cause::UniformCn0,
        Cause::StrongCn0,
        Cause::OffHome,
        Cause::PositionJump,
        Cause::AltitudeOffHome,
        Cause::TimeJump,
        Cause::ClockBiasStep,
        Cause::ClockDriftStep,
        Cause::ImpossibleSat,
        Cause::TimeFloor,
    ];

    /// A spoofing check (else jamming).
    pub fn is_spoofing(self) -> bool {
        self as u8 >= Cause::RxSpoofFlag as u8
    }

    /// The evidence event `kind` byte: 1.. within its category.
    pub fn kind(self) -> u8 {
        if self.is_spoofing() { self as u8 - Cause::RxSpoofFlag as u8 + 1 } else { self as u8 + 1 }
    }

    /// Short name for logs and JSON.
    pub fn name(self) -> &'static str {
        match self {
            Cause::RxJamFlag => "rx_jam_flag",
            Cause::CwJam => "cw_jam",
            Cause::AgcDrop => "agc_drop",
            Cause::Cn0Collapse => "cn0_collapse",
            Cause::RxSpoofFlag => "rx_spoof_flag",
            Cause::UniformCn0 => "uniform_cn0",
            Cause::StrongCn0 => "strong_cn0",
            Cause::OffHome => "off_home",
            Cause::PositionJump => "position_jump",
            Cause::AltitudeOffHome => "altitude_off_home",
            Cause::TimeJump => "time_jump",
            Cause::ClockBiasStep => "clock_bias_step",
            Cause::ClockDriftStep => "clock_drift_step",
            Cause::ImpossibleSat => "impossible_sat",
            Cause::TimeFloor => "time_floor",
        }
    }

    /// Unit of the number that goes with it.
    pub fn unit(self) -> &'static str {
        match self {
            Cause::RxJamFlag | Cause::RxSpoofFlag => "state",
            Cause::CwJam => "0-255",
            Cause::AgcDrop => "%",
            Cause::Cn0Collapse | Cause::UniformCn0 => "0.1 dB",
            Cause::StrongCn0 => "dB-Hz",
            Cause::OffHome | Cause::PositionJump | Cause::AltitudeOffHome => "m",
            Cause::TimeJump => "ms",
            Cause::ClockBiasStep => "ns",
            Cause::ClockDriftStep => "ns/s",
            Cause::ImpossibleSat => "satellites",
            Cause::TimeFloor => "s",
        }
    }

    /// Alone it never raises an alarm; it only adds weight.
    fn corroborating(self) -> bool {
        matches!(self, Cause::CwJam | Cause::ClockBiasStep | Cause::ClockDriftStep | Cause::ImpossibleSat)
    }

    /// Fires once and stays counted for [`LATCH_EPOCHS`].
    fn one_shot(self) -> bool {
        matches!(self, Cause::PositionJump | Cause::ClockBiasStep | Cause::ClockDriftStep)
    }

    fn sustain(self) -> u16 {
        match self {
            Cause::RxJamFlag => SUSTAIN_RX_FLAG,
            Cause::CwJam => SUSTAIN_CW,
            Cause::AgcDrop => SUSTAIN_AGC,
            Cause::Cn0Collapse => SUSTAIN_CN0_DROP,
            Cause::RxSpoofFlag => SUSTAIN_RX_SPOOF,
            Cause::UniformCn0 => SUSTAIN_UNIFORM,
            Cause::StrongCn0 => SUSTAIN_TOO_STRONG,
            Cause::OffHome => SUSTAIN_HOME,
            Cause::AltitudeOffHome => SUSTAIN_ALT,
            Cause::ImpossibleSat => SUSTAIN_IMPOSSIBLE,
            Cause::TimeJump => SUSTAIN_TIME,
            Cause::PositionJump | Cause::ClockBiasStep | Cause::ClockDriftStep | Cause::TimeFloor => 1,
        }
    }
}

/// One check that is currently counted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Finding {
    /// Which check.
    pub cause: Cause,
    /// How strong.
    pub level: Level,
    /// Its number, in [`Cause::unit`].
    pub value: i32,
}

/// The figures behind the verdict, for a status screen. `None`/0 when unknown.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Metrics {
    /// L1 satellites listed / tracked in the last epoch.
    pub listed: u8,
    /// Tracked (with a C/N0).
    pub tracked: u8,
    /// Mean C/N0 of the tracked L1 satellites, dB-Hz.
    pub cn0_mean: f32,
    /// Its standard deviation, dB.
    pub cn0_std: f32,
    /// Mean of the four strongest, dB-Hz.
    pub cn0_top4: f32,
    /// Learned baseline of `cn0_top4`, dB-Hz.
    pub cn0_baseline: Option<f32>,
    /// u-blox AGC count and its learned baseline.
    pub agc: Option<u16>,
    /// Baseline of `agc`.
    pub agc_baseline: Option<f32>,
    /// u-blox CW jamming indicator.
    pub cw: Option<u8>,
    /// Receiver jamming state (MON-RF or `$PAIRSPF`, worst band), 0..3.
    pub rx_jam: Option<u8>,
    /// u-blox spoofing detector state, 0..3.
    pub rx_spoof: Option<u8>,
    /// Home learned.
    pub home: bool,
    /// Usable fixes collected towards home.
    pub home_progress: u32,
    /// Horizontal distance from home, m.
    pub home_dist_m: Option<f32>,
    /// Height above home, m.
    pub home_dalt_m: Option<f32>,
    /// Last GPS-UTC minus hub-clock change against the reference, ms.
    pub time_step_ms: Option<i64>,
    /// Where home came from, when there is one.
    pub home_origin: Option<HomeOrigin>,
    /// A new home has been learned that disagrees with the current one: waiting for the
    /// operator ([`Monitor::confirm_home`]).
    pub home_pending: bool,
    /// The hub's clock is disciplined by trusted GPS time (else it has none of its own yet).
    pub time_disciplined: bool,
    /// GPS time is before the build date or the remembered time ([`Cause::TimeFloor`]).
    pub time_floor: bool,
}

/// Where the home the position checks use came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HomeOrigin {
    /// Learned this boot (15 minutes of steady fixes).
    Learned,
    /// Learned on an earlier boot and kept on the card.
    Remembered,
    /// Placed by the operator (`home` line in the site file): the strongest prior.
    Placed,
}

impl HomeOrigin {
    /// Short name for logs and JSON.
    pub fn name(self) -> &'static str {
        match self {
            HomeOrigin::Learned => "learned",
            HomeOrigin::Remembered => "remembered",
            HomeOrigin::Placed => "placed",
        }
    }
}

/// Something about home the hub should store or log ([`Monitor::take_home_event`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HomeEvent {
    /// Home learned (there was none): store it.
    Learned {
        /// Latitude, degrees.
        lat: f64,
        /// Longitude, degrees.
        lon: f64,
        /// Height above the ellipsoid, m.
        alt: Option<f64>,
    },
    /// A new place was learned that disagrees with home by `dist_m`: not adopted; the operator
    /// must confirm it.
    Pending {
        /// Latitude, degrees.
        lat: f64,
        /// Longitude, degrees.
        lon: f64,
        /// Distance from the current home, m.
        dist_m: f32,
    },
}

/// The monitor's answer. Cheap to copy.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Status {
    /// The verdict.
    pub state: State,
    /// The jamming level and the spoofing level separately.
    pub jamming: Level,
    /// See `jamming`.
    pub spoofing: Level,
    /// The check behind the state (the strongest one in the state's category).
    pub cause: Option<Finding>,
    /// How many checks of that category are counted.
    pub checks: u8,
    /// No spoofing check fired at all in the last epoch, not even an unconfirmed one: this
    /// epoch's position and time are as trustworthy as GPS gets.
    pub clean: bool,
    /// Clean, not in a spoofing state, and clean for [`TRUST_EPOCHS`] in a row: this epoch's
    /// position and time may be published and anchor the hub's clock.
    pub trusted: bool,
    /// The figures.
    pub metrics: Metrics,
}

impl Status {
    const fn new() -> Self {
        Status {
            state: State::Ok,
            jamming: Level::None,
            spoofing: Level::None,
            cause: None,
            checks: 0,
            clean: true,
            trusted: false,
            metrics: Metrics {
                listed: 0,
                tracked: 0,
                cn0_mean: 0.0,
                cn0_std: 0.0,
                cn0_top4: 0.0,
                cn0_baseline: None,
                agc: None,
                agc_baseline: None,
                cw: None,
                rx_jam: None,
                rx_spoof: None,
                home: false,
                home_progress: 0,
                home_dist_m: None,
                home_dalt_m: None,
                time_step_ms: None,
                home_origin: None,
                home_pending: false,
                time_disciplined: false,
                time_floor: false,
            },
        }
    }

    /// Do not take position or time from GPS now.
    pub fn hold(&self) -> bool {
        self.state.is_spoofing()
    }

    /// Write the plain-English reason, e.g. `possible GPS spoofing: position 412 m from
    /// where the hub has stood (accuracy 3 m)`.
    pub fn write_reason<W: Write>(&self, w: &mut W) -> fmt::Result {
        w.write_str(self.state.text())?;
        let Some(f) = self.cause else { return Ok(()) };
        w.write_str(": ")?;
        let m = &self.metrics;
        let v = f.value;
        match f.cause {
            Cause::RxJamFlag => write!(w, "receiver reports jamming ({})", if v >= 3 { "critical, no fix" } else { "warning" }),
            Cause::CwJam => write!(w, "receiver's CW jamming indicator at {v}/255"),
            Cause::AgcDrop => write!(w, "receiver turned its RF gain {v}% below normal: strong signal in the GPS band"),
            Cause::Cn0Collapse => write!(
                w,
                "signal strength fell {}.{} dB on all satellites at once ({} of {} still heard)",
                v / 10,
                v % 10,
                m.tracked,
                m.listed
            ),
            Cause::RxSpoofFlag => write!(
                w,
                "receiver's spoofing detector: {}",
                if v >= 3 { "multiple spoofing indications" } else { "spoofing indicated" }
            ),
            Cause::UniformCn0 => write!(
                w,
                "{} satellites all near {:.0} dB-Hz (spread {}.{} dB), like one transmitter",
                m.tracked,
                m.cn0_mean,
                v / 10,
                v % 10
            ),
            Cause::StrongCn0 => write!(w, "satellite signals at {v} dB-Hz, stronger than signals from space"),
            Cause::OffHome => write!(w, "position {v} m from where the hub has stood"),
            Cause::PositionJump => write!(w, "position jumped {v} m between two fixes"),
            Cause::AltitudeOffHome => write!(w, "altitude {v} m off the learned height"),
            Cause::TimeJump => write!(w, "GPS time jumped {v} ms against the hub's clock"),
            Cause::ClockBiasStep => write!(w, "receiver clock stepped {v} ns"),
            Cause::ClockDriftStep => write!(w, "receiver clock rate stepped {v} ns/s"),
            Cause::ImpossibleSat => write!(w, "{v} satellites with impossible numbers or positions"),
            Cause::TimeFloor => write!(w, "GPS time {} s before the build date or the last time the hub trusted", -i64::from(v)),
        }?;
        if self.checks > 1 {
            write!(w, " (+{} more check{})", self.checks - 1, if self.checks > 2 { "s" } else { "" })?;
        }
        if self.hold() {
            w.write_str("; holding last trusted position and time")?;
        }
        Ok(())
    }
}

/// A state change, returned by [`Monitor::on_fix`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Change {
    /// Before.
    pub from: State,
    /// The new status.
    pub status: Status,
}

#[derive(Clone, Copy, Default)]
struct Ind {
    /// Consecutive epochs the raw check fired.
    run: u16,
    /// One-shot: epochs left counted.
    latch: u16,
    level: Level,
    value: i32,
}

#[derive(Clone, Copy, Default)]
struct Ema {
    v: f32,
    n: u16,
}

impl Ema {
    fn add(&mut self, x: f32) {
        if self.n == 0 {
            self.v = x;
        } else {
            let k = 1.0 / BASELINE_EPOCHS.min(f32::from(self.n) + 1.0);
            self.v += (x - self.v) * k;
        }
        self.n = self.n.saturating_add(1);
    }
    fn get(&self) -> Option<f32> {
        (self.n >= BASELINE_MIN).then_some(self.v)
    }
}

#[derive(Clone, Copy)]
struct Home {
    lat: f64,
    lon: f64,
    alt: Option<f64>,
    origin: HomeOrigin,
}

#[derive(Clone, Copy, Default)]
struct Learn {
    n: u32,
    lat: f64,
    lon: f64,
    alt: f64,
    alt_n: u32,
}

/// The integrity monitor. About 820 bytes (WP-12 added about 200: the time guard, a waiting
/// home, the trust run); no allocation.
pub struct Monitor {
    inds: [Ind; N_CAUSES],
    status: Status,
    lower_run: u16,
    // Receiver indicators and when they arrived.
    rf: Option<(MonRf, u64)>,
    nav_status: Option<(NavStatus, u64)>,
    clock: Option<(NavClock, u64)>,
    prev_clock: Option<NavClock>,
    clock_used: Option<u32>,
    quectel_jam: [Option<(u8, u64)>; 2],
    // Learned.
    cn0_base: Ema,
    agc_base: Ema,
    home: Option<Home>,
    learn: Learn,
    /// A learned place that disagrees with home, waiting for the operator.
    pending: Option<Home>,
    home_event: Option<HomeEvent>,
    last_fix: Option<(f64, f64, u64)>,
    time: crate::timeguard::TimeGuard,
    /// Latest PPS edge, hub microseconds.
    pps_us: Option<u64>,
    /// Clean epochs in a row (not in a spoofing state).
    clean_run: u16,
    last_sats_seq: u32,
    was_usable: bool,
}

impl Default for Monitor {
    fn default() -> Self {
        Self::new()
    }
}

impl Monitor {
    /// Nothing learned yet.
    pub const fn new() -> Self {
        const I: Ind = Ind { run: 0, latch: 0, level: Level::None, value: 0 };
        Monitor {
            inds: [I; N_CAUSES],
            status: Status::new(),
            lower_run: 0,
            rf: None,
            nav_status: None,
            clock: None,
            prev_clock: None,
            clock_used: None,
            quectel_jam: [None; 2],
            cn0_base: Ema { v: 0.0, n: 0 },
            agc_base: Ema { v: 0.0, n: 0 },
            home: None,
            learn: Learn { n: 0, lat: 0.0, lon: 0.0, alt: 0.0, alt_n: 0 },
            pending: None,
            home_event: None,
            last_fix: None,
            time: crate::timeguard::TimeGuard::new(0),
            pps_us: None,
            clean_run: 0,
            last_sats_seq: 0,
            was_usable: false,
        }
    }

    /// The current verdict.
    pub fn status(&self) -> &Status {
        &self.status
    }

    /// The learned home `(lat, lon, height above ellipsoid)`, if any.
    pub fn home(&self) -> Option<(f64, f64, Option<f64>)> {
        self.home.map(|h| (h.lat, h.lon, h.alt))
    }

    /// Where home came from, if there is one.
    pub fn home_origin(&self) -> Option<HomeOrigin> {
        self.home.map(|h| h.origin)
    }

    /// The learned place waiting for the operator, `(lat, lon, alt)`.
    pub fn home_pending(&self) -> Option<(f64, f64, Option<f64>)> {
        self.pending.map(|h| (h.lat, h.lon, h.alt))
    }

    /// Forget home (the hub was moved on purpose) and learn it again. Clears the position
    /// and altitude checks at once.
    pub fn forget_home(&mut self) {
        self.home = None;
        self.pending = None;
        self.learn = Learn::default();
        self.clear_home_checks();
    }

    fn clear_home_checks(&mut self) {
        for c in [Cause::OffHome, Cause::AltitudeOffHome, Cause::PositionJump] {
            self.inds[c as usize] = Ind::default();
        }
        self.last_fix = None;
    }

    /// Use this home (remembered on the card, or placed in the site file). A placed home
    /// replaces anything; a remembered one only fills an empty home (one learned this boot
    /// wins: it is the more recent). Returns whether it was taken.
    pub fn set_home(&mut self, lat: f64, lon: f64, alt: Option<f64>, origin: HomeOrigin) -> bool {
        if origin != HomeOrigin::Placed && self.home.is_some() {
            return false;
        }
        self.home = Some(Home { lat, lon, alt, origin });
        self.pending = None;
        self.learn = Learn::default();
        self.clear_home_checks();
        true
    }

    /// The operator confirms the learned place waiting since a [`HomeEvent::Pending`]: it
    /// becomes home. Returns it, or `None` (nothing waiting, or home is placed: the site file
    /// wins and must be edited).
    pub fn confirm_home(&mut self) -> Option<(f64, f64, Option<f64>)> {
        if self.home.is_some_and(|h| h.origin == HomeOrigin::Placed) {
            return None;
        }
        let p = self.pending.take()?;
        self.home = Some(Home { origin: HomeOrigin::Learned, ..p });
        self.learn = Learn::default();
        self.clear_home_checks();
        Some((p.lat, p.lon, p.alt))
    }

    /// A home learned, or a new place waiting for the operator, since the last call.
    pub fn take_home_event(&mut self) -> Option<HomeEvent> {
        self.home_event.take()
    }

    /// The build-date floor for GPS time, Unix ms.
    pub fn set_time_floor(&mut self, floor_ms: i64) {
        self.time.set_floor(floor_ms);
    }

    /// The latest time the hub remembers trusting (from its card), Unix ms.
    pub fn set_remembered_time(&mut self, utc_ms: Option<i64>) {
        self.time.set_remembered(utc_ms);
    }

    /// The time guard (references, floor, remembered time).
    pub fn time_guard(&self) -> &crate::timeguard::TimeGuard {
        &self.time
    }

    /// The operator accepts GPS time as it now is: the hub's clock references and the
    /// remembered time are dropped and re-learned (the build-date floor stays).
    pub fn accept_time(&mut self) {
        self.time.reset();
        self.inds[Cause::TimeJump as usize] = Ind::default();
        self.inds[Cause::TimeFloor as usize] = Ind::default();
    }

    /// The latest PPS edge, hub microseconds (call before feeding each epoch's bytes).
    pub fn set_pps(&mut self, edge_us: Option<u64>) {
        self.pps_us = edge_us;
    }

    /// Feed every parser event. Receiver indicators are kept for the next epoch; a
    /// [`Event::Fix`] runs the checks (`sats` is [`crate::Parser::sats`]). Returns the
    /// change when the state changed.
    pub fn on_event(&mut self, ev: &Event, sats: &SatTable, now_ms: u64) -> Option<Change> {
        match ev {
            Event::MonRf(r) => self.rf = Some((*r, now_ms)),
            Event::NavStatus(s) => self.nav_status = Some((*s, now_ms)),
            Event::NavClock(c) => {
                if let Some((old, _)) = self.clock {
                    self.prev_clock = Some(old);
                }
                self.clock = Some((*c, now_ms));
            }
            Event::Quectel(Reply::Jamming { l5, status }) => {
                self.quectel_jam[usize::from(*l5)] = Some((*status, now_ms));
            }
            Event::Fix(f) => return self.on_fix(f, sats, now_ms),
            _ => {}
        }
        None
    }

    fn fresh<T: Copy>(v: Option<(T, u64)>, now: u64) -> Option<T> {
        v.filter(|(_, t)| now.saturating_sub(*t) <= AUX_FRESH_MS).map(|(x, _)| x)
    }

    /// One navigation epoch. Returns the change when the state changed.
    pub fn on_fix(&mut self, fix: &Fix, sats: &SatTable, now_ms: u64) -> Option<Change> {
        let mut raw = [(Level::None, 0i32); N_CAUSES];
        let mut m = Metrics {
            home: self.home.is_some(),
            home_progress: self.learn.n,
            cn0_baseline: self.cn0_base.get(),
            agc_baseline: self.agc_base.get(),
            ..Metrics::default()
        };
        let usable = fix.is_usable();

        // ---- satellites (only when this epoch brought a fresh table) ----
        let fresh_sats = sats.seq != 0 && sats.seq != self.last_sats_seq;
        self.last_sats_seq = sats.seq;
        let st: Option<Cn0Stats> = fresh_sats.then(|| sats.cn0_stats(Band::L1));
        if let Some(st) = st {
            let std = sqrt(st.var);
            m.listed = st.listed;
            m.tracked = st.tracked;
            m.cn0_mean = st.mean;
            m.cn0_std = std;
            m.cn0_top4 = st.top4;
            // Jamming: all satellites fall at once while the receiver still lists them.
            if let Some(base) = self.cn0_base.get() {
                let drop = base - if st.tracked > 0 { st.top4 } else { 0.0 };
                if drop >= CN0_DROP_DB {
                    raw[Cause::Cn0Collapse as usize] = (Level::Suspect, (drop * 10.0) as i32);
                }
            }
            // Spoofing: one transmitter makes every satellite equally (and very) strong.
            if st.tracked >= UNIFORM_MIN_SATS
                && std < UNIFORM_MAX_STD_DB
                && st.max - st.min <= UNIFORM_MAX_RANGE_DB
                && st.mean >= UNIFORM_MIN_MEAN
            {
                let lvl = if st.mean >= UNIFORM_STRONG_MEAN { Level::Strong } else { Level::Suspect };
                raw[Cause::UniformCn0 as usize] = (lvl, (std * 10.0) as i32);
            }
            if st.strong >= TOO_STRONG_SATS {
                raw[Cause::StrongCn0 as usize] = (Level::Suspect, i32::from(st.max));
            }
            let bad = sats.sats().iter().filter(|s| !s.plausible).count();
            if bad > 0 {
                raw[Cause::ImpossibleSat as usize] = (Level::Suspect, bad as i32);
            }
        }

        // ---- receiver indicators ----
        let rf = Self::fresh(self.rf, now_ms);
        if let Some(rf) = rf {
            let jam = rf.blocks().iter().map(|b| b.jamming_state).max().unwrap_or(0);
            let cw = rf.blocks().iter().map(|b| b.cw_suppression).max().unwrap_or(0);
            m.cw = Some(cw);
            m.rx_jam = Some(jam);
            if cw >= CW_SUSPECT {
                raw[Cause::CwJam as usize] = (Level::Suspect, i32::from(cw));
            }
            if let Some(b) = rf.blocks().first() {
                m.agc = Some(b.agc_cnt);
                if let Some(base) = self.agc_base.get()
                    && base > 0.0
                {
                    let frac = (base - f32::from(b.agc_cnt)) / base;
                    if frac >= AGC_DROP_FRAC {
                        raw[Cause::AgcDrop as usize] = (Level::Suspect, (frac * 100.0) as i32);
                    }
                }
            }
        }
        let q = [Self::fresh(self.quectel_jam[0], now_ms), Self::fresh(self.quectel_jam[1], now_ms)];
        let rx_jam = m.rx_jam.into_iter().chain(q.iter().flatten().copied()).max();
        m.rx_jam = rx_jam;
        if let Some(j) = rx_jam
            && j >= 2
        {
            raw[Cause::RxJamFlag as usize] = (if j >= 3 { Level::Strong } else { Level::Suspect }, i32::from(j));
        }
        if let Some(s) = Self::fresh(self.nav_status, now_ms) {
            m.rx_spoof = Some(s.spoof_det_state);
            if s.spoof_det_state >= 2 {
                let lvl = if s.spoof_det_state >= 3 { Level::Strong } else { Level::Suspect };
                raw[Cause::RxSpoofFlag as usize] = (lvl, i32::from(s.spoof_det_state));
            }
        }
        if let Some(c) = Self::fresh(self.clock, now_ms)
            && self.clock_used != Some(c.itow_ms)
        {
            self.clock_used = Some(c.itow_ms);
            // Only between two epochs that both had a fix: a reacquisition re-estimates the
            // clock and may legitimately step it.
            if let Some(p) = self.prev_clock
                && usable
                && self.was_usable
            {
                let dt_ms = i64::from(c.itow_ms.wrapping_sub(p.itow_ms) as i32);
                if (500..=1_500).contains(&dt_ms) {
                    let dd = c.drift_ns_s.saturating_sub(p.drift_ns_s);
                    if dd.abs() > CLK_DRIFT_STEP_NS_S {
                        raw[Cause::ClockDriftStep as usize] = (Level::Suspect, dd);
                    }
                    let pred = i64::from(p.bias_ns) + i64::from(p.drift_ns_s) * dt_ms / 1000;
                    // Whole-millisecond clock jumps are normal receiver housekeeping.
                    let mut r = (i64::from(c.bias_ns) - pred) % 1_000_000;
                    if r > 500_000 {
                        r -= 1_000_000;
                    } else if r < -500_000 {
                        r += 1_000_000;
                    }
                    if r.abs() > CLK_BIAS_STEP_NS {
                        raw[Cause::ClockBiasStep as usize] = (Level::Suspect, r as i32);
                    }
                }
            }
        }
        self.was_usable = usable;

        // ---- position ----
        let h_acc = fix.h_acc_m.or(fix.hdop.map(|h| h * UERE_M));
        let v_acc = fix.v_acc_m.or(h_acc.map(|a| a * 1.5));
        let alt = fix.alt_ellipsoid_m.or(fix.alt_msl_m);
        if usable {
            if let Some(h) = self.home {
                let d = dist_m(h.lat, h.lon, fix.lat, fix.lon);
                m.home_dist_m = Some(d);
                let a = h_acc.unwrap_or(10.0);
                if d > HOME_STRONG_M.max(6.0 * a) {
                    raw[Cause::OffHome as usize] = (Level::Strong, d as i32);
                } else if d > HOME_SUSPECT_M.max(4.0 * a) {
                    raw[Cause::OffHome as usize] = (Level::Suspect, d as i32);
                }
                if let (Some(ha), Some(fa)) = (h.alt, alt) {
                    let dz = fa as f64 - ha;
                    m.home_dalt_m = Some(dz as f32);
                    let a = v_acc.unwrap_or(15.0);
                    let adz = abs(dz as f32);
                    if adz > ALT_STRONG_M.max(6.0 * a) {
                        raw[Cause::AltitudeOffHome as usize] = (Level::Strong, dz as i32);
                    } else if adz > ALT_SUSPECT_M.max(4.0 * a) {
                        raw[Cause::AltitudeOffHome as usize] = (Level::Suspect, dz as i32);
                    }
                }
            }
            if let Some((la, lo, t)) = self.last_fix {
                let d = dist_m(la, lo, fix.lat, fix.lon);
                let dt = (now_ms.saturating_sub(t) as f32 / 1000.0).max(1.0);
                if d > JUMP_M && d / dt > JUMP_MPS {
                    raw[Cause::PositionJump as usize] = (Level::Suspect, d as i32);
                }
            }
        }

        // ---- time ----
        let utc_trusted = fix.utc.filter(|_| fix.source == Source::Nmea || fix.utc_fully_resolved);
        let at_us = now_ms.saturating_mul(1000);
        // The PPS edge of this epoch's second: under a second before the epoch was read.
        let pps_us = self.pps_us.filter(|&e| e <= at_us && at_us - e < 1_000_000);
        let sample = utc_trusted.map(|u| crate::timeguard::Sample { utc_ms: u.unix_ms(), at_us, pps_us });
        if let Some(s) = sample {
            let c = self.time.check(&s);
            m.time_step_ms = Some(c.step_ms);
            if c.jump {
                let v = c.step_ms.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
                raw[Cause::TimeJump as usize] = (Level::Suspect, v);
            }
            if let Some(before_s) = self.time.floor_check(s.utc_ms) {
                m.time_floor = true;
                raw[Cause::TimeFloor as usize] = (Level::Suspect, before_s.max(i64::from(i32::MIN)) as i32);
            }
        }

        // ---- combine ----
        let clean = Cause::ALL.iter().filter(|c| c.is_spoofing()).all(|&c| raw[c as usize].0 == Level::None);
        for c in Cause::ALL {
            let i = &mut self.inds[c as usize];
            let (lvl, val) = raw[c as usize];
            if c.one_shot() {
                if lvl > Level::None {
                    i.latch = LATCH_EPOCHS;
                    i.level = lvl;
                    i.value = val;
                } else if i.latch > 0 {
                    i.latch -= 1;
                    if i.latch == 0 {
                        i.level = Level::None;
                    }
                }
            } else if lvl > Level::None {
                i.run = i.run.saturating_add(1);
                if i.run >= c.sustain() {
                    i.level = lvl;
                    i.value = val;
                }
            } else {
                i.run = 0;
                i.level = Level::None;
            }
        }
        // A collapse that never ends, with nothing else pointing at jamming: the hub was
        // moved somewhere with a weaker sky. Learn it.
        let cn0 = &self.inds[Cause::Cn0Collapse as usize];
        let other_jam = [Cause::RxJamFlag, Cause::CwJam, Cause::AgcDrop].iter().any(|&c| self.inds[c as usize].level > Level::None);
        if cn0.run >= CN0_REBASE_EPOCHS && !other_jam {
            self.cn0_base = Ema::default();
            self.inds[Cause::Cn0Collapse as usize] = Ind::default();
        }

        let (jam, jam_n, jam_top) = self.category(false);
        let (spoof, spoof_n, spoof_top) = self.category(true);
        let target = match (spoof, jam) {
            (Level::Strong, _) => State::Spoofing,
            (Level::Suspect, _) => State::SuspectSpoofing,
            (_, Level::Strong) => State::Jamming,
            (_, Level::Suspect) => State::SuspectJamming,
            _ => State::Ok,
        };
        let from = self.status.state;
        let mut state = from;
        if target > from {
            state = target;
            self.lower_run = 0;
        } else if target < from {
            self.lower_run += 1;
            if self.lower_run >= CLEAR_EPOCHS {
                state = target;
                self.lower_run = 0;
            }
        } else {
            self.lower_run = 0;
        }
        let (cause, checks) = if state.is_spoofing() {
            (spoof_top, spoof_n)
        } else if state.is_jamming() {
            (jam_top, jam_n)
        } else {
            (None, 0)
        };

        // ---- trust ----
        if clean && !state.is_spoofing() {
            self.clean_run = self.clean_run.saturating_add(1);
        } else {
            self.clean_run = 0;
        }
        let trusted = self.clean_run >= TRUST_EPOCHS;
        if trusted && let Some(s) = sample {
            self.time.commit(&s);
        }

        // ---- learn (only when all is well) ----
        if state == State::Ok && clean {
            if let Some(st) = st
                && st.tracked >= 4
                && raw[Cause::Cn0Collapse as usize].0 == Level::None
            {
                self.cn0_base.add(st.top4);
            }
            if let Some(rf) = rf
                && let Some(b) = rf.blocks().first()
                && usable
                && raw[Cause::AgcDrop as usize].0 == Level::None
            {
                self.agc_base.add(f32::from(b.agc_cnt));
            }
            if usable && self.home.is_none() {
                self.learn_home(fix, h_acc, alt);
            }
        }
        // With a home: keep learning where the hub stands while the only complaint is the
        // position itself, so a real move can be confirmed by the operator (never adopted
        // on its own).
        let only_position = Cause::ALL
            .iter()
            .filter(|c| c.is_spoofing() && !matches!(c, Cause::OffHome | Cause::AltitudeOffHome | Cause::PositionJump))
            .all(|&c| raw[c as usize].0 == Level::None && self.inds[c as usize].level == Level::None);
        if usable && self.home.is_some() && only_position {
            self.learn_home(fix, h_acc, alt);
        }
        if usable && (clean || self.last_fix.is_none()) {
            self.last_fix = Some((fix.lat, fix.lon, now_ms));
        }
        m.home = self.home.is_some();
        m.home_progress = self.learn.n;
        m.home_origin = self.home.map(|h| h.origin);
        m.home_pending = self.pending.is_some();
        m.time_disciplined = self.time.disciplined();

        self.status = Status {
            state,
            jamming: jam,
            spoofing: spoof,
            cause,
            checks,
            clean,
            trusted,
            metrics: m,
        };
        (state != from).then_some(Change { from, status: self.status })
    }

    /// Level, number of counted checks, and the strongest one, for one category.
    fn category(&self, spoofing: bool) -> (Level, u8, Option<Finding>) {
        let mut lvl = Level::None;
        let mut n = 0u8;
        let mut top: Option<Finding> = None;
        for c in Cause::ALL.iter().copied().filter(|c| c.is_spoofing() == spoofing) {
            let i = &self.inds[c as usize];
            if i.level == Level::None {
                continue;
            }
            n += 1;
            let f = Finding { cause: c, level: i.level, value: i.value };
            // Primary checks first, then by level; ties keep the earlier (better-documented)
            // check.
            let rank = |f: &Finding| (!f.cause.corroborating(), f.level);
            if top.is_none_or(|t| rank(&f) > rank(&t)) {
                top = Some(f);
            }
            if !c.corroborating() {
                lvl = lvl.max(i.level);
            }
        }
        if n >= 2 {
            lvl = if lvl >= Level::Suspect { Level::Strong } else { Level::Suspect };
        }
        (lvl, n, top)
    }

    fn learn_home(&mut self, fix: &Fix, h_acc: Option<f32>, alt: Option<f32>) {
        let acc = h_acc.unwrap_or(10.0);
        if acc > HOME_MAX_ACC_M {
            return;
        }
        let l = &mut self.learn;
        if l.n > 0 {
            let n = l.n as f64;
            let d = dist_m(l.lat / n, l.lon / n, fix.lat, fix.lon);
            if d > HOME_LEARN_RADIUS_M.max(3.0 * acc) {
                // Not still: start over from here.
                *l = Learn::default();
            }
        }
        l.n += 1;
        l.lat += fix.lat;
        l.lon += fix.lon;
        if let Some(a) = alt {
            l.alt += f64::from(a);
            l.alt_n += 1;
        }
        if l.n >= HOME_LEARN_EPOCHS {
            let n = l.n as f64;
            let alt = (l.alt_n * 2 >= l.n).then(|| l.alt / f64::from(l.alt_n));
            let new = Home { lat: l.lat / n, lon: l.lon / n, alt, origin: HomeOrigin::Learned };
            match self.home {
                None => {
                    self.home = Some(new);
                    self.home_event = Some(HomeEvent::Learned { lat: new.lat, lon: new.lon, alt });
                }
                Some(h) => {
                    // A second opinion on where the hub stands. Agreeing with home: nothing
                    // to do. Disagreeing by as much as the off-home check's suspect level:
                    // never adopted silently; the operator decides.
                    let d = dist_m(h.lat, h.lon, new.lat, new.lon);
                    if d > HOME_SUSPECT_M.max(4.0 * acc) {
                        if self.pending.is_none() {
                            self.home_event = Some(HomeEvent::Pending { lat: new.lat, lon: new.lon, dist_m: d });
                        }
                        self.pending = Some(new);
                    } else {
                        self.pending = None;
                    }
                    self.learn = Learn::default();
                }
            }
        }
    }
}

// ------------------------------------------------------------------ maths --

fn abs(x: f32) -> f32 {
    if x < 0.0 { -x } else { x }
}

/// Square root by Newton's method (no libm in `core`).
pub(crate) fn sqrt(v: f32) -> f32 {
    if v <= 0.0 {
        return 0.0;
    }
    let mut x = if v > 1.0 { v } else { 1.0 };
    for _ in 0..40 {
        let nx = 0.5 * (x + v / x);
        if abs(nx - x) <= 1e-6 * nx {
            return nx;
        }
        x = nx;
    }
    x
}

/// Cosine for |x| <= pi/2 (latitudes), Taylor series to x^14: error under 1e-9.
fn cos(x: f64) -> f64 {
    let x2 = x * x;
    let mut term = 1.0;
    let mut sum = 1.0;
    for k in 1..=7 {
        term *= -x2 / f64::from((2 * k - 1) * (2 * k));
        sum += term;
    }
    sum
}

/// Horizontal distance in metres (flat-earth: fine for the kilometres this deals with).
pub(crate) fn dist_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f32 {
    const M_PER_DEG: f64 = 111_195.0;
    let dlat = (lat2 - lat1) * M_PER_DEG;
    let mut dl = lon2 - lon1;
    if dl > 180.0 {
        dl -= 360.0;
    } else if dl < -180.0 {
        dl += 360.0;
    }
    let dlon = dl * M_PER_DEG * cos((lat1 + lat2) * 0.5 * (core::f64::consts::PI / 180.0));
    sqrt((dlat * dlat + dlon * dlon) as f32)
}
