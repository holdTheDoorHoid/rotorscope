//! Reading a pack in place (no copies, no allocation).

use crate::format::*;
use crate::geom::{EARTH_R, M_PER_FT, distance_m, point_in_rings};

/// Why a pack was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackError {
    TooShort,
    BadMagic,
    /// A layout this build does not read (the pack's version).
    Version(u16),
    /// The CRC does not match: the file is damaged or incomplete.
    Crc {
        stored: u32,
        computed: u32,
    },
    /// A section or record points outside the file.
    Bounds,
}

fn u16_at(d: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([d[o], d[o + 1]])
}
fn i16_at(d: &[u8], o: usize) -> i16 {
    i16::from_le_bytes([d[o], d[o + 1]])
}
fn u32_at(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}
fn i32_at(d: &[u8], o: usize) -> i32 {
    i32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}
pub(crate) fn pad4(n: usize) -> usize {
    (n + 3) & !3
}

/// CRC-32 (IEEE 802.3, as zip and PNG), bitwise: a 1 MB pack takes well
/// under a second on the P4, once, at load.
pub fn crc32(data: &[u8]) -> u32 {
    crc32_update(0xFFFF_FFFF, data) ^ 0xFFFF_FFFF
}

/// Running CRC-32: start with `0xFFFF_FFFF`, finish with `^ 0xFFFF_FFFF`.
pub fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    crc
}

/// Where a pack's data came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Source<'a> {
    pub id: u8,
    /// The data's own date (edition, last edit or issue), Unix seconds.
    pub date: u32,
    /// When the tool downloaded it, Unix seconds.
    pub fetched: u32,
    pub name: &'a str,
}

/// A gridded layer: `rows` x `cols` values starting at (`lat0`, `lon0`)
/// (the south-west post or cell corner), `dlat`/`dlon` apart (1e-7 deg).
#[derive(Debug, Clone, Copy)]
pub struct Grid<'a> {
    pub lat0: i32,
    pub lon0: i32,
    pub dlat: i32,
    pub dlon: i32,
    pub rows: u32,
    pub cols: u32,
    data: &'a [u8],
}

/// Bytes a `rows` x `cols` grid of `elem_bits`-wide cells needs. Done in u64
/// (the hub's usize is 32 bits, where 65536 x 65536 wraps to 0).
pub(crate) fn grid_bytes_needed(rows: u32, cols: u32, elem_bits: u64) -> Option<u64> {
    u64::from(rows)
        .checked_mul(u64::from(cols))?
        .checked_mul(elem_bits)
        .map(|bits| bits.div_ceil(8))
}

impl<'a> Grid<'a> {
    fn parse(d: &'a [u8], elem_bits: usize) -> Option<Self> {
        if d.len() < GRID_HEAD_LEN {
            return None;
        }
        let g = Grid {
            lat0: i32_at(d, 0),
            lon0: i32_at(d, 4),
            dlat: i32_at(d, 8),
            dlon: i32_at(d, 12),
            rows: u32_at(d, 16),
            cols: u32_at(d, 20),
            data: &d[GRID_HEAD_LEN..],
        };
        // The file is hostile: a size that overflows is a size it cannot hold.
        let need = grid_bytes_needed(g.rows, g.cols, elem_bits as u64)?;
        (g.dlat > 0 && g.dlon > 0 && g.data.len() as u64 >= need).then_some(g)
    }

    /// Fractional (row, col) of a position, posts at integer values.
    fn frac(&self, lat: f64, lon: f64) -> (f64, f64) {
        let r = (lat * 1e7 - f64::from(self.lat0)) / f64::from(self.dlat);
        let c = (lon * 1e7 - f64::from(self.lon0)) / f64::from(self.dlon);
        (r, c)
    }

    /// The cell a position falls in (cell (r, c) spans r..r+1, c..c+1).
    pub fn cell(&self, lat: f64, lon: f64) -> Option<(u32, u32)> {
        let (r, c) = self.frac(lat, lon);
        if r < 0.0 || c < 0.0 || r >= f64::from(self.rows) || c >= f64::from(self.cols) {
            return None;
        }
        Some((r as u32, c as u32))
    }

    /// Cell index, `None` outside the grid (u64: no wrap on a 32-bit usize).
    fn idx(&self, r: u32, c: u32) -> Option<u64> {
        if r >= self.rows || c >= self.cols {
            return None;
        }
        Some(u64::from(r) * u64::from(self.cols) + u64::from(c))
    }

    /// Outside the grid, or past the data: [`UASFM_NONE`], never a panic.
    pub fn u8_at(&self, r: u32, c: u32) -> u8 {
        let at = self.idx(r, c).and_then(|i| usize::try_from(i).ok());
        at.and_then(|i| self.data.get(i))
            .copied()
            .unwrap_or(UASFM_NONE)
    }

    /// Outside the grid, or past the data: [`TERRAIN_NONE`], never a panic.
    pub fn i16_at(&self, r: u32, c: u32) -> i16 {
        let at = self
            .idx(r, c)
            .and_then(|i| usize::try_from(i.checked_mul(2)?).ok());
        at.and_then(|i| self.data.get(i..i.checked_add(2)?))
            .map_or(TERRAIN_NONE, |b| i16::from_le_bytes([b[0], b[1]]))
    }

    /// Outside the grid, or past the data: false, never a panic.
    pub fn bit_at(&self, r: u32, c: u32) -> bool {
        let Some(i) = self.idx(r, c) else {
            return false;
        };
        usize::try_from(i / 8)
            .ok()
            .and_then(|b| self.data.get(b))
            .is_some_and(|&b| b & (1 << (i % 8)) != 0)
    }

    /// Bilinear between the four i16 posts around a position, scaled by
    /// `scale`. `None` outside the grid or next to a no-data post.
    pub fn bilinear_i16(&self, lat: f64, lon: f64, scale: f32) -> Option<f32> {
        let (r, c) = self.frac(lat, lon);
        let (rmax, cmax) = (f64::from(self.rows) - 1.0, f64::from(self.cols) - 1.0);
        if !(0.0..=rmax).contains(&r) || !(0.0..=cmax).contains(&c) {
            return None;
        }
        let (r0, c0) = (libm::floor(r) as u32, libm::floor(c) as u32);
        let (r1, c1) = ((r0 + 1).min(self.rows - 1), (c0 + 1).min(self.cols - 1));
        let (fr, fc) = ((r - f64::from(r0)) as f32, (c - f64::from(c0)) as f32);
        let v = [
            self.i16_at(r0, c0),
            self.i16_at(r0, c1),
            self.i16_at(r1, c0),
            self.i16_at(r1, c1),
        ];
        if v.contains(&TERRAIN_NONE) {
            return None;
        }
        let f = |x: i16| f32::from(x) * scale;
        let south = f(v[0]) + (f(v[1]) - f(v[0])) * fc;
        let north = f(v[2]) + (f(v[3]) - f(v[2])) * fc;
        Some(south + (north - south) * fr)
    }

    /// Post spacing in metres at this grid's latitude (north-south, east-west).
    pub fn spacing_m(&self) -> (f64, f64) {
        let lat = (f64::from(self.lat0) + f64::from(self.dlat) * f64::from(self.rows) / 2.0) * 1e-7;
        let ns = crate::geom::rad(f64::from(self.dlat) * 1e-7) * EARTH_R;
        (
            ns,
            ns * libm::cos(crate::geom::rad(lat)) * f64::from(self.dlon) / f64::from(self.dlat),
        )
    }
}

/// One airspace area.
#[derive(Debug, Clone, Copy)]
pub struct Area<'a> {
    pub kind: AreaKind,
    pub source: u8,
    pub floor_ref: AltRef,
    pub floor_ft: i32,
    pub ceil_ref: AltRef,
    pub ceil_ft: i32,
    /// Valid from / until (Unix s); 0 = always (permanent areas).
    pub start: u32,
    pub end: u32,
    /// Bounding box, 1e-7 deg.
    pub lat_min: i32,
    pub lat_max: i32,
    pub lon_min: i32,
    pub lon_max: i32,
    pub flags: u8,
    pub name: &'a str,
    rings: &'a [u8],
    points: &'a [u8],
}

impl<'a> Area<'a> {
    pub fn n_points(&self) -> usize {
        self.points.len() / 8
    }

    /// Point `i` as (lat, lon); (0, 0) past the end. Parsing rejects areas
    /// whose rings claim more points than they hold, so that is unreachable
    /// for a pack that passed [`Pack::parse`]; this keeps it a wrong answer
    /// rather than a panic if it ever is reached.
    pub fn point(&self, i: usize) -> (i32, i32) {
        i.checked_mul(8)
            .and_then(|o| self.points.get(o..o.checked_add(8)?))
            .map_or((0, 0), |b| {
                (
                    i32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                    i32::from_le_bytes([b[4], b[5], b[6], b[7]]),
                )
            })
    }

    fn ring_lens(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.rings.len() / 2).map(|i| usize::from(u16_at(self.rings, 2 * i)))
    }

    /// Inside the shape (ignoring altitude and time).
    pub fn contains(&self, lat: f64, lon: f64) -> bool {
        let (la, lo) = ((lat * 1e7) as i32, (lon * 1e7) as i32);
        if la < self.lat_min || la > self.lat_max || lo < self.lon_min || lo > self.lon_max {
            return false;
        }
        point_in_rings(la, lo, self.ring_lens(), |i| self.point(i))
    }

    /// Distance from the position to the nearest edge of the shape, metres
    /// (local flat projection: fine at airspace scales).
    pub fn edge_distance_m(&self, lat: f64, lon: f64) -> f64 {
        let kx = libm::cos(crate::geom::rad(lat)) * EARTH_R * 1e-7 * core::f64::consts::PI / 180.0;
        let ky = EARTH_R * 1e-7 * core::f64::consts::PI / 180.0;
        let (py, px) = (lat * 1e7, lon * 1e7);
        let xy = |i: usize| {
            let (la, lo) = self.point(i);
            ((f64::from(lo) - px) * kx, (f64::from(la) - py) * ky)
        };
        let mut best = f64::INFINITY;
        let mut start = 0;
        for len in self.ring_lens() {
            for k in 0..len {
                let (ax, ay) = xy(start + k);
                let (bx, by) = xy(start + (k + 1) % len);
                let (dx, dy) = (bx - ax, by - ay);
                let l2 = dx * dx + dy * dy;
                let t = if l2 > 0.0 {
                    (-(ax * dx + ay * dy) / l2).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let (cx, cy) = (ax + t * dx, ay + t * dy);
                best = best.min(cx * cx + cy * cy);
            }
            start += len;
        }
        libm::sqrt(best)
    }

    /// In force at `utc_s` (always true for permanent areas). With no clock,
    /// a dated area counts as in force (the caller shows that it guessed).
    pub fn active_at(&self, utc_s: Option<u32>) -> bool {
        match utc_s {
            None => true,
            Some(t) => (self.start == 0 || t >= self.start) && (self.end == 0 || t < self.end),
        }
    }

    /// Floor and ceiling in metres MSL at a place whose ground is
    /// `ground_msl_m` (needed for AGL limits; `None` makes an AGL limit
    /// unknown). Unlimited ceilings are `f64::INFINITY`.
    pub fn limits_msl_m(&self, ground_msl_m: Option<f64>) -> (Option<f64>, Option<f64>) {
        let conv = |r: AltRef, ft: i32, unl: f64| match r {
            AltRef::Msl => Some(f64::from(ft) * M_PER_FT),
            AltRef::Agl => ground_msl_m.map(|g| g + f64::from(ft) * M_PER_FT),
            AltRef::Unlimited => Some(unl),
        };
        (
            conv(self.floor_ref, self.floor_ft, f64::NEG_INFINITY),
            conv(self.ceil_ref, self.ceil_ft, f64::INFINITY),
        )
    }

    /// Whether an altitude (metres MSL) is between floor and ceiling, given
    /// the ground there. A floor at the surface (0 AGL) contains everything
    /// below the ceiling even when the ground is unknown.
    pub fn contains_alt(&self, alt_msl_m: f64, ground_msl_m: Option<f64>) -> Option<bool> {
        let (floor, ceil) = self.limits_msl_m(ground_msl_m);
        let floor = if self.floor_ref == AltRef::Agl && self.floor_ft <= 0 {
            Some(f64::NEG_INFINITY)
        } else {
            floor
        };
        Some(alt_msl_m >= floor? && alt_msl_m <= ceil?)
    }
}

/// An airport or heliport point.
#[derive(Debug, Clone, Copy)]
pub struct Airport<'a> {
    pub lat: f64,
    pub lon: f64,
    pub elev_ft: i16,
    /// 0 airport, 1 heliport, 2 seaplane base, 3 other.
    pub kind: u8,
    /// Bit 0: public use. Bit 1: has an instrument approach.
    pub flags: u8,
    pub ident: &'a str,
}

/// Terrain between two points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Los {
    /// Highest terrain above the sight line, metres (negative: the line
    /// clears all terrain by that much).
    pub worst_m: f32,
    /// Where along the line that was, 0..1 from the first point.
    pub at: f32,
    /// Samples checked.
    pub samples: u16,
}

/// A parsed pack. Cheap to copy; borrows the bytes.
#[derive(Debug, Clone, Copy)]
pub struct Pack<'a> {
    data: &'a [u8],
    pub version: u16,
    pub created: u32,
    pub center_lat: f64,
    pub center_lon: f64,
    pub radius_m: u32,
    n_sources: usize,
    n_sections: usize,
    header_len: usize,
    /// Optional bounding-box index ([`Pack::with_index`]).
    index: Option<&'a [AreaRef]>,
}

/// One entry of an area index: where the record is and its bounding box.
/// A hub keeps the index in fast RAM ([`Pack::build_index`]) so a lookup
/// reads only the records whose box contains the point, not every header
/// (the pack itself may sit in slow external RAM).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AreaRef {
    pub off: u32,
    pub lat_min: i32,
    pub lat_max: i32,
    pub lon_min: i32,
    pub lon_max: i32,
    pub kind: u8,
}

const MAX_NAME: usize = 64;

fn str_of(b: &[u8]) -> &str {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    core::str::from_utf8(&b[..end]).unwrap_or("?")
}

impl<'a> Pack<'a> {
    /// Check the header and CRC. Every later read is bounds-checked against
    /// what this accepted.
    pub fn parse(data: &'a [u8]) -> Result<Self, PackError> {
        if data.len() < HEADER_LEN {
            return Err(PackError::TooShort);
        }
        if data[..4] != MAGIC {
            return Err(PackError::BadMagic);
        }
        let version = u16_at(data, 4);
        if version != VERSION {
            return Err(PackError::Version(version));
        }
        let header_len = usize::from(u16_at(data, 6));
        let total = u32_at(data, 32) as usize;
        if header_len < HEADER_LEN || total > data.len() || total < header_len {
            return Err(PackError::TooShort);
        }
        let data = &data[..total];
        let stored = u32_at(data, 36);
        let computed = crc32(&data[header_len..]);
        if stored != computed {
            return Err(PackError::Crc { stored, computed });
        }
        let p = Pack {
            data,
            version,
            created: u32_at(data, 8),
            center_lat: f64::from(i32_at(data, 12)) * 1e-7,
            center_lon: f64::from(i32_at(data, 16)) * 1e-7,
            radius_m: u32_at(data, 20),
            n_sources: usize::from(u16_at(data, 28)),
            n_sections: usize::from(u16_at(data, 30)),
            header_len,
            index: None,
        };
        if p.sections_off() + p.n_sections * SECTION_LEN > total {
            return Err(PackError::Bounds);
        }
        for i in 0..p.n_sections {
            let o = p.sections_off() + i * SECTION_LEN;
            let (off, len) = (u32_at(data, o + 4) as usize, u32_at(data, o + 8) as usize);
            if off.checked_add(len).is_none_or(|e| e > total) {
                return Err(PackError::Bounds);
            }
        }
        p.check_areas()?;
        Ok(p)
    }

    /// Total length in bytes.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    fn sources_off(&self) -> usize {
        self.header_len
    }

    fn sections_off(&self) -> usize {
        self.header_len + self.n_sources * SOURCE_LEN
    }

    pub fn sources(&self) -> impl Iterator<Item = Source<'a>> + '_ {
        let d = self.data;
        let base = self.sources_off();
        (0..self.n_sources).map(move |i| {
            let o = base + i * SOURCE_LEN;
            Source {
                id: d[o],
                date: u32_at(d, o + 4),
                fetched: u32_at(d, o + 8),
                name: str_of(&d[o + 12..o + 12 + SOURCE_NAME_LEN]),
            }
        })
    }

    pub fn source(&self, id: u8) -> Option<Source<'a>> {
        self.sources().find(|s| s.id == id)
    }

    /// The first section of this kind.
    pub fn section(&self, kind: Section) -> Option<&'a [u8]> {
        let d = self.data;
        (0..self.n_sections).find_map(|i| {
            let o = self.sections_off() + i * SECTION_LEN;
            (u16_at(d, o) == kind as u16).then(|| {
                let (off, len) = (u32_at(d, o + 4) as usize, u32_at(d, o + 8) as usize);
                &d[off..off + len]
            })
        })
    }

    fn check_areas(&self) -> Result<(), PackError> {
        let Some(s) = self.section(Section::Areas) else {
            return Ok(());
        };
        if s.len() < 4 {
            return Err(PackError::Bounds);
        }
        let n = u32_at(s, 0) as usize;
        let mut o = 4;
        for _ in 0..n {
            o = area_end(s, o).ok_or(PackError::Bounds)?;
        }
        Ok(())
    }

    /// Every area, in pack order.
    pub fn areas(&self) -> AreaIter<'a> {
        match self.section(Section::Areas) {
            Some(s) if s.len() >= 4 => AreaIter {
                s,
                off: 4,
                left: u32_at(s, 0),
            },
            _ => AreaIter {
                s: &[],
                off: 0,
                left: 0,
            },
        }
    }

    /// Fill `out` with every area's offset and box (known kinds only).
    /// Returns how many, or `None` if `out` is too small.
    pub fn build_index(&self, out: &mut [AreaRef]) -> Option<usize> {
        let Some(s) = self.section(Section::Areas).filter(|s| s.len() >= 4) else {
            return Some(0);
        };
        let mut o = 4;
        let mut n = 0;
        for _ in 0..u32_at(s, 0) {
            let end = area_end(s, o)?;
            if AreaKind::from_u8(s[o]).is_some() {
                *out.get_mut(n)? = AreaRef {
                    off: o as u32,
                    lat_min: i32_at(s, o + 20),
                    lat_max: i32_at(s, o + 24),
                    lon_min: i32_at(s, o + 28),
                    lon_max: i32_at(s, o + 32),
                    kind: s[o],
                };
                n += 1;
            }
            o = end;
        }
        Some(n)
    }

    /// This pack, answering [`Pack::areas_at`] from `index` (made by
    /// [`Pack::build_index`] for this same pack).
    pub fn with_index(mut self, index: &'a [AreaRef]) -> Self {
        self.index = Some(index);
        self
    }

    /// Areas whose shape contains the position (any altitude, any time).
    pub fn areas_at(&self, lat: f64, lon: f64) -> impl Iterator<Item = Area<'a>> + use<'a, '_> {
        let (la, lo) = ((lat * 1e7) as i32, (lon * 1e7) as i32);
        let s = self.section(Section::Areas).unwrap_or(&[]);
        let indexed = self
            .index
            .into_iter()
            .flatten()
            .filter(move |r| {
                la >= r.lat_min && la <= r.lat_max && lo >= r.lon_min && lo <= r.lon_max
            })
            .filter_map(move |r| {
                let kind = AreaKind::from_u8(r.kind)?;
                Some(area_at(s, r.off as usize, kind))
            });
        let scanned = self
            .index
            .is_none()
            .then(|| self.areas())
            .into_iter()
            .flatten();
        indexed.chain(scanned).filter(move |a| a.contains(lat, lon))
    }

    pub fn terrain(&self) -> Option<Grid<'a>> {
        self.section(Section::Terrain)
            .and_then(|s| Grid::parse(s, 16))
    }

    pub fn uasfm(&self) -> Option<Grid<'a>> {
        self.section(Section::Uasfm).and_then(|s| Grid::parse(s, 8))
    }

    fn geoid(&self) -> Option<Grid<'a>> {
        self.section(Section::Geoid)
            .and_then(|s| Grid::parse(s, 16))
    }

    pub(crate) fn urban_grid(&self) -> Option<Grid<'a>> {
        self.section(Section::Urban).and_then(|s| Grid::parse(s, 1))
    }

    /// Ground height above the geoid (≈ MSL), metres, bilinear.
    pub fn ground_msl_m(&self, lat: f64, lon: f64) -> Option<f64> {
        self.terrain()?.bilinear_i16(lat, lon, 1.0).map(f64::from)
    }

    /// Geoid height (ellipsoid minus geoid), metres. Clamped to the grid's
    /// edge outside it: the geoid changes by centimetres per kilometre.
    pub fn geoid_m(&self, lat: f64, lon: f64) -> Option<f64> {
        let g = self.geoid()?;
        let lat_max =
            (f64::from(g.lat0) + f64::from(g.dlat) * f64::from(g.rows.saturating_sub(1))) * 1e-7;
        let lon_max =
            (f64::from(g.lon0) + f64::from(g.dlon) * f64::from(g.cols.saturating_sub(1))) * 1e-7;
        let lat = lat.clamp(f64::from(g.lat0) * 1e-7, lat_max);
        let lon = lon.clamp(f64::from(g.lon0) * 1e-7, lon_max);
        g.bilinear_i16(lat, lon, 0.01).map(f64::from)
    }

    /// Ground height above the WGS-84 ellipsoid, metres: what to subtract
    /// from a Remote ID geodetic altitude to get height above ground.
    pub fn ground_hae_m(&self, lat: f64, lon: f64) -> Option<f64> {
        Some(self.ground_msl_m(lat, lon)? + self.geoid_m(lat, lon)?)
    }

    /// The UAS Facility Map ceiling, feet AGL: `Some(0..=400)` inside a
    /// facility-map cell, `None` outside the grid or where no map applies.
    pub fn uasfm_ceiling_ft(&self, lat: f64, lon: f64) -> Option<u16> {
        let g = self.uasfm()?;
        let (r, c) = g.cell(lat, lon)?;
        let v = g.u8_at(r, c);
        (v != UASFM_NONE).then_some(u16::from(v) * 10)
    }

    /// Inside a census urban area (`None`: no urban layer or outside it).
    pub fn urban(&self, lat: f64, lon: f64) -> Option<bool> {
        let g = self.urban_grid()?;
        let (r, c) = g.cell(lat, lon)?;
        Some(g.bit_at(r, c))
    }

    pub fn airports(&self) -> impl Iterator<Item = Airport<'a>> {
        let s = self.section(Section::Airports).unwrap_or(&[]);
        let n = if s.len() >= 4 {
            (u32_at(s, 0) as usize).min((s.len() - 4) / AIRPORT_LEN)
        } else {
            0
        };
        (0..n).map(move |i| {
            let o = 4 + i * AIRPORT_LEN;
            Airport {
                lat: f64::from(i32_at(s, o)) * 1e-7,
                lon: f64::from(i32_at(s, o + 4)) * 1e-7,
                elev_ft: i16_at(s, o + 8),
                kind: s[o + 10],
                flags: s[o + 11],
                ident: str_of(&s[o + 12..o + 20]),
            }
        })
    }

    /// The nearest airport accepted by `keep`, and its distance in metres.
    pub fn nearest_airport(
        &self,
        lat: f64,
        lon: f64,
        keep: impl Fn(&Airport) -> bool,
    ) -> Option<(Airport<'a>, f64)> {
        self.airports()
            .filter(|a| keep(a))
            .map(|a| (a, distance_m(lat, lon, a.lat, a.lon)))
            .min_by(|a, b| a.1.total_cmp(&b.1))
    }

    /// Terrain against the straight line from `a` to `b` (lat, lon, metres
    /// above the geoid), with the Earth's curvature (standard refraction,
    /// k = 7/6). Samples every half post spacing, at most 400 samples, and
    /// skips the first and last 2 % (the ends sit on or near the ground).
    /// `None` without terrain or if any sample is off the grid.
    pub fn line_of_sight(&self, a: (f64, f64, f64), b: (f64, f64, f64)) -> Option<Los> {
        let g = self.terrain()?;
        let d = distance_m(a.0, a.1, b.0, b.1);
        let (ns, ew) = g.spacing_m();
        let step = (ns.min(ew) / 2.0).max(1.0);
        let n = ((d / step) as usize).clamp(2, 400);
        let re = EARTH_R * 7.0 / 6.0;
        let mut worst = Los {
            worst_m: f32::NEG_INFINITY,
            at: 0.0,
            samples: 0,
        };
        for i in 1..n {
            let t = i as f64 / n as f64;
            if !(0.02..=0.98).contains(&t) {
                continue;
            }
            let lat = a.0 + (b.0 - a.0) * t;
            let lon = a.1 + (b.1 - a.1) * t;
            // The line is straight; the ground under it bulges up by
            // d1 * d2 / (2 R) relative to a chord.
            let bulge = (t * d) * ((1.0 - t) * d) / (2.0 * re);
            let line = a.2 + (b.2 - a.2) * t;
            let ground = g.bilinear_i16(lat, lon, 1.0)? as f64 + bulge;
            let above = (ground - line) as f32;
            worst.samples += 1;
            if above > worst.worst_m {
                worst.worst_m = above;
                worst.at = t as f32;
            }
        }
        (worst.samples > 0).then_some(worst)
    }
}

/// Offset just past the area record at `o`, if it fits in `s`.
fn area_end(s: &[u8], o: usize) -> Option<usize> {
    if o + AREA_HEAD_LEN > s.len() {
        return None;
    }
    let n_points = usize::from(u16_at(s, o + 36));
    let n_rings = usize::from(u16_at(s, o + 38));
    let name_len = usize::from(s[o + 40]).min(MAX_NAME);
    let rings_at = o + AREA_HEAD_LEN + pad4(name_len);
    let end = rings_at + pad4(2 * n_rings) + 8 * n_points;
    if end > s.len() {
        return None;
    }
    // Ring lengths index into the points; a sum past the count would read
    // beyond them.
    let claimed: usize = (0..n_rings)
        .map(|i| usize::from(u16_at(s, rings_at + 2 * i)))
        .sum();
    (claimed <= n_points).then_some(end)
}

/// Iterator over the areas section.
pub struct AreaIter<'a> {
    s: &'a [u8],
    off: usize,
    left: u32,
}

impl<'a> Iterator for AreaIter<'a> {
    type Item = Area<'a>;

    fn next(&mut self) -> Option<Area<'a>> {
        loop {
            if self.left == 0 {
                return None;
            }
            let (s, o) = (self.s, self.off);
            let end = area_end(s, o)?;
            self.left -= 1;
            self.off = end;
            // A kind this build does not know (added by a newer tool) is skipped.
            if let Some(kind) = AreaKind::from_u8(s[o]) {
                return Some(area_at(s, o, kind));
            }
        }
    }
}

fn area_at(s: &[u8], o: usize, kind: AreaKind) -> Area<'_> {
    {
        let n_points = usize::from(u16_at(s, o + 36));
        let n_rings = usize::from(u16_at(s, o + 38));
        let name_len = usize::from(s[o + 40]).min(MAX_NAME);
        let name_at = o + AREA_HEAD_LEN;
        let rings_at = name_at + pad4(name_len);
        let pts_at = rings_at + pad4(2 * n_rings);
        Area {
            kind,
            source: s[o + 1],
            floor_ref: AltRef::from_u8(s[o + 2]),
            ceil_ref: AltRef::from_u8(s[o + 3]),
            floor_ft: i32_at(s, o + 4),
            ceil_ft: i32_at(s, o + 8),
            start: u32_at(s, o + 12),
            end: u32_at(s, o + 16),
            lat_min: i32_at(s, o + 20),
            lat_max: i32_at(s, o + 24),
            lon_min: i32_at(s, o + 28),
            lon_max: i32_at(s, o + 32),
            flags: s[o + 41],
            name: core::str::from_utf8(&s[name_at..name_at + name_len]).unwrap_or("?"),
            rings: &s[rings_at..rings_at + 2 * n_rings],
            points: &s[pts_at..pts_at + 8 * n_points],
        }
    }
}
