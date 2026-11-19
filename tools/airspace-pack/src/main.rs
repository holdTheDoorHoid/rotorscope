//! `airspace-pack`: build a Rotorscope airspace pack (PLAN.md Phase 12).
//!
//! Downloads public US-government data around a point, cuts it to a region,
//! simplifies it and writes one pack file for the hub's SD card
//! (`ROTORSCP/AIRSPACE.PAK`):
//!
//! | Data | Where from |
//! |---|---|
//! | Class B/C/D/E surface airspace | FAA ADDS open data, `Class_Airspace` |
//! | UAS Facility Map (LAANC) ceilings | FAA ADDS, `FAA_UAS_FacilityMap_Data` |
//! | Prohibited / restricted areas | FAA ADDS, `Special_Use_Airspace` (P, R) and `Prohibited_Areas` |
//! | National-security UAS restrictions | FAA ADDS, `DoD_Mar_13` and `Part_Time_National_Security_UAS_Flight_Restrictions` |
//! | Stadiums (3 NM, events only) | FAA ADDS, `Stadiums` |
//! | National defense airspace | FAA ADDS, `National_Defense_Airspace_TFR_Areas` |
//! | FRIAs | FAA ADDS, `FAA_Recognized_Identification_Areas` |
//! | Airports | FAA ADDS, `US_Airport` |
//! | TFRs (dated list) | tfr.faa.gov (list API, WFS shapes, NOTAM XML) |
//! | Terrain | USGS 3DEP elevation service (resampled server-side) |
//! | Geoid | NOAA NGS GEOID18 API (5 x 5 points) |
//! | Urban areas (91.119 "congested") | US Census TIGERweb 2020 Urban Areas |
//!
//! All are US-government public-domain data with no key or login.
//! Downloads are cached under `--cache` (default `tools/airspace-pack/cache`,
//! git-ignored); TFRs are cached per day.
//!
//! ```text
//! airspace-pack --lat 39.95 --lon -75.16 [--radius-km 100] [--terrain-arcsec 15]
//!               [--out AIRSPACE.PAK] [--cache DIR] [--offline]
//! ```

use std::collections::HashSet;
use std::io::Read;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use airspace::format::{UASFM_NONE, src};
use airspace::write::{AirportDef, AreaDef, GridDef, PackBuilder, SourceDef, circle};
use airspace::{AltRef, AreaKind, Pack};
use serde_json::Value;

type Res<T> = Result<T, String>;

const ADDS: &str = "https://services6.arcgis.com/ssFJjBXIUyZDrSYZ/arcgis/rest/services";
const NM: f64 = 1852.0;

struct Args {
    lat: f64,
    lon: f64,
    radius_km: f64,
    terrain_arcsec: f64,
    out: PathBuf,
    cache: PathBuf,
    offline: bool,
}

#[cfg(test)]
mod fuzz_tests;

fn parse_args() -> Res<Args> {
    let mut a = Args {
        lat: f64::NAN,
        lon: f64::NAN,
        radius_km: 100.0,
        terrain_arcsec: 15.0,
        out: PathBuf::from("AIRSPACE.PAK"),
        cache: PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/cache")),
        offline: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut v = || it.next().ok_or_else(|| format!("{k} needs a value"));
        let num = |s: String| s.parse::<f64>().map_err(|e| format!("{s}: {e}"));
        match k.as_str() {
            "--lat" => a.lat = num(v()?)?,
            "--lon" => a.lon = num(v()?)?,
            "--radius-km" => a.radius_km = num(v()?)?,
            "--terrain-arcsec" => a.terrain_arcsec = num(v()?)?,
            "--out" => a.out = PathBuf::from(v()?),
            "--cache" => a.cache = PathBuf::from(v()?),
            "--offline" => a.offline = true,
            "-h" | "--help" => {
                println!(
                    "airspace-pack --lat 39.95 --lon -75.16 [--radius-km 100] [--terrain-arcsec 15] [--out AIRSPACE.PAK] [--cache DIR] [--offline]"
                );
                std::process::exit(0);
            }
            _ => return Err(format!("unknown argument {k}")),
        }
    }
    if !a.lat.is_finite() || !a.lon.is_finite() {
        return Err("--lat and --lon are required".into());
    }
    if !(1.0..=400.0).contains(&a.radius_km) {
        return Err("--radius-km must be 1..400".into());
    }
    Ok(a)
}

/// The region: a bounding box around the centre.
#[derive(Clone, Copy)]
struct Region {
    lat_min: f64,
    lat_max: f64,
    lon_min: f64,
    lon_max: f64,
}

impl Region {
    fn of(lat: f64, lon: f64, r_m: f64) -> Self {
        let dlat = r_m / 111_195.0;
        let dlon = dlat / lat.to_radians().cos();
        Self {
            lat_min: lat - dlat,
            lat_max: lat + dlat,
            lon_min: lon - dlon,
            lon_max: lon + dlon,
        }
    }

    fn envelope(&self) -> String {
        format!(
            "{:.5},{:.5},{:.5},{:.5}",
            self.lon_min, self.lat_min, self.lon_max, self.lat_max
        )
    }

    fn overlaps(&self, rings: &[Vec<(f64, f64)>]) -> bool {
        let pts = rings.iter().flatten();
        let (mut a, mut b, mut c, mut d) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
        for &(la, lo) in pts {
            a = a.min(la);
            b = b.max(la);
            c = c.min(lo);
            d = d.max(lo);
        }
        a <= self.lat_max && b >= self.lat_min && c <= self.lon_max && d >= self.lon_min
    }
}

struct Net {
    cache: PathBuf,
    offline: bool,
    agent: ureq::Agent,
    downloaded: u64,
    from_cache: u64,
}

impl Net {
    /// GET `url`, cached as `name` under the cache directory.
    fn get(&mut self, url: &str, name: &str) -> Res<Vec<u8>> {
        let path = self.cache.join(name);
        if let Ok(b) = std::fs::read(&path) {
            self.from_cache += b.len() as u64;
            return Ok(b);
        }
        if self.offline {
            return Err(format!("{name} not cached and --offline"));
        }
        let mut last = String::new();
        for attempt in 0..3 {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_secs(2 * attempt));
            }
            match self.agent.get(url).set("Accept", "*/*").call() {
                Ok(r) => {
                    let mut b = Vec::new();
                    r.into_reader()
                        .take(200 << 20)
                        .read_to_end(&mut b)
                        .map_err(|e| format!("{url}: {e}"))?;
                    self.downloaded += b.len() as u64;
                    std::fs::create_dir_all(&self.cache).map_err(|e| e.to_string())?;
                    std::fs::write(&path, &b).map_err(|e| e.to_string())?;
                    return Ok(b);
                }
                Err(ureq::Error::Status(code, r)) => {
                    let body = r.into_string().unwrap_or_default();
                    last = format!(
                        "{url}: HTTP {code}: {}",
                        body.chars()
                            .take(160)
                            .collect::<String>()
                            .replace('\n', " ")
                    );
                }
                Err(e) => last = format!("{url}: {e}"),
            }
        }
        Err(last)
    }

    fn json(&mut self, url: &str, name: &str) -> Res<Value> {
        let b = self.get(url, name)?;
        serde_json::from_slice(&b).map_err(|e| {
            let _ = std::fs::remove_file(self.cache.join(name));
            format!("{name}: {e}")
        })
    }
}

fn enc(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'*' => {
                o.push(b as char)
            }
            _ => o.push_str(&format!("%{b:02X}")),
        }
    }
    o
}

/// One FAA ADDS (ArcGIS) layer: every feature intersecting the region, as
/// GeoJSON, simplified server-side by `offset_deg`; plus the layer's data date.
fn adds_layer(
    net: &mut Net,
    key: &str,
    service: &str,
    filter: &str,
    fields: &str,
    geometry: bool,
    offset_deg: f64,
    rg: &Region,
) -> Res<(Vec<Value>, u32)> {
    let base = format!("{ADDS}/{service}/FeatureServer/0");
    let meta = net.json(
        &format!("{base}?f=json"),
        &format!("{service}-{key}-meta.json"),
    )?;
    let date = meta["editingInfo"]["dataLastEditDate"]
        .as_u64()
        .or(meta["editingInfo"]["lastEditDate"].as_u64())
        .map_or(0, |ms| (ms / 1000) as u32);
    let mut out = Vec::new();
    let page = 1000;
    for p in 0.. {
        let url = format!(
            "{base}/query?where={}&geometry={}&geometryType=esriGeometryEnvelope&inSR=4326&spatialRel=esriSpatialRelIntersects&outFields={}&returnGeometry={geometry}&outSR=4326&maxAllowableOffset={offset_deg}&geometryPrecision=6&resultOffset={}&resultRecordCount={page}&f=geojson",
            enc(filter),
            rg.envelope(),
            enc(fields),
            p * page
        );
        let v = net.json(&url, &format!("{service}-{key}-p{p}.json"))?;
        if let Some(e) = v.get("error") {
            return Err(format!("{service}: {e}"));
        }
        let feats = v["features"].as_array().cloned().unwrap_or_default();
        let n = feats.len();
        out.extend(feats);
        let more = v["properties"]["exceededTransferLimit"]
            .as_bool()
            .unwrap_or(false)
            || v["exceededTransferLimit"].as_bool().unwrap_or(false);
        if !more || n == 0 {
            break;
        }
    }
    Ok((out, date))
}

/// GeoJSON Polygon / MultiPolygon to rings of (lat, lon).
fn rings_of(geom: &Value) -> Vec<Vec<(f64, f64)>> {
    let ring = |r: &Value| -> Vec<(f64, f64)> {
        let mut v: Vec<(f64, f64)> = r
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| Some((p[1].as_f64()?, p[0].as_f64()?)))
            .collect();
        if v.len() > 1 && v.first() == v.last() {
            v.pop();
        }
        v
    };
    match geom["type"].as_str() {
        Some("Polygon") => geom["coordinates"]
            .as_array()
            .into_iter()
            .flatten()
            .map(ring)
            .collect(),
        Some("MultiPolygon") => geom["coordinates"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|p| p.as_array().into_iter().flatten().map(ring))
            .collect(),
        _ => Vec::new(),
    }
    .into_iter()
    .filter(|r| r.len() >= 3)
    .collect()
}

fn point_of(geom: &Value) -> Option<(f64, f64)> {
    (geom["type"].as_str() == Some("Point")).then(|| {
        Some((
            geom["coordinates"][1].as_f64()?,
            geom["coordinates"][0].as_f64()?,
        ))
    })?
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

/// An FAA altitude (value, code, unit) as (ref, feet).
fn alt(val: &Value, code: &Value, uom: &Value) -> (AltRef, i32) {
    let v = num(val).unwrap_or(-9998.0);
    let code = code.as_str().unwrap_or("");
    if v <= -9000.0 || code == "UNLTD" {
        return (AltRef::Unlimited, 0);
    }
    let ft = if uom.as_str() == Some("FL") {
        v * 100.0
    } else {
        v
    };
    match code {
        "SFC" | "AGL" => (AltRef::Agl, ft as i32),
        _ => (AltRef::Msl, ft as i32),
    }
}

/// "400' AGL", "Surface", "1,200 MSL" to (ref, feet).
fn alt_text(s: &str) -> (AltRef, i32) {
    let s = s.to_ascii_uppercase();
    if s.contains("SURFACE") || s.trim() == "SFC" {
        return (AltRef::Agl, 0);
    }
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    let v = digits.parse::<i32>().unwrap_or(400);
    if s.contains("MSL") {
        (AltRef::Msl, v)
    } else {
        (AltRef::Agl, v)
    }
}

fn text(v: &Value) -> String {
    v.as_str().unwrap_or("").trim().to_string()
}

fn area(
    kind: AreaKind,
    source: u8,
    floor: (AltRef, i32),
    ceil: (AltRef, i32),
    name: String,
    rings: Vec<Vec<(f64, f64)>>,
) -> AreaDef {
    AreaDef {
        kind,
        source,
        floor_ref: floor.0,
        floor_ft: floor.1,
        ceil_ref: ceil.0,
        ceil_ft: ceil.1,
        start: 0,
        end: 0,
        flags: 0,
        name,
        rings,
    }
}

/// Sutherland-Hodgman clip of one ring to a box. Concave rings may gain
/// edges along the box sides; even-odd point tests inside the box are
/// unaffected.
fn clip_ring(ring: &[(f64, f64)], b: &Region) -> Vec<(f64, f64)> {
    let mut pts = ring.to_vec();
    // (inside test, intersection) for each of the four sides.
    let sides: [(
        &dyn Fn((f64, f64)) -> bool,
        &dyn Fn((f64, f64), (f64, f64)) -> (f64, f64),
    ); 4] = [
        (&|p| p.0 >= b.lat_min, &|a, c| lerp_lat(a, c, b.lat_min)),
        (&|p| p.0 <= b.lat_max, &|a, c| lerp_lat(a, c, b.lat_max)),
        (&|p| p.1 >= b.lon_min, &|a, c| lerp_lon(a, c, b.lon_min)),
        (&|p| p.1 <= b.lon_max, &|a, c| lerp_lon(a, c, b.lon_max)),
    ];
    for (inside, cut) in sides {
        if pts.is_empty() {
            break;
        }
        let input = std::mem::take(&mut pts);
        for i in 0..input.len() {
            let (cur, prev) = (input[i], input[(i + input.len() - 1) % input.len()]);
            match (inside(cur), inside(prev)) {
                (true, true) => pts.push(cur),
                (true, false) => {
                    pts.push(cut(prev, cur));
                    pts.push(cur);
                }
                (false, true) => pts.push(cut(prev, cur)),
                (false, false) => {}
            }
        }
    }
    pts
}

fn lerp_lat(a: (f64, f64), c: (f64, f64), lat: f64) -> (f64, f64) {
    let t = (lat - a.0) / (c.0 - a.0);
    (lat, a.1 + t * (c.1 - a.1))
}

fn lerp_lon(a: (f64, f64), c: (f64, f64), lon: f64) -> (f64, f64) {
    let t = (lon - a.1) / (c.1 - a.1);
    (a.0 + t * (c.0 - a.0), lon)
}

/// Douglas-Peucker on a closed ring, tolerance in degrees.
fn simplify(ring: &[(f64, f64)], tol: f64) -> Vec<(f64, f64)> {
    if ring.len() < 8 {
        return ring.to_vec();
    }
    let mut keep = vec![false; ring.len()];
    keep[0] = true;
    keep[ring.len() / 2] = true;
    let mut stack = vec![(0, ring.len() / 2), (ring.len() / 2, ring.len())];
    while let Some((a, b)) = stack.pop() {
        let pa = ring[a];
        let pb = ring[b % ring.len()];
        let (dy, dx) = (pb.0 - pa.0, pb.1 - pa.1);
        let len = (dx * dx + dy * dy).sqrt().max(1e-12);
        let (mut best, mut at) = (0.0, 0);
        for (i, p) in ring.iter().enumerate().take(b).skip(a + 1) {
            let d = ((p.1 - pa.1) * dy - (p.0 - pa.0) * dx).abs() / len;
            if d > best {
                best = d;
                at = i;
            }
        }
        if best > tol {
            keep[at] = true;
            stack.push((a, at));
            stack.push((at, b));
        }
    }
    ring.iter()
        .zip(keep)
        .filter(|(_, k)| *k)
        .map(|(p, _)| *p)
        .collect()
}

/// Scan-line fill of rings (even-odd) into a cell grid.
fn rasterize(grid: &mut GridDef<bool>, rings: &[Vec<(f64, f64)>]) {
    for r in 0..grid.rows {
        let y = grid.lat0 + (f64::from(r) + 0.5) * grid.dlat;
        let mut xs: Vec<f64> = Vec::new();
        for ring in rings {
            for i in 0..ring.len() {
                let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
                if (a.0 > y) != (b.0 > y) {
                    xs.push(a.1 + (y - a.0) * (b.1 - a.1) / (b.0 - a.0));
                }
            }
        }
        xs.sort_by(f64::total_cmp);
        for pair in xs.chunks(2) {
            if pair.len() < 2 {
                break;
            }
            let c0 = ((pair[0] - grid.lon0) / grid.dlon - 0.5).ceil().max(0.0) as i64;
            let c1 = ((pair[1] - grid.lon0) / grid.dlon - 0.5)
                .floor()
                .min(f64::from(grid.cols) - 1.0) as i64;
            for c in c0..=c1 {
                if c >= 0 {
                    grid.values[(r * grid.cols) as usize + c as usize] = true;
                }
            }
        }
    }
}

/// The value of the first `<tag>` after `from` in an XML string.
fn tag<'x>(xml: &'x str, name: &str) -> Option<&'x str> {
    let open = format!("<{name}>");
    let i = xml.find(&open)? + open.len();
    let j = xml[i..].find('<')? + i;
    Some(xml[i..j].trim())
}

/// "2026-11-17T22:00:00" in time zone `tz` to Unix seconds.
fn xml_time(s: &str, tz: &str) -> Option<u32> {
    let (d, t) = s.split_once('T')?;
    let mut dp = d.split('-').map(|x| x.parse::<i64>().ok());
    let (y, m, dd) = (dp.next()??, dp.next()??, dp.next()??);
    let mut tp = t.split(':').map(|x| x.parse::<i64>().ok());
    let (h, mi) = (tp.next()??, tp.next()??);
    let off_h = match tz {
        "UTC" | "GMT" | "" => 0,
        "EST" => -5,
        "EDT" => -4,
        "CST" => -6,
        "CDT" => -5,
        "MST" => -7,
        "MDT" => -6,
        "PST" => -8,
        "PDT" => -7,
        "AKST" => -9,
        "AKDT" => -8,
        "HST" => -10,
        _ => 0,
    };
    let days = days_from_civil(y, m, dd);
    Some((days * 86_400 + (h - off_h) * 3600 + mi * 60) as u32)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

fn date_str(unix: u32) -> String {
    let z = i64::from(unix / 86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{m:02}-{d:02}", yoe + era * 400 + i64::from(m <= 2))
}

fn main() {
    if let Err(e) = run() {
        eprintln!("airspace-pack: {e}");
        std::process::exit(1);
    }
}

fn run() -> Res<()> {
    let a = parse_args()?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs() as u32;
    let rg = Region::of(a.lat, a.lon, a.radius_km * 1000.0);
    let key = format!("{:.3}_{:.3}_{}", a.lat, a.lon, a.radius_km);
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(120))
        .user_agent("rotorscope-airspace-pack/0.1")
        .build();
    let mut net = Net {
        cache: a.cache.clone(),
        offline: a.offline,
        agent,
        downloaded: 0,
        from_cache: 0,
    };
    let mut b = PackBuilder {
        created: now,
        center: (a.lat, a.lon),
        radius_m: (a.radius_km * 1000.0) as u32,
        ..Default::default()
    };
    let source = |b: &mut PackBuilder, id: u8, date: u32, name: &str| {
        b.sources.push(SourceDef {
            id,
            date,
            fetched: now,
            name: name.to_string(),
        });
        println!("  source {id:>2}: {name} (data date {})", date_str(date));
    };
    let mut skipped: Vec<String> = Vec::new();

    // --- Class airspace -------------------------------------------------
    let (feats, date) = adds_layer(
        &mut net,
        &key,
        "Class_Airspace",
        "LOCAL_TYPE IN ('CLASS_B','CLASS_C','CLASS_D','CLASS_E2','CLASS_E3','CLASS_E4')",
        "IDENT,NAME,LOCAL_TYPE,UPPER_VAL,UPPER_CODE,UPPER_UOM,LOWER_VAL,LOWER_CODE,LOWER_UOM",
        true,
        0.0002,
        &rg,
    )?;
    source(&mut b, src::CLASS_AIRSPACE, date, "FAA ADDS Class_Airspace");
    for f in &feats {
        let p = &f["properties"];
        let kind = match p["LOCAL_TYPE"].as_str() {
            Some("CLASS_B") => AreaKind::ClassB,
            Some("CLASS_C") => AreaKind::ClassC,
            Some("CLASS_D") => AreaKind::ClassD,
            Some("CLASS_E2") => AreaKind::ClassE2,
            _ => AreaKind::ClassE4,
        };
        let rings = rings_of(&f["geometry"]);
        if rings.is_empty() {
            continue;
        }
        b.areas.push(area(
            kind,
            src::CLASS_AIRSPACE,
            alt(&p["LOWER_VAL"], &p["LOWER_CODE"], &p["LOWER_UOM"]),
            alt(&p["UPPER_VAL"], &p["UPPER_CODE"], &p["UPPER_UOM"]),
            text(&p["NAME"]),
            rings,
        ));
    }
    println!("  class airspace: {} areas", feats.len());

    // --- Special use: prohibited and restricted -------------------------
    let mut sua = 0;
    for (svc, filter, id) in [
        (
            "Special_Use_Airspace",
            "TYPE_CODE IN ('R','P')",
            src::SPECIAL_USE,
        ),
        ("Prohibited_Areas", "1=1", src::PROHIBITED),
    ] {
        let (feats, date) = adds_layer(
            &mut net,
            &key,
            svc,
            filter,
            "NAME,TYPE_CODE,UPPER_VAL,UPPER_CODE,UPPER_UOM,LOWER_VAL,LOWER_CODE,LOWER_UOM",
            true,
            0.0002,
            &rg,
        )?;
        source(&mut b, id, date, &format!("FAA ADDS {svc}"));
        for f in &feats {
            let p = &f["properties"];
            let kind = if p["TYPE_CODE"].as_str() == Some("P") || svc == "Prohibited_Areas" {
                AreaKind::Prohibited
            } else {
                AreaKind::Restricted
            };
            let rings = rings_of(&f["geometry"]);
            if rings.is_empty() {
                continue;
            }
            sua += 1;
            b.areas.push(area(
                kind,
                id,
                alt(&p["LOWER_VAL"], &p["LOWER_CODE"], &p["LOWER_UOM"]),
                alt(&p["UPPER_VAL"], &p["UPPER_CODE"], &p["UPPER_UOM"]),
                text(&p["NAME"]),
                rings,
            ));
        }
    }
    println!("  prohibited/restricted: {sua} areas");

    // --- National-security UAS restrictions -----------------------------
    for (svc, kind, id) in [
        (
            "DoD_Mar_13",
            AreaKind::NationalSecurity,
            src::NATIONAL_SECURITY,
        ),
        (
            "Part_Time_National_Security_UAS_Flight_Restrictions",
            AreaKind::NationalSecurityPartTime,
            src::NATIONAL_SECURITY_PART_TIME,
        ),
    ] {
        let (feats, date) = adds_layer(
            &mut net,
            &key,
            svc,
            "1=1",
            "Base,Facility,Floor,Ceiling",
            true,
            0.0001,
            &rg,
        )?;
        source(
            &mut b,
            id,
            date,
            &format!(
                "FAA ADDS {}",
                if svc == "DoD_Mar_13" {
                    "NatSec UAS restr"
                } else {
                    "NatSec UAS part-time"
                }
            ),
        );
        for f in &feats {
            let p = &f["properties"];
            let rings = rings_of(&f["geometry"]);
            if rings.is_empty() {
                continue;
            }
            let name = format!("{} {}", text(&p["Base"]), text(&p["Facility"]))
                .trim()
                .to_string();
            b.areas.push(area(
                kind,
                id,
                alt_text(p["Floor"].as_str().unwrap_or("Surface")),
                alt_text(p["Ceiling"].as_str().unwrap_or("400' AGL")),
                name,
                rings,
            ));
        }
        println!("  {svc}: {} areas", feats.len());
    }

    // --- National defense airspace (SFRA/FRZ) ---------------------------
    let (feats, date) = adds_layer(
        &mut net,
        &key,
        "National_Defense_Airspace_TFR_Areas",
        "1=1",
        "NAME",
        true,
        0.0002,
        &rg,
    )?;
    source(
        &mut b,
        src::NATIONAL_DEFENSE,
        date,
        "FAA ADDS Natl Defense Airspace",
    );
    for f in &feats {
        let rings = rings_of(&f["geometry"]);
        if !rings.is_empty() {
            b.areas.push(area(
                AreaKind::NationalDefense,
                src::NATIONAL_DEFENSE,
                (AltRef::Agl, 0),
                (AltRef::Msl, 18000),
                text(&f["properties"]["NAME"]),
                rings,
            ));
        }
    }
    println!("  national defense: {} areas", feats.len());

    // --- Stadiums: 3 NM, up to 3000 ft AGL, during events ---------------
    let (feats, date) = adds_layer(
        &mut net,
        &key,
        "Stadiums",
        "1=1",
        "NAME,STATUS_CODE",
        true,
        0.0,
        &rg,
    )?;
    source(&mut b, src::STADIUMS, date, "FAA ADDS Stadiums");
    for f in &feats {
        if let Some((la, lo)) = point_of(&f["geometry"]) {
            b.areas.push(area(
                AreaKind::Stadium,
                src::STADIUMS,
                (AltRef::Agl, 0),
                (AltRef::Agl, 3000),
                text(&f["properties"]["NAME"]),
                vec![circle(la, lo, 3.0 * NM, 48)],
            ));
        }
    }
    println!("  stadiums: {}", feats.len());

    // --- FRIAs -----------------------------------------------------------
    match adds_layer(
        &mut net,
        &key,
        "FAA_Recognized_Identification_Areas",
        "1=1",
        "title",
        true,
        0.00005,
        &rg,
    ) {
        Ok((feats, date)) => {
            source(&mut b, src::FRIA, date, "FAA ADDS FRIA");
            for f in &feats {
                let rings = rings_of(&f["geometry"]);
                if !rings.is_empty() {
                    b.areas.push(area(
                        AreaKind::Fria,
                        src::FRIA,
                        (AltRef::Agl, 0),
                        (AltRef::Agl, 400),
                        text(&f["properties"]["title"]),
                        rings,
                    ));
                }
            }
            println!("  FRIAs: {}", feats.len());
        }
        Err(e) => skipped.push(format!("FRIA: {e}")),
    }

    // --- TFRs (dated) ----------------------------------------------------
    match tfrs(&mut net, &rg, now) {
        Ok((areas, n_list)) => {
            source(&mut b, src::TFR, now, "tfr.faa.gov TFR list");
            println!(
                "  TFRs: {} in region of {n_list} nationwide (as of {})",
                areas.len(),
                date_str(now)
            );
            b.areas.extend(areas);
        }
        Err(e) => {
            skipped.push(format!("TFR: {e}"));
            println!(
                "  TFRs skipped. tfr.faa.gov refuses this client; to include TFRs, save today's files in a browser into {} as tfr-list-{day}.json (tfrapi/exportTfrList), tfr-wfs-{day}-{:.2}_{:.2}.json (the WFS URL above) and tfr-<id>-{day}.xml per TFR, then run again.",
                a.cache.display(),
                rg.lat_min,
                rg.lon_min,
                day = date_str(now)
            );
        }
    }
    b.areas.retain(|a| rg.overlaps(&a.rings));
    // Cut every shape to the region (plus a margin) and thin very detailed
    // ones (coastlines): lookups cost one step per edge.
    let clip = Region {
        lat_min: rg.lat_min - 0.05,
        lat_max: rg.lat_max + 0.05,
        lon_min: rg.lon_min - 0.05,
        lon_max: rg.lon_max + 0.05,
    };
    let before: usize = b
        .areas
        .iter()
        .map(|a| a.rings.iter().map(Vec::len).sum::<usize>())
        .sum();
    for a in &mut b.areas {
        let n: usize = a.rings.iter().map(Vec::len).sum();
        a.rings = a
            .rings
            .iter()
            .map(|r| clip_ring(r, &clip))
            .map(|r| if n > 400 { simplify(&r, 0.0004) } else { r })
            .filter(|r| r.len() >= 3)
            .collect();
    }
    b.areas.retain(|a| !a.rings.is_empty());
    let after: usize = b
        .areas
        .iter()
        .map(|a| a.rings.iter().map(Vec::len).sum::<usize>())
        .sum();
    println!("  shapes clipped to the region and thinned: {before} -> {after} points");

    // --- UAS Facility Map grid (30 arc-second cells) ---------------------
    let (feats, date) = adds_layer(
        &mut net,
        &key,
        "FAA_UAS_FacilityMap_Data",
        "1=1",
        "CEILING,LATITUDE,LONGITUDE",
        false,
        0.0,
        &rg,
    )?;
    source(
        &mut b,
        src::UAS_FACILITY_MAP,
        date,
        "FAA ADDS UAS Facility Map",
    );
    let cell = 1.0 / 120.0;
    let lat0 = (rg.lat_min / cell).floor() * cell;
    let lon0 = (rg.lon_min / cell).floor() * cell;
    let rows = ((rg.lat_max - lat0) / cell).ceil() as u32;
    let cols = ((rg.lon_max - lon0) / cell).ceil() as u32;
    let mut fm = GridDef {
        lat0,
        lon0,
        dlat: cell,
        dlon: cell,
        rows,
        cols,
        values: vec![UASFM_NONE; (rows * cols) as usize],
    };
    let mut placed = 0;
    for f in &feats {
        let p = &f["properties"];
        let (Some(la), Some(lo), Some(c)) = (
            num(&p["LATITUDE"]),
            num(&p["LONGITUDE"]),
            num(&p["CEILING"]),
        ) else {
            continue;
        };
        let (r, cc) = (((la - lat0) / cell).floor(), ((lo - lon0) / cell).floor());
        if r >= 0.0 && cc >= 0.0 && (r as u32) < rows && (cc as u32) < cols {
            fm.values[(r as u32 * cols + cc as u32) as usize] =
                (c / 10.0).round().clamp(0.0, 254.0) as u8;
            placed += 1;
        }
    }
    println!("  facility map: {placed} cells in a {rows}x{cols} grid");
    b.uasfm = Some(fm);

    // --- Terrain: USGS 3DEP, posts every `terrain_arcsec` ---------------
    let step = a.terrain_arcsec / 3600.0;
    let t_lat0 = (rg.lat_min / step).floor() * step;
    let t_lon0 = (rg.lon_min / step).floor() * step;
    let t_rows = ((rg.lat_max - t_lat0) / step).ceil() as u32 + 1;
    let t_cols = ((rg.lon_max - t_lon0) / step).ceil() as u32 + 1;
    let (terrain, nodata) = terrain(&mut net, &key, t_lat0, t_lon0, step, t_rows, t_cols)?;
    source(&mut b, src::TERRAIN, now, "USGS 3DEP elevation");
    let (tmin, tmax) = terrain
        .iter()
        .fold((i16::MAX, i16::MIN), |(a, b), &v| (a.min(v), b.max(v)));
    println!(
        "  terrain: {t_rows}x{t_cols} posts at {} arcsec, {tmin}..{tmax} m, {nodata} no-data posts set to 0 m (water)",
        a.terrain_arcsec
    );
    b.terrain = Some(GridDef {
        lat0: t_lat0,
        lon0: t_lon0,
        dlat: step,
        dlon: step,
        rows: t_rows,
        cols: t_cols,
        values: terrain,
    });

    // --- Geoid: NGS GEOID18 on a 5 x 5 grid ------------------------------
    let mut geoid = Vec::new();
    let (glat, glon) = (
        (rg.lat_max - rg.lat_min) / 4.0,
        (rg.lon_max - rg.lon_min) / 4.0,
    );
    for r in 0..5 {
        for c in 0..5 {
            let (la, lo) = (
                rg.lat_min + glat * f64::from(r),
                rg.lon_min + glon * f64::from(c),
            );
            let v = net.json(
                &format!("https://geodesy.noaa.gov/api/geoid/ght?lat={la:.5}&lon={lo:.5}"),
                &format!("geoid-{la:.5}_{lo:.5}.json"),
            )?;
            let h = v["geoidHeight"]
                .as_f64()
                .ok_or_else(|| format!("geoid at {la},{lo}: {v}"))?;
            geoid.push((h * 100.0).round() as i16);
        }
    }
    source(&mut b, src::GEOID, 1_546_300_800, "NOAA NGS GEOID18");
    println!(
        "  geoid: {:.2}..{:.2} m",
        f64::from(*geoid.iter().min().unwrap()) / 100.0,
        f64::from(*geoid.iter().max().unwrap()) / 100.0
    );
    b.geoid_cm = Some(GridDef {
        lat0: rg.lat_min,
        lon0: rg.lon_min,
        dlat: glat,
        dlon: glon,
        rows: 5,
        cols: 5,
        values: geoid,
    });

    // --- Urban areas: TIGERweb, rasterized to the terrain cells ---------
    match urban(&mut net, &key, &rg) {
        Ok((rings, n)) => {
            let mut g = GridDef {
                lat0: t_lat0,
                lon0: t_lon0,
                dlat: step,
                dlon: step,
                rows: t_rows,
                cols: t_cols,
                values: vec![false; (t_rows * t_cols) as usize],
            };
            for r in &rings {
                rasterize(&mut g, r);
            }
            let set = g.values.iter().filter(|&&v| v).count();
            println!(
                "  urban: {n} areas, {:.0} % of cells",
                100.0 * set as f64 / g.values.len() as f64
            );
            source(
                &mut b,
                src::URBAN,
                1_577_836_800,
                "Census TIGERweb 2020 Urban",
            );
            b.urban = Some(g);
        }
        Err(e) => skipped.push(format!("urban: {e}")),
    }

    // --- Airports ----------------------------------------------------------
    let (feats, date) = adds_layer(
        &mut net,
        &key,
        "US_Airport",
        "1=1",
        "IDENT,TYPE_CODE,PRIVATEUSE,IAPEXISTS,ELEVATION,OPERSTATUS",
        true,
        0.0,
        &rg,
    )?;
    source(&mut b, src::AIRPORTS, date, "FAA ADDS US_Airport");
    for f in &feats {
        let p = &f["properties"];
        let Some((la, lo)) = point_of(&f["geometry"]) else {
            continue;
        };
        if p["OPERSTATUS"]
            .as_str()
            .is_some_and(|s| s.contains("CLOSED"))
        {
            continue;
        }
        let kind = match p["TYPE_CODE"].as_str() {
            Some("AD") => 0,
            Some("HP") => 1,
            Some("SP") => 2,
            _ => 3,
        };
        let flags = u8::from(num(&p["PRIVATEUSE"]) == Some(0.0))
            | (u8::from(num(&p["IAPEXISTS"]) == Some(1.0)) << 1);
        b.airports.push(AirportDef {
            lat: la,
            lon: lo,
            elev_ft: num(&p["ELEVATION"]).unwrap_or(0.0) as i16,
            kind,
            flags,
            ident: text(&p["IDENT"]),
        });
    }
    println!("  airports: {}", b.airports.len());

    // --- Write and check ---------------------------------------------------
    let bytes = b.to_bytes();
    let p = Pack::parse(&bytes).map_err(|e| format!("own pack does not parse: {e:?}"))?;
    std::fs::write(&a.out, &bytes).map_err(|e| format!("{}: {e}", a.out.display()))?;
    println!(
        "wrote {} ({} bytes, {} areas, {} points); downloaded {} bytes, {} bytes from cache",
        a.out.display(),
        bytes.len(),
        p.areas().count(),
        p.areas().map(|a| a.n_points()).sum::<usize>(),
        net.downloaded,
        net.from_cache
    );
    for s in &skipped {
        println!("skipped: {s}");
    }
    Ok(())
}

/// Active TFRs in the region: (areas, TFRs in the national list).
fn tfrs(net: &mut Net, rg: &Region, now: u32) -> Res<(Vec<AreaDef>, usize)> {
    let day = date_str(now);
    // The national list says which NOTAMs are current. Without it, the
    // shapes service's own entries are filtered by their XML end time.
    let active: Option<HashSet<String>> = match net.json(
        "https://tfr.faa.gov/tfrapi/exportTfrList",
        &format!("tfr-list-{day}.json"),
    ) {
        Ok(list) => Some(
            list.as_array()
                .into_iter()
                .flatten()
                .filter_map(|t| t["notam_id"].as_str().map(str::to_string))
                .collect(),
        ),
        Err(e) => {
            println!(
                "  TFR list unavailable ({}); filtering shapes by their end time",
                e.chars().take(80).collect::<String>()
            );
            None
        }
    };
    let wfs = format!(
        "https://tfr.faa.gov/geoserver/TFR/ows?service=WFS&version=1.0.0&request=GetFeature&typeName=TFR:V_TFR_LOC&outputFormat=application/json&bbox={}",
        rg.envelope()
    );
    let shapes = net.json(
        &wfs,
        &format!("tfr-wfs-{day}-{:.2}_{:.2}.json", rg.lat_min, rg.lon_min),
    )?;
    let mut out = Vec::new();
    for f in shapes["features"].as_array().into_iter().flatten() {
        let key = f["properties"]["NOTAM_KEY"].as_str().unwrap_or("");
        let id = key.split('-').next().unwrap_or("");
        if active.as_ref().is_some_and(|a| !a.contains(id)) {
            continue;
        }
        let rings = rings_of(&f["geometry"]);
        if rings.is_empty() || !rg.overlaps(&rings) {
            continue;
        }
        let xml = net.get(
            &format!(
                "https://tfr.faa.gov/download/detail_{}.xml",
                id.replace('/', "_")
            ),
            &format!("tfr-{}-{day}.xml", id.replace('/', "_")),
        )?;
        let xml = String::from_utf8_lossy(&xml);
        let tz = tag(&xml, "codeTimeZone").unwrap_or("UTC");
        let start = tag(&xml, "dateEffective")
            .and_then(|s| xml_time(s, tz))
            .unwrap_or(0);
        let end = tag(&xml, "dateExpire")
            .and_then(|s| xml_time(s, tag(&xml, "codeExpirationTimeZone").unwrap_or(tz)))
            .unwrap_or(0);
        let lim = |code: &str, val: &str| -> (AltRef, i32) {
            let v = tag(&xml, val).and_then(|s| s.parse::<i32>().ok());
            match (tag(&xml, code), v) {
                (Some("HEI"), Some(v)) => (AltRef::Agl, v),
                (_, Some(v)) => (AltRef::Msl, v),
                _ => (AltRef::Unlimited, 0),
            }
        };
        let floor = lim("codeDistVerLower", "valDistVerLower");
        let ceil = lim("codeDistVerUpper", "valDistVerUpper");
        let title = f["properties"]["TITLE"].as_str().unwrap_or(id);
        if end != 0 && end < now {
            continue;
        }
        let mut a = area(
            AreaKind::Tfr,
            src::TFR,
            floor,
            ceil,
            format!("TFR {id} {title}"),
            rings,
        );
        a.start = start;
        a.end = end;
        out.push(a);
    }
    Ok((out, active.map_or(0, |a| a.len())))
}

/// 3DEP posts (row 0 south), and how many were no-data.
fn terrain(
    net: &mut Net,
    key: &str,
    lat0: f64,
    lon0: f64,
    step: f64,
    rows: u32,
    cols: u32,
) -> Res<(Vec<i16>, usize)> {
    // Pixel centres on the posts: the box extends half a pixel beyond them.
    let bbox = format!(
        "{:.7},{:.7},{:.7},{:.7}",
        lon0 - step / 2.0,
        lat0 - step / 2.0,
        lon0 + (f64::from(cols) - 0.5) * step,
        lat0 + (f64::from(rows) - 0.5) * step
    );
    let url = format!(
        "https://elevation.nationalmap.gov/arcgis/rest/services/3DEPElevation/ImageServer/exportImage?bbox={bbox}&bboxSR=4326&imageSR=4326&size={cols},{rows}&format=tiff&pixelType=F32&interpolation=RSP_BilinearInterpolation&noData=-9999&f=image"
    );
    let tif = net.get(&url, &format!("3dep-{key}-{rows}x{cols}.tif"))?;
    let mut dec = tiff::decoder::Decoder::new(std::io::Cursor::new(&tif)).map_err(|e| {
        format!(
            "3DEP tiff: {e} ({} bytes: {:?})",
            tif.len(),
            String::from_utf8_lossy(&tif[..tif.len().min(200)])
        )
    })?;
    let (w, h) = dec.dimensions().map_err(|e| e.to_string())?;
    if (w, h) != (cols, rows) {
        return Err(format!("3DEP returned {w}x{h}, asked {cols}x{rows}"));
    }
    let data = match dec.read_image().map_err(|e| e.to_string())? {
        tiff::decoder::DecodingResult::F32(v) => v,
        _ => return Err("3DEP: not float32".into()),
    };
    let mut out = vec![0i16; (rows * cols) as usize];
    let mut nodata = 0;
    for i in 0..rows {
        for c in 0..cols {
            let v = data[(i * cols + c) as usize];
            let r = rows - 1 - i;
            out[(r * cols + c) as usize] = if v.is_finite() && v > -1000.0 {
                // Below sea level here is river and bay bottom (3DEP
                // includes some bathymetry): a drone over water is above
                // the water surface, so clamp to 0.
                v.round().clamp(0.0, 9000.0) as i16
            } else {
                nodata += 1;
                0
            };
        }
    }
    Ok((out, nodata))
}

/// Census 2020 urban-area polygons in the region (simplified ~200 m).
fn urban(net: &mut Net, key: &str, rg: &Region) -> Res<(Vec<Vec<Vec<(f64, f64)>>>, usize)> {
    let mut out = Vec::new();
    for p in 0.. {
        let url = format!(
            "https://tigerweb.geo.census.gov/arcgis/rest/services/TIGERweb/Urban/MapServer/2/query?where=1%3D1&geometry={}&geometryType=esriGeometryEnvelope&inSR=4326&spatialRel=esriSpatialRelIntersects&outFields=NAME&returnGeometry=true&outSR=4326&maxAllowableOffset=0.002&geometryPrecision=5&resultOffset={}&resultRecordCount=100&f=geojson",
            rg.envelope(),
            p * 100
        );
        let v = net.json(&url, &format!("urban-{key}-p{p}.json"))?;
        let feats = v["features"].as_array().cloned().unwrap_or_default();
        let n = feats.len();
        out.extend(
            feats
                .iter()
                .map(|f| rings_of(&f["geometry"]))
                .filter(|r| !r.is_empty()),
        );
        if n < 100 {
            break;
        }
    }
    let n = out.len();
    Ok((out, n))
}
