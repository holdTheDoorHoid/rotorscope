//! Host tests. The helpers here deliberately re-implement framing and checksums
//! independently of the crate so a bug in the crate cannot cancel itself out.

use crate::config::{self, keys, BuildError, ConfigOptions, Value};
use crate::*;
use std::vec::Vec;

// ---------- independent reference implementations ----------

fn fl(data: &[u8]) -> (u8, u8) {
    let (mut a, mut b) = (0u32, 0u32);
    for &x in data {
        a = (a + x as u32) & 0xff;
        b = (b + a) & 0xff;
    }
    (a as u8, b as u8)
}

fn ubx(class: u8, id: u8, payload: &[u8]) -> Vec<u8> {
    let mut v = std::vec![0xB5, 0x62, class, id, payload.len() as u8, (payload.len() >> 8) as u8];
    v.extend_from_slice(payload);
    let (a, b) = fl(&v[2..]);
    v.push(a);
    v.push(b);
    v
}

fn nmea(body: &str) -> Vec<u8> {
    let x = body.bytes().fold(0u8, |a, b| a ^ b);
    std::format!("${body}*{x:02X}\r\n").into_bytes()
}

#[derive(Clone)]
struct Pvt {
    itow: u32,
    year: u16,
    month: u8,
    day: u8,
    hour: u8,
    min: u8,
    sec: u8,
    valid: u8,
    tacc: u32,
    nano: i32,
    fix: u8,
    flags: u8,
    numsv: u8,
    lon: i32,
    lat: i32,
    height: i32,
    hmsl: i32,
    hacc: u32,
    vacc: u32,
    gspeed: i32,
    head: i32,
    pdop: u16,
    flags3: u16,
}

impl Default for Pvt {
    fn default() -> Self {
        // 2024-03-15 12:34:56.250 UTC, 3D fix near Philadelphia.
        Pvt {
            itow: 303_296_250,
            year: 2024,
            month: 3,
            day: 15,
            hour: 12,
            min: 34,
            sec: 56,
            valid: 0b0111,
            tacc: 25,
            nano: 250_000_000,
            fix: 3,
            flags: 0x01,
            numsv: 14,
            lon: -751_652_000,   // -75.1652
            lat: 399_526_000,    // 39.9526
            height: 30_500,      // ellipsoid 30.5 m
            hmsl: 70_250,        // MSL 70.25 m
            hacc: 1_800,
            vacc: 2_900,
            gspeed: 12_345,
            head: 9_000_000, // 90 deg
            pdop: 123,
            flags3: 0,
        }
    }
}

impl Pvt {
    fn payload(&self) -> Vec<u8> {
        let mut p = std::vec![0u8; 92];
        p[0..4].copy_from_slice(&self.itow.to_le_bytes());
        p[4..6].copy_from_slice(&self.year.to_le_bytes());
        p[6] = self.month;
        p[7] = self.day;
        p[8] = self.hour;
        p[9] = self.min;
        p[10] = self.sec;
        p[11] = self.valid;
        p[12..16].copy_from_slice(&self.tacc.to_le_bytes());
        p[16..20].copy_from_slice(&self.nano.to_le_bytes());
        p[20] = self.fix;
        p[21] = self.flags;
        p[23] = self.numsv;
        p[24..28].copy_from_slice(&self.lon.to_le_bytes());
        p[28..32].copy_from_slice(&self.lat.to_le_bytes());
        p[32..36].copy_from_slice(&self.height.to_le_bytes());
        p[36..40].copy_from_slice(&self.hmsl.to_le_bytes());
        p[40..44].copy_from_slice(&self.hacc.to_le_bytes());
        p[44..48].copy_from_slice(&self.vacc.to_le_bytes());
        p[60..64].copy_from_slice(&self.gspeed.to_le_bytes());
        p[64..68].copy_from_slice(&self.head.to_le_bytes());
        p[76..78].copy_from_slice(&self.pdop.to_le_bytes());
        p[78..80].copy_from_slice(&self.flags3.to_le_bytes());
        p
    }
    fn frame(&self) -> Vec<u8> {
        ubx(0x01, 0x07, &self.payload())
    }
}

fn all_events(chunks: &[&[u8]]) -> Vec<Event> {
    let mut p = Parser::new();
    let mut out = Vec::new();
    for c in chunks {
        out.extend(p.feed(c));
    }
    out
}

fn events(data: &[u8]) -> Vec<Event> {
    all_events(&[data])
}

fn fixes(ev: &[Event]) -> Vec<Fix> {
    ev.iter().filter_map(|e| if let Event::Fix(f) = e { Some(*f) } else { None }).collect()
}

fn one_fix(data: &[u8]) -> Fix {
    let f = fixes(&events(data));
    assert_eq!(f.len(), 1, "expected exactly one fix, got {f:?}");
    f[0]
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

// ---------- UBX NAV-PVT ----------

#[test]
fn pvt_3d_fix() {
    let f = one_fix(&Pvt::default().frame());
    assert_eq!(f.source, Source::UbxNavPvt);
    assert_eq!(f.fix, FixType::ThreeD);
    assert!(f.fix_ok && f.position_valid && f.is_usable());
    assert!((f.lat - 39.9526).abs() < 1e-9);
    assert!((f.lon - -75.1652).abs() < 1e-9);
    assert_eq!(f.alt_ellipsoid_m, Some(30.5));
    assert_eq!(f.alt_msl_m, Some(70.25));
    assert_eq!(f.h_acc_m, Some(1.8));
    assert_eq!(f.v_acc_m, Some(2.9));
    assert_eq!(f.sats_used, 14);
    assert!((f.speed_mps.unwrap() - 12.345).abs() < 1e-4);
    assert!((f.heading_deg.unwrap() - 90.0).abs() < 1e-3);
    assert!((f.pdop.unwrap() - 1.23).abs() < 1e-4);
    assert_eq!(f.itow_ms, Some(303_296_250));
    assert_eq!(f.time_acc_ns, Some(25));
    assert!(f.utc_fully_resolved);
    let t = f.utc.unwrap();
    assert_eq!((t.year, t.month, t.day, t.hour, t.minute, t.second, t.millis), (2024, 3, 15, 12, 34, 56, 250));
    assert_eq!(t.unix_ms(), 1_710_506_096_250);
    assert_eq!(f.position(), Some((f.lat, f.lon)));
}

#[test]
fn pvt_negative_lat_lon() {
    let mut p = Pvt::default();
    p.lat = -338_688_000; // Sydney -33.8688
    p.lon = 1_512_093_000; // 151.2093
    let f = one_fix(&p.frame());
    assert!((f.lat - -33.8688).abs() < 1e-9 && (f.lon - 151.2093).abs() < 1e-9);

    p.lat = -346_037_000; // Buenos Aires -34.6037, -58.3816
    p.lon = -583_816_000;
    let f = one_fix(&p.frame());
    assert!((f.lat - -34.6037).abs() < 1e-9 && (f.lon - -58.3816).abs() < 1e-9);
}

#[test]
fn pvt_no_fix() {
    let mut p = Pvt::default();
    p.fix = 0;
    p.flags = 0;
    p.valid = 0;
    p.numsv = 0;
    p.flags3 = 1; // invalidLlh
    p.lat = 123_456;
    p.lon = 654_321;
    let f = one_fix(&p.frame());
    assert_eq!(f.fix, FixType::None);
    assert!(!f.fix_ok && !f.position_valid && !f.is_usable());
    assert_eq!((f.lat, f.lon), (0.0, 0.0));
    assert_eq!(f.position(), None);
    assert_eq!(f.alt_ellipsoid_m, None);
    assert_eq!(f.h_acc_m, None);
    assert_eq!(f.speed_mps, None);
    assert_eq!(f.utc, None);
    assert_eq!(f.sats_used, 0);
}

#[test]
fn pvt_invalid_time_but_good_fix() {
    let mut p = Pvt::default();
    p.valid = 0; // no validDate, validTime
    let f = one_fix(&p.frame());
    assert!(f.is_usable());
    assert_eq!(f.utc, None);

    p.valid = 0b001; // date only
    assert_eq!(one_fix(&p.frame()).utc, None);
    p.valid = 0b010; // time only
    assert_eq!(one_fix(&p.frame()).utc, None);
    p.valid = 0b011; // both, but nonsense month
    p.month = 13;
    assert_eq!(one_fix(&p.frame()).utc, None);
}

#[test]
fn pvt_unknown_time_accuracy_and_negative_nano() {
    let mut p = Pvt::default();
    p.tacc = 0xFFFF_FFFF;
    p.nano = -5_000;
    let f = one_fix(&p.frame());
    assert_eq!(f.time_acc_ns, None);
    assert_eq!(f.utc.unwrap().millis, 0);
}

#[test]
fn pvt_usable_rules() {
    let mut p = Pvt::default();
    p.fix = 2; // 2D
    p.hacc = 20_000;
    let f = one_fix(&p.frame());
    assert_eq!(f.fix, FixType::TwoD);
    assert!(f.is_usable());
    assert_eq!(f.alt_ellipsoid_m, None, "no altitude on a 2D fix");
    p.hacc = 80_000;
    assert!(!one_fix(&p.frame()).is_usable());
    p.hacc = 50_000; // boundary: must be < 50 m
    assert!(!one_fix(&p.frame()).is_usable());
    p.fix = 3;
    p.flags = 0; // gnssFixOK clear
    assert!(!one_fix(&p.frame()).is_usable());
    p.fix = 1; // dead reckoning only
    p.flags = 1;
    assert!(!one_fix(&p.frame()).is_usable());
    p.fix = 5; // time only
    let f = one_fix(&p.frame());
    assert!(!f.position_valid && !f.is_usable());
    p.fix = 4; // GNSS + DR
    assert!(one_fix(&p.frame()).is_usable());
}

#[test]
fn pvt_heading_is_normalised() {
    let mut p = Pvt::default();
    p.head = -9_000_000; // -90 deg
    assert!((one_fix(&p.frame()).heading_deg.unwrap() - 270.0).abs() < 1e-3);
    p.head = 0;
    assert_eq!(one_fix(&p.frame()).heading_deg, Some(0.0));
}

#[test]
fn pvt_wrong_length_ignored() {
    let short = ubx(0x01, 0x07, &[0u8; 91]);
    assert!(fixes(&events(&short)).is_empty());
    // A longer payload (future firmware) is accepted using the first 92 bytes.
    let mut long = Pvt::default().payload();
    long.extend_from_slice(&[0; 8]);
    assert_eq!(fixes(&events(&ubx(0x01, 0x07, &long))).len(), 1);
}

#[test]
fn unix_ms_known_dates() {
    let t = |y, mo, d, h, mi, s| UtcTime { year: y, month: mo, day: d, hour: h, minute: mi, second: s, millis: 0 };
    assert_eq!(t(1970, 1, 1, 0, 0, 0).unix_ms(), 0);
    assert_eq!(t(2000, 2, 29, 0, 0, 0).unix_ms(), 951_782_400_000);
    assert_eq!(t(2038, 1, 19, 3, 14, 8).unix_ms(), 2_147_483_648_000);
    assert_eq!(t(1969, 12, 31, 23, 59, 59).unix_ms(), -1000);
}

// ---------- NMEA ----------

#[test]
fn nmea_known_real_sentences() {
    // Well-known sample sentences with their published checksums.
    let gga = b"$GPGGA,092750.000,5321.6802,N,00630.3372,W,1,8,1.03,61.7,M,55.2,M,,*76\r\n";
    let rmc = b"$GPRMC,083559.00,A,4717.11437,N,00833.91522,E,0.004,77.52,091202,,,A*57\r\n";
    assert_eq!(nmea("GPGGA,092750.000,5321.6802,N,00630.3372,W,1,8,1.03,61.7,M,55.2,M,,"), gga.to_vec());
    assert_eq!(nmea("GPRMC,083559.00,A,4717.11437,N,00833.91522,E,0.004,77.52,091202,,,A"), rmc.to_vec());

    // A lone sentence is released when the next one of the same kind arrives.
    let f = fixes(&events(&[&gga[..], &gga[..]].concat()))[0];
    assert_eq!(f.source, Source::Nmea);
    assert_eq!(f.fix, FixType::ThreeD);
    assert!((f.lat - (53.0 + 21.6802 / 60.0)).abs() < 1e-9);
    assert!((f.lon - -(6.0 + 30.3372 / 60.0)).abs() < 1e-9);
    assert_eq!(f.sats_used, 8);
    assert_eq!(f.hdop, Some(1.03));
    assert_eq!(f.alt_msl_m, Some(61.7));
    assert!((f.alt_ellipsoid_m.unwrap() - 116.9).abs() < 1e-3);
    assert_eq!(f.h_acc_m, None);
    assert_eq!(f.v_acc_m, None);
    assert_eq!(f.speed_mps, None);
    assert_eq!(f.utc, None, "GGA carries no date");
    assert!(f.is_usable());

    let f = fixes(&events(&[&rmc[..], &rmc[..]].concat()))[0];
    assert!((f.lat - (47.0 + 17.11437 / 60.0)).abs() < 1e-9);
    assert!((f.lon - (8.0 + 33.91522 / 60.0)).abs() < 1e-9);
    assert_eq!(f.fix, FixType::TwoD, "RMC alone has no altitude");
    assert!(!f.is_usable(), "2D without an accuracy figure is not usable");
    assert!((f.speed_mps.unwrap() - 0.004 * 0.514_444).abs() < 1e-5);
    assert!((f.heading_deg.unwrap() - 77.52).abs() < 1e-3);
    let t = f.utc.unwrap();
    assert_eq!((t.year, t.month, t.day, t.hour, t.minute, t.second), (2002, 12, 9, 8, 35, 59));
}

fn gn_pair(time: &str) -> Vec<u8> {
    let mut v = nmea(&std::format!("GNRMC,{time},A,3957.1560,N,07509.9120,W,5.40,181.5,150324,,,A,V"));
    v.extend(nmea("GNVTG,181.5,T,,M,5.40,N,10.00,K,A"));
    v.extend(nmea(&std::format!("GNGGA,{time},3957.1560,N,07509.9120,W,1,12,0.80,70.2,M,-34.0,M,,")));
    v.extend(nmea("GNGSA,A,3,01,02,03,04,05,06,,,,,,,1.50,0.80,1.30,1"));
    v
}

#[test]
fn nmea_gn_pair_merges_into_one_fix() {
    let f = one_fix(&gn_pair("123456.00"));
    assert_eq!(f.fix, FixType::ThreeD);
    assert!((f.lat - 39.9526).abs() < 1e-9, "{}", f.lat);
    assert!((f.lon - -75.1652).abs() < 1e-9);
    assert_eq!(f.sats_used, 12);
    assert_eq!(f.alt_msl_m, Some(70.2));
    assert!((f.alt_ellipsoid_m.unwrap() - 36.2).abs() < 1e-4, "ellipsoid = MSL + separation");
    assert!((f.speed_mps.unwrap() - 2.778).abs() < 1e-3);
    assert!((f.heading_deg.unwrap() - 181.5).abs() < 1e-4);
    let t = f.utc.unwrap();
    assert_eq!((t.year, t.month, t.day, t.hour, t.minute, t.second, t.millis), (2024, 3, 15, 12, 34, 56, 0));
    assert!(f.is_usable());
}

#[test]
fn nmea_any_talker() {
    for talker in ["GP", "GL", "GN", "GA", "GB", "GQ"] {
        let mut s = nmea(&std::format!("{talker}RMC,010203.00,A,1000.0000,N,02000.0000,E,0.0,,010124,,,A"));
        s.extend(nmea(&std::format!("{talker}GGA,010203.00,1000.0000,N,02000.0000,E,1,05,1.0,10.0,M,0.0,M,,")));
        let f = one_fix(&s);
        assert!((f.lat - 10.0).abs() < 1e-9 && (f.lon - 20.0).abs() < 1e-9, "{talker}");
        assert_eq!(f.heading_deg, None, "empty course while stationary");
    }
}

#[test]
fn nmea_empty_gga_is_no_fix() {
    // What a cold M10 sends before it has anything: empty fields, quality 0.
    let mut s = nmea("GNRMC,,V,,,,,,,,,,N,V");
    s.extend(nmea("GNGGA,,,,,,0,00,99.99,,,,,,"));
    let f = one_fix(&s);
    assert_eq!(f.fix, FixType::None);
    assert!(!f.fix_ok && !f.position_valid && !f.is_usable());
    assert_eq!((f.lat, f.lon), (0.0, 0.0));
    assert_eq!(f.sats_used, 0);
    assert_eq!(f.utc, None);
    assert_eq!(f.alt_msl_m, None);

    // A lone empty GGA as well.
    let f = one_fix(&nmea("GNGGA,,,,,,0,00,99.99,,,,,,").iter().chain(nmea("GNGGA,,,,,,0,00,99.99,,,,,,").iter()).copied().collect::<Vec<_>>());
    assert_eq!(f.fix, FixType::None);
}

#[test]
fn nmea_void_rmc_with_time_is_not_trusted() {
    let mut s = nmea("GNRMC,123456.00,V,,,,,,,150324,,,N,V");
    s.extend(nmea("GNGGA,123456.00,,,,,0,00,99.99,,,,,,"));
    let f = one_fix(&s);
    assert_eq!(f.fix, FixType::None);
    assert_eq!(f.utc, None);
}

#[test]
fn nmea_gga_says_fix_rmc_says_void_means_no_fix() {
    let mut s = nmea("GNRMC,123456.00,V,3957.1560,N,07509.9120,W,0.0,,150324,,,N,V");
    s.extend(nmea("GNGGA,123456.00,3957.1560,N,07509.9120,W,1,12,0.80,70.2,M,-34.0,M,,"));
    let f = one_fix(&s);
    assert!(!f.fix_ok && !f.position_valid);
}

#[test]
fn nmea_lone_gga_flushes_on_next_epoch() {
    // RMC is switched off: every GGA stands alone and is released by the next epoch's GGA.
    let mut s = nmea("GNGGA,123456.00,3957.1560,N,07509.9120,W,1,12,0.80,70.2,M,-34.0,M,,");
    s.extend(nmea("GNGGA,123457.00,3957.1561,N,07509.9120,W,1,12,0.80,70.2,M,-34.0,M,,"));
    s.extend(nmea("GNGGA,123458.00,3957.1562,N,07509.9120,W,1,12,0.80,70.2,M,-34.0,M,,"));
    let f = fixes(&events(&s));
    assert_eq!(f.len(), 2);
    assert!(f[0].lat < f[1].lat);
}

#[test]
fn nmea_southern_eastern_and_range_checks() {
    let mut s = nmea("GNRMC,010203.00,A,3352.1280,S,15112.5580,E,0.0,,010124,,,A,V");
    s.extend(nmea("GNGGA,010203.00,3352.1280,S,15112.5580,E,1,09,1.0,5.0,M,20.0,M,,"));
    let f = one_fix(&s);
    assert!((f.lat - -(33.0 + 52.128 / 60.0)).abs() < 1e-9);
    assert!((f.lon - (151.0 + 12.558 / 60.0)).abs() < 1e-9);

    // Latitude 95 degrees: malformed, ignored.
    let bad = nmea("GNGGA,010203.00,9500.0000,N,02000.0000,E,1,09,1.0,5.0,M,20.0,M,,");
    assert!(fixes(&events(&[bad.clone(), bad].concat())).is_empty());
    // Minutes >= 60.
    let bad = nmea("GNGGA,010203.00,1075.0000,N,02000.0000,E,1,09,1.0,5.0,M,20.0,M,,");
    assert!(fixes(&events(&[bad.clone(), bad].concat())).is_empty());
}

#[test]
fn nmea_dead_reckoning_quality() {
    let mut s = nmea("GNRMC,010203.00,A,1000.0000,N,02000.0000,E,0.0,,010124,,,E,V");
    s.extend(nmea("GNGGA,010203.00,1000.0000,N,02000.0000,E,6,00,1.0,5.0,M,0.0,M,,"));
    let f = one_fix(&s);
    assert_eq!(f.fix, FixType::DeadReckoning);
    assert!(!f.is_usable());
}

#[test]
fn nmea_proprietary_and_other_sentences_ignored() {
    let mut s = nmea("PUBX,00,081350.00,4717.113210,N,00833.915187,E,546.589,G3,2.1,2.0,0.007,77.52,0.007,,0.92,1.19,0.77,9,0,0");
    s.extend(nmea("GNGSV,3,1,11,01,40,083,46,02,17,308,41,12,07,344,39,14,22,228,45,0"));
    s.extend(nmea("GNTXT,01,01,02,u-blox AG - www.u-blox.com"));
    assert!(events(&s).is_empty());
}

#[test]
fn nmea_bad_checksum_rejected_then_recovers() {
    let good = gn_pair("123456.00");
    let mut bad = good.clone();
    // Corrupt one digit inside the first sentence (RMC).
    let i = bad.iter().position(|&b| b == b'5').unwrap();
    bad[i] = b'6';
    let mut p = Parser::new();
    assert!(p.feed(&bad).filter(|e| matches!(e, Event::Fix(_))).count() <= 1);
    assert!(p.stats().checksum_errors >= 1);
    // Parser still works.
    let f: Vec<_> = p.feed(&gn_pair("123457.00")).collect();
    assert!(f.iter().any(|e| matches!(e, Event::Fix(_))));

    // Missing checksum is rejected as well.
    assert!(events(b"$GNGGA,010203.00,1000.0000,N,02000.0000,E,1,09,1.0,5.0,M,0.0,M,,\r\n").is_empty());
    // Lower-case hex digits in the checksum are accepted.
    let mut found = false;
    for n in 0..200 {
        let mut s = nmea(&std::format!("GNGGA,010203.00,1000.0000,N,02000.0000,E,1,05,1.0,{n}.0,M,0.0,M,,"));
        let l = s.len();
        if s[l - 4].is_ascii_uppercase() || s[l - 3].is_ascii_uppercase() {
            s[l - 4] = s[l - 4].to_ascii_lowercase();
            s[l - 3] = s[l - 3].to_ascii_lowercase();
            let mut twice = s.clone();
            twice.extend(&s);
            assert_eq!(fixes(&events(&twice)).len(), 1);
            found = true;
            break;
        }
    }
    assert!(found);
}

#[test]
fn nmea_overlong_line_dropped_and_parser_survives() {
    let mut s = std::vec![b'$'];
    s.extend(core::iter::repeat(b'A').take(200));
    s.extend(b"\r\n");
    s.extend(gn_pair("123456.00"));
    assert_eq!(fixes(&events(&s)).len(), 1);
}

// ---------- stream robustness ----------

fn mixed_stream() -> (Vec<u8>, usize) {
    let mut p = Pvt::default();
    let mut s = Vec::new();
    let mut fixes = 0;
    for i in 0..6 {
        p.itow += 1000;
        p.sec = 56 + (i as u8 % 3);
        s.extend(p.frame());
        fixes += 1;
        s.extend(ubx(0x05, 0x01, &[0x06, 0x8A]));
        s.extend(gn_pair(&std::format!("12345{i}.00")));
        fixes += 1;
    }
    (s, fixes)
}

#[test]
fn byte_at_a_time_equals_whole() {
    let (s, n) = mixed_stream();
    let whole = events(&s);
    let bytes: Vec<&[u8]> = s.chunks(1).collect();
    let single = all_events(&bytes);
    assert_eq!(whole, single);
    assert_eq!(fixes(&whole).len(), n);
    assert_eq!(whole.iter().filter(|e| matches!(e, Event::Ack { .. })).count(), 6);
    // push() form as well
    let mut p = Parser::new();
    let pushed: Vec<Event> = s.iter().filter_map(|&b| p.push(b)).collect();
    assert_eq!(pushed, whole);
}

#[test]
fn random_chunking_is_equivalent() {
    let (s, _) = mixed_stream();
    let reference = events(&s);
    let mut rng = Rng(0x1234_5678_9abc_def1);
    for _ in 0..300 {
        let mut chunks: Vec<&[u8]> = Vec::new();
        let mut i = 0;
        while i < s.len() {
            let n = 1 + rng.below(150);
            let e = (i + n).min(s.len());
            chunks.push(&s[i..e]);
            i = e;
        }
        assert_eq!(all_events(&chunks), reference);
    }
}

#[test]
fn garbage_between_frames() {
    let mut rng = Rng(42);
    // Interleave noise that
    // contains lone 0xB5 and '$' bytes (but no 0xB5 0x62 pair, no newline).
    let mut s = Vec::new();
    let mut p = Pvt::default();
    let mut expect = 0;
    for i in 0..40 {
        for _ in 0..rng.below(60) {
            let mut b = rng.next() as u8;
            if b == b'\n' {
                b = 0x20;
            }
            s.push(b);
            if s.len() >= 2 && s[s.len() - 2] == 0xB5 && s[s.len() - 1] == 0x62 {
                s.pop();
            }
        }
        if i % 4 == 0 {
            s.push(0xB5);
            s.push(b'$');
        }
        p.itow += 1000;
        s.extend(if i % 2 == 0 { p.frame() } else { nmea_pair_for(i) });
        expect += 1;
    }
    let got = fixes(&events(&s));
    assert_eq!(got.len(), expect, "every real frame recovered");
}

fn nmea_pair_for(i: usize) -> Vec<u8> {
    gn_pair(&std::format!("1200{:02}.00", i % 60))
}

#[test]
fn bad_ubx_checksum_rejected_then_recovers() {
    let mut f = Pvt::default().frame();
    let n = f.len();
    f[n - 1] ^= 0x55;
    let mut data = f.clone();
    data.extend(Pvt::default().frame());
    let mut p = Parser::new();
    let ev: Vec<_> = p.feed(&data).collect();
    assert_eq!(fixes(&ev).len(), 1, "only the good frame");
    assert_eq!(p.stats().checksum_errors, 1);

    // A single flipped payload bit is caught too.
    let mut f = Pvt::default().frame();
    f[30] ^= 0x01;
    assert!(fixes(&events(&f)).is_empty());
}

#[test]
fn truncated_frame_followed_by_good_frame() {
    let good = Pvt::default().frame();
    let mut data = good[..50].to_vec();
    data.extend(&good);
    data.extend(&good);
    let ev = fixes(&events(&data));
    assert_eq!(ev.len(), 2, "the half frame is lost, both complete frames survive");
}

#[test]
fn false_header_hiding_a_real_frame() {
    // B5 62 + plausible class/id + length 100, then a real frame, then padding.
    let mut data = std::vec![0xB5, 0x62, 0x01, 0x35, 100, 0];
    data.extend(Pvt::default().frame());
    data.extend(core::iter::repeat(0u8).take(120));
    assert_eq!(fixes(&events(&data)).len(), 1);
}

#[test]
fn oversize_length_is_garbage_not_a_stall() {
    let mut data = std::vec![0xB5, 0x62, 0x01, 0x07, 0xFF, 0xFF];
    data.extend(Pvt::default().frame());
    assert_eq!(fixes(&events(&data)).len(), 1);
}

#[test]
fn ddc_idle_fill_between_frames() {
    let mut data = std::vec![0xFF; 37];
    data.extend(Pvt::default().frame());
    data.extend(std::vec![0xFF; 11]);
    data.extend(Pvt::default().frame());
    assert_eq!(fixes(&events(&data)).len(), 2);
    assert!(ddc::is_idle(&[0xFF; 4]) && !ddc::is_idle(&[0xFF, 0]));
    assert_eq!(ddc::bytes_available([0x01, 0x2C]), 300);
    assert_eq!((ddc::ADDRESS, ddc::REG_AVAIL_HI, ddc::REG_AVAIL_LO, ddc::REG_STREAM), (0x42, 0xFD, 0xFE, 0xFF));
}

#[test]
fn random_noise_never_panics() {
    let mut rng = Rng(7);
    let mut p = Parser::new();
    let mut buf = std::vec![0u8; 4096];
    for _ in 0..500 {
        for b in buf.iter_mut() {
            // bias towards the interesting bytes
            *b = match rng.below(8) {
                0 => 0xB5,
                1 => 0x62,
                2 => b'$',
                3 => b'\n',
                _ => rng.next() as u8,
            };
        }
        for _ in p.feed(&buf) {}
    }
}

#[test]
fn ack_nak_events() {
    let ev = events(&[ubx(0x05, 0x01, &[0x06, 0x8A]), ubx(0x05, 0x00, &[0x06, 0x8A])].concat());
    assert_eq!(ev, [Event::Ack { class: 0x06, id: 0x8A }, Event::Nak { class: 0x06, id: 0x8A }]);
}

#[test]
fn mon_ver_reply() {
    let mut p = std::vec![0u8; 40];
    p[..22].copy_from_slice(b"EXT CORE 5.10 (fc3e2e)");
    p[30..38].copy_from_slice(b"000A0000");
    for ext in ["ROM BASE 0x1F1C7E3D", "FWVER=SPG 5.10", "PROTVER=34.10", "MOD=MAX-M10S"] {
        let mut e = [0u8; 30];
        e[..ext.len()].copy_from_slice(ext.as_bytes());
        p.extend(e);
    }
    let ev = events(&ubx(0x0A, 0x04, &p));
    let Event::MonVer(v) = ev[0] else { panic!("{ev:?}") };
    assert_eq!(v.sw_str(), "EXT CORE 5.10 (fc3e2e)");
    assert_eq!(v.hw_str(), "000A0000");
    assert_eq!(v.protver, Some((34, 10)));
}

#[test]
fn val_get_reply() {
    let mut p = std::vec![1u8, 0, 0, 0];
    p.extend(keys::UART1_BAUDRATE.to_le_bytes());
    p.extend(115_200u32.to_le_bytes());
    p.extend(keys::MSGOUT_UBX_NAV_PVT_UART1.to_le_bytes());
    p.push(1);
    p.extend(keys::UART1OUTPROT_NMEA.to_le_bytes());
    p.push(0);
    let ev = events(&ubx(0x06, 0x8B, &p));
    let Event::ValGet(v) = ev[0] else { panic!("{ev:?}") };
    assert_eq!((v.layer, v.count, v.truncated), (0, 3, false));
    assert_eq!(v.get(keys::UART1_BAUDRATE), Some(115_200));
    assert_eq!(v.get(keys::MSGOUT_UBX_NAV_PVT_UART1), Some(1));
    assert_eq!(v.get(keys::UART1OUTPROT_NMEA), Some(0));
    assert_eq!(v.get(keys::TP_TP1_ENA), None);
}

// ---------- config builders ----------

fn check_frame(f: &[u8], class: u8, id: u8) -> &[u8] {
    assert_eq!(&f[..2], &[0xB5, 0x62]);
    assert_eq!((f[2], f[3]), (class, id));
    let n = u16::from_le_bytes([f[4], f[5]]) as usize;
    assert_eq!(f.len(), n + 8, "length field matches frame");
    let (a, b) = fl(&f[2..6 + n]);
    assert_eq!((f[6 + n], f[7 + n]), (a, b), "checksum");
    &f[6..6 + n]
}

/// Independent decoder for a VALSET payload: returns (version, layers, reserved, [(key, value)]).
fn decode_valset(p: &[u8]) -> (u8, u8, [u8; 2], Vec<(u32, u64)>) {
    let mut items = Vec::new();
    let mut o = 4;
    while o < p.len() {
        let key = u32::from_le_bytes(p[o..o + 4].try_into().unwrap());
        o += 4;
        let sz = match (key >> 28) & 7 {
            1 | 2 => 1,
            3 => 2,
            4 => 4,
            5 => 8,
            s => panic!("bad size code {s}"),
        };
        let mut v = 0u64;
        for i in 0..sz {
            v |= (p[o + i] as u64) << (8 * i);
        }
        o += sz;
        items.push((key, v));
    }
    assert_eq!(o, p.len());
    (p[0], p[1], [p[2], p[3]], items)
}

#[test]
fn valset_golden_layout() {
    let mut out = [0u8; 64];
    let n = config::valset(&mut out, config::LAYER_RAM | config::LAYER_BBR, &[(keys::MSGOUT_UBX_NAV_PVT_UART1, Value::U8(1))]).unwrap();
    let f = &out[..n];
    // B5 62 | 06 8A | len 9 | ver 0, layers 3, reserved 0 0 | key 07 00 91 20 | value 01 | ck
    assert_eq!(&f[..15], &[0xB5, 0x62, 0x06, 0x8A, 0x09, 0x00, 0x00, 0x03, 0x00, 0x00, 0x07, 0x00, 0x91, 0x20, 0x01]);
    check_frame(f, 0x06, 0x8A);
    assert_eq!(n, 17);
}

#[test]
fn valset_value_widths_little_endian() {
    let mut out = [0u8; 64];
    let n = config::valset(
        &mut out,
        1,
        &[
            (keys::UART1_BAUDRATE, Value::U32(115_200)),
            (keys::UART1OUTPROT_NMEA, Value::Bool(false)),
            (keys::NAVSPG_DYNMODEL, Value::U8(0)),
            (0x3005_0001, Value::U16(0x1234)), // CFG-TP-ANT_CABLEDELAY is I2; value width 2
        ],
    )
    .unwrap();
    let p = check_frame(&out[..n], 0x06, 0x8A);
    assert_eq!(&p[4..12], &[0x01, 0x00, 0x52, 0x40, 0x00, 0xC2, 0x01, 0x00], "115200 = 0x0001C200 LE");
    let (ver, layers, res, items) = decode_valset(p);
    assert_eq!((ver, layers, res), (0, 1, [0, 0]));
    assert_eq!(items, [(keys::UART1_BAUDRATE, 115_200), (keys::UART1OUTPROT_NMEA, 0), (keys::NAVSPG_DYNMODEL, 0), (0x3005_0001, 0x1234)]);
}

#[test]
fn valset_errors() {
    let mut out = [0u8; 64];
    assert_eq!(config::valset(&mut out, 3, &[(keys::UART1_BAUDRATE, Value::U8(1))]), Err(BuildError::ValueMismatch));
    assert_eq!(config::valset(&mut out, 3, &[(keys::TP_TP1_ENA, Value::U8(1))]), Err(BuildError::ValueMismatch));
    let mut tiny = [0u8; 16];
    assert_eq!(config::valset(&mut tiny, 3, &[(keys::UART1_BAUDRATE, Value::U32(1))]), Err(BuildError::BufferTooSmall));
    let many = [(keys::TP_TP1_ENA, Value::Bool(true)); 65];
    let mut big = [0u8; 1024];
    assert_eq!(config::valset(&mut big, 3, &many), Err(BuildError::TooManyItems));
    assert!(config::valset(&mut big, 3, &many[..64]).is_ok());
}

#[test]
fn valget_and_mon_ver_polls() {
    let mut out = [0u8; 64];
    let n = config::poll_mon_ver(&mut out).unwrap();
    assert_eq!(&out[..n], &[0xB5, 0x62, 0x0A, 0x04, 0x00, 0x00, 0x0E, 0x34]);

    let n = config::valget(&mut out, 0, 0, &[keys::UART1_BAUDRATE, keys::NAVSPG_DYNMODEL]).unwrap();
    let p = check_frame(&out[..n], 0x06, 0x8B);
    assert_eq!(p, &[0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x52, 0x40, 0x21, 0x00, 0x11, 0x20]);
    assert_eq!(config::poll_mon_ver(&mut [0u8; 4]), Err(BuildError::BufferTooSmall));
}

#[test]
fn key_ids_match_the_m10_interface_description() {
    // UBX-21035062 R03, section 4.9 (checked against the PDF text).
    assert_eq!(keys::MSGOUT_UBX_NAV_PVT_I2C, 0x20910006);
    assert_eq!(keys::MSGOUT_UBX_NAV_PVT_UART1, 0x20910007);
    assert_eq!(keys::UART1_BAUDRATE, 0x40520001);
    assert_eq!(keys::NAVSPG_DYNMODEL, 0x20110021);
    assert_eq!(keys::UART1OUTPROT_NMEA, 0x10740002);
    assert_eq!(keys::I2COUTPROT_NMEA, 0x10720002);
    assert_eq!(keys::TP_PULSE_DEF, 0x20050023);
    assert_eq!(keys::TP_PULSE_LENGTH_DEF, 0x20050030);
    assert_eq!(keys::TP_PERIOD_TP1, 0x40050002);
    assert_eq!(keys::TP_PERIOD_LOCK_TP1, 0x40050003);
    assert_eq!(keys::TP_LEN_TP1, 0x40050004);
    assert_eq!(keys::TP_LEN_LOCK_TP1, 0x40050005);
    assert_eq!(keys::TP_TP1_ENA, 0x10050007);
    assert_eq!(keys::TP_SYNC_GNSS_TP1, 0x10050008);
    assert_eq!(keys::TP_USE_LOCKED_TP1, 0x10050009);
    assert_eq!(keys::TP_ALIGN_TO_TOW_TP1, 0x1005000a);
    assert_eq!(keys::TP_POL_TP1, 0x1005000b);
    assert_eq!(keys::TP_TIMEGRID_TP1, 0x2005000c);
    // Integrity messages (4.9.11 CFG-MSGOUT) and the interference monitor (4.9.9 CFG-ITFM).
    assert_eq!(keys::MSGOUT_UBX_NAV_SAT_I2C, 0x20910015);
    assert_eq!(keys::MSGOUT_UBX_NAV_SAT_UART1, 0x20910016);
    assert_eq!(keys::MSGOUT_UBX_NAV_STATUS_I2C, 0x2091001a);
    assert_eq!(keys::MSGOUT_UBX_NAV_STATUS_UART1, 0x2091001b);
    assert_eq!(keys::MSGOUT_UBX_NAV_CLOCK_I2C, 0x20910065);
    assert_eq!(keys::MSGOUT_UBX_NAV_CLOCK_UART1, 0x20910066);
    assert_eq!(keys::MSGOUT_UBX_MON_RF_I2C, 0x20910359);
    assert_eq!(keys::MSGOUT_UBX_MON_RF_UART1, 0x2091035a);
    assert_eq!(keys::ITFM_ENABLE, 0x1041000d);
}

#[test]
fn configure_sequence_contents() {
    let steps: Vec<_> = config::configure_sequence().with_integrity(true).collect();
    let names: Vec<_> = steps.iter().map(|s| s.name).collect();
    assert_eq!(names, ["nav-pvt-out", "protocols", "dynmodel", "timepulse", "integrity-out", "itfm", "baud"]);
    let optional: Vec<_> = steps.iter().map(|s| s.optional).collect();
    assert_eq!(optional, [false, false, false, false, true, true, false]);
    let mut all = Vec::new();
    for s in &steps {
        assert!(!s.bytes().is_empty());
        let p = check_frame(s.bytes(), 0x06, 0x8A);
        let (ver, layers, res, items) = decode_valset(p);
        assert_eq!((ver, layers, res), (0, 0b11, [0, 0]), "RAM + BBR");
        all.extend(items);
    }
    let get = |k| all.iter().find(|(key, _)| *key == k).map(|x| x.1);
    assert_eq!(get(keys::MSGOUT_UBX_NAV_PVT_UART1), Some(1));
    assert_eq!(get(keys::MSGOUT_UBX_NAV_PVT_I2C), Some(1));
    assert_eq!(get(keys::UART1OUTPROT_NMEA), Some(0));
    assert_eq!(get(keys::I2COUTPROT_NMEA), Some(0));
    assert_eq!(get(keys::UART1OUTPROT_UBX), Some(1));
    assert_eq!(get(keys::I2COUTPROT_UBX), Some(1));
    assert_eq!(get(keys::NAVSPG_DYNMODEL), Some(0), "portable");
    assert_eq!(get(keys::UART1_BAUDRATE), Some(115_200));
    assert_eq!(get(keys::TP_TP1_ENA), Some(1));
    assert_eq!(get(keys::TP_PERIOD_TP1), Some(1_000_000));
    assert_eq!(get(keys::TP_PERIOD_LOCK_TP1), Some(1_000_000));
    assert_eq!(get(keys::TP_LEN_TP1), Some(0), "no pulse before lock");
    assert_eq!(get(keys::TP_LEN_LOCK_TP1), Some(100_000));
    assert_eq!(get(keys::TP_USE_LOCKED_TP1), Some(1));
    assert_eq!(get(keys::TP_SYNC_GNSS_TP1), Some(1));
    for k in [
        keys::MSGOUT_UBX_NAV_SAT_UART1,
        keys::MSGOUT_UBX_NAV_SAT_I2C,
        keys::MSGOUT_UBX_NAV_STATUS_UART1,
        keys::MSGOUT_UBX_NAV_STATUS_I2C,
        keys::MSGOUT_UBX_NAV_CLOCK_UART1,
        keys::MSGOUT_UBX_NAV_CLOCK_I2C,
        keys::MSGOUT_UBX_MON_RF_UART1,
        keys::MSGOUT_UBX_MON_RF_I2C,
    ] {
        assert_eq!(get(k), Some(1), "{k:08x}");
    }
    assert_eq!(get(keys::ITFM_ENABLE), Some(1));
    // Output exists before NMEA goes away; baud change is last.
    assert_eq!(steps[0].name, "nav-pvt-out");
    assert_eq!(steps[6].baud_after, Some(115_200));
    assert!(steps[..6].iter().all(|s| s.baud_after.is_none()));
}

#[test]
fn configure_sequence_options() {
    let no_baud = config::ConfigSequence::with_options(ConfigOptions { baud: None, ..Default::default() });
    assert_eq!(no_baud.map(|s| s.name).collect::<Vec<_>>(), ["nav-pvt-out", "protocols", "dynmodel", "timepulse"]);
    let plain: Vec<_> = config::configure_sequence().map(|s| s.name).collect();
    assert_eq!(plain, ["nav-pvt-out", "protocols", "dynmodel", "timepulse", "baud"], "integrity off by default");
    let integ = config::ConfigSequence::with_options(ConfigOptions { baud: None, ..Default::default() }).with_integrity(true);
    assert_eq!(integ.count(), 6);
    let o = ConfigOptions { baud: Some(57_600), layers: config::LAYER_RAM, pps_len_us: 10_000 };
    let last = config::ConfigSequence::with_options(o).last().unwrap();
    assert_eq!(last.baud_after, Some(57_600));
    let (_, layers, _, items) = decode_valset(check_frame(last.bytes(), 6, 0x8A));
    assert_eq!(layers, 1);
    assert_eq!(items, [(keys::UART1_BAUDRATE, 57_600)]);
}

#[test]
fn ack_confirms_config_step() {
    let step = config::configure_sequence().next().unwrap();
    assert_eq!(step.confirmed_by(&Event::Ack { class: 6, id: 0x8A }), Some(true));
    assert_eq!(step.confirmed_by(&Event::Nak { class: 6, id: 0x8A }), Some(false));
    assert_eq!(step.confirmed_by(&Event::Ack { class: 6, id: 0x8B }), None);
    assert_eq!(step.confirmed_by(&Event::Fix(one_fix(&Pvt::default().frame()))), None);
}

#[test]
fn receiver_replies_flow_through_parser() {
    // The ACK a receiver would send for each step is decoded and confirms it.
    let mut p = Parser::new();
    for step in config::configure_sequence() {
        let reply = ubx(0x05, 0x01, &[0x06, 0x8A]);
        let ev: Vec<_> = p.feed(&reply).collect();
        assert_eq!(step.confirmed_by(&ev[0]), Some(true));
    }
}

#[test]
fn build_frame_matches_reference() {
    let mut out = [0u8; 32];
    let n = build_frame(&mut out, 0x05, 0x01, &[0x06, 0x8A]).unwrap();
    assert_eq!(&out[..n], &ubx(0x05, 0x01, &[0x06, 0x8A])[..]);
    assert_eq!(build_frame(&mut out[..9], 0x05, 0x01, &[0x06, 0x8A]), None);
}
