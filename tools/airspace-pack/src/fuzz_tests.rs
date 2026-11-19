//! Fuzz-style tests (crates/fuzzkit) for the FAA source readers in
//! airspace-pack (GeoJSON geometry, altitudes as numbers and as text) and
//! for the pack they feed: whatever geometry survives clipping and
//! simplifying must write a pack the hub can load.

use airspace::write::{AreaDef, PackBuilder, SourceDef};
use airspace::{AreaKind, Pack};
use serde_json::Value;

use crate::{Region, alt, alt_text, area, clip_ring, point_of, rings_of, simplify};

#[test]
fn fuzz_geometry_to_pack() {
    let seeds: [&[u8]; 4] = [
        br#"{"type":"Polygon","coordinates":[[[-75.2,39.9],[-75.1,39.9],[-75.1,40.0],[-75.2,40.0],[-75.2,39.9]]]}"#,
        br#"{"type":"MultiPolygon","coordinates":[[[[-75.3,39.8],[-75.25,39.8],[-75.25,39.85],[-75.3,39.8]]],[[[-75,40],[-74.9,40],[-74.9,40.1],[-75,40]]]]}"#,
        br#"{"type":"Point","coordinates":[-75.16,39.95]}"#,
        br#"{"v":"1200","code":"MSL","uom":"FT","t":"400' AGL"}"#,
    ];
    let dict: [&[u8]; 6] = [b"[", b"]", b",", b"-", b"1e308", b"\"coordinates\":"];
    let cfg = fuzzkit::Config::new().max_len(800).dict(&dict);
    let region = Region { lat_min: 39.7, lat_max: 40.2, lon_min: -75.5, lon_max: -74.8 };
    let mut built = 0;
    fuzzkit::run_with("airspace-pack::geometry", &cfg, &seeds, 3_000, |bytes| {
        let _ = alt_text(&fuzzkit::text(bytes));
        let Ok(v) = serde_json::from_slice::<Value>(bytes) else { return };
        let _ = point_of(&v);
        let _ = alt(&v["v"], &v["code"], &v["uom"]);
        let mut rings = Vec::new();
        for r in rings_of(&v) {
            let c = clip_ring(&r, &region);
            let s = simplify(&c, 0.0005);
            for &(lat, lon) in &s {
                assert!(lat.is_nan() || (region.lat_min - 1e-9..=region.lat_max + 1e-9).contains(&lat), "{lat}");
                assert!(lon.is_nan() || (region.lon_min - 1e-9..=region.lon_max + 1e-9).contains(&lon), "{lon}");
            }
            if s.len() >= 3 && s.iter().all(|p| p.0.is_finite() && p.1.is_finite()) {
                rings.push(s);
            }
        }
        if rings.is_empty() || rings.len() > 8 {
            return;
        }
        let mut b = PackBuilder { created: 1_794_700_000, center: (39.95, -75.16), radius_m: 40_000, ..Default::default() };
        b.sources.push(SourceDef { id: 1, date: 1_794_700_000, fetched: 1_794_700_000, name: "FUZZ".into() });
        let a: AreaDef = area(AreaKind::ClassB, 1, alt_text("Surface"), alt(&v["v"], &v["code"], &v["uom"]), "FUZZ AREA".into(), rings);
        b.areas.push(a);
        let bytes = b.to_bytes();
        let p = Pack::parse(&bytes).expect("a written pack loads");
        assert_eq!(p.areas().count(), 1);
        built += 1;
    });
    assert!(built > 0, "no input reached the pack writer");
}
