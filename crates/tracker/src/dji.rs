//! DJI's own broadcasts, folded into drone tracks.
//!
//! - **OcuSync DroneID** ([`link::DroneIdSighting`]), decoded on a
//!   raw-capture node. Raw units as `crates/droneid` documents them
//!   (`payload::FlightInfo`): positions in radians x 1e7, altitude and
//!   height in feet (the NDSS receiver's reading), velocities in cm/s, yaw
//!   in hundredths of a degree. Scaled here exactly as `droneid` does.
//! - **Legacy Wi-Fi DroneID**, the vendor element `rid::dji` decodes
//!   (Spark, Mavic Air, Mini SE...). Its extension fields are "believed"
//!   metres and cm/s (see `rid::dji::FlightInfoExt`).
//!
//! # Identity
//! A plaintext record carries the aircraft serial number. DJI aircraft use
//! the same serial in their Remote ID Basic ID, so the serial is stored as
//! a [`crate::UasId`] of type `SerialNumber` and the ordinary Remote ID
//! merge puts both on one track, whichever arrives first. With
//! [`crate::Config::hide_droneid_ids`] the serial is kept only in
//! [`DjiInfo::serial`] (never output) and still matches.
//!
//! An O4 record is encrypted: only its 4-byte session key ID is readable.
//! It links the records of one flight (CRYP and INFP alike), so it keys a
//! presence-only track, labelled `DJI O4 <key id>`. Which record types were
//! heard gives a coarse state ([`O4State`]): on the one model studied (DJI
//! Mini 5 Pro, EdwardBlair/dji-droneid-spec) CRYP packets start after a GNSS
//! fix and INFP packets after takeoff. Other models may differ, so the
//! state is always shown as model-dependent. A
//! burst that matched DroneID but did not decode carries nothing at all, so
//! every such burst joins one shared presence-only track.
//!
//! # Positions
//! A drone heard on both Remote ID and DroneID sends its position twice.
//! Remote ID's is preferred (it has a WGS-84 altitude and accuracy
//! figures): a DroneID position replaces it only when it is older than
//! [`RID_PREFERRED_MS`]. [`crate::Track::location_src`] says which one is
//! shown. DJI altitude is barometric, so it goes into `altitude_pressure`,
//! never `altitude_geodetic`.
//!
//! # Trust
//! Plaintext DroneID, decrypted O4 and Remote ID are separate claims about
//! one serial, none authenticated ([`crate::claims`]). A DJI position is
//! judged against the track's established path like any report: one that
//! fits agrees (it may be shown when Remote ID is quiet); one that does not
//! is a rival claim, never applied, and a dispute if both continue. The
//! serial in a DJI record finds the track that has it; it never merges two
//! tracks on its own, and a decrypted O4 session joins the serial's track
//! only after [`crate::claims::MERGE_MIN_FRAMES`] records with compatible
//! positions. "Decrypted" is not "authentic": a recording can be replayed
//! and the position cipher is malleable (`djicrypt` refuses sequence and
//! GPS-time regressions within a session; the rest is the dispute rule).

use libm::{atan2, hypot};
use droneid::o4plain::Infp10;
use link::{DroneIdRecord, DroneIdSighting};
use rid::message::{
    Category, ClassificationType, DescType, FixedStr, HeightType, HorizontalAccuracy, IdType,
    LatLon, Location, OperatorLocationType, SelfId, SpeedAccuracy, Status, System, UaClass, UaType,
    VerticalAccuracy,
};

use crate::claims::{self, ClaimSrc, Outcome, Verdict};
use crate::{Stamped, Track, TrackId, Tracker, UasId};

/// A Remote ID position younger than this is not replaced by a DroneID one.
pub const RID_PREFERRED_MS: u64 = 3_000;

/// Where a track's position (or operator position) came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
pub enum Source {
    #[default]
    RemoteId,
    /// OcuSync DroneID burst.
    DroneId,
    /// DJI legacy Wi-Fi DroneID element.
    DjiWifi,
    /// Protocol 12: telemetry decoded from a drone control link (ELRS,
    /// mLRS, SiK), with the owner's content opt-in (`ctrllinks`).
    ControlLink,
}

/// Reception of one DJI broadcast family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DjiLink {
    /// Records heard. Zero means never.
    pub count: u32,
    pub last_seen_ms: u64,
    pub rssi: i8,
    /// OcuSync only: signal to noise of the latest burst, dB.
    pub snr_db: i8,
    /// OcuSync only: burst centre, 100 kHz units (5756.5 MHz = 57565).
    pub freq: u32,
    /// Wi-Fi only: channel and transmitter address.
    pub channel: u8,
    pub mac: [u8; 6],
    pub node: u8,
}

impl DjiLink {
    pub fn heard(&self) -> bool {
        self.count > 0
    }

    fn newer(&mut self, other: &DjiLink) {
        let count = self.count.saturating_add(other.count);
        if other.heard() && (!self.heard() || other.last_seen_ms > self.last_seen_ms) {
            *self = *other;
        }
        self.count = count;
    }
}

/// What DJI's own broadcasts said about a track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DjiInfo {
    /// The decoded serial number, kept for matching even when
    /// [`crate::Config::hide_droneid_ids`] keeps it out of `uas_ids`.
    /// **Never output from here**: formatters show `uas_ids` only.
    pub serial: Option<FixedStr<20>>,
    /// DJI product code (`droneid::payload::product_name`).
    pub product_type: Option<u8>,
    /// O4 session key ID. Formatters hide it with `hide_droneid_ids`.
    pub key_id: Option<[u8; 4]>,
    /// The shared track for DroneID bursts that did not decode.
    pub undecoded: bool,
    /// O4 record types heard on this track.
    pub o4: O4Seen,
    /// O4 records on this track were decrypted with the owner's key (the
    /// hub's `djicrypt` layer): its serial and positions are real, not
    /// just the session ID.
    pub o4_decrypted: bool,
    pub ocusync: DjiLink,
    pub wifi: DjiLink,
}

/// Counts of each O4 record type heard (protocol 8 nodes report the type;
/// older nodes' `Encrypted` records count in none of them).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct O4Seen {
    pub cryp: u32,
    pub infp10: u32,
    pub infp11: u32,
    pub last_cryp_ms: u64,
    pub last_infp_ms: u64,
}

/// What the O4 record types suggest. **Model-dependent**: observed on a
/// DJI Mini 5 Pro only (CRYP after GNSS fix, INFP after takeoff).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum O4State {
    /// Only CRYP records: GPS locked, maybe still on the ground.
    GpsLocked,
    /// INFP records: probably flying.
    ProbablyFlying,
}

impl O4State {
    /// Text for screens and logs; says it is a model-dependent guess.
    pub fn label(self) -> &'static str {
        match self {
            O4State::GpsLocked => "GPS locked (CRYP seen; model-dependent)",
            O4State::ProbablyFlying => "probably flying (INFP seen; model-dependent)",
        }
    }
    /// A short form for small screens.
    pub fn short(self) -> &'static str {
        match self {
            O4State::GpsLocked => "GPS lock?",
            O4State::ProbablyFlying => "flying?",
        }
    }
}

impl O4Seen {
    pub fn heard(&self) -> bool {
        self.cryp > 0 || self.infp10 > 0 || self.infp11 > 0
    }
    pub fn state(&self) -> Option<O4State> {
        if self.infp10 > 0 || self.infp11 > 0 {
            Some(O4State::ProbablyFlying)
        } else if self.cryp > 0 {
            Some(O4State::GpsLocked)
        } else {
            None
        }
    }
    fn count(&mut self, kind: link::O4Kind, now: u64) {
        match kind {
            link::O4Kind::Cryp => {
                self.cryp = self.cryp.saturating_add(1);
                self.last_cryp_ms = now;
            }
            link::O4Kind::Infp10 => {
                self.infp10 = self.infp10.saturating_add(1);
                self.last_infp_ms = now;
            }
            link::O4Kind::Infp11 => {
                self.infp11 = self.infp11.saturating_add(1);
                self.last_infp_ms = now;
            }
        }
    }
    fn absorb(&mut self, o: &O4Seen) {
        self.cryp = self.cryp.saturating_add(o.cryp);
        self.infp10 = self.infp10.saturating_add(o.infp10);
        self.infp11 = self.infp11.saturating_add(o.infp11);
        self.last_cryp_ms = self.last_cryp_ms.max(o.last_cryp_ms);
        self.last_infp_ms = self.last_infp_ms.max(o.last_infp_ms);
    }
}

impl DjiInfo {
    /// Any DJI broadcast heard.
    pub fn heard(&self) -> bool {
        self.ocusync.heard() || self.wifi.heard()
    }

    /// The model name for [`DjiInfo::product_type`], when known.
    pub fn product_name(&self) -> Option<&'static str> {
        self.product_type.and_then(droneid::payload::product_name)
    }

    /// Burst frequency in MHz, when an OcuSync burst was heard.
    pub fn freq_mhz(&self) -> Option<f64> {
        self.ocusync
            .heard()
            .then(|| f64::from(self.ocusync.freq) / 10.0)
    }

    pub(crate) fn absorb(&mut self, other: &DjiInfo) {
        self.serial = self.serial.or(other.serial);
        self.product_type = self.product_type.or(other.product_type);
        self.key_id = self.key_id.or(other.key_id);
        self.undecoded &= other.undecoded;
        self.o4.absorb(&other.o4);
        self.o4_decrypted |= other.o4_decrypted;
        self.ocusync.newer(&other.ocusync);
        self.wifi.newer(&other.wifi);
    }
}

/// Feet per metre, as `droneid::payload` uses it.
const FEET_PER_M: f64 = 3.281;

/// DJI coordinates (radians x 1e7) to a position; `None` for 0,0 (privacy
/// mode or no fix) or out of range. Same factor as
/// `droneid::payload::rad_e7_to_deg`.
pub fn rad_e7_position(lat_raw: i32, lon_raw: i32) -> Option<LatLon> {
    if lat_raw == 0 && lon_raw == 0 {
        return None;
    }
    let p = LatLon {
        lat: droneid::payload::rad_e7_to_deg(lat_raw),
        lon: droneid::payload::rad_e7_to_deg(lon_raw),
    };
    ((-90.0..=90.0).contains(&p.lat) && (-180.0..=180.0).contains(&p.lon)).then_some(p)
}

/// Ground speed (m/s), track (deg true) and vertical speed (m/s) from DJI's
/// north/east/up velocities in cm/s. No track below 0.3 m/s: the direction
/// of a hovering drone is noise.
fn motion(v_north: i16, v_east: i16, v_up: i16) -> (f32, Option<f32>, f32) {
    let (n, e) = (f64::from(v_north) / 100.0, f64::from(v_east) / 100.0);
    let speed = hypot(n, e);
    let track = (speed >= 0.3).then(|| {
        let d = atan2(e, n).to_degrees();
        (if d < 0.0 { d + 360.0 } else { d }) as f32 % 360.0
    });
    (speed as f32, track, (f64::from(v_up) / 100.0) as f32)
}

/// A serial as an ID: printable ASCII, NULs trimmed, not empty.
fn serial_id(raw: &[u8]) -> Option<FixedStr<20>> {
    let f: FixedStr<20> = FixedStr::new(raw.get(..raw.len().min(20))?).ok()?;
    (f.is_clean_text() && !f.as_bytes().is_empty()).then_some(f)
}

/// The parts of one DJI record that land on a track.
struct Fix {
    position: Option<LatLon>,
    alt_m: Option<f32>,
    height_m: Option<f32>,
    motion: Option<(f32, Option<f32>, f32)>,
    pilot: Option<LatLon>,
    home: Option<LatLon>,
}

fn location(f: &Fix) -> Location {
    let (speed, track, vspeed) = match f.motion {
        Some((s, t, v)) => (Some(s), t, Some(v)),
        None => (None, None, None),
    };
    Location {
        status: Status::Undeclared,
        height_type: HeightType::AboveTakeoff,
        direction: track,
        speed_horizontal: speed,
        speed_vertical: vspeed,
        position: f.position,
        altitude_pressure: f.alt_m,
        altitude_geodetic: None,
        height: f.height_m,
        horizontal_accuracy: HorizontalAccuracy::Unknown,
        vertical_accuracy: VerticalAccuracy::Unknown,
        baro_accuracy: VerticalAccuracy::Unknown,
        speed_accuracy: SpeedAccuracy::Unknown,
        timestamp: None,
        timestamp_accuracy: None,
    }
}

fn pilot_system(p: LatLon) -> System {
    System {
        // DJI sends the phone's (the app's) GNSS position.
        operator_location_type: OperatorLocationType::LiveGnss,
        classification_type: ClassificationType::Undeclared,
        operator_position: Some(p),
        area_count: 1,
        area_radius: 0,
        area_ceiling: None,
        area_floor: None,
        category: Category::Undeclared,
        class: UaClass::Undeclared,
        operator_altitude: None,
        timestamp: None,
    }
}

impl Track {
    /// Apply a DJI position, unless a fresh Remote ID one should win.
    fn apply_dji(&mut self, f: &Fix, src: crate::Source, now: u64) {
        let rid_fresh = |src_was: crate::Source, rx: Option<u64>| {
            src_was == crate::Source::RemoteId
                && rx.is_some_and(|t| now.saturating_sub(t) < RID_PREFERRED_MS)
        };
        if (f.position.is_some() || f.alt_m.is_some())
            && !rid_fresh(self.location_src, self.location.as_ref().map(|l| l.rx_ms))
        {
            self.location = Some(Stamped {
                value: location(f),
                rx_ms: now,
            });
            self.location_src = src;
        }
        if let Some(p) = f.pilot
            && !rid_fresh(self.system_src, self.system.as_ref().map(|s| s.rx_ms))
        {
            self.system = Some(Stamped {
                value: pilot_system(p),
                rx_ms: now,
            });
            self.system_src = src;
        }
        if let Some(h) = f.home {
            self.home = Some(Stamped {
                value: h,
                rx_ms: now,
            });
        }
        if self.ua_type.is_none_or(|t| t == UaType::None) {
            // Every DJI DroneID product is a multirotor.
            self.ua_type = Some(UaType::HelicopterOrMultirotor);
        }
    }

    fn observe_dji(link: &mut DjiLink, node: u8, now: u64) {
        link.count = link.count.saturating_add(1);
        link.last_seen_ms = now;
        link.node = node;
    }
}

impl<const N: usize, const A: usize> Tracker<N, A> {
    /// The track for a DJI serial: the most established one that has it
    /// (as a Remote ID or DJI ID), else a new one if it is admitted
    /// ([`crate::probation`]). One record never merges tracks.
    fn dji_track_by_serial(&mut self, node: u8, serial: FixedStr<20>, now: u64) -> Option<usize> {
        let id = UasId {
            id_type: IdType::SerialNumber,
            id: serial,
        };
        let i = match self.serial_track(&id, None) {
            Some(p) => p,
            None => self.admit(crate::probation::hash(&[b"dji-sn", serial.as_bytes()]), node, now)?,
        };
        let t = &mut self.tracks[i];
        t.dji.serial = Some(serial);
        if !self.config.hide_droneid_ids && !t.uas_ids.contains(&id) {
            t.add_id(id);
        }
        Some(i)
    }

    /// The most established track holding `id`, other than `not`.
    fn serial_track(&self, id: &UasId, not: Option<TrackId>) -> Option<usize> {
        self.tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| Some(t.id) != not && t.has_id(id))
            .min_by_key(|(_, t)| (t.first_seen_ms, t.id))
            .map(|(i, _)| i)
    }

    /// A DJI position for `tracks[i]` from `src`: applied if it fits the
    /// track's path (and Remote ID is not fresher), else held as a rival
    /// claim ([`crate::claims`]). `plain`: node and RSSI of a plaintext
    /// OcuSync record, for the DroneID checks. Returns whether it was
    /// applied.
    #[allow(clippy::too_many_arguments)]
    fn dji_fix(
        &mut self,
        i: usize,
        f: &Fix,
        src: crate::Source,
        claim: ClaimSrc,
        mac: [u8; 6],
        plain: Option<(u8, i8)>,
        now: u64,
    ) -> bool {
        let env = self.spoof_env(now);
        let t = &mut self.tracks[i];
        if let Some(p) = f.position {
            if claims::judge(t, Some(p), None, now) == Verdict::Rival {
                match t.rival_claim(p, None, claim, mac, now) {
                    Outcome::Held => return false,
                    Outcome::Follow => t.follow(p, now),
                }
            } else {
                t.claims.original_report(now);
                if let Some((node, rssi)) = plain {
                    crate::spoof::droneid_position(t, p, node, rssi, &env, now);
                }
            }
        }
        t.apply_dji(f, src, now);
        true
    }

    /// The most recently heard track that `f` picks, else a new one for
    /// identity `key` if it is admitted ([`crate::probation`]).
    fn dji_track_where(&mut self, node: u8, now: u64, key: &[&[u8]], f: impl Fn(&Track) -> bool) -> Option<usize> {
        match self
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| f(t))
            .max_by_key(|(_, t)| t.last_seen_ms)
        {
            Some((i, _)) => Some(i),
            None => self.admit(crate::probation::hash(key), node, now),
        }
    }

    /// Feed one OcuSync DroneID sighting from `node` (see [`crate::dji`]).
    /// Returns the track it updated; `None` when it is a new drone the
    /// table could not admit yet ([`crate::probation`]: busy table, not
    /// heard often enough).
    pub fn ingest_droneid(&mut self, node: u8, s: &DroneIdSighting, now_ms: u64) -> Option<TrackId> {
        self.expire(now_ms);
        let i = match &s.record {
            DroneIdRecord::Flight { serial, .. } => match serial_id(serial) {
                Some(sn) => self.dji_track_by_serial(node, sn, now_ms)?,
                // A record with an unreadable serial: no identity to key on.
                None => self.dji_track_where(node, now_ms, &[b"dji-undecoded"], |t| t.dji.undecoded)?,
            },
            DroneIdRecord::Encrypted { key_id } | DroneIdRecord::O4 { key_id, .. } => {
                let k = *key_id;
                let i = self.dji_track_where(node, now_ms, &[b"dji-o4", &k], |t| t.dji.key_id == Some(k))?;
                self.tracks[i].dji.key_id = Some(k);
                if let DroneIdRecord::O4 { kind, .. } = &s.record {
                    self.tracks[i].dji.o4.count(*kind, now_ms);
                }
                i
            }
            DroneIdRecord::Detected => self.dji_track_where(node, now_ms, &[b"dji-undecoded"], |t| t.dji.undecoded)?,
        };
        self.heard(i, now_ms);
        let t = &mut self.tracks[i];
        if t.uas_ids.is_empty() && t.dji.serial.is_none() && t.dji.key_id.is_none() {
            t.dji.undecoded = true;
        }
        let l = &mut t.dji.ocusync;
        Track::observe_dji(l, node, now_ms);
        l.rssi = s.rssi;
        l.snr_db = s.snr_db;
        l.freq = s.freq;
        if node < 8 {
            t.nodes |= 1 << node;
        }
        t.last_seen_ms = t.last_seen_ms.max(now_ms);
        let id = t.id;
        if let DroneIdRecord::Flight {
            lat_raw,
            lon_raw,
            altitude_raw,
            height_raw,
            v_north,
            v_east,
            v_up,
            pilot_lat_raw,
            pilot_lon_raw,
            home_lat_raw,
            home_lon_raw,
            product_type,
            ..
        } = s.record
        {
            let fix = Fix {
                position: rad_e7_position(lat_raw, lon_raw),
                alt_m: Some((f64::from(altitude_raw) / FEET_PER_M) as f32),
                height_m: Some((f64::from(height_raw) / FEET_PER_M) as f32),
                motion: Some(motion(v_north, v_east, v_up)),
                pilot: rad_e7_position(pilot_lat_raw, pilot_lon_raw),
                home: rad_e7_position(home_lat_raw, home_lon_raw),
            };
            if product_type != 0 {
                t.dji.product_type = Some(product_type);
            }
            self.dji_fix(i, &fix, crate::Source::DroneId, ClaimSrc::DroneId, [0; 6], Some((node, s.rssi)), now_ms);
        }
        Some(id)
    }

    /// Feed one DJI legacy Wi-Fi DroneID element (as a Wi-Fi node sends it
    /// in [`link::Frame::DjiWifi`]: the vendor element body from the OUI,
    /// or the whole element with its ID and length bytes). `None` when the
    /// element is not DJI's, or says nothing that can be tied to a drone.
    #[allow(clippy::too_many_arguments)]
    pub fn ingest_dji_wifi(
        &mut self,
        node: u8,
        channel: u8,
        rssi: i8,
        mac: [u8; 6],
        element: &[u8],
        now_ms: u64,
    ) -> Option<TrackId> {
        use rid::dji::{OUI, Record, parse_element};
        let body = match element {
            [221, _, rest @ ..] if rest.starts_with(&OUI) => rest,
            _ => element,
        };
        let record = parse_element(body)?;
        self.expire(now_ms);
        let by_mac = |t: &Track| t.dji.wifi.heard() && t.dji.wifi.mac == mac;
        let (i, fix, product, purpose) = match &record {
            Record::FlightInfo(f) => {
                let i = match serial_id(f.serial.as_bytes()) {
                    Some(sn) => self.dji_track_by_serial(node, sn, now_ms)?,
                    None => self.dji_track_where(node, now_ms, &[b"dji-wifi", &mac], by_mac)?,
                };
                let fix = Fix {
                    position: f.position,
                    alt_m: f.ext.as_ref().map(|e| f32::from(e.altitude)),
                    height_m: f.ext.as_ref().map(|e| f32::from(e.height)),
                    motion: f.ext.as_ref().map(|e| motion(e.v_north, e.v_east, e.v_up)),
                    // The Wi-Fi record has no pilot position.
                    pilot: None,
                    home: f.ext.as_ref().and_then(|e| e.home),
                };
                let product = f.ext.as_ref().map(|e| e.product_type).filter(|&p| p != 0);
                (i, Some(fix), product, None)
            }
            Record::FlightPurpose(p) => {
                let i = match serial_id(p.serial.as_bytes()) {
                    Some(sn) => self.dji_track_by_serial(node, sn, now_ms)?,
                    None => self.dji_track_where(node, now_ms, &[b"dji-wifi", &mac], by_mac)?,
                };
                let text = p.purpose.0;
                let desc = FixedStr::<23>::new(&text[..text.len().min(23)]).ok();
                (i, None, None, desc)
            }
            Record::Unknown { .. } => {
                // Count it against a drone already known by this address.
                let i = self.tracks.iter().position(by_mac)?;
                (i, None, None, None)
            }
        };
        self.heard(i, now_ms);
        let hide = self.config.hide_droneid_ids;
        let t = &mut self.tracks[i];
        let l = &mut t.dji.wifi;
        Track::observe_dji(l, node, now_ms);
        l.rssi = rssi;
        l.channel = channel;
        l.mac = mac;
        crate::spoof::dji_wifi_frame(t, &mac);
        if node < 8 {
            t.nodes |= 1 << node;
        }
        t.last_seen_ms = t.last_seen_ms.max(now_ms);
        if let Some(p) = product {
            t.dji.product_type = Some(p);
        }
        // DJI's flight purpose fills an empty Self-ID; it never replaces
        // one (Remote ID's, or an earlier one: see `claims`).
        if let Some(d) = purpose
            && !hide
            && !d.as_bytes().is_empty()
            && t.self_id.is_none()
        {
            t.self_id = Some(Stamped {
                value: SelfId {
                    desc_type: DescType::Text,
                    description: d,
                },
                rx_ms: now_ms,
            });
        }
        let id = t.id;
        if let Some(fix) = fix {
            self.dji_fix(i, &fix, crate::Source::DjiWifi, ClaimSrc::DjiWifi, mac, None, now_ms);
        }
        Some(id)
    }

    /// Apply one decrypted O4 record (the hub decrypts with the owner's
    /// key; see `crates/djicrypt`). Call it after [`Tracker::ingest_droneid`]
    /// fed the same record, which created or refreshed the session track.
    ///
    /// Decrypted is not authenticated (see [`crate::claims`]). The serial
    /// is a claim: with no track holding it, the session track takes it;
    /// when another track holds it, the record's position is judged against
    /// that track's path (a rival claim if it does not fit) and the session
    /// joins it only after [`crate::claims::MERGE_MIN_FRAMES`] records over
    /// [`crate::claims::MERGE_SPAN_MS`] with compatible positions. Positions
    /// land on the session's track, judged like any DJI fix. The pilot's DJI
    /// account ID that INFP-10 carries is personal data and is never taken
    /// in.
    ///
    /// `None` when the session has no track (it was not admitted, see
    /// [`crate::probation`]).
    pub fn ingest_o4_decrypted(&mut self, key_id: [u8; 4], rec: O4Decrypted<'_>, now_ms: u64) -> Option<TrackId> {
        let serial = match rec {
            O4Decrypted::Cryp(a) => a.serial_str(),
            O4Decrypted::Infp10(t) => t.serial_str().filter(|_| t.has(Infp10::STATE_SERIAL_VALID)),
            O4Decrypted::Infp11(p) => p.serial_str(),
        }
        .and_then(|s| serial_id(s.as_bytes()));
        let fix = match rec {
            O4Decrypted::Cryp(a) => Some(Fix {
                position: a.position_valid().then(|| rad_e7_position(a.lat_raw, a.lon_raw)).flatten(),
                alt_m: None,
                height_m: None,
                motion: None,
                pilot: None,
                home: None,
            })
            .filter(|f| f.position.is_some()),
            O4Decrypted::Infp10(r) => Some(Fix {
                position: r.has(Infp10::STATE_POSITION_VALID).then(|| rad_e7_position(r.lat_raw, r.lon_raw)).flatten(),
                // INFP-10 altitude is metres and height decimetres
                // (not feet as in OcuSync 2).
                alt_m: r.has(Infp10::STATE_ALTITUDE_VALID).then_some(f32::from(r.altitude_m)),
                height_m: r.has(Infp10::STATE_HEIGHT_VALID).then_some(r.height_m() as f32),
                motion: r.has(Infp10::STATE_HVEL_VALID).then(|| {
                    let (s, tr, v) = motion(r.v_north, r.v_east, r.v_up);
                    (s, tr, if r.has(Infp10::STATE_VVEL_VALID) { v } else { 0.0 })
                }),
                pilot: r
                    .has(Infp10::STATE_APP_POSITION_VALID)
                    .then(|| rad_e7_position(r.app_lat_raw, r.app_lon_raw))
                    .flatten(),
                home: r.home_set().then(|| rad_e7_position(r.home_lat_raw, r.home_lon_raw)).flatten(),
            }),
            O4Decrypted::Infp11(_) => None,
        };
        let session = self.dji_track_where(crate::threats::NODE_ANY, now_ms, &[b"dji-o4", &key_id], |t| t.dji.key_id == Some(key_id))?;
        self.tracks[session].dji.key_id = Some(key_id);
        let mut sid = self.tracks[session].id;
        let hide = self.config.hide_droneid_ids;
        if let Some(sn) = serial {
            let uid = UasId { id_type: IdType::SerialNumber, id: sn };
            match self.serial_track(&uid, Some(sid)) {
                // Nobody else claims the serial: the session's own.
                None => {
                    let t = &mut self.tracks[session];
                    t.dji.serial = Some(sn);
                    if !hide && !t.uas_ids.iter().any(|u| u.id_type == IdType::SerialNumber) {
                        t.add_id(uid);
                    }
                }
                Some(o) => {
                    let other = &self.tracks[o];
                    let fits = fix.as_ref().and_then(|f| f.position).is_none_or(|p| {
                        claims::judge(other, Some(p), None, now_ms) == Verdict::Apply
                    });
                    let oid = other.id;
                    if fits {
                        // Evidence the session is that drone's; the
                        // survivor is the more established track.
                        let kept = self.note_merge(oid, sid, now_ms);
                        if self.tracks.iter().all(|t| t.id != sid) {
                            sid = kept;
                        }
                    } else if let Some(p) = fix.as_ref().and_then(|f| f.position) {
                        // A claim on that drone's ID from elsewhere.
                        let _ = self.tracks[o].rival_claim(p, None, ClaimSrc::DroneIdO4, [0; 6], now_ms);
                    }
                }
            }
        }
        let i = self.tracks.iter().position(|t| t.id == sid)?;
        let t = &mut self.tracks[i];
        t.dji.key_id = Some(key_id);
        t.dji.o4_decrypted = true;
        t.dji.undecoded = false;
        t.last_seen_ms = t.last_seen_ms.max(now_ms);
        match rec {
            O4Decrypted::Infp10(r) if r.product_type != 0 => t.dji.product_type = Some(r.product_type),
            O4Decrypted::Infp11(p) => {
                // Fills an empty Self-ID, never replaces one.
                if let Some(text) = p.purpose_str()
                    && !hide
                    && !text.is_empty()
                    && t.self_id.is_none()
                {
                    let b = text.as_bytes();
                    if let Ok(d) = FixedStr::<23>::new(&b[..b.len().min(23)]) {
                        t.self_id = Some(Stamped { value: SelfId { desc_type: DescType::Text, description: d }, rx_ms: now_ms });
                    }
                }
            }
            _ => {}
        }
        if let Some(f) = fix {
            self.dji_fix(i, &f, crate::Source::DroneId, ClaimSrc::DroneIdO4, [0; 6], None, now_ms);
        }
        Some(sid)
    }
}

/// One decrypted O4 record, as [`Tracker::ingest_o4_decrypted`] takes it.
#[derive(Clone, Copy, Debug)]
pub enum O4Decrypted<'a> {
    Cryp(&'a droneid::o4plain::CrypAux),
    Infp10(&'a Infp10),
    Infp11(&'a droneid::o4plain::Infp11),
}
