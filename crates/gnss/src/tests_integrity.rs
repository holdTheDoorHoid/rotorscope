//! Host tests for the integrity monitor: synthetic NMEA (LC29H-style) and UBX (M10-style)
//! streams for a clean sky, an indoor bench, jamming, sudden and slow spoofing, and time jumps.
//!
//! Every coordinate here is made up (a point in the open ocean near 0 N 0 E); nothing is a
//! real place. Frames are built by this file's own helpers (own checksums), not the crate's.

use crate::integrity::{self, Cause, Level, Monitor, State};
use crate::quectel::Reply;
use crate::*;
use std::format;
use std::string::String;
use std::vec::Vec;

// ------------------------------------------------------------ framing ----

fn nmea(body: &str) -> String {
    let x = body.bytes().fold(0u8, |a, b| a ^ b);
    format!("${body}*{x:02X}\r\n")
}

fn ubx(class: u8, id: u8, p: &[u8]) -> Vec<u8> {
    let mut v = std::vec![0xB5, 0x62, class, id, p.len() as u8, (p.len() >> 8) as u8];
    v.extend_from_slice(p);
    let (mut a, mut b) = (0u8, 0u8);
    for &x in &v[2..] {
        a = a.wrapping_add(x);
        b = b.wrapping_add(a);
    }
    v.push(a);
    v.push(b);
    v
}

/// (year, month, day, hour, minute, second) from Unix seconds (Hinnant's civil_from_days).
fn civil(t: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = t.div_euclid(86_400);
    let s = t.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d, (s / 3600) as u32, (s / 60 % 60) as u32, (s % 60) as u32)
}

fn ddmm(v: f64, deg_digits: usize) -> String {
    let a = v.abs();
    let d = a.trunc();
    let m = (a - d) * 60.0;
    format!("{:0w$}{:09.6}", d as u32, m, w = deg_digits)
}

// ------------------------------------------------------------ the sky ----

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
    /// Uniform integer in -k..=k.
    fn pm(&mut self, k: i32) -> i32 {
        (self.next() % (2 * k as u64 + 1)) as i32 - k
    }
    fn unit(&mut self) -> f64 {
        (self.next() % 1_000_000) as f64 / 1_000_000.0
    }
}

#[derive(Clone, Copy)]
struct SatModel {
    prn: u16,
    elev0: f64,
    /// Degrees per epoch (positive: rising).
    rate: f64,
    azim: u16,
}

/// A made-up home: open ocean, roughly 0.12 N 0.57 E.
const HOME_LAT: f64 = 0.123_4;
const HOME_LON: f64 = 0.567_8;
const HOME_ALT: f64 = 25.0;
const START_UTC: i64 = 1_800_000_000;

#[derive(Clone, Copy, PartialEq)]
enum Env {
    /// Open sky: 30 + 0.22 x elevation dB-Hz, +-2.
    Open,
    /// Indoors by a window: 14 + 0.2 x elevation, +-4, under 18 not tracked.
    Bench,
}

/// One epoch's worth of everything a receiver would say.
#[derive(Clone)]
struct Epoch {
    lat: f64,
    lon: f64,
    alt: f64,
    utc: i64,
    fix: bool,
    /// (prn, elevation, azimuth, C/N0)
    sats: Vec<(u16, i32, u16, Option<u8>)>,
    hdop: f32,
    h_acc_m: f32,
}

struct Sky {
    rng: Rng,
    models: Vec<SatModel>,
    t: u32,
    env: Env,
    /// Added to every satellite's C/N0 (a fade, a jammer).
    offset_db: i32,
    /// If set, every satellite at this C/N0 +-1 (a spoofer).
    spoof_cn0: Option<u8>,
    /// Position offset from home, metres north / east, and height.
    dn: f64,
    de: f64,
    dz: f64,
    /// UTC offset, s.
    dt_s: i64,
    /// Indoor bench wander state.
    wn: f64,
    we: f64,
    /// Force "no fix" this epoch.
    no_fix: bool,
}

impl Sky {
    fn new(seed: u64, env: Env) -> Sky {
        // Ten GPS satellites spread over the sky; one rising from below the horizon, one
        // setting towards it.
        let models = std::vec![
            SatModel { prn: 2, elev0: 72.0, rate: -0.002, azim: 40 },
            SatModel { prn: 5, elev0: 55.0, rate: 0.003, azim: 120 },
            SatModel { prn: 7, elev0: 41.0, rate: -0.004, azim: 200 },
            SatModel { prn: 9, elev0: 33.0, rate: 0.004, azim: 300 },
            SatModel { prn: 13, elev0: 25.0, rate: 0.002, azim: 80 },
            SatModel { prn: 16, elev0: 18.0, rate: -0.003, azim: 160 },
            SatModel { prn: 20, elev0: 62.0, rate: 0.001, azim: 250 },
            SatModel { prn: 24, elev0: 12.0, rate: -0.008, azim: 340 }, // setting
            SatModel { prn: 27, elev0: -2.0, rate: 0.012, azim: 15 },   // rising
            SatModel { prn: 30, elev0: 48.0, rate: -0.001, azim: 100 },
        ];
        Sky {
            rng: Rng(seed),
            models,
            t: 0,
            env,
            offset_db: 0,
            spoof_cn0: None,
            dn: 0.0,
            de: 0.0,
            dz: 0.0,
            dt_s: 0,
            wn: 0.0,
            we: 0.0,
            no_fix: false,
        }
    }

    fn step(&mut self) -> Epoch {
        let t = f64::from(self.t);
        let mut sats = Vec::new();
        for m in self.models.clone() {
            let e = m.elev0 + m.rate * t;
            if e < 0.0 {
                continue;
            }
            let (base, noise, floor) = match self.env {
                Env::Open => (30.0 + 0.22 * e, 2, 20),
                Env::Bench => (14.0 + 0.2 * e, 4, 18),
            };
            let c = match self.spoof_cn0 {
                Some(s) => i32::from(s) + self.rng.pm(1),
                None => base as i32 + self.rng.pm(noise) + self.offset_db,
            };
            let cn0 = (c >= floor).then_some(c.clamp(0, 99) as u8);
            sats.push((m.prn, e as i32, m.azim, cn0));
        }
        let tracked = sats.iter().filter(|s| s.3.is_some()).count();
        let (hdop, mut h_acc) = match self.env {
            Env::Open => (0.9, 2.5),
            Env::Bench => (2.5 + 3.0 * self.rng.unit() as f32, 15.0 + 20.0 * self.rng.unit() as f32),
        };
        if self.env == Env::Bench {
            // Mean-reverting wander of up to about +-35 m (multipath).
            self.wn = self.wn * 0.95 + (self.rng.unit() - 0.5) * 6.0;
            self.we = self.we * 0.95 + (self.rng.unit() - 0.5) * 6.0;
        } else {
            self.wn = (self.rng.unit() - 0.5) * 3.0;
            self.we = (self.rng.unit() - 0.5) * 3.0;
        }
        if self.spoof_cn0.is_some() {
            h_acc = 2.0;
        }
        let fix = tracked >= 4 && !self.no_fix;
        let lat = HOME_LAT + (self.dn + self.wn) / 111_195.0;
        let lon = HOME_LON + (self.de + self.we) / (111_195.0 * HOME_LAT.to_radians().cos());
        let alt = HOME_ALT + self.dz + if self.env == Env::Bench { self.rng.pm(8) as f64 } else { self.rng.pm(2) as f64 };
        let e = Epoch { lat, lon, alt, utc: START_UTC + i64::from(self.t) + self.dt_s, fix, sats, hdop, h_acc_m: h_acc };
        self.t += 1;
        e
    }
}

// ---------------------------------------------------- receiver output ----

/// What an LC29H-style receiver prints for one epoch: GGA, GSV (L1), RMC, optionally $PAIRSPF.
fn nmea_epoch(e: &Epoch, pairspf: Option<u8>) -> Vec<u8> {
    let (_, mo, d, h, mi, s) = civil(e.utc);
    let (y, ..) = civil(e.utc);
    let tod = format!("{h:02}{mi:02}{s:02}.000");
    let lat = ddmm(e.lat, 2);
    let lon = ddmm(e.lon, 3);
    let ns = if e.lat >= 0.0 { "N" } else { "S" };
    let ew = if e.lon >= 0.0 { "E" } else { "W" };
    let mut out = String::new();
    let tracked = e.sats.iter().filter(|s| s.3.is_some()).count();
    if e.fix {
        out += &nmea(&format!("GNGGA,{tod},{lat},{ns},{lon},{ew},1,{tracked:02},{:.2},{:.1},M,0.0,M,,", e.hdop, e.alt));
    } else {
        out += &nmea(&format!("GNGGA,{tod},,,,,0,00,99.99,,,,,,"));
    }
    let n = e.sats.len();
    let groups = n.div_ceil(4).max(1);
    for g in 0..groups {
        let mut b = format!("GPGSV,{groups},{},{n:02}", g + 1);
        for &(prn, el, az, cn0) in e.sats.iter().skip(4 * g).take(4) {
            let c = cn0.map_or(String::new(), |c| format!("{c:02}"));
            b += &format!(",{prn:02},{el:02},{az:03},{c}");
        }
        b += ",1";
        out += &nmea(&b);
    }
    if e.fix {
        out += &nmea(&format!(
            "GNRMC,{tod},A,{lat},{ns},{lon},{ew},0.00,0.00,{d:02}{mo:02}{:02},,,A,V",
            y % 100
        ));
    } else {
        out += &nmea(&format!("GNRMC,{tod},V,,,,,,,{d:02}{mo:02}{:02},,,N,V", y % 100));
    }
    if let Some(st) = pairspf {
        out += &nmea(&format!("PAIRSPF,{st}"));
    }
    out.into_bytes()
}

#[derive(Clone, Copy)]
struct UbxExtras {
    jam_state: u8,
    agc: u16,
    cw: u8,
    spoof: u8,
    clk_bias: i32,
    clk_drift: i32,
}

impl Default for UbxExtras {
    fn default() -> Self {
        UbxExtras { jam_state: 1, agc: 5000, cw: 10, spoof: 1, clk_bias: 120_000, clk_drift: 250 }
    }
}

/// What an M10-style receiver sends for one epoch: NAV-PVT, NAV-SAT, NAV-STATUS, NAV-CLOCK,
/// MON-RF.
fn ubx_epoch(e: &Epoch, x: &UbxExtras) -> Vec<u8> {
    let (y, mo, d, h, mi, s) = civil(e.utc);
    let itow = ((e.utc - START_UTC) as u32).wrapping_mul(1000);
    let mut p = std::vec![0u8; 92];
    p[0..4].copy_from_slice(&itow.to_le_bytes());
    p[4..6].copy_from_slice(&(y as u16).to_le_bytes());
    p[6] = mo as u8;
    p[7] = d as u8;
    p[8] = h as u8;
    p[9] = mi as u8;
    p[10] = s as u8;
    p[11] = 0b0111;
    p[12..16].copy_from_slice(&30u32.to_le_bytes());
    p[20] = if e.fix { 3 } else { 0 };
    p[21] = u8::from(e.fix);
    p[23] = e.sats.iter().filter(|s| s.3.is_some()).count() as u8;
    p[24..28].copy_from_slice(&((e.lon * 1e7) as i32).to_le_bytes());
    p[28..32].copy_from_slice(&((e.lat * 1e7) as i32).to_le_bytes());
    p[32..36].copy_from_slice(&((e.alt * 1000.0) as i32).to_le_bytes());
    p[36..40].copy_from_slice(&((e.alt * 1000.0) as i32 + 30_000).to_le_bytes());
    p[40..44].copy_from_slice(&((e.h_acc_m * 1000.0) as u32).to_le_bytes());
    p[44..48].copy_from_slice(&((e.h_acc_m * 1500.0) as u32).to_le_bytes());
    p[76..78].copy_from_slice(&150u16.to_le_bytes());
    let mut out = ubx(0x01, 0x07, &p);

    let mut sat = std::vec![0u8; 8];
    sat[0..4].copy_from_slice(&itow.to_le_bytes());
    sat[4] = 1;
    sat[5] = e.sats.len() as u8;
    for &(prn, el, az, cn0) in &e.sats {
        let mut b = [0u8; 12];
        b[0] = 0; // GPS
        b[1] = prn as u8;
        b[2] = cn0.unwrap_or(0);
        b[3] = el as i8 as u8;
        b[4..6].copy_from_slice(&(az as i16).to_le_bytes());
        let q: u32 = if cn0.is_some() { 7 } else { 1 };
        let used = u32::from(cn0.is_some() && e.fix) << 3;
        b[8..12].copy_from_slice(&(q | used | 1 << 4).to_le_bytes());
        sat.extend_from_slice(&b);
    }
    out.extend(ubx(0x01, 0x35, &sat));

    let mut st = [0u8; 16];
    st[0..4].copy_from_slice(&itow.to_le_bytes());
    st[4] = if e.fix { 3 } else { 0 };
    st[5] = u8::from(e.fix);
    st[7] = x.spoof << 3;
    out.extend(ubx(0x01, 0x03, &st));

    let mut ck = [0u8; 20];
    ck[0..4].copy_from_slice(&itow.to_le_bytes());
    ck[4..8].copy_from_slice(&x.clk_bias.to_le_bytes());
    ck[8..12].copy_from_slice(&x.clk_drift.to_le_bytes());
    out.extend(ubx(0x01, 0x22, &ck));

    let mut rf = std::vec![0u8; 4 + 24];
    rf[1] = 1;
    rf[4] = 0;
    rf[5] = x.jam_state;
    rf[6] = 2;
    rf[7] = 1;
    rf[16..18].copy_from_slice(&90u16.to_le_bytes());
    rf[18..20].copy_from_slice(&x.agc.to_le_bytes());
    rf[20] = x.cw;
    out.extend(ubx(0x0A, 0x38, &rf));
    out
}

// --------------------------------------------------------------- harness --

struct Hub {
    parser: Parser,
    mon: Monitor,
    now: u64,
    jitter: Rng,
    /// Arrival jitter, +-ms (message length, main loop).
    jitter_ms: i32,
    /// The receiver's epochs arrive this much later than they should (a time spoofer moving
    /// the receiver's second against the hub's clock), ms.
    shift_ms: i64,
    /// A PPS edge with every epoch (at the top of the receiver's second).
    pps: bool,
    /// The hub's crystal runs this many ppm fast (its uptime gains on true time).
    crystal_ppm: i64,
    crystal_acc_us: i64,
    states: Vec<State>,
    changes: Vec<(State, State, Option<Cause>)>,
    trusted: Vec<bool>,
}

impl Hub {
    fn new() -> Hub {
        Hub {
            parser: Parser::new(),
            mon: Monitor::new(),
            now: 10_000,
            jitter: Rng(99),
            jitter_ms: 40,
            shift_ms: 0,
            pps: false,
            crystal_ppm: 0,
            crystal_acc_us: 0,
            states: Vec::new(),
            changes: Vec::new(),
            trusted: Vec::new(),
        }
    }

    /// One epoch of bytes, processed the way the hub does (collect events, then handle).
    fn feed(&mut self, bytes: &[u8]) -> State {
        // Hub processing latency: 1 s per epoch plus up to +-40 ms of jitter.
        self.crystal_acc_us += self.crystal_ppm;
        let gain_ms = self.crystal_acc_us / 1000;
        self.crystal_acc_us -= gain_ms * 1000;
        self.now = (self.now as i64 + 1000 + gain_ms) as u64;
        let top = self.now as i64 + self.shift_ms;
        if self.pps {
            self.mon.set_pps(Some(((top - 100) * 1000 + i64::from(self.jitter.pm(50))) as u64));
        }
        let at = (top + i64::from(self.jitter.pm(self.jitter_ms))) as u64;
        let evs: Vec<Event> = self.parser.feed(bytes).collect();
        for ev in &evs {
            if let Some(c) = self.mon.on_event(ev, self.parser.sats(), at) {
                self.changes.push((c.from, c.status.state, c.status.cause.map(|f| f.cause)));
            }
        }
        let s = self.mon.status().state;
        self.states.push(s);
        self.trusted.push(self.mon.status().trusted);
        s
    }

    fn worst(&self) -> State {
        self.states.iter().copied().max().unwrap_or(State::Ok)
    }
}

fn run_nmea(hub: &mut Hub, sky: &mut Sky, n: usize) {
    for _ in 0..n {
        let e = sky.step();
        hub.feed(&nmea_epoch(&e, None));
    }
}

fn run_ubx(hub: &mut Hub, sky: &mut Sky, x: &UbxExtras, n: usize) {
    for _ in 0..n {
        let e = sky.step();
        hub.feed(&ubx_epoch(&e, x));
    }
}

fn reason(hub: &Hub) -> String {
    let mut s = String::new();
    hub.mon.status().write_reason(&mut s).unwrap();
    s
}

// ------------------------------------------------------- parser pieces --

#[test]
fn ubx_integrity_messages_decode() {
    let mut sky = Sky::new(1, Env::Open);
    let e = sky.step();
    let x = UbxExtras { jam_state: 2, agc: 4321, cw: 77, spoof: 3, clk_bias: -5, clk_drift: 260 };
    let mut p = Parser::new();
    let evs: Vec<Event> = p.feed(&ubx_epoch(&e, &x)).collect();
    assert!(matches!(evs[0], Event::Fix(_)));
    let st = evs.iter().find_map(|e| if let Event::NavStatus(s) = e { Some(*s) } else { None }).unwrap();
    assert_eq!(st.spoof_det_state, 3);
    let ck = evs.iter().find_map(|e| if let Event::NavClock(c) = e { Some(*c) } else { None }).unwrap();
    assert_eq!((ck.bias_ns, ck.drift_ns_s), (-5, 260));
    let rf = evs.iter().find_map(|e| if let Event::MonRf(r) = e { Some(*r) } else { None }).unwrap();
    assert_eq!(rf.blocks().len(), 1);
    assert_eq!((rf.blocks()[0].jamming_state, rf.blocks()[0].agc_cnt, rf.blocks()[0].cw_suppression), (2, 4321, 77));
    assert_eq!(rf.blocks()[0].noise_per_ms, 90);
    let t = p.sats();
    assert_eq!(t.seq, 1);
    assert_eq!(t.sats().len(), e.sats.len());
    let s0 = t.sats()[0];
    assert_eq!((s0.prn, s0.elev, s0.azim, s0.used, s0.plausible), (2, Some(72), Some(40), true, true));
    assert_eq!(p.stats().checksum_errors, 0);
}

#[test]
fn nav_sat_with_64_satellites_fits() {
    let mut sat = std::vec![0u8; 8];
    sat[4] = 1;
    sat[5] = 64;
    for i in 0..64u8 {
        let mut b = [0u8; 12];
        b[0] = if i < 32 { 0 } else { 6 };
        b[1] = i % 32 + 1;
        b[2] = 40;
        b[3] = 30;
        b[8] = 7;
        sat.extend_from_slice(&b);
    }
    let mut p = Parser::new();
    let _ = p.feed(&ubx(0x01, 0x35, &sat)).count();
    assert_eq!(p.sats().sats().len(), 64);
    assert_eq!(p.sats().overflow, 0);
    assert!(p.sats().sats().iter().all(|s| s.plausible));
}

#[test]
fn nav_sat_unknown_angles_and_impossible_numbers() {
    let mut sat = std::vec![0u8; 8];
    sat[4] = 1;
    sat[5] = 3;
    // GPS 40 (impossible), GLONASS 255 (slot unknown: fine), Galileo 5 with elev -100 (unknown).
    for (g, sv, el) in [(0u8, 40u8, 20i8), (6, 255, 20), (2, 5, -100)] {
        let mut b = [0u8; 12];
        b[0] = g;
        b[1] = sv;
        b[2] = 35;
        b[3] = el as u8;
        b[4..6].copy_from_slice(&(-1i16).to_le_bytes());
        b[8] = 7;
        sat.extend_from_slice(&b);
    }
    let mut p = Parser::new();
    let _ = p.feed(&ubx(0x01, 0x35, &sat)).count();
    let s = p.sats().sats();
    assert!(!s[0].plausible);
    assert!(s[1].plausible);
    assert_eq!((s[2].elev, s[2].azim, s[2].plausible), (None, None, true));
}

#[test]
fn gsv_satellites_and_impossible_values() {
    let mut p = Parser::new();
    let mut b = String::new();
    // Satellite 99 under GP is not a GPS/SBAS/QZSS number; elevation 95 and azimuth 400
    // are outside the sky; 193 under GP is QZSS.
    b += &nmea("GPGSV,2,1,05,05,45,120,41,99,30,100,40,07,95,200,38,09,20,400,35,1");
    b += &nmea("GPGSV,2,2,05,193,60,010,44,1");
    b += &nmea("GLGSV,1,1,02,70,30,050,39,12,30,050,39,1");
    b += &nmea("GNGGA,120000.000,0007.404000,N,00034.068000,E,1,08,1.0,25.0,M,0.0,M,,");
    b += &nmea("GNRMC,120000.000,A,0007.404000,N,00034.068000,E,0.0,0.0,150126,,,A,V");
    let _ = p.feed(b.as_bytes()).count();
    let s = p.sats().sats();
    assert_eq!(s.len(), 7);
    let pl: Vec<(u16, bool)> = s.iter().map(|s| (s.prn, s.plausible)).collect();
    assert_eq!(pl, [(5, true), (99, false), (7, false), (9, false), (193, true), (70, true), (12, false)]);
    assert_eq!(s[4].sys, crate::sky::System::Qzss);
    let st = p.sats().cn0_stats(crate::sky::Band::L1);
    assert_eq!((st.listed, st.tracked, st.implausible, st.max, st.min), (7, 7, 4, 44, 35));
}

#[test]
fn pairspf_decodes() {
    let mut p = Parser::new();
    let evs: Vec<Event> = p.feed(format!("{}{}", nmea("PAIRSPF,3"), nmea("PAIRSPF5,1")).as_bytes()).collect();
    assert_eq!(evs[0], Event::Quectel(Reply::Jamming { l5: false, status: 3 }));
    assert_eq!(evs[1], Event::Quectel(Reply::Jamming { l5: true, status: 1 }));
}

// ----------------------------------------------------------- clean sky --

#[test]
fn clean_open_sky_nmea_never_alarms_and_learns_home() {
    let mut hub = Hub::new();
    let mut sky = Sky::new(7, Env::Open);
    run_nmea(&mut hub, &mut sky, 1200);
    assert!(hub.mon.home().is_some(), "home learned after 15 min");
    let (lat, lon, alt) = hub.mon.home().unwrap();
    assert!(integrity::dist_m(lat, lon, HOME_LAT, HOME_LON) < 2.0);
    assert!((alt.unwrap() - HOME_ALT).abs() < 1.0);
    // Brief fix loss (a doorway, 20 s) with everything 15 dB down.
    sky.offset_db = -15;
    sky.no_fix = true;
    run_nmea(&mut hub, &mut sky, 20);
    sky.offset_db = 0;
    sky.no_fix = false;
    // A satellite has risen and one set by now; keep going.
    run_nmea(&mut hub, &mut sky, 1500);
    assert_eq!(hub.worst(), State::Ok, "changes: {:?}", hub.changes);
    assert!(hub.mon.status().clean);
    assert!(hub.mon.status().metrics.cn0_std > 2.0, "a real sky is not flat");
}

#[test]
fn clean_open_sky_ubx_never_alarms() {
    let mut hub = Hub::new();
    let mut sky = Sky::new(8, Env::Open);
    let mut x = UbxExtras::default();
    for i in 0..2500 {
        // Normal receiver clock: drift wanders slowly, bias jumps by whole ms now and then.
        x.clk_drift = 250 + (i / 100) as i32;
        x.clk_bias += x.clk_drift;
        if x.clk_bias > 500_000 {
            x.clk_bias -= 1_000_000;
        }
        x.agc = (5000 + hub.jitter.pm(40)) as u16;
        let e = sky.step();
        hub.feed(&ubx_epoch(&e, &x));
    }
    assert_eq!(hub.worst(), State::Ok, "changes: {:?}", hub.changes);
    assert!(hub.mon.home().is_some());
    let m = hub.mon.status().metrics;
    assert_eq!(m.rx_spoof, Some(1));
    assert!(m.agc_baseline.is_some());
}

#[test]
fn indoor_bench_by_a_window_never_alarms() {
    // Weak, noisy signals, satellites dropping in and out, fixes coming and going, position
    // wandering tens of metres: an hour on the bench must not raise anything.
    for seed in [11, 12, 13] {
        let mut hub = Hub::new();
        let mut sky = Sky::new(seed, Env::Bench);
        for i in 0..3600 {
            // Someone walks past the window now and then (8 dB, 15 s); once the window is
            // blocked for 25 s (12 dB).
            sky.offset_db = match i % 600 {
                100..=114 => -8,
                _ => 0,
            };
            if (2000..2025).contains(&i) {
                sky.offset_db = -12;
            }
            let e = sky.step();
            hub.feed(&nmea_epoch(&e, Some(1)));
        }
        assert_eq!(hub.worst(), State::Ok, "seed {seed}: {:?}", hub.changes);
    }
}

// ------------------------------------------------------------- jamming --

#[test]
fn jamming_ramp_ublox() {
    let mut hub = Hub::new();
    let mut sky = Sky::new(21, Env::Open);
    let mut x = UbxExtras::default();
    run_ubx(&mut hub, &mut sky, &x, 300);
    assert_eq!(hub.worst(), State::Ok);
    // A jammer approaches: C/N0 falls 1 dB a second, the AGC turns down, the CW indicator
    // rises, and the interference monitor goes warning then critical once the fix is gone.
    let mut first_suspect = None;
    let mut first_jam = None;
    for i in 0..60 {
        sky.offset_db = -(i as i32);
        x.agc = 5000u16.saturating_sub(80 * i as u16);
        x.cw = (10 + 4 * i).min(255) as u8;
        let e = sky.step();
        x.jam_state = if !e.fix { 3 } else if i > 15 { 2 } else { 1 };
        let s = hub.feed(&ubx_epoch(&e, &x));
        if s == State::SuspectJamming && first_suspect.is_none() {
            first_suspect = Some(i);
        }
        if s == State::Jamming && first_jam.is_none() {
            first_jam = Some(i);
        }
    }
    assert!(first_suspect.is_some(), "{:?}", hub.changes);
    assert!(first_jam.unwrap() < 40, "{:?}", hub.changes);
    assert!(!hub.states.iter().any(|s| s.is_spoofing()), "{:?}", hub.changes);
    let r = reason(&hub);
    assert!(r.starts_with("GPS jamming: "), "{r}");
    // The jammer goes away: back to normal after the clear-down time.
    sky.offset_db = 0;
    x = UbxExtras::default();
    run_ubx(&mut hub, &mut sky, &x, 60);
    assert_eq!(hub.mon.status().state, State::Ok);
    assert_eq!(hub.changes.last().unwrap().1, State::Ok);
}

#[test]
fn jamming_lc29h_with_pairspf() {
    let mut hub = Hub::new();
    let mut sky = Sky::new(22, Env::Open);
    for _ in 0..300 {
        let e = sky.step();
        hub.feed(&nmea_epoch(&e, Some(1)));
    }
    for i in 0..60 {
        sky.offset_db = -(i as i32) / 2;
        let e = sky.step();
        let st = if i < 10 { 1 } else if i < 25 { 2 } else { 3 };
        hub.feed(&nmea_epoch(&e, Some(st)));
    }
    assert_eq!(hub.mon.status().state, State::Jamming, "{:?}", hub.changes);
    assert_eq!(hub.mon.status().cause.unwrap().cause, Cause::RxJamFlag);
    assert_eq!(hub.mon.status().cause.unwrap().cause.kind(), 1);
}

#[test]
fn jamming_plain_nmea_is_only_ever_suspect() {
    // No receiver jamming flag: a C/N0 collapse alone could be a hand over the antenna.
    let mut hub = Hub::new();
    let mut sky = Sky::new(23, Env::Open);
    run_nmea(&mut hub, &mut sky, 300);
    sky.offset_db = -14;
    run_nmea(&mut hub, &mut sky, 20);
    assert_eq!(hub.mon.status().state, State::Ok, "under the sustain time");
    run_nmea(&mut hub, &mut sky, 40);
    assert_eq!(hub.mon.status().state, State::SuspectJamming);
    let r = reason(&hub);
    assert!(r.contains("signal strength fell"), "{r}");
    // Total loss: nothing heard at all.
    sky.offset_db = -40;
    run_nmea(&mut hub, &mut sky, 60);
    assert_eq!(hub.worst(), State::SuspectJamming, "{:?}", hub.changes);
}

#[test]
fn moved_indoors_for_good_relearns_the_baseline() {
    let mut hub = Hub::new();
    let mut sky = Sky::new(24, Env::Open);
    run_nmea(&mut hub, &mut sky, 300);
    sky.offset_db = -12;
    run_nmea(&mut hub, &mut sky, 700);
    // Accepted as the new normal after CN0_REBASE_EPOCHS, then cleared.
    run_nmea(&mut hub, &mut sky, 60);
    assert_eq!(hub.mon.status().state, State::Ok, "{:?}", hub.changes);
}

// ------------------------------------------------------------ spoofing --

/// Clean sky long enough to learn home.
fn settled_nmea(seed: u64) -> (Hub, Sky) {
    let mut hub = Hub::new();
    let mut sky = Sky::new(seed, Env::Open);
    run_nmea(&mut hub, &mut sky, 1000);
    assert!(hub.mon.home().is_some());
    assert_eq!(hub.worst(), State::Ok);
    (hub, sky)
}

#[test]
fn sudden_spoof_with_uniform_cn0_and_a_new_position() {
    let (mut hub, mut sky) = settled_nmea(31);
    // A spoofer next door: every satellite at 47 dB-Hz +-1, and the position walks off 2 km.
    sky.spoof_cn0 = Some(47);
    sky.dn = 1_500.0;
    sky.de = -1_300.0;
    let mut at = None;
    for i in 0..30 {
        let e = sky.step();
        let s = hub.feed(&nmea_epoch(&e, None));
        if s == State::Spoofing && at.is_none() {
            at = Some(i);
        }
    }
    assert!(at.unwrap() <= 10, "{:?}", hub.changes);
    let st = hub.mon.status();
    assert!(st.hold());
    assert!(!st.clean);
    assert!(st.checks >= 2);
    let r = reason(&hub);
    assert!(r.starts_with("GPS spoofing: "), "{r}");
    assert!(r.contains("holding last trusted position and time"), "{r}");
    assert!(hub.changes.iter().all(|c| c.1 != State::SuspectJamming));
}

#[test]
fn uniform_strong_cn0_alone_is_spoofing() {
    let (mut hub, mut sky) = settled_nmea(32);
    sky.spoof_cn0 = Some(50);
    run_nmea(&mut hub, &mut sky, 10);
    assert_eq!(hub.mon.status().state, State::Spoofing, "{:?}", hub.changes);
    assert_eq!(hub.mon.status().cause.unwrap().cause, Cause::UniformCn0);
}

#[test]
fn uniform_but_weak_is_not_spoofing() {
    // A flat, weak sky (all at 30 +-1) is what a bench indoors can look like.
    let mut hub = Hub::new();
    let mut sky = Sky::new(33, Env::Open);
    sky.spoof_cn0 = Some(30);
    run_nmea(&mut hub, &mut sky, 300);
    assert_eq!(hub.worst(), State::Ok, "{:?}", hub.changes);
}

#[test]
fn slow_drag_off_is_caught() {
    let (mut hub, mut sky) = settled_nmea(34);
    // A careful spoofer: realistic C/N0, no jumps, the position slides north-east at 0.5 m/s.
    let mut suspect_at = None;
    let mut spoof_at = None;
    for i in 0..900 {
        sky.dn += 0.35;
        sky.de += 0.35;
        let e = sky.step();
        let s = hub.feed(&nmea_epoch(&e, None));
        if s == State::SuspectSpoofing && suspect_at.is_none() {
            suspect_at = Some(i);
        }
        if s == State::Spoofing && spoof_at.is_none() {
            spoof_at = Some(i);
        }
    }
    // 75 m at 0.5 m/s is 150 s, plus the 10 s sustain.
    let s = suspect_at.expect("suspect");
    assert!((150..=200).contains(&s), "{s} {:?}", hub.changes);
    // 300 m is 600 s.
    let f = spoof_at.expect("spoofing");
    assert!((600..=650).contains(&f), "{f}");
    assert!(!hub.changes.iter().any(|c| c.2 == Some(Cause::PositionJump)), "a slide is not a jump");
    let st = hub.mon.status();
    assert_eq!(st.cause.unwrap().cause, Cause::OffHome);
    assert!(st.metrics.home_dist_m.unwrap() > 300.0);

    // The operator really moved the hub: forget home, learn the new place, all clear.
    hub.mon.forget_home();
    run_nmea(&mut hub, &mut sky, 1000);
    assert_eq!(hub.mon.status().state, State::Ok, "{:?}", hub.changes);
    assert!(hub.mon.home().is_some());
}

#[test]
fn altitude_jump_is_caught() {
    let (mut hub, mut sky) = settled_nmea(35);
    sky.dz = 400.0;
    run_nmea(&mut hub, &mut sky, 15);
    assert_eq!(hub.mon.status().state, State::Spoofing, "{:?}", hub.changes);
    assert_eq!(hub.mon.status().cause.unwrap().cause, Cause::AltitudeOffHome);
    let v = hub.mon.status().cause.unwrap().value;
    assert!((395..=405).contains(&v), "{v}");
}

#[test]
fn position_jump_before_home_is_learned() {
    let mut hub = Hub::new();
    let mut sky = Sky::new(36, Env::Open);
    run_nmea(&mut hub, &mut sky, 100);
    assert!(hub.mon.home().is_none());
    sky.dn = 5_000.0;
    run_nmea(&mut hub, &mut sky, 3);
    assert_eq!(hub.mon.status().state, State::SuspectSpoofing, "{:?}", hub.changes);
    assert_eq!(hub.mon.status().cause.unwrap().cause, Cause::PositionJump);
}

#[test]
fn time_jump_is_caught_and_cleared() {
    let mut hub = Hub::new();
    let mut sky = Sky::new(37, Env::Open);
    run_nmea(&mut hub, &mut sky, 200);
    // UTC jumps 30 s ahead while position stays put.
    sky.dt_s = 30;
    run_nmea(&mut hub, &mut sky, 5);
    let st = *hub.mon.status();
    assert_eq!(st.state, State::SuspectSpoofing, "{:?}", hub.changes);
    assert_eq!(st.cause.unwrap().cause, Cause::TimeJump);
    let v = st.cause.unwrap().value;
    assert!((29_500..=30_500).contains(&v), "{v}");
    assert!(reason(&hub).contains("GPS time jumped"));
    // It stays suspect while the time stays wrong (the reference is not moved by bad time).
    run_nmea(&mut hub, &mut sky, 200);
    assert_eq!(hub.mon.status().state, State::SuspectSpoofing);
    // Real time back: the latch runs out, then the clear-down.
    sky.dt_s = 0;
    run_nmea(&mut hub, &mut sky, (integrity::LATCH_EPOCHS + integrity::CLEAR_EPOCHS + 5) as usize);
    assert_eq!(hub.mon.status().state, State::Ok, "{:?}", hub.changes);
}

#[test]
fn hub_clock_drift_and_jitter_are_not_time_jumps() {
    // GPS lost for an hour (hub crystal at the edge of the budget, 20 ppm: 72 ms), then back.
    let mut hub = Hub::new();
    let mut sky = Sky::new(38, Env::Open);
    run_nmea(&mut hub, &mut sky, 100);
    hub.now += 3_600_000 + 72;
    sky.t += 3600;
    run_nmea(&mut hub, &mut sky, 100);
    assert_eq!(hub.worst(), State::Ok, "{:?}", hub.changes);
}

#[test]
fn ublox_spoof_flag() {
    let mut hub = Hub::new();
    let mut sky = Sky::new(41, Env::Open);
    let mut x = UbxExtras::default();
    x.clk_drift = 0;
    run_ubx(&mut hub, &mut sky, &x, 100);
    x.spoof = 2;
    run_ubx(&mut hub, &mut sky, &x, 3);
    assert_eq!(hub.mon.status().state, State::SuspectSpoofing);
    assert!(reason(&hub).contains("spoofing indicated"));
    x.spoof = 3;
    run_ubx(&mut hub, &mut sky, &x, 3);
    assert_eq!(hub.mon.status().state, State::Spoofing);
    let f = hub.mon.status().cause.unwrap();
    assert_eq!((f.cause, f.cause.kind(), f.value), (Cause::RxSpoofFlag, 1, 3));
}

#[test]
fn ublox_clock_step_only_corroborates() {
    let mut hub = Hub::new();
    let mut sky = Sky::new(42, Env::Open);
    let mut x = UbxExtras::default();
    x.clk_drift = 0;
    run_ubx(&mut hub, &mut sky, &x, 100);
    // A 20 us bias step: alone, no alarm.
    x.clk_bias += 20_000;
    run_ubx(&mut hub, &mut sky, &x, 5);
    assert_eq!(hub.mon.status().state, State::Ok);
    // ...but with the receiver's own spoofing hint it becomes strong.
    x.clk_bias += 20_000;
    x.spoof = 2;
    run_ubx(&mut hub, &mut sky, &x, 3);
    assert_eq!(hub.mon.status().state, State::Spoofing, "{:?}", hub.changes);
    assert_eq!(hub.mon.status().cause.unwrap().cause, Cause::RxSpoofFlag);
    assert_eq!(hub.mon.status().checks, 2);
}

#[test]
fn impossible_satellites_only_corroborate() {
    let mut hub = Hub::new();
    let mut sky = Sky::new(43, Env::Open);
    run_nmea(&mut hub, &mut sky, 100);
    sky.models.push(SatModel { prn: 77, elev0: 30.0, rate: 0.0, azim: 90 }); // not a GPS number
    run_nmea(&mut hub, &mut sky, 30);
    assert_eq!(hub.mon.status().state, State::Ok);
    assert!(!hub.mon.status().clean || hub.mon.status().metrics.listed > 0);
    // With satellites stronger than the sky allows as well: spoofing.
    sky.spoof_cn0 = None;
    sky.offset_db = 15;
    run_nmea(&mut hub, &mut sky, 10);
    let st = hub.mon.status();
    assert_eq!(st.state, State::Spoofing, "{:?}", hub.changes);
    assert_eq!(st.cause.unwrap().cause, Cause::StrongCn0);
}

#[test]
fn cause_kinds_and_names() {
    assert_eq!(Cause::RxJamFlag.kind(), 1);
    assert_eq!(Cause::Cn0Collapse.kind(), 4);
    assert_eq!(Cause::RxSpoofFlag.kind(), 1);
    assert_eq!(Cause::ImpossibleSat.kind(), 10);
    assert_eq!(Cause::TimeFloor.kind(), 11);
    assert!(Cause::OffHome.is_spoofing() && !Cause::AgcDrop.is_spoofing());
    assert_eq!(State::SuspectSpoofing.name(), "suspect_spoofing");
    assert!(State::Spoofing > State::Jamming);
    assert_eq!(Level::default(), Level::None);
}

#[test]
fn local_maths() {
    for v in [0.0f32, 1e-4, 0.25, 2.0, 1e6] {
        let r = integrity::sqrt(v);
        assert!((r * r - v).abs() <= 1e-4 * v.max(1e-3), "{v}");
    }
    // 1 degree of latitude, and of longitude at 60 degrees.
    assert!((integrity::dist_m(0.0, 0.0, 1.0, 0.0) - 111_195.0).abs() < 1.0);
    assert!((integrity::dist_m(60.0, 0.0, 60.0, 1.0) - 55_597.0).abs() < 5.0);
    assert!((integrity::dist_m(10.0, 179.9, 10.0, -179.9) - 21_901.0).abs() < 50.0, "across the date line");
}

// ------------------------------------------------- WP-12: time and trust --

use crate::trust::{Pub, Publisher};

/// Steps of 1 s, 0.5 s and 0.3 s of the receiver's second against the hub's clock (with and
/// without PPS): each raises TimeJump within two epochs, no epoch after it is trusted (so its
/// time is never published or adopted), and it stays raised while the step lasts. Before
/// WP-12 anything under 2 s passed.
#[test]
fn sub_second_time_steps_are_caught_and_not_adopted() {
    for &step in &[1_000i64, 500, 300, -500] {
        for pps in [false, true] {
            let mut hub = Hub::new();
            hub.pps = pps;
            let mut sky = Sky::new(40, Env::Open);
            run_nmea(&mut hub, &mut sky, 200);
            assert!(hub.mon.status().trusted);
            assert!(hub.mon.status().metrics.time_disciplined);
            hub.shift_ms = step;
            let first = hub.trusted.len();
            run_nmea(&mut hub, &mut sky, 2);
            let st = *hub.mon.status();
            assert_eq!(st.state, State::SuspectSpoofing, "step {step} pps {pps}: {:?}", hub.changes);
            assert_eq!(st.cause.unwrap().cause, Cause::TimeJump);
            run_nmea(&mut hub, &mut sky, 300);
            assert_eq!(hub.mon.status().state, State::SuspectSpoofing, "stays while it lasts");
            assert!(hub.trusted[first..].iter().all(|t| !t), "no stepped epoch is trusted");
            let v = hub.mon.status().cause.unwrap().value;
            assert!((i64::from(v) + step).abs() < 100, "step {step}: {v}");
        }
    }
}

/// An honest receiver for an hour, hub crystal at the edge of the budget (+-20 ppm), +-40 ms
/// arrival jitter: never an alarm, trusted throughout once established.
#[test]
fn honest_hour_at_the_crystal_edge() {
    for ppm in [20, -20] {
        let mut hub = Hub::new();
        hub.crystal_ppm = ppm;
        hub.pps = ppm > 0;
        let mut sky = Sky::new(41, Env::Open);
        run_nmea(&mut hub, &mut sky, 3600);
        assert_eq!(hub.worst(), State::Ok, "{ppm}: {:?} {:?} {:?} at {}", hub.changes, hub.mon.status().cause, hub.mon.time_guard(), hub.states.iter().position(|s| *s != State::Ok).unwrap_or(0));
        assert!(hub.trusted[integrity::TRUST_EPOCHS as usize..].iter().all(|&t| t));
    }
}

/// A slow walk of the receiver's time (2 ms a second, no PPS) is caught within 150 ms /
/// (2000 - 20) ppm, about 76 s, plus the sustain.
#[test]
fn slow_time_walk_is_caught() {
    let mut hub = Hub::new();
    let mut sky = Sky::new(42, Env::Open);
    run_nmea(&mut hub, &mut sky, 200);
    let mut at = None;
    for i in 0..300 {
        hub.shift_ms += 2;
        let e = sky.step();
        if hub.feed(&nmea_epoch(&e, None)) == State::SuspectSpoofing {
            at = Some(i);
            break;
        }
    }
    let at = at.expect("caught");
    assert!((40..=110).contains(&at), "{at}");
}

/// The hub's publication as gps.rs applies it: (lat, lon) and UTC ms.
fn publish(p: &mut Publisher<(f64, f64), i64>, hub: &Hub, e: &Epoch) -> (Pub<(f64, f64)>, Pub<i64>) {
    let fix = e.fix.then_some((e.lat, e.lon));
    let o = p.epoch(hub.mon.status().trusted, fix, e.fix.then_some(e.utc * 1000));
    (o.fix, o.time)
}

/// A spoofer on from power-up (uniform signal strengths, another place, another time): the
/// hub never trusts an epoch, so it publishes no position and no time, held or otherwise.
/// Before WP-12 the first epochs' (spoofed) position and time were published and then held.
/// When the spoofer goes, trust is established after a clean period and publication resumes
/// with the real position and time.
#[test]
fn spoofer_from_boot_publishes_nothing_until_trust() {
    let mut hub = Hub::new();
    let mut sky = Sky::new(43, Env::Open);
    sky.spoof_cn0 = Some(47);
    sky.dn = 2_000.0;
    sky.dt_s = 3_600;
    let mut p = Publisher::new();
    for _ in 0..600 {
        let e = sky.step();
        hub.feed(&nmea_epoch(&e, None));
        let (f, t) = publish(&mut p, &hub, &e);
        assert_eq!((f, t), (Pub::Withhold, Pub::Withhold));
    }
    assert!(hub.mon.status().hold());
    sky.spoof_cn0 = None;
    sky.dn = 0.0;
    sky.dt_s = 0;
    let mut live_at = None;
    for i in 0..300 {
        let e = sky.step();
        hub.feed(&nmea_epoch(&e, None));
        let (f, t) = publish(&mut p, &hub, &e);
        if let (Pub::Live((lat, _)), Pub::Live(utc)) = (f, t) {
            assert!((lat - HOME_LAT).abs() < 1e-3);
            assert_eq!(utc, e.utc * 1000);
            live_at = Some(i);
            break;
        }
        assert_eq!((f, t), (Pub::Withhold, Pub::Withhold), "epoch {i}");
    }
    let at = live_at.expect("trust established");
    // The 2 km jump stays counted 60 epochs, then 30 to clear, then 10 clean.
    assert!(at <= 110, "{at} {:?}", hub.changes);
}

/// With trust established, a later spoof holds the last trusted values (never the receiver's).
#[test]
fn hold_after_trust_publishes_the_last_trusted() {
    let mut hub = Hub::new();
    let mut sky = Sky::new(44, Env::Open);
    let mut p = Publisher::new();
    let mut last = None;
    for _ in 0..100 {
        let e = sky.step();
        hub.feed(&nmea_epoch(&e, None));
        publish(&mut p, &hub, &e);
        last = Some(e.utc * 1000);
    }
    sky.spoof_cn0 = Some(47);
    sky.dt_s = 600;
    for _ in 0..50 {
        let e = sky.step();
        hub.feed(&nmea_epoch(&e, None));
        let (_, t) = publish(&mut p, &hub, &e);
        assert!(matches!(t, Pub::Held(u) if u <= last.unwrap()), "{t:?}");
    }
}

/// Home is remembered: a second boot given the first boot's learned home has it from the
/// start, and a spoofer present from that boot is caught by the position check at once.
#[test]
fn remembered_home_survives_a_restart() {
    let (mut hub, _) = settled_nmea(45);
    let Some(integrity::HomeEvent::Learned { lat, lon, alt }) = hub.mon.take_home_event() else { panic!("learned event") };
    assert_eq!(hub.mon.take_home_event(), None, "once");
    // Restart, honest sky: home from the card, no new event, no alarm.
    let mut hub = Hub::new();
    assert!(hub.mon.set_home(lat, lon, alt, integrity::HomeOrigin::Remembered));
    let mut sky = Sky::new(46, Env::Open);
    run_nmea(&mut hub, &mut sky, 1000);
    assert_eq!(hub.worst(), State::Ok, "{:?}", hub.changes);
    assert_eq!(hub.mon.home_origin(), Some(integrity::HomeOrigin::Remembered));
    assert_eq!(hub.mon.take_home_event(), None);
    // Restart with a careful spoofer from power-up (realistic C/N0) placing the hub 500 m
    // away: caught by the remembered home within the sustain.
    let mut hub = Hub::new();
    hub.mon.set_home(lat, lon, alt, integrity::HomeOrigin::Remembered);
    let mut sky = Sky::new(47, Env::Open);
    sky.dn = 500.0;
    run_nmea(&mut hub, &mut sky, 15);
    assert_eq!(hub.mon.status().state, State::Spoofing, "{:?}", hub.changes);
    assert_eq!(hub.mon.status().cause.unwrap().cause, Cause::OffHome);
}

/// A newly learned place that disagrees with the remembered home is not adopted: the position
/// check keeps alarming, a Pending event names it, and only the operator's confirmation makes
/// it home.
#[test]
fn new_home_waits_for_the_operator() {
    let (mut hub, _) = settled_nmea(48);
    let Some(integrity::HomeEvent::Learned { lat, lon, alt }) = hub.mon.take_home_event() else { panic!() };
    let mut hub = Hub::new();
    hub.mon.set_home(lat, lon, alt, integrity::HomeOrigin::Remembered);
    let mut sky = Sky::new(49, Env::Open);
    sky.dn = 800.0; // moved while switched off (or a spoofer)
    run_nmea(&mut hub, &mut sky, 950);
    assert_eq!(hub.mon.status().state, State::Spoofing);
    match hub.mon.take_home_event() {
        Some(integrity::HomeEvent::Pending { dist_m, .. }) => assert!((780.0..820.0).contains(&dist_m), "{dist_m}"),
        e => panic!("{e:?}"),
    }
    assert!(hub.mon.status().metrics.home_pending);
    let (hl, _, _) = hub.mon.home().unwrap();
    assert!((hl - lat).abs() < 1e-6, "home not moved");
    // Another 15 minutes: still not adopted, no second event.
    run_nmea(&mut hub, &mut sky, 950);
    assert_eq!(hub.mon.take_home_event(), None);
    assert_eq!(hub.mon.status().state, State::Spoofing);
    // The operator confirms.
    let (nl, _, _) = hub.mon.confirm_home().expect("pending");
    assert!((nl - (HOME_LAT + 800.0 / 111_195.0)).abs() < 1e-4);
    run_nmea(&mut hub, &mut sky, (integrity::CLEAR_EPOCHS + 5) as usize);
    assert_eq!(hub.mon.status().state, State::Ok, "{:?}", hub.changes);
    assert_eq!(hub.mon.home_origin(), Some(integrity::HomeOrigin::Learned));
    assert!(hub.mon.confirm_home().is_none(), "nothing waiting");
}

/// A placed home (site file) is the strongest prior: replaces a remembered one, and the
/// operator cannot confirm a learned place over it (the file must be edited).
#[test]
fn placed_home_wins() {
    let mut hub = Hub::new();
    assert!(hub.mon.set_home(HOME_LAT, HOME_LON, None, integrity::HomeOrigin::Remembered));
    assert!(!hub.mon.set_home(1.0, 1.0, None, integrity::HomeOrigin::Remembered), "a remembered one only fills an empty home");
    assert!(hub.mon.set_home(HOME_LAT, HOME_LON, Some(HOME_ALT), integrity::HomeOrigin::Placed));
    let mut sky = Sky::new(50, Env::Open);
    sky.dn = 900.0;
    run_nmea(&mut hub, &mut sky, 950);
    assert!(hub.mon.status().metrics.home_pending);
    assert!(hub.mon.confirm_home().is_none());
    assert_eq!(hub.mon.home_origin(), Some(integrity::HomeOrigin::Placed));
}

/// GPS time before the remembered time is untrusted: TimeFloor, nothing trusted, no time
/// published. GPS time after it is fine. The operator's "accept GPS time" clears it.
#[test]
fn time_before_the_remembered_time_is_untrusted() {
    let mut hub = Hub::new();
    hub.mon.set_time_floor((START_UTC - 30 * 86_400) * 1000);
    hub.mon.set_remembered_time(Some((START_UTC + 86_400) * 1000));
    let mut sky = Sky::new(51, Env::Open);
    let mut p = Publisher::new();
    for _ in 0..100 {
        let e = sky.step();
        hub.feed(&nmea_epoch(&e, None));
        assert_eq!(publish(&mut p, &hub, &e), (Pub::Withhold, Pub::Withhold));
    }
    let st = *hub.mon.status();
    assert_eq!(st.state, State::SuspectSpoofing);
    assert_eq!(st.cause.unwrap().cause, Cause::TimeFloor);
    assert!(st.metrics.time_floor);
    assert!(reason(&hub).contains("before the build date or the last time the hub trusted"));
    hub.mon.accept_time();
    run_nmea(&mut hub, &mut sky, (integrity::CLEAR_EPOCHS + integrity::TRUST_EPOCHS + 2) as usize);
    assert!(hub.mon.status().trusted, "{:?}", hub.changes);

    // Remembered time in the past: no complaint.
    let mut hub = Hub::new();
    hub.mon.set_remembered_time(Some((START_UTC - 3_600) * 1000));
    run_nmea(&mut hub, &mut Sky::new(52, Env::Open), 50);
    assert_eq!(hub.worst(), State::Ok);
}

/// GPS time before this firmware's build date is impossible.
#[test]
fn time_before_the_build_date_is_impossible() {
    let mut hub = Hub::new();
    hub.mon.set_time_floor((START_UTC + 400 * 86_400) * 1000);
    run_nmea(&mut hub, &mut Sky::new(53, Env::Open), 50);
    let st = *hub.mon.status();
    assert_eq!(st.cause.unwrap().cause, Cause::TimeFloor, "{st:?}");
    assert!(st.cause.unwrap().value <= -400 * 86_400 + 60, "{st:?}");
    assert!(hub.trusted.iter().all(|t| !t));
}

/// The monitor lives inside the hub's `Gps`, on the main task's stack.
#[test]
fn monitor_size() {
    let m = core::mem::size_of::<Monitor>();
    let g = core::mem::size_of::<crate::timeguard::TimeGuard>();
    let p = core::mem::size_of::<Publisher<(f64, f64, Option<f64>, u64, Option<f32>, Option<f32>, bool), (u64, u64, bool)>>();
    std::println!("Monitor {m} bytes, TimeGuard {g}, Publisher(hub types) {p}");
    assert!(m <= 1024, "{m}");
}
