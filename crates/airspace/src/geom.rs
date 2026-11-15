//! Small geometry: distance, point in polygon.

use libm::{asin, cos, sin, sqrt};

pub const M_PER_FT: f64 = 0.3048;
pub const FT_PER_M: f64 = 1.0 / 0.3048;
/// Mean Earth radius, metres.
pub const EARTH_R: f64 = 6_371_008.8;

pub(crate) fn rad(d: f64) -> f64 {
    d * (core::f64::consts::PI / 180.0)
}

/// Great-circle distance in metres (haversine). Within 0.5 % of the
/// ellipsoid, which is plenty for zone edges and rule margins.
pub fn distance_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (rad(lat1), rad(lat2));
    let dp = p2 - p1;
    let dl = rad(lon2 - lon1);
    let h = sin(dp / 2.0) * sin(dp / 2.0) + cos(p1) * cos(p2) * sin(dl / 2.0) * sin(dl / 2.0);
    2.0 * EARTH_R * asin(sqrt(h.min(1.0)))
}

/// Even-odd point in polygon over every ring (outer rings and holes alike,
/// so holes and multi-part areas both come out right). `pt(i)` returns
/// point `i` as (lat, lon) in 1e-7 degrees; `rings` are the ring lengths in
/// order. Rings are treated as closed whether or not the last point
/// repeats the first.
pub fn point_in_rings(
    lat_e7: i32,
    lon_e7: i32,
    rings: impl Iterator<Item = usize>,
    pt: impl Fn(usize) -> (i32, i32),
) -> bool {
    let (y, x) = (i64::from(lat_e7), i64::from(lon_e7));
    let mut inside = false;
    let mut start = 0usize;
    for len in rings {
        if len >= 3 {
            let mut j = start + len - 1;
            for i in start..start + len {
                let (yi, xi) = pt(i);
                let (yj, xj) = pt(j);
                let (yi, xi, yj, xj) = (i64::from(yi), i64::from(xi), i64::from(yj), i64::from(xj));
                if (yi > y) != (yj > y) {
                    // x < xi + (y - yi) * (xj - xi) / (yj - yi), in integers:
                    // compare (x - xi) * (yj - yi) with (y - yi) * (xj - xi),
                    // flipping for a negative denominator. Each factor is a
                    // coordinate difference in 1e-7 deg (at most 3.6e9 and
                    // 1.8e9), so the products fit in i64 (no i128: slow on
                    // a 32-bit hub).
                    let lhs = (x - xi) * (yj - yi);
                    let rhs = (y - yi) * (xj - xi);
                    let left = if yj > yi { lhs < rhs } else { lhs > rhs };
                    if left {
                        inside = !inside;
                    }
                }
                j = i;
            }
        }
        start += len;
    }
    inside
}
