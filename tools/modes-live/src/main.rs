//! `modes-live`: check our Mode S / ADS-B decoder (`crates/modes`) against
//! the ADSBee's own decode of the same aircraft (PLAN.md Phase 14a).
//!
//! The ADSBee's USB console carries one output protocol at a time, so the
//! live run alternates: a few seconds of **Beast** (raw frames, decoded by
//! us), then about a second and a half of **CSBee** (the ADSBee's summary,
//! one line per aircraft). Each CSBee line is compared with what our table
//! says about that aircraft at that moment: address, callsign, squawk,
//! category, altitude and position (allowing for how far the aircraft can
//! have moved since our last update).
//!
//! ```text
//! modes-live live PORT [--secs 600] [--own-pos LAT,LON] [--record FILE] [--log FILE]
//! modes-live replay FILE [--own-pos LAT,LON] [--log FILE]
//! modes-live export FILE OUT [SECS]  # Beast frames + timing, for the P4 loop-back test
//! modes-live check-export OUT         # what our decoder makes of an export
//! ```
//!
//! **What it changes on the ADSBee:** only the running console output
//! protocol (`AT+PROTOCOL_OUT=CONSOLE,...`). It reads the setting first and
//! puts it back at the end (also on Ctrl-C). It never sends
//! `AT+SETTINGS=SAVE` and never touches Wi-Fi or COMMS_UART.
//!
//! **Recording format** (`--record`): records of `f64` Unix time (LE),
//! one byte of mode (1 = the console was in Beast, 2 = CSBee, 0 = other),
//! `u32` length (LE), then the bytes as read.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use modes::table::{Config, Table, distance_m};
use modes::{BeastReader, Received};

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigint(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

fn usage() -> ! {
    eprintln!(
        "usage:\n  modes-live live PORT [--secs N] [--beast-s S] [--csbee-s S] [--own-pos LAT,LON] [--record FILE] [--log FILE]\n  modes-live replay FILE [--own-pos LAT,LON] [--log FILE]\n  modes-live export FILE OUT [SECS]\n  modes-live check-export OUT"
    );
    std::process::exit(2)
}

struct Opts {
    secs: f64,
    beast_s: f64,
    csbee_s: f64,
    own: Option<(f64, f64)>,
    record: Option<String>,
    log: Option<String>,
}

fn parse_opts(args: &[String]) -> Opts {
    let mut o = Opts { secs: 600.0, beast_s: 4.0, csbee_s: 1.6, own: None, record: None, log: None };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().unwrap_or_else(|| usage());
        match a.as_str() {
            "--secs" => o.secs = val().parse().unwrap_or_else(|_| usage()),
            "--beast-s" => o.beast_s = val().parse().unwrap_or_else(|_| usage()),
            "--csbee-s" => o.csbee_s = val().parse().unwrap_or_else(|_| usage()),
            "--own-pos" => {
                let v = val();
                let (a, b) = v.split_once(',').unwrap_or_else(|| usage());
                o.own = Some((a.trim().parse().unwrap_or_else(|_| usage()), b.trim().parse().unwrap_or_else(|_| usage())));
            }
            "--record" => o.record = Some(val()),
            "--log" => o.log = Some(val()),
            _ => usage(),
        }
    }
    o
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("live") if args.len() >= 2 => {
            let o = parse_opts(&args[2..]);
            if let Err(e) = live(&args[1], &o) {
                eprintln!("modes-live: {e}");
                std::process::exit(1);
            }
        }
        Some("replay") if args.len() >= 2 => {
            let o = parse_opts(&args[2..]);
            let recs = read_recording(&args[1]).unwrap_or_else(|e| {
                eprintln!("modes-live: {}: {e}", args[1]);
                std::process::exit(1)
            });
            let mut c = Compare::new(o.own, o.log.as_deref());
            for r in &recs {
                c.feed(r.t, &r.bytes);
            }
            c.finish();
        }
        Some("export") if args.len() == 3 || args.len() == 4 => {
            let secs = args.get(3).map_or(f64::MAX, |s| s.parse().unwrap_or_else(|_| usage()));
            if let Err(e) = export(&args[1], &args[2], secs) {
                eprintln!("modes-live: {e}");
                std::process::exit(1);
            }
        }
        Some("check-export") if args.len() == 2 => {
            if let Err(e) = check_export(&args[1]) {
                eprintln!("modes-live: {e}");
                std::process::exit(1);
            }
        }
        _ => usage(),
    }
}

// ------------------------------------------------------------------ serial

fn raw_mode(file: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let fd = file.as_raw_fd();
    // SAFETY: termios calls on a descriptor we own; `t` is filled by
    // tcgetattr before use.
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(fd, &mut t) != 0 {
            return Err(io::Error::last_os_error());
        }
        libc::cfmakeraw(&mut t);
        t.c_cflag |= libc::CLOCAL | libc::CREAD;
        // Reads return after 100 ms with whatever arrived.
        t.c_cc[libc::VMIN] = 0;
        t.c_cc[libc::VTIME] = 1;
        libc::cfsetispeed(&mut t, libc::B115200);
        libc::cfsetospeed(&mut t, libc::B115200);
        if libc::tcsetattr(fd, libc::TCSANOW, &t) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn unix_now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

/// Send an AT command and collect the reply for `wait`.
fn at(port: &mut File, cmd: &str, wait: Duration) -> io::Result<String> {
    port.write_all(cmd.as_bytes())?;
    port.write_all(b"\r\n")?;
    let mut out = Vec::new();
    let start = Instant::now();
    let mut buf = [0u8; 4096];
    while start.elapsed() < wait {
        let n = port.read(&mut buf)?;
        out.extend_from_slice(&buf[..n]);
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

fn live(path: &str, o: &Opts) -> io::Result<()> {
    let mut port = std::fs::OpenOptions::new().read(true).write(true).open(path)?;
    raw_mode(&port)?;
    // SAFETY: installing a handler that only stores to an atomic.
    unsafe {
        libc::signal(libc::SIGINT, on_sigint as *const () as libc::sighandler_t);
    }
    // Drain, then read the current console protocol so it can be restored.
    let _ = at(&mut port, "", Duration::from_millis(300))?;
    let reply = at(&mut port, "AT+PROTOCOL_OUT?", Duration::from_millis(1200))?;
    let original = reply
        .lines()
        .find_map(|l| l.trim().strip_prefix("PROTOCOL_OUT=CONSOLE,"))
        .map(|s| s.trim().to_string())
        .ok_or_else(|| io::Error::other(format!("no PROTOCOL_OUT reply from the ADSBee: {reply:?}")))?;
    eprintln!("modes-live: console protocol was {original}; will restore it at the end");

    let mut rec = match &o.record {
        Some(p) => Some(File::create(p)?),
        None => None,
    };
    let mut c = Compare::new(o.own, o.log.as_deref());
    let start = Instant::now();
    let mut buf = [0u8; 4096];
    let result = (|| -> io::Result<()> {
        'outer: while start.elapsed().as_secs_f64() < o.secs {
            for (mode, proto, dur) in [(1u8, "BEAST", o.beast_s), (2, "CSBEE", o.csbee_s)] {
                port.write_all(format!("AT+PROTOCOL_OUT=CONSOLE,{proto}\r\n").as_bytes())?;
                let t0 = Instant::now();
                while t0.elapsed().as_secs_f64() < dur {
                    if STOP.load(Ordering::Relaxed) {
                        break 'outer;
                    }
                    let n = port.read(&mut buf)?;
                    if n == 0 {
                        continue;
                    }
                    let t = unix_now();
                    if let Some(f) = rec.as_mut() {
                        f.write_all(&t.to_le_bytes())?;
                        f.write_all(&[mode])?;
                        f.write_all(&(n as u32).to_le_bytes())?;
                        f.write_all(&buf[..n])?;
                    }
                    c.feed(t, &buf[..n]);
                }
            }
            eprint!(
                "\rmodes-live: {:4.0} s, {} frames, {} aircraft (ours), {} CSBee reports compared   ",
                start.elapsed().as_secs_f64(),
                c.table.stats.frames,
                c.table.len(),
                c.compared
            );
        }
        Ok(())
    })();
    eprintln!();
    // Always restore, even after an error.
    let back = at(&mut port, &format!("AT+PROTOCOL_OUT=CONSOLE,{original}"), Duration::from_millis(800));
    let check = at(&mut port, "AT+PROTOCOL_OUT?", Duration::from_millis(1200)).unwrap_or_default();
    let restored = check.contains(&format!("PROTOCOL_OUT=CONSOLE,{original}"));
    eprintln!(
        "modes-live: console protocol restored to {original}: {}",
        if restored && back.is_ok() { "confirmed" } else { "NOT CONFIRMED, check by hand" }
    );
    result?;
    c.finish();
    Ok(())
}

// ------------------------------------------------------------------ recording

struct Rec {
    t: f64,
    #[allow(dead_code)]
    mode: u8,
    bytes: Vec<u8>,
}

fn read_recording(path: &str) -> io::Result<Vec<Rec>> {
    let mut data = Vec::new();
    File::open(path)?.read_to_end(&mut data)?;
    let mut out = Vec::new();
    let mut i = 0;
    while i + 13 <= data.len() {
        let t = f64::from_le_bytes(data[i..i + 8].try_into().unwrap_or_default());
        let mode = data[i + 8];
        let n = u32::from_le_bytes(data[i + 9..i + 13].try_into().unwrap_or_default()) as usize;
        let end = (i + 13 + n).min(data.len());
        out.push(Rec { t, mode, bytes: data[i + 13..end].to_vec() });
        i = end;
    }
    Ok(out)
}

/// Export the Beast frames of a recording for the hub's loop-back test:
/// records of `u32` LE milliseconds since the first frame, one length
/// byte, then the Beast-escaped frame as the ADSBee sent it (re-encoded).
fn export(input: &str, out: &str, secs: f64) -> io::Result<()> {
    let recs = read_recording(input)?;
    let mut r = BeastReader::new();
    let mut frames: Vec<(f64, Received)> = Vec::new();
    for rec in &recs {
        r.feed(&rec.bytes, |x| frames.push((rec.t, x)));
    }
    let t0 = frames.first().map_or(0.0, |f| f.0);
    frames.retain(|f| f.0 - t0 <= secs);
    let mut f = File::create(out)?;
    for (t, x) in &frames {
        let mut buf = [0u8; 44];
        let level = x.signal_dbm.map_or(0, dbm_to_level);
        let n = modes::input::beast_encode(&x.frame, x.ticks_12mhz.unwrap_or(0), level, &mut buf);
        f.write_all(&(((t - t0) * 1000.0) as u32).to_le_bytes())?;
        f.write_all(&[n as u8])?;
        f.write_all(&buf[..n])?;
    }
    eprintln!("modes-live: exported {} frames over {:.0} s to {out}", frames.len(), frames.last().map_or(0.0, |f| f.0 - t0));
    Ok(())
}

/// Run an export through the decoder at its recorded pace (no receiver
/// position, as on the hub's loop-back bench) and print what comes out:
/// the numbers the hub's `modesloop` console should show after one pass.
fn check_export(path: &str) -> io::Result<()> {
    let mut data = Vec::new();
    File::open(path)?.read_to_end(&mut data)?;
    let mut table: Table<32> = Table::new(Config::default());
    let mut r = BeastReader::new();
    let (mut i, mut last_report) = (0usize, 0u64);
    let mut end_ms = 0;
    while i + 5 <= data.len() {
        let at = u64::from(u32::from_le_bytes(data[i..i + 4].try_into().unwrap_or_default()));
        let n = usize::from(data[i + 4]);
        let end = (i + 5 + n).min(data.len());
        let mut frames = Vec::new();
        r.feed(&data[i + 5..end], |x| frames.push(x));
        for x in frames {
            let _ = table.ingest(&x, at);
        }
        if at >= last_report + 1000 {
            last_report = at;
            table.report(at, |_| {});
        }
        end_ms = at;
        i = end;
    }
    let s = table.stats;
    println!(
        "{:.0} s: beast_frames={} broken={} used={} bad_crc={} unconfirmed={} pos_global={} pos_local={} pos_refused={} zone_mismatch={} bds={:?} table={}",
        end_ms as f64 / 1000.0, r.stats.frames, r.stats.broken, s.used, s.bad_crc, s.unconfirmed, s.positions_global,
        s.positions_local, s.positions_refused, s.cpr_zone_mismatch, s.bds, table.len()
    );
    for p in table.planes() {
        if let Some((lat, lon)) = p.position() {
            println!(
                "  {:06X} {} alt={:?} pos={:.5},{:.5}",
                p.icao,
                p.callsign().as_ref().map_or("-", |c| c.as_str()),
                p.baro_altitude_ft(),
                lat,
                lon
            );
        }
    }
    Ok(())
}

fn dbm_to_level(dbm: i16) -> u8 {
    let dbfs = (f64::from(dbm) + 100.0) * 0.75 - 45.0;
    (255.0 * 10f64.powf(dbfs / 20.0)).round().clamp(1.0, 255.0) as u8
}

// ------------------------------------------------------------------ comparison

#[derive(Default)]
struct PerAircraft {
    /// The ADSBee's last position and altitude, and when they last changed
    /// (ms): its CSBee line repeats old values, so their age is the time
    /// since they last changed, not the line's time.
    their_pos: Option<((f64, f64), u64)>,
    their_alt: Option<(i32, u64)>,
    reports: u32,
    known: u32,
    callsign: Option<String>,
    their_callsign: Option<String>,
    alt_n: u32,
    alt_ok: u32,
    pos_n: u32,
    pos_ok: u32,
    max_excess_m: f64,
}

#[derive(Default)]
struct Field {
    compared: u32,
    agree: u32,
}

impl Field {
    fn add(&mut self, ok: bool) {
        self.compared += 1;
        if ok {
            self.agree += 1;
        }
    }
    fn show(&self, name: &str) {
        if self.compared == 0 {
            println!("  {name:<28} no overlap");
        } else {
            println!(
                "  {name:<28} {:>5} / {:<5} agree ({:.1}%)",
                self.agree,
                self.compared,
                100.0 * f64::from(self.agree) / f64::from(self.compared)
            );
        }
    }
}

struct Compare {
    table: Table<64>,
    beast: BeastReader,
    split: adsb::LineSplitter,
    last_report_ms: u64,
    t0: Option<f64>,
    compared: u32,
    presence: Field,
    callsign: Field,
    squawk: Field,
    category: Field,
    alt: Field,
    alt_exact: Field,
    pos: Field,
    pos_dist: Vec<f64>,
    alt_diff: Vec<i32>,
    csbee_lines: u32,
    csbee_errors: u32,
    /// Heard CSBee lines skipped: the ADSBee's frame counts say it heard
    /// nothing from that aircraft in the last second (its "updated" flag
    /// bits stay set on firmware 0.9.0, so they cannot be used).
    not_heard: u32,
    /// Comparisons where the ADSBee's value had not changed for 10 s or
    /// more while ours was updated in the last 2 s and differed.
    their_stale: u32,
    per: BTreeMap<u32, PerAircraft>,
    log: Option<File>,
}

impl Compare {
    fn new(own: Option<(f64, f64)>, log: Option<&str>) -> Self {
        let mut table = Table::new(Config::default());
        table.set_reference(own);
        Self {
            table,
            beast: BeastReader::new(),
            split: adsb::LineSplitter::new(),
            last_report_ms: 0,
            t0: None,
            compared: 0,
            presence: Field::default(),
            callsign: Field::default(),
            squawk: Field::default(),
            category: Field::default(),
            alt: Field::default(),
            alt_exact: Field::default(),
            pos: Field::default(),
            pos_dist: Vec::new(),
            alt_diff: Vec::new(),
            csbee_lines: 0,
            csbee_errors: 0,
            not_heard: 0,
            their_stale: 0,
            per: BTreeMap::new(),
            log: log.and_then(|p| File::create(p).ok()),
        }
    }

    fn ms(&mut self, t: f64) -> u64 {
        let t0 = *self.t0.get_or_insert(t);
        ((t - t0) * 1000.0).max(0.0) as u64
    }

    fn feed(&mut self, t: f64, bytes: &[u8]) {
        let now = self.ms(t);
        // Every byte goes to both parsers: CSBee text never contains the
        // Beast start byte, and a Beast frame never contains "#A:" plus a
        // valid CRC, so each ignores the other's data. This keeps the
        // frames that are still in flight when the protocol is switched.
        let mut frames = Vec::new();
        self.beast.feed(bytes, |r| frames.push(r));
        for r in frames {
            let _ = self.table.ingest(&r, now);
        }
        if now >= self.last_report_ms + 1000 {
            self.last_report_ms = now;
            // Keep our table's per-second counters and expiry running, as
            // the hub does.
            self.table.report(now, |_| {});
        }
        let mut reports = Vec::new();
        self.split.feed(bytes, |r| reports.push(r));
        for r in reports {
            match r {
                Ok(adsb::Report::Aircraft(a)) => {
                    self.csbee_lines += 1;
                    self.compare(&a, now, t);
                }
                Ok(adsb::Report::Stats(_)) => self.csbee_lines += 1,
                // Beast binary fed to the line splitter: a stray '#' byte
                // starts a "line" that never ends. Not a CSBee problem.
                Err(adsb::Error::NotCsbee | adsb::Error::TooLong) => {}
                Err(e) => {
                    self.csbee_errors += 1;
                    if let Some(f) = self.log.as_mut() {
                        let _ = writeln!(f, "{t:.1} csbee-error {e:?}");
                    }
                }
            }
        }
    }

    fn mismatch(&mut self, t: f64, icao: u32, what: &str) {
        if let Some(f) = self.log.as_mut() {
            let _ = writeln!(f, "{t:.1} {icao:06X} {what}");
        }
    }

    fn compare(&mut self, theirs: &adsb::Aircraft, now: u64, t: f64) {
        // Only lines whose frame counts say the ADSBee heard the aircraft
        // in the last second; the others repeat old data. (Its UPDATED_*
        // flag bits are not used: on 0.9.0 they stay set on lines that
        // repeat old values, see docs/ADSBEE.md.)
        if theirs.frames_per_s == 0 && theirs.es_frames_per_s == 0 {
            self.not_heard += 1;
            return;
        }
        self.compared += 1;
        let icao = theirs.icao24();
        let per = self.per.entry(icao).or_default();
        per.reports += 1;
        if !theirs.callsign.is_empty() {
            per.their_callsign = Some(theirs.callsign.as_str().to_string());
        }
        if let Some(p) = theirs.position()
            && per.their_pos.is_none_or(|(q, _)| q != p) {
                per.their_pos = Some((p, now));
            }
        if let Some(a) = theirs.baro_altitude_ft()
            && per.their_alt.is_none_or(|(b, _)| b != a) {
                per.their_alt = Some((a, now));
            }
        let their_pos_age_s = per.their_pos.map_or(0.0, |(_, t)| now.saturating_sub(t) as f64 / 1000.0);
        let their_alt_age_s = per.their_alt.map_or(0.0, |(_, t)| now.saturating_sub(t) as f64 / 1000.0);
        let Some(plane) = self.table.get(icao).copied() else {
            self.presence.add(false);
            self.mismatch(t, icao, "not in our table");
            return;
        };
        self.presence.add(true);
        let per = self.per.entry(icao).or_default();
        per.known += 1;
        let ours = plane.to_aircraft(now);
        if !ours.callsign.is_empty() {
            per.callsign = Some(ours.callsign.as_str().to_string());
        }
        if !ours.callsign.is_empty() && !theirs.callsign.is_empty() {
            let ok = ours.callsign == theirs.callsign;
            self.callsign.add(ok);
            if !ok {
                let m = format!("callsign ours {} theirs {}", ours.callsign.as_str(), theirs.callsign.as_str());
                self.mismatch(t, icao, &m);
            }
        }
        if let (Some(a), Some(b)) = (ours.squawk, theirs.squawk) {
            self.squawk.add(a == b);
            if a != b {
                self.mismatch(t, icao, &format!("squawk ours {a:04o} theirs {b:04o}"));
            }
        }
        if ours.category >= 0 && theirs.category >= 0 {
            self.category.add(ours.category == theirs.category);
        }
        if let (Some(a), Some(b)) = (ours.baro_altitude_ft(), theirs.baro_altitude_ft()) {
            let d = a - b;
            self.alt_diff.push(d);
            // Allow for climb or descent since our last altitude message.
            let age_s = plane.baro_altitude_ms().map_or(0.0, |m| now.saturating_sub(m) as f64 / 1000.0);
            let rate = theirs.vertical_rate_fpm().unwrap_or(0).unsigned_abs() as f64 / 60.0;
            let ok = f64::from(d.unsigned_abs()) <= 25.0 + rate * (age_s + their_alt_age_s + 1.5);
            self.alt.add(ok);
            if d != 0 && their_alt_age_s >= 10.0 && age_s <= 2.0 {
                self.their_stale += 1;
            }
            self.alt_exact.add(d == 0);
            let per = self.per.entry(icao).or_default();
            per.alt_n += 1;
            per.alt_ok += u32::from(ok);
            if !ok {
                self.mismatch(t, icao, &format!("altitude ours {a} theirs {b} (ours {age_s:.1} s old, theirs unchanged {their_alt_age_s:.1} s)"));
            }
        }
        if let (Some(a), Some(b)) = (ours.position(), theirs.position()) {
            let d = distance_m(a, b);
            self.pos_dist.push(d);
            // How far it can have moved between our fix and theirs (theirs
            // is at most ~1 s old when its updated flag is set, up to the
            // reporting interval otherwise), plus CPR resolution.
            let age_s = plane.position_ms().map_or(0.0, |m| now.saturating_sub(m) as f64 / 1000.0);
            let theirs_age = their_pos_age_s + 1.5;
            let speed = f64::from(theirs.speed_kt().unwrap_or(250).max(0)) * 0.514_444;
            let allowed = 100.0 + speed * (age_s + theirs_age);
            let ok = d <= allowed;
            self.pos.add(ok);
            if d > 500.0 && their_pos_age_s >= 10.0 && age_s <= 2.0 {
                self.their_stale += 1;
            }
            let per = self.per.entry(icao).or_default();
            per.pos_n += 1;
            per.pos_ok += u32::from(ok);
            per.max_excess_m = per.max_excess_m.max(d - allowed);
            if !ok {
                self.mismatch(t, icao, &format!("position {d:.0} m apart (allowed {allowed:.0} m, our fix {age_s:.1} s old, theirs unchanged {their_pos_age_s:.1} s)"));
            }
        }
    }

    fn finish(mut self) {
        let end = self.last_report_ms;
        let ours_icaos: Vec<u32> = self.table.planes().map(|p| p.icao).collect();
        let s = self.table.stats;
        println!("\n=== crates/modes vs the ADSBee's CSBee decode ===");
        println!(
            "run {:.0} s; Beast: {} frames ({} broken, {} bytes skipped); CSBee: {} lines, {} bad",
            end as f64 / 1000.0,
            self.beast.stats.frames,
            self.beast.stats.broken,
            self.beast.stats.skipped,
            self.csbee_lines,
            self.csbee_errors
        );
        println!(
            "our decoder: {} used, {} bad CRC, {} 1-bit fixed, {} 2-bit fixed ({} refused), {} unconfirmed AP, {} global + {} local positions, {} refused, {} zone mismatches, TIS-B {}, ADS-R {}",
            s.used, s.bad_crc, s.fixed_1, s.fixed_2, s.fix_2_refused, s.unconfirmed, s.positions_global,
            s.positions_local, s.positions_refused, s.cpr_zone_mismatch, s.tis_b, s.ads_r
        );
        let dfs: Vec<String> =
            s.by_df.iter().enumerate().filter(|(_, n)| **n > 0).map(|(d, n)| format!("DF{d}:{n}")).collect();
        println!("frames by format: {}", dfs.join(" "));
        println!(
            "Comm-B inferred: 1,0:{} 2,0:{} 4,0:{} 5,0:{} 6,0:{} ambiguous:{} unknown:{}",
            s.bds[0], s.bds[1], s.bds[2], s.bds[3], s.bds[4], s.bds[5], s.bds[6]
        );
        println!(
            "\nCSBee aircraft lines compared: {} (skipped {} whose frame counts were 0)",
            self.compared, self.not_heard
        );
        println!(
            "  ADSBee value unchanged for 10+ s while ours was fresh and differed: {}",
            self.their_stale
        );
        self.presence.show("aircraft also in our table");
        self.callsign.show("callsign");
        self.squawk.show("squawk");
        self.category.show("emitter category");
        self.alt.show("baro altitude (rate-allowed)");
        self.alt_exact.show("baro altitude exactly equal");
        self.pos.show("position (motion-allowed)");
        if !self.pos_dist.is_empty() {
            self.pos_dist.sort_by(|a, b| a.total_cmp(b));
            let q = |p: f64| self.pos_dist[((self.pos_dist.len() - 1) as f64 * p) as usize];
            println!(
                "  position distance, m: median {:.0}, 90% {:.0}, max {:.0}",
                q(0.5),
                q(0.9),
                q(1.0)
            );
        }
        if !self.alt_diff.is_empty() {
            let mut a: Vec<i32> = self.alt_diff.iter().map(|d| d.abs()).collect();
            a.sort();
            println!("  altitude |difference|, ft: median {}, max {}", a[a.len() / 2], a[a.len() - 1]);
        }
        println!("\nper aircraft (CSBee reports / in our table / altitude ok / position ok):");
        for (icao, p) in &self.per {
            println!(
                "  {icao:06X} {:<8} {:>4} / {:>4} / {:>3} of {:<3} / {:>3} of {:<3}{}",
                p.their_callsign.as_deref().or(p.callsign.as_deref()).unwrap_or("-"),
                p.reports,
                p.known,
                p.alt_ok,
                p.alt_n,
                p.pos_ok,
                p.pos_n,
                if p.pos_n > 0 && p.max_excess_m > 0.0 { format!("  worst excess {:.0} m", p.max_excess_m) } else { String::new() }
            );
        }
        let extra: Vec<String> =
            ours_icaos.iter().filter(|i| !self.per.contains_key(i)).map(|i| format!("{i:06X}")).collect();
        if !extra.is_empty() {
            println!("in our table but never in a heard CSBee line: {}", extra.join(" "));
        }
    }
}
