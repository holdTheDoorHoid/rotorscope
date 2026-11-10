//! Crewed aircraft from an ADSBee 1090 (ADS-B / Mode S on 1090 MHz, and UAT
//! on 978 MHz with a 1090U), kept apart from Remote ID drones.
//!
//! The hub reads the ADSBee's CSBee lines with the `adsb` crate and hands
//! each aircraft report to [`crate::Tracker::ingest_aircraft`]. Here they
//! become one [`AircraftTrack`] per transponder address (and receiver), with
//! range and bearing from the unit by the same Vincenty code as drones.
//!
//! **Never an alert.** Aircraft are for awareness (medevac helicopters, crop
//! dusters), not a detection: they never enter the drone table, never
//! trigger the new-drone alert or the buzzer. The only extra notion is
//! [`AircraftGeometry::low_nearby`], a quiet flag the phone page and screen
//! show as text.
//!
//! **Freshness.** The ADSBee lists every aircraft in its own table once a
//! second, including ones it has stopped hearing, until it prunes them. A
//! report only counts as hearing the aircraft when it says so
//! ([`adsb::Aircraft::heard`]), and a position is only re-stamped when the
//! ADSBee says it was updated in the last second, so ages here are real.
//!
//! **Expiry.** Aircraft move fast: an airliner at 250 kt crosses 2 km in 15
//! s. They go stale after [`AIRCRAFT_STALE_MS`] and are removed after
//! [`AIRCRAFT_REMOVE_MS`], much sooner than drones.
//!
//! **Capacity.** The table holds `A` aircraft (a const generic of the
//! tracker, 0 on hubs built without ADS-B). Near an airport there are more;
//! when full, the least useful one makes room: stale before active, then no
//! position before positioned, then farthest from the unit. A newcomer that
//! would itself be the least useful is not added.

use adsb::{Callsign, Kind, Source};

use crate::{OwnFix, Stamped, Track, TrackId, TrackState, geo};

/// Aircraft the hub holds by default (the [`crate::Tracker`] default).
/// Each costs about 120 bytes of static RAM.
pub const MAX_AIRCRAFT: usize = 32;
/// Not heard for this long: shown as stale.
pub const AIRCRAFT_STALE_MS: u64 = 15_000;
/// Not heard for this long: removed.
pub const AIRCRAFT_REMOVE_MS: u64 = 60_000;
/// "Low aircraft nearby": within this distance...
pub const LOW_RANGE_M: f64 = 5_000.0;
/// ... and at most this far above the unit (2000 ft).
pub const LOW_ABOVE_M: f64 = 610.0;
/// An ADS-B "UAV" this close to a Remote ID drone is taken to be that drone
/// ([`same_drone`]), so it is not sent to TAK twice.
pub const SAME_AS_DRONE_M: f64 = 300.0;

const FT: f64 = 0.3048;
const KT: f64 = 0.514_444;

/// Which aircraft: its 24-bit address and the receiver that heard it.
/// (UAT and 1090 MHz addresses are the same ICAO space, but an aircraft is
/// very rarely heard on both, and keeping them apart keeps each one's data
/// coherent.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AircraftKey {
    pub icao: u32,
    pub source: Source,
}

/// A position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position {
    pub lat: f64,
    pub lon: f64,
}

/// Everything known about one aircraft.
#[derive(Debug, Clone, PartialEq)]
pub struct AircraftTrack {
    pub key: AircraftKey,
    /// UAT address qualifier (0 for Mode S): 2 and 3 are TIS-B targets
    /// relayed by a ground station, not the aircraft's own broadcast.
    pub address_qualifier: u8,
    pub callsign: Callsign,
    pub squawk: Option<u16>,
    pub category: i8,
    /// When `rx_ms` is the hub time the ADSBee last *updated* it.
    pub position: Option<Stamped<Position>>,
    /// Pressure altitude, feet (what ATC and pilots use).
    pub baro_alt_ft: Option<i32>,
    /// Geometric altitude above the WGS-84 ellipsoid, feet.
    pub gnss_alt_ft: Option<i32>,
    pub direction_deg: Option<f32>,
    pub speed_kt: Option<i32>,
    pub vrate_fpm: Option<i32>,
    pub airborne: bool,
    pub military: bool,
    /// Signal strength of the latest message, dBm.
    pub rssi_dbm: i16,
    pub first_seen_ms: u64,
    /// Last report in which the ADSBee had heard it.
    pub last_seen_ms: u64,
    /// Reports received (heard or not).
    pub reports: u32,
}

impl AircraftTrack {
    pub fn kind(&self) -> Kind {
        Kind::of(self.category)
    }

    /// The emergency a squawk code declares, if any.
    pub fn emergency(&self) -> Option<&'static str> {
        match self.squawk? {
            0o7500 => Some("hijack"),
            0o7600 => Some("radio failure"),
            0o7700 => Some("emergency"),
            _ => None,
        }
    }

    /// Best altitude above the WGS-84 ellipsoid, metres: geometric if
    /// known, else pressure altitude (which is close to height above sea
    /// level, so this is off by the geoid there, about 30 m around
    /// Philadelphia, plus the pressure error; fine for "how high").
    pub fn alt_m(&self) -> Option<f64> {
        self.gnss_alt_ft
            .or(self.baro_alt_ft)
            .map(|ft| f64::from(ft) * FT)
    }

    pub fn speed_mps(&self) -> Option<f64> {
        self.speed_kt.map(|k| f64::from(k) * KT)
    }

    fn update(&mut self, a: &adsb::Aircraft, now_ms: u64) {
        self.address_qualifier = a.address_qualifier();
        if !a.callsign.is_empty() {
            self.callsign = a.callsign;
        }
        self.squawk = a.squawk.or(self.squawk);
        if a.category >= 0 {
            self.category = a.category;
        }
        if let Some((lat, lon)) = a.position() {
            let p = Position { lat, lon };
            let moved = self.position.as_ref().is_none_or(|s| s.value != p);
            if a.position_updated() || moved {
                self.position = Some(Stamped {
                    value: p,
                    rx_ms: now_ms,
                });
            }
        }
        self.baro_alt_ft = a.baro_altitude_ft().or(self.baro_alt_ft);
        self.gnss_alt_ft = a.gnss_altitude_ft().or(self.gnss_alt_ft);
        self.direction_deg = a.direction_deg().or(self.direction_deg);
        self.speed_kt = a.speed_kt().or(self.speed_kt);
        self.vrate_fpm = a.vertical_rate_fpm().or(self.vrate_fpm);
        self.airborne = a.airborne();
        self.military |= a.military();
        self.rssi_dbm = a.signal_dbm;
        if a.heard() {
            self.last_seen_ms = self.last_seen_ms.max(now_ms);
        }
        self.reports = self.reports.saturating_add(1);
    }
}

/// Range, bearing and height of an aircraft from the unit.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AircraftGeometry {
    /// `None` without an own fix or an aircraft position.
    pub range: Option<geo::RangeBearing>,
    /// Aircraft altitude minus own altitude, metres ([`AircraftTrack::alt_m`]
    /// minus the own fix's height). `None` without both.
    pub above_m: Option<f64>,
    /// Airborne, within [`LOW_RANGE_M`] and at most [`LOW_ABOVE_M`] above
    /// the unit. With no own altitude, the aircraft's own altitude is used
    /// as if the unit were at sea level. A quiet heads-up, never an alarm.
    pub low_nearby: bool,
}

impl AircraftGeometry {
    pub fn of(a: &AircraftTrack, own: Option<&OwnFix>) -> Self {
        let Some(own) = own else {
            return Self::default();
        };
        let range = a
            .position
            .as_ref()
            .map(|p| geo::range_bearing(own.lat, own.lon, p.value.lat, p.value.lon));
        let alt = a.alt_m();
        let above_m = alt.zip(own.alt_m).map(|(a, o)| a - o);
        let low_nearby = a.airborne
            && range.is_some_and(|r| r.distance_m <= LOW_RANGE_M)
            && above_m.or(alt).is_some_and(|h| h <= LOW_ABOVE_M);
        Self {
            range,
            above_m,
            low_nearby,
        }
    }
}

/// Active or stale, by the aircraft thresholds.
pub fn aircraft_state(a: &AircraftTrack, now_ms: u64) -> TrackState {
    if now_ms.saturating_sub(a.last_seen_ms) >= AIRCRAFT_STALE_MS {
        TrackState::Stale
    } else {
        TrackState::Active
    }
}

/// The Remote ID drone an ADS-B aircraft probably *is*: only for emitter
/// category UAV, the nearest active drone track with a position within
/// [`SAME_AS_DRONE_M`]. Needs no own fix. Used so a drone that carries both
/// Remote ID and ADS-B is not sent to TAK twice, and so the phone page can
/// say so.
pub fn same_drone(a: &AircraftTrack, drones: &[Track], ctx: &crate::Context) -> Option<TrackId> {
    if a.kind() != Kind::Uav {
        return None;
    }
    let p = a.position.as_ref()?.value;
    drones
        .iter()
        .filter(|t| ctx.state(t) == TrackState::Active)
        .filter_map(|t| {
            let pos = t.location.as_ref()?.value.position?;
            let d = geo::range_bearing(p.lat, p.lon, pos.lat, pos.lon).distance_m;
            (d <= SAME_AS_DRONE_M).then_some((t.id, d))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(id, _)| id)
}

/// The aircraft table: at most `A` aircraft.
#[derive(Debug, Clone)]
pub struct AircraftTable<const A: usize> {
    aircraft: heapless::Vec<AircraftTrack, A>,
}

impl<const A: usize> Default for AircraftTable<A> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const A: usize> AircraftTable<A> {
    pub const fn new() -> Self {
        Self {
            aircraft: heapless::Vec::new(),
        }
    }

    pub fn all(&self) -> &[AircraftTrack] {
        &self.aircraft
    }

    pub fn get(&self, key: AircraftKey) -> Option<&AircraftTrack> {
        self.aircraft.iter().find(|a| a.key == key)
    }

    /// Merge one report. Returns the key if the aircraft is (now) in the
    /// table: a report of an aircraft the ADSBee is no longer hearing does
    /// not create an entry, and neither does one that would be the first
    /// to go if the table is full.
    pub fn ingest(
        &mut self,
        r: &adsb::Aircraft,
        now_ms: u64,
        own: Option<&OwnFix>,
    ) -> Option<AircraftKey> {
        self.expire(now_ms);
        let key = AircraftKey {
            icao: r.icao24(),
            source: r.source,
        };
        if let Some(a) = self.aircraft.iter_mut().find(|a| a.key == key) {
            a.update(r, now_ms);
            return Some(key);
        }
        if !r.heard() {
            return None;
        }
        let mut new = AircraftTrack {
            key,
            address_qualifier: 0,
            callsign: Callsign::default(),
            squawk: None,
            category: -1,
            position: None,
            baro_alt_ft: None,
            gnss_alt_ft: None,
            direction_deg: None,
            speed_kt: None,
            vrate_fpm: None,
            airborne: false,
            military: false,
            rssi_dbm: 0,
            first_seen_ms: now_ms,
            last_seen_ms: now_ms,
            reports: 0,
        };
        new.update(r, now_ms);
        if self.aircraft.is_full() {
            let rank = |a: &AircraftTrack| keep_rank(a, now_ms, own);
            let worst = (0..self.aircraft.len())
                .max_by(|&i, &j| rank(&self.aircraft[i]).total_cmp_key(&rank(&self.aircraft[j])))?;
            if rank(&new).total_cmp_key(&rank(&self.aircraft[worst])) != core::cmp::Ordering::Less {
                return None;
            }
            self.aircraft.swap_remove(worst);
        }
        self.aircraft.push(new).ok()?;
        Some(key)
    }

    /// Drop aircraft not heard for [`AIRCRAFT_REMOVE_MS`].
    pub fn expire(&mut self, now_ms: u64) -> usize {
        let before = self.aircraft.len();
        self.aircraft
            .retain(|a| now_ms.saturating_sub(a.last_seen_ms) < AIRCRAFT_REMOVE_MS);
        before - self.aircraft.len()
    }

    pub fn active_count(&self, now_ms: u64) -> usize {
        self.aircraft
            .iter()
            .filter(|a| aircraft_state(a, now_ms) == TrackState::Active)
            .count()
    }
}

/// How much an aircraft is worth keeping: lower is better.
#[derive(Clone, Copy)]
struct KeepRank {
    stale: bool,
    no_position: bool,
    distance_m: f64,
    quiet_ms: u64,
}

impl KeepRank {
    fn total_cmp_key(&self, o: &Self) -> core::cmp::Ordering {
        (self.stale, self.no_position)
            .cmp(&(o.stale, o.no_position))
            .then(self.distance_m.total_cmp(&o.distance_m))
            .then(self.quiet_ms.cmp(&o.quiet_ms))
    }
}

fn keep_rank(a: &AircraftTrack, now_ms: u64, own: Option<&OwnFix>) -> KeepRank {
    let g = AircraftGeometry::of(a, own);
    KeepRank {
        stale: aircraft_state(a, now_ms) == TrackState::Stale,
        no_position: a.position.is_none(),
        distance_m: g.range.map_or(0.0, |r| r.distance_m),
        quiet_ms: now_ms.saturating_sub(a.last_seen_ms),
    }
}

/// The ICAO address as six upper-case hex digits.
pub fn icao_hex(a: &AircraftTrack) -> heapless::String<6> {
    crate::text::render(IcaoHex(a.key.icao))
}

pub(crate) struct IcaoHex(pub u32);

impl core::fmt::Display for IcaoHex {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:06X}", self.0 & 0x00ff_ffff)
    }
}
