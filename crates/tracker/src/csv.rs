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

use core::fmt::Write;

use crate::json::round_bearing;
use crate::openlinks::{LinkTrack, shown_id};
use crate::text::{Buf, CsvField, FormatError, IdText, IsoTime, render};
use crate::{Band, Context, Track};

/// A buffer this size always holds one row (see the worst-case test).
pub const MAX_ROW: usize = 512;

/// The header line, `\n`-terminated.
pub const HEADER: &str = "utc,hub_ms,track,own_lat,own_lon,own_alt_m,uas_id,id_type,ua_type,\
transports,drone_lat,drone_lon,drone_alt_m,height_m,speed_mps,op_lat,op_lon,rssi_dbm,\
dist_m,brg_deg,op_dist_m,op_brg_deg\n";

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
        for (i, (s, _)) in track.slots().enumerate() {
            if i > 0 {
                w.write_char('|')?;
            }
            w.write_str(s.name())?;
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
        if let Some((_, l)) = track.latest_link() {
            write!(w, "{}", l.rssi)?;
        }
        writeln!(
            w,
            ",{},{},{},{}",
            Opt(g.drone.map(|d| d.distance_m), 1),
            Opt(g.drone.map(|d| round_bearing(d.bearing_deg)), 1),
            Opt(g.operator.map(|d| d.distance_m), 1),
            Opt(g.operator.map(|d| round_bearing(d.bearing_deg)), 1),
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
        // No range or bearing either.
        w.write_str(",,,,\n")
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
