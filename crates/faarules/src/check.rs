//! The checks themselves: one track's conditions right now, without any
//! history (durations and levels are [`crate::Entry`]'s job).

use airspace::format::src;
use airspace::sun::{self, Light};
use airspace::{AltRef, AreaKind, FT_PER_M, M_PER_FT, Pack, distance_m};
use heapless::{String, Vec};
use rid::message::{HeightType, OperatorLocationType, Status};
use tracker::{AircraftTrack, Track};

use crate::{Rule, Settings};

/// What the checks get besides the track.
pub struct Inputs<'p, 'a> {
    pub pack: Option<&'p Pack<'a>>,
    pub settings: &'p Settings,
    pub now_ms: u64,
    pub utc_ms: Option<u64>,
}

/// How the height above ground was worked out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeightSource {
    /// Remote ID height with type "above ground".
    AboveGround,
    /// Geodetic altitude minus the terrain (and geoid) from the pack.
    Terrain,
    /// Height above takeoff, corrected by the terrain difference when known.
    AboveTakeoff,
    /// Pressure altitude minus terrain: least certain.
    Pressure,
}

impl HeightSource {
    pub fn name(self) -> &'static str {
        match self {
            Self::AboveGround => "RID height AGL",
            Self::Terrain => "GNSS alt - terrain",
            Self::AboveTakeoff => "height above takeoff",
            Self::Pressure => "baro alt - terrain",
        }
    }
}

/// One condition found on a track at one evaluation.
#[derive(Debug, Clone, PartialEq)]
pub struct Cond {
    pub rule: Rule,
    pub area: Option<AreaKind>,
    /// Level as soon as it has held `min_s`.
    pub instant: u8,
    /// Level once it has held `sustain_s`.
    pub sustained: u8,
    pub sustain_s: f32,
    pub min_s: f32,
    /// Evidence, per rule:
    /// - Height: height above ground m, its error m, the limit m
    /// - FacilityMap: height above ground m, ceiling ft, error m
    /// - Controlled / Area: distance to the area's edge m, horizontal accuracy m, 0
    /// - NoRemoteId: 0; NoOperator: 0; SerialChanged: number of changes
    /// - LowAircraft: height above ground m, minimum ft, error m
    /// - Vlos: distance to operator m, operator position type (0 takeoff,
    ///   1 live GNSS, 2 fixed), horizontal accuracy m
    /// - VlosTerrain: terrain above the sight line m, distance m, where (0..1)
    /// - Night: sun elevation deg, 1 night / 0 twilight, 0
    pub ev: [f32; 3],
    pub src: u8,
    pub name: String<24>,
    pub no_clock: bool,
    /// Shown as a flag (false: evidence for the summary only).
    pub show: bool,
    /// For areas: the drone is between floor and ceiling.
    pub inside_band: bool,
    /// Height above ground, metres, when known (for the summary).
    pub agl: Option<f32>,
}

pub type Conds = Vec<Cond, 16>;

fn short(s: &str) -> String<24> {
    let mut out = String::new();
    for ch in s.chars() {
        if out.push(ch).is_err() {
            break;
        }
    }
    out
}

fn cond(
    rule: Rule,
    instant: u8,
    sustained: u8,
    sustain_s: f32,
    min_s: f32,
    ev: [f32; 3],
    src: u8,
) -> Cond {
    Cond {
        rule,
        area: None,
        instant,
        sustained: sustained.max(instant),
        sustain_s,
        min_s,
        ev,
        src,
        name: String::new(),
        no_clock: false,
        show: true,
        inside_band: false,
        agl: None,
    }
}

const H_ACC_UNKNOWN_M: f64 = 30.0;

/// Height above ground of a drone: (metres, 95 % error metres, how, source id).
pub fn drone_agl(t: &Track, inp: &Inputs<'_, '_>) -> Option<(f64, f64, HeightSource, u8)> {
    let s = inp.settings;
    let loc = &t.location.as_ref()?.value;
    let pos = loc.position?;
    let vacc = |a: rid::message::VerticalAccuracy| {
        a.meters()
            .map_or(f64::from(s.vert_acc_unknown_m), f64::from)
    };
    if let (HeightType::AboveGround, Some(h)) = (loc.height_type, loc.height) {
        return Some((
            f64::from(h),
            vacc(loc.baro_accuracy),
            HeightSource::AboveGround,
            src::BROADCAST,
        ));
    }
    let pack = inp.pack;
    if let (Some(alt), Some(g)) = (
        loc.altitude_geodetic,
        pack.and_then(|p| p.ground_hae_m(pos.lat, pos.lon)),
    ) {
        return Some((
            f64::from(alt) - g,
            vacc(loc.vertical_accuracy) + f64::from(s.terrain_err_m),
            HeightSource::Terrain,
            src::TERRAIN,
        ));
    }
    if let (HeightType::AboveTakeoff, Some(h)) = (loc.height_type, loc.height) {
        // Correct for the ground under the drone being higher or lower
        // than where it took off (the operator position, when the module
        // says it is the takeoff point or a live position).
        let op = t.system.as_ref().and_then(|s| s.value.operator_position);
        let corr = match (pack, op) {
            (Some(p), Some(op)) => p
                .ground_msl_m(op.lat, op.lon)
                .zip(p.ground_msl_m(pos.lat, pos.lon))
                .map(|(a, b)| a - b),
            _ => None,
        };
        let err = vacc(loc.baro_accuracy)
            + f64::from(s.takeoff_height_err_m)
            + if corr.is_some() {
                f64::from(s.terrain_err_m)
            } else {
                0.0
            };
        return Some((
            f64::from(h) + corr.unwrap_or(0.0),
            err,
            HeightSource::AboveTakeoff,
            src::BROADCAST,
        ));
    }
    if let (Some(alt), Some(g)) = (
        loc.altitude_pressure,
        pack.and_then(|p| p.ground_msl_m(pos.lat, pos.lon)),
    ) {
        return Some((
            f64::from(alt) - g,
            f64::from(s.baro_err_m + s.terrain_err_m),
            HeightSource::Pressure,
            src::TERRAIN,
        ));
    }
    None
}

/// Every condition on a drone track now, and whether two independent
/// sources (Remote ID and DJI DroneID with the same serial) agree.
/// `serial_changes`: Remote ID serial changes still within their hold time.
pub fn drone_conditions(
    t: &Track,
    inp: &Inputs<'_, '_>,
    serial_changes: Option<u8>,
) -> (Conds, bool) {
    let s = inp.settings;
    let now = inp.now_ms;
    let fresh = s.fresh_ms as u64;
    let mut out = Conds::new();
    let rid_recent = t.slots().any(|(_, l)| l.last_seen_ms + fresh >= now);
    let dji_recent = [t.dji.ocusync, t.dji.wifi]
        .iter()
        .any(|l| l.heard() && l.last_seen_ms + fresh >= now);
    let serial = t
        .uas_ids
        .iter()
        .find(|u| u.id_type == rid::message::IdType::SerialNumber)
        .map(|u| u.id);
    let confirmed = rid_recent && dji_recent && serial.is_some() && serial == t.dji.serial;

    let loc = t
        .location
        .as_ref()
        .filter(|l| l.rx_ms + fresh >= now)
        .map(|l| &l.value);
    let pos = loc.and_then(|l| l.position);
    let on_ground = loc.is_some_and(|l| l.status == Status::Ground);
    let h_acc = loc
        .and_then(|l| l.horizontal_accuracy.meters())
        .map_or(H_ACC_UNKNOWN_M, f64::from);
    let pack = inp.pack;
    let mut in_fria = false;

    if let Some(n) = serial_changes {
        let lvl = if n >= 2 { 2 } else { 1 };
        let _ = out.push(cond(
            Rule::SerialChanged,
            lvl,
            lvl,
            0.0,
            0.0,
            [f32::from(n), 0.0, 0.0],
            src::BROADCAST,
        ));
    }

    if let Some(pos) = pos {
        let agl = drone_agl(t, inp);
        let urban = pack
            .and_then(|p| p.urban(pos.lat, pos.lon))
            .unwrap_or(false);
        // 1. Height above ground.
        if let Some((agl_m, err, how, from)) = agl {
            let limit_m = f64::from(s.agl_limit_ft) * M_PER_FT;
            let lb_ft = (agl_m - err) * FT_PER_M;
            let mut sustained = if lb_ft > f64::from(s.agl_alert_alone_ft) {
                3
            } else if lb_ft > f64::from(s.agl_limit_ft + s.agl_warn_margin_ft) {
                2
            } else {
                1
            };
            let mut name = short(how.name());
            if urban && agl_m * FT_PER_M < f64::from(s.agl_limit_ft + s.structure_allow_ft) {
                sustained = 1;
                name = short("check for structures");
            }
            let mut c = cond(
                Rule::Height,
                1,
                sustained,
                s.agl_warn_s,
                s.agl_watch_s,
                [agl_m as f32, err as f32, limit_m as f32],
                from,
            );
            c.name = name;
            c.show = agl_m > limit_m && !on_ground;
            c.agl = Some(agl_m as f32);
            let _ = out.push(c);
            // 2b. Above the facility-map ceiling.
            if let Some(ceil_ft) = pack.and_then(|p| p.uasfm_ceiling_ft(pos.lat, pos.lon))
                && !on_ground
            {
                let ceil_m = f64::from(ceil_ft) * M_PER_FT;
                if agl_m > ceil_m + f64::from(s.uasfm_margin_m) {
                    let sustained = if agl_m - err > ceil_m { 2 } else { 1 };
                    let _ = out.push(cond(
                        Rule::FacilityMap,
                        1,
                        sustained,
                        s.uasfm_warn_s,
                        2.0,
                        [agl_m as f32, f32::from(ceil_ft), err as f32],
                        src::UAS_FACILITY_MAP,
                    ));
                }
            }
        }
        // 2a, 3. Areas.
        if let Some(p) = pack {
            let geoid = p.geoid_m(pos.lat, pos.lon);
            let ground = p.ground_msl_m(pos.lat, pos.lon);
            let alt_msl = loc
                .and_then(|l| l.altitude_geodetic)
                .zip(geoid)
                .map(|(a, g)| f64::from(a) - g)
                .or_else(|| ground.zip(agl).map(|(g, a)| g + a.0));
            let utc_s = inp.utc_ms.map(|u| (u / 1000) as u32);
            for a in p.areas_at(pos.lat, pos.lon) {
                if a.kind == AreaKind::Fria {
                    in_fria = true;
                    continue;
                }
                if !a.active_at(utc_s) {
                    continue;
                }
                // Unknown altitude: assume low (the drone is most likely
                // under 400 ft), so surface areas count.
                let alt = alt_msl.unwrap_or(ground.unwrap_or(0.0) + 30.0);
                if a.contains_alt(alt, ground) != Some(true) {
                    continue;
                }
                let controlled = a.kind.is_controlled();
                let rule = if controlled {
                    Rule::Controlled
                } else {
                    Rule::Area
                };
                if out.iter().any(|c| c.rule == rule && c.area == Some(a.kind)) {
                    continue;
                }
                let no_clock = utc_s.is_none() && (a.start != 0 || a.end != 0);
                let (mut i, mut su) = match a.kind {
                    k if k.is_controlled() => (0, 0),
                    AreaKind::Stadium => (0, 0),
                    AreaKind::Prohibited
                    | AreaKind::NationalSecurity
                    | AreaKind::Tfr
                    | AreaKind::NationalDefense => (2, 3),
                    _ => (1, 2),
                };
                let edge = a.edge_distance_m(pos.lat, pos.lon);
                if edge < h_acc {
                    i = i.min(1);
                    su = su.min(1);
                }
                if no_clock {
                    su = su.min(2);
                }
                if on_ground {
                    i = i.min(1);
                    su = su.min(1);
                }
                let mut c = cond(
                    rule,
                    i,
                    su,
                    s.area_alert_s,
                    0.0,
                    [edge as f32, h_acc as f32, 0.0],
                    a.source,
                );
                c.area = Some(a.kind);
                c.name = short(a.name);
                c.no_clock = no_clock;
                c.inside_band = true;
                // The floor/ceiling kind is not needed beyond this point.
                let _ = (AltRef::Msl, a.ceil_ref);
                let _ = out.push(c);
            }
        }
        // 6. Distance from the operator, and terrain in the way.
        if let Some(sys) = t.system.as_ref().map(|s| &s.value)
            && let Some(op) = sys.operator_position
        {
            let d = distance_m(pos.lat, pos.lon, op.lat, op.lon);
            let lb = d - h_acc;
            let live = sys.operator_location_type == OperatorLocationType::LiveGnss;
            let op_type = match sys.operator_location_type {
                OperatorLocationType::Takeoff => 0.0,
                OperatorLocationType::LiveGnss => 1.0,
                _ => 2.0,
            };
            if lb >= f64::from(s.vlos_watch_m) {
                let mut su = if lb >= f64::from(s.vlos_warn_m) { 2 } else { 1 };
                if !live && lb < f64::from(s.vlos_alert_m) {
                    su = 1;
                }
                let _ = out.push(cond(
                    Rule::Vlos,
                    1,
                    su,
                    s.vlos_warn_s,
                    3.0,
                    [d as f32, op_type, h_acc as f32],
                    src::BROADCAST,
                ));
            }
            if let Some(p) = pack
                && d > 100.0
            {
                let geoid = p.geoid_m(pos.lat, pos.lon);
                let drone_msl = loc
                    .and_then(|l| l.altitude_geodetic)
                    .zip(geoid)
                    .map(|(a, g)| f64::from(a) - g)
                    .or_else(|| {
                        p.ground_msl_m(pos.lat, pos.lon)
                            .zip(agl)
                            .map(|(g, a)| g + a.0)
                    });
                let op_msl = sys
                    .operator_altitude
                    .zip(geoid)
                    .map(|(a, g)| f64::from(a) - g)
                    .or_else(|| {
                        p.ground_msl_m(op.lat, op.lon)
                            .map(|g| g + f64::from(s.eye_height_m))
                    });
                if let (Some(dm), Some(om)) = (drone_msl, op_msl)
                    && let Some(los) = p.line_of_sight((op.lat, op.lon, om), (pos.lat, pos.lon, dm))
                    && los.worst_m > s.los_block_m
                {
                    let su = if live && !urban { 2 } else { 1 };
                    let _ = out.push(cond(
                        Rule::VlosTerrain,
                        1,
                        su,
                        s.vlos_warn_s,
                        3.0,
                        [los.worst_m, d as f32, los.at],
                        src::TERRAIN,
                    ));
                }
            }
        }
        // 7. Night and twilight.
        if let Some(utc) = inp.utc_ms {
            let (light, el) = sun::light(utc, pos.lat, pos.lon);
            if light != Light::Day {
                let night = light == Light::Night;
                let _ = out.push(cond(
                    Rule::Night,
                    0,
                    0,
                    0.0,
                    0.0,
                    [el as f32, if night { 1.0 } else { 0.0 }, 0.0],
                    src::COMPUTED,
                ));
            }
        }
    }

    // 4. Remote ID problems.
    if dji_recent && !rid_recent && !in_fria && !t.dji.undecoded {
        let _ = out.push(cond(
            Rule::NoRemoteId,
            1,
            2,
            s.no_rid_warn_s,
            s.no_rid_s,
            [0.0; 3],
            src::BROADCAST,
        ));
    }
    if rid_recent && loc.is_some() {
        let op_known = t.system.as_ref().is_some_and(|sy| {
            sy.value.operator_position.is_some()
                && sy.rx_ms + (s.op_missing_s * 1000.0) as u64 >= now
        });
        if !op_known {
            let _ = out.push(cond(
                Rule::NoOperator,
                1,
                1,
                0.0,
                s.op_missing_s,
                [0.0; 3],
                src::BROADCAST,
            ));
        }
    }
    (out, confirmed)
}

/// Conditions on a crewed aircraft (91.119 minimum safe altitudes).
pub fn aircraft_conditions(a: &AircraftTrack, inp: &Inputs<'_, '_>) -> Conds {
    use adsb::Kind;
    let s = inp.settings;
    let mut out = Conds::new();
    let Some(p) = inp.pack else { return out };
    let Some(pos) = a
        .position
        .as_ref()
        .filter(|p| p.rx_ms + 15_000 >= inp.now_ms)
        .map(|p| p.value)
    else {
        return out;
    };
    if !a.airborne
        || matches!(
            a.kind(),
            Kind::Rotorcraft
                | Kind::LighterThanAir
                | Kind::Parachutist
                | Kind::SurfaceVehicle
                | Kind::Obstacle
                | Kind::Uav
                | Kind::Space
                | Kind::Glider
        )
    {
        return out;
    }
    // Away from airports (takeoff and landing are exempt).
    // Heliports do not count: helicopters are not checked at all (91.119(d)).
    let near = p
        .airports()
        .filter(|ap| ap.kind == 0 || ap.kind == 2)
        .any(|ap| {
            let excl = if ap.flags & 1 != 0 {
                s.ac_airport_excl_m
            } else {
                s.ac_private_excl_m
            };
            distance_m(pos.lat, pos.lon, ap.lat, ap.lon) < f64::from(excl)
        });
    if near {
        return out;
    }
    let (agl, err) =
        if let (Some(g), Some(ground)) = (a.gnss_alt_ft, p.ground_hae_m(pos.lat, pos.lon)) {
            (
                f64::from(g) * M_PER_FT - ground,
                f64::from(s.vert_acc_unknown_m),
            )
        } else if let (Some(b), Some(ground)) = (a.baro_alt_ft, p.ground_msl_m(pos.lat, pos.lon)) {
            (f64::from(b) * M_PER_FT - ground, f64::from(s.baro_err_m))
        } else {
            return out;
        };
    let congested = p.urban(pos.lat, pos.lon).unwrap_or(false);
    let min_ft = if congested {
        s.ac_congested_ft
    } else {
        s.ac_other_ft
    };
    let min_m = f64::from(min_ft) * M_PER_FT;
    if agl < min_m {
        let su = if agl + err < min_m { 2 } else { 1 };
        let mut c = cond(
            Rule::LowAircraft,
            1,
            su,
            s.ac_warn_s,
            5.0,
            [agl as f32, min_ft, err as f32],
            src::ADSB,
        );
        c.name = short(if congested {
            "congested (urban)"
        } else {
            "other area"
        });
        c.agl = Some(agl as f32);
        let _ = out.push(c);
    }
    out
}
