//! Host tests for the Quectel LC29H(AA) path: NMEA 4.10 sentences, the sky view, PAIR/PQTM
//! replies, the boot sequence and module detection.
//!
//! Sentence samples are data strings in the field formats of the LC29H protocol
//! specification (V1.3); the checksums of the ones copied from its examples were re-checked
//! independently, and every other sample is sealed by the test's own checksum helper.

use crate::quectel::{self, AckResult, Expect, Lc29hOptions, Lc29hSequence, Reply};
use crate::sky::System;
use crate::*;
use std::string::String;
use std::vec::Vec;

/// Independent checksum (not the crate's).
fn seal(body: &str) -> String {
    let x = body.bytes().fold(0u8, |a, b| a ^ b);
    std::format!("${body}*{x:02X}\r\n")
}

fn feed_all(p: &mut Parser, s: &str) -> Vec<Event> {
    p.feed(s.as_bytes()).collect()
}

fn fixes(ev: &[Event]) -> Vec<Fix> {
    ev.iter().filter_map(|e| if let Event::Fix(f) = e { Some(*f) } else { None }).collect()
}

fn replies(ev: &[Event]) -> Vec<Reply> {
    ev.iter().filter_map(|e| if let Event::Quectel(r) = e { Some(*r) } else { None }).collect()
}

/// One second of LC29H(AA) output at its defaults (GGA, GLL, GSA x4, GSV, RMC, VTG), dual
/// band, NMEA 4.10. GSV/GSA/GGA/RMC lines are the protocol specification's examples.
const EPOCH: &str = concat!(
    "$GNGGA,093316.000,3149.332558,N,11706.912570,E,1,28,0.67,54.481,M,-0.337,M,,*5D\r\n",
    "$GNGLL,3149.332558,N,11706.912570,E,093316.000,A,A*45\r\n",
    "$GNGSA,A,3,08,195,194,199,27,07,21,16,09,04,,,1.03,0.67,0.78,1*3E\r\n",
    "$GNGSA,A,3,85,76,66,,,,,,,,,,1.03,0.67,0.78,2*02\r\n",
    "$GNGSA,A,3,,,,,,,,,,,,,1.03,0.67,0.78,3*0F\r\n",
    "$GNGSA,A,3,07,40,36,06,16,35,26,,,,,,1.03,0.67,0.78,4*0D\r\n",
    "$GPGSV,3,1,11,08,76,353,46,195,68,113,44,194,64,080,42,199,51,162,37,1*5C\r\n",
    "$GPGSV,3,2,11,27,45,037,43,07,44,315,43,21,43,156,44,16,30,071,39,1*6B\r\n",
    "$GPGSV,3,3,11,09,28,244,40,04,20,206,37,30,,,38,1*6F\r\n",
    "$GPGSV,2,1,08,08,76,353,35,195,68,113,34,194,64,080,33,199,51,162,34,8*5A\r\n",
    "$GPGSV,2,2,08,27,45,037,33,09,28,244,28,04,20,206,26,30,,,27,8*5E\r\n",
    "$GLGSV,1,1,03,85,59,042,41,76,35,333,40,66,07,279,32,1*43\r\n",
    "$GAGSV,1,1,00,7*73\r\n",
    "$GAGSV,1,1,00,1*75\r\n",
    "$GBGSV,4,1,13,07,64,319,41,40,61,345,43,36,47,113,43,06,45,201,40,1*7E\r\n",
    "$GBGSV,4,2,13,16,41,195,41,35,40,213,45,26,17,313,39,56,,,19,1*49\r\n",
    "$GBGSV,4,3,13,57,,,37,01,,,39,02,,,36,60,,,40,1*7B\r\n",
    "$GBGSV,4,4,13,03,,,40,1*73\r\n",
    "$GBGSV,1,1,04,40,61,345,32,36,47,113,32,35,40,213,29,26,17,313,23,5*79\r\n",
    "$GNRMC,093316.000,A,3149.332558,N,11706.912570,E,0.00,237.67,140122,,,A,V*0B\r\n",
    "$GNVTG,237.67,T,,M,0.00,N,0.00,K,A*24\r\n",
);

#[test]
fn checksums_of_samples_are_right() {
    for line in EPOCH.lines() {
        let (body, cs) = line[1..].split_once('*').unwrap();
        assert_eq!(seal(body).trim_end(), line, "{line}");
        assert_eq!(cs.len(), 2);
    }
}

#[test]
fn lc29h_epoch_gives_one_3d_fix() {
    let mut p = Parser::new();
    let ev = feed_all(&mut p, EPOCH);
    let f = fixes(&ev);
    assert_eq!(f.len(), 1, "GGA + RMC of one epoch merge into one fix");
    let f = f[0];
    assert_eq!(f.source, Source::Nmea);
    assert_eq!(f.fix, FixType::ThreeD);
    assert!(f.fix_ok && f.is_usable());
    // 31 deg 49.332558 min = 31.8222093 deg; 117 deg 06.912570 min = 117.1152095 deg.
    assert!((f.lat - (31.0 + 49.332558 / 60.0)).abs() < 1e-9);
    assert!((f.lon - (117.0 + 6.912570 / 60.0)).abs() < 1e-9);
    assert_eq!(f.sats_used, 28);
    assert_eq!(f.alt_msl_m, Some(54.481));
    let hae = f.alt_ellipsoid_m.unwrap();
    assert!((hae - (54.481 - 0.337)).abs() < 1e-3, "{hae}");
    assert_eq!(f.hdop, Some(0.67));
    assert_eq!(f.pdop, Some(1.03), "from GSA");
    assert_eq!(f.heading_deg, Some(237.67));
    let u = f.utc.unwrap();
    assert_eq!((u.year, u.month, u.day, u.hour, u.minute, u.second, u.millis), (2022, 1, 14, 9, 33, 16, 0));
    assert_eq!(p.stats().checksum_errors, 0);
    assert_eq!(p.stats().frames_ok as usize, EPOCH.lines().count());
}

#[test]
fn lc29h_sky_view_per_constellation_and_band() {
    let mut p = Parser::new();
    let _ = feed_all(&mut p, EPOCH);
    let sky = p.sky();
    assert!(sky.signal_ids);
    let gps = sky.constellation(System::Gps);
    let qzss = sky.constellation(System::Qzss);
    // 11 GP satellites on L1, of which 195, 194, 199 are QZSS.
    assert_eq!(gps.in_view, 8);
    assert_eq!(qzss.in_view, 3);
    // L1: 8 GPS all with C/N0 (30 has 38); L5 (signal 8): 5 GPS with C/N0, 3 QZSS.
    assert_eq!(gps.l1, 8);
    assert_eq!(gps.l5, 5);
    assert_eq!(qzss.l1, 3);
    assert_eq!(qzss.l5, 3);
    assert_eq!(gps.best_cn0, 46);
    // GSA system 1 lists 10: QZSS 195, 194, 199 by number, 7 GPS.
    assert_eq!(gps.used, 7);
    assert_eq!(qzss.used, 3);
    let glo = sky.constellation(System::Glonass);
    assert_eq!((glo.in_view, glo.l1, glo.l5, glo.used), (3, 3, 0, 3));
    let gal = sky.constellation(System::Galileo);
    assert_eq!((gal.in_view, gal.used), (0, 0));
    let bds = sky.constellation(System::Beidou);
    // B1I: 13 in view, all with C/N0; B2a: 4.
    assert_eq!((bds.in_view, bds.l1, bds.l5, bds.used), (13, 13, 4, 7));
    assert!(sky.dual_band());
    assert_eq!(sky.total_in_view(), 8 + 3 + 3 + 13);
    assert_eq!(sky.total_used(), 7 + 3 + 3 + 7);
}

#[test]
fn sky_view_forgets_a_band_that_stops() {
    let mut p = Parser::new();
    let _ = feed_all(&mut p, EPOCH);
    assert!(p.sky().dual_band());
    // Four more epochs with L1 GSV only (and GGA + RMC so epochs count).
    for s in 17..21 {
        let t = std::format!("0933{s}.000");
        let mut e = String::new();
        e += &seal(&std::format!("GNGGA,{t},3149.332558,N,11706.912570,E,1,28,0.67,54.481,M,-0.337,M,,"));
        e += &seal("GPGSV,1,1,01,08,76,353,46,1");
        e += &seal(&std::format!("GNRMC,{t},A,3149.332558,N,11706.912570,E,0.00,237.67,140122,,,A,V"));
        let _ = feed_all(&mut p, &e);
    }
    let gps = p.sky().constellation(System::Gps);
    assert_eq!((gps.in_view, gps.l1, gps.l5), (1, 1, 0));
    assert!(!p.sky().dual_band(), "L5 groups older than 3 epochs are dropped");
    assert_eq!(p.sky().constellation(System::Beidou).in_view, 0);
}

#[test]
fn gsv_with_a_lost_middle_message_keeps_the_previous_count() {
    let mut p = Parser::new();
    let _ = feed_all(&mut p, EPOCH);
    // Next epoch's GPS L1 group arrives without its 2nd message.
    let mut e = String::new();
    e += &seal("GPGSV,3,1,12,08,76,353,46,195,68,113,44,194,64,080,42,199,51,162,37,1");
    e += &seal("GPGSV,3,3,12,09,28,244,40,04,20,206,37,30,,,38,05,10,100,20,1");
    let _ = feed_all(&mut p, &e);
    assert_eq!(p.sky().constellation(System::Gps).l1, 8, "incomplete group not committed");
}

#[test]
fn gsa_2d_mode_downgrades_the_fix() {
    let mut p = Parser::new();
    let mut e = String::new();
    e += &seal("GNGGA,120000.000,4000.000000,N,07500.000000,W,1,04,2.10,12.000,M,-34.000,M,,");
    e += &seal("GNGSA,A,2,05,13,15,18,,,,,,,,,2.40,2.10,1.00,1");
    e += &seal("GNRMC,120000.000,A,4000.000000,N,07500.000000,W,0.00,0.00,151126,,,A,V");
    let f = fixes(&feed_all(&mut p, &e));
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].fix, FixType::TwoD);
    assert_eq!(f[0].alt_msl_m, None);
    assert!(!f[0].is_usable(), "NMEA 2D has no accuracy figure");
    assert!(f[0].utc.is_some(), "time is still good with a 2D fix");
    assert!(f[0].lon < 0.0);
}

#[test]
fn every_talker_gives_a_fix() {
    for talker in ["GN", "GP", "GB", "GA", "GL", "GQ", "BD"] {
        let mut p = Parser::new();
        let mut e = String::new();
        e += &seal(&std::format!("{talker}GGA,235959.500,3958.123456,N,07509.654321,W,2,09,0.90,30.100,M,-33.900,M,,"));
        e += &seal(&std::format!("{talker}RMC,235959.500,A,3958.123456,N,07509.654321,W,1.50,90.00,311226,,,D,V"));
        let f = fixes(&feed_all(&mut p, &e));
        assert_eq!(f.len(), 1, "{talker}");
        assert!(f[0].is_usable(), "{talker}");
        let u = f[0].utc.unwrap();
        assert_eq!((u.year, u.month, u.day, u.second, u.millis), (2026, 12, 31, 59, 500));
    }
}

#[test]
fn rmc_navigation_status_v_is_not_a_void_fix() {
    // NMEA 4.10 adds a navigational status after the mode: V there means "no safety
    // status", not "void". The validity is field 2 and the mode (A).
    let mut p = Parser::new();
    let e = seal("GNRMC,093316.000,A,3149.332558,N,11706.912570,E,0.00,237.67,140122,,,A,V");
    let mut ev = feed_all(&mut p, &e);
    ev.extend(feed_all(&mut p, &seal("GNGGA,093317.000,,,,,0,00,99.99,,,,,,")));
    let f = fixes(&ev);
    assert!(f[0].fix_ok && f[0].utc.is_some());
}

#[test]
fn no_fix_yet_output() {
    // Before a fix: empty position, quality 0, RMC status V, mode N.
    let mut p = Parser::new();
    let mut e = String::new();
    e += &seal("GNGGA,000012.000,,,,,0,00,,,M,,M,,");
    e += &seal("GNGSA,A,1,,,,,,,,,,,,,,,,1");
    e += &seal("GNRMC,000012.000,V,,,,,,,060180,,,N,V");
    let f = fixes(&feed_all(&mut p, &e));
    assert_eq!(f.len(), 1);
    assert!(!f[0].fix_ok && f[0].position().is_none() && f[0].utc.is_none());
}

#[test]
fn pair_ack_and_pqtm_replies_decode() {
    let mut p = Parser::new();
    let mut s = String::new();
    s += "$PAIR001,062,0*3F\r\n"; // computed independently below
    s += &seal("PAIR001,050,4");
    s += &seal("PAIR001,864,1");
    s += &seal("PAIR001,999,3");
    s += "$PQTMVERNO,LC29HAANR01A04S,2022/11/04,16:39:48*34\r\n";
    s += "$PQTMCFGPPS,OK*21\r\n";
    s += "$PQTMCFGPPS,OK,1,1,100,1,1,0*20\r\n";
    s += &seal("PQTMCFGPPS,ERROR,1");
    s += &seal("PQTMVERNO,ERROR,2");
    let r = replies(&feed_all(&mut p, &s));
    assert_eq!(p.stats().checksum_errors, 0);
    assert_eq!(r.len(), 9);
    assert_eq!(r[0], Reply::PairAck { cmd: 62, result: AckResult::Ok });
    assert_eq!(r[1], Reply::PairAck { cmd: 50, result: AckResult::BadParameter });
    assert_eq!(r[2], Reply::PairAck { cmd: 864, result: AckResult::Processing });
    assert_eq!(r[3], Reply::PairAck { cmd: 999, result: AckResult::Unsupported });
    let Reply::Version(v) = r[4] else { panic!("{:?}", r[4]) };
    assert_eq!(v.version.as_str(), "LC29HAANR01A04S");
    assert_eq!(v.date.as_str(), "2022/11/04");
    assert_eq!(v.time.as_str(), "16:39:48");
    assert!(v.is_lc29h() && v.is_lc29h_aa());
    assert!(matches!(r[5], Reply::PqtmOk { name } if name.as_str() == "CFGPPS"));
    let Reply::Pps(pps) = r[6] else { panic!() };
    assert_eq!((pps.enabled, pps.width_ms, pps.mode, pps.active_high, pps.interval), (true, 100, 1, true, 0));
    assert!(matches!(r[7], Reply::PqtmError { name, code: 1 } if name.as_str() == "CFGPPS"));
    assert!(matches!(r[8], Reply::PqtmError { name, code: 2 } if name.as_str() == "VERNO"));
    assert_eq!(seal("PAIR001,062,0"), "$PAIR001,062,0*3F\r\n");
}

#[test]
fn unknown_proprietary_sentences_are_ignored() {
    let mut p = Parser::new();
    let mut s = String::new();
    s += &seal("PQTMEPE,2,1.000,1.000,1.000,1.414,1.732");
    s += &seal("PAIR051,1000");
    s += &seal("PSTMVER,whatever");
    let ev = feed_all(&mut p, &s);
    assert!(ev.is_empty(), "{ev:?}");
    assert_eq!(p.stats().frames_ok, 3);
}

#[test]
fn command_builder_matches_known_sentences() {
    let mut b = [0u8; 64];
    let cases = [
        ("PAIR062,0,3", "$PAIR062,0,3*3D\r\n"),
        ("PAIR050,1000", "$PAIR050,1000*12\r\n"),
        ("PQTMVERNO", "$PQTMVERNO*58\r\n"),
        ("PQTMCFGPPS,W,1,1,100,1,1,0", "$PQTMCFGPPS,W,1,1,100,1,1,0*73\r\n"),
        ("PAIR513", "$PAIR513*3D\r\n"),
    ];
    for (body, want) in cases {
        let n = quectel::command(&mut b, body).unwrap();
        assert_eq!(core::str::from_utf8(&b[..n]).unwrap(), want);
        assert_eq!(seal(body), want);
    }
    assert!(quectel::command(&mut b, "PAIR*1").is_none());
    assert!(quectel::command(&mut [0u8; 8], "PAIR062,0,1").is_none());
}

#[test]
fn lc29h_boot_sequence() {
    let steps: Vec<QuectelCommand> = Lc29hSequence::new(Lc29hOptions::default()).collect();
    let got: Vec<(&str, String)> =
        steps.iter().map(|c| (c.name, String::from_utf8(c.bytes().to_vec()).unwrap())).collect();
    let want = [
        ("gga-on", seal("PAIR062,0,1")),
        ("rmc-on", seal("PAIR062,4,1")),
        ("gsa-on", seal("PAIR062,2,1")),
        ("gsv-on", seal("PAIR062,3,1")),
        ("gll-off", seal("PAIR062,1,0")),
        ("vtg-off", seal("PAIR062,5,0")),
        ("pps", seal("PQTMCFGPPS,W,1,1,100,3,1,0")),
    ];
    assert_eq!(got.len(), want.len());
    for (g, w) in got.iter().zip(want.iter()) {
        assert_eq!(g.0, w.0);
        assert_eq!(g.1, w.1);
    }
    let no_gsv: Vec<QuectelCommand> =
        Lc29hSequence::new(Lc29hOptions { gsv: false, pps_width_ms: 5000 }).collect();
    assert_eq!(no_gsv[3].bytes(), seal("PAIR062,3,0").as_bytes());
    assert_eq!(no_gsv[6].bytes(), seal("PQTMCFGPPS,W,1,1,999,3,1,0").as_bytes(), "width clamped");
}

#[test]
fn lc29h_steps_are_confirmed_by_the_right_reply_only() {
    let mut p = Parser::new();
    let ev = |p: &mut Parser, body: &str| -> Event { feed_all(p, &seal(body))[0] };
    let rate = Lc29hSequence::new(Lc29hOptions::default()).next().unwrap();
    assert_eq!(rate.expect, Expect::PairAck(62));
    assert_eq!(rate.confirmed_by(&ev(&mut p, "PAIR001,062,0")), Some(true));
    assert_eq!(rate.confirmed_by(&ev(&mut p, "PAIR001,062,4")), Some(false));
    assert_eq!(rate.confirmed_by(&ev(&mut p, "PAIR001,062,1")), None, "processing: keep waiting");
    assert_eq!(rate.confirmed_by(&ev(&mut p, "PAIR001,050,0")), None, "another command's ACK");
    let pps = Lc29hSequence::new(Lc29hOptions::default()).nth(6).unwrap();
    assert_eq!(pps.confirmed_by(&ev(&mut p, "PQTMCFGPPS,OK")), Some(true));
    assert_eq!(pps.confirmed_by(&ev(&mut p, "PQTMCFGPPS,ERROR,1")), Some(false));
    assert_eq!(pps.confirmed_by(&ev(&mut p, "PQTMSAVEPAR,OK")), None);
    assert_eq!(pps.confirmed_by(&ev(&mut p, "PAIR001,062,0")), None);
    // A UBX ACK never confirms a Quectel step, and vice versa.
    let ubx = Event::Ack { class: 0x06, id: 0x8A };
    assert_eq!(rate.confirmed_by(&ubx), None);
    let m10 = Module::UbloxM10.sequence(ConfigOptions::default(), Lc29hOptions::default()).next().unwrap();
    assert_eq!(m10.confirmed_by(&ev(&mut p, "PAIR001,062,0")), None);
    assert_eq!(m10.confirmed_by(&ubx), Some(true));
}

#[test]
fn module_detection() {
    let mut b = [0u8; 64];
    let n = module::probe_bytes(&mut b);
    // MON-VER poll (8 bytes) then $PQTMVERNO*58\r\n.
    assert_eq!(&b[..8], &[0xB5, 0x62, 0x0A, 0x04, 0x00, 0x00, 0x0E, 0x34]);
    assert_eq!(&b[8..n], b"$PQTMVERNO*58\r\n");
    assert_eq!(module::probe_bytes(&mut [0u8; 16]), 0);

    let mut p = Parser::new();
    let lc = feed_all(&mut p, "$PQTMVERNO,LC29HAANR01A04S,2022/11/04,16:39:48*34\r\n");
    assert_eq!(Module::from_event(&lc[0]), Some(Module::QuectelLc29h));
    let ack = feed_all(&mut p, &seal("PAIR001,062,0"));
    assert_eq!(Module::from_event(&ack[0]), Some(Module::QuectelLc29h));
    assert_eq!(Module::from_event(&Event::Ack { class: 6, id: 0x8A }), Some(Module::UbloxM10));
    let f = fixes(&feed_all(&mut p, EPOCH));
    assert_eq!(Module::from_event(&Event::Fix(f[0])), None, "NMEA alone says nothing");
    assert_eq!(Module::QuectelLc29h.name(), "lc29h");
    assert_eq!(Module::Nmea.sequence(ConfigOptions::default(), Lc29hOptions::default()).count(), 0);
    let u: Vec<Step> = Module::UbloxM10.sequence(ConfigOptions::default(), Lc29hOptions::default()).collect();
    assert_eq!(u.len(), 5);
    assert_eq!(u[4].baud_after(), Some(115_200));
}

#[test]
fn interleaved_stream_any_chunking() {
    let mut s = String::new();
    s += EPOCH;
    s += &seal("PAIR001,062,0");
    s += "\u{0}\u{ff}garbage$GN";
    s += "$PQTMVERNO,LC29HAANR01A04S,2022/11/04,16:39:48*34\r\n";
    s += &EPOCH.replace("093316", "093317").lines().map(|l| {
        let body = &l[1..l.find('*').unwrap()];
        seal(body)
    }).collect::<String>();
    let bytes = s.as_bytes();
    let whole: Vec<Event> = Parser::new().feed(bytes).collect();
    for chunk in [1usize, 2, 3, 7, 64] {
        let mut p = Parser::new();
        let mut ev = Vec::new();
        for c in bytes.chunks(chunk) {
            ev.extend(p.feed(c));
        }
        assert_eq!(ev, whole, "chunk {chunk}");
    }
    assert_eq!(fixes(&whole).len(), 2);
    assert_eq!(replies(&whole).len(), 2);
}
