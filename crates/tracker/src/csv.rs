//! CSV rows for the SD-card log (O3): one row per track update, with the
//! unit's own position at the time so a log can be replayed or mapped later.
//!
//! Empty field = unknown. Times are UTC ISO-8601 when the hub has GPS time,
//! and always the hub's uptime in ms, so rows stay ordered without a fix.
//!
//! Open FPV links ([`link_row`]) share the file and the columns: their
//! `track` is `L<n>` (a separate id space from drones), `transports` names
//! the system, and the position columns are empty because a sighting has
//! none. `uas_id` and `id_type` carry the link's identifier only when
//! `Config::show_link_ids` is set.
//!
//! ADS-B aircraft ([`aircraft_row`]) share them too: `track` is
//! `ICAO-<hex>`, `uas_id` the callsign (`id_type` `Callsign`), `ua_type` the
//! emitter category, `transports` `Adsb1090`, `Uat978`, `OGN` or `FLARM`, `drone_lat`/
//! `drone_lon`/`drone_alt_m` the aircraft's position and geometric altitude,
//! and the last column, `baro_alt_m`, its pressure altitude. (For drones
//! `baro_alt_m` is the Remote ID pressure altitude, or DJI's barometric
//! altitude for a position from DroneID.)
//!
//! DJI DroneID tracks are drone rows; `transports` adds `DroneId`,
//! `DroneIdO4` or `DjiWifi`. An O4 track with no other ID has its session
//! key ID as `uas_id` with `id_type` `O4Session` (unless DroneID IDs are
//! hidden).
//!
//! RF emitters ([`rf_row`]): `track` is `R<n>`, `uas_id` the frequency in
//! MHz with `id_type` `FreqMHz`, `ua_type` the kind (`AnalogVideo`, ...),
//! `transports` `Rf`, and `rssi_dbm` holds the level **above the noise
//! floor** in dB, not an absolute level.
//!
//! Wi-Fi drones ([`wifi_drone_row`]): `track` is `W<n>`, `uas_id` the
//! network name with `id_type` `Ssid`, `ua_type` the maker, `transports`
//! `WifiDrone` (`WifiDrone5G` on 5 GHz). The address is not logged.

use core::fmt::Write;

use crate::aircraft::{AircraftTrack, IcaoHex};
use crate::json::round_bearing;
use crate::openlinks::{LinkTrack, shown_id};
use crate::signals::{Emitter, WifiDrone};
use crate::text::{Buf, CsvField, FormatError, IdText, IsoTime, render};
use crate::{Band, Context, Track};

/// A buffer this size always holds one row (see the worst-case test).
pub const MAX_ROW: usize = 512;

/// The header line, `\n`-terminated.
pub const HEADER: &str = "utc,hub_ms,track,own_lat,own_lon,own_alt_m,uas_id,id_type,ua_type,\
transports,drone_lat,drone_lon,drone_alt_m,height_m,speed_mps,op_lat,op_lon,rssi_dbm,\
dist_m,brg_deg,op_dist_m,op_brg_deg,baro_alt_m\n";

/// Write [`HEADER`] into `buf`.
pub fn header(buf: &mut [u8]) -> Result<usize, FormatError> {
    let mut w = Buf::new(buf);
    let _ = w.write_str(HEADER);
    w.finish()
}

/// Writes `Some(v)` with `prec` decimals, `None` as an empty field.
struct Opt<T>(Option<T>, usize);

impl<T: Into<f64> + Copy> core::fmt::Display for Opt<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Some(v) => write!(f, "{:.*}", self.1, v.into()),
            None => Ok(()),
        }
    }
}

/// Write one row for `track` (columns as [`HEADER`]), `\n`-terminated.
pub fn row(track: &Track, ctx: &Context, buf: &mut [u8]) -> Result<usize, FormatError> {
    let own = ctx.own;
    let g = ctx.geometry(track);
    let loc = track.location.as_ref().map(|l| &l.value);
    let pos = loc.and_then(|l| l.position);
    let op = track
        .system
        .as_ref()
        .and_then(|s| s.value.operator_position);
    let id = track.primary_id();

    let mut w = Buf::new(buf);
    let _ = (|| -> core::fmt::Result {
        if let Some(utc) = ctx.utc_ms {
            write!(w, "{}", IsoTime(utc))?;
        }
        write!(
            w,
            ",{},{},{},{},{},",
            ctx.now_ms,
            track.id.0,
            Opt(own.map(|o| o.lat), 7),
            Opt(own.map(|o| o.lon), 7),
            Opt(own.and_then(|o| o.alt_m), 1),
        )?;
        if let Some(id) = id {
            let text: heapless::String<48> = render(IdText(&id.id));
            write!(w, "{},{:?}", CsvField(&text), id.id_type)?;
        } else if let Some(k) = track.dji.key_id.filter(|_| !ctx.config.hide_droneid_ids) {
            // An O4 track's only identity: its session key ID.
            write!(w, "{},O4Session", crate::json::key_hex(k))?;
        } else {
            w.write_char(',')?;
        }
        w.write_char(',')?;
        if let Some(ua) = track.ua_type {
            write!(w, "{ua:?}")?;
        }
        w.write_char(',')?;
        // `|` keeps the transport list in one field without quoting. One
        // name per slot, so 5 GHz shows as `WifiBeacon5G` / `WifiNan5G`.
        for (i, name) in track.source_names().enumerate() {
            if i > 0 {
                w.write_char('|')?;
            }
            w.write_str(name)?;
        }
        write!(
            w,
            ",{},{},{},{},{},{},{},",
            Opt(pos.map(|p| p.lat), 7),
            Opt(pos.map(|p| p.lon), 7),
            Opt(loc.and_then(|l| l.altitude_geodetic), 1),
            Opt(loc.and_then(|l| l.height), 1),
            Opt(loc.and_then(|l| l.speed_horizontal), 2),
            Opt(op.map(|p| p.lat), 7),
            Opt(op.map(|p| p.lon), 7),
        )?;
        if let Some(r) = track.latest_rssi() {
            write!(w, "{r}")?;
        }
        writeln!(
            w,
            ",{},{},{},{},{}",
            Opt(g.drone.map(|d| d.distance_m), 1),
            Opt(g.drone.map(|d| round_bearing(d.bearing_deg)), 1),
            Opt(g.operator.map(|d| d.distance_m), 1),
            Opt(g.operator.map(|d| round_bearing(d.bearing_deg)), 1),
            Opt(loc.and_then(|l| l.altitude_pressure), 1),
        )
    })();
    w.finish()
}

/// Write one row for an open link (columns as [`HEADER`]; see the module
/// docs for what each means here), `\n`-terminated.
pub fn link_row(t: &LinkTrack, ctx: &Context, buf: &mut [u8]) -> Result<usize, FormatError> {
    let own = ctx.own;
    let mut w = Buf::new(buf);
    let _ = (|| -> core::fmt::Result {
        if let Some(utc) = ctx.utc_ms {
            write!(w, "{}", IsoTime(utc))?;
        }
        write!(
            w,
            ",{},L{},{},{},{},",
            ctx.now_ms,
            t.id.0,
            Opt(own.map(|o| o.lat), 7),
            Opt(own.map(|o| o.lon), 7),
            Opt(own.and_then(|o| o.alt_m), 1),
        )?;
        // Identifier columns: empty unless the operator opted in.
        if let Some(id) = shown_id(t, ctx) {
            write!(w, "{id},{}", t.system.id_name().unwrap_or(""))?;
        } else {
            w.write_char(',')?;
        }
        // ua_type: none for a link. transports: the system, once per band
        // heard, `5G` appended for 5 GHz (as the Wi-Fi Remote ID slots).
        w.write_str(",,")?;
        let name = system_name(t.system);
        let mut first = true;
        for band in [Band::Ghz24, Band::Ghz5] {
            if t.streams.iter().any(|s| s.band() == band) {
                if !first {
                    w.write_char('|')?;
                }
                first = false;
                w.write_str(name)?;
                if band == Band::Ghz5 {
                    w.write_str("5G")?;
                }
            }
        }
        // No drone position, speed or operator: seven empty fields.
        w.write_str(",,,,,,,,")?;
        if let Some(s) = t.latest() {
            write!(w, "{}", s.rssi)?;
        }
        // No range, bearing or altitude either.
        w.write_str(",,,,,\n")
    })();
    w.finish()
}

/// Write one row for an ADS-B aircraft (columns as [`HEADER`]; see the
/// module docs for what each means here), `\n`-terminated.
pub fn aircraft_row(
    a: &AircraftTrack,
    ctx: &Context,
    buf: &mut [u8],
) -> Result<usize, FormatError> {
    let own = ctx.own;
    let g = ctx.aircraft_geometry(a);
    let pos = a.position.as_ref().map(|p| p.value);
    let ft = |v: Option<i32>| v.map(|f| f64::from(f) * 0.3048);
    let mut w = Buf::new(buf);
    let _ = (|| -> core::fmt::Result {
        if let Some(utc) = ctx.utc_ms {
            write!(w, "{}", IsoTime(utc))?;
        }
        write!(
            w,
            ",{},ICAO-{},{},{},{},",
            ctx.now_ms,
            IcaoHex(a.key.icao),
            Opt(own.map(|o| o.lat), 7),
            Opt(own.map(|o| o.lon), 7),
            Opt(own.and_then(|o| o.alt_m), 1),
        )?;
        if a.callsign.is_empty() {
            w.write_char(',')?;
        } else {
            write!(w, "{},Callsign", CsvField(a.callsign.as_str()))?;
        }
        write!(
            w,
            ",{},{},{},{},{},,{},,,",
            a.kind().name(),
            match a.key.source {
                adsb::Source::ModeS => "Adsb1090",
                adsb::Source::Uat => "Uat978",
                adsb::Source::Ogn => "OGN",
                adsb::Source::Flarm => "FLARM",
            },
            Opt(pos.map(|p| p.lat), 7),
            Opt(pos.map(|p| p.lon), 7),
            Opt(ft(a.gnss_alt_ft), 1),
            Opt(a.speed_mps(), 2),
        )?;
        writeln!(
            w,
            "{},{},{},,,{}",
            a.rssi_dbm,
            Opt(g.range.map(|r| r.distance_m), 1),
            Opt(g.range.map(|r| round_bearing(r.bearing_deg)), 1),
            Opt(ft(a.baro_alt_ft), 1),
        )
    })();
    w.finish()
}

/// The front columns every row shares: UTC, hub time, track, own position.
fn front(w: &mut Buf<'_>, ctx: &Context, track: core::fmt::Arguments<'_>) -> core::fmt::Result {
    let own = ctx.own;
    if let Some(utc) = ctx.utc_ms {
        write!(w, "{}", IsoTime(utc))?;
    }
    write!(
        w,
        ",{},{},{},{},{},",
        ctx.now_ms,
        track,
        Opt(own.map(|o| o.lat), 7),
        Opt(own.map(|o| o.lon), 7),
        Opt(own.and_then(|o| o.alt_m), 1),
    )
}

/// Write one row for an RF emitter (see the module docs), `\n`-terminated.
pub fn rf_row(e: &Emitter, ctx: &Context, buf: &mut [u8]) -> Result<usize, FormatError> {
    let mut w = Buf::new(buf);
    let _ = (|| -> core::fmt::Result {
        front(&mut w, ctx, format_args!("R{}", e.id.0))?;
        write!(w, "{:.1},FreqMHz,{:?},Rf", e.freq_mhz(), e.kind)?;
        // No position, speed or operator: seven empty fields.
        write!(w, ",,,,,,,,{}", e.snr_db)?;
        w.write_str(",,,,,\n")
    })();
    w.finish()
}

/// Write one row for a Wi-Fi drone (see the module docs), `\n`-terminated.
pub fn wifi_drone_row(d: &WifiDrone, ctx: &Context, buf: &mut [u8]) -> Result<usize, FormatError> {
    let mut w = Buf::new(buf);
    let _ = (|| -> core::fmt::Result {
        front(&mut w, ctx, format_args!("W{}", d.id.0))?;
        write!(
            w,
            "{},Ssid,{},WifiDrone{}",
            CsvField(d.ssid.as_str()),
            CsvField(d.maker_name()),
            if d.band() == Band::Ghz5 { "5G" } else { "" }
        )?;
        write!(w, ",,,,,,,,{}", d.rssi)?;
        w.write_str(",,,,,\n")
    })();
    w.finish()
}

/// The system's name as in JSON (`WfbNg`, `OpenHd`, `RubyFpv`, `DroneBridge`).
fn system_name(s: openlink::System) -> &'static str {
    match s {
        openlink::System::WfbNg => "WfbNg",
        openlink::System::OpenHd => "OpenHd",
        openlink::System::RubyFpv => "RubyFpv",
        openlink::System::DroneBridge => "DroneBridge",
    }
}
