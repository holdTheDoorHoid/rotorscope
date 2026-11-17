//! The 1 Hz JSON lines on the console (PLAN.md O4), as the S3 hub's USB
//! stream (`firmware/hub/src/tick.rs`): `{"t":"new"}` per new drone, one
//! `{"t":"track"}` per drone track, `{"t":"link"}` per open FPV link,
//! `{"t":"aircraft"}` per ADS-B aircraft (the tracker's own formatters, so
//! byte-for-byte the S3 format), then one `{"t":"hub"}` status line, which
//! is P4-specific (this hub's links, GPS module, SD and console figures).
//!
//! The console runs at 115200 baud (~11.5 KB/s, shared with the log), so the
//! hub line goes first and the per-track lines after it; whatever does not
//! fit in the console ring is dropped whole and counted in `console_drops`.

use core::fmt::Write as _;

use tracker::json::{self, GpsState, MAX_LINE};

use crate::adsbee::Adsbee;
use crate::app::{App, OwnSource};
use crate::console::{self, LineBuf};
use crate::gps::{self, Gps};
use crate::links::GatewayLink;
use crate::sd::Logger;

struct W<'a>(&'a mut LineBuf);

impl W<'_> {
    fn opt<T: core::fmt::Display>(&mut self, v: Option<T>) {
        let _ = match v {
            Some(v) => write!(self.0, "{v}"),
            None => write!(self.0, "null"),
        };
    }
}

pub fn tick(app: &mut App, gw: &GatewayLink<'_>, gps: &Gps, adsbee: &Adsbee, sd: &Logger, faa: &crate::faa::Faa, now: u64) {
    let utc = gps.utc_ms(now);
    while let Some(id) = app.new_drones.pop_front() {
        let mut b = LineBuf::new();
        let _ = writeln!(b, "{{\"t\":\"new\",\"id\":{id},\"up_ms\":{now}}}");
        console::json(&b.buf[..b.len]);
    }
    hub_line(app, gw, gps, adsbee, sd, now, utc);

    let ctx = app.tracker.context(now, utc);
    let mut line = [0u8; MAX_LINE];
    for t in app.tracker.tracks() {
        if let Ok(n) = json::track_line(t, &ctx, &mut line)
            && !console::json(&line[..n])
        {
            return;
        }
    }
    for l in app.tracker.open_links() {
        if let Ok(n) = json::link_line(l, &ctx, &mut line)
            && !console::json(&line[..n])
        {
            return;
        }
    }
    for a in app.tracker.aircraft() {
        if let Ok(n) = json::aircraft_line(a, app.tracker.tracks(), &ctx, &mut line)
            && !console::json(&line[..n])
        {
            return;
        }
    }
    // Protocol 9: one {"t":"threat"} line per attack or jamming entry.
    let mut tl = [0u8; tracker::threats::MAX_THREAT_LINE];
    for t in app.tracker.threats() {
        if let Ok(n) = tracker::threats::threat_line(t, &ctx, &mut tl)
            && !console::json(&tl[..n])
        {
            return;
        }
    }
    // Phase 12: one {"t":"flags"} line per flagged drone or aircraft (only
    // whole lines: one that does not fit the buffer is skipped).
    for e in faa.engine.entries().iter().filter(|e| !e.flags.is_empty()) {
        let mut s: heapless::String<3072> = heapless::String::new();
        if faarules::text::write_json(&mut s, e, faa.pack()).is_ok() && !console::json(s.as_bytes()) {
            return;
        }
    }
}

fn hub_line(app: &App, gw: &GatewayLink<'_>, gps: &Gps, adsbee: &Adsbee, sd: &Logger, now: u64, utc: Option<u64>) {
    let mut b = LineBuf::new();
    let mut w = W(&mut b);
    let t = &app.tracker;
    let _ = write!(w.0, "{{\"t\":\"hub\",\"up_ms\":{now},\"utc_ms\":");
    w.opt(utc);
    let src = gps.time.map(|a| if a.pps { "\"gps_pps\"" } else { "\"gps\"" });
    let _ = write!(
        w.0,
        ",\"utc_src\":{},\"threats\":{},\"blind\":{},\"tracks\":{},\"active\":{},\"open_links\":{},\"open_active\":{},\"aircraft\":{},\"aircraft_active\":{},\"own\":",
        src.unwrap_or("null"),
        t.active_threats(now).count(),
        t.partly_blind(now),
        t.tracks().len(),
        t.active_count(now),
        t.open_links().len(),
        t.active_link_count(now),
        t.aircraft().len(),
        t.active_aircraft_count(now)
    );
    match app.own_pos(gps, now) {
        Some(p) => {
            let _ = write!(w.0, "{{\"lat\":{:.7},\"lon\":{:.7},\"alt\":", p.lat, p.lon);
            w.opt(p.alt.map(|a| (a * 10.0) as i64 as f64 / 10.0));
            let _ = write!(
                w.0,
                ",\"source\":\"{}\",\"age_ms\":{}}}",
                if p.source == OwnSource::Gps { "gps" } else { "manual" },
                now.saturating_sub(p.at_ms)
            );
        }
        None => {
            let _ = write!(w.0, "null");
        }
    }
    let state = match gps.status.state {
        GpsState::Absent => "absent",
        GpsState::Searching => "searching",
        GpsState::Fix2d => "2d",
        GpsState::Fix3d => "3d",
    };
    let st = gps.stats();
    let sky = gps.sky();
    let now_us = esp_hal::time::Instant::now().duration_since_epoch().as_micros();
    let _ = write!(w.0, ",\"gps\":{{\"state\":\"{state}\",\"sats\":");
    w.opt(gps.status.sats);
    let _ = write!(
        w.0,
        ",\"module\":{}{}{},\"ver\":\"{}\",\"baud\":{},\"cfg\":\"{}\",\"frames\":{},\"csum_err\":{},\"in_view\":{},\"used\":{},\"dual_band\":{},\"pps_n\":{},\"pps_age_ms\":",
        if gps.module.is_some() { "\"" } else { "" },
        gps.module.map_or("null", |m| m.name()),
        if gps.module.is_some() { "\"" } else { "" },
        gps.version,
        gps.baud(),
        gps.cfg.name(),
        st.frames_ok,
        st.checksum_errors,
        sky.total_in_view(),
        sky.total_used(),
        sky.dual_band(),
        gps::PPS_COUNT.load(core::sync::atomic::Ordering::Relaxed),
    );
    w.opt(gps::pps_last_us().map(|t| now_us.saturating_sub(t) / 1000));
    let _ = write!(w.0, ",\"fix_age_ms\":");
    w.opt(gps.fix.map(|f| now.saturating_sub(f.at_ms)));
    let a = adsbee.status(now);
    let _ = write!(w.0, "}},\"adsb\":{{\"alive\":{},\"age_ms\":", a.alive);
    w.opt(a.age_ms);
    let _ = write!(
        w.0,
        ",\"bytes\":{},\"lines\":{},\"errors\":{},\"other\":{},\"uart_err\":{},\"ring_full\":{},\"valid_ps\":",
        adsbee.bytes(),
        adsbee.lines,
        adsbee.errors,
        adsbee.other,
        crate::uartring::adsb_rx::UART_ERR.load(core::sync::atomic::Ordering::Relaxed),
        crate::uartring::adsb_rx::RING_FULL.load(core::sync::atomic::Ordering::Relaxed),
    );
    w.opt(a.frames_per_s);
    let _ = write!(w.0, ",\"table\":");
    w.opt(a.aircraft);
    #[cfg(feature = "modes")]
    {
        let m = &adsbee.raw.table.stats;
        let _ = write!(
            w.0,
            ",\"modes\":{{\"frames\":{},\"used\":{},\"bad_crc\":{},\"fixed\":[{},{}],\"unconfirmed\":{},\"pos\":[{},{}],\"pos_refused\":{},\"tisb\":{},\"adsr\":{},\"aircraft\":{}}}",
            m.frames, m.used, m.bad_crc, m.fixed_1, m.fixed_2, m.unconfirmed, m.positions_global, m.positions_local,
            m.positions_refused, m.tis_b, m.ads_r, adsbee.raw.table.len()
        );
    }
    let s = &gw.stats;
    let o = &app.o4.stats;
    let _ = write!(
        w.0,
        "}},\"o4\":{{\"key\":\"{}\",\"sessions\":{},\"unwrapped\":{},\"decrypted\":{},\"no_session\":{},\"rejected\":{}",
        if app.o4.has_key() { "loaded" } else { sd.o4_key_state },
        app.o4.sessions().count(),
        o.sessions_unwrapped,
        o.records_decrypted,
        o.no_session,
        o.rejected,
    );
    let _ = writeln!(
        w.0,
        "}},\"sd\":{{\"state\":\"{}\",\"file\":\"{}\",\"rows\":{},\"dropped\":{},\"errors\":{}}},\"link\":{{\"frames\":{},\"corrupt\":{},\"uart_err\":{},\"ring_full\":{},\"nodes\":{},\"gw\":\"{}\"}},\"console_drops\":{}}}",
        sd.state_name(),
        sd.file_name(),
        sd.rows,
        sd.dropped,
        sd.errors,
        s.frames,
        s.corrupt,
        s.uart_err,
        s.ring_full,
        gw.nodes.len(),
        s.gw_hello,
        console::DROPS.load(core::sync::atomic::Ordering::Relaxed),
    );
    console::json(&b.buf[..b.len]);
}
