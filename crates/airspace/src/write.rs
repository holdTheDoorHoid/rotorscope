//! Building a pack (host side: `tools/airspace-pack` and tests).

use alloc::string::String;
use alloc::vec::Vec;

use crate::format::*;
use crate::pack::{crc32, pad4};

/// A data source, for the source table.
#[derive(Debug, Clone)]
pub struct SourceDef {
    pub id: u8,
    pub date: u32,
    pub fetched: u32,
    pub name: String,
}

/// One area to write. Points are (lat, lon) degrees; `rings` are lengths.
#[derive(Debug, Clone)]
pub struct AreaDef {
    pub kind: AreaKind,
    pub source: u8,
    pub floor_ref: AltRef,
    pub floor_ft: i32,
    pub ceil_ref: AltRef,
    pub ceil_ft: i32,
    pub start: u32,
    pub end: u32,
    pub flags: u8,
    pub name: String,
    pub rings: Vec<Vec<(f64, f64)>>,
}

/// A grid to write: posts or cells from (`lat0`, `lon0`) degrees, spacing
/// `dlat`/`dlon` degrees, row 0 south, values row-major.
#[derive(Debug, Clone)]
pub struct GridDef<T> {
    pub lat0: f64,
    pub lon0: f64,
    pub dlat: f64,
    pub dlon: f64,
    pub rows: u32,
    pub cols: u32,
    pub values: Vec<T>,
}

#[derive(Debug, Clone)]
pub struct AirportDef {
    pub lat: f64,
    pub lon: f64,
    pub elev_ft: i16,
    pub kind: u8,
    pub flags: u8,
    pub ident: String,
}

/// Everything a pack holds. Fill it, then [`PackBuilder::to_bytes`].
#[derive(Debug, Clone, Default)]
pub struct PackBuilder {
    pub created: u32,
    pub center: (f64, f64),
    pub radius_m: u32,
    pub sources: Vec<SourceDef>,
    pub areas: Vec<AreaDef>,
    pub uasfm: Option<GridDef<u8>>,
    pub terrain: Option<GridDef<i16>>,
    pub geoid_cm: Option<GridDef<i16>>,
    pub urban: Option<GridDef<bool>>,
    pub airports: Vec<AirportDef>,
}

fn e7(d: f64) -> i32 {
    libm::round(d * 1e7) as i32
}

fn grid_head<T>(out: &mut Vec<u8>, g: &GridDef<T>) {
    for v in [e7(g.lat0), e7(g.lon0), e7(g.dlat), e7(g.dlon)] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&g.rows.to_le_bytes());
    out.extend_from_slice(&g.cols.to_le_bytes());
}

fn align(out: &mut Vec<u8>) {
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

impl PackBuilder {
    fn areas_section(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(self.areas.len() as u32).to_le_bytes());
        for a in &self.areas {
            let pts: Vec<(i32, i32)> = a
                .rings
                .iter()
                .flatten()
                .map(|&(la, lo)| (e7(la), e7(lo)))
                .collect();
            let (mut la0, mut la1, mut lo0, mut lo1) = (i32::MAX, i32::MIN, i32::MAX, i32::MIN);
            for &(la, lo) in &pts {
                la0 = la0.min(la);
                la1 = la1.max(la);
                lo0 = lo0.min(lo);
                lo1 = lo1.max(lo);
            }
            let name = &a.name.as_bytes()[..a.name.len().min(64)];
            out.extend_from_slice(&[a.kind as u8, a.source, a.floor_ref as u8, a.ceil_ref as u8]);
            out.extend_from_slice(&a.floor_ft.to_le_bytes());
            out.extend_from_slice(&a.ceil_ft.to_le_bytes());
            out.extend_from_slice(&a.start.to_le_bytes());
            out.extend_from_slice(&a.end.to_le_bytes());
            for v in [la0, la1, lo0, lo1] {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.extend_from_slice(&(pts.len() as u16).to_le_bytes());
            out.extend_from_slice(&(a.rings.len() as u16).to_le_bytes());
            out.extend_from_slice(&[name.len() as u8, a.flags, 0, 0]);
            out.extend_from_slice(name);
            out.resize(out.len() + pad4(name.len()) - name.len(), 0);
            for r in &a.rings {
                out.extend_from_slice(&(r.len() as u16).to_le_bytes());
            }
            align(&mut out);
            for (la, lo) in pts {
                out.extend_from_slice(&la.to_le_bytes());
                out.extend_from_slice(&lo.to_le_bytes());
            }
        }
        out
    }

    /// The finished pack.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut sections: Vec<(Section, Vec<u8>)> = Vec::new();
        if !self.areas.is_empty() {
            sections.push((Section::Areas, self.areas_section()));
        }
        if let Some(g) = &self.uasfm {
            let mut s = Vec::new();
            grid_head(&mut s, g);
            s.extend_from_slice(&g.values);
            sections.push((Section::Uasfm, s));
        }
        for (kind, g) in [
            (Section::Terrain, &self.terrain),
            (Section::Geoid, &self.geoid_cm),
        ] {
            if let Some(g) = g {
                let mut s = Vec::new();
                grid_head(&mut s, g);
                for v in &g.values {
                    s.extend_from_slice(&v.to_le_bytes());
                }
                sections.push((kind, s));
            }
        }
        if let Some(g) = &self.urban {
            let mut s = Vec::new();
            grid_head(&mut s, g);
            let mut bits = alloc::vec![0u8; g.values.len().div_ceil(8)];
            for (i, &b) in g.values.iter().enumerate() {
                if b {
                    bits[i / 8] |= 1 << (i % 8);
                }
            }
            s.extend_from_slice(&bits);
            sections.push((Section::Urban, s));
        }
        if !self.airports.is_empty() {
            let mut s = Vec::new();
            s.extend_from_slice(&(self.airports.len() as u32).to_le_bytes());
            for a in &self.airports {
                s.extend_from_slice(&e7(a.lat).to_le_bytes());
                s.extend_from_slice(&e7(a.lon).to_le_bytes());
                s.extend_from_slice(&a.elev_ft.to_le_bytes());
                s.extend_from_slice(&[a.kind, a.flags]);
                let mut id = [0u8; 8];
                let n = a.ident.len().min(8);
                id[..n].copy_from_slice(&a.ident.as_bytes()[..n]);
                s.extend_from_slice(&id);
            }
            sections.push((Section::Airports, s));
        }

        let mut out = alloc::vec![0u8; HEADER_LEN];
        for src in &self.sources {
            let mut rec = [0u8; SOURCE_LEN];
            rec[0] = src.id;
            rec[4..8].copy_from_slice(&src.date.to_le_bytes());
            rec[8..12].copy_from_slice(&src.fetched.to_le_bytes());
            let n = src.name.len().min(SOURCE_NAME_LEN - 1);
            rec[12..12 + n].copy_from_slice(&src.name.as_bytes()[..n]);
            out.extend_from_slice(&rec);
        }
        let table_at = out.len();
        out.resize(table_at + sections.len() * SECTION_LEN, 0);
        align(&mut out);
        for (i, (kind, data)) in sections.iter().enumerate() {
            align(&mut out);
            let off = out.len() as u32;
            out.extend_from_slice(data);
            let t = table_at + i * SECTION_LEN;
            out[t..t + 2].copy_from_slice(&(*kind as u16).to_le_bytes());
            out[t + 4..t + 8].copy_from_slice(&off.to_le_bytes());
            out[t + 8..t + 12].copy_from_slice(&(data.len() as u32).to_le_bytes());
        }
        align(&mut out);
        let total = out.len() as u32;
        out[0..4].copy_from_slice(&MAGIC);
        out[4..6].copy_from_slice(&VERSION.to_le_bytes());
        out[6..8].copy_from_slice(&(HEADER_LEN as u16).to_le_bytes());
        out[8..12].copy_from_slice(&self.created.to_le_bytes());
        out[12..16].copy_from_slice(&e7(self.center.0).to_le_bytes());
        out[16..20].copy_from_slice(&e7(self.center.1).to_le_bytes());
        out[20..24].copy_from_slice(&self.radius_m.to_le_bytes());
        out[28..30].copy_from_slice(&(self.sources.len() as u16).to_le_bytes());
        out[30..32].copy_from_slice(&(sections.len() as u16).to_le_bytes());
        out[32..36].copy_from_slice(&total.to_le_bytes());
        let crc = crc32(&out[HEADER_LEN..]);
        out[36..40].copy_from_slice(&crc.to_le_bytes());
        out
    }
}

/// A circle as a polygon ring: `n` points around (lat, lon), radius metres.
pub fn circle(lat: f64, lon: f64, radius_m: f64, n: usize) -> Vec<(f64, f64)> {
    let dlat = radius_m / crate::geom::EARTH_R * (180.0 / core::f64::consts::PI);
    let dlon = dlat / libm::cos(crate::geom::rad(lat));
    (0..n)
        .map(|i| {
            let a = 2.0 * core::f64::consts::PI * i as f64 / n as f64;
            (lat + dlat * libm::cos(a), lon + dlon * libm::sin(a))
        })
        .collect()
}
