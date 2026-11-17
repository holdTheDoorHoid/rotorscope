//! Spoofing and consistency checks on drone tracks.
//!
//! Remote ID has no authentication in practice, and transmitters are cheap:
//! ESP8266/ESP32 sketches and phone apps broadcast dozens of invented drones,
//! and DJI DroneID has been forged with an SDR. Nothing here can *prove* a
//! broadcast genuine. What the hub can do is notice when a broadcast
//! disagrees with physics, with itself, or with other things the unit hears.
//! `docs/SPOOFING.md` gives every check's threshold, reasoning, known false
//! alarms and what an attacker can still get away with.
//!
//! # Flags
//! Each check that fires leaves a [`Flag`] on the track: a [`Check`], a
//! confidence 0-100 and a `detail` number (units per check, see
//! [`Check`]). [`Reason`] turns one into a short plain-English sentence.
//! Flags are sticky for the life of the track; a confidence only rises.
//! A track keeps at most [`MAX_FLAGS`]; a stronger flag replaces the weakest.
//!
//! Some checks are **weak** (a fact that is also true of plenty of honest
//! drones, e.g. an Espressif transmitter address): their confidence stays
//! below [`REPORT_MIN`]. [`SpoofState::score`] combines flags as independent
//! evidence (`1 - prod(1 - c)`), but a track whose flags are all weak never
//! scores [`REPORT_MIN`] or more, so weak checks alone never raise an alarm.
//!
//! # For the hub
//! [`crate::Tracker::take_spoof_flag`] hands out each flag once when it
//! reaches [`REPORT_MIN`], and again each time it rises by
//! [`REREPORT_STEP`], as a [`SpoofEvent`]; log it as evidence event
//! `SpoofSuspect` with [`SpoofEvent::data`] and [`event_text`]. The hub
//! should tell the tracker UTC when it has it ([`crate::Tracker::set_utc`])
//! and, if it knows them, where its nodes are
//! ([`crate::Tracker::set_node_position`]); without them the clock and
//! signal-strength checks are skipped or use generous assumptions.
//!
//! # Privacy
//! Everything shown is what the broadcast itself said (its IDs, positions)
//! or a number derived from it. Nothing is looked up, and a DJI pilot's
//! account ID is never taken in anywhere in the tracker.

use core::fmt;

use heapless::Vec;
use libm::{atan2, cos, fabs, log10, sqrt};
use rid::Transport;
use rid::message::{
    Auth, AuthType, BasicId, HeightType, IdType, LatLon, Location, Message, Status, System,
};

use crate::dji::Source;
use crate::{Observation, Slot, Stamped, Track, TrackId, Tracker};

/// Flags kept per track.
pub const MAX_FLAGS: usize = 4;
/// A flag is handed to the hub ([`crate::Tracker::take_spoof_flag`]) from
/// this confidence up; a track whose flags are all below it scores below it.
pub const REPORT_MIN: u8 = 30;
/// A reported flag is reported again when its confidence rises this much.
pub const REREPORT_STEP: u8 = 20;
/// Nodes whose position can be set ([`crate::Tracker::set_node_position`]).
pub const MAX_NODE_POS: usize = 8;
/// Without a node position, a node is assumed to be at most this far from
/// the hub (an ESP-NOW node can be a few hundred metres away).
pub const NODE_SPREAD_M: f64 = 300.0;

/// The checks. The numeric code is what the evidence log stores (data byte
/// 0 of `SpoofSuspect`): **append only**.
///
/// `detail` units: metres for distances, m/s for speeds, dB for signal
/// excess, seconds for clocks, a count for repetitions, or a small code
/// documented on the variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum Check {
    /// Position jumped further than any drone flies in the time between
    /// frames. Detail: metres.
    PositionJump = 1,
    /// Positions move faster than the reported speed allows, or the track
    /// over ground disagrees with the reported direction. Detail: computed
    /// speed m/s (positive), or the angle in degrees (negative).
    MotionMismatch = 2,
    /// Geodetic altitude and height above takeoff changed by different
    /// amounts over the same few seconds. Detail: metres of disagreement.
    AltitudeMismatch = 3,
    /// A value no drone reports. Detail: 1 speed over 120 m/s, 2 altitude
    /// outside -500..9000 m, 3 height above ground below -50 m, 4 geodetic
    /// and pressure altitude over 800 m apart.
    ImplausibleValue = 4,
    /// The operator position is exactly the drone's while the drone moves.
    OperatorOnDrone = 5,
    /// The operator is tens of km from the drone. Detail: km.
    OperatorFar = 6,
    /// The operator position jumped. Detail: metres.
    OperatorJump = 7,
    /// Received far louder than free-space loss allows at the claimed
    /// range. Detail: dB over the (generous) limit.
    RssiTooStrong = 8,
    /// One transmitter address sends several drone IDs. Detail: tracks
    /// sharing the address.
    SharedAddress = 9,
    /// One drone ID arrives from addresses that alternate. Detail: flips.
    AddressAlternation = 10,
    /// One of several drones that appeared together on one node with the
    /// same signal strength. Detail: group size.
    SpooferSwarm = 11,
    /// The serial number does not follow ANSI/CTA-2063-A. Detail: a
    /// [`SerialFault`] code.
    SerialFormat = 12,
    /// The transmitter's maker does not match the drone's. Detail: 1 DJI
    /// serial from a non-DJI Wi-Fi address, 2 DJI serial from an Espressif
    /// address, 3 non-DJI serial from a DJI address, 4 DJI Wi-Fi DroneID
    /// from an Espressif address, 5 DJI Wi-Fi DroneID from another non-DJI
    /// address.
    VendorMismatch = 13,
    /// DJI DroneID and Remote ID put the same drone in different places.
    /// Detail: metres.
    DroneIdDisagrees = 14,
    /// The Location timestamp is far from the hub's UTC. Detail: seconds.
    ClockSkew = 15,
    /// Location timestamps do not advance with reception time (they jump or
    /// go backwards). Detail: seconds of disagreement.
    TimeInconsistent = 16,
    /// An identical Location message (position and timestamp) came back
    /// seconds later: a replay. Detail: repeats.
    Replay = 17,
    /// The F3411 message counter does not advance. Detail: percent frozen.
    FrozenCounter = 18,
    /// A Standard Remote ID message is never sent. Detail: 1 no System
    /// (operator) message, 2 no Basic ID.
    IncompleteMessageSet = 19,
    /// A DJI drone with no DJI video or control link seen by an RF node
    /// that was running.
    NoDjiRf = 20,
    /// The transmitter address belongs to Espressif (ESP32/ESP8266).
    /// Detail: 0 Bluetooth, 1 Wi-Fi.
    DevBoard = 21,
    /// Authentication pages that contradict themselves. Detail: count.
    AuthMalformed = 22,
    /// The drone appears to have changed its ID mid-flight (see
    /// [`crate::continuity`]). Detail: the previous track's id.
    IdentityChanged = 23,
}

impl Check {
    pub const ALL: [Check; 23] = [
        Check::PositionJump,
        Check::MotionMismatch,
        Check::AltitudeMismatch,
        Check::ImplausibleValue,
        Check::OperatorOnDrone,
        Check::OperatorFar,
        Check::OperatorJump,
        Check::RssiTooStrong,
        Check::SharedAddress,
        Check::AddressAlternation,
        Check::SpooferSwarm,
        Check::SerialFormat,
        Check::VendorMismatch,
        Check::DroneIdDisagrees,
        Check::ClockSkew,
        Check::TimeInconsistent,
        Check::Replay,
        Check::FrozenCounter,
        Check::IncompleteMessageSet,
        Check::NoDjiRf,
        Check::DevBoard,
        Check::AuthMalformed,
        Check::IdentityChanged,
    ];

    /// The evidence-log code.
    pub const fn code(self) -> u8 {
        self as u8
    }

    pub fn from_code(code: u8) -> Option<Check> {
        Check::ALL.into_iter().find(|c| c.code() == code)
    }

    /// The name JSON uses (snake case).
    pub const fn name(self) -> &'static str {
        match self {
            Check::PositionJump => "position_jump",
            Check::MotionMismatch => "motion_mismatch",
            Check::AltitudeMismatch => "altitude_mismatch",
            Check::ImplausibleValue => "implausible_value",
            Check::OperatorOnDrone => "operator_on_drone",
            Check::OperatorFar => "operator_far",
            Check::OperatorJump => "operator_jump",
            Check::RssiTooStrong => "rssi_too_strong",
            Check::SharedAddress => "shared_address",
            Check::AddressAlternation => "address_alternation",
            Check::SpooferSwarm => "spoofer_swarm",
            Check::SerialFormat => "serial_format",
            Check::VendorMismatch => "vendor_mismatch",
            Check::DroneIdDisagrees => "drone_id_disagrees",
            Check::ClockSkew => "clock_skew",
            Check::TimeInconsistent => "time_inconsistent",
            Check::Replay => "replay",
            Check::FrozenCounter => "frozen_counter",
            Check::IncompleteMessageSet => "incomplete_message_set",
            Check::NoDjiRf => "no_dji_rf",
            Check::DevBoard => "dev_board",
            Check::AuthMalformed => "auth_malformed",
            Check::IdentityChanged => "identity_changed",
        }
    }
}

/// One check that fired on a track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flag {
    pub check: Check,
    /// 0-100.
    pub confidence: u8,
    /// See [`Check`] for the units.
    pub detail: i32,
    /// Confidence last handed to the hub (0: never).
    reported: u8,
}

impl Flag {
    pub fn reason(&self) -> Reason {
        Reason {
            check: self.check,
            detail: self.detail,
        }
    }
}

/// A short plain-English sentence for one flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reason {
    pub check: Check,
    pub detail: i32,
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let d = self.detail;
        match self.check {
            Check::PositionJump => write!(f, "position jumped {d} m, faster than any drone flies"),
            Check::MotionMismatch if d < 0 => {
                write!(f, "moves {} deg away from the direction it reports", -d)
            }
            Check::MotionMismatch => {
                write!(
                    f,
                    "positions move at {d} m/s, faster than its reported speed"
                )
            }
            Check::AltitudeMismatch => {
                write!(f, "GPS altitude and height above takeoff disagree by {d} m")
            }
            Check::ImplausibleValue => f.write_str(match d {
                1 => "reports a speed over 120 m/s",
                2 => "reports an altitude no drone flies at",
                3 => "reports a height far below the ground",
                _ => "GPS and pressure altitude are over 800 m apart",
            }),
            Check::OperatorOnDrone => {
                f.write_str("operator position is exactly the drone's while it flies")
            }
            Check::OperatorFar => write!(f, "operator is {d} km from the drone"),
            Check::OperatorJump => write!(f, "operator position jumped {d} m"),
            Check::RssiTooStrong => write!(
                f,
                "signal {d} dB stronger than possible at its claimed distance"
            ),
            Check::SharedAddress => {
                write!(f, "its transmitter address sends {d} different drone IDs")
            }
            Check::AddressAlternation => write!(
                f,
                "its ID comes from alternating transmitter addresses ({d} switches)"
            ),
            Check::SpooferSwarm => write!(
                f,
                "one of {d} drones that appeared at once with the same signal strength"
            ),
            Check::SerialFormat => write!(
                f,
                "serial number is not ANSI/CTA-2063-A: {}",
                SerialFault::from_code(d).map_or("malformed", SerialFault::text)
            ),
            Check::VendorMismatch => f.write_str(match d {
                1 => "DJI serial number from a non-DJI Wi-Fi transmitter",
                2 => "DJI serial number from an ESP32/ESP8266 transmitter",
                3 => "non-DJI serial number from a DJI Wi-Fi transmitter",
                4 => "DJI Wi-Fi DroneID from an ESP32/ESP8266 transmitter",
                _ => "DJI Wi-Fi DroneID from a non-DJI transmitter",
            }),
            Check::DroneIdDisagrees => {
                write!(f, "DJI DroneID puts it {d} m from its Remote ID position")
            }
            Check::ClockSkew => write!(f, "its clock is {d} s off GPS time"),
            Check::TimeInconsistent => {
                write!(f, "its timestamps jump {d} s against the time they arrived")
            }
            Check::Replay => write!(
                f,
                "identical position report repeated {d} times seconds apart (replay)"
            ),
            Check::FrozenCounter => write!(f, "message counter frozen in {d}% of frames"),
            Check::IncompleteMessageSet => f.write_str(if d == 2 {
                "never sends a Basic ID"
            } else {
                "never sends the operator (System) message"
            }),
            Check::NoDjiRf => {
                f.write_str("DJI drone, but no DJI video or control link on the RF scanner")
            }
            Check::DevBoard => f.write_str("transmitter is an ESP32/ESP8266 (Espressif) chip"),
            Check::AuthMalformed => write!(f, "{d} malformed authentication pages"),
            Check::IdentityChanged => write!(f, "changed identity mid-flight (was track {d})"),
        }
    }
}

/// Why a serial number fails ANSI/CTA-2063-A (detail of
/// [`Check::SerialFormat`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SerialFault {
    /// Shorter than manufacturer code, length code and one character.
    TooShort = 1,
    /// The 4-character manufacturer code has a character outside 0-9, A-Z
    /// without O and I.
    ManufacturerCode = 2,
    /// The length character is not 1-9 or A-F.
    LengthCode = 3,
    /// The serial part is not as long as the length character says.
    LengthMismatch = 4,
    /// The serial part has a character outside 0-9, A-Z without O and I.
    SerialChar = 5,
}

impl SerialFault {
    pub fn from_code(c: i32) -> Option<Self> {
        Some(match c {
            1 => Self::TooShort,
            2 => Self::ManufacturerCode,
            3 => Self::LengthCode,
            4 => Self::LengthMismatch,
            5 => Self::SerialChar,
            _ => return None,
        })
    }
    pub fn text(self) -> &'static str {
        match self {
            Self::TooShort => "too short",
            Self::ManufacturerCode => "bad manufacturer code",
            Self::LengthCode => "bad length character",
            Self::LengthMismatch => "length character does not match its length",
            Self::SerialChar => "character not allowed (O, I, lowercase or symbol)",
        }
    }
}

/// ANSI/CTA-2063-A: a 4-character manufacturer code, one length character
/// (`1`-`9`, `A`-`F` = 1-15), then that many serial characters. Allowed
/// characters are digits and upper-case letters except O and I.
pub fn cta2063_check(id: &[u8]) -> Result<(), SerialFault> {
    let ok = |c: &u8| c.is_ascii_digit() || (c.is_ascii_uppercase() && *c != b'O' && *c != b'I');
    if id.len() < 6 {
        return Err(SerialFault::TooShort);
    }
    if !id[..4].iter().all(ok) {
        return Err(SerialFault::ManufacturerCode);
    }
    let len = match id[4] {
        c @ b'1'..=b'9' => usize::from(c - b'0'),
        c @ b'A'..=b'F' => usize::from(c - b'A') + 10,
        _ => return Err(SerialFault::LengthCode),
    };
    let rest = &id[5..];
    if !rest.iter().all(ok) {
        return Err(SerialFault::SerialChar);
    }
    if rest.len() != len {
        return Err(SerialFault::LengthMismatch);
    }
    Ok(())
}

/// DJI's ANSI/CTA-2063-A manufacturer code, as on every DJI Remote ID
/// serial number we know of.
pub const DJI_MFR_CODE: &[u8; 4] = b"1581";

fn is_dji_serial(id: &[u8]) -> bool {
    id.starts_with(DJI_MFR_CODE)
}

/// A globally administered Wi-Fi address in one of DJI's blocks (the list
/// `crates/wifidrone` keeps).
pub fn dji_oui(mac: &[u8; 6]) -> bool {
    wifidrone::MAKERS
        .iter()
        .find(|m| m.name == "DJI")
        .is_some_and(|m| m.prefixes.iter().any(|p| p.matches(mac)))
}

/// A globally administered address in one of Espressif's MA-L blocks.
pub fn espressif_oui(mac: &[u8; 6]) -> bool {
    if mac[0] & 0x02 != 0 {
        return false;
    }
    let oui = u32::from(mac[0]) << 16 | u32::from(mac[1]) << 8 | u32::from(mac[2]);
    ESPRESSIF_OUIS.binary_search(&oui).is_ok()
}

fn wifi(t: Transport) -> bool {
    matches!(t, Transport::WifiBeacon | Transport::WifiNan)
}

// ------------------------------------------------------------ geometry --

/// Metres per degree of latitude (mean).
const M_PER_DEG: f64 = 111_132.0;

/// Short-range distance in metres (equirectangular; under 0.5 % error out
/// to 100 km, far cheaper than Vincenty on an FPU-less core).
pub(crate) fn dist_m(a: LatLon, b: LatLon) -> f64 {
    let (x, y) = enu(a, b);
    sqrt(x * x + y * y)
}

/// East and north offset of `b` from `a`, metres.
fn enu(a: LatLon, b: LatLon) -> (f64, f64) {
    let mid = ((a.lat + b.lat) / 2.0).to_radians();
    let mut dlon = b.lon - a.lon;
    if dlon > 180.0 {
        dlon -= 360.0;
    } else if dlon < -180.0 {
        dlon += 360.0;
    }
    (dlon * M_PER_DEG * cos(mid), (b.lat - a.lat) * M_PER_DEG)
}

fn bearing_deg(a: LatLon, b: LatLon) -> f64 {
    let (x, y) = enu(a, b);
    let d = atan2(x, y).to_degrees();
    if d < 0.0 { d + 360.0 } else { d }
}

fn angle_diff(a: f64, b: f64) -> f64 {
    let d = fabs(a - b) % 360.0;
    if d > 180.0 { 360.0 - d } else { d }
}

fn e7(p: LatLon) -> (i32, i32) {
    ((p.lat * 1e7) as i32, (p.lon * 1e7) as i32)
}

fn from_e7(v: (i32, i32)) -> LatLon {
    LatLon {
        lat: f64::from(v.0) / 1e7,
        lon: f64::from(v.1) / 1e7,
    }
}

/// Free-space path loss at 2.4 GHz, dB. 5.8 GHz loses 7.7 dB more, so
/// using 2.4 GHz for every band only makes the limit more generous.
fn fspl_24(d_m: f64) -> f64 {
    20.0 * log10(d_m.max(1.0)) + 40.05
}

/// The most a legal-power drone transmitter (30 dBm EIRP) can deliver to
/// a receiver with a 10 dBi antenna, plus 12 dB for two-ray gain and RSSI
/// calibration error.
const RSSI_HEADROOM_DB: f64 = 30.0 + 10.0 + 12.0;
/// Closer than this the limit is meaningless (near field, nodes not at the hub).
const RSSI_MIN_RANGE_M: f64 = 200.0;

/// The strongest plausible RSSI (dBm) at `d_m` metres.
pub fn rssi_limit_dbm(d_m: f64) -> f64 {
    RSSI_HEADROOM_DB - fspl_24(d_m)
}

// --------------------------------------------------------------- state --

/// Kinematics window: a position and what was reported since.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Anchor {
    pos: (i32, i32),
    /// Low 32 bits of hub ms (differences wrap correctly).
    ms: u32,
    /// Geodetic altitude and height, whole metres; `i16::MIN` = unknown.
    alt: i16,
    height: i16,
    /// Most reported ground speed in the window, 0.1 m/s.
    vmax: u16,
    /// Reported direction at the start, degrees; 0xFFFF = unknown.
    dir: u16,
    speeds: u8,
    valid: bool,
}

/// What the checks remember about one track. About 100 bytes; see
/// `docs/SPOOFING.md` for the RAM budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpoofState {
    flags: Vec<Flag, MAX_FLAGS>,
    anchor: Anchor,
    /// Recent Location fingerprints and when each was first heard.
    recent: [u32; 3],
    recent_ms: [u32; 3],
    recent_next: u8,
    pub(crate) loc_frames: u16,
    ctr_total: u16,
    ctr_same: u16,
    /// Bit n: authentication page n heard.
    pub auth_pages: u16,
    auth_last: u8,
    auth_bad: u8,
    jumps: u8,
    motion_bad: u8,
    motion_ok: u8,
    alt_bad: u8,
    value_bad: u8,
    op_same: u8,
    op_far: u8,
    op_jump: u8,
    rssi_bad: u8,
    rssi_ok: u8,
    rssi_excess: u8,
    mac_conflicts: u8,
    mac_conflicts_alike: u8,
    mac_flips: u8,
    skew_bad: u8,
    skew_ok: u8,
    time_bad: u8,
    replays: u8,
    droneid_bad: u8,
    droneid_ok: u8,
    /// Where the track was born: node (0xFF unknown), RSSI, slot index.
    pub(crate) birth_node: u8,
    pub(crate) birth_rssi: i8,
    pub(crate) birth_slot: u8,
    /// Size of the group of drones born with it (0: none).
    swarm: u8,
}

impl Default for SpoofState {
    fn default() -> Self {
        Self {
            flags: Vec::new(),
            anchor: Anchor::default(),
            recent: [0; 3],
            recent_ms: [0; 3],
            recent_next: 0,
            loc_frames: 0,
            ctr_total: 0,
            ctr_same: 0,
            auth_pages: 0,
            auth_last: 0xFF,
            auth_bad: 0,
            jumps: 0,
            motion_bad: 0,
            motion_ok: 0,
            alt_bad: 0,
            value_bad: 0,
            op_same: 0,
            op_far: 0,
            op_jump: 0,
            rssi_bad: 0,
            rssi_ok: 0,
            rssi_excess: 0,
            mac_conflicts: 0,
            mac_conflicts_alike: 0,
            mac_flips: 0,
            skew_bad: 0,
            skew_ok: 0,
            time_bad: 0,
            replays: 0,
            droneid_bad: 0,
            droneid_ok: 0,
            birth_node: 0xFF,
            birth_rssi: 0,
            birth_slot: 0,
            swarm: 0,
        }
    }
}

fn inc(c: &mut u8) -> u8 {
    *c = c.saturating_add(1);
    *c
}

impl SpoofState {
    /// Flags raised, strongest first is not guaranteed (table order).
    pub fn flags(&self) -> &[Flag] {
        &self.flags
    }

    pub fn flag(&self, check: Check) -> Option<&Flag> {
        self.flags.iter().find(|f| f.check == check)
    }

    /// Combined suspicion 0-100: flags as independent evidence. Below
    /// [`REPORT_MIN`] when every flag is weak.
    pub fn score(&self) -> u8 {
        let mut keep = 1.0f32;
        let mut strong = false;
        for f in &self.flags {
            keep *= 1.0 - f32::from(f.confidence.min(100)) / 100.0;
            strong |= f.confidence >= REPORT_MIN;
        }
        let s = libm::roundf(100.0 * (1.0 - keep)) as u8;
        if strong { s } else { s.min(REPORT_MIN - 1) }
    }

    /// The flag with the highest confidence.
    pub fn strongest(&self) -> Option<&Flag> {
        self.flags.iter().max_by_key(|f| f.confidence)
    }

    /// Raise (or strengthen) a flag. Confidence only rises; the detail
    /// follows the strongest evidence.
    pub(crate) fn raise(&mut self, check: Check, confidence: u8, detail: i32) {
        let confidence = confidence.min(100);
        if let Some(f) = self.flags.iter_mut().find(|f| f.check == check) {
            if confidence >= f.confidence {
                f.confidence = confidence;
                f.detail = detail;
            }
            return;
        }
        let flag = Flag {
            check,
            confidence,
            detail,
            reported: 0,
        };
        if self.flags.push(flag).is_err()
            && let Some(weak) = self.flags.iter_mut().min_by_key(|f| f.confidence)
            && weak.confidence < confidence
        {
            *weak = flag;
        }
    }

    pub(crate) fn clear(&mut self, check: Check) {
        self.flags.retain(|f| f.check != check);
    }

    /// Fold another track's state (the same drone) into this one.
    pub(crate) fn absorb(&mut self, o: &SpoofState) {
        for f in &o.flags {
            let reported = f.reported;
            self.raise(f.check, f.confidence, f.detail);
            if let Some(m) = self.flags.iter_mut().find(|m| m.check == f.check) {
                m.reported = m.reported.max(reported);
            }
        }
        let add = |a: &mut u8, b: u8| *a = a.saturating_add(b);
        add(&mut self.jumps, o.jumps);
        add(&mut self.motion_bad, o.motion_bad);
        add(&mut self.motion_ok, o.motion_ok);
        add(&mut self.alt_bad, o.alt_bad);
        add(&mut self.value_bad, o.value_bad);
        add(&mut self.mac_conflicts, o.mac_conflicts);
        add(&mut self.mac_conflicts_alike, o.mac_conflicts_alike);
        add(&mut self.mac_flips, o.mac_flips);
        add(&mut self.replays, o.replays);
        add(&mut self.auth_bad, o.auth_bad);
        self.loc_frames = self.loc_frames.saturating_add(o.loc_frames);
        self.auth_pages |= o.auth_pages;
        self.swarm = self.swarm.max(o.swarm);
    }
}

/// Environment the checks need besides the track.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct Env {
    /// Own position, when the fix is fresh.
    pub own: Option<LatLon>,
    /// UTC minus hub ms, when UTC is known.
    pub utc_offset_ms: Option<i64>,
    pub nodes: [Option<(i32, i32)>; MAX_NODE_POS],
}

impl Env {
    /// Distance from the node that heard a frame to `p`: exact when the
    /// node's position is known, else the distance from the hub less
    /// [`NODE_SPREAD_M`]. `None` without either.
    fn node_distance(&self, node: u8, p: LatLon) -> Option<f64> {
        if let Some(Some(n)) = self.nodes.get(usize::from(node)) {
            return Some(dist_m(from_e7(*n), p));
        }
        self.own.map(|o| (dist_m(o, p) - NODE_SPREAD_M).max(0.0))
    }
}

/// What the tracker keeps for the checks across all tracks.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct SpoofConfig {
    pub utc_offset_ms: Option<i64>,
    pub nodes: [Option<(i32, i32)>; MAX_NODE_POS],
}

// -------------------------------------------------------------- checks --

/// Positions closer than this are "the same" (well under GPS noise, above
/// the 1e-7 degree quantisation).
const SAME_POS_M: f64 = 0.5;
/// Fastest a drone is assumed to fly, m/s (FAA Part 107 allows 44; racing
/// quads and fixed-wing UAS reach somewhat more).
const MAX_DRONE_MPS: f64 = 100.0;
/// Fastest an operator is assumed to move (a pilot in a vehicle), m/s.
const MAX_OPERATOR_MPS: f64 = 70.0;
/// A swarm: births within this window...
const SWARM_WINDOW_MS: u64 = 10_000;
/// ...on one node and slot, within this many dB of each other.
const SWARM_RSSI_DB: i16 = 3;
const SWARM_MIN: u8 = 4;
/// Another track sharing the MAC counts if heard this recently.
pub(crate) const SHARED_MAC_WINDOW_MS: u64 = 5_000;

fn loc_fingerprint(l: &Location) -> u32 {
    // FNV-1a over the fields a replay keeps identical.
    let mut h: u32 = 0x811c_9dc5;
    let mut eat = |v: u64| {
        for b in v.to_le_bytes() {
            h ^= u32::from(b);
            h = h.wrapping_mul(0x0100_0193);
        }
    };
    let f = |v: Option<f32>| u64::from(v.map_or(u32::MAX, f32::to_bits));
    if let Some(p) = l.position {
        eat(p.lat.to_bits());
        eat(p.lon.to_bits());
    }
    eat(f(l.altitude_geodetic));
    eat(f(l.altitude_pressure));
    eat(f(l.height));
    eat(f(l.speed_horizontal));
    eat(f(l.speed_vertical));
    eat(f(l.direction));
    eat(f(l.timestamp));
    h
}

/// Seconds `a - b` wrapped into -1800..1800 (timestamps are seconds past
/// the hour).
fn hour_diff(a: f64, b: f64) -> f64 {
    let mut d = (a - b) % 3600.0;
    if d >= 1800.0 {
        d -= 3600.0;
    } else if d < -1800.0 {
        d += 3600.0;
    }
    d
}

fn moving(l: &Location) -> bool {
    l.status == Status::Airborne
        || l.speed_horizontal.is_some_and(|v| v > 2.0)
        || l.height.is_some_and(|h| h > 10.0)
}

/// Checks on a Remote ID Location, against the track's previous one.
fn location(
    s: &mut SpoofState,
    prev: Option<&Stamped<Location>>,
    l: &Location,
    obs: &Observation,
    env: &Env,
) {
    let now = obs.rx_ms;
    s.loc_frames = s.loc_frames.saturating_add(1);

    // Absurd values.
    let bad_value = if l.speed_horizontal.is_some_and(|v| v > 120.0) {
        Some(1)
    } else if l
        .altitude_geodetic
        .is_some_and(|a| !(-500.0..=9000.0).contains(&a))
    {
        Some(2)
    } else if l.height_type == HeightType::AboveGround && l.height.is_some_and(|h| h < -50.0) {
        Some(3)
    } else if let (Some(g), Some(b)) = (l.altitude_geodetic, l.altitude_pressure)
        && fabs(f64::from(g - b)) > 800.0
    {
        Some(4)
    } else {
        None
    };
    if let Some(kind) = bad_value
        && inc(&mut s.value_bad) >= 3
    {
        s.raise(Check::ImplausibleValue, 35, kind);
    }

    // Replay: an identical report (position and timestamp) heard again
    // seconds after it was first heard. A drone repeats a report between
    // its own updates (and every node hears it), but never 3 s later.
    if l.position.is_some() && l.timestamp.is_some() {
        let h = loc_fingerprint(l);
        let now32 = now as u32;
        match s.recent.iter().position(|&r| r == h) {
            Some(i) => {
                if now32.wrapping_sub(s.recent_ms[i]) > 3_000 {
                    let n = inc(&mut s.replays);
                    if n >= 3 {
                        s.raise(Check::Replay, if n >= 10 { 75 } else { 50 }, i32::from(n));
                    }
                    s.recent_ms[i] = now32;
                }
            }
            None => {
                let i = usize::from(s.recent_next) % s.recent.len();
                s.recent[i] = h;
                s.recent_ms[i] = now32;
                s.recent_next = ((i + 1) % s.recent.len()) as u8;
            }
        }
    }

    // Timestamps against the hub's UTC.
    if let (Some(ts), Some(off)) = (l.timestamp, env.utc_offset_ms) {
        let utc = now as i64 + off;
        let sph = utc.rem_euclid(3_600_000) as f64 / 1000.0;
        let d = hour_diff(f64::from(ts), sph);
        if fabs(d) > 30.0 {
            let n = inc(&mut s.skew_bad);
            if n >= 5 && s.skew_bad > s.skew_ok {
                s.raise(
                    Check::ClockSkew,
                    if fabs(d) > 300.0 { 40 } else { 30 },
                    d as i32,
                );
            }
        } else {
            inc(&mut s.skew_ok);
        }
    }

    let Some(prev) = prev else {
        anchor(s, l, now);
        rssi(s, l, obs, env);
        return;
    };
    let dt_ms = now.saturating_sub(prev.rx_ms);
    let dt = dt_ms as f64 / 1000.0;

    // Timestamps against reception time.
    if let (Some(nt), Some(ot)) = (l.timestamp, prev.value.timestamp)
        && dt_ms < 60_000
    {
        let d = hour_diff(f64::from(nt) - f64::from(ot), dt);
        if fabs(d) > 10.0 {
            let n = inc(&mut s.time_bad);
            if n >= 3 {
                s.raise(
                    Check::TimeInconsistent,
                    if n >= 8 { 70 } else { 45 },
                    d as i32,
                );
            }
        }
    }

    // Position jump.
    if let (Some(p1), Some(p0)) = (l.position, prev.value.position)
        && dt_ms <= 120_000
    {
        let hacc = [
            l.horizontal_accuracy.meters(),
            prev.value.horizontal_accuracy.meters(),
        ]
        .into_iter()
        .flatten()
        .map(f64::from)
        .reduce(f64::max)
        .unwrap_or(30.0)
        .min(150.0);
        let d = dist_m(p0, p1);
        // The report may be up to ~2 s old when it arrives.
        if d > 60.0 + 2.0 * hacc + MAX_DRONE_MPS * (dt + 2.0) {
            let n = inc(&mut s.jumps);
            let c = match n {
                1 => 25,
                2 => 50,
                3..=5 => 75,
                _ => 90,
            };
            s.raise(Check::PositionJump, c, d as i32);
        }
    }

    anchor(s, l, now);
    rssi(s, l, obs, env);
}

/// The kinematics window: every >= 5 s, compare how far the positions
/// moved with the speeds and directions reported meanwhile.
fn anchor(s: &mut SpoofState, l: &Location, now: u64) {
    let Some(p) = l.position else { return };
    let now32 = now as u32;
    let alt = |v: Option<f32>| v.map_or(i16::MIN, |a| a.clamp(-30_000.0, 30_000.0) as i16);
    let fresh = |l: &Location| Anchor {
        pos: e7(p),
        ms: now32,
        alt: alt(l.altitude_geodetic),
        height: alt(l.height),
        vmax: l.speed_horizontal.map_or(0, |v| (v * 10.0) as u16),
        dir: l.direction.map_or(0xFFFF, |d| d as u16),
        speeds: u8::from(l.speed_horizontal.is_some()),
        valid: true,
    };
    if !s.anchor.valid {
        s.anchor = fresh(l);
        return;
    }
    if let Some(v) = l.speed_horizontal {
        s.anchor.vmax = s.anchor.vmax.max((v * 10.0) as u16);
        s.anchor.speeds = s.anchor.speeds.saturating_add(1);
    }
    let a = s.anchor;
    let dt_ms = now32.wrapping_sub(a.ms);
    if dt_ms < 5_000 {
        return;
    }
    if dt_ms <= 30_000 {
        let dt = f64::from(dt_ms) / 1000.0;
        let p0 = from_e7(a.pos);
        let hacc = l
            .horizontal_accuracy
            .meters()
            .map_or(30.0, f64::from)
            .min(150.0);
        let d = (dist_m(p0, p) - 2.0 * hacc - 10.0).max(0.0);
        let v = d / dt;
        let vmax = f64::from(a.vmax) / 10.0;
        let mut bad = None;
        if a.speeds > 0 && v > 1.5 * vmax + 8.0 {
            bad = Some(v as i32);
        } else if v > 5.0
            && vmax > 5.0
            && a.dir != 0xFFFF
            && let Some(dir) = l.direction
        {
            // The chord of any turn under ~200 deg lies between the start
            // and end directions, so it must be near at least one.
            let chord = bearing_deg(p0, p);
            let off = angle_diff(chord, f64::from(a.dir)).min(angle_diff(chord, f64::from(dir)));
            if off > 100.0 {
                bad = Some(-(off as i32));
            }
        }
        match bad {
            Some(detail) => {
                let n = inc(&mut s.motion_bad);
                if n >= 2 && u16::from(n) * 3 > u16::from(s.motion_ok) {
                    s.raise(Check::MotionMismatch, if n >= 4 { 60 } else { 40 }, detail);
                }
            }
            None => drop(inc(&mut s.motion_ok)),
        }
        // Altitude change against height change (same reference, so a
        // fixed offset such as HAE vs MSL cancels out).
        let (g1, h1) = (alt(l.altitude_geodetic), alt(l.height));
        if a.alt != i16::MIN && a.height != i16::MIN && g1 != i16::MIN && h1 != i16::MIN {
            let dg = f64::from(g1) - f64::from(a.alt);
            let dh = f64::from(h1) - f64::from(a.height);
            let off = fabs(dg - dh);
            if off > 40.0 + 0.3 * fabs(dh) {
                let n = inc(&mut s.alt_bad);
                if n >= 2 {
                    s.raise(
                        Check::AltitudeMismatch,
                        if n >= 4 { 50 } else { 30 },
                        off as i32,
                    );
                }
            }
        }
    }
    s.anchor = fresh(l);
}

/// Received strength against the claimed distance.
fn rssi(s: &mut SpoofState, l: &Location, obs: &Observation, env: &Env) {
    let Some(p) = l.position else { return };
    rssi_at(s, p, obs.node, obs.rssi, env);
}

fn rssi_at(s: &mut SpoofState, p: LatLon, node: u8, rssi: i8, env: &Env) {
    let Some(d) = env.node_distance(node, p) else {
        return;
    };
    if d < RSSI_MIN_RANGE_M {
        return;
    }
    let excess = f64::from(rssi) - rssi_limit_dbm(d);
    if excess > 0.0 {
        let n = inc(&mut s.rssi_bad);
        s.rssi_excess = s.rssi_excess.max(excess.min(100.0) as u8);
        if n >= 3 && u16::from(n) * 4 >= u16::from(s.rssi_ok) {
            let x = s.rssi_excess;
            let c = if x < 10 {
                45
            } else if x < 20 {
                65
            } else {
                85
            };
            s.raise(Check::RssiTooStrong, c, i32::from(x));
        }
    } else {
        inc(&mut s.rssi_ok);
    }
}

/// Operator position checks.
fn system(
    s: &mut SpoofState,
    prev: Option<&Stamped<System>>,
    sys: &System,
    drone: Option<&Location>,
    now: u64,
) {
    let Some(op) = sys.operator_position else {
        return;
    };
    if let Some(prev) = prev
        && prev.value.operator_location_type == sys.operator_location_type
        && let Some(op0) = prev.value.operator_position
    {
        let dt_ms = now.saturating_sub(prev.rx_ms);
        let d = dist_m(op0, op);
        if dt_ms <= 120_000 && d > 100.0 + MAX_OPERATOR_MPS * (dt_ms as f64 / 1000.0 + 2.0) {
            let n = inc(&mut s.op_jump);
            if n >= 2 {
                s.raise(Check::OperatorJump, if n >= 4 { 55 } else { 35 }, d as i32);
            }
        }
    }
    if let Some(l) = drone
        && let Some(p) = l.position
    {
        let d = dist_m(op, p);
        if d < SAME_POS_M && l.speed_horizontal.is_some_and(|v| v > 2.0) && moving(l) {
            if inc(&mut s.op_same) >= 5 {
                s.raise(Check::OperatorOnDrone, 35, 0);
            }
        } else if d > 30_000.0 && inc(&mut s.op_far) >= 3 {
            s.raise(Check::OperatorFar, 30, (d / 1000.0) as i32);
        }
    }
}

fn auth(s: &mut SpoofState, a: &Auth) {
    s.auth_pages |= 1 << (a.page & 0x0F);
    let mut bad = matches!(a.auth_type, AuthType::Reserved(_));
    match a.page_zero {
        Some(z) => s.auth_last = z.last_page_index,
        None => bad |= s.auth_last != 0xFF && a.page > s.auth_last,
    }
    if bad {
        auth_bad(s, 1);
    }
}

fn auth_bad(s: &mut SpoofState, n: u8) {
    s.auth_bad = s.auth_bad.saturating_add(n);
    if s.auth_bad >= 2 {
        s.raise(Check::AuthMalformed, 30, i32::from(s.auth_bad));
    }
}

fn serial(s: &mut SpoofState, b: &BasicId) {
    if b.id_type != IdType::SerialNumber {
        return;
    }
    if let Err(fault) = cta2063_check(b.uas_id.as_bytes()) {
        // Weak: shipping drones (some Autel and older Parrot firmware) send
        // non-conforming serials; see SkySafe's survey in SPOOFING.md.
        s.raise(Check::SerialFormat, 20, fault as i32);
    }
}

// ------------------------------------------------------- tracker hooks --

/// Before a Remote ID frame is applied to `tracks[i]` (whose previous
/// state is still in place). `conflict`: another track, heard recently,
/// that last used this frame's address with a different ID.
pub(crate) fn before_frame(
    tracks: &mut [Track],
    i: usize,
    obs: &Observation,
    msgs: &[Message],
    conflict: Option<TrackId>,
    created: bool,
    env: &Env,
) {
    let now = obs.rx_ms;
    let slot = Slot::of(obs.transport, obs.channel);
    if created {
        let s = &mut tracks[i].spoof;
        s.birth_node = obs.node;
        s.birth_rssi = obs.rssi;
        s.birth_slot = slot.index() as u8;
        swarm_on_birth(tracks, i);
    }
    if let Some(other) = conflict {
        shared_address(tracks, i, other, obs);
    }
    crate::continuity::before_frame(tracks, i, now);

    let t = &mut tracks[i];
    let l = t.links[slot.index()];
    let s = &mut t.spoof;
    // The same ID from alternating addresses on one slot. A BLE address
    // rotates every few minutes: one switch, not a pattern.
    if l.heard() && l.mac != obs.mac && now.saturating_sub(l.last_seen_ms) < 2_000 {
        let n = inc(&mut s.mac_flips);
        if n >= 4 {
            s.raise(
                Check::AddressAlternation,
                if n >= 10 { 70 } else { 45 },
                i32::from(n),
            );
        }
    }
    // Frozen counter: same node, same slot, a later frame, same counter.
    // BLE 4 legacy keeps one counter per message type, so it is skipped.
    if obs.transport != Transport::BleLegacy
        && l.heard()
        && l.node == obs.node
        && (50..5_000).contains(&now.saturating_sub(l.last_seen_ms))
    {
        s.ctr_total = s.ctr_total.saturating_add(1);
        if l.counter == obs.counter {
            s.ctr_same = s.ctr_same.saturating_add(1);
        }
        if s.ctr_total >= 20 && u32::from(s.ctr_same) * 10 >= u32::from(s.ctr_total) * 9 {
            let pct = u32::from(s.ctr_same) * 100 / u32::from(s.ctr_total);
            s.raise(Check::FrozenCounter, 20, pct as i32);
        }
    }

    let prev_loc = t
        .location
        .as_ref()
        .filter(|_| t.location_src == Source::RemoteId);
    let prev_sys = t
        .system
        .as_ref()
        .filter(|_| t.system_src == Source::RemoteId);
    let frame_loc = msgs.iter().find_map(|m| match m {
        Message::Location(l) => Some(l),
        _ => None,
    });
    // The drone's position for the operator checks: this frame's, else a
    // Remote ID one under 2 s old.
    let drone = frame_loc.or_else(|| {
        prev_loc
            .filter(|p| now.saturating_sub(p.rx_ms) < 2_000)
            .map(|p| &p.value)
    });
    let s = &mut t.spoof;
    for m in msgs {
        match m {
            Message::BasicId(b) => serial(s, b),
            Message::Location(l) => location(s, prev_loc, l, obs, env),
            Message::System(sys) => system(s, prev_sys, sys, drone, now),
            Message::Auth(a) => auth(s, a),
            Message::SelfId(_) | Message::OperatorId(_) => {}
        }
    }

    // Who made the transmitter.
    let mac = &obs.mac;
    let ble_random = !wifi(obs.transport) && mac[0] & 0xC0 == 0xC0;
    if espressif_oui(mac) && !ble_random {
        s.raise(Check::DevBoard, 15, i32::from(wifi(obs.transport)));
    }
    if wifi(obs.transport) && mac[0] & 0x02 == 0 {
        let frame_serial = msgs.iter().find_map(|m| match m {
            Message::BasicId(b) if b.id_type == IdType::SerialNumber => Some(b.uas_id),
            _ => None,
        });
        let serial = frame_serial.or_else(|| {
            t.uas_ids
                .iter()
                .find(|u| u.id_type == IdType::SerialNumber)
                .map(|u| u.id)
        });
        if let Some(sn) = serial {
            let sn = sn.as_bytes();
            let s = &mut t.spoof;
            if is_dji_serial(sn) && !dji_oui(mac) {
                if espressif_oui(mac) {
                    s.raise(Check::VendorMismatch, 45, 2);
                } else {
                    s.raise(Check::VendorMismatch, 25, 1);
                }
            } else if !is_dji_serial(sn) && cta2063_check(sn).is_ok() && dji_oui(mac) {
                s.raise(Check::VendorMismatch, 30, 3);
            }
        }
    }
}

/// After a Remote ID frame was applied to `tracks[i]`.
pub(crate) fn after_frame(tracks: &mut [Track], i: usize, env: &Env, now: u64) {
    let t = &mut tracks[i];
    // Standard Remote ID sends Basic ID, Location and System at least
    // (14 CFR 89.305; a broadcast module 89.315). BLE 4 rotates through
    // them, so allow 15 s and 20 Location frames before judging.
    if t.spoof.loc_frames >= 20 && now.saturating_sub(t.first_seen_ms) >= 15_000 {
        if t.uas_ids.is_empty() && t.dji.serial.is_none() && t.dji.key_id.is_none() {
            t.spoof.raise(Check::IncompleteMessageSet, 20, 2);
        } else if t.system.is_none() && !t.dji.heard() {
            t.spoof.raise(Check::IncompleteMessageSet, 20, 1);
        }
    }
    swarm_geometry(tracks, i, env);
    crate::continuity::after_frame(tracks, i, now);
}

fn same_birth(a: &SpoofState, b: &SpoofState, a_ms: u64, b_ms: u64) -> bool {
    a.birth_node != 0xFF
        && a.birth_node == b.birth_node
        && a.birth_slot == b.birth_slot
        && (i16::from(a.birth_rssi) - i16::from(b.birth_rssi)).abs() <= SWARM_RSSI_DB
        && a_ms.abs_diff(b_ms) <= SWARM_WINDOW_MS
}

/// A new track: is it one of several born together, alike?
fn swarm_on_birth(tracks: &mut [Track], i: usize) {
    let (me, ms) = (&tracks[i].spoof, tracks[i].first_seen_ms);
    let n = tracks
        .iter()
        .filter(|t| same_birth(me, &t.spoof, ms, t.first_seen_ms))
        .count()
        .min(255) as u8;
    if n < SWARM_MIN {
        return;
    }
    let me = tracks[i].spoof.clone();
    for t in tracks.iter_mut() {
        if same_birth(&me, &t.spoof, ms, t.first_seen_ms) {
            t.spoof.swarm = t.spoof.swarm.max(n);
            // Light shows also power up together: weak until geometry says
            // the "drones" cannot share a signal strength.
            t.spoof.raise(
                Check::SpooferSwarm,
                if n >= 6 { 25 } else { 20 },
                i32::from(n),
            );
        }
    }
}

/// A swarm member whose claimed distances from the unit differ by 4x or
/// more from another member's (12 dB of path loss) although both were
/// born at the same signal strength on the same node.
fn swarm_geometry(tracks: &mut [Track], i: usize, env: &Env) {
    if tracks[i].spoof.swarm < SWARM_MIN {
        return;
    }
    let Some(own) = env.own else { return };
    let pos = |t: &Track| t.location.as_ref().and_then(|l| l.value.position);
    let Some(p) = pos(&tracks[i]) else { return };
    let d = dist_m(own, p).max(1.0);
    let (me, ms) = (&tracks[i].spoof, tracks[i].first_seen_ms);
    let spread = tracks
        .iter()
        .filter(|t| t.id != tracks[i].id && same_birth(me, &t.spoof, ms, t.first_seen_ms))
        .filter_map(|t| pos(t).map(|q| dist_m(own, q).max(1.0)))
        .any(|e| (e / d >= 4.0 || d / e >= 4.0) && e.max(d) >= 400.0);
    if spread {
        let n = tracks[i].spoof.swarm;
        tracks[i].spoof.raise(Check::SpooferSwarm, 65, i32::from(n));
    }
}

/// Two tracks with different IDs on one address, heard within
/// [`SHARED_MAC_WINDOW_MS`]. Two Autel drones (fixed MAC on every unit)
/// do this honestly, so two sharers stay weak unless the frames are the
/// same strength; three or more is a multi-ID transmitter.
fn shared_address(tracks: &mut [Track], i: usize, other: TrackId, obs: &Observation) {
    let Some(j) = tracks.iter().position(|t| t.id == other) else {
        return;
    };
    if j == i {
        return;
    }
    let slot = Slot::of(obs.transport, obs.channel).index();
    let o = tracks[j].links[slot];
    let alike = o.heard()
        && o.node == obs.node
        && obs.rx_ms.saturating_sub(o.last_seen_ms) < 2_000
        && (i16::from(o.rssi) - i16::from(obs.rssi)).abs() <= 2;
    let now = obs.rx_ms;
    let sharers = tracks
        .iter()
        .filter(|t| {
            now.saturating_sub(t.last_seen_ms) < SHARED_MAC_WINDOW_MS
                && t.slots()
                    .any(|(s, l)| wifi(s.transport) == wifi(obs.transport) && l.mac == obs.mac)
        })
        .count()
        .clamp(2, 255) as u8;
    for k in [i, j] {
        let s = &mut tracks[k].spoof;
        let n = inc(&mut s.mac_conflicts);
        if alike {
            inc(&mut s.mac_conflicts_alike);
        }
        if n < 3 {
            continue;
        }
        let alike_most = s.mac_conflicts >= 5
            && u16::from(s.mac_conflicts_alike) * 5 >= u16::from(s.mac_conflicts) * 4;
        let c = match sharers {
            2 if alike_most => 45,
            2 => 25,
            3 | 4 => 55,
            _ => 80,
        };
        s.raise(Check::SharedAddress, c, i32::from(sharers));
    }
}

/// A plaintext DroneID position (before it is applied): compare with a
/// fresh Remote ID position, and check its signal strength.
pub(crate) fn droneid_position(t: &mut Track, p: LatLon, node: u8, rssi: i8, env: &Env, now: u64) {
    if let Some(l) = t
        .location
        .as_ref()
        .filter(|l| t.location_src == Source::RemoteId && now.saturating_sub(l.rx_ms) < 3_000)
        && let Some(q) = l.value.position
    {
        let dt = now.saturating_sub(l.rx_ms) as f64 / 1000.0;
        let hacc = l
            .value
            .horizontal_accuracy
            .meters()
            .map_or(30.0, f64::from)
            .min(150.0);
        let d = dist_m(p, q);
        let s = &mut t.spoof;
        if d > 100.0 + 2.0 * hacc + 30.0 * dt {
            let n = inc(&mut s.droneid_bad);
            if n >= 2 && s.droneid_bad > s.droneid_ok {
                s.raise(
                    Check::DroneIdDisagrees,
                    if n >= 5 { 75 } else { 55 },
                    d as i32,
                );
            }
        } else {
            inc(&mut s.droneid_ok);
        }
    }
    rssi_at(&mut t.spoof, p, node, rssi, env);
}

/// A DJI legacy Wi-Fi DroneID element from `mac`.
pub(crate) fn dji_wifi_frame(t: &mut Track, mac: &[u8; 6]) {
    if mac[0] & 0x02 != 0 || dji_oui(mac) {
        return;
    }
    if espressif_oui(mac) {
        t.spoof.raise(Check::VendorMismatch, 45, 4);
    } else {
        t.spoof.raise(Check::VendorMismatch, 20, 5);
    }
}

/// Authentication messages that did not decode (page-0 lengths that
/// contradict each other).
pub(crate) fn auth_decode_errors(t: &mut Track, n: u8) {
    if n > 0 {
        auth_bad(&mut t.spoof, n);
    }
}

// ----------------------------------------------------------- reporting --

/// One flag for the hub's evidence log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpoofEvent {
    pub track: TrackId,
    pub check: Check,
    pub confidence: u8,
    pub detail: i32,
    /// The track's combined score when this was handed out.
    pub score: u8,
}

impl SpoofEvent {
    /// Evidence `SpoofSuspect` data: check code, confidence.
    pub fn data(&self) -> [u8; 2] {
        [self.check.code(), self.confidence]
    }

    pub fn reason(&self) -> Reason {
        Reason {
            check: self.check,
            detail: self.detail,
        }
    }
}

/// Evidence text for `ev`: the track, its broadcast ID, the reason and
/// the numbers, e.g. `track 7 SN 1581F...: position jumped 2400 m, ...
/// (confidence 75, score 80)`.
pub fn event_text(ev: &SpoofEvent, track: Option<&Track>) -> heapless::String<192> {
    crate::text::render(EventText(ev, track))
}

struct EventText<'a>(&'a SpoofEvent, Option<&'a Track>);

impl fmt::Display for EventText<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ev = self.0;
        write!(f, "track {}", ev.track.0)?;
        if let Some(id) = self.1.and_then(Track::primary_id) {
            write!(f, " {:?} {}", id.id_type, crate::text::IdText(&id.id))?;
        }
        write!(
            f,
            ": {} (confidence {}, score {})",
            ev.reason(),
            ev.confidence,
            ev.score
        )
    }
}

/// One-line text summary of a track's suspicion and continuity, empty
/// when there is nothing to say: `possible spoof 80%: <strongest reason>
/// (+2 more); was track 12, new identity mid-flight`.
pub struct Summary<'a>(pub &'a Track);

impl fmt::Display for Summary<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let t = self.0;
        let score = t.spoof.score();
        let mut wrote = false;
        if score >= REPORT_MIN
            && let Some(top) = t.spoof.strongest()
        {
            write!(f, "possible spoof {score}%: {}", top.reason())?;
            let more = t.spoof.flags().len() - 1;
            if more > 0 {
                write!(f, " (+{more} more)")?;
            }
            wrote = true;
        }
        if let Some(prev) = t.continuity.previous {
            if wrote {
                f.write_str("; ")?;
            }
            write!(f, "probably the same drone as track {}", prev.0)?;
            if t.continuity.identity_changed {
                f.write_str(", new identity mid-flight")?;
            } else {
                f.write_str(", new identity")?;
            }
        }
        Ok(())
    }
}

impl<const N: usize, const A: usize> Tracker<N, A> {
    /// Tell the checks the current UTC (Unix ms) at hub time `now_ms`, for
    /// the clock check. Call whenever GPS time is known; the offset is kept.
    pub fn set_utc(&mut self, utc_ms: u64, now_ms: u64) {
        self.spoof_cfg.utc_offset_ms = Some(utc_ms as i64 - now_ms as i64);
    }

    /// Forget UTC (the clock check is skipped).
    pub fn clear_utc(&mut self) {
        self.spoof_cfg.utc_offset_ms = None;
    }

    /// Where node `node` (0-7) is, for the signal-strength check. Without
    /// it a node is assumed within [`NODE_SPREAD_M`] of the hub.
    pub fn set_node_position(&mut self, node: u8, lat: f64, lon: f64) {
        if let Some(n) = self.spoof_cfg.nodes.get_mut(usize::from(node)) {
            *n = Some(e7(LatLon { lat, lon }));
        }
    }

    pub fn clear_node_position(&mut self, node: u8) {
        if let Some(n) = self.spoof_cfg.nodes.get_mut(usize::from(node)) {
            *n = None;
        }
    }

    pub(crate) fn spoof_env(&self, now_ms: u64) -> Env {
        Env {
            own: self.own_position(now_ms).map(|o| LatLon {
                lat: o.lat,
                lon: o.lon,
            }),
            utc_offset_ms: self.spoof_cfg.utc_offset_ms,
            nodes: self.spoof_cfg.nodes,
        }
    }

    /// The next flag to log, once per flag when it reaches [`REPORT_MIN`]
    /// and again each time it rises by [`REREPORT_STEP`]. Also runs the
    /// checks that look across tables. Call after ingesting, and ~1 Hz.
    pub fn take_spoof_flag(&mut self, now_ms: u64) -> Option<SpoofEvent> {
        self.no_dji_rf(now_ms);
        for t in self.tracks.iter_mut() {
            let score = t.spoof.score();
            if let Some(f) = t.spoof.flags.iter_mut().find(|f| {
                f.confidence >= REPORT_MIN
                    && (f.reported == 0 || f.confidence >= f.reported.saturating_add(REREPORT_STEP))
            }) {
                f.reported = f.confidence;
                return Some(SpoofEvent {
                    track: t.id,
                    check: f.check,
                    confidence: f.confidence,
                    detail: f.detail,
                    score,
                });
            }
        }
        None
    }

    /// Weak corroboration: a DJI drone flying for two minutes while a
    /// raw-capture node is running, and no DJI video or control link in the
    /// emitter table. The node may simply be scanning another band.
    fn no_dji_rf(&mut self, now_ms: u64) {
        use link::{NodeRole, RfKind};
        let scanner = self.nodes().iter().any(|n| {
            matches!(n.role, Some(NodeRole::Fpv) | Some(NodeRole::DroneId))
                && now_ms.saturating_sub(n.last_ms) < 10_000
        });
        if !scanner {
            return;
        }
        let dji_rf = self.emitters().iter().any(|e| {
            matches!(
                e.kind,
                RfKind::DjiVideo
                    | RfKind::DjiO4Video
                    | RfKind::DjiFpvAirUnit
                    | RfKind::DjiController
            )
        });
        if dji_rf {
            return;
        }
        for t in self.tracks.iter_mut() {
            let dji = t.dji.heard()
                || t.uas_ids
                    .iter()
                    .any(|u| u.id_type == IdType::SerialNumber && is_dji_serial(u.id.as_bytes()));
            let flying = t.location.as_ref().is_some_and(|l| moving(&l.value));
            if dji
                && flying
                && now_ms.saturating_sub(t.first_seen_ms) >= 120_000
                && now_ms.saturating_sub(t.last_seen_ms) < 10_000
            {
                t.spoof.raise(Check::NoDjiRf, 15, 0);
            }
        }
    }
}

/// Espressif Inc. MA-L blocks (24-bit OUIs), sorted. From the IEEE
/// registry as listed by maclookup.app/vendors/espressif-inc (fetched
/// 2026-11-17). Used only for the weak [`Check::DevBoard`] and as a
/// strengthener of [`Check::VendorMismatch`].
static ESPRESSIF_OUIS: [u32; 355] = [
    0x004B12, 0x007007, 0x00FB4A, 0x048308, 0x04B247, 0x083A8D, 0x083AF2, 0x089272, 0x08A6F7,
    0x08AD0A, 0x08B61F, 0x08D1F9, 0x08F9E0, 0x0C4EA0, 0x0C8B95, 0x0CB815, 0x0CDC7E, 0x10003B,
    0x10061C, 0x1020BA, 0x1051DB, 0x10521C, 0x1091A8, 0x1097BD, 0x10B41D, 0x10BDA3, 0x140808,
    0x142B2F, 0x14335C, 0x146393, 0x14C19F, 0x188B0E, 0x18FE34, 0x1C2904, 0x1C6465, 0x1C6920,
    0x1C8F57, 0x1C9DC2, 0x1CC3AB, 0x1CDBD4, 0x202565, 0x2043A8, 0x20500D, 0x206EF1, 0x209BA9,
    0x20E7C8, 0x240AC4, 0x244CAB, 0x24587C, 0x2462AB, 0x246F28, 0x24A160, 0x24B2DE, 0x24D7EB,
    0x24DCC3, 0x24EC4A, 0x2805A5, 0x28372F, 0x28562F, 0x288485, 0x2C3AE8, 0x2CBCBB, 0x2CF432,
    0x3030F9, 0x3076F5, 0x308398, 0x30AEA4, 0x30C6F7, 0x30C922, 0x30EDA0, 0x345F45, 0x348518,
    0x34865D, 0x349454, 0x34987A, 0x34AB95, 0x34B472, 0x34B7DA, 0x34C934, 0x34CDB0, 0x38182B,
    0x383E51, 0x3844BE, 0x3C0F02, 0x3C6105, 0x3C71BF, 0x3C8427, 0x3C8A1F, 0x3CDC75, 0x3CE255,
    0x3CE90E, 0x4022D8, 0x402BD6, 0x404CCA, 0x409151, 0x40F520, 0x441793, 0x441BF6, 0x441D64,
    0x447B30, 0x44B176, 0x44BD8D, 0x4827E2, 0x4831B7, 0x483FDA, 0x485519, 0x489D31, 0x48AFF3,
    0x48CA43, 0x48E729, 0x48F6EE, 0x4C11AE, 0x4C7525, 0x4C8421, 0x4CC382, 0x4CEBD6, 0x500291,
    0x50787D, 0x543204, 0x5443B2, 0x545AA6, 0x549DEA, 0x582ABD, 0x582BD3, 0x588C81, 0x58BF25,
    0x58CF79, 0x58E6C5, 0x5C013B, 0x5CCF7F, 0x600194, 0x6055F9, 0x648914, 0x64B708, 0x64E833,
    0x680947, 0x6825DD, 0x686725, 0x689DD2, 0x68B6B3, 0x68C63A, 0x68EE8F, 0x68FE71, 0x6C2FA0,
    0x6CB456, 0x6CC840, 0x70039F, 0x70041D, 0x704BCA, 0x70AF09, 0x70B8F6, 0x744DBD, 0x748927,
    0x781C3C, 0x782184, 0x78421C, 0x78E36D, 0x78EE4C, 0x7C0C5F, 0x7C2C67, 0x7C4FAD, 0x7C7398,
    0x7C87CE, 0x7C9EBD, 0x7CD544, 0x7CDFA1, 0x7CE8B1, 0x80456B, 0x8053E0, 0x80646F, 0x806599,
    0x807D3A, 0x80B54E, 0x80F1B2, 0x80F3DA, 0x840D8E, 0x841FE8, 0x84C7BB, 0x84CCA8, 0x84F3EB,
    0x84F703, 0x84FCE6, 0x8813BF, 0x8856A6, 0x885721, 0x88F155, 0x8C4598, 0x8C4B14, 0x8C4F00,
    0x8C8C29, 0x8C94DF, 0x8CAAB5, 0x8CBFEA, 0x8CCE4E, 0x8CFD49, 0x901506, 0x90380C, 0x90649B,
    0x907069, 0x9097D5, 0x90B339, 0x90DA72, 0x90E5B1, 0x943CC6, 0x9451DC, 0x9454C5, 0x94A990,
    0x94B555, 0x94B97E, 0x94E686, 0x983DAE, 0x9888E0, 0x98A316, 0x98C377, 0x98CDAC, 0x98F4AB,
    0x9C139E, 0x9C9C1F, 0x9C9E6E, 0x9CCC01, 0xA020A6, 0xA0764E, 0xA085E3, 0xA0A3B3, 0xA0B765,
    0xA0DD6C, 0xA0F262, 0xA47B9D, 0xA4CB8F, 0xA4CF12, 0xA4E57C, 0xA4E972, 0xA4F00F, 0xA8032A,
    0xA842E3, 0xA84674, 0xA848FA, 0xA8FD07, 0xAC0BFB, 0xAC1518, 0xAC276E, 0xAC67B2, 0xACA704,
    0xACD074, 0xACEBE6, 0xB03FD3, 0xB08184, 0xB0A604, 0xB0A732, 0xB0B21C, 0xB0CBD8, 0xB43A45,
    0xB48A0A, 0xB4A64A, 0xB4BFE9, 0xB4E62D, 0xB81F3F, 0xB87B4D, 0xB8D61A, 0xB8F009, 0xB8F862,
    0xBC1926, 0xBCDDC2, 0xBCFF4D, 0xC049EF, 0xC04E30, 0xC05D89, 0xC0CDD6, 0xC44F33, 0xC45BBE,
    0xC49E7E, 0xC4D8D5, 0xC4DD57, 0xC4DEE2, 0xC82B96, 0xC82E18, 0xC88541, 0xC88A7B, 0xC8C9A3,
    0xC8EA71, 0xC8ED8B, 0xC8F09E, 0xCC50E3, 0xCC68C7, 0xCC7B5C, 0xCC7E1F, 0xCC8DA2, 0xCCBA97,
    0xCCDBA7, 0xCCE732, 0xD0CF13, 0xD0EF76, 0xD40592, 0xD48AFC, 0xD48C49, 0xD4D4DA, 0xD4E9F4,
    0xD4F98D, 0xD8132A, 0xD83BDA, 0xD885AC, 0xD8A01D, 0xD8BC38, 0xD8BFC0, 0xD8F15B, 0xDC0675,
    0xDC0A69, 0xDC1ED5, 0xDC4F22, 0xDC5475, 0xDC55B1, 0xDCB4D9, 0xDCDA0C, 0xE05A1B, 0xE072A1,
    0xE08CFE, 0xE09806, 0xE0E2E6, 0xE465B8, 0xE4B063, 0xE4B323, 0xE80690, 0xE831CD, 0xE83DC1,
    0xE868E7, 0xE86BEA, 0xE89F6D, 0xE8DB84, 0xE8EDBD, 0xE8F60A, 0xEC6260, 0xEC64C9, 0xEC94CB,
    0xECC9FF, 0xECD61B, 0xECDA3B, 0xECE334, 0xECFABC, 0xF008D1, 0xF0161D, 0xF024F9, 0xF09E9E,
    0xF0F5BD, 0xF412FA, 0xF42DC9, 0xF4650B, 0xF4CFA2, 0xF80C4E, 0xF843EE, 0xF85B1B, 0xF8B3B7,
    0xFC012C, 0xFCB467, 0xFCE8C0, 0xFCF5C4,
];
