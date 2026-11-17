//! UAT tests. No published raw UAT frames (with parity) were found in the
//! sources we have, so frames are built here: payload bits are packed by
//! hand from DO-282B / dump978's layout, and parity comes from a test-only
//! Reed-Solomon encoder written from the generator polynomial (independent
//! of the decoder's syndrome / Berlekamp-Massey path).

extern crate std;

use std::vec::Vec;

use crate::payload::{self, AirGround, AltKind, TrackKind};
use crate::rs::{self, Code, FCR};
use crate::{BASIC_BYTES, Error, FrameKind, LONG_BYTES, correct_adsb_frame, decode};

// ---------------------------------------------------------------- encoder

/// Generator polynomial, descending powers, monic.
fn generator(nroots: usize) -> Vec<u8> {
    let mut g = std::vec![1u8];
    for i in 0..nroots {
        let r = rs::pow_alpha(FCR + i);
        let mut q = std::vec![0u8; g.len() + 1];
        for k in 0..q.len() {
            let hi = if k < g.len() { g[k] } else { 0 };
            let lo = if k >= 1 { rs::mul(r, g[k - 1]) } else { 0 };
            q[k] = hi ^ lo;
        }
        g = q;
    }
    g
}

/// Systematic encoding: data followed by the remainder of data(x) x^nroots
/// divided by g(x).
fn encode(code: Code, data: &[u8]) -> Vec<u8> {
    assert_eq!(data.len(), code.k());
    let g = generator(code.nroots);
    let mut w: Vec<u8> = data.to_vec();
    w.resize(code.n, 0);
    for i in 0..code.k() {
        let c = w[i];
        if c != 0 {
            for j in 1..=code.nroots {
                w[i + j] ^= rs::mul(g[j], c);
            }
        }
    }
    w[..code.k()].copy_from_slice(data);
    w
}

/// Small deterministic PRNG (xorshift32).
struct Rng(u32);
impl Rng {
    fn next(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }
    fn byte(&mut self) -> u8 {
        self.next() as u8
    }
    /// `n` distinct positions below `len`.
    fn positions(&mut self, n: usize, len: usize) -> Vec<usize> {
        let mut v = Vec::new();
        while v.len() < n {
            let p = (self.next() as usize) % len;
            if !v.contains(&p) {
                v.push(p);
            }
        }
        v
    }
    fn nonzero(&mut self) -> u8 {
        loop {
            let b = self.byte();
            if b != 0 {
                return b;
            }
        }
    }
}

fn random_data(rng: &mut Rng, k: usize, mdb_type: u8) -> Vec<u8> {
    let mut d: Vec<u8> = (0..k).map(|_| rng.byte()).collect();
    d[0] = (mdb_type << 3) | (d[0] & 7);
    d
}

// ------------------------------------------------------------ bit packing

/// Write `value` into `n` bits starting at bit `start` (bit 0 = MSB of
/// byte 0), the way DO-282B numbers payload bits.
fn put(buf: &mut [u8], start: usize, n: usize, value: u32) {
    for i in 0..n {
        let bit = (value >> (n - 1 - i)) & 1;
        let pos = start + i;
        let mask = 0x80u8 >> (pos % 8);
        if bit != 0 {
            buf[pos / 8] |= mask;
        } else {
            buf[pos / 8] &= !mask;
        }
    }
}

#[derive(Clone, Copy)]
struct Sv {
    aq: u8,
    address: u32,
    lat: f64,
    lon: f64,
    geo_alt: bool,
    alt_ft: i32,
    nic: u8,
    ag: u8,
    ns: i32,
    ew: i32,
    vvel_baro: bool,
    vrate: i32,
}

fn alt_code(ft: i32) -> u32 {
    ((ft + 1000) / 25 + 1) as u32
}

fn signed11(v: i32) -> u32 {
    let m = (v.unsigned_abs() + 1) & 0x3ff;
    m | if v < 0 { 0x400 } else { 0 }
}

/// Header and state vector into `buf` (bytes 0..17).
fn put_sv(buf: &mut [u8], mdb_type: u8, s: &Sv) {
    put(buf, 0, 5, u32::from(mdb_type));
    put(buf, 5, 3, u32::from(s.aq));
    put(buf, 8, 24, s.address);
    let lat = if s.lat < 0.0 { s.lat + 180.0 } else { s.lat };
    let lon = if s.lon < 0.0 { s.lon + 360.0 } else { s.lon };
    put(buf, 32, 23, libm::round(lat * 16_777_216.0 / 360.0) as u32);
    put(buf, 55, 24, libm::round(lon * 16_777_216.0 / 360.0) as u32);
    put(buf, 79, 1, u32::from(s.geo_alt));
    put(buf, 80, 12, alt_code(s.alt_ft));
    put(buf, 92, 4, u32::from(s.nic));
    put(buf, 96, 2, u32::from(s.ag));
    put(buf, 99, 11, signed11(s.ns));
    put(buf, 110, 11, signed11(s.ew));
    // vertical velocity: source bit, sign bit, 9-bit magnitude in 64 fpm
    let mag = ((s.vrate.unsigned_abs() / 64) + 1) & 0x1ff;
    let vv = (u32::from(s.vvel_baro) << 10) | (u32::from(s.vrate < 0) << 9) | mag;
    put(buf, 121, 11, vv);
}

fn base40(c: u8) -> u32 {
    match c {
        b'0'..=b'9' => u32::from(c - b'0'),
        b'A'..=b'Z' => u32::from(c - b'A') + 10,
        _ => 36,
    }
}

/// Mode status into `buf` (bytes 17..28).
fn put_ms(buf: &mut [u8], cat: u8, call: &str, csid: bool, emergency: u8, version: u8) {
    let mut c = [b' '; 8];
    c[..call.len()].copy_from_slice(call.as_bytes());
    let v0 = u32::from(cat) * 1600 + base40(c[0]) * 40 + base40(c[1]);
    let v1 = base40(c[2]) * 1600 + base40(c[3]) * 40 + base40(c[4]);
    let v2 = base40(c[5]) * 1600 + base40(c[6]) * 40 + base40(c[7]);
    put(buf, 136, 16, v0);
    put(buf, 152, 16, v1);
    put(buf, 168, 16, v2);
    put(buf, 184, 3, u32::from(emergency));
    put(buf, 187, 3, u32::from(version));
    put(buf, 214, 1, u32::from(csid));
}

fn philly() -> Sv {
    Sv {
        aq: 0,
        address: 0xA1B2C3,
        lat: 39.95,
        lon: -75.16,
        geo_alt: false,
        alt_ft: 3500,
        nic: 8,
        ag: 0,
        ns: 100,
        ew: 100,
        vvel_baro: true,
        vrate: -640,
    }
}

fn basic_frame(s: &Sv) -> Vec<u8> {
    let mut d = [0u8; 18];
    put_sv(&mut d, 0, s);
    encode(rs::BASIC, &d)
}

fn long_frame(mdb_type: u8, s: &Sv, call: &str) -> Vec<u8> {
    let mut d = [0u8; 34];
    put_sv(&mut d, mdb_type, s);
    if mdb_type == 1 || mdb_type == 3 {
        put_ms(&mut d, 7, call, true, 0, 2);
    }
    if mdb_type == 1 || mdb_type == 2 {
        // secondary (geometric, since the primary is baro) altitude
        put(&mut d, 232, 12, alt_code(s.alt_ft + 150));
    }
    let mut w = encode(rs::LONG, &d);
    w.resize(LONG_BYTES, 0);
    w
}

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

// ----------------------------------------------------------------- tests

#[test]
fn gf_tables_are_a_field() {
    // alpha has order 255 under 0x187 (so 0x187 is primitive), and every
    // non-zero element has an inverse.
    let mut seen = [false; 256];
    for e in 0..255 {
        let v = rs::pow_alpha(e);
        assert!(!seen[usize::from(v)], "alpha^{e} repeats");
        seen[usize::from(v)] = true;
    }
    for a in 1..=255u8 {
        assert!((1..=255u8).any(|b| rs::mul(a, b) == 1));
    }
    // x^8 = x^7 + x^2 + x + 1
    assert_eq!(rs::pow_alpha(8), 0x87);
}

#[test]
fn encoder_output_has_zero_syndromes() {
    let mut rng = Rng(1);
    for code in [rs::BASIC, rs::LONG] {
        for _ in 0..20 {
            let d: Vec<u8> = (0..code.k()).map(|_| rng.byte()).collect();
            let mut w = encode(code, &d);
            assert_eq!(code.correct(&mut w), Some(0));
            assert_eq!(&w[..code.k()], &d[..]);
        }
    }
}

#[test]
fn corrects_up_to_the_limit_at_every_count() {
    let mut rng = Rng(7);
    for (code, mdb) in [(rs::BASIC, 0u8), (rs::LONG, 1u8)] {
        for errors in 1..=code.t() {
            for _ in 0..50 {
                let d = random_data(&mut rng, code.k(), mdb);
                let good = encode(code, &d);
                let mut bad = good.clone();
                for p in rng.positions(errors, code.n) {
                    bad[p] ^= rng.nonzero();
                }
                assert_eq!(code.correct(&mut bad), Some(errors), "{code:?} {errors}");
                assert_eq!(bad, good);
            }
        }
    }
}

#[test]
fn errors_in_parity_bytes_only() {
    let mut rng = Rng(9);
    let d = random_data(&mut rng, 18, 0);
    let good = encode(rs::BASIC, &d);
    let mut bad = good.clone();
    for b in &mut bad[18..24] {
        *b ^= 0xff;
    }
    assert_eq!(rs::BASIC.correct(&mut bad), Some(6));
    assert_eq!(bad, good);
}

#[test]
fn rejects_beyond_the_limit_and_leaves_frame_alone() {
    let mut rng = Rng(11);
    for code in [rs::BASIC, rs::LONG] {
        let mut rejected = 0;
        let trials = 400;
        for _ in 0..trials {
            let d: Vec<u8> = (0..code.k()).map(|_| rng.byte()).collect();
            let good = encode(code, &d);
            let mut bad = good.clone();
            for p in rng.positions(code.t() + 1, code.n) {
                bad[p] ^= rng.nonzero();
            }
            let before = bad.clone();
            match code.correct(&mut bad) {
                None => {
                    rejected += 1;
                    assert_eq!(bad, before, "a rejected frame must be untouched");
                }
                // A miscorrection to a different codeword is possible in
                // principle (that is what "beyond the limit" means), but it
                // can never give back the original.
                Some(_) => assert_ne!(bad, good),
            }
        }
        // Shortened codes reject almost every such pattern.
        assert!(
            rejected * 100 >= trials * 99,
            "{code:?}: only {rejected}/{trials} rejected"
        );
    }
}

#[test]
fn rejects_random_noise() {
    let mut rng = Rng(13);
    let mut accepted = 0;
    for _ in 0..2000 {
        let mut f: Vec<u8> = (0..LONG_BYTES).map(|_| rng.byte()).collect();
        if correct_adsb_frame(&mut f).is_ok() {
            accepted += 1;
        }
    }
    assert!(accepted <= 2, "noise accepted {accepted} times");
}

#[test]
fn frame_kind_detection_follows_payload_type() {
    let s = philly();
    // A basic frame inside a 48-byte capture (the rest is noise).
    let mut cap = basic_frame(&s);
    let mut rng = Rng(3);
    cap.extend((0..18).map(|_| rng.byte()));
    assert_eq!(correct_adsb_frame(&mut cap), Ok((FrameKind::Basic, 0)));

    let mut l = long_frame(1, &s, "N123AB");
    assert_eq!(correct_adsb_frame(&mut l), Ok((FrameKind::Long, 0)));

    // A valid RS(48,34) word with payload type 0 is not a long frame, and
    // its first 30 bytes are not a basic codeword: refused.
    let mut d = [0u8; 34];
    put_sv(&mut d, 0, &s);
    let mut w = encode(rs::LONG, &d);
    assert_eq!(correct_adsb_frame(&mut w), Err(Error::Uncorrectable));

    // A valid RS(30,18) word with payload type 1 is not a basic frame.
    let mut d = [0u8; 18];
    put_sv(&mut d, 1, &s);
    let mut w = encode(rs::BASIC, &d);
    assert_eq!(correct_adsb_frame(&mut w), Err(Error::Uncorrectable));

    let mut short = [0u8; 29];
    assert_eq!(correct_adsb_frame(&mut short), Err(Error::TooShort));
}

#[test]
fn basic_frame_only_30_bytes_is_enough() {
    let mut f = basic_frame(&philly());
    assert_eq!(f.len(), BASIC_BYTES);
    f[5] ^= 0x40;
    let d = decode(&mut f).unwrap();
    assert_eq!((d.kind, d.corrected), (FrameKind::Basic, 1));
}

#[test]
fn decodes_state_vector() {
    let mut f = basic_frame(&philly());
    // three bad bytes on the way
    f[2] ^= 0x11;
    f[9] ^= 0x80;
    f[27] ^= 0x01;
    let d = decode(&mut f).unwrap();
    assert_eq!(d.corrected, 3);
    let p = d.payload;
    assert_eq!(
        (p.mdb_type, p.address_qualifier, p.address),
        (0, 0, 0xA1B2C3)
    );
    assert!(p.ms.is_none());
    let sv = p.sv.unwrap();
    let (lat, lon) = sv.position.unwrap();
    assert!(
        close(lat, 39.95, 2.2e-5) && close(lon, -75.16, 2.2e-5),
        "{lat} {lon}"
    );
    assert_eq!(sv.altitude, Some((3500, AltKind::Baro)));
    assert_eq!(sv.nic, 8);
    assert_eq!(sv.air_ground, AirGround::Subsonic);
    assert_eq!((sv.ns_kt, sv.ew_kt), (Some(100), Some(100)));
    let (trk, kind) = sv.track.unwrap();
    assert!(close(f64::from(trk), 45.0, 0.01) && kind == TrackKind::Track);
    assert_eq!(sv.speed_kt, Some(141));
    assert_eq!(sv.vrate, Some((-640, AltKind::Baro)));
}

#[test]
fn track_quadrants_and_negative_coordinates() {
    for (ns, ew, want) in [
        (0, 50, 90.0),
        (-50, 0, 180.0),
        (0, -50, 270.0),
        (50, 0, 0.0),
        (-30, -30, 225.0),
    ] {
        let mut s = philly();
        s.ns = ns;
        s.ew = ew;
        s.lat = -33.86;
        s.lon = 151.21;
        let mut f = basic_frame(&s);
        let sv = decode(&mut f).unwrap().payload.sv.unwrap();
        let (t, _) = sv.track.unwrap();
        assert!(close(f64::from(t), want, 0.01), "{ns},{ew}: {t}");
        let (lat, lon) = sv.position.unwrap();
        assert!(close(lat, -33.86, 2.2e-5) && close(lon, 151.21, 2.2e-5));
    }
}

#[test]
fn supersonic_scales_velocity_by_four() {
    let mut s = philly();
    s.ag = 1;
    s.ns = 300; // encoded as 300, decoded x4
    s.ew = 0;
    let mut f = basic_frame(&s);
    let sv = decode(&mut f).unwrap().payload.sv.unwrap();
    assert_eq!(sv.ns_kt, Some(1200));
    assert_eq!(sv.air_ground, AirGround::Supersonic);
}

#[test]
fn ground_state_has_speed_and_heading() {
    let mut d = [0u8; 18];
    let mut s = philly();
    s.ag = 2;
    put_sv(&mut d, 0, &s);
    // ground speed field (11 bits at 99): 0x000 | (12+1)
    put(&mut d, 99, 11, 13);
    // track field: type 3 (true heading) in bits 9-10, angle 128/512 = 90 deg
    put(&mut d, 110, 11, (3 << 9) | 128);
    let mut f = encode(rs::BASIC, &d);
    let dec = decode(&mut f).unwrap();
    let sv = dec.payload.sv.unwrap();
    assert_eq!(sv.air_ground, AirGround::Ground);
    assert_eq!(sv.speed_kt, Some(12));
    assert_eq!(sv.track, Some((90.0, TrackKind::TrueHeading)));
    assert_eq!(sv.vrate, None);
    let a = dec.aircraft();
    assert!(!a.airborne());
    assert_eq!(a.speed_kt(), Some(12));
}

#[test]
fn unknown_fields_stay_unknown() {
    let mut d = [0u8; 18];
    put(&mut d, 8, 24, 0x123456);
    let mut f = encode(rs::BASIC, &d);
    let dec = decode(&mut f).unwrap();
    let sv = dec.payload.sv.unwrap();
    assert_eq!(sv.position, None);
    assert_eq!(sv.altitude, None);
    assert_eq!(
        (sv.ns_kt, sv.ew_kt, sv.speed_kt, sv.track, sv.vrate),
        (None, None, None, None, None)
    );
    let a = dec.aircraft();
    assert_eq!(a.position(), None);
    assert_eq!(a.baro_altitude_ft(), None);
    assert_eq!(a.speed_kt(), None);
    assert_eq!(a.category, -1);
    assert!(a.heard());
}

#[test]
fn long_frame_mode_status_and_aux_altitude() {
    let s = philly();
    let mut f = long_frame(1, &s, "N123AB");
    for p in [0usize, 10, 20, 30, 40, 45, 47] {
        f[p] ^= 0x5a;
    }
    let d = decode(&mut f).unwrap();
    assert_eq!((d.kind, d.corrected), (FrameKind::Long, 7));
    let ms = d.payload.ms.unwrap();
    assert_eq!(ms.callsign.as_str(), "N123AB");
    assert_eq!(ms.squawk, None);
    assert_eq!(ms.emitter_category, 7);
    assert_eq!(ms.uat_version, 2);
    assert_eq!(d.payload.aux_altitude, Some((3650, AltKind::Geo)));

    let a = d.aircraft();
    assert_eq!(a.source, adsb::Source::Uat);
    assert_eq!(a.icao24(), 0xA1B2C3);
    assert_eq!(a.address_qualifier(), 0);
    assert_eq!(a.callsign.as_str(), "N123AB");
    assert_eq!(a.kind(), adsb::Kind::Rotorcraft);
    assert_eq!(a.baro_altitude_ft(), Some(3500));
    assert_eq!(a.gnss_altitude_ft(), Some(3650));
    assert_eq!(a.vertical_rate_fpm(), Some(-640));
    assert_eq!(a.speed_kt(), Some(141));
    assert!(a.airborne() && a.position_updated() && a.heard());
    assert_eq!(a.signal_quality, 7);
    assert_eq!(a.version, 2);
}

#[test]
fn eight_bad_bytes_in_a_long_frame_are_refused() {
    let s = philly();
    let mut f = long_frame(1, &s, "N123AB");
    for p in [0usize, 6, 12, 18, 24, 30, 36, 42] {
        f[p] ^= 0xa5;
    }
    let before = f.clone();
    assert_eq!(decode(&mut f), Err(Error::Uncorrectable));
    assert_eq!(f, before);
}

#[test]
fn seven_bad_bytes_in_a_basic_frame_are_refused() {
    let mut f = basic_frame(&philly());
    for p in [0usize, 4, 8, 12, 16, 20, 24] {
        f[p] ^= 0x3c;
    }
    assert_eq!(decode(&mut f), Err(Error::Uncorrectable));
}

#[test]
fn squawk_in_call_sign_field_and_emergency() {
    let s = philly();
    let mut d = [0u8; 34];
    put_sv(&mut d, 3, &s);
    put_ms(&mut d, 1, "7700", false, 1, 2);
    let mut f = encode(rs::LONG, &d);
    let dec = decode(&mut f).unwrap();
    let ms = dec.payload.ms.unwrap();
    assert_eq!(ms.squawk, Some(0o7700));
    assert!(ms.callsign.is_empty());
    assert_eq!(ms.emergency, 1);
    // type 3 has no auxiliary state vector
    assert_eq!(dec.payload.aux_altitude, None);
    let a = dec.aircraft();
    assert_eq!(a.emergency(), Some("emergency"));
    assert_eq!(a.uat_emergency, 1);
    assert_eq!(a.kind(), adsb::Kind::Light);
}

#[test]
fn geometric_primary_altitude() {
    let mut s = philly();
    s.geo_alt = true;
    s.alt_ft = -1000; // lowest code
    s.vvel_baro = false;
    s.vrate = 128;
    let mut f = basic_frame(&s);
    let a = decode(&mut f).unwrap().aircraft();
    assert_eq!(a.gnss_altitude_ft(), Some(-1000));
    assert_eq!(a.baro_altitude_ft(), None);
    assert_eq!(a.vertical_rate_fpm(), Some(128));
    assert!(a.flags & (1 << adsb::flag::GNSS_VRATE_VALID) != 0);
}

#[test]
fn tisb_target_keeps_qualifier_and_site() {
    let mut s = philly();
    s.aq = 2;
    let mut d = [0u8; 18];
    put_sv(&mut d, 0, &s);
    put(&mut d, 132, 4, 9);
    let mut f = encode(rs::BASIC, &d);
    let dec = decode(&mut f).unwrap();
    assert_eq!(dec.payload.sv.unwrap().tisb_site_id, 9);
    let a = dec.aircraft();
    assert_eq!(a.address_qualifier(), 2);
    assert_eq!(a.icao24(), 0xA1B2C3);
}

/// Bytes assembled directly from dump978's expressions (not through the
/// bit packer above), to pin the field positions independently.
#[test]
fn hand_assembled_frame_matches_dump978_layout() {
    let mut d = [0u8; 18];
    d[0] = 1; // type 0, aq 1
    d[1..4].copy_from_slice(&[0xAB, 0xCD, 0xEF]);
    // raw_lat = 0x200000 (45 degrees): f4<<15 | f5<<7 | f6>>1
    d[4] = 0x40;
    d[5] = 0x00;
    // raw_lon = 0xC00000 (270 -> -90 degrees): (f6&1)<<23 | f7<<15 | f8<<7 | f9>>1
    d[6] = 0x01;
    d[7] = 0x80;
    d[8] = 0x00;
    d[9] = 0x00; // baro
    // raw_alt = 0x0A1 = 161 -> 160*25-1000 = 3000 ft; nic 5
    d[10] = 0x0A;
    d[11] = 0x15;
    // airborne subsonic, ns raw = 0x400|11 (south 10 kt), ew raw = 21 (east 20)
    let ns: u32 = 0x400 | 11;
    let ew: u32 = 21;
    d[12] = ((ns >> 6) & 0x1f) as u8;
    d[13] = (((ns & 0x3f) << 2) | ((ew >> 9) & 3)) as u8;
    d[14] = ((ew >> 1) & 0xff) as u8;
    let vv: u32 = 0x400 | 3; // baro, up, (3-1)*64 = 128
    d[15] = (((ew & 1) << 7) | ((vv >> 4) & 0x7f)) as u8;
    d[16] = ((vv & 0x0f) << 4) as u8 | 0x08; // utc coupled
    let p = payload::decode(&d).unwrap();
    assert_eq!((p.address_qualifier, p.address), (1, 0xABCDEF));
    let sv = p.sv.unwrap();
    assert_eq!(sv.position, Some((45.0, -90.0)));
    assert_eq!(sv.altitude, Some((3000, AltKind::Baro)));
    assert_eq!(sv.nic, 5);
    assert_eq!((sv.ns_kt, sv.ew_kt), (Some(-10), Some(20)));
    assert_eq!(sv.vrate, Some((128, AltKind::Baro)));
    assert!(sv.utc_coupled);
    // and the bit packer agrees with the hand assembly
    let s = Sv {
        aq: 1,
        address: 0xABCDEF,
        lat: 45.0,
        lon: -90.0,
        geo_alt: false,
        alt_ft: 3000,
        nic: 5,
        ag: 0,
        ns: -10,
        ew: 20,
        vvel_baro: true,
        vrate: 128,
    };
    let mut q = [0u8; 18];
    put_sv(&mut q, 0, &s);
    put(&mut q, 132, 4, 8);
    assert_eq!(q, d);
}

#[test]
fn reserved_payload_types_give_header_only() {
    let mut d = [0u8; 34];
    put_sv(&mut d, 12, &philly());
    let mut f = encode(rs::LONG, &d);
    let dec = decode(&mut f).unwrap();
    assert_eq!(dec.payload.mdb_type, 12);
    assert!(dec.payload.sv.is_none());
    let a = dec.aircraft();
    assert_eq!(a.icao24(), 0xA1B2C3);
    assert_eq!(a.position(), None);
}

#[test]
fn decoded_record_survives_csbee_round_trip() {
    let mut f = long_frame(1, &philly(), "N123AB");
    let a = decode(&mut f).unwrap().aircraft();
    let mut line = [0u8; adsb::MAX_LINE];
    let n = adsb::encode::aircraft(&a, &mut line).unwrap();
    match adsb::parse(&line[..n]).unwrap() {
        adsb::Report::Aircraft(b) => {
            assert_eq!(b.source, adsb::Source::Uat);
            assert_eq!(b.icao24(), a.icao24());
            assert_eq!(b.callsign, a.callsign);
            assert_eq!(b.baro_altitude_ft(), a.baro_altitude_ft());
        }
        r => panic!("{r:?}"),
    }
}
