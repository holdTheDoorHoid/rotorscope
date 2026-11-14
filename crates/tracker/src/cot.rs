//! TAK Cursor-on-Target events for a drone and its operator (O5).
//!
//! The shape follows DragonSync (WarDragon's TAK bridge, Apache-2.0,
//! <https://github.com/alphafox02/DragonSync>, `utils/cot_builder.py` and
//! `core/drone.py`), because that is what ATAK users already see for Remote
//! ID drones: uid `drone-<UAS ID>` with the same string as callsign, operator
//! uid `pilot-<UAS ID>` as a `b-m-p-s-m` spot map point with the civilian
//! "Person" icon, `how="m-g"`, `<precisionlocation>`, `<track>`, a
//! semicolon-separated `<remarks>` summary and `<color argb="-256"/>`
//! (yellow). No DragonSync code is copied; only its conventions.
//!
//! Where we differ, and why:
//! - Fixed-wing aircraft get `a-u-A-M-F-Q` (unknown-affiliation fixed-wing
//!   UAV) instead of DragonSync's `a-f-A-f`, which marks them *friendly*: a
//!   detector has no basis for that.
//! - `ce`/`le` carry the drone's declared accuracy when it sends one, and
//!   the CoT "unknown" value 9999999.0 otherwise, instead of a fixed 35 m.
//! - `time`/`start` are when the position was received, not when the event
//!   was written, and `stale` is that plus twice the track's stale time.
//! - One event per line with no pretty-printing, to fit one UDP datagram.

use core::fmt::Write;

use crate::aircraft::{AIRCRAFT_STALE_MS, AircraftTrack, IcaoHex, same_drone};
use crate::json::Octal4;
use crate::text::{Buf, FormatError, IdText, IsoTime, Mac, Xml, render};
use crate::{Context, Track};
use rid::message::UaType;

/// CoT's "unknown" for `hae`, `ce` and `le`.
const UNKNOWN: f64 = 9_999_999.0;

/// The CoT type for a drone, by its Basic ID UA type.
pub fn drone_type(ua: Option<UaType>) -> &'static str {
    match ua {
        Some(UaType::Aeroplane | UaType::Ornithopter | UaType::Glider) => "a-u-A-M-F-Q",
        // Lighter-than-air, tethered and ground objects: a spot map point,
        // as DragonSync does, since no 2525 air symbol fits.
        Some(
            UaType::Kite
            | UaType::FreeBalloon
            | UaType::CaptiveBalloon
            | UaType::Airship
            | UaType::FreeFallParachute
            | UaType::Rocket
            | UaType::TetheredPoweredAircraft
            | UaType::GroundObstacle
            | UaType::Other,
        ) => "b-m-p-s-m",
        // Multirotors are nearly every Remote ID drone; unknown types default
        // here too, matching DragonSync's fallback.
        _ => "a-u-A-M-H-R",
    }
}

/// A buffer this size always holds one event (see the worst-case test).
pub const MAX_EVENT: usize = 2048;

/// The CoT type for an operator.
pub const OPERATOR_TYPE: &str = "b-m-p-s-m";

/// The part after `drone-` / `pilot-`: the primary UAS ID, or the latest MAC
/// (without colons) for a track that has not sent a Basic ID yet, or
/// `dji-t<track id>` for a DJI DroneID track with neither (an O4 or
/// undecoded presence track, or a hidden serial).
pub fn uid_base(track: &Track) -> heapless::String<48> {
    match track.primary_id() {
        Some(id) => render(IdText(&id.id)),
        None if track.latest_link().is_none() => {
            let mut out = heapless::String::new();
            let _ = write!(out, "dji-t{}", track.id.0);
            out
        }
        None => {
            let mac = track.latest_link().map(|(_, l)| l.mac).unwrap_or_default();
            let mut out = heapless::String::new();
            let _ = out.push_str("mac-");
            for b in mac {
                let _ = write!(out, "{b:02x}");
            }
            out
        }
    }
}

fn header(w: &mut Buf<'_>, uid: &str, kind: &str, time: u64, stale: u64) -> core::fmt::Result {
    write!(
        w,
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <event version=\"2.0\" uid=\"{}\" type=\"{kind}\" how=\"m-g\" \
         time=\"{}\" start=\"{}\" stale=\"{}\">",
        Xml(uid),
        IsoTime(time),
        IsoTime(time),
        IsoTime(stale)
    )
}

fn point(
    w: &mut Buf<'_>,
    lat: f64,
    lon: f64,
    hae: Option<f32>,
    ce: Option<f32>,
    le: Option<f32>,
) -> core::fmt::Result {
    let or = |v: Option<f32>| v.map_or(UNKNOWN, f64::from);
    write!(
        w,
        "<point lat=\"{lat:.7}\" lon=\"{lon:.7}\" hae=\"{:.1}\" ce=\"{:.1}\" le=\"{:.1}\"/>",
        or(hae),
        or(ce),
        or(le)
    )
}

/// Write the drone's CoT event. Needs a Location with a position and UTC.
pub fn drone_event(track: &Track, ctx: &Context, buf: &mut [u8]) -> Result<usize, FormatError> {
    let loc = track.location.as_ref().ok_or(FormatError::NoPosition)?;
    let pos = loc.value.position.ok_or(FormatError::NoPosition)?;
    let time = ctx.utc_of(loc.rx_ms).ok_or(FormatError::NoTime)?;
    let stale = time + 2 * ctx.config.stale_after_ms;
    let l = &loc.value;
    let mut uid: heapless::String<56> = heapless::String::new();
    let _ = write!(uid, "drone-{}", uid_base(track));

    let mut w = Buf::new(buf);
    let _ = (|| -> core::fmt::Result {
        header(&mut w, &uid, drone_type(track.ua_type), time, stale)?;
        point(
            &mut w,
            pos.lat,
            pos.lon,
            l.altitude_geodetic,
            l.horizontal_accuracy.meters(),
            l.vertical_accuracy.meters(),
        )?;
        write!(
            w,
            "<detail><contact callsign=\"{}\"/>\
             <precisionlocation geopointsrc=\"gps\" altsrc=\"{}\"/>",
            Xml(&uid),
            if l.altitude_geodetic.is_some() {
                "gps"
            } else {
                "???"
            }
        )?;
        if l.direction.is_some() || l.speed_horizontal.is_some() {
            write!(
                w,
                "<track course=\"{:.1}\" speed=\"{:.2}\"/>",
                l.direction.unwrap_or(0.0),
                l.speed_horizontal.unwrap_or(0.0)
            )?;
        }
        w.write_str("<remarks>")?;
        remarks(&mut w, track, ctx)?;
        w.write_str("</remarks><color argb=\"-256\"/></detail></event>")
    })();
    w.finish()
}

/// `Key: value; ` pairs in DragonSync's order, skipping unknowns.
fn remarks(w: &mut Buf<'_>, track: &Track, ctx: &Context) -> core::fmt::Result {
    if let Some((_, link)) = track.latest_link() {
        write!(w, "MAC: {}, RSSI: {}dBm", Mac(&link.mac), link.rssi)?;
    } else if let Some(r) = track.latest_rssi() {
        write!(w, "RSSI: {r}dBm")?;
    }
    if let Some(m) = track.dji.product_name() {
        write!(w, "; DJI Model: {}", Xml(m))?;
    }
    if let Some(f) = track.dji.freq_mhz() {
        write!(w, "; DroneID: {f:.1} MHz")?;
    }
    if let Some(id) = track.primary_id() {
        write!(w, "; ID Type: {:?}", id.id_type)?;
    }
    if let Some(ua) = track.ua_type {
        write!(w, "; UA Type: {ua:?} ({})", ua.code())?;
    }
    if let Some(op) = &track.operator_id {
        let text: heapless::String<48> = render(IdText(&op.value.operator_id));
        write!(w, "; Operator ID: {}", Xml(&text))?;
    }
    if let Some(l) = &track.location {
        let l = &l.value;
        if let Some(v) = l.speed_horizontal {
            write!(w, "; Speed: {v} m/s")?;
        }
        if let Some(v) = l.speed_vertical {
            write!(w, "; Vert Speed: {v} m/s")?;
        }
        if let Some(v) = l.altitude_geodetic {
            write!(w, "; Altitude: {v} m")?;
        }
        if let Some(v) = l.height {
            write!(w, "; Height: {v} m {:?}", l.height_type)?;
        }
        if let Some(v) = l.direction {
            write!(w, "; Course: {v}°")?;
        }
        write!(w, "; Status: {:?}", l.status)?;
    }
    if let Some(s) = &track.self_id {
        let text: heapless::String<48> = render(IdText(&s.value.description));
        write!(w, "; Description: {}", Xml(&text))?;
    }
    w.write_str("; Transport: ")?;
    // One name per slot, so a 5 GHz frame reads `WifiBeacon5G`; then the
    // DJI sources (`DroneId`, `DroneIdO4`, `DjiWifi`).
    for (i, name) in track.source_names().enumerate() {
        if i > 0 {
            w.write_char('+')?;
        }
        w.write_str(name)?;
    }
    if let Some(g) = ctx.geometry(track).drone {
        write!(
            w,
            "; Range: {:.0} m; Bearing: {:.0}°",
            g.distance_m, g.bearing_deg
        )?;
    }
    Ok(())
}

/// Write the operator's CoT event. Needs a System message with an operator
/// position, and UTC.
pub fn operator_event(track: &Track, ctx: &Context, buf: &mut [u8]) -> Result<usize, FormatError> {
    let sys = track.system.as_ref().ok_or(FormatError::NoPosition)?;
    let pos = sys.value.operator_position.ok_or(FormatError::NoPosition)?;
    let time = ctx.utc_of(sys.rx_ms).ok_or(FormatError::NoTime)?;
    let stale = time + 2 * ctx.config.stale_after_ms;
    let base = uid_base(track);
    let mut uid: heapless::String<56> = heapless::String::new();
    let _ = write!(uid, "pilot-{base}");

    let mut w = Buf::new(buf);
    let _ = (|| -> core::fmt::Result {
        header(&mut w, &uid, OPERATOR_TYPE, time, stale)?;
        point(
            &mut w,
            pos.lat,
            pos.lon,
            sys.value.operator_altitude,
            None,
            None,
        )?;
        write!(
            w,
            "<detail><contact callsign=\"{}\"/>\
             <precisionlocation geopointsrc=\"gps\" altsrc=\"{}\"/>\
             <usericon iconsetpath=\"com.atakmap.android.maps.public/Civilian/Person.png\"/>\
             <remarks>Pilot location for drone drone-{}; Source: {:?}</remarks>\
             </detail></event>",
            Xml(&uid),
            if sys.value.operator_altitude.is_some() {
                "gps"
            } else {
                "???"
            },
            Xml(&base),
            sys.value.operator_location_type
        )
    })();
    w.finish()
}

// --------------------------------------------------------------- aircraft --

/// The CoT type for an ADS-B aircraft, by emitter category: neutral
/// (`n`) civilian air, since ADS-B is broadcast by cooperative civil
/// traffic and a detector has no basis for any other affiliation. Rotary
/// `a-n-A-C-H`, lighter-than-air `a-n-A-C-L`, fixed wing (and anything
/// unknown) `a-n-A-C-F`. A UAV that carries ADS-B gets the same
/// unknown-affiliation UAV type as a Remote ID drone; ground vehicles
/// `a-n-G-E-V`; obstacles a spot map point.
pub fn aircraft_type(kind: adsb::Kind) -> &'static str {
    use adsb::Kind;
    match kind {
        Kind::Rotorcraft => "a-n-A-C-H",
        Kind::LighterThanAir => "a-n-A-C-L",
        Kind::Uav => "a-u-A-M-F-Q",
        Kind::SurfaceVehicle => "a-n-G-E-V",
        Kind::Obstacle => "b-m-p-s-m",
        _ => "a-n-A-C-F",
    }
}

/// Write an ADS-B aircraft's CoT event: uid `ICAO-<hex>` (the convention
/// ADS-B-to-TAK bridges use), callsign the flight ID if known. Needs a
/// position and UTC. `drones` is the drone table: an ADS-B "UAV" that is
/// one of those drones ([`crate::aircraft::same_drone`]) is not written
/// ([`FormatError::Duplicate`]); the drone's own event already shows it.
pub fn aircraft_event(
    a: &AircraftTrack,
    drones: &[Track],
    ctx: &Context,
    buf: &mut [u8],
) -> Result<usize, FormatError> {
    let pos = a.position.as_ref().ok_or(FormatError::NoPosition)?;
    if same_drone(a, drones, ctx).is_some() {
        return Err(FormatError::Duplicate);
    }
    let time = ctx.utc_of(pos.rx_ms).ok_or(FormatError::NoTime)?;
    let stale = time + 2 * AIRCRAFT_STALE_MS;
    let mut uid: heapless::String<12> = heapless::String::new();
    let _ = write!(uid, "ICAO-{}", IcaoHex(a.key.icao));
    let call = if a.callsign.is_empty() {
        uid.as_str()
    } else {
        a.callsign.as_str()
    };
    // `hae` should be above the ellipsoid: geometric altitude when sent,
    // else pressure altitude (close to above sea level), flagged as such.
    let (hae, altsrc) = match (a.gnss_alt_ft, a.baro_alt_ft) {
        (Some(g), _) => (Some(g as f32 * 0.3048), "gps"),
        (None, Some(b)) => (Some(b as f32 * 0.3048), "baro"),
        _ => (None, "???"),
    };

    let mut w = Buf::new(buf);
    let _ = (|| -> core::fmt::Result {
        header(&mut w, &uid, aircraft_type(a.kind()), time, stale)?;
        point(&mut w, pos.value.lat, pos.value.lon, hae, None, None)?;
        write!(
            w,
            "<detail><contact callsign=\"{}\"/>\
             <precisionlocation geopointsrc=\"gps\" altsrc=\"{altsrc}\"/>",
            Xml(call),
        )?;
        if a.direction_deg.is_some() || a.speed_kt.is_some() {
            write!(
                w,
                "<track course=\"{:.1}\" speed=\"{:.2}\"/>",
                a.direction_deg.unwrap_or(0.0),
                a.speed_mps().unwrap_or(0.0)
            )?;
        }
        write!(w, "<remarks>ADS-B {}", IcaoHex(a.key.icao))?;
        if a.key.source == adsb::Source::Uat {
            w.write_str(" (UAT)")?;
        }
        write!(w, "; {}", a.kind().label())?;
        if let Some(s) = a.squawk {
            write!(w, "; Squawk: {}", Octal4(s))?;
        }
        if let Some(e) = a.emergency() {
            write!(w, " ({e})")?;
        }
        if let Some(b) = a.baro_alt_ft {
            write!(w, "; Altitude: {b} ft")?;
        }
        if let Some(v) = a.vrate_fpm {
            write!(w, "; Vert rate: {v} ft/min")?;
        }
        if let Some(k) = a.speed_kt {
            write!(w, "; Speed: {k} kt")?;
        }
        write!(w, "; RSSI: {}dBm", a.rssi_dbm)?;
        if let Some(r) = ctx.aircraft_geometry(a).range {
            write!(
                w,
                "; Range: {:.0} m; Bearing: {:.0}°",
                r.distance_m, r.bearing_deg
            )?;
        }
        w.write_str("</remarks></detail></event>")
    })();
    w.finish()
}
