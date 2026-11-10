//! Fixtures are built exactly per the format strings in the ADSBee's
//! `csbee_utils.hh` (tag adsbee_1090-0.9.0). The ADSBee repository has no
//! captured CSBee output to copy, so the first three lines use the field
//! values of its own host test (`pico/host_test/test_reporting_csbee.cc`),
//! formatted by hand. Every CRC below was computed independently of this
//! crate, with Python's `binascii.crc_hqx(line, 0xFFFF)` (CRC-16/
//! CCITT-FALSE) and the bytes swapped.

extern crate std;

use std::string::String;
use std::vec::Vec;

use crate::*;

/// The ADSBee host test's Mode S aircraft (every flag set). The latitude is
/// out of range on purpose there; it is a formatting test.
const MODE_S: &str = "#A:12345E,FFFFFFFF,ABCDEFG,1234,9,-120.65432,-80.12346,1000,997,301,123,-200,-205,BDBB,FFFA1453,3,-75,2,1,3,8AA8\r\n";
/// The same aircraft as UAT (with the emergency status field).
const UAT: &str = "#U:12345E,FFFFFFFF,ABCDEFG,1234,9,-120.65432,-80.12346,1000,997,301,123,-200,-205,6,BDBB,FFFA1453,3,-75,2,1,935C\r\n";
/// The host test's statistics line.
const STATS: &str = "#S:2,10,20,18,10,8,5,4,10,123456,654321,9F57\r\n";
/// A medevac helicopter squawking 7700 over City Hall, position updated.
const HELI: &str = "#A:A1B2C3,200007F,N911PH,7700,7,39.95600,-75.16000,1200,1325,270,95,-500,-512,0,0,2,-62,9,3,4,DA00\r\n";
/// An airliner leaving PHL; its CRC is only two hex digits (`%X` does not pad).
const SHORT_CRC: &str = "#A:C0FFEE,3F,UAL123,1200,3,39.87190,-75.24110,1029,1179,85,250,1024,1088,0,0,2,-80,3,0,7,F9\r\n";

const FIXTURES: [&str; 5] = [MODE_S, UAT, STATS, HELI, SHORT_CRC];

fn aircraft(line: &str) -> Aircraft {
    match parse(line.as_bytes()) {
        Ok(Report::Aircraft(a)) => a,
        other => panic!("{line:?}: {other:?}"),
    }
}

/// `body` (ending in a comma) with its correct CRC and `\r\n`.
fn sealed(body: &str) -> String {
    std::format!("{body}{:X}\r\n", crc16(body.as_bytes()))
}

// --------------------------------------------------------------- the CRC --

#[test]
fn crc_is_ccitt_false_byte_swapped() {
    // CRC-16/CCITT-FALSE's published check value is 0x29B1.
    assert_eq!(crc16(b"123456789"), 0xB129);
    assert_eq!(crc16(b""), 0xFFFF);
}

// --------------------------------------------------------- good lines ----

#[test]
fn mode_s_line_matches_adsbee_host_test() {
    let a = aircraft(MODE_S);
    assert_eq!(a.source, Source::ModeS);
    assert_eq!(a.icao, 0x12345E);
    assert_eq!(a.icao24(), 0x12345E);
    assert_eq!(a.flags, u32::MAX);
    assert_eq!(a.callsign.as_str(), "ABCDEFG");
    assert_eq!(a.squawk, Some(0o1234));
    assert_eq!(a.category, 9);
    assert_eq!(a.kind(), Kind::Glider);
    assert_eq!(a.lat_deg, -120.65432);
    assert_eq!(a.lon_deg, -80.12346);
    assert_eq!(a.baro_altitude_ft(), Some(1000));
    assert_eq!(a.gnss_altitude_ft(), Some(997));
    assert_eq!(a.direction_deg(), Some(301.0));
    assert_eq!(a.speed_kt(), Some(123));
    assert_eq!(a.vertical_rate_fpm(), Some(-200));
    assert_eq!(a.gnss_vrate_fpm, -205);
    assert_eq!(a.nicnac, 0xBDBB);
    assert_eq!(a.acdims, 0xFFFA_1453);
    assert_eq!(a.version, 3);
    assert_eq!(a.signal_dbm, -75);
    assert_eq!(a.signal_quality, 2);
    assert_eq!(a.frames_per_s, 1);
    assert_eq!(a.es_frames_per_s, 3);
    // Flag says valid, but -120 degrees latitude is not a place.
    assert_eq!(a.position(), None);
    assert!(a.military() && a.airborne() && a.heard() && a.position_updated());
}

#[test]
fn uat_line_has_emergency_field_and_no_esfps() {
    let a = aircraft(UAT);
    assert_eq!(a.source, Source::Uat);
    assert_eq!(a.uat_emergency, 6);
    assert_eq!(a.nicnac, 0xBDBB);
    assert_eq!(a.acdims, 0xFFFA_1453);
    assert_eq!(a.frames_per_s, 1);
    assert_eq!(a.es_frames_per_s, 0);
    // Bit 8 means something else on UAT (CDTI), not military.
    assert!(!a.military());
}

#[test]
fn uat_address_qualifier_is_in_the_icao_field() {
    // As the ADSBee's own test: qualifier 2 (TIS-B) prints as 212345E.
    let line = sealed("#U:212345E,0,?,?,0,0.00000,0.00000,0,0,0,0,0,0,0,0,0,0,-80,3,2,");
    let a = aircraft(&line);
    assert_eq!(a.icao24(), 0x12345E);
    assert_eq!(a.address_qualifier(), 2);
    assert!(a.callsign.is_empty());
    assert_eq!(a.squawk, None);
    assert_eq!(a.position(), None);
    assert_eq!(a.baro_altitude_ft(), None);
    assert_eq!(a.direction_deg(), None);
}

#[test]
fn stats_line_matches_adsbee_host_test() {
    let Ok(Report::Stats(s)) = parse(STATS.as_bytes()) else {
        panic!()
    };
    assert_eq!(
        s,
        Stats {
            version: 2,
            demods_per_s: 10,
            raw_squitters_per_s: 20,
            squitters_per_s: 18,
            raw_extended_squitters_per_s: 10,
            extended_squitters_per_s: 8,
            raw_uat_per_s: 5,
            uat_per_s: 4,
            aircraft: 10,
            tscal: 123_456,
            uptime_s: 654_321,
        }
    );
    assert_eq!(s.valid_per_s(), 30);
}

#[test]
fn realistic_helicopter() {
    let a = aircraft(HELI);
    assert_eq!(a.position(), Some((39.956, -75.16)));
    assert_eq!(a.kind(), Kind::Rotorcraft);
    assert_eq!(a.emergency(), Some("emergency"));
    assert_eq!(a.callsign.as_str(), "N911PH");
    assert!(a.position_updated() && !a.military());
    assert_eq!(a.vertical_rate_fpm(), Some(-500));
}

#[test]
fn short_crc_is_not_padded() {
    let a = aircraft(SHORT_CRC);
    assert_eq!(a.callsign.as_str(), "UAL123");
    assert_eq!(a.kind(), Kind::Medium);
    // Flags 3F: no vertical rate valid, position not updated this second.
    assert_eq!(a.vertical_rate_fpm(), None);
    assert!(!a.position_updated());
    // Padded or lower case is still the same number.
    let padded = SHORT_CRC.replace(",F9\r", ",00f9\r");
    assert_eq!(parse(padded.as_bytes()), parse(SHORT_CRC.as_bytes()));
}

#[test]
fn line_endings_are_optional() {
    for f in FIXTURES {
        let want = parse(f.as_bytes()).unwrap();
        let bare = f.trim_end();
        assert_eq!(parse(bare.as_bytes()).unwrap(), want);
        assert_eq!(parse(std::format!("{bare}\n").as_bytes()).unwrap(), want);
    }
}

#[test]
fn encoder_reproduces_every_fixture() {
    let mut buf = [0u8; MAX_LINE];
    for f in FIXTURES {
        let n = match parse(f.as_bytes()).unwrap() {
            Report::Aircraft(a) => encode::aircraft(&a, &mut buf),
            Report::Stats(s) => encode::stats(&s, &mut buf),
        }
        .unwrap();
        assert_eq!(core::str::from_utf8(&buf[..n]).unwrap(), f);
    }
}

#[test]
fn directions_wrap() {
    let line = sealed("#A:1,10,?,?,0,0.00000,0.00000,0,0,360,0,0,0,0,0,0,0,0,0,0,");
    assert_eq!(aircraft(&line).direction_deg(), Some(0.0));
    let line = sealed("#A:1,10,?,?,0,0.00000,0.00000,0,0,-90,0,0,0,0,0,0,0,0,0,0,");
    assert_eq!(aircraft(&line).direction_deg(), Some(270.0));
}

#[test]
fn quiet_aircraft_is_not_heard() {
    // Still in the ADSBee's table, nothing received in the last second.
    let line = sealed("#A:ABC123,3F,?,?,1,40.00000,-75.00000,3000,0,10,120,0,0,0,0,2,-90,0,0,0,");
    let a = aircraft(&line);
    assert!(!a.heard());
    assert_eq!(a.position(), Some((40.0, -75.0)));
}

// ---------------------------------------------------------- bad lines ----

#[test]
fn refusals() {
    use Error::*;
    let cases: &[(&str, Error)] = &[
        ("", NotCsbee),
        ("AT+PROTOCOL_OUT=CONSOLE,CSBEE\r\nOK", NotCsbee),
        ("#B:1,2,3,", NotCsbee),
        ("#a:12345E,", NotCsbee),
        ("INFO: CommsManager::UpdateReporting overran", NotCsbee),
        ("#A:", FieldCount),
        ("#A:12345E", FieldCount),
        // CRC field missing, not hex, or too long.
        ("#S:2,10,20,18,10,8,5,4,10,123456,654321,\r\n", NoCrc),
        ("#S:2,10,20,18,10,8,5,4,10,123456,654321,9G57", NoCrc),
        ("#S:2,10,20,18,10,8,5,4,10,123456,654321,09F57", NoCrc),
        // One digit of the checksum changed.
        ("#S:2,10,20,18,10,8,5,4,10,123456,654321,9F56", BadCrc),
        // One digit of the data changed.
        ("#S:2,10,20,18,10,8,5,4,11,123456,654321,9F57", BadCrc),
        ("#S:2,10,20,18,10,8,5,4,10,123456,654321,9F57\0", NotText),
    ];
    for (line, want) in cases {
        assert_eq!(parse(line.as_bytes()), Err(*want), "{line:?}");
    }
    let long = std::format!("#S:{}", "1,".repeat(120));
    assert_eq!(parse(long.as_bytes()), Err(TooLong));
    let mut utf8 = String::from("#A:12345E,0,\u{e9}");
    utf8.push(',');
    assert_eq!(parse(utf8.as_bytes()), Err(NotText));
}

/// Lines with a correct CRC but wrong content: the CRC protects the wire,
/// not the meaning, so the fields are still checked.
#[test]
fn well_sealed_but_malformed() {
    use Error::*;
    let a = "#A:A1B2C3,200007F,N911PH,7700,7,39.95600,-75.16000,1200,1325,270,95,-500,-512,0,0,2,-62,9,3,4,";
    assert!(parse(sealed(a).as_bytes()).is_ok());
    let edits: &[(&str, &str, Error)] = &[
        // One field too many / too few.
        ("4,", "4,5,", FieldCount),
        (",3,4,", ",3,", FieldCount),
        ("A1B2C3", "", BadField(0)),
        ("A1B2C3", "XYZ", BadField(0)),
        ("A1B2C3", "1A1B2C3F", BadField(0)),
        ("200007F", "123456789", BadField(1)),
        ("N911PH", "TOOLONGCS", BadField(2)),
        ("7700", "7800", BadField(3)),
        ("7700", "770", BadField(3)),
        (",7,39", ",128,39", BadField(4)),
        ("39.95600", "nan", BadField(5)),
        ("39.95600", "+39.95600", BadField(5)),
        ("-75.16000", "inf", BadField(6)),
        ("1200", "1e3", BadField(7)),
        ("1325", "99999999999", BadField(8)),
        (",2,-62,", ",256,-62,", BadField(15)),
        ("-62", "-99999", BadField(16)),
    ];
    for (from, to, want) in edits {
        let body = a.replacen(from, to, 1);
        assert_eq!(parse(sealed(&body).as_bytes()), Err(*want), "{body}");
    }
}

#[test]
fn every_truncation_is_refused() {
    for f in FIXTURES {
        let line = f.trim_end().as_bytes();
        for n in 0..line.len() {
            assert!(parse(&line[..n]).is_err(), "{:?}", &f[..n]);
        }
    }
}

#[test]
fn every_single_byte_change_is_refused() {
    // CRC-16 catches every error burst up to 16 bits, so any one changed
    // byte before the checksum is caught whatever it becomes.
    for f in FIXTURES {
        let line = f.trim_end().as_bytes();
        let body_end = line.iter().rposition(|&b| b == b',').unwrap();
        for i in 0..=body_end {
            for v in 0..=255u8 {
                if v == line[i] {
                    continue;
                }
                let mut m = line.to_vec();
                m[i] = v;
                assert!(parse(&m).is_err(), "{:?}", String::from_utf8_lossy(&m));
            }
        }
    }
}

/// Tiny deterministic PRNG (xorshift64*), so the fuzz tests need no crate.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

#[test]
fn fuzz_random_bytes_never_panic_or_pass() {
    let mut rng = Rng(0x00C0_FFEE_D00D);
    let mut ok = 0;
    for _ in 0..50_000 {
        let len = rng.below(260) as usize;
        let mut v: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        // Half the time give it a real prefix so it reaches the deeper checks.
        if rng.below(2) == 0 && v.len() >= 3 {
            v[..3].copy_from_slice([b"#A:", b"#U:", b"#S:"][rng.below(3) as usize]);
        }
        ok += usize::from(parse(&v).is_ok());
    }
    assert_eq!(ok, 0);
}

#[test]
fn fuzz_random_fields_with_good_crc_never_panic() {
    // Plausible-looking fields from a small alphabet, correctly sealed, so
    // the field parsers (not the CRC) take the hits.
    const ALPHABET: &[u8] = b"0123456789ABCDEF-.?,+e ";
    let mut rng = Rng(42);
    let (mut ok, mut field_errors) = (0, 0);
    for _ in 0..50_000 {
        let tag = ["#A:", "#U:", "#S:"][rng.below(3) as usize];
        let mut body = String::from(tag);
        for _ in 0..rng.below(170) {
            body.push(ALPHABET[rng.below(ALPHABET.len() as u64) as usize] as char);
        }
        body.push(',');
        match parse(sealed(&body).as_bytes()) {
            Ok(_) => ok += 1,
            Err(Error::BadField(_) | Error::FieldCount) => field_errors += 1,
            Err(Error::TooLong) => {}
            Err(e) => panic!("{e:?} for sealed {body:?}"),
        }
    }
    assert!(field_errors > 40_000, "{field_errors}");
    std::println!("random sealed lines: {ok} parsed, {field_errors} refused");
}

#[test]
fn fuzz_mutated_fixtures_never_panic() {
    let mut rng = Rng(7);
    for _ in 0..20_000 {
        let mut v = FIXTURES[rng.below(5) as usize].as_bytes().to_vec();
        for _ in 0..=rng.below(4) {
            match rng.below(3) {
                0 if !v.is_empty() => {
                    let i = rng.below(v.len() as u64) as usize;
                    v.remove(i);
                }
                1 => {
                    let i = rng.below(v.len() as u64 + 1) as usize;
                    v.insert(i, rng.next() as u8);
                }
                _ if !v.is_empty() => {
                    let i = rng.below(v.len() as u64) as usize;
                    v[i] = rng.next() as u8;
                }
                _ => {}
            }
        }
        let _ = parse(&v);
        let mut s = LineSplitter::new();
        s.feed(&v, |_| {});
    }
}

// -------------------------------------------------------- the splitter ---

fn split_all(chunks: &[&[u8]]) -> Vec<Result<Report, Error>> {
    let mut s = LineSplitter::new();
    let mut out = Vec::new();
    for c in chunks {
        s.feed(c, |r| out.push(r));
    }
    out
}

#[test]
fn splitter_handles_any_chunking() {
    let stream: String = std::format!("{HELI}{SHORT_CRC}{UAT}{STATS}");
    let want: Vec<_> = [HELI, SHORT_CRC, UAT, STATS]
        .iter()
        .map(|l| parse(l.as_bytes()))
        .collect();
    let bytes = stream.as_bytes();
    for size in 1..=bytes.len() {
        let chunks: Vec<&[u8]> = bytes.chunks(size).collect();
        assert_eq!(split_all(&chunks), want, "chunk size {size}");
    }
}

#[test]
fn splitter_resyncs_on_hash_after_a_lost_newline() {
    // The first line lost its last bytes (and its \n); the second is whole.
    let cut = &HELI[..HELI.len() - 6];
    let stream = std::format!("{cut}{STATS}");
    let got = split_all(&[stream.as_bytes()]);
    assert_eq!(got.len(), 2);
    assert!(got[0].is_err());
    assert_eq!(got[1], parse(STATS.as_bytes()));
}

#[test]
fn splitter_reports_overflow_and_recovers() {
    let junk = std::vec![b'7'; 1000];
    let got = split_all(&[&junk, b"\r\n", STATS.as_bytes()]);
    assert_eq!(got, [Err(Error::TooLong), parse(STATS.as_bytes())]);
}

#[test]
fn splitter_skips_blank_lines_and_flags_console_text() {
    let got = split_all(&[b"\r\n\r\n  \r\nINFO: hello\r\n", HELI.as_bytes()]);
    assert_eq!(got, [Err(Error::NotCsbee), parse(HELI.as_bytes())]);
}
