//! Scripted flights: where a made-up drone is at any moment.
//!
//! Each flight is a plain function of time (seconds since it started) to a
//! position, height, speed and direction, with no randomness and no state.
//! The host tests fly them past the threat-level rules, and the bench test
//! transmitter (`rid-beacon`) can fly the very same paths as Remote ID so a
//! real hub hears them over the air (fake IDs, lowest power, bench only).
//! Later slices add their own shapes (loiter, drop, follow) here.
//!
//! Positions use a flat map round the start point, which is exact enough
//! for flights of a few kilometres.

use libm::{atan2, cos, sin, sqrt};

use super::places::Frame;

/// One moment of a flight.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub lat: f64,
    pub lon: f64,
    /// Height above the WGS-84 ellipsoid, metres.
    pub alt_m: f32,
    /// Ground speed, m/s.
    pub speed_mps: f32,
    /// Direction of travel, degrees true (0 when hovering).
    pub course_deg: f32,
    /// Climb rate, m/s (negative: descending).
    pub vspeed_mps: f32,
}

/// The point `east_m`, `north_m` metres from `(lat, lon)`.
pub fn offset(lat: f64, lon: f64, east_m: f64, north_m: f64) -> (f64, f64) {
    Frame::at(lat, lon).latlon(east_m as f32, north_m as f32)
}

/// A flight shape.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Flight {
    /// Stay at one place.
    Hover { lat: f64, lon: f64, alt_m: f32 },
    /// Fly straight from `from` towards `to` at `speed_mps`, changing
    /// height evenly from `alt_from` to `alt_to`, then hover at `to`.
    Toward { from: (f64, f64), to: (f64, f64), speed_mps: f64, alt_from: f32, alt_to: f32 },
    /// Fly straight on for ever from `from` along `course_deg`.
    Line { from: (f64, f64), course_deg: f64, speed_mps: f64, alt_m: f32 },
    /// Circle round `centre` at `radius_m`, `speed_mps` along the circle,
    /// clockwise seen from above when `clockwise`; starts due north of the
    /// centre.
    Orbit { centre: (f64, f64), radius_m: f64, speed_mps: f64, alt_m: f32, clockwise: bool },
}

impl Flight {
    /// Where the drone is `t_s` seconds into the flight.
    pub fn at(&self, t_s: f64) -> Sample {
        let t = t_s.max(0.0);
        match *self {
            Flight::Hover { lat, lon, alt_m } => {
                Sample { lat, lon, alt_m, speed_mps: 0.0, course_deg: 0.0, vspeed_mps: 0.0 }
            }
            Flight::Toward { from, to, speed_mps, alt_from, alt_to } => {
                let f = Frame::at(from.0, from.1);
                let (x, y) = f.xy(to.0, to.1);
                let (x, y) = (f64::from(x), f64::from(y));
                let dist = sqrt(x * x + y * y);
                let total = if speed_mps > 0.0 { dist / speed_mps } else { 0.0 };
                let course = norm(atan2(x, y).to_degrees());
                if t >= total || dist < 1e-6 {
                    return Sample { lat: to.0, lon: to.1, alt_m: alt_to, speed_mps: 0.0, course_deg: course as f32, vspeed_mps: 0.0 };
                }
                let k = t / total;
                let (lat, lon) = f.latlon((x * k) as f32, (y * k) as f32);
                let climb = (alt_to - alt_from) as f64 / total;
                Sample {
                    lat,
                    lon,
                    alt_m: alt_from + ((alt_to - alt_from) as f64 * k) as f32,
                    speed_mps: speed_mps as f32,
                    course_deg: course as f32,
                    vspeed_mps: climb as f32,
                }
            }
            Flight::Line { from, course_deg, speed_mps, alt_m } => {
                let d = speed_mps * t;
                let r = course_deg.to_radians();
                let (lat, lon) = offset(from.0, from.1, d * sin(r), d * cos(r));
                Sample { lat, lon, alt_m, speed_mps: speed_mps as f32, course_deg: norm(course_deg) as f32, vspeed_mps: 0.0 }
            }
            Flight::Orbit { centre, radius_m, speed_mps, alt_m, clockwise } => {
                let w = if radius_m > 0.0 { speed_mps / radius_m } else { 0.0 };
                // Angle from north, clockwise positive.
                let a = if clockwise { w * t } else { -w * t };
                let (lat, lon) = offset(centre.0, centre.1, radius_m * sin(a), radius_m * cos(a));
                let course = a.to_degrees() + if clockwise { 90.0 } else { -90.0 };
                Sample { lat, lon, alt_m, speed_mps: speed_mps as f32, course_deg: norm(course) as f32, vspeed_mps: 0.0 }
            }
        }
    }
}

fn norm(d: f64) -> f64 {
    let r = d % 360.0;
    if r < 0.0 { r + 360.0 } else { r }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::range_bearing;

    const SITE: (f64, f64) = (39.95210, -75.16380);

    #[test]
    fn flights_go_where_they_say() {
        let from = offset(SITE.0, SITE.1, 0.0, -600.0);
        let f = Flight::Toward { from, to: SITE, speed_mps: 12.0, alt_from: 80.0, alt_to: 40.0 };
        let s = f.at(25.0);
        let rb = range_bearing(SITE.0, SITE.1, s.lat, s.lon);
        assert!((rb.distance_m - 300.0).abs() < 2.0, "{}", rb.distance_m);
        assert!((s.course_deg - 0.0).abs() < 0.5 || (s.course_deg - 360.0).abs() < 0.5, "{}", s.course_deg);
        assert!((s.alt_m - 60.0).abs() < 0.1);
        assert_eq!(f.at(100.0).speed_mps, 0.0, "arrived: hovering");
        let o = Flight::Orbit { centre: SITE, radius_m: 100.0, speed_mps: 10.0, alt_m: 50.0, clockwise: true };
        for t in [0.0, 7.0, 31.4] {
            let s = o.at(t);
            let rb = range_bearing(SITE.0, SITE.1, s.lat, s.lon);
            assert!((rb.distance_m - 100.0).abs() < 1.0);
            // Course is at right angles to the bearing from the centre.
            let diff = norm(f64::from(s.course_deg) - rb.bearing_deg);
            assert!((diff - 90.0).abs() < 1.0, "{diff}");
        }
        let l = Flight::Line { from: SITE, course_deg: 90.0, speed_mps: 15.0, alt_m: 70.0 };
        let rb = range_bearing(SITE.0, SITE.1, l.at(10.0).lat, l.at(10.0).lon);
        assert!((rb.distance_m - 150.0).abs() < 1.0 && (rb.bearing_deg - 90.0).abs() < 0.5);
    }
}
