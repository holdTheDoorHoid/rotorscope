//! Sun elevation from UTC and position, for the night/twilight check
//! (14 CFR 107.29). NOAA's solar position equations (the ones behind the
//! NOAA Solar Calculator, after Meeus), good to about 0.01 degree for
//! years 1950-2100: far tighter than the minute of rounding in a
//! published twilight table.
//!
//! Civil twilight is the sun's centre 6 degrees below the horizon,
//! geometric (no refraction), as the US Naval Observatory defines it. The
//! elevation returned here is geometric too.

use libm::{acos, asin, cos, sin, tan};

use crate::geom::rad;

/// Sun elevation at the end of evening / start of morning civil twilight.
pub const CIVIL_TWILIGHT_DEG: f64 = -6.0;

fn deg(r: f64) -> f64 {
    r * (180.0 / core::f64::consts::PI)
}

/// Geometric elevation of the sun's centre above the horizon, degrees.
/// `utc_ms`: Unix milliseconds.
pub fn elevation_deg(utc_ms: u64, lat: f64, lon: f64) -> f64 {
    let jd = utc_ms as f64 / 86_400_000.0 + 2_440_587.5;
    let t = (jd - 2_451_545.0) / 36_525.0;
    let l0 = (280.466_46 + t * (36_000.769_83 + t * 0.000_303_2)) % 360.0;
    let m = 357.529_11 + t * (35_999.050_29 - 0.000_153_7 * t);
    let e = 0.016_708_634 - t * (0.000_042_037 + 0.000_000_126_7 * t);
    let mr = rad(m);
    let c = sin(mr) * (1.914_602 - t * (0.004_817 + 0.000_014 * t))
        + sin(2.0 * mr) * (0.019_993 - 0.000_101 * t)
        + sin(3.0 * mr) * 0.000_289;
    let true_long = l0 + c;
    let omega = rad(125.04 - 1_934.136 * t);
    let lambda = rad(true_long - 0.005_69 - 0.004_78 * sin(omega));
    let eps0 =
        23.0 + (26.0 + (21.448 - t * (46.815 + t * (0.000_59 - t * 0.001_813))) / 60.0) / 60.0;
    let eps = rad(eps0 + 0.002_56 * cos(omega));
    let decl = asin(sin(eps) * sin(lambda));
    let y = tan(eps / 2.0) * tan(eps / 2.0);
    let l0r = rad(l0);
    let eot_min = 4.0
        * deg(
            y * sin(2.0 * l0r) - 2.0 * e * sin(mr) + 4.0 * e * y * sin(mr) * cos(2.0 * l0r)
                - 0.5 * y * y * sin(4.0 * l0r)
                - 1.25 * e * e * sin(2.0 * mr),
        );
    let day_min = (utc_ms % 86_400_000) as f64 / 60_000.0;
    let true_solar = (day_min + eot_min + 4.0 * lon) % 1440.0;
    let true_solar = if true_solar < 0.0 {
        true_solar + 1440.0
    } else {
        true_solar
    };
    let ha = rad(true_solar / 4.0 - 180.0);
    let latr = rad(lat);
    let cos_z = (sin(latr) * sin(decl) + cos(latr) * cos(decl) * cos(ha)).clamp(-1.0, 1.0);
    90.0 - deg(acos(cos_z))
}

/// Light at a time and place, as 107.29 divides it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Light {
    /// Sun above the horizon (geometric centre above -0.833 deg).
    Day,
    /// Civil twilight: between sunset and -6 deg (or -6 deg and sunrise).
    /// Allowed with anti-collision lighting (107.29(c)).
    Twilight,
    /// Below -6 deg: "night" in 14 CFR 1.1 / 107.29(a).
    Night,
}

/// Which light it is at `utc_ms` and the position.
pub fn light(utc_ms: u64, lat: f64, lon: f64) -> (Light, f64) {
    let el = elevation_deg(utc_ms, lat, lon);
    let l = if el < CIVIL_TWILIGHT_DEG {
        Light::Night
    } else if el < -0.833 {
        Light::Twilight
    } else {
        Light::Day
    };
    (l, el)
}
