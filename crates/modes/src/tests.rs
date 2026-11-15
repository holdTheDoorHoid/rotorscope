//! Decoder tests.
//!
//! Published frames are cited where used:
//! - **Riddle**: Junzi Sun, *The 1090 Megahertz Riddle*, 2nd ed. (TU Delft
//!   OPEN, 2021), <https://mode-s.org/1090mhz/>, the worked examples.
//! - **pyModeS**: the test suite of pyModeS (GPL-3.0,
//!   <https://github.com/junzis/pyModeS>, `tests/`), frame strings and the
//!   values it asserts.
//!
//! Everything else is built here with the encoder helpers below, or is our
//! own recording from the owner's ADSBee (2026-11-15, Philadelphia).

extern crate std;

use std::vec::Vec;

use crate::alt::{self, Altitude};
use crate::commb::{self, Bds, Hint};
use crate::cpr::{self, Cpr, Kind as CprKind};
use crate::crc;
use crate::decode::{AddressCheck, Body, EsSource, Reject, decode};
use crate::es::{self, Es};
use crate::frame::{Fix, Frame, correct};
use crate::input::{self, BeastReader};

pub(crate) fn f(hex: &str) -> Frame {
    Frame::from_hex(hex).unwrap_or_else(|| panic!("bad test frame {hex}"))
}

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

/// Set ME/MB bits (1-based `first`, `n` bits) in a 7-byte field.
pub(crate) fn put(m: &mut [u8; 7], first: usize, n: usize, v: u32) {
    for k in 0..n {
        let bit = (v >> (n - 1 - k)) & 1;
        let i = first - 1 + k;
        if bit == 1 {
            m[i / 8] |= 0x80 >> (i % 8);
        } else {
            m[i / 8] &= !(0x80 >> (i % 8));
        }
    }
}

/// A DF17/18 frame with a correct CRC.
pub(crate) fn es_frame(df: u8, ca: u8, icao: u32, me: [u8; 7]) -> Frame {
    let mut b = [0u8; 14];
    b[0] = df << 3 | ca;
    b[1..4].copy_from_slice(&icao.to_be_bytes()[1..]);
    b[4..11].copy_from_slice(&me);
    let p = crc::crc24(&b[..11]);
    b[11..].copy_from_slice(&p.to_be_bytes()[1..]);
    Frame::new(&b).unwrap()
}

/// An address/parity frame: `head` is everything but the parity.
pub(crate) fn ap_frame(head: &[u8], icao: u32) -> Frame {
    let mut b = [0u8; 14];
    b[..head.len()].copy_from_slice(head);
    let p = crc::crc24(head) ^ icao;
    b[head.len()..head.len() + 3].copy_from_slice(&p.to_be_bytes()[1..]);
    Frame::new(&b[..head.len() + 3]).unwrap()
}

/// A DF4/DF20 head with the given AC13 field (and FS 0).
pub(crate) fn df4_head(df: u8, ac: u16, mb: Option<[u8; 7]>) -> Vec<u8> {
    let mut v = std::vec![df << 3, 0, 0, 0];
    v[2] = ((ac >> 8) & 0x1F) as u8;
    v[3] = (ac & 0xFF) as u8;
    if let Some(mb) = mb {
        v.extend_from_slice(&mb);
    }
    v
}

pub(crate) fn airborne_me(tc: u8, alt_ft: i32, lat: f64, lon: f64, odd: bool) -> [u8; 7] {
    let mut m = [0u8; 7];
    put(&mut m, 1, 5, u32::from(tc));
    put(&mut m, 9, 12, u32::from(alt::ac12_field_25ft(alt_ft).unwrap()));
    let c = cpr::encode(lat, lon, odd, CprKind::Airborne);
    put(&mut m, 22, 1, u32::from(odd));
    put(&mut m, 23, 17, c.lat);
    put(&mut m, 40, 17, c.lon);
    m
}

// ---------------------------------------------------------------- CRC

#[test]
fn crc_clean_frames_have_zero_syndrome() {
    for h in [
        "8D4840D6202CC371C32CE0576098", // Riddle, identification
        "8D40621D58C382D690C8AC2863A7", // Riddle, position (even)
        "8D40621D58C386435CC412692AD6", // Riddle, position (odd)
        "8D485020994409940838175B284F", // Riddle, velocity
        "8DA05F219B06B6AF189400CBC33F", // Riddle, airspeed
        "8D406B902015A678D4D220AA4BDA", // pyModeS, identification
    ] {
        assert_eq!(f(h).syndrome(), 0, "{h}");
    }
}

#[test]
fn bit_syndromes_are_distinct_and_nonzero() {
    for long in [false, true] {
        let s = crate::frame::bit_syndromes(long);
        for (i, a) in s.iter().enumerate() {
            assert_ne!(*a, 0);
            for b in &s[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }
}

#[test]
fn address_parity_recovers_the_address() {
    // pyModeS tests/test_c_common: icao() of address/parity frames.
    assert_eq!(f("A0001839CA3800315800007448D9").syndrome(), 0x400940);
    assert_eq!(f("A000139381951536E024D4CCF6B5").syndrome(), 0x3C4DD2);
    assert_eq!(f("A000029CFFBAA11E2004727281F1").syndrome(), 0x4243D0);
}

// ---------------------------------------------------------------- repair

#[test]
fn one_bit_errors_are_repaired_everywhere_but_the_df_field() {
    let clean = f("8D4840D6202CC371C32CE0576098");
    for bit in 0..112 {
        let mut x = clean;
        x.flip(bit);
        let r = correct(&mut x, 1);
        if bit < 5 {
            // DF field: either no longer DF17/18, or refused.
            assert!(r.is_none(), "bit {bit}");
        } else {
            assert_eq!(r, Some(Fix::One(bit as u8)), "bit {bit}");
            assert_eq!(x, clean);
        }
    }
}

#[test]
fn two_bit_errors_are_repaired_only_when_allowed() {
    let clean = f("8D40621D58C382D690C8AC2863A7");
    let mut n = 0;
    for i in 5..112 {
        for j in (i + 1..112).step_by(7) {
            let mut x = clean;
            x.flip(i);
            x.flip(j);
            assert_eq!(correct(&mut x.clone(), 1), None);
            let r = correct(&mut x, 2);
            assert_eq!(r, Some(Fix::Two(i as u8, j as u8)));
            assert_eq!(x, clean);
            n += 1;
        }
    }
    assert!(n > 500);
}

#[test]
fn three_bit_errors_are_mostly_refused() {
    // With 3 wrong bits a 2-bit "repair" can produce a wrong frame that
    // passes the CRC. Count how often: the table guards against these by
    // only accepting 2-bit repairs for aircraft it already knows.
    let clean = f("8D485020994409940838175B284F");
    let (mut refused, mut wrong, mut total) = (0, 0, 0);
    for i in (5..112).step_by(5) {
        for j in (i + 1..112).step_by(9) {
            for k in (j + 1..112).step_by(13) {
                let mut x = clean;
                x.flip(i);
                x.flip(j);
                x.flip(k);
                total += 1;
                match correct(&mut x, 2) {
                    None => refused += 1,
                    Some(_) => {
                        assert_ne!(x, clean);
                        wrong += 1;
                    }
                }
            }
        }
    }
    assert_eq!(refused + wrong, total);
    // Distance 6: a 3-bit error is never within 2 of the right codeword,
    // and only sometimes within 2 of another one.
    assert!(wrong * 4 < total, "{wrong} wrong of {total}");
}

#[test]
fn address_parity_formats_are_never_repaired() {
    let mut x = f("A0001839CA3800315800007448D9");
    assert_eq!(correct(&mut x, 2), None);
}

// ---------------------------------------------------------------- altitude, squawk

/// Gillham encoder, from the definition (tests only).
fn gillham_encode(feet: i32) -> u16 {
    let n = feet + 1200;
    assert!(n >= 0 && n % 100 == 0);
    let fives = (n / 500) as u16;
    let mut h = ((n % 500) / 100 + 1) as u16;
    if fives & 1 == 1 {
        h = 6 - h;
    }
    if h == 5 {
        h = 7;
    }
    let g500 = fives ^ (fives >> 1);
    let g100 = h ^ (h >> 1);
    let b = |v: u16, k: u16| (v >> k) & 1;
    // C1 A1 C2 A2 C4 A4 M B1 Q B2 D2 B4 D4
    b(g100, 2) << 12
        | b(g500, 5) << 11
        | b(g100, 1) << 10
        | b(g500, 4) << 9
        | b(g100, 0) << 8
        | b(g500, 3) << 7
        | b(g500, 2) << 5
        | b(g500, 1) << 3
        | b(g500, 7) << 2
        | b(g500, 0) << 1
        | b(g500, 6)
}

#[test]
fn gillham_roundtrip_and_gray_property() {
    let mut prev: Option<u16> = None;
    for feet in (-1200..=126_700).step_by(100) {
        let code = gillham_encode(feet);
        assert_eq!(alt::ac13(code), Altitude::Feet(feet), "{feet} ft");
        // A real Gillham code changes exactly one bit per 100 ft step.
        if let Some(p) = prev {
            assert_eq!((p ^ code).count_ones(), 1, "{feet} ft");
        }
        prev = Some(code);
    }
}

#[test]
fn gillham_rejects_illegal_hundreds() {
    // C bits all zero, and C1 C2 C4 = 101 / 111 (binary 6 and 5): not altitudes.
    assert_eq!(alt::ac13(0x0080), Altitude::Invalid);
    assert_eq!(alt::ac13(0x1100 | 0x0080), Altitude::Invalid);
}

#[test]
fn q_bit_altitudes() {
    for feet in (-1000..=50_175).step_by(25) {
        let f12 = alt::ac12_field_25ft(feet).unwrap();
        assert_eq!(alt::ac12(f12), Altitude::Feet(feet));
    }
    // pyModeS tests/test_c_common: altcode of a DF20 reply.
    let x = f("A02014B400000000000000F9D514");
    assert_eq!(alt::ac13(x.field(19, 13) as u16), Altitude::Feet(32300));
    assert_eq!(alt::ac13(0), Altitude::Unavailable);
    assert_eq!(alt::ac13(0x0040), Altitude::Metric);
}

#[test]
fn squawks() {
    for code in 0..0o10000u16 {
        assert_eq!(alt::squawk(alt::squawk_field(code)), code);
    }
    // pyModeS tests/test_c_common: idcode of a DF21 reply is 1346.
    let x = f("A800292DFFBBA9383FFCEB903D01");
    assert_eq!(alt::squawk(x.field(19, 13) as u16), 0o1346);
}

// ---------------------------------------------------------------- formats

#[test]
fn identification() {
    // Riddle 3.x: KLM1023, emitter category A0 (TC 4, CA 0).
    let mut x = f("8D4840D6202CC371C32CE0576098");
    let m = decode(&mut x, 0).unwrap();
    assert_eq!(m.address, 0x4840D6);
    assert_eq!(m.check, AddressCheck::Verified(Fix::Clean));
    let Body::Extended { source: EsSource::AdsB, es: Es::Identification { tc, category, callsign }, .. } = m.body
    else {
        panic!("{m:?}")
    };
    assert_eq!((tc, category), (4, 0));
    assert_eq!(callsign.as_str(), "KLM1023");
    // pyModeS tests/test_adsb: EZY85MH.
    let mut y = f("8D406B902015A678D4D220AA4BDA");
    let Body::Extended { es: Es::Identification { callsign, .. }, .. } = decode(&mut y, 0).unwrap().body else {
        panic!()
    };
    assert_eq!(callsign.as_str(), "EZY85MH");
    assert_eq!(es::category_number(4, 1), 1); // A1 light
    assert_eq!(es::category_number(4, 7), 7); // A7 rotorcraft
    assert_eq!(es::category_number(3, 1), 9); // B1 glider
    assert_eq!(es::category_number(3, 6), 14); // B6 UAV
    assert_eq!(adsb::Kind::of(es::category_number(4, 7)), adsb::Kind::Rotorcraft);
}

fn airborne(hex: &str) -> (Altitude, Cpr) {
    let mut x = f(hex);
    match decode(&mut x, 0).unwrap().body {
        Body::Extended { es: Es::AirbornePosition { altitude, cpr, gnss: false, .. }, .. } => (altitude, cpr),
        b => panic!("{b:?}"),
    }
}

#[test]
fn cpr_global_riddle_example() {
    // Riddle 5.2: even then odd frame, even newest; 52.2572, 3.91937, 38000 ft.
    let (a0, e) = airborne("8D40621D58C382D690C8AC2863A7");
    let (a1, o) = airborne("8D40621D58C386435CC412692AD6");
    assert_eq!(a0, Altitude::Feet(38000));
    assert_eq!(a1, Altitude::Feet(38000));
    assert!(!e.odd && o.odd);
    let (lat, lon) = cpr::global(e, o, false, CprKind::Airborne, None).unwrap();
    assert!(close(lat, 52.2572, 1e-4) && close(lon, 3.91937, 1e-4), "{lat} {lon}");
    // Riddle 5.3: the even frame decoded locally against (52.258, 3.918).
    let (lat, lon) = cpr::local(e, CprKind::Airborne, (52.258, 3.918));
    assert!(close(lat, 52.2572, 1e-4) && close(lon, 3.91937, 1e-4), "{lat} {lon}");
}

#[test]
fn cpr_global_pymodes_example() {
    // pyModeS tests/test_adsb: position(even, odd, t_even < t_odd) gives
    // (49.81755, 6.08442), altitude 39000 ft.
    let (a, e) = airborne("8D40058B58C901375147EFD09357");
    let (_, o) = airborne("8D40058B58C904A87F402D3B8C59");
    assert_eq!(a, Altitude::Feet(39000));
    let (lat, lon) = cpr::global(e, o, true, CprKind::Airborne, None).unwrap();
    assert!(close(lat, 49.81755, 1e-5) && close(lon, 6.08442, 1e-5), "{lat} {lon}");
    // ...and position_with_ref(even, 49.0, 6.0) = (49.82410, 6.06785).
    let (lat, lon) = cpr::local(e, CprKind::Airborne, (49.0, 6.0));
    assert!(close(lat, 49.82410, 1e-5) && close(lon, 6.06785, 1e-5), "{lat} {lon}");
}

#[test]
fn nl_table_matches_formula() {
    use core::f64::consts::PI;
    for nl in 2..=59u32 {
        let a = 1.0 - libm::cos(PI / (2.0 * cpr::NZ));
        let b = 1.0 - libm::cos(2.0 * PI / f64::from(nl));
        let edge = libm::acos(libm::sqrt(a / b)).to_degrees();
        assert!(close(edge, cpr::NL_EDGES[(59 - nl) as usize], 1e-8), "NL {nl}");
        assert_eq!(cpr::nl(edge - 1e-6), nl);
        assert_eq!(cpr::nl(-(edge - 1e-6)), nl);
        assert_eq!(cpr::nl(edge + 1e-6), nl - 1);
    }
    assert_eq!(cpr::nl(0.0), 59);
    assert_eq!(cpr::nl(89.9), 1);
}

#[test]
fn cpr_roundtrip_worldwide() {
    // Encode and decode a grid of positions, both orders, globally and
    // locally. Resolution is ~5 m airborne.
    let mut n = 0;
    let mut lat = -84.5;
    while lat < 85.0 {
        let mut lon = -179.7;
        while lon < 180.0 {
            let e = cpr::encode(lat, lon, false, CprKind::Airborne);
            let o = cpr::encode(lat, lon, true, CprKind::Airborne);
            for odd_newer in [false, true] {
                match cpr::global(e, o, odd_newer, CprKind::Airborne, None) {
                    Ok((la, lo)) => {
                        assert!(close(la, lat, 1e-4), "{lat},{lon} -> {la}");
                        let dlon = ((lo - lon + 540.0) % 360.0) - 180.0;
                        assert!(dlon.abs() < 1e-3, "{lat},{lon} -> {lo}");
                        n += 1;
                    }
                    Err(e) => panic!("{lat},{lon}: {e:?}"),
                }
            }
            let (la, lo) = cpr::local(o, CprKind::Airborne, (lat + 0.7, lon - 0.9));
            assert!(close(la, lat, 1e-4) && close(lo, lon, 1e-3), "{lat},{lon} local");
            lon += 7.3;
        }
        lat += 3.1;
    }
    assert!(n > 2500);
}

#[test]
fn cpr_zone_mismatch_detected() {
    // Even and odd 550 m apart, either side of the NL 59/58 edge
    // (10.4705 N).
    let e = cpr::encode(10.468, 20.0, false, CprKind::Airborne);
    let o = cpr::encode(10.473, 20.0, true, CprKind::Airborne);
    assert_eq!(
        cpr::global(e, o, true, CprKind::Airborne, None),
        Err(cpr::CprError::ZoneMismatch)
    );
}

fn surface(hex: &str) -> (Option<f32>, Option<f32>, Cpr) {
    let mut x = f(hex);
    match decode(&mut x, 0).unwrap().body {
        Body::Extended { es: Es::SurfacePosition { speed_kt, track_deg, cpr, .. }, .. } => (speed_kt, track_deg, cpr),
        b => panic!("{b:?}"),
    }
}

#[test]
fn surface_position_riddle_example() {
    // Riddle 6: an even and an odd surface frame (odd received last),
    // reference (51.990, 4.375): 52.32061, 4.73473.
    let (speed_kt, track_deg, e) = surface("8C4841753AAB238733C8CD4020B1");
    let (_, _, o) = surface("8C4841753A8A35323FAEBDAC702D");
    assert!(!e.odd && o.odd);
    let (lat, lon) = cpr::global(e, o, true, CprKind::Surface, Some((51.990, 4.375))).unwrap();
    assert!(close(lat, 52.32061, 1e-5) && close(lon, 4.73473, 1e-5), "{lat} {lon}");
    // Each decoded locally lands within a few hundred metres of that (the
    // aircraft taxied between the two).
    for c in [e, o] {
        let (la, lo) = cpr::local(c, CprKind::Surface, (51.990, 4.375));
        assert!(close(la, 52.3206, 0.004) && close(lo, 4.7347, 0.006), "{la} {lo}");
    }
    // Movement code 42 and track code 50 (read off the bits): 18 kt by the
    // DO-260B movement table, 140.6 degrees.
    assert_eq!(speed_kt, Some(18.0));
    assert!(close(f64::from(track_deg.unwrap()), 140.625, 1e-3));
}

#[test]
fn surface_global_both_hemispheres() {
    for (lat, lon, rf) in [
        (39.8719, -75.2411, (39.95, -75.16)),   // PHL
        (-43.4856, 172.5394, (-43.496, 172.558)), // Christchurch
        (-33.9461, 151.1772, (-33.9, 151.2)),   // Sydney
        (51.4700, -0.4543, (51.5, -0.1)),       // Heathrow
    ] {
        let e = cpr::encode(lat, lon, false, CprKind::Surface);
        let o = cpr::encode(lat, lon, true, CprKind::Surface);
        let (la, lo) = cpr::global(e, o, true, CprKind::Surface, Some(rf)).unwrap();
        assert!(close(la, lat, 1e-4) && close(lo, lon, 1e-4), "{lat},{lon} -> {la},{lo}");
        assert_eq!(
            cpr::global(e, o, true, CprKind::Surface, None),
            Err(cpr::CprError::NeedReference)
        );
    }
}

#[test]
fn velocity_ground_speed() {
    // Riddle 7.1: 159.20 kt, 182.88 degrees, -832 ft/min (source bit 0:
    // GNSS), GNSS height 550 ft above barometric.
    let mut x = f("8D485020994409940838175B284F");
    let Body::Extended { es: Es::Velocity(v), .. } = decode(&mut x, 0).unwrap().body else { panic!() };
    assert_eq!(v.subtype, 1);
    assert!(close(f64::from(v.ground_speed_kt.unwrap()), 159.20, 0.01));
    assert!(close(f64::from(v.track_deg.unwrap()), 182.88, 0.01));
    assert_eq!(v.vertical_rate_fpm, Some(-832));
    assert!(!v.vertical_rate_baro);
    assert_eq!(v.gnss_minus_baro_ft, Some(550));
}

#[test]
fn velocity_airspeed() {
    // Riddle 7.2: true airspeed 375 kt, heading 243.98, -2304 ft/min.
    let mut x = f("8DA05F219B06B6AF189400CBC33F");
    let Body::Extended { es: Es::Velocity(v), .. } = decode(&mut x, 0).unwrap().body else { panic!() };
    assert_eq!(v.subtype, 3);
    assert_eq!(v.airspeed_kt, Some(375));
    assert!(v.airspeed_true);
    assert!(close(f64::from(v.heading_deg.unwrap()), 243.98, 0.01));
    assert_eq!(v.vertical_rate_fpm, Some(-2304));
    assert!(v.ground_speed_kt.is_none());
}

#[test]
fn velocity_supersonic_subtypes_scale_by_four() {
    let mut m = [0u8; 7];
    put(&mut m, 1, 5, 19);
    put(&mut m, 6, 3, 2);
    put(&mut m, 14, 1, 0);
    put(&mut m, 15, 10, 301); // 300 * 4 = 1200 kt east
    put(&mut m, 25, 1, 0);
    put(&mut m, 26, 10, 1); // 0 north
    let Es::Velocity(v) = es::decode(&m) else { panic!() };
    assert!(close(f64::from(v.ground_speed_kt.unwrap()), 1200.0, 0.01));
    assert!(close(f64::from(v.track_deg.unwrap()), 90.0, 0.01));
    put(&mut m, 6, 3, 4);
    put(&mut m, 14, 1, 1);
    put(&mut m, 15, 10, 256); // 90 degrees
    put(&mut m, 25, 1, 0);
    put(&mut m, 26, 10, 101); // 400 kt IAS
    let Es::Velocity(v) = es::decode(&m) else { panic!() };
    assert_eq!(v.airspeed_kt, Some(400));
    assert!(!v.airspeed_true);
    assert!(close(f64::from(v.heading_deg.unwrap()), 90.0, 0.01));
}

#[test]
fn emergency_and_operational_status() {
    let mut m = [0u8; 7];
    put(&mut m, 1, 5, 28);
    put(&mut m, 6, 3, 1);
    put(&mut m, 9, 3, 2); // medical
    put(&mut m, 12, 13, u32::from(alt::squawk_field(0o7700)));
    assert_eq!(es::decode(&m), Es::EmergencyStatus { emergency: 2, squawk: 0o7700 });

    let mut m = [0u8; 7];
    put(&mut m, 1, 5, 31);
    put(&mut m, 6, 3, 0);
    put(&mut m, 41, 3, 2);
    put(&mut m, 44, 1, 1);
    put(&mut m, 45, 4, 9);
    put(&mut m, 49, 2, 2);
    put(&mut m, 51, 2, 3);
    put(&mut m, 54, 1, 1);
    let Es::OperationalStatus(o) = es::decode(&m) else { panic!() };
    assert_eq!((o.version, o.nic_a, o.nacp, o.gva, o.sil, o.hrd_magnetic), (2, true, 9, 2, 3, true));
    assert_eq!(o.lw_code, None);
}

#[test]
fn gnss_height_type_codes() {
    let me = airborne_me(20, 1525, 39.95, -75.16, false);
    let mut x = es_frame(17, 5, 0xA1B2C3, me);
    let Body::Extended { es: Es::AirbornePosition { gnss, altitude, tc, .. }, .. } = decode(&mut x, 0).unwrap().body
    else {
        panic!()
    };
    assert!(gnss);
    assert_eq!(tc, 20);
    assert_eq!(altitude, Altitude::Feet(1525));
}

#[test]
fn df18_sources() {
    let me = airborne_me(11, 3000, 39.9, -75.2, false);
    let cases = [
        (0u8, EsSource::NonTransponder { icao: true }),
        (1, EsSource::NonTransponder { icao: false }),
        (2, EsSource::TisB { icao: true, fine: true }),
        (5, EsSource::TisB { icao: false, fine: true }),
        (6, EsSource::AdsR { icao: true }),
        (4, EsSource::Other(4)),
    ];
    for (cf, want) in cases {
        let mut x = es_frame(18, cf, 0xA00001, me);
        let Body::Extended { source, es, .. } = decode(&mut x, 0).unwrap().body else { panic!() };
        assert_eq!(source, want, "CF {cf}");
        if matches!(want, EsSource::Other(_)) {
            assert!(matches!(es, Es::Other { .. }));
        } else {
            assert!(matches!(es, Es::AirbornePosition { .. }));
        }
        assert_eq!(source.relayed(), matches!(cf, 2 | 5 | 6));
    }
    // CF 6 with the IMF bit (ME bit 8) set: not an ICAO address.
    let mut me2 = me;
    put(&mut me2, 8, 1, 1);
    let mut x = es_frame(18, 6, 0xA00001, me2);
    let Body::Extended { source, .. } = decode(&mut x, 0).unwrap().body else { panic!() };
    assert_eq!(source, EsSource::AdsR { icao: false });
    // CF 3 (coarse TIS-B) is recognised but its position is not decoded.
    let mut x = es_frame(18, 3, 0xA00001, me);
    let Body::Extended { source, es, .. } = decode(&mut x, 0).unwrap().body else { panic!() };
    assert_eq!(source, EsSource::TisB { icao: true, fine: false });
    assert!(matches!(es, Es::Other { .. }));
}

#[test]
fn short_formats() {
    // DF4 with a Q-bit altitude, DF5 with a squawk, DF0 on the ground.
    let ac = {
        let f12 = alt::ac12_field_25ft(2500).unwrap();
        ((f12 & 0x0FC0) << 1) | (f12 & 0x003F)
    };
    let mut x = ap_frame(&df4_head(4, ac, None), 0xABCDEF);
    let m = decode(&mut x, 0).unwrap();
    assert_eq!(m.address, 0xABCDEF);
    assert_eq!(m.check, AddressCheck::AddressParity);
    assert_eq!(m.body, Body::AltitudeReply { fs: 0, altitude: Altitude::Feet(2500) });

    let id = alt::squawk_field(0o1200);
    let mut head = std::vec![5 << 3 | 1, 0, ((id >> 8) & 0x1F) as u8, (id & 0xFF) as u8];
    let mut x = ap_frame(&head, 0x123456);
    assert_eq!(decode(&mut x, 0).unwrap().body, Body::IdentityReply { fs: 1, squawk: 0o1200 });

    head[0] = 1 << 2; // DF0, VS = 1 (on the ground)
    let mut x = ap_frame(&head, 0x123456);
    let m = decode(&mut x, 0).unwrap();
    assert_eq!(m.df, 0);
    assert!(matches!(m.body, Body::ShortAcas { airborne: false, .. }));
}

#[test]
fn all_call() {
    // A DF11 from our recording (Philadelphia, 2026-11-15): address
    // A9F4C6, capability 5, acquisition squitter (no interrogator).
    let mut x = f("5DA9F4C6CF7A6D");
    let m = decode(&mut x, 0).unwrap();
    assert_eq!(m.address, 0xA9F4C6);
    assert_eq!(m.check, AddressCheck::Verified(Fix::Clean));
    assert_eq!(m.body, Body::AllCall { ca: 5 });
    // The same reply to interrogator II=3: CRC overlaid with 3.
    let mut b = [0u8; 7];
    b.copy_from_slice(x.as_bytes());
    b[6] ^= 3;
    let mut y = Frame::new(&b).unwrap();
    assert_eq!(decode(&mut y, 0).unwrap().check, AddressCheck::Interrogator(3));
    // A corrupt one.
    b[2] ^= 0x10;
    let mut z = Frame::new(&b).unwrap();
    assert_eq!(decode(&mut z, 0), Err(Reject::BadAllCall));
}

#[test]
fn length_must_match_df() {
    assert!(Frame::from_hex("8D4840D6202CC3").is_none()); // DF17 in 56 bits
    assert!(Frame::from_hex("5DA9F4C6CF7A6D00000000000000").is_none()); // DF11 in 112
    assert!(Frame::from_hex("5DA9F4C6CF7A6").is_none());
    assert!(Frame::from_hex("5DA9F4C6CF7AGG").is_none());
}

// ---------------------------------------------------------------- Comm-B

fn mb_of(hex: &str) -> [u8; 7] {
    f(hex).payload()
}

#[test]
fn bds_published_examples() {
    let none = Hint::default();
    // pyModeS tests/test_commb (bds20): KLM1017.
    match commb::infer(&mb_of("A000083E202CC371C31DE0AA1CCF"), &none) {
        Bds::Identification(cs) => assert_eq!(cs.as_str(), "KLM1017"),
        b => panic!("{b:?}"),
    }
    // bds40: MCP 3008 ft, FMS 3008 ft, 1020.0 hPa.
    match commb::infer(&mb_of("A000029C85E42F313000007047D3"), &none) {
        Bds::VerticalIntent(b) => {
            assert_eq!(b.mcp_alt_ft, Some(3008));
            assert_eq!(b.fms_alt_ft, Some(3008));
            assert!(close(f64::from(b.baro_hpa.unwrap()), 1020.0, 0.05));
        }
        b => panic!("{b:?}"),
    }
    // bds50: roll 2.1, track 114.258, GS 438, track rate 0.125, TAS 424.
    match commb::infer(&mb_of("A000139381951536E024D4CCF6B5"), &none) {
        Bds::TrackTurn(b) => {
            assert!(close(f64::from(b.roll_deg.unwrap()), 2.1, 0.05));
            assert!(close(f64::from(b.track_deg.unwrap()), 114.258, 0.01));
            assert_eq!(b.ground_speed_kt, Some(438));
            assert!(close(f64::from(b.track_rate_dps.unwrap()), 0.125, 1e-3));
            assert_eq!(b.tas_kt, Some(424));
        }
        b => panic!("{b:?}"),
    }
    // bds60: heading 42.715, IAS 252, Mach 0.42, -1920 / -1920 ft/min.
    match commb::infer(&mb_of("A00004128F39F91A7E27C46ADC21"), &none) {
        Bds::HeadingSpeed(b) => {
            assert!(close(f64::from(b.heading_deg.unwrap()), 42.715, 0.01));
            assert_eq!(b.ias_kt, Some(252));
            assert!(close(f64::from(b.mach.unwrap()), 0.42, 1e-3));
            assert_eq!(b.baro_rate_fpm, Some(-1920));
            assert_eq!(b.inertial_rate_fpm, Some(-1920));
        }
        b => panic!("{b:?}"),
    }
}

#[test]
fn bds_hint_breaks_ties_and_rejects() {
    // A 5,0 whose ground speed disagrees with ADS-B by 100 kt is not a 5,0.
    let m = mb_of("A000139381951536E024D4CCF6B5");
    let hint = Hint { ground_speed_kt: Some(338.0), ..Hint::default() };
    assert!(!matches!(commb::infer(&m, &hint), Bds::TrackTurn(_)));
    let hint = Hint { ground_speed_kt: Some(440.0), track_deg: Some(115.0), ..Hint::default() };
    assert!(matches!(commb::infer(&m, &hint), Bds::TrackTurn(_)));
    assert_eq!(commb::infer(&[0; 7], &Hint::default()), Bds::Unknown);
}

#[test]
fn bds10_layout() {
    let mut m = [0u8; 7];
    m[0] = 0x10;
    m[1] = 0x01; // bit 16 set (not reserved)
    assert_eq!(commb::infer(&m, &Hint::default()), Bds::DataLinkCapability);
    m[1] = 0x40; // bit 10: reserved
    assert_ne!(commb::infer(&m, &Hint::default()), Bds::DataLinkCapability);
}

// ---------------------------------------------------------------- inputs

/// The first frames of our own Beast recording (ADSBee 1090, firmware
/// 0.9.0, `AT+PROTOCOL_OUT=CONSOLE,BEAST`, Philadelphia 2026-11-15),
/// starting with the console's `OK\r\n` reply.
const BEAST_REC: &str = "4f4b0d0a1a3300c5a3f9f838158dac4bb899114c2fd0048fd7d2921a3200c5a4037c8f1302e197b0e62a851a3300c5a403b1a5128da9f4c6ea0fb8a8015f88753dd41a3300c5a40a9796128da9f4c69910dd1d7854878c75c41a3200c5a41785521b2000093e5686301a3200c5a4318f2b1202c1893e1522531a3300c5a4387b62128dac4bb8ea485864615c08296a36";

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

#[test]
fn beast_recording_parses() {
    let bytes = unhex(BEAST_REC);
    let mut r = BeastReader::new();
    let mut got = Vec::new();
    r.feed(&bytes, |x| got.push(x));
    assert_eq!(got.len(), 7);
    assert_eq!(r.stats.skipped, 4); // "OK\r\n"
    assert_eq!(r.stats.broken, 0);
    assert_eq!(std::format!("{:?}", got[0].frame), "8DAC4BB899114C2FD0048FD7D292");
    assert_eq!(got[0].ticks_12mhz, Some(0x00C5A3F9F838));
    assert_eq!(got[0].signal_dbm, Some(-69)); // level 0x15 (21)
    // Every frame checks out: DF17 syndrome 0, DF0/4/11 give known addresses.
    for x in &got {
        let mut fr = x.frame;
        assert!(decode(&mut fr, 0).is_ok(), "{:?}", x.frame);
    }
    // The same bytes in 1-byte chunks give the same frames.
    let mut r2 = BeastReader::new();
    let mut got2 = Vec::new();
    for b in &bytes {
        r2.feed(core::slice::from_ref(b), |x| got2.push(x));
    }
    assert_eq!(got, got2);
}

#[test]
fn beast_escapes_and_resync() {
    // A frame whose timestamp, level and data contain 0x1A.
    let fr = es_frame(17, 5, 0x1A1A1A, airborne_me(11, 1000, 39.9, -75.1, false));
    let mut buf = [0u8; 44];
    let n = input::beast_encode(&fr, 0x1A_0000_001A, 0x1A, &mut buf);
    let mut stream = Vec::new();
    stream.extend_from_slice(b"log text\r\n");
    stream.extend_from_slice(&buf[..n]);
    // A frame cut short by the next frame start.
    stream.extend_from_slice(&buf[..10]);
    stream.extend_from_slice(&buf[..n]);
    let mut r = BeastReader::new();
    let mut got = Vec::new();
    r.feed(&stream, |x| got.push(x));
    assert_eq!(got.len(), 2);
    assert!(got.iter().all(|g| g.frame == fr && g.ticks_12mhz == Some(0x1A_0000_001A)));
    assert_eq!(r.stats.broken, 1);
}

#[test]
fn beast_signal_inverts_the_adsbee_table() {
    // ADSBee beast_tables.hh: -100 dBm -> 1, -40 dBm -> 255 (its encoder
    // maps dBm linearly onto -45..0 dBFS and writes 255 * 10^(dBFS/20)).
    for dbm in -95..=-40i32 {
        let dbfs = (f64::from(dbm) + 100.0) * 0.75 - 45.0;
        let level = libm::round(255.0 * libm::pow(10.0, dbfs / 20.0)) as u8;
        let back = input::beast_signal_dbm(level).unwrap();
        assert!((i32::from(back) - dbm).abs() <= 2, "{dbm} -> {level} -> {back}");
    }
    assert_eq!(input::beast_signal_dbm(0), None);
}

#[test]
fn raw_lines() {
    let r = input::parse_raw_line(b"#MDS*8D4840D6202CC371C32CE0576098;(0,-72,9,0000000312345678)\r\n").unwrap();
    assert_eq!(r.frame, f("8D4840D6202CC371C32CE0576098"));
    assert_eq!(r.signal_dbm, Some(-72));
    assert_eq!(r.ticks_12mhz, Some(0x312345678 / 4));
    assert!(input::parse_raw_line(b"#UAT*-0011;(1,2,0000)").is_none());
    assert!(input::parse_raw_line(b"#MDS*8D4840D6202CC371C32CE0576098;(0,-72)").is_none());
    assert!(input::parse_raw_line(b"hello").is_none());
}
