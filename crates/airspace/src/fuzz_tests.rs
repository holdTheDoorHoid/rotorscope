//! Fuzz-style tests (crates/fuzzkit) for the airspace pack loader and every
//! lookup over a loaded pack. The pack comes off the hub's SD card, so its
//! bytes are outside input. Mutated packs get their CRC (and length) fixed
//! up most of the time, so damage reaches the readers behind the CRC check.

extern crate std;

use std::vec::Vec;

use crate::tests::{grid_pack, ring_pack, sample_pack};
use crate::{AreaRef, Pack, crc32};

/// Make `b` pass the header's length and CRC checks where it can.
fn fix_up(b: &mut [u8]) {
    if b.len() < 40 {
        return;
    }
    let header_len = usize::from(u16::from_le_bytes([b[6], b[7]]));
    let total = b.len() as u32;
    b[32..36].copy_from_slice(&total.to_le_bytes());
    if (40..=b.len()).contains(&header_len) {
        let c = crc32(&b[header_len..]);
        b[36..40].copy_from_slice(&c.to_le_bytes());
    }
}

fn exercise(p: &Pack<'_>) {
    let pts = [(39.95, -75.16), (39.9, -75.3), (40.2, -74.9), (90.0, 180.0), (-90.0, -180.0), (f64::NAN, 0.0), (39.95, f64::INFINITY)];
    for &(lat, lon) in &pts {
        for a in p.areas_at(lat, lon) {
            let _ = (a.contains(lat, lon), a.edge_distance_m(lat, lon), a.active_at(Some(1_794_700_000)), a.limits_msl_m(Some(10.0)), a.contains_alt(100.0, None));
        }
        let _ = (p.ground_msl_m(lat, lon), p.geoid_m(lat, lon), p.ground_hae_m(lat, lon), p.uasfm_ceiling_ft(lat, lon), p.urban(lat, lon));
        let _ = p.nearest_airport(lat, lon, |_| true);
        if let Some(g) = p.terrain() {
            if let Some((r, c)) = g.cell(lat, lon) {
                let _ = (g.u8_at(r, c), g.i16_at(r, c), g.bit_at(r, c));
            }
            let _ = g.bilinear_i16(lat, lon, 1.0);
            let _ = g.spacing_m();
        }
    }
    for a in p.areas() {
        for i in 0..a.n_points().min(64) {
            let _ = a.point(i);
        }
        let _ = a.contains(39.95, -75.16);
    }
    for _ in p.sources() {}
    for a in p.airports() {
        assert!(a.ident.len() <= 8);
    }
    let _ = p.line_of_sight((39.95, -75.16, 30.0), (39.97, -75.10, 120.0));
    let mut idx = [AreaRef::default(); 64];
    if let Some(n) = p.build_index(&mut idx) {
        assert!(n <= idx.len());
        let q = p.clone().with_index(&idx[..n]);
        for _ in q.areas_at(39.95, -75.16) {}
    }
}

#[test]
fn fuzz_airspace_pack() {
    let seeds = [sample_pack(), grid_pack(40, 40), ring_pack(5)];
    let refs: Vec<&[u8]> = seeds.iter().map(|s| s.as_slice()).collect();
    for s in &seeds {
        let _ = Pack::parse(s);
    }
    assert!(Pack::parse(&seeds[0]).is_ok());
    let max = seeds.iter().map(Vec::len).max().unwrap() + 64;
    let cfg = fuzzkit::Config::new().max_len(max).random_percent(5);
    let (mut tried, mut parsed) = (0u32, 0u32);
    fuzzkit::run_with("airspace::Pack::parse", &cfg, &refs, 1_500, |bytes| {
        if let Ok(p) = Pack::parse(bytes) {
            exercise(&p);
        }
        let mut b = bytes.to_vec();
        if bytes.first() != Some(&0xEE) {
            fix_up(&mut b);
        }
        tried += 1;
        if let Ok(p) = Pack::parse(&b) {
            parsed += 1;
            assert!(p.len() <= b.len());
            exercise(&p);
        }
    });
    // Enough damaged packs get past the CRC to test the readers behind it.
    assert!(parsed * 10 >= tried || tried < 20, "{parsed} of {tried} parsed");
}
