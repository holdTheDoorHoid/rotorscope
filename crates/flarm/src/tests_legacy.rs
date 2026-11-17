//! FLARM legacy tests.
//!
//! No recorded FLARM packets are available, and the real keys are not in
//! this repository, so:
//! - the algorithm is tested with **made-up keys** ([`FAKE_KEYS`]: not
//!   FLARM's, just numbers) and a test-only encoder ported from SoftRF's
//!   `legacy_v6_encode` / `legacy_encode`;
//! - the same round trips run with the **real keys** when the environment
//!   variable `FLARM_KEYS_FILE` names a key file (made by
//!   `cargo run -p flarm-keys`); without it those tests print a note and
//!   pass. The key values are never printed.
//!
//! A round trip through our own encoder and decoder checks that the two
//! agree and that every step is applied; it cannot prove that real FLARM
//! hardware lays the bits out the same way. That needs a real packet.

extern crate std;

use std::string::String;

use crate::legacy::{self, Error, Receiver, crc16, descale, lon_div, v6_key, v7_key};
use crate::tea::xxtea_encrypt;
use crate::{AddressType, AircraftType, KeyError, Keys};

/// Made-up keys for algorithm tests. Not FLARM's keys.
const FAKE_KEYS: &str = "\
# made-up test keys (not FLARM's)
key1 = 01234567 89abcdef 13579bdf 2468ace0 0f1e2d3c 4b5a6978 deadbeef 0badf00d
key2 = 0x1234567b
key3 = 0x76543210
key4 = 0x55aa55aa
key5 = 11111111, 22222222, 33333333, 44444444
";

fn fake() -> Keys {
    Keys::parse(FAKE_KEYS).unwrap()
}

/// The real keys, if `FLARM_KEYS_FILE` is set.
fn real() -> Option<Keys> {
    let Some(path) = std::env::var_os("FLARM_KEYS_FILE") else {
        std::println!("FLARM_KEYS_FILE not set: skipping the real-key test (passes)");
        return None;
    };
    let text = std::fs::read_to_string(&path).expect("FLARM_KEYS_FILE unreadable");
    Some(Keys::parse(&text).expect("FLARM_KEYS_FILE does not parse"))
}

// --------------------------------------------------------------- encoder

fn put(p: &mut [u8; 24], lsb: usize, n: usize, v: u32) {
    for i in 0..n {
        let b = lsb + i;
        if (v >> i) & 1 != 0 {
            p[b / 8] |= 1 << (b % 8);
        } else {
            p[b / 8] &= !(1 << (b % 8));
        }
    }
}

fn words(p: &[u8; 24]) -> [u32; 6] {
    core::array::from_fn(|i| {
        u32::from_le_bytes([p[4 * i], p[4 * i + 1], p[4 * i + 2], p[4 * i + 3]])
    })
}

fn bytes(w: &[u32; 6]) -> [u8; 24] {
    let mut p = [0u8; 24];
    for (i, x) in w.iter().enumerate() {
        p[4 * i..4 * i + 4].copy_from_slice(&x.to_le_bytes());
    }
    p
}

#[derive(Clone, Copy)]
struct Tx {
    addr: u32,
    acft: u8,
    lat: f64,
    lon: f64,
    /// metres above the ellipsoid
    alt: f64,
    course: f64,
    /// m/s
    speed: f64,
    /// m/s
    vs: f64,
    stealth: bool,
    no_track: bool,
    time: u32,
}

fn tug() -> Tx {
    Tx {
        addr: 0xDDA5BA,
        acft: 2,
        lat: 40.05,
        lon: -75.40,
        alt: 450.0,
        course: 135.0,
        speed: 30.0,
        vs: 2.5,
        stealth: false,
        no_track: false,
        time: 1_763_400_000,
    }
}

fn ground_threshold(acft: u8) -> f64 {
    AircraftType(acft)
        .airborne_threshold_mps()
        .map_or(1e9, f64::from)
}

/// Port of SoftRF's `legacy_v6_encode`.
fn encode_v6(t: &Tx, keys: &Keys) -> [u8; 24] {
    let mut p = [0u8; 24];
    let lat = (t.lat * 1e7) as i32;
    let lon = (t.lon * 1e7) as i32;
    let speed4 = libm::round(t.speed * 4.0).min(1023.0) as u32;
    let smult = if speed4 & 0x200 != 0 {
        3
    } else if speed4 & 0x100 != 0 {
        2
    } else if speed4 & 0x80 != 0 {
        1
    } else {
        0
    };
    let speed = (speed4 >> smult) as u8 as f64;
    let ns = (speed * libm::cos(t.course.to_radians())) as i8;
    let ew = (speed * libm::sin(t.course.to_radians())) as i8;
    let vs10 = libm::round(t.vs * 10.0) as i32;
    let vs = if t.stealth { 0 } else { vs10 >> smult };
    put(&mut p, 0, 24, t.addr);
    put(&mut p, 24, 4, 0);
    put(&mut p, 28, 3, 2); // FLARM address
    put(&mut p, 32, 10, vs as u32 & 0x3ff);
    let airborne = (t.speed as i32) as f64 >= ground_threshold(t.acft);
    put(&mut p, 44, 1, u32::from(airborne));
    put(&mut p, 45, 1, u32::from(t.stealth));
    put(&mut p, 46, 1, u32::from(t.no_track));
    put(&mut p, 48, 12, 323);
    put(&mut p, 60, 4, u32::from(t.acft));
    let round = |v: i32, shift: u32| {
        (v >> shift)
            + if v & 0x40 != 0 {
                if v < 0 { -1 } else { 1 }
            } else {
                0
            }
    };
    put(&mut p, 64, 19, round(lat, 7) as u32 & 0x7ffff);
    put(&mut p, 83, 13, t.alt.max(0.0) as u32);
    put(&mut p, 96, 20, round(lon, 7) as u32 & 0xfffff);
    put(&mut p, 126, 2, smult);
    for i in 0..4 {
        p[16 + i] = ns as u8;
        p[20 + i] = ew as u8;
    }
    let ones: u32 = p.iter().map(|b| b.count_ones()).sum();
    put(&mut p, 47, 1, ones % 2);
    let mut w = words(&p);
    let key = v6_key(keys, t.time, (t.addr << 8) & 0xff_ffff);
    xxtea_encrypt(&mut w[1..6], &key, 6);
    bytes(&w)
}

fn enscale_unsigned(value: u32, mbits: u32, ebits: u32) -> u32 {
    let offset = 1u32 << mbits;
    let max_val = (offset << ebits) - 1;
    if value < offset {
        return value;
    }
    let (mut e, mut m, mlimit) = (0u32, offset + value, offset + offset - 1);
    while m > mlimit {
        m >>= 1;
        e += offset;
        if e > max_val {
            return max_val;
        }
    }
    e | (m - offset)
}

fn enscale_signed(value: i32, mbits: u32, ebits: u32) -> u32 {
    let offset = 1u32 << mbits;
    let signbit = offset << ebits;
    let sign = if value < 0 { signbit } else { 0 };
    let v = value.unsigned_abs();
    if v < offset {
        return sign | v;
    }
    let max_val = signbit - 1;
    let (mut e, mut m, mlimit) = (0u32, offset + v, offset + offset - 1);
    while m > mlimit {
        m >>= 1;
        e += offset;
        if e > max_val {
            return sign | max_val;
        }
    }
    sign | e | (m - offset)
}

/// Port of SoftRF's `legacy_encode` (v7).
fn encode_v7(t: &Tx, keys: &Keys) -> [u8; 24] {
    let mut p = [0u8; 24];
    let lat = (t.lat * 1e7) as i32;
    let lon = (t.lon * 1e7) as i32;
    put(&mut p, 0, 24, t.addr);
    put(&mut p, 24, 4, 2);
    put(&mut p, 28, 2, 2);
    put(&mut p, 54, 1, u32::from(t.stealth));
    put(&mut p, 55, 1, u32::from(t.no_track));
    put(&mut p, 56, 4, 3);
    put(&mut p, 60, 4, 3);
    put(&mut p, 66, 4, t.time & 0xf);
    put(&mut p, 70, 5, u32::from(t.acft));
    let alt = ((t.alt as i32) + 1000).max(0) as u32;
    put(&mut p, 75, 13, enscale_unsigned(alt, 12, 1));
    let rlat = lat / 52
        + if lat & 0x40 != 0 {
            if lat < 0 { -1 } else { 1 }
        } else {
            0
        };
    put(&mut p, 88, 20, rlat as u32 & 0xfffff);
    put(&mut p, 108, 20, (lon / lon_div(t.lat)) as u32 & 0xfffff);
    put(
        &mut p,
        137,
        10,
        enscale_unsigned(libm::round(t.speed * 10.0) as u32, 8, 2),
    );
    let vs = if t.stealth {
        0
    } else {
        enscale_signed(libm::round(t.vs * 10.0) as i32, 6, 2)
    };
    put(&mut p, 147, 9, vs);
    put(&mut p, 156, 10, (t.course * 2.0) as u32);
    let airborne = if (t.speed as i32) as f64 >= ground_threshold(t.acft) {
        2
    } else {
        1
    };
    put(&mut p, 166, 2, airborne);
    put(&mut p, 168, 6, 0x1d);
    put(&mut p, 174, 5, 0x11);
    let mut w = words(&p);
    let k = v7_key([w[0], w[1], t.time >> 4, keys.v7_word]);
    for i in 0..4 {
        w[2 + i] ^= k[i];
    }
    xxtea_encrypt(&mut w[2..6], &keys.v7_xxtea, 6);
    bytes(&w)
}

fn with_crc(p: &[u8; 24]) -> [u8; 26] {
    let mut out = [0u8; 26];
    out[..24].copy_from_slice(p);
    out[24..].copy_from_slice(&crc16(p).to_be_bytes());
    out
}

/// A receiver about 15 km from the transmitter.
fn rx_near(t: &Tx) -> Receiver {
    Receiver {
        unix_time: t.time,
        lat_deg: t.lat - 0.1,
        lon_deg: t.lon + 0.12,
    }
}

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

fn check_round_trip(t: &Tx, d: &crate::Decoded, v7: bool) {
    let a = d.aircraft;
    assert_eq!(a.source, adsb::Source::Flarm);
    assert_eq!(a.icao24(), t.addr);
    assert_eq!(d.address_type, AddressType::Flarm);
    assert_eq!(d.aircraft_type, AircraftType(t.acft));
    assert_eq!((d.stealth, d.no_track), (t.stealth, t.no_track));
    let (lat, lon) = a.position().unwrap();
    // v6 keeps 2^7 x 1e-7 deg (~1.4 m; SoftRF's rounding of negative
    // values can cost one more step); v7 52 x 1e-7 deg in latitude and up
    // to 806 x 1e-7 deg in longitude.
    let lon_tol = if v7 {
        f64::from(lon_div(t.lat)) * 1.01e-7
    } else {
        2.6e-5
    };
    assert!(close(lat, t.lat, 2.6e-5), "lat {lat} vs {}", t.lat);
    assert!(close(lon, t.lon, lon_tol), "lon {lon} vs {}", t.lon);
    let alt_ft = a.gnss_altitude_ft().unwrap();
    assert!(
        (f64::from(alt_ft) - t.alt / 0.3048).abs() < 4.0,
        "alt {alt_ft}"
    );
    let spd = f64::from(a.speed_kt().unwrap()) / (3600.0 / 1852.0);
    assert!(close(spd, t.speed, 1.5), "speed {spd} m/s vs {}", t.speed);
    let dir = f64::from(a.direction_deg().unwrap());
    let dd = (dir - t.course).rem_euclid(360.0);
    assert!(dd.min(360.0 - dd) < 3.0, "course {dir} vs {}", t.course);
    if !t.stealth {
        let vs = f64::from(a.vertical_rate_fpm().unwrap()) * 0.3048 / 60.0;
        assert!(close(vs, t.vs, 0.25), "vs {vs} vs {}", t.vs);
    }
    assert!(a.heard() && a.position_updated());
}

// ----------------------------------------------------------------- keys

#[test]
fn key_file_parses_and_never_shows_its_values() {
    let k = fake();
    let shown = std::format!("{k:?}");
    assert_eq!(shown, "Keys(<redacted>)");
    for word in ["01234567", "deadbeef", "1234567b", "55aa55aa", "11111111"] {
        assert!(!shown.to_lowercase().contains(word));
    }
    // upper case, 0x prefixes, comments after values, any order
    let text = "key5 = 0X1 2 3 4 # x\nkey4=5\n\n  KEY3 = 6\nkey2 = 7\nkey1 = 1,2,3,4,5,6,7,8\n";
    assert!(Keys::parse(text).is_ok());
}

#[test]
fn key_file_errors_name_lines_not_contents() {
    let cases: [(&str, KeyError); 7] = [
        ("key1 1 2 3", KeyError::Syntax(1)),
        ("# c\nkey9 = 1", KeyError::UnknownName(2)),
        ("key2 = 1\nkey2 = 2", KeyError::Duplicate(2)),
        ("key2 = 1g", KeyError::BadWord(1)),
        ("key2 = 123456789", KeyError::BadWord(1)),
        ("key5 = 1 2 3", KeyError::WordCount(1)),
        ("key2 = 1 2", KeyError::WordCount(1)),
    ];
    for (text, want) in cases {
        assert_eq!(Keys::parse(text).unwrap_err(), want, "{text:?}");
    }
    let missing = FAKE_KEYS.replace("key4 = 0x55aa55aa\n", "");
    assert_eq!(
        Keys::parse(&missing).unwrap_err(),
        KeyError::Missing("key4")
    );
    assert_eq!(Keys::parse("").unwrap_err(), KeyError::Missing("key1"));
}

// ---------------------------------------------------------------- pieces

/// CRC-16/CCITT-FALSE, bit by bit, for checking.
fn ccitt_false(data: &[u8]) -> u16 {
    let mut crc = 0xffffu16;
    for &b in data {
        for i in (0..8).rev() {
            let bit = ((b >> i) & 1) as u16 ^ (crc >> 15);
            crc <<= 1;
            if bit != 0 {
                crc ^= 0x1021;
            }
        }
    }
    crc
}

#[test]
fn crc_is_ccitt_false_over_sync_tail_and_payload() {
    assert_eq!(ccitt_false(b"123456789"), 0x29B1);
    let payload: [u8; 24] = core::array::from_fn(|i| (i * 37) as u8);
    let mut all = std::vec![0x31u8, 0xFA, 0xB6];
    all.extend_from_slice(&payload);
    assert_eq!(crc16(&payload), ccitt_false(&all));
}

#[test]
fn descale_matches_enscale() {
    for v in 0..12_286u32 {
        let d = descale(enscale_unsigned(v, 12, 1), 12, 1);
        assert!(d.abs_diff(v as i32) <= 1, "{v} -> {d}");
    }
    for v in 0..3832u32 {
        let d = descale(enscale_unsigned(v, 8, 2), 8, 2);
        assert!(d as u32 <= v && v - d as u32 <= 7, "{v} -> {d}");
    }
    for v in -952..=952i32 {
        let d = descale(enscale_signed(v, 6, 2), 6, 2);
        assert!(d.abs() <= v.abs() && (v - d).abs() <= 7, "{v} -> {d}");
    }
    assert_eq!(descale(0x1fff, 12, 1), 12_286);
    assert_eq!(descale(0x1ff, 6, 2), -952);
}

#[test]
fn v7_key_mix_depends_on_every_input_word() {
    let base = v7_key([1, 2, 3, 4]);
    for i in 0..4 {
        let mut w = [1, 2, 3, 4];
        w[i] ^= 0x100;
        assert_ne!(v7_key(w), base, "word {i}");
    }
}

#[test]
fn lon_div_table_edges() {
    assert_eq!(lon_div(0.0), 52);
    assert_eq!(lon_div(13.9), 52);
    assert_eq!(lon_div(-14.0), 53);
    assert_eq!(lon_div(40.0), 67);
    assert_eq!(lon_div(89.9), 806);
    assert_eq!(lon_div(95.0), 806);
}

// ------------------------------------------------------------ round trips

#[test]
fn v6_round_trip_with_made_up_keys() {
    let k = fake();
    for (i, (lat, lon, course)) in [
        (40.05, -75.40, 135.0),
        (-33.9, 151.2, 10.0),
        (51.5, -0.1, 270.0),
        (0.01, -0.01, 359.0),
    ]
    .into_iter()
    .enumerate()
    {
        let mut t = tug();
        t.lat = lat;
        t.lon = lon;
        t.course = course;
        t.time += i as u32 * 1000;
        let pk = encode_v6(&t, &k);
        let d = legacy::decode(&pk, &k, &rx_near(&t)).unwrap();
        check_round_trip(&t, &d, false);
        assert!(d.aircraft.airborne());
        assert_eq!(d.aircraft.kind(), adsb::Kind::Light);
    }
}

#[test]
fn v7_round_trip_with_made_up_keys() {
    let k = fake();
    for (i, (lat, lon, course)) in [
        (40.05, -75.40, 135.0),
        (-33.9, 151.2, 10.0),
        (64.8, -147.7, 270.0),
        (5.0, 100.0, 359.5),
    ]
    .into_iter()
    .enumerate()
    {
        let mut t = tug();
        t.lat = lat;
        t.lon = lon;
        t.course = course;
        t.acft = 1;
        t.time += i as u32 * 777;
        let pk = encode_v7(&t, &k);
        let d = legacy::decode(&with_crc(&pk), &k, &rx_near(&t)).unwrap();
        check_round_trip(&t, &d, true);
        assert_eq!(d.aircraft.kind(), adsb::Kind::Glider);
    }
}

#[test]
fn stealth_no_track_and_ground() {
    let k = fake();
    let mut t = tug();
    t.stealth = true;
    t.no_track = true;
    t.speed = 3.0; // below the tow plane's 10 m/s
    for pk in [encode_v6(&t, &k), encode_v7(&t, &k)] {
        let d = legacy::decode(&pk, &k, &rx_near(&t)).unwrap();
        assert!(d.stealth && d.no_track);
        assert!(!d.aircraft.airborne());
        assert_eq!(d.aircraft.vertical_rate_fpm(), Some(0));
    }
}

#[test]
fn receiver_time_in_the_same_window_still_decodes() {
    let k = fake();
    let t = tug(); // time is a multiple of 64
    let mut rx = rx_near(&t);
    rx.unix_time = t.time + 15; // same 16 s (v7) and 64 s (v6) window
    check_round_trip(
        &t,
        &legacy::decode(&encode_v7(&t, &k), &k, &rx).unwrap(),
        true,
    );
    check_round_trip(
        &t,
        &legacy::decode(&encode_v6(&t, &k), &k, &rx).unwrap(),
        false,
    );
}

#[test]
fn wrong_time_or_keys_do_not_give_the_right_aircraft() {
    let k = fake();
    let t = tug();
    // (v6 uses table words 0-3 or 4-7 by time; change one of each half)
    let other = Keys::parse(
        &FAKE_KEYS
            .replace("01234567", "01234566")
            .replace("deadbeef", "deadbeee")
            .replace("44444444", "44444445"),
    )
    .unwrap();
    let mut wrong_rx = rx_near(&t);
    wrong_rx.unix_time = t.time + 64;
    let mut parity_failures = 0;
    for (keys, rx) in [(&k, wrong_rx), (&other, rx_near(&t))] {
        for pk in [encode_v6(&t, &k), encode_v7(&t, &k)] {
            match legacy::decode(&pk, keys, &rx) {
                Err(Error::Parity) => parity_failures += 1,
                Err(e) => panic!("{e:?}"),
                Ok(d) => {
                    // garbage: not where the aircraft is
                    let (la, lo) = d.aircraft.position().unwrap();
                    assert!(!(close(la, t.lat, 0.01) && close(lo, t.lon, 0.01)));
                }
            }
        }
    }
    std::println!("v6 parity caught {parity_failures} of 2 wrong-key cases");
}

#[test]
fn crc_length_and_type_checks() {
    let k = fake();
    let t = tug();
    let rx = rx_near(&t);
    let mut pk = with_crc(&encode_v7(&t, &k));
    assert!(legacy::decode(&pk, &k, &rx).is_ok());
    pk[10] ^= 0x04;
    assert_eq!(legacy::decode(&pk, &k, &rx).unwrap_err(), Error::Crc);
    assert_eq!(
        legacy::decode(&pk[..25], &k, &rx).unwrap_err(),
        Error::Length
    );
    let mut other = encode_v7(&t, &k);
    other[3] = (other[3] & 0xf0) | 1;
    assert_eq!(
        legacy::decode(&other, &k, &rx).unwrap_err(),
        Error::NotPosition(1)
    );
}

#[test]
fn v6_single_bit_errors_fail_parity() {
    // Without the CRC (a 24-byte input) only the v6 parity bit is left. One
    // flipped bit scrambles the whole decrypted block, so about half the
    // cases are caught: a weak check, which is why the CRC matters.
    let k = fake();
    let t = tug();
    let good = encode_v6(&t, &k);
    let mut caught = 0;
    for bit in 32..192 {
        let mut bad = good;
        bad[bit / 8] ^= 1 << (bit % 8);
        if legacy::decode(&bad, &k, &rx_near(&t)) == Err(Error::Parity) {
            caught += 1;
        }
    }
    assert!(caught > 40, "{caught}");
}

#[test]
fn flarm_record_passes_through_csbee() {
    let k = fake();
    let t = tug();
    let a = legacy::decode(&encode_v7(&t, &k), &k, &rx_near(&t))
        .unwrap()
        .aircraft;
    let mut line = [0u8; adsb::MAX_LINE];
    let n = adsb::encode::aircraft(&a, &mut line).unwrap();
    let text = String::from_utf8(line[..n].to_vec()).unwrap();
    assert!(text.starts_with("#F:2DDA5BA,"), "{text}");
}

// ------------------------------------------------------------- real keys

#[test]
fn real_keys_round_trip_v6_and_v7() {
    let Some(k) = real() else { return };
    let t = tug();
    let rx = rx_near(&t);
    check_round_trip(
        &t,
        &legacy::decode(&encode_v6(&t, &k), &k, &rx).unwrap(),
        false,
    );
    check_round_trip(
        &t,
        &legacy::decode(&with_crc(&encode_v7(&t, &k)), &k, &rx).unwrap(),
        true,
    );
    // The real keys and the made-up ones give different ciphertext.
    assert_ne!(encode_v7(&t, &k), encode_v7(&t, &fake()));
    std::println!("real-key round trips passed (key values not shown)");
}
