//! The aircraft table: messages from many frames joined into one record per
//! aircraft, reported once a second as the same [`adsb::Aircraft`] record
//! the ADSBee's CSBee output gives, so everything downstream (tracker,
//! screen, SD log, phone page, CoT, the FAA-rule checks, the web-head feed)
//! takes it unchanged.
//!
//! Trust rules, in order of how much damage a wrong one does:
//! - An aircraft enters the table only from a **verified** message: DF17/18
//!   with a good (or repaired) CRC, DF11, or DF19.
//! - **Address/parity** replies (DF0/4/5/16/20/21) carry no checkable
//!   address: any corrupt frame yields some address. They are used only
//!   for an aircraft verified in the last [`AP_TRUST_MS`], and are counted
//!   and dropped otherwise.
//! - A **2-bit repair** is accepted only for an aircraft already in the
//!   table from a clean or 1-bit-repaired frame.
//! - **Positions**: global CPR from an even/odd pair at most
//!   [`PAIR_MS`] apart (airborne; [`SURFACE_PAIR_MS`] surface); otherwise
//!   local CPR from the aircraft's own position of the last
//!   [`LOCAL_SELF_MS`], or from the receiver's position. A new position that
//!   implies more than [`MAX_SPEED_KT`] from the last one, or lies more than
//!   [`Config::max_range_km`] from the receiver, is refused.

use libm::{asin, cos, sqrt};

use crate::commb::{self, Bds, Hint};
use crate::cpr::{self, Cpr, CprError, Kind as CprKind};
use crate::decode::{self, AddressCheck, Body, EsSource, Message, Reject};
use crate::es::{self, Es};
use crate::frame::Fix;
use crate::input::Received;

/// Address/parity replies are believed for this long after the aircraft's
/// last verified message.
pub const AP_TRUST_MS: u64 = 60_000;
/// Even and odd airborne positions at most this far apart form a pair.
pub const PAIR_MS: u64 = 10_000;
/// The same for surface positions (slow movers).
pub const SURFACE_PAIR_MS: u64 = 50_000;
/// The aircraft's own last position serves as the local CPR reference for
/// this long.
pub const LOCAL_SELF_MS: u64 = 30_000;
/// Values older than this are reported as not valid.
pub const VALID_MS: u64 = 60_000;
/// Aircraft not heard for this long leave the table.
pub const REMOVE_MS: u64 = 60_000;
/// Positions implying more than this from the previous one are refused.
pub const MAX_SPEED_KT: f64 = 1_200.0;

const NM_M: f64 = 1852.0;

/// Settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Config {
    /// Bits DF17/18 error correction may repair (0-2). The ADSBee already
    /// repairs single-bit errors before it reports a frame, so on its feed
    /// this rarely fires; it matters for any other raw source.
    pub max_fix_bits: u8,
    /// Positions farther than this from the receiver are refused (when the
    /// receiver's position is known).
    pub max_range_km: f64,
    /// Allow a first position from one message and the receiver's position
    /// (local CPR) instead of waiting for an even/odd pair.
    pub local_from_receiver: bool,
    /// ... but only when it lands within this distance of the receiver.
    /// Local CPR is ambiguous beyond half a zone (180 NM); an aircraft a
    /// zone away would alias to a point near the receiver. Line of sight
    /// at 1090 MHz rarely exceeds 400 km, so 250 km leaves the alias case
    /// to aircraft 417+ km away, and the next global pair corrects it.
    pub local_receiver_km: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self { max_fix_bits: 2, max_range_km: 500.0, local_from_receiver: true, local_receiver_km: 250.0 }
    }
}

/// Counters since start.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Frames offered.
    pub frames: u32,
    /// Frames used (decoded and attributed to an aircraft).
    pub used: u32,
    /// Frames by downlink format (index 24 = DF24 and up).
    pub by_df: [u32; 25],
    pub bad_crc: u32,
    pub bad_all_call: u32,
    /// DF22/24: no usable parity.
    pub unchecked: u32,
    /// Repairs made: one bit, two bits.
    pub fixed_1: u32,
    pub fixed_2: u32,
    /// 2-bit repairs refused because the aircraft was not already known.
    pub fix_2_refused: u32,
    /// Address/parity replies for an address not verified recently.
    pub unconfirmed: u32,
    pub tis_b: u32,
    pub ads_r: u32,
    pub positions_global: u32,
    pub positions_local: u32,
    /// Even/odd pairs that straddled a longitude-zone edge.
    pub cpr_zone_mismatch: u32,
    /// Positions refused as implausible (speed or range).
    pub positions_refused: u32,
    /// Comm-B registers inferred: 1,0 2,0 4,0 5,0 6,0 ambiguous unknown.
    pub bds: [u32; 7],
    /// New aircraft that found the table full.
    pub table_full: u32,
}

/// A value and when it was last set.
#[derive(Debug, Clone, Copy, PartialEq)]
struct At<T> {
    v: T,
    ms: u64,
}

fn fresh<T: Copy>(a: Option<At<T>>, now: u64) -> Option<T> {
    a.filter(|a| now.saturating_sub(a.ms) <= VALID_MS).map(|a| a.v)
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Half {
    cpr: Cpr,
    kind: CprKind,
    ms: u64,
}

/// Everything known about one aircraft.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plane {
    pub icao: u32,
    pub first_ms: u64,
    pub last_ms: u64,
    /// Last DF11/17/18/19 (or repaired) message.
    pub verified_ms: u64,
    /// At least one message came from a ground relay (TIS-B / ADS-R), and
    /// none from the aircraft itself.
    pub relayed_only: bool,
    pub military: bool,
    callsign: Option<At<adsb::Callsign>>,
    category: i8,
    squawk: Option<At<u16>>,
    emergency: u8,
    airborne: Option<At<bool>>,
    baro_alt: Option<At<i32>>,
    gnss_alt: Option<At<i32>>,
    gnss_minus_baro: Option<At<i32>>,
    even: Option<Half>,
    odd: Option<Half>,
    pos: Option<At<(f64, f64)>>,
    /// The position chain started from a global (even/odd) decode. A chain
    /// started from the receiver's position (local CPR) could be an alias
    /// of an aircraft a zone away, so a global decode overrides it.
    pos_global: bool,
    refused_in_row: u8,
    speed: Option<At<f32>>,
    track: Option<At<f32>>,
    /// The speed is airspeed and the direction a heading (no ground
    /// velocity heard recently).
    air_data: bool,
    vrate_baro: Option<At<i32>>,
    vrate_gnss: Option<At<i32>>,
    version: u8,
    signal_dbm: i16,
    /// Frames this reporting interval: 56-bit and 112-bit.
    short_n: u16,
    long_n: u16,
    /// adsb::flag::UPDATED_* bits set this interval.
    updated: u32,
}

impl Plane {
    fn new(icao: u32, now: u64) -> Self {
        Self {
            icao,
            first_ms: now,
            last_ms: now,
            verified_ms: now,
            relayed_only: true,
            military: false,
            callsign: None,
            category: -1,
            squawk: None,
            emergency: 0,
            airborne: None,
            baro_alt: None,
            gnss_alt: None,
            gnss_minus_baro: None,
            even: None,
            odd: None,
            pos: None,
            pos_global: false,
            refused_in_row: 0,
            speed: None,
            track: None,
            air_data: false,
            vrate_baro: None,
            vrate_gnss: None,
            version: 0,
            signal_dbm: 0,
            short_n: 0,
            long_n: 0,
            updated: 0,
        }
    }

    pub fn position(&self) -> Option<(f64, f64)> {
        self.pos.map(|p| p.v)
    }

    /// When the position was last set (caller's clock).
    pub fn position_ms(&self) -> Option<u64> {
        self.pos.map(|p| p.ms)
    }

    /// When the barometric altitude was last set.
    pub fn baro_altitude_ms(&self) -> Option<u64> {
        self.baro_alt.map(|a| a.ms)
    }

    /// The position chain was confirmed by a global (even/odd) decode.
    pub fn position_global(&self) -> bool {
        self.pos_global
    }

    pub fn callsign(&self) -> Option<adsb::Callsign> {
        self.callsign.map(|c| c.v)
    }

    pub fn baro_altitude_ft(&self) -> Option<i32> {
        self.baro_alt.map(|a| a.v)
    }

    pub fn ground_speed_kt(&self) -> Option<f32> {
        (!self.air_data).then_some(self.speed.map(|s| s.v)).flatten()
    }

    fn hint(&self) -> Hint {
        Hint {
            ground_speed_kt: self.ground_speed_kt(),
            track_deg: (!self.air_data).then_some(self.track.map(|t| t.v)).flatten(),
            callsign: self.callsign(),
        }
    }

    /// The record the ADSBee would print for this aircraft.
    pub fn to_aircraft(&self, now: u64) -> adsb::Aircraft {
        use adsb::flag::*;
        let mut flags = self.updated;
        let mut set = |bit: u32, on: bool| {
            if on {
                flags |= 1 << bit;
            }
        };
        let airborne = fresh(self.airborne, now);
        set(AIRBORNE, airborne == Some(true));
        let baro = fresh(self.baro_alt, now);
        let gnss = fresh(self.gnss_alt, now).or_else(|| Some(baro? + fresh(self.gnss_minus_baro, now)?));
        set(BARO_ALT_VALID, baro.is_some());
        set(GNSS_ALT_VALID, gnss.is_some());
        let pos = fresh(self.pos, now);
        set(POSITION_VALID, pos.is_some());
        let trk = fresh(self.track, now);
        set(DIRECTION_VALID, trk.is_some());
        let spd = fresh(self.speed, now);
        set(SPEED_VALID, spd.is_some());
        let vb = fresh(self.vrate_baro, now);
        let vg = fresh(self.vrate_gnss, now);
        set(BARO_VRATE_VALID, vb.is_some());
        set(GNSS_VRATE_VALID, vg.is_some());
        set(MODE_S_MILITARY, self.military);
        let (lat, lon) = pos.unwrap_or((0.0, 0.0));
        adsb::Aircraft {
            source: adsb::Source::ModeS,
            icao: self.icao,
            flags,
            callsign: fresh(self.callsign, now).unwrap_or_default(),
            squawk: fresh(self.squawk, now),
            category: self.category,
            lat_deg: lat,
            lon_deg: lon,
            baro_alt_ft: baro.unwrap_or(0),
            gnss_alt_ft: gnss.unwrap_or(0),
            direction_deg: trk.unwrap_or(0.0),
            speed_kt: spd.map_or(0, |s| libm::roundf(s) as i32),
            baro_vrate_fpm: vb.unwrap_or(0),
            gnss_vrate_fpm: vg.unwrap_or(0),
            uat_emergency: 0,
            nicnac: 0,
            acdims: 0,
            version: self.version,
            signal_dbm: self.signal_dbm,
            signal_quality: 0,
            frames_per_s: self.short_n,
            es_frames_per_s: self.long_n,
        }
    }
}

/// Great-circle distance, metres (haversine on a 6371 km sphere; plenty
/// for plausibility checks).
pub fn distance_m(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (la1, lo1) = (a.0.to_radians(), a.1.to_radians());
    let (la2, lo2) = (b.0.to_radians(), b.1.to_radians());
    let s1 = libm::sin((la2 - la1) / 2.0);
    let s2 = libm::sin((lo2 - lo1) / 2.0);
    let h = s1 * s1 + cos(la1) * cos(la2) * s2 * s2;
    2.0 * 6_371_000.0 * asin(sqrt(h.min(1.0)))
}

/// The aircraft table, holding up to `N` aircraft.
pub struct Table<const N: usize> {
    planes: [Option<Plane>; N],
    pub config: Config,
    pub stats: Stats,
    reference: Option<(f64, f64)>,
    /// Interval counters for the `#S:`-style statistics.
    raw_short: u16,
    raw_long: u16,
    ok_short: u16,
    ok_long: u16,
    started_ms: Option<u64>,
}

impl<const N: usize> Default for Table<N> {
    fn default() -> Self {
        Self::new(Config::default())
    }
}

impl<const N: usize> Table<N> {
    pub fn new(config: Config) -> Self {
        Self {
            planes: [None; N],
            config,
            stats: Stats::default(),
            reference: None,
            raw_short: 0,
            raw_long: 0,
            ok_short: 0,
            ok_long: 0,
            started_ms: None,
        }
    }

    /// Build an empty table directly at `dst` (for example in external
    /// RAM), field by field, so a whole table (~12 KB for 32 aircraft) never
    /// sits on a small embedded stack the way `ptr.write(Table::new(..))`
    /// may put it.
    ///
    /// # Safety
    ///
    /// `dst` must be valid for writes of a `Table<N>` and properly aligned.
    /// Whatever was there is overwritten without being dropped.
    pub unsafe fn init_in_place(dst: *mut Self, config: Config) {
        // SAFETY: the caller guarantees `dst` is writable and aligned; each
        // field is written once through a raw pointer, no reference to
        // uninitialised memory is made.
        unsafe {
            let planes = (&raw mut (*dst).planes).cast::<Option<Plane>>();
            for i in 0..N {
                planes.add(i).write(None);
            }
            (&raw mut (*dst).config).write(config);
            (&raw mut (*dst).stats).write(Stats::default());
            (&raw mut (*dst).reference).write(None);
            (&raw mut (*dst).raw_short).write(0);
            (&raw mut (*dst).raw_long).write(0);
            (&raw mut (*dst).ok_short).write(0);
            (&raw mut (*dst).ok_long).write(0);
            (&raw mut (*dst).started_ms).write(None);
        }
    }

    /// The receiver's own position (hub GPS or a manual position): the
    /// local-CPR reference, the surface-decode reference and the range
    /// check. `None` when unknown.
    pub fn set_reference(&mut self, r: Option<(f64, f64)>) {
        self.reference = r;
    }

    pub fn planes(&self) -> impl Iterator<Item = &Plane> {
        self.planes.iter().flatten()
    }

    pub fn get(&self, icao: u32) -> Option<&Plane> {
        self.planes().find(|p| p.icao == icao)
    }

    pub fn len(&self) -> usize {
        self.planes().count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn index(&self, icao: u32) -> Option<usize> {
        self.planes.iter().position(|p| p.is_some_and(|p| p.icao == icao))
    }

    /// Find or add. When full, the aircraft heard longest ago makes room.
    fn slot(&mut self, icao: u32, now: u64) -> Option<usize> {
        if let Some(i) = self.index(icao) {
            return Some(i);
        }
        let i = match self.planes.iter().position(|p| p.is_none()) {
            Some(i) => i,
            None => {
                self.stats.table_full += 1;
                let (i, oldest) = self
                    .planes
                    .iter()
                    .enumerate()
                    .filter_map(|(i, p)| p.map(|p| (i, p.last_ms)))
                    .min_by_key(|&(_, t)| t)?;
                // Only evict an aircraft that has gone quiet.
                if now.saturating_sub(oldest) < 5_000 {
                    return None;
                }
                i
            }
        };
        self.planes[i] = Some(Plane::new(icao, now));
        Some(i)
    }

    /// Take one received frame.
    pub fn ingest(&mut self, rx: &Received, now: u64) -> Result<u32, Reject> {
        self.started_ms.get_or_insert(now);
        self.stats.frames += 1;
        let mut frame = rx.frame;
        let df = frame.df();
        self.stats.by_df[usize::from(df.min(24))] += 1;
        if frame.is_long() {
            self.raw_long = self.raw_long.saturating_add(1);
        } else {
            self.raw_short = self.raw_short.saturating_add(1);
        }
        let msg = match decode::decode(&mut frame, self.config.max_fix_bits) {
            Ok(m) => m,
            Err(e) => {
                match e {
                    Reject::BadCrc => self.stats.bad_crc += 1,
                    Reject::BadAllCall => self.stats.bad_all_call += 1,
                    Reject::Unchecked => self.stats.unchecked += 1,
                }
                return Err(e);
            }
        };
        let idx = match msg.check {
            AddressCheck::Verified(Fix::Two(..)) => {
                let known = self.index(msg.address).filter(|&i| {
                    self.planes[i].is_some_and(|p| now.saturating_sub(p.verified_ms) <= AP_TRUST_MS)
                });
                match known {
                    Some(i) => {
                        self.stats.fixed_2 += 1;
                        i
                    }
                    None => {
                        self.stats.fix_2_refused += 1;
                        return Err(Reject::BadCrc);
                    }
                }
            }
            AddressCheck::Verified(_) | AddressCheck::Interrogator(_) => {
                if let AddressCheck::Verified(Fix::One(_)) = msg.check {
                    self.stats.fixed_1 += 1;
                }
                match self.slot(msg.address, now) {
                    Some(i) => i,
                    None => return Ok(msg.address),
                }
            }
            AddressCheck::AddressParity => {
                let known = self.index(msg.address).filter(|&i| {
                    self.planes[i].is_some_and(|p| now.saturating_sub(p.verified_ms) <= AP_TRUST_MS)
                });
                match known {
                    Some(i) => i,
                    None => {
                        self.stats.unconfirmed += 1;
                        return Err(Reject::BadCrc);
                    }
                }
            }
        };
        self.stats.used += 1;
        if frame.is_long() {
            self.ok_long = self.ok_long.saturating_add(1);
        } else {
            self.ok_short = self.ok_short.saturating_add(1);
        }
        let reference = self.reference;
        let config = self.config;
        let Some(p) = self.planes[idx].as_mut() else {
            return Ok(msg.address);
        };
        p.last_ms = now;
        if matches!(msg.check, AddressCheck::Verified(_) | AddressCheck::Interrogator(_)) {
            p.verified_ms = now;
        }
        if let Some(d) = rx.signal_dbm {
            p.signal_dbm = d;
        }
        if frame.is_long() {
            p.long_n = p.long_n.saturating_add(1);
        } else {
            p.short_n = p.short_n.saturating_add(1);
        }
        let mut st = Applied::default();
        apply(p, &msg, now, reference, &config, &mut st);
        self.stats.positions_global += st.global;
        self.stats.positions_local += st.local;
        self.stats.cpr_zone_mismatch += st.zone_mismatch;
        self.stats.positions_refused += st.refused;
        self.stats.tis_b += st.tis_b;
        self.stats.ads_r += st.ads_r;
        if let Some(b) = st.bds {
            self.stats.bds[b] += 1;
        }
        Ok(msg.address)
    }

    /// Once a second: every aircraft heard in the last [`REMOVE_MS`], as
    /// an ADSBee-style record, then the interval counters restart. Aircraft
    /// not heard for longer are dropped.
    pub fn report(&mut self, now: u64, mut out: impl FnMut(adsb::Aircraft)) -> adsb::Stats {
        let mut n = 0u16;
        for slot in self.planes.iter_mut() {
            let Some(p) = slot else { continue };
            if now.saturating_sub(p.last_ms) > REMOVE_MS {
                *slot = None;
                continue;
            }
            n += 1;
            out(p.to_aircraft(now));
            p.short_n = 0;
            p.long_n = 0;
            p.updated = 0;
        }
        let s = adsb::Stats {
            version: adsb::PROTOCOL_VERSION,
            demods_per_s: self.raw_short.saturating_add(self.raw_long),
            raw_squitters_per_s: self.raw_short,
            squitters_per_s: self.ok_short,
            raw_extended_squitters_per_s: self.raw_long,
            extended_squitters_per_s: self.ok_long,
            raw_uat_per_s: 0,
            uat_per_s: 0,
            aircraft: n,
            tscal: 0,
            uptime_s: (now.saturating_sub(self.started_ms.unwrap_or(now)) / 1000) as u32,
        };
        self.raw_short = 0;
        self.raw_long = 0;
        self.ok_short = 0;
        self.ok_long = 0;
        s
    }
}

#[derive(Default)]
struct Applied {
    global: u32,
    local: u32,
    zone_mismatch: u32,
    refused: u32,
    tis_b: u32,
    ads_r: u32,
    bds: Option<usize>,
}

fn set<T>(slot: &mut Option<At<T>>, v: T, now: u64) {
    *slot = Some(At { v, ms: now });
}

fn apply(p: &mut Plane, msg: &Message, now: u64, reference: Option<(f64, f64)>, config: &Config, st: &mut Applied) {
    use adsb::flag::*;
    let not_relay = |p: &mut Plane| p.relayed_only = false;
    match msg.body {
        Body::ShortAcas { airborne, altitude } | Body::LongAcas { airborne, altitude, .. } => {
            not_relay(p);
            set(&mut p.airborne, airborne, now);
            if let Some(f) = altitude.feet() {
                set(&mut p.baro_alt, f, now);
                p.updated |= 1 << UPDATED_BARO_ALT;
            }
        }
        Body::AltitudeReply { fs, altitude } | Body::CommBAltitude { fs, altitude, .. } => {
            not_relay(p);
            flight_status(p, fs, now);
            if let Some(f) = altitude.feet() {
                set(&mut p.baro_alt, f, now);
                p.updated |= 1 << UPDATED_BARO_ALT;
            }
        }
        Body::IdentityReply { fs, squawk } | Body::CommBIdentity { fs, squawk, .. } => {
            not_relay(p);
            flight_status(p, fs, now);
            set(&mut p.squawk, squawk, now);
        }
        Body::AllCall { ca } => {
            not_relay(p);
            capability(p, ca, now);
        }
        Body::Military => {
            not_relay(p);
            p.military = true;
        }
        Body::Extended { source, ca, es } => {
            match source {
                EsSource::TisB { .. } => st.tis_b += 1,
                EsSource::AdsR { .. } => st.ads_r += 1,
                _ => not_relay(p),
            }
            if msg.df == 17 {
                capability(p, ca, now);
            }
            extended(p, &es, now, reference, config, st);
        }
        Body::Other => {}
    }
    // Comm-B content.
    let mb = match msg.body {
        Body::CommBAltitude { mb, .. } | Body::CommBIdentity { mb, .. } => Some(mb),
        _ => None,
    };
    if let Some(mb) = mb {
        let bds = commb::infer(&mb, &p.hint());
        st.bds = Some(match bds {
            Bds::DataLinkCapability => 0,
            Bds::Identification(cs) => {
                if p.callsign.is_none() {
                    set(&mut p.callsign, cs, now);
                }
                1
            }
            Bds::VerticalIntent(_) => 2,
            Bds::TrackTurn(_) => 3,
            Bds::HeadingSpeed(_) => 4,
            Bds::Ambiguous(_) => 5,
            Bds::Unknown => 6,
        });
    }
}

fn flight_status(p: &mut Plane, fs: u8, now: u64) {
    match fs {
        0 | 2 => set(&mut p.airborne, true, now),
        1 | 3 => set(&mut p.airborne, false, now),
        _ => {}
    }
}

/// DF11/17 capability: 4 on the ground, 5 airborne.
fn capability(p: &mut Plane, ca: u8, now: u64) {
    match ca {
        4 => set(&mut p.airborne, false, now),
        5 => set(&mut p.airborne, true, now),
        _ => {}
    }
}

fn extended(p: &mut Plane, es: &Es, now: u64, reference: Option<(f64, f64)>, config: &Config, st: &mut Applied) {
    use adsb::flag::*;
    match *es {
        Es::Identification { tc, category, callsign } => {
            set(&mut p.callsign, callsign, now);
            p.category = es::category_number(tc, category);
        }
        Es::AirbornePosition { altitude, gnss, cpr, .. } => {
            set(&mut p.airborne, true, now);
            if let Some(f) = altitude.feet() {
                if gnss {
                    set(&mut p.gnss_alt, f, now);
                    p.updated |= 1 << UPDATED_GNSS_ALT;
                } else {
                    set(&mut p.baro_alt, f, now);
                    p.updated |= 1 << UPDATED_BARO_ALT;
                }
            }
            position(p, cpr, CprKind::Airborne, now, reference, config, st);
        }
        Es::SurfacePosition { speed_kt, track_deg, cpr, .. } => {
            set(&mut p.airborne, false, now);
            if let Some(s) = speed_kt {
                set(&mut p.speed, s, now);
                p.air_data = false;
                p.updated |= 1 << UPDATED_SPEED;
            }
            if let Some(t) = track_deg {
                set(&mut p.track, t, now);
                p.updated |= 1 << UPDATED_DIRECTION;
            }
            position(p, cpr, CprKind::Surface, now, reference, config, st);
        }
        Es::Velocity(v) => {
            if let (Some(gs), Some(trk)) = (v.ground_speed_kt, v.track_deg) {
                set(&mut p.speed, gs, now);
                set(&mut p.track, trk, now);
                p.air_data = false;
                p.updated |= 1 << UPDATED_SPEED | 1 << UPDATED_DIRECTION;
            } else {
                // Airspeed and heading only when no ground velocity is
                // recent: track over ground is what a map needs.
                let gs_recent = !p.air_data && p.speed.is_some_and(|s| now.saturating_sub(s.ms) <= 10_000);
                if !gs_recent {
                    if let Some(a) = v.airspeed_kt {
                        set(&mut p.speed, f32::from(a), now);
                        p.air_data = true;
                        p.updated |= 1 << UPDATED_SPEED;
                    }
                    if let Some(h) = v.heading_deg {
                        set(&mut p.track, h, now);
                        p.air_data = true;
                        p.updated |= 1 << UPDATED_DIRECTION;
                    }
                }
            }
            if let Some(r) = v.vertical_rate_fpm {
                if v.vertical_rate_baro {
                    set(&mut p.vrate_baro, r, now);
                    p.updated |= 1 << UPDATED_BARO_VRATE;
                } else {
                    set(&mut p.vrate_gnss, r, now);
                    p.updated |= 1 << UPDATED_GNSS_VRATE;
                }
            }
            if let Some(d) = v.gnss_minus_baro_ft {
                set(&mut p.gnss_minus_baro, d, now);
            }
        }
        Es::EmergencyStatus { emergency, squawk } => {
            p.emergency = emergency;
            if squawk != 0 {
                set(&mut p.squawk, squawk, now);
            }
        }
        Es::OperationalStatus(o) => {
            p.version = o.version;
        }
        Es::TcasRa | Es::TargetState | Es::Other { .. } => {}
    }
}

fn position(
    p: &mut Plane,
    cpr: Cpr,
    kind: CprKind,
    now: u64,
    reference: Option<(f64, f64)>,
    config: &Config,
    st: &mut Applied,
) {
    let half = Half { cpr, kind, ms: now };
    if cpr.odd {
        p.odd = Some(half);
    } else {
        p.even = Some(half);
    }
    let window = match kind {
        CprKind::Airborne => PAIR_MS,
        CprKind::Surface => SURFACE_PAIR_MS,
    };
    let own = p.pos.filter(|a| now.saturating_sub(a.ms) <= LOCAL_SELF_MS).map(|a| a.v);
    let mut result = None;
    let mut global = false;
    if let (Some(e), Some(o)) = (p.even, p.odd)
        && e.kind == kind && o.kind == kind && e.ms.abs_diff(o.ms) <= window {
            let surface_ref = own.or(reference);
            match cpr::global(e.cpr, o.cpr, cpr.odd, kind, surface_ref) {
                Ok(r) => {
                    result = Some(r);
                    global = true;
                }
                Err(CprError::ZoneMismatch) => st.zone_mismatch += 1,
                Err(_) => {}
            }
        }
    let mut from_receiver = false;
    if result.is_none() {
        let (local_ref, limit) = match (own, reference) {
            (Some(o), _) => (
                Some(o),
                match kind {
                    CprKind::Airborne => 180.0 * NM_M,
                    CprKind::Surface => 45.0 * NM_M,
                },
            ),
            (None, Some(r)) if config.local_from_receiver => {
                from_receiver = true;
                let half_zone = match kind {
                    CprKind::Airborne => 180.0 * NM_M,
                    CprKind::Surface => 45.0 * NM_M,
                };
                (Some(r), (config.local_receiver_km * 1000.0).min(half_zone))
            }
            _ => (None, 0.0),
        };
        if let Some(r) = local_ref {
            let lp = cpr::local(cpr, kind, r);
            if distance_m(lp, r) < limit {
                result = Some(lp);
            }
        }
    }
    let Some(new) = result else { return };
    if !(-90.0..=90.0).contains(&new.0) || !(-180.0..=180.0).contains(&new.1) {
        st.refused += 1;
        return;
    }
    if let Some(r) = reference
        && distance_m(new, r) > config.max_range_km * 1000.0 {
            st.refused += 1;
            // A global decode puts it out of range: a position chain that
            // started from the receiver was an alias.
            if global && !p.pos_global {
                p.pos = None;
            }
            return;
        }
    // A global decode overrides a chain that never had one.
    let override_chain = global && !p.pos_global;
    if let (Some(prev), false) = (p.pos, override_chain) {
        let dt_s = now.saturating_sub(prev.ms) as f64 / 1000.0;
        let max_m = MAX_SPEED_KT * NM_M / 3600.0 * dt_s + 2_000.0;
        if distance_m(new, prev.v) > max_m {
            st.refused += 1;
            p.refused_in_row = p.refused_in_row.saturating_add(1);
            // Several refusals in a row: the old position was the bad one.
            // Forget it so the next global pair starts afresh.
            if p.refused_in_row >= 3 {
                p.pos = None;
                p.pos_global = false;
                p.refused_in_row = 0;
            }
            // The pair that produced this is suspect too.
            if global {
                p.even = None;
                p.odd = None;
            }
            return;
        }
    }
    if global {
        p.pos_global = true;
    } else if from_receiver {
        p.pos_global = false;
    }
    p.refused_in_row = 0;
    set(&mut p.pos, new, now);
    p.updated |= 1 << adsb::flag::UPDATED_POSITION;
    if global {
        st.global += 1;
    } else {
        st.local += 1;
    }
}
