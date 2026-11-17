//! OGN tests. No recorded OGN packets were available, so packets are built
//! by a test-only encoder ported from the OGN tracker library (`ogn.h`
//! `Encode*`, `Whiten`, `ldpc.cpp` `LDPC_Encode` with the generator
//! table). The generator and the parity-check tables are two different
//! tables from the OGN code, so a packet passing the check after encoding
//! confirms both tables and the byte order.

extern crate std;

use std::vec::Vec;

use crate::ldpc_tables::PARITY_GEN;
use crate::ogn::{
    self, Error, PACKET_BYTES, decode_sr2v6, decode_ur2v8, decode_ur2v12, ldpc_check,
};
use crate::tea::tea_encrypt_key0;
use crate::{AddressType, AircraftType};

fn encode_ur2v12(v: u32) -> u32 {
    if v < 0x1000 {
        v
    } else if v < 0x3000 {
        0x1000 | ((v - 0x1000) >> 1)
    } else if v < 0x7000 {
        0x2000 | ((v - 0x3000) >> 2)
    } else if v < 0xF000 {
        0x3000 | ((v - 0x7000) >> 3)
    } else {
        0x3FFF
    }
}

fn encode_ur2v8(v: u32) -> u32 {
    if v < 0x100 {
        v
    } else if v < 0x300 {
        0x100 | ((v - 0x100) >> 1)
    } else if v < 0x700 {
        0x200 | ((v - 0x300) >> 2)
    } else if v < 0xF00 {
        0x300 | ((v - 0x700) >> 3)
    } else {
        0x3FF
    }
}

fn encode_sr2v6(v: i32) -> u32 {
    let sign = if v < 0 { 0x100 } else { 0 };
    let a = v.unsigned_abs();
    let u = if a < 0x40 {
        a
    } else if a < 0xC0 {
        0x40 | ((a - 0x40) >> 1)
    } else if a < 0x1C0 {
        0x80 | ((a - 0xC0) >> 2)
    } else if a < 0x3C0 {
        0xC0 | ((a - 0x1C0) >> 3)
    } else {
        0xFF
    };
    u | sign
}

#[derive(Clone, Copy)]
struct Pos {
    address: u32,
    addr_type: u8,
    lat_deg: f64,
    lon_deg: f64,
    alt_m: u32,
    std_alt_m: Option<i32>,
    speed_dmps: u32,
    heading_ddeg: u32,
    climb_dmps: i32,
    acft: u8,
    stealth: bool,
    relay: u8,
    emergency: bool,
    other: bool,
    encrypted: bool,
}

fn glider() -> Pos {
    Pos {
        address: 0xDD1234,
        addr_type: 2,
        lat_deg: 40.1,
        lon_deg: -75.3,
        alt_m: 1234,
        std_alt_m: Some(1200),
        speed_dmps: 250,
        heading_ddeg: 900,
        climb_dmps: 15,
        acft: 1,
        stealth: false,
        relay: 0,
        emergency: false,
        other: false,
        encrypted: false,
    }
}

/// Header + data words, whitened, LDPC-encoded: 26 bytes on air.
fn encode(p: &Pos) -> [u8; PACKET_BYTES] {
    let mut header = (p.address & 0xff_ffff)
        | (u32::from(p.addr_type & 3) << 24)
        | (u32::from(p.other) << 26)
        | (u32::from(p.relay & 3) << 28)
        | (u32::from(p.encrypted) << 30)
        | (u32::from(p.emergency) << 31);
    // calcAddrParity
    if (header & 0x0fff_ffff).count_ones() & 1 != 0 {
        header ^= 0x0800_0000;
    }
    let lat = libm::round(p.lat_deg * 600_000.0) as i32;
    let lon = libm::round(p.lon_deg * 600_000.0) as i32;
    let d0 = ((lat >> 3) as u32 & 0xff_ffff) | (17 << 24) | (1 << 30);
    let mut d1 = ((lon >> 4) as u32 & 0xff_ffff) | (5 << 24) | (1 << 31);
    let alt = encode_ur2v12(p.alt_m);
    let d2 = alt | (encode_ur2v8(p.speed_dmps) << 14);
    let heading = ((p.heading_ddeg << 10) + 1800) / 3600;
    let mut d3 = heading
        | (encode_sr2v6(p.climb_dmps) << 10)
        | (u32::from(p.stealth) << 19)
        | (u32::from(p.acft & 15) << 20);
    if let Some(s) = p.std_alt_m {
        // setBaroAltDiff
        let diff = (s - ogn::decode_ur2v12(alt) as i32).clamp(-255, 255);
        if diff as u32 & 0xff00 == 0 {
            d1 |= 1 << 30;
        }
        d3 |= (diff as u32 & 0xff) << 24;
    }
    let mut a = [d0, d1];
    let mut b = [d2, d3];
    tea_encrypt_key0(&mut a, 8);
    tea_encrypt_key0(&mut b, 8);
    let words = [header, a[0], a[1], b[0], b[1]];
    let mut out = [0u8; PACKET_BYTES];
    for (i, w) in words.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&w.to_le_bytes());
    }
    // LDPC_Encode (byte form): parity bit r = parity of data & gen[r].
    for (r, row) in PARITY_GEN.iter().enumerate() {
        let mut ones = 0;
        for (i, &b) in out[..20].iter().enumerate() {
            ones += (b & (row[i / 4] >> (8 * (i % 4))) as u8).count_ones();
        }
        if ones & 1 != 0 {
            out[20 + r / 8] |= 1 << (r % 8);
        }
    }
    out
}

#[test]
fn variable_resolution_codes_round_trip() {
    // Each code keeps the value to within half its step (the decoders add
    // half a step back).
    for v in (0..61_432u32).step_by(97) {
        let d = decode_ur2v12(encode_ur2v12(v));
        assert!(d.abs_diff(v) <= 4, "{v} -> {d}");
    }
    for v in 0..3832u32 {
        let d = decode_ur2v8(encode_ur2v8(v));
        assert!(d.abs_diff(v) <= 4, "{v} -> {d}");
    }
    for v in -952..=952i32 {
        let d = decode_sr2v6(encode_sr2v6(v));
        assert!(d.abs_diff(v) <= 4, "{v} -> {d}");
    }
    // the top of each range (the OGN comments round these to 61432 / 952)
    assert_eq!(decode_ur2v12(0x3FFF), 0xEFFC);
    assert_eq!(decode_ur2v8(0x3FF), 0xEFC);
    assert_eq!(decode_sr2v6(0x1FF), -956);
    assert_eq!(decode_ur2v8(0x100), 0x101);
}

#[test]
fn encoded_packets_pass_all_48_checks() {
    let mut p = glider();
    for i in 0..50u32 {
        p.address = 0x10_0000 + i * 7919;
        p.lat_deg = -60.0 + f64::from(i) * 2.5;
        p.lon_deg = -179.0 + f64::from(i) * 7.1;
        p.alt_m = i * 211;
        let pk = encode(&p);
        assert_eq!(ldpc_check(&pk), 0, "packet {i}");
    }
}

#[test]
fn every_single_bit_error_is_detected() {
    let good = encode(&glider());
    for bit in 0..PACKET_BYTES * 8 {
        let mut bad = good;
        bad[bit / 8] ^= 1 << (bit % 8);
        assert!(ldpc_check(&bad) > 0, "bit {bit}");
        assert!(matches!(ogn::decode(&bad), Err(Error::Parity(_))));
    }
}

#[test]
fn decodes_a_glider() {
    let pk = encode(&glider());
    let d = ogn::decode(&pk).unwrap();
    assert_eq!(d.address_type, AddressType::Flarm);
    assert_eq!(d.aircraft_type, AircraftType::GLIDER);
    assert!(!d.stealth && !d.no_track);
    assert_eq!(d.relay_count, 0);
    let a = d.aircraft;
    assert_eq!(a.source, adsb::Source::Ogn);
    assert_eq!(a.icao24(), 0xDD1234);
    assert_eq!(a.address_qualifier(), 2);
    assert_eq!(a.kind(), adsb::Kind::Glider);
    let (lat, lon) = a.position().unwrap();
    // 1/600000 deg units, latitude kept to 8 of them, longitude to 16
    assert!((lat - 40.1).abs() < 8.0 / 600_000.0, "{lat}");
    assert!((lon + 75.3).abs() < 16.0 / 600_000.0, "{lon}");
    assert_eq!(a.gnss_altitude_ft(), Some(4049)); // 1234 m
    assert_eq!(a.baro_altitude_ft(), Some(3937)); // 1200 m
    assert_eq!(a.speed_kt(), Some(49)); // 25.0 m/s
    assert_eq!(a.direction_deg(), Some(90.0));
    assert_eq!(a.vertical_rate_fpm(), Some(295)); // 1.5 m/s
    assert!(a.airborne() && a.heard() && a.position_updated());
    assert_eq!(a.uat_emergency, 0);
}

#[test]
fn packet_fields_and_address_parity() {
    let pk = ogn::open(&encode(&glider())).unwrap();
    assert!(pk.good_address_parity());
    assert_eq!(pk.time_s(), 17);
    assert_eq!(pk.fix_quality(), 1);
    assert_eq!(pk.heading_ddeg(), 900);
    assert_eq!(pk.climb_dmps(), 15);
    assert_eq!(pk.std_altitude_m(), 1200);
}

#[test]
fn negative_baro_difference_and_no_baro() {
    let mut p = glider();
    p.std_alt_m = Some(1100); // 134 m below GNSS
    let a = ogn::decode(&encode(&p)).unwrap().aircraft;
    assert_eq!(a.baro_altitude_ft(), Some(3609));
    p.std_alt_m = None;
    let a = ogn::decode(&encode(&p)).unwrap().aircraft;
    assert_eq!(a.baro_altitude_ft(), None);
    assert_eq!(a.gnss_altitude_ft(), Some(4049));
}

#[test]
fn southern_and_eastern_hemispheres() {
    let mut p = glider();
    p.lat_deg = -33.9;
    p.lon_deg = 151.2;
    let (lat, lon) = ogn::decode(&encode(&p))
        .unwrap()
        .aircraft
        .position()
        .unwrap();
    assert!(
        (lat + 33.9).abs() < 1e-5 && (lon - 151.2).abs() < 3e-5,
        "{lat} {lon}"
    );
}

#[test]
fn slow_glider_is_on_the_ground_and_static_never_flies() {
    let mut p = glider();
    p.speed_dmps = 10; // 1 m/s, below the 2 m/s glider threshold
    let a = ogn::decode(&encode(&p)).unwrap().aircraft;
    assert!(!a.airborne());
    p.acft = 15;
    p.speed_dmps = 400;
    let d = ogn::decode(&encode(&p)).unwrap();
    assert!(!d.aircraft.airborne());
    assert_eq!(d.aircraft.kind(), adsb::Kind::Obstacle);
}

#[test]
fn refuses_other_and_encrypted_and_short() {
    let mut p = glider();
    p.other = true;
    assert_eq!(ogn::decode(&encode(&p)), Err(Error::NotPosition));
    let mut p = glider();
    p.encrypted = true;
    assert_eq!(ogn::decode(&encode(&p)), Err(Error::Encrypted));
    assert_eq!(ogn::decode(&[0u8; 25]), Err(Error::TooShort));
}

#[test]
fn relayed_stealth_emergency_paraglider() {
    let mut p = glider();
    p.relay = 2;
    p.stealth = true;
    p.emergency = true;
    p.acft = 7;
    p.addr_type = 3;
    p.climb_dmps = -302;
    let d = ogn::decode(&encode(&p)).unwrap();
    assert_eq!(d.relay_count, 2);
    assert!(d.stealth);
    assert_eq!(d.address_type, AddressType::Ogn);
    assert_eq!(d.aircraft.kind(), adsb::Kind::Ultralight);
    assert_eq!(d.aircraft.uat_emergency, 1);
    assert_eq!(d.aircraft.vertical_rate_fpm(), Some(-5945)); // -30.2 m/s
}

#[test]
fn whitening_matters() {
    // The same packet without whitening decodes to a different position:
    // the de-whitening step is really applied.
    let p = glider();
    let pk = encode(&p);
    let raw_lat_word = u32::from_le_bytes([pk[4], pk[5], pk[6], pk[7]]);
    let opened = ogn::open(&pk).unwrap();
    assert_ne!(raw_lat_word, opened.data[0]);
}

#[test]
fn random_noise_is_refused() {
    let mut x = 0x1234_5678u32;
    let mut accepted = 0;
    for _ in 0..5000 {
        let mut b = [0u8; PACKET_BYTES];
        for v in &mut b {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            *v = x as u8;
        }
        if ldpc_check(&b) == 0 {
            accepted += 1;
        }
    }
    // 48 parity checks: about 1 in 2^48 passes by chance.
    assert_eq!(accepted, 0);
}

#[test]
fn record_passes_through_csbee_to_the_web_head() {
    let a = ogn::decode(&encode(&glider())).unwrap().aircraft;
    let mut line = [0u8; adsb::MAX_LINE];
    let n = adsb::encode::aircraft(&a, &mut line).unwrap();
    assert!(line.starts_with(b"#O:2DD1234,"));
    let back: Vec<u8> = line[..n].to_vec();
    match adsb::parse(&back).unwrap() {
        adsb::Report::Aircraft(b) => {
            assert_eq!(b.source, adsb::Source::Ogn);
            assert_eq!(b.icao, a.icao);
        }
        r => panic!("{r:?}"),
    }
}
