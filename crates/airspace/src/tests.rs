extern crate std;

use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;

use crate::format::*;
use crate::sun::{self, Light};
use crate::write::*;
use crate::*;

/// Unix ms for a UTC date and time.
fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> u64 {
    // Days from civil (Howard Hinnant).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = (y - era * 400) as i64;
    let mp = ((mo + 9) % 12) as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era as i64 * 146_097 + doe - 719_468;
    ((days * 86_400 + h as i64 * 3600 + mi as i64 * 60) * 1000) as u64
}

const PHL: (f64, f64) = (39.95, -75.16);

/// US Naval Observatory, "Sun and Moon Data for One Day", 39.95 N 75.16 W,
/// times in UTC (aa.usno.navy.mil/api/rstt/oneday, fetched 2026-11-15):
///   2026-03-20  begin civil twilight 10:37, sunrise 11:04, transit 17:08,
///               sunset 23:13, end civil twilight 23:40
///   2026-06-21  end civil twilight 01:05 (of the 20th's evening),
///               begin civil twilight 08:59, sunrise 09:32, transit 17:03
///   2026-12-21  begin civil twilight 11:48, sunrise 12:19, transit 16:59,
///               sunset 21:39, end civil twilight 22:09
/// USNO rounds to the minute; near the horizon the sun moves about 0.15-0.2
/// degree a minute here, so within 0.3 degree of -6 at the listed minute.
#[test]
fn civil_twilight_matches_usno_philadelphia() {
    let twilight = [
        utc(2026, 3, 20, 10, 37),
        utc(2026, 3, 20, 23, 40),
        utc(2026, 6, 21, 1, 5),
        utc(2026, 6, 21, 8, 59),
        utc(2026, 12, 21, 11, 48),
        utc(2026, 12, 21, 22, 9),
    ];
    for t in twilight {
        let el = sun::elevation_deg(t, PHL.0, PHL.1);
        assert!((el + 6.0).abs() < 0.3, "{t}: {el}");
    }
    // Sunrise/sunset: centre at -0.833 (refraction + semi-diameter).
    for t in [
        utc(2026, 3, 20, 11, 4),
        utc(2026, 3, 20, 23, 13),
        utc(2026, 6, 21, 9, 32),
        utc(2026, 12, 21, 12, 19),
        utc(2026, 12, 21, 21, 39),
    ] {
        let el = sun::elevation_deg(t, PHL.0, PHL.1);
        assert!((el + 0.833).abs() < 0.3, "{t}: {el}");
    }
    // Upper transit: 90 - lat + declination (+23.44 June, -23.44 Dec, ~0 March).
    let noon = |t, decl: f64| {
        let el = sun::elevation_deg(t, PHL.0, PHL.1);
        assert!((el - (90.0 - PHL.0 + decl)).abs() < 0.3, "{t}: {el}");
    };
    noon(utc(2026, 6, 21, 17, 3), 23.44);
    noon(utc(2026, 12, 21, 16, 59), -23.44);
    noon(utc(2026, 3, 20, 17, 8), 0.0);
    // Light classes either side of the twilight boundary.
    assert_eq!(
        sun::light(utc(2026, 12, 21, 22, 15), PHL.0, PHL.1).0,
        Light::Night
    );
    assert_eq!(
        sun::light(utc(2026, 12, 21, 22, 0), PHL.0, PHL.1).0,
        Light::Twilight
    );
    assert_eq!(
        sun::light(utc(2026, 12, 21, 18, 0), PHL.0, PHL.1).0,
        Light::Day
    );
}

fn square(lat: f64, lon: f64, half: f64) -> Vec<(f64, f64)> {
    vec![
        (lat - half, lon - half),
        (lat - half, lon + half),
        (lat + half, lon + half),
        (lat + half, lon - half),
    ]
}

fn area(
    kind: AreaKind,
    rings: Vec<Vec<(f64, f64)>>,
    floor: (AltRef, i32),
    ceil: (AltRef, i32),
) -> AreaDef {
    AreaDef {
        kind,
        source: 1,
        floor_ref: floor.0,
        floor_ft: floor.1,
        ceil_ref: ceil.0,
        ceil_ft: ceil.1,
        start: 0,
        end: 0,
        flags: 0,
        name: "TEST AREA".to_string(),
        rings,
    }
}

fn sample_pack() -> Vec<u8> {
    let mut b = PackBuilder {
        created: 1_794_700_000,
        center: PHL,
        radius_m: 20_000,
        ..Default::default()
    };
    b.sources.push(SourceDef {
        id: 1,
        date: 1_793_274_474,
        fetched: 1_794_700_000,
        name: "FAA ADDS Class_Airspace".to_string(),
    });
    // Square with a square hole.
    b.areas.push(area(
        AreaKind::ClassB,
        vec![square(39.9, -75.2, 0.05), square(39.9, -75.2, 0.01)],
        (AltRef::Agl, 0),
        (AltRef::Msl, 7000),
    ));
    let mut tfr = area(
        AreaKind::Tfr,
        vec![circle(40.0, -75.0, 5_000.0, 36)],
        (AltRef::Agl, 0),
        (AltRef::Msl, 3000),
    );
    tfr.start = 1000;
    tfr.end = 2000;
    b.areas.push(tfr);
    // Terrain: 11 x 11 posts, 0.01 deg apart from (39.85, -75.25); height
    // rises 10 m per row: a ridge of 200 m at row 5.
    let mut t = Vec::new();
    for r in 0..11 {
        for _c in 0..11 {
            t.push(if r == 5 { 200 } else { (r * 10) as i16 });
        }
    }
    b.terrain = Some(GridDef {
        lat0: 39.85,
        lon0: -75.25,
        dlat: 0.01,
        dlon: 0.01,
        rows: 11,
        cols: 11,
        values: t,
    });
    b.geoid_cm = Some(GridDef {
        lat0: 39.8,
        lon0: -75.3,
        dlat: 0.2,
        dlon: 0.2,
        rows: 2,
        cols: 2,
        values: vec![-3300, -3300, -3200, -3200],
    });
    b.uasfm = Some(GridDef {
        lat0: 39.85,
        lon0: -75.25,
        dlat: 1.0 / 120.0,
        dlon: 1.0 / 120.0,
        rows: 2,
        cols: 2,
        values: vec![0, 10, 40, format::UASFM_NONE],
    });
    b.urban = Some(GridDef {
        lat0: 39.85,
        lon0: -75.25,
        dlat: 0.05,
        dlon: 0.05,
        rows: 2,
        cols: 2,
        values: vec![true, false, false, true],
    });
    b.airports.push(AirportDef {
        lat: 39.8719,
        lon: -75.2411,
        elev_ft: 36,
        kind: 0,
        flags: 3,
        ident: "PHL".to_string(),
    });
    b.airports.push(AirportDef {
        lat: 40.0819,
        lon: -75.0106,
        elev_ft: 120,
        kind: 0,
        flags: 3,
        ident: "PNE".to_string(),
    });
    b.to_bytes()
}

#[test]
fn pack_round_trip_and_lookups() {
    let bytes = sample_pack();
    let p = Pack::parse(&bytes).unwrap();
    assert_eq!(p.created, 1_794_700_000);
    assert_eq!(p.source(1).unwrap().name, "FAA ADDS Class_Airspace");
    assert_eq!(p.areas().count(), 2);
    // Inside the ring, outside the hole.
    let a: Vec<_> = p.areas_at(39.93, -75.2).collect();
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].kind, AreaKind::ClassB);
    assert_eq!(a[0].name, "TEST AREA");
    assert_eq!(p.areas_at(39.9, -75.2).count(), 0, "in the hole");
    // 0.02 deg north of the hole's top edge (39.91): ~2.2 km.
    let e = a[0].edge_distance_m(39.93, -75.2);
    assert!((e - 2224.0).abs() < 20.0, "{e}");
    assert_eq!(p.areas_at(39.7, -75.2).count(), 0, "outside");
    // The same answers through a bounding-box index.
    let mut idx = [AreaRef::default(); 4];
    let n = p.build_index(&mut idx).unwrap();
    assert_eq!(n, 2);
    let pi = p.with_index(&idx[..n]);
    assert_eq!(pi.areas_at(39.93, -75.2).count(), 1);
    assert_eq!(pi.areas_at(39.9, -75.2).count(), 0);
    assert_eq!(pi.areas_at(40.0, -75.0).count(), 1);
    assert!(p.build_index(&mut idx[..1]).is_none(), "too small");
    // Altitude band: surface floor, 7000 ft MSL ceiling.
    assert_eq!(a[0].contains_alt(100.0, None), Some(true));
    assert_eq!(a[0].contains_alt(7001.0 * M_PER_FT, None), Some(false));
    // TFR time window.
    let tfr = p.areas_at(40.0, -75.0).next().unwrap();
    assert!(tfr.active_at(Some(1500)) && !tfr.active_at(Some(2500)) && tfr.active_at(None));
    // Terrain: exactly on a post, and halfway between rows 1 and 2.
    assert_eq!(p.ground_msl_m(39.86, -75.2), Some(10.0));
    assert!((p.ground_msl_m(39.865, -75.2).unwrap() - 15.0).abs() < 1e-3);
    assert_eq!(p.ground_msl_m(40.5, -75.2), None, "off the grid");
    assert!((p.geoid_m(39.8, -75.3).unwrap() + 33.0).abs() < 1e-3);
    assert!((p.ground_hae_m(39.86, -75.2).unwrap() - (10.0 - 32.7)).abs() < 0.01);
    // Facility map cells.
    assert_eq!(p.uasfm_ceiling_ft(39.851, -75.249), Some(0));
    assert_eq!(p.uasfm_ceiling_ft(39.851, -75.24), Some(100));
    assert_eq!(p.uasfm_ceiling_ft(39.86, -75.249), Some(400));
    assert_eq!(p.uasfm_ceiling_ft(39.86, -75.24), None);
    assert_eq!(p.uasfm_ceiling_ft(39.95, -75.24), None);
    assert_eq!(p.urban(39.86, -75.24), Some(true));
    assert_eq!(p.urban(39.86, -75.19), Some(false));
    let (ap, d) = p.nearest_airport(39.88, -75.24, |_| true).unwrap();
    assert_eq!(ap.ident, "PHL");
    assert!(d > 800.0 && d < 1100.0, "{d}");
}

#[test]
fn line_of_sight_over_a_ridge() {
    let bytes = sample_pack();
    let p = Pack::parse(&bytes).unwrap();
    // From row 1 (10 m) to row 9 (90 m), along a column: the 200 m ridge
    // at row 5 blocks a line 2 m above the ground at each end...
    let a = (39.86, -75.2, 12.0);
    let b = (39.94, -75.2, 92.0);
    let los = p.line_of_sight(a, b).unwrap();
    assert!(los.worst_m > 100.0, "{los:?}");
    assert!((los.at - 0.5).abs() < 0.15, "{los:?}");
    // ...but not a drone 400 m up.
    let los = p.line_of_sight(a, (39.94, -75.2, 400.0)).unwrap();
    assert!(los.worst_m < 0.0, "{los:?}");
}

#[test]
fn damaged_pack_is_refused() {
    let mut bytes = sample_pack();
    let n = bytes.len();
    bytes[n - 5] ^= 0x40;
    assert!(matches!(Pack::parse(&bytes), Err(PackError::Crc { .. })));
    assert_eq!(Pack::parse(&bytes[..30]).unwrap_err(), PackError::TooShort);
    let mut v = sample_pack();
    v[4] = 9;
    assert_eq!(Pack::parse(&v).unwrap_err(), PackError::Version(9));
}

#[test]
fn point_in_polygon_edges() {
    let ring = [(0, 0), (0, 100), (100, 100), (100, 0)];
    let pip = |la, lo| point_in_rings(la, lo, [4usize].into_iter(), |i| ring[i]);
    assert!(pip(50, 50));
    assert!(!pip(150, 50));
    assert!(!pip(50, -1));
    assert!(pip(1, 99));
    // A concave "U".
    let u = [
        (0, 0),
        (0, 30),
        (20, 30),
        (20, 20),
        (10, 20),
        (10, 10),
        (20, 10),
        (20, 0),
    ];
    let pip = |la, lo| point_in_rings(la, lo, [8usize].into_iter(), |i| u[i]);
    assert!(pip(5, 15));
    assert!(!pip(15, 15), "in the notch");
    assert!(pip(15, 5));
}

/// The size maths must not wrap on a 32-bit hub: 65536 x 65536 one-byte
/// cells is 4 GiB, which is 0 in a u32/usize product.
#[test]
fn grid_size_does_not_wrap() {
    use crate::pack::grid_bytes_needed;
    assert_eq!(grid_bytes_needed(65536, 65536, 8), Some(1u64 << 32));
    assert_eq!(grid_bytes_needed(16384, 65536, 16), Some(1u64 << 31));
    assert_eq!(grid_bytes_needed(65536, 65536, 16), Some(1u64 << 33));
    assert_eq!(grid_bytes_needed(3, 3, 1), Some(2));
    assert_eq!(
        grid_bytes_needed(u32::MAX, u32::MAX, 1),
        Some((u32::MAX as u64).pow(2).div_ceil(8))
    );
    assert_eq!(grid_bytes_needed(u32::MAX, u32::MAX, 8), None);
    assert_eq!(grid_bytes_needed(u32::MAX, u32::MAX, u64::MAX), None);
    // Truncated to 32 bits, the wrapped cases above would all read 0.
    assert_eq!((1u64 << 32) as u32, 0);
}

fn grid_pack(rows: u32, cols: u32) -> Vec<u8> {
    // Build a valid pack with a tiny urban grid, then rewrite the header's
    // rows/cols (and the CRC) so the grid claims more than it holds.
    let b = PackBuilder {
        urban: Some(GridDef {
            lat0: 39.0,
            lon0: -75.0,
            dlat: 0.01,
            dlon: 0.01,
            rows: 2,
            cols: 4,
            values: vec![true; 8],
        }),
        ..Default::default()
    };
    let mut bytes = b.to_bytes();
    let off = Pack::parse(&bytes)
        .unwrap()
        .section(Section::Urban)
        .unwrap()
        .as_ptr() as usize
        - bytes.as_ptr() as usize;
    bytes[off + 16..off + 20].copy_from_slice(&rows.to_le_bytes());
    bytes[off + 20..off + 24].copy_from_slice(&cols.to_le_bytes());
    let crc = crc32(&bytes[HEADER_LEN..]);
    bytes[36..40].copy_from_slice(&crc.to_le_bytes());
    bytes
}

#[test]
fn oversized_grid_is_refused_not_read() {
    let bytes = grid_pack(65536, 65536);
    let p = Pack::parse(&bytes).unwrap();
    // The grid claims 512 MiB of bits; the file holds one byte.
    assert!(p.urban(39.005, -74.995).is_none());
    assert!(p.urban_grid().is_none());
}

#[test]
fn out_of_range_grid_reads_do_not_panic() {
    let bytes = grid_pack(2, 4);
    let p = Pack::parse(&bytes).unwrap();
    let g = p.urban_grid().unwrap();
    assert!(g.bit_at(1, 3));
    assert!(!g.bit_at(1_000_000, 1_000_000));
    assert!(!g.bit_at(u32::MAX, u32::MAX));
    assert_eq!(g.u8_at(u32::MAX, u32::MAX), UASFM_NONE);
    assert_eq!(g.i16_at(u32::MAX, u32::MAX), TERRAIN_NONE);
    // Reading 16-bit cells off an 8-cell bit grid must not panic either.
    assert_eq!(g.i16_at(1, 3), TERRAIN_NONE);
}

/// A pack whose first area claims ring lengths summing past its points.
fn ring_pack(fake_len: u16) -> Vec<u8> {
    let mut b = PackBuilder::default();
    b.areas.push(area(
        AreaKind::ClassB,
        vec![square(39.9, -75.2, 0.05)],
        (AltRef::Agl, 0),
        (AltRef::Msl, 7000),
    ));
    let mut bytes = b.to_bytes();
    let off = Pack::parse(&bytes)
        .unwrap()
        .section(Section::Areas)
        .unwrap()
        .as_ptr() as usize
        - bytes.as_ptr() as usize;
    // Area record: 4 bytes of count, 44 head, name padded to 4, then rings.
    let rings_at = off + 4 + AREA_HEAD_LEN + "TEST AREA".len().next_multiple_of(4);
    bytes[rings_at..rings_at + 2].copy_from_slice(&fake_len.to_le_bytes());
    let crc = crc32(&bytes[HEADER_LEN..]);
    bytes[36..40].copy_from_slice(&crc.to_le_bytes());
    bytes
}

#[test]
fn ring_longer_than_points_is_refused() {
    assert!(Pack::parse(&ring_pack(4)).is_ok());
    assert_eq!(Pack::parse(&ring_pack(5)).unwrap_err(), PackError::Bounds);
    assert_eq!(
        Pack::parse(&ring_pack(60000)).unwrap_err(),
        PackError::Bounds
    );
}

#[test]
fn point_out_of_range_does_not_panic() {
    let bytes = ring_pack(4);
    let p = Pack::parse(&bytes).unwrap();
    let a = p.areas().next().unwrap();
    assert_eq!(a.n_points(), 4);
    assert_eq!(a.point(4), (0, 0));
    assert_eq!(a.point(usize::MAX), (0, 0));
}
