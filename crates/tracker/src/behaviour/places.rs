//! Places to protect: fixed areas (a circle or a polygon), the moving
//! bubble round the unit itself, and friendly drones. Set by the site file
//! ([`crate::site`]: `area`, `bubble`, `friend` lines).
//!
//! **Geometry.** Each area is worked on in its own flat map in metres
//! (east, north) centred on the area. Over a few kilometres a flat map is
//! off by well under a metre, far less than the 30 m a Remote ID position
//! may be off. The sums are done in single precision (`f32`), which the
//! P4's processor does in hardware; only the conversion from degrees uses
//! double precision.

use heapless::{String, Vec};
use libm::{cos, sqrtf};

/// Fixed areas held.
pub const MAX_AREAS: usize = 8;
/// Corners of a polygon area, at most (and at least [`MIN_VERTICES`]).
pub const MAX_VERTICES: usize = 16;
pub const MIN_VERTICES: usize = 3;
/// Friendly drones held.
pub const MAX_FRIENDS: usize = 8;
/// Longest area name.
pub const NAME_LEN: usize = 12;
/// Longest friend serial (Remote ID serials are at most 20 characters).
pub const SERIAL_LEN: usize = 20;
/// Longest friend label.
pub const LABEL_LEN: usize = 20;
/// The ceiling of an area that sets none: 120 m (400 ft), the usual legal
/// limit for small drones.
pub const DEFAULT_CEILING_M: f32 = 120.0;
/// Radius limits for a circle area or the bubble, metres.
pub const MIN_RADIUS_M: f32 = 10.0;
pub const MAX_RADIUS_M: f32 = 10_000.0;

pub type Name = String<NAME_LEN>;

/// Metres per degree of latitude (WGS-84 mean, good to 0.5 % everywhere).
const M_PER_DEG: f64 = 111_195.0;

/// A polygon corner, 1e-7 degree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vertex {
    pub lat_e7: i32,
    pub lon_e7: i32,
}

impl Vertex {
    pub fn new(lat: f64, lon: f64) -> Self {
        Self { lat_e7: libm::round(lat * 1e7) as i32, lon_e7: libm::round(lon * 1e7) as i32 }
    }

    pub fn lat(&self) -> f64 {
        f64::from(self.lat_e7) * 1e-7
    }

    pub fn lon(&self) -> f64 {
        f64::from(self.lon_e7) * 1e-7
    }
}

/// The outline of an area.
#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    Circle { lat: f64, lon: f64, radius_m: f32 },
    Poly(Vec<Vertex, MAX_VERTICES>),
}

/// One protected area. An area named only by a `ceiling` line has no
/// shape yet and protects nothing until a `circle` or `poly` line gives it
/// one.
#[derive(Debug, Clone, PartialEq)]
pub struct Area {
    pub name: Name,
    pub shape: Option<Shape>,
    /// Highest a drone may fly over it, metres above the unit; `None`:
    /// [`DEFAULT_CEILING_M`].
    pub ceiling_m: Option<f32>,
}

impl Area {
    pub fn ceiling(&self) -> f32 {
        self.ceiling_m.unwrap_or(DEFAULT_CEILING_M)
    }
}

/// A drone the owner knows (`friend` line). Matched on any of the track's
/// IDs. Remote ID serials can be faked, so a friend is only ever lowered,
/// never hidden (see [`crate::behaviour`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Friend {
    pub serial: String<SERIAL_LEN>,
    pub label: String<LABEL_LEN>,
}

/// "Several at once" (rule 15) counts this many drones at the places by
/// default (`crowd on`).
pub const CROWD_DEFAULT: u8 = 3;
/// The range a `crowd N` line may set.
pub const CROWD_MIN: u8 = 2;
pub const CROWD_MAX: u8 = 20;

/// Everything the threat levels are judged against.
#[derive(Debug, Clone, PartialEq)]
pub struct Places {
    pub areas: Vec<Area, MAX_AREAS>,
    /// Radius of the circle that moves with the unit's own GPS, metres;
    /// `None`: off.
    pub bubble_m: Option<f32>,
    pub friends: Vec<Friend, MAX_FRIENDS>,
    /// How many drones at the places at once make "several at once" (rule
    /// 15, slice 16d); `None`: that count is off (`crowd off`, for events
    /// with many friendly drones). Drones appearing during jamming or GPS
    /// interference still count.
    pub crowd_min: Option<u8>,
    /// Keep the repeat-visitor history (rule 17, `visits on|off`).
    pub visits: bool,
}

impl Default for Places {
    fn default() -> Self {
        Self::new()
    }
}

impl Places {
    pub const fn new() -> Self {
        Self {
            areas: Vec::new(),
            bubble_m: None,
            friends: Vec::new(),
            crowd_min: Some(CROWD_DEFAULT),
            visits: true,
        }
    }

    /// Become a copy of `o`, one entry at a time (no whole `Places` on the
    /// stack).
    pub fn copy_from(&mut self, o: &Places) {
        self.areas.clear();
        for a in &o.areas {
            let _ = self.areas.push(a.clone());
        }
        self.friends.clear();
        for f in &o.friends {
            let _ = self.friends.push(f.clone());
        }
        self.bubble_m = o.bubble_m;
        self.crowd_min = o.crowd_min;
        self.visits = o.visits;
    }

    /// Areas with a shape, and whether the bubble is on: anything to protect.
    pub fn any(&self) -> bool {
        self.bubble_m.is_some() || self.areas.iter().any(|a| a.shape.is_some())
    }

    pub fn area(&self, name: &str) -> Option<&Area> {
        self.areas.iter().find(|a| a.name.eq_ignore_ascii_case(name))
    }

    fn area_mut(&mut self, name: &str) -> Option<&mut Area> {
        self.areas.iter_mut().find(|a| a.name.eq_ignore_ascii_case(name))
    }

    /// The friend entry for one of these IDs, if any.
    pub fn friend<'a>(&self, mut ids: impl Iterator<Item = &'a str>) -> Option<&Friend> {
        ids.find_map(|id| self.friends.iter().find(|f| !id.is_empty() && f.serial.as_str() == id))
    }

    /// Whether `name` is new and the area list is full (the caller refuses
    /// the line: an area is never dropped to make room for another).
    pub fn area_full(&self, name: &str) -> bool {
        self.area(name).is_none() && self.areas.is_full()
    }

    pub fn friends_full(&self, serial: &str) -> bool {
        !self.friends.iter().any(|f| f.serial.as_str() == serial) && self.friends.is_full()
    }

    /// Set (or replace) an area's outline, keeping its ceiling.
    pub fn set_shape(&mut self, name: &Name, shape: Shape) -> bool {
        if let Some(a) = self.area_mut(name) {
            a.shape = Some(shape);
            return true;
        }
        self.areas.push(Area { name: name.clone(), shape: Some(shape), ceiling_m: None }).is_ok()
    }

    pub fn set_ceiling(&mut self, name: &Name, ceiling_m: f32) -> bool {
        if let Some(a) = self.area_mut(name) {
            a.ceiling_m = Some(ceiling_m);
            return true;
        }
        self.areas.push(Area { name: name.clone(), shape: None, ceiling_m: Some(ceiling_m) }).is_ok()
    }

    pub fn clear_area(&mut self, name: &str) {
        self.areas.retain(|a| !a.name.eq_ignore_ascii_case(name));
    }

    pub fn set_friend(&mut self, f: Friend) -> bool {
        if let Some(o) = self.friends.iter_mut().find(|o| o.serial == f.serial) {
            *o = f;
            return true;
        }
        self.friends.push(f).is_ok()
    }

    pub fn clear_friend(&mut self, serial: &str) {
        self.friends.retain(|f| f.serial.as_str() != serial);
    }
}

// ----------------------------------------------------------- flat maps --

/// A flat map in metres (east, north) round one point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    lat0: f64,
    lon0: f64,
    /// Metres per degree of longitude at `lat0`.
    kx: f64,
}

impl Frame {
    pub fn at(lat0: f64, lon0: f64) -> Self {
        Self { lat0, lon0, kx: M_PER_DEG * cos(lat0.to_radians()) }
    }

    /// `(east, north)` metres of a point from the centre.
    pub fn xy(&self, lat: f64, lon: f64) -> (f32, f32) {
        let mut dlon = lon - self.lon0;
        if dlon > 180.0 {
            dlon -= 360.0;
        } else if dlon < -180.0 {
            dlon += 360.0;
        }
        ((dlon * self.kx) as f32, ((lat - self.lat0) * M_PER_DEG) as f32)
    }

    /// The point `(east, north)` metres from the centre.
    pub fn latlon(&self, x: f32, y: f32) -> (f64, f64) {
        (self.lat0 + f64::from(y) / M_PER_DEG, self.lon0 + f64::from(x) / self.kx)
    }
}

/// An area outline in its own flat map, ready for the per-second checks.
#[derive(Debug, Clone)]
pub struct Local {
    pub frame: Frame,
    kind: LocalKind,
    /// "The thing protected" for a drone already inside: the circle's
    /// centre or the polygon's middle (the origin of the frame), and how
    /// near it a drone must be predicted to pass to count as heading for it.
    pub core_m: f32,
    /// Widest distance from the origin to the outline.
    pub size_m: f32,
}

#[derive(Debug, Clone)]
enum LocalKind {
    Circle(f32),
    Poly(Vec<(f32, f32), MAX_VERTICES>),
}

/// Closest a drone already inside an area must be predicted to pass the
/// middle to count as heading for it: a quarter of the area's size, and
/// never under 40 m (positions are only good to about 30 m).
pub const MIN_CORE_M: f32 = 40.0;

impl Local {
    pub fn circle(lat: f64, lon: f64, radius_m: f32) -> Self {
        Self {
            frame: Frame::at(lat, lon),
            kind: LocalKind::Circle(radius_m),
            core_m: (radius_m * 0.25).max(MIN_CORE_M),
            size_m: radius_m,
        }
    }

    pub fn of(shape: &Shape) -> Self {
        match shape {
            Shape::Circle { lat, lon, radius_m } => Self::circle(*lat, *lon, *radius_m),
            Shape::Poly(v) => {
                // The middle: the average of the corners (good enough for
                // the outlines people draw round a house or a yard).
                let n = v.len().max(1) as f64;
                let lat0 = v.iter().map(|p| p.lat()).sum::<f64>() / n;
                let lon0 = v.iter().map(|p| p.lon()).sum::<f64>() / n;
                let frame = Frame::at(lat0, lon0);
                let pts: Vec<(f32, f32), MAX_VERTICES> = v.iter().map(|p| frame.xy(p.lat(), p.lon())).collect();
                let size = pts.iter().map(|(x, y)| sqrtf(x * x + y * y)).fold(0.0f32, f32::max);
                Self { frame, kind: LocalKind::Poly(pts), core_m: (size * 0.25).max(MIN_CORE_M), size_m: size }
            }
        }
    }

    /// Inside (or on) the outline.
    pub fn contains(&self, p: (f32, f32)) -> bool {
        match &self.kind {
            LocalKind::Circle(r) => p.0 * p.0 + p.1 * p.1 <= r * r,
            LocalKind::Poly(v) => {
                // Ray casting: count edges crossed by a ray going east.
                let mut inside = false;
                let n = v.len();
                for i in 0..n {
                    let (a, b) = (v[i], v[(i + n - 1) % n]);
                    if (a.1 > p.1) != (b.1 > p.1) {
                        let x = a.0 + (p.1 - a.1) / (b.1 - a.1) * (b.0 - a.0);
                        if p.0 < x {
                            inside = !inside;
                        }
                    }
                }
                inside
            }
        }
    }

    /// Metres from `p` to the outline, 0 inside.
    pub fn outside_by(&self, p: (f32, f32)) -> f32 {
        if self.contains(p) {
            return 0.0;
        }
        match &self.kind {
            LocalKind::Circle(r) => (sqrtf(p.0 * p.0 + p.1 * p.1) - r).max(0.0),
            LocalKind::Poly(v) => {
                let n = v.len();
                (0..n).map(|i| seg_dist(p, v[i], v[(i + 1) % n])).fold(f32::MAX, f32::min)
            }
        }
    }

    /// Seconds until a point now at `p`, moving at `vel` (m/s, relative to
    /// the area), first enters the outline, if within `lookahead_s`. `Some(0)`
    /// when already inside.
    pub fn entry_s(&self, p: (f32, f32), vel: (f32, f32), lookahead_s: f32) -> Option<f32> {
        if self.contains(p) {
            return Some(0.0);
        }
        let v2 = vel.0 * vel.0 + vel.1 * vel.1;
        if v2 < 0.01 {
            return None;
        }
        match &self.kind {
            LocalKind::Circle(r) => {
                // The closest point of approach to the centre (the same
                // sums as the aircraft check, `conflict`), then back along
                // the path to where it crosses the circle.
                let (t, d) = crate::conflict::closest_approach(
                    (f64::from(p.0), f64::from(p.1)),
                    (f64::from(vel.0), f64::from(vel.1)),
                    f64::MAX,
                )?;
                let (t, d) = (t as f32, d as f32);
                if d >= *r {
                    return None;
                }
                let t_in = t - sqrtf(r * r - d * d) / sqrtf(v2);
                (t_in <= lookahead_s).then_some(t_in.max(0.0))
            }
            LocalKind::Poly(_) => {
                // Step along the path, about 10 m at a time.
                let speed = sqrtf(v2);
                let dt = (10.0 / speed).clamp(0.05, 1.0);
                let mut t = dt;
                while t <= lookahead_s {
                    if self.contains((p.0 + vel.0 * t, p.1 + vel.1 * t)) {
                        return Some(t);
                    }
                    t += dt;
                }
                None
            }
        }
    }
}

fn seg_dist(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
    let (x, y) = (a.0 + t * dx - p.0, a.1 + t * dy - p.1);
    sqrtf(x * x + y * y)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(c: (f64, f64), half_m: f64) -> Shape {
        let f = Frame::at(c.0, c.1);
        let mut v = Vec::new();
        for (x, y) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
            let (lat, lon) = f.latlon((x * half_m) as f32, (y * half_m) as f32);
            v.push(Vertex::new(lat, lon)).unwrap();
        }
        Shape::Poly(v)
    }

    #[test]
    fn circles_and_polygons_contain_and_measure() {
        let c = Local::circle(39.95, -75.16, 100.0);
        assert!(c.contains((60.0, 70.0)));
        assert!(!c.contains((80.0, 70.0)));
        assert!((c.outside_by((0.0, 150.0)) - 50.0).abs() < 1e-3);
        let s = Local::of(&square((39.95, -75.16), 50.0));
        assert!(s.contains((0.0, 0.0)) && s.contains((49.0, -49.0)));
        assert!(!s.contains((51.0, 0.0)));
        assert!((s.outside_by((80.0, 0.0)) - 30.0).abs() < 0.2, "{}", s.outside_by((80.0, 0.0)));
        assert!((s.outside_by((80.0, 90.0)) - 50.0).abs() < 0.3, "corner: {}", s.outside_by((80.0, 90.0)));
        assert!(s.size_m > 70.0 && s.size_m < 71.0);
    }

    #[test]
    fn entry_time_along_a_straight_path() {
        let c = Local::circle(39.95, -75.16, 100.0);
        // 300 m south, 10 m/s north: enters after 20 s.
        let t = c.entry_s((0.0, -300.0), (0.0, 10.0), 60.0).unwrap();
        assert!((t - 20.0).abs() < 0.01, "{t}");
        // Passing 150 m to the side: never.
        assert_eq!(c.entry_s((150.0, -300.0), (0.0, 10.0), 60.0), None);
        // Too far for the look-ahead.
        assert_eq!(c.entry_s((0.0, -1000.0), (0.0, 10.0), 60.0), None);
        // Going away.
        assert_eq!(c.entry_s((0.0, -300.0), (0.0, -10.0), 60.0), None);
        let s = Local::of(&square((39.95, -75.16), 50.0));
        let t = s.entry_s((-250.0, 0.0), (10.0, 0.0), 60.0).unwrap();
        assert!((t - 20.0).abs() < 1.1, "{t}");
        assert_eq!(s.entry_s((-250.0, 80.0), (10.0, 0.0), 60.0), None);
    }

    #[test]
    fn friends_and_areas_by_name() {
        let mut p = Places::new();
        let n: Name = Name::try_from("House").unwrap();
        assert!(p.set_ceiling(&n, 60.0));
        assert!(!p.any(), "a ceiling alone protects nothing");
        assert!(p.set_shape(&Name::try_from("house").unwrap(), Shape::Circle { lat: 1.0, lon: 2.0, radius_m: 50.0 }));
        assert_eq!(p.areas.len(), 1);
        assert_eq!(p.area("HOUSE").unwrap().ceiling(), 60.0);
        p.set_friend(Friend { serial: String::try_from("SN1").unwrap(), label: String::new() });
        assert!(p.friend(["x", "SN1"].into_iter()).is_some());
        assert!(p.friend(["", "SN2"].into_iter()).is_none());
        p.clear_friend("SN1");
        assert!(p.friends.is_empty());
    }
}
