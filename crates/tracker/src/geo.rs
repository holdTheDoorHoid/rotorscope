//! Range and bearing on the WGS-84 ellipsoid, with no `std`.
//!
//! Remote ID positions are WGS-84 and our GPS is WGS-84, so distances are
//! computed on that ellipsoid with Vincenty's inverse formula. A sphere
//! (haversine) is off by up to 0.5 %, which is 5 m at 1 km: small, but the
//! ellipsoid costs only a few more multiplies on the S3's FPU-less f64 path
//! and makes the number match what a phone map or ATAK reports. Vincenty can
//! fail to converge for nearly antipodal points; those fall back to the
//! sphere, which is irrelevant at Remote ID ranges but keeps the function total.

use libm::{atan, atan2, cos, fabs, sin, sqrt, tan};

/// WGS-84 semi-major axis, metres.
pub const WGS84_A: f64 = 6_378_137.0;
/// WGS-84 flattening.
pub const WGS84_F: f64 = 1.0 / 298.257_223_563;
/// IUGG mean Earth radius, metres (for the spherical fallback).
pub const MEAN_RADIUS: f64 = 6_371_008.8;

/// Distance and direction from one point to another.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RangeBearing {
    /// Ellipsoidal (geodesic) distance, metres.
    pub distance_m: f64,
    /// Initial bearing, degrees clockwise from true north, 0 to <360.
    pub bearing_deg: f64,
}

fn to_rad(deg: f64) -> f64 {
    deg * (core::f64::consts::PI / 180.0)
}

fn to_deg(rad: f64) -> f64 {
    rad * (180.0 / core::f64::consts::PI)
}

/// Wrap any angle in degrees to 0 to <360.
fn normalize_deg(d: f64) -> f64 {
    let r = d % 360.0;
    let r = if r < 0.0 { r + 360.0 } else { r };
    // -1e-15 % 360 + 360 rounds to exactly 360.0.
    if r >= 360.0 { 0.0 } else { r }
}

/// Great-circle distance and initial bearing on a sphere of [`MEAN_RADIUS`].
pub fn haversine(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> RangeBearing {
    let (p1, p2) = (to_rad(lat1), to_rad(lat2));
    let dp = p2 - p1;
    let dl = to_rad(lon2 - lon1);
    let h = sin(dp / 2.0) * sin(dp / 2.0) + cos(p1) * cos(p2) * sin(dl / 2.0) * sin(dl / 2.0);
    let c = 2.0 * atan2(sqrt(h), sqrt(1.0 - h));
    let y = sin(dl) * cos(p2);
    let x = cos(p1) * sin(p2) - sin(p1) * cos(p2) * cos(dl);
    RangeBearing {
        distance_m: MEAN_RADIUS * c,
        bearing_deg: normalize_deg(to_deg(atan2(y, x))),
    }
}

/// Geodesic distance and initial bearing on WGS-84 (Vincenty inverse). Same
/// point gives 0 m, bearing 0.
pub fn range_bearing(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> RangeBearing {
    let (a, f) = (WGS84_A, WGS84_F);
    let b = a * (1.0 - f);
    let l = to_rad(lon2 - lon1);
    // Reduced latitudes.
    let u1 = atan((1.0 - f) * tan(to_rad(lat1)));
    let u2 = atan((1.0 - f) * tan(to_rad(lat2)));
    let (sin_u1, cos_u1) = (sin(u1), cos(u1));
    let (sin_u2, cos_u2) = (sin(u2), cos(u2));

    let mut lambda = l;
    for _ in 0..200 {
        let (sin_l, cos_l) = (sin(lambda), cos(lambda));
        let t = cos_u2 * sin_l;
        let s = cos_u1 * sin_u2 - sin_u1 * cos_u2 * cos_l;
        let sin_sigma = sqrt(t * t + s * s);
        if sin_sigma == 0.0 {
            return RangeBearing {
                distance_m: 0.0,
                bearing_deg: 0.0,
            };
        }
        let cos_sigma = sin_u1 * sin_u2 + cos_u1 * cos_u2 * cos_l;
        let sigma = atan2(sin_sigma, cos_sigma);
        let sin_alpha = cos_u1 * cos_u2 * sin_l / sin_sigma;
        let cos2_alpha = 1.0 - sin_alpha * sin_alpha;
        // On the equator cos2_alpha is 0 and the term is defined as 0.
        let cos_2sm = if cos2_alpha != 0.0 {
            cos_sigma - 2.0 * sin_u1 * sin_u2 / cos2_alpha
        } else {
            0.0
        };
        let c = f / 16.0 * cos2_alpha * (4.0 + f * (4.0 - 3.0 * cos2_alpha));
        let prev = lambda;
        lambda = l
            + (1.0 - c)
                * f
                * sin_alpha
                * (sigma
                    + c * sin_sigma * (cos_2sm + c * cos_sigma * (-1.0 + 2.0 * cos_2sm * cos_2sm)));
        if fabs(lambda - prev) < 1e-12 {
            let u_sq = cos2_alpha * (a * a - b * b) / (b * b);
            let big_a =
                1.0 + u_sq / 16384.0 * (4096.0 + u_sq * (-768.0 + u_sq * (320.0 - 175.0 * u_sq)));
            let big_b = u_sq / 1024.0 * (256.0 + u_sq * (-128.0 + u_sq * (74.0 - 47.0 * u_sq)));
            let delta_sigma = big_b
                * sin_sigma
                * (cos_2sm
                    + big_b / 4.0
                        * (cos_sigma * (-1.0 + 2.0 * cos_2sm * cos_2sm)
                            - big_b / 6.0
                                * cos_2sm
                                * (-3.0 + 4.0 * sin_sigma * sin_sigma)
                                * (-3.0 + 4.0 * cos_2sm * cos_2sm)));
            let (sin_l, cos_l) = (sin(lambda), cos(lambda));
            let alpha1 = atan2(cos_u2 * sin_l, cos_u1 * sin_u2 - sin_u1 * cos_u2 * cos_l);
            return RangeBearing {
                distance_m: b * big_a * (sigma - delta_sigma),
                bearing_deg: normalize_deg(to_deg(alpha1)),
            };
        }
    }
    // Nearly antipodal: Vincenty does not converge. Never happens at radio range.
    haversine(lat1, lon1, lat2, lon2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dms(d: f64, m: f64, s: f64) -> f64 {
        d + m / 60.0 + s / 3600.0
    }

    /// Vincenty's own worked example (Geoscience Australia): Flinders Peak to
    /// Buninyong, 54 972.271 m at 306°52'05.37".
    #[test]
    fn flinders_peak_to_buninyong() {
        let r = range_bearing(
            -dms(37.0, 57.0, 3.72030),
            dms(144.0, 25.0, 29.52440),
            -dms(37.0, 39.0, 10.15610),
            dms(143.0, 55.0, 35.38390),
        );
        assert!((r.distance_m - 54_972.271).abs() < 0.001, "{r:?}");
        assert!(
            (r.bearing_deg - dms(306.0, 52.0, 5.37)).abs() < 0.5 / 3600.0,
            "{r:?}"
        );
    }

    /// Philadelphia City Hall (William Penn's statue) to Independence Hall.
    /// Reference: GeographicLib (Karney's algorithm, independent of
    /// Vincenty) gives 1 363.931 m at 107.4958° for these coordinates; the
    /// spherical value must agree to within the sphere's known 0.5 % error.
    #[test]
    fn city_hall_to_independence_hall() {
        let (a, b) = ((39.952_583, -75.165_222), (39.948_889, -75.150_000));
        let r = range_bearing(a.0, a.1, b.0, b.1);
        let h = haversine(a.0, a.1, b.0, b.1);
        assert!((r.distance_m - 1_363.931).abs() < 0.001, "{r:?}");
        assert!((r.bearing_deg - 107.4958).abs() < 0.0001, "{r:?}");
        assert!((h.distance_m - r.distance_m).abs() / r.distance_m < 0.005);
        assert!((h.bearing_deg - r.bearing_deg).abs() < 0.2);
        // Reverse direction: same distance, roughly opposite bearing.
        let back = range_bearing(b.0, b.1, a.0, a.1);
        assert!((back.distance_m - r.distance_m).abs() < 1e-6);
        assert!((back.bearing_deg - (r.bearing_deg + 180.0)).abs() < 0.05);
    }

    #[test]
    fn cardinal_directions_and_degenerate_cases() {
        let n = range_bearing(40.0, -75.0, 40.01, -75.0);
        assert!(n.bearing_deg.abs() < 1e-9 || (n.bearing_deg - 360.0).abs() < 1e-9);
        // GeographicLib: 1 110.347 m.
        assert!((n.distance_m - 1_110.347).abs() < 0.001, "{n:?}");
        let e = range_bearing(40.0, -75.0, 40.0, -74.99);
        assert!((e.bearing_deg - 90.0).abs() < 0.01);
        let s = range_bearing(40.0, -75.0, 39.99, -75.0);
        assert!((s.bearing_deg - 180.0).abs() < 1e-9);
        let w = range_bearing(40.0, -75.0, 40.0, -75.01);
        assert!((w.bearing_deg - 270.0).abs() < 0.01);
        let same = range_bearing(40.0, -75.0, 40.0, -75.0);
        assert_eq!(same.distance_m, 0.0);
        // Equatorial and antipodal inputs stay finite.
        let eq = range_bearing(0.0, 0.0, 0.0, 1.0);
        assert!((eq.distance_m - 111_319.491).abs() < 0.001, "{eq:?}");
        let anti = range_bearing(0.0, 0.0, 0.5, 179.7);
        assert!(anti.distance_m.is_finite() && anti.distance_m > 19_000_000.0);
    }
}
