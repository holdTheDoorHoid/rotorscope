//! The simulated flight: a slow circle around a fixed centre.
//!
//! Nothing here is a real aircraft. The centre is chosen at build time
//! (`RID_TEST_LAT` / `RID_TEST_LON`, default Philadelphia City Hall) so a
//! bench test can be placed wherever the receiver under test is, and the
//! track is small and slow enough that no one could mistake it for a flight.

use core::f64::consts::PI;

/// Default centre: Philadelphia City Hall.
pub const DEFAULT_LAT: f64 = 39.9526;
pub const DEFAULT_LON: f64 = -75.1652;

/// Circle radius in metres.
pub const RADIUS_M: f64 = 50.0;
/// Seconds per lap.
pub const PERIOD_S: f64 = 60.0;
/// Height above ground, metres (Location "height", type AGL).
pub const HEIGHT_AGL_M: f32 = 50.0;
/// Assumed ground elevation at the centre, metres above the WGS-84
/// ellipsoid. A bench rig has no GPS; this only has to be plausible so
/// receivers that show geodetic altitude print something sensible.
pub const GROUND_M: f32 = 15.0;

/// Mean Earth radius (m), the same spherical approximation receivers use for
/// range/bearing; at 50 m the error is far below anything a receiver shows.
const EARTH_R: f64 = 6_371_000.0;

#[derive(Debug, Clone, Copy)]
pub struct Centre {
    pub lat: f64,
    pub lon: f64,
    /// False when a build-time override was present but unparseable or out of
    /// range, so the default was used instead. Reported at boot.
    pub from_env_ok: bool,
}

/// The centre, from the build-time environment if set and valid.
pub fn centre() -> Centre {
    let lat = option_env!("RID_TEST_LAT").map(|s| s.trim().parse::<f64>());
    let lon = option_env!("RID_TEST_LON").map(|s| s.trim().parse::<f64>());
    let mut c = Centre {
        lat: DEFAULT_LAT,
        lon: DEFAULT_LON,
        from_env_ok: true,
    };
    match lat {
        Some(Ok(v)) if (-89.0..=89.0).contains(&v) => c.lat = v,
        Some(_) => c.from_env_ok = false,
        None => {}
    }
    match lon {
        Some(Ok(v)) if (-180.0..=180.0).contains(&v) => c.lon = v,
        Some(_) => c.from_env_ok = false,
        None => {}
    }
    c
}

/// Where the simulated aircraft is at one instant.
#[derive(Debug, Clone, Copy)]
pub struct Fix {
    pub lat: f64,
    pub lon: f64,
    /// Track over ground, degrees clockwise from true north, 0..360.
    pub heading_deg: f32,
    /// Ground speed, m/s (constant: 2*pi*R/T, about 5.2 m/s).
    pub speed_mps: f32,
}

/// Position on the circle `t_s` seconds after boot. The aircraft starts due
/// north of the centre and goes clockwise (north -> east -> south -> west).
pub fn at(c: &Centre, t_s: f64) -> Fix {
    let theta = 2.0 * PI * (t_s % PERIOD_S) / PERIOD_S;
    let north = RADIUS_M * libm::cos(theta);
    let east = RADIUS_M * libm::sin(theta);
    let lat = c.lat + (north / EARTH_R).to_degrees();
    let lon = c.lon + (east / (EARTH_R * libm::cos(c.lat.to_radians()))).to_degrees();
    // Velocity is the derivative: (dn, de) = (-sin, cos) * R*omega, so the
    // heading is theta + 90 degrees.
    let mut heading = theta.to_degrees() + 90.0;
    if heading >= 360.0 {
        heading -= 360.0;
    }
    Fix {
        lat,
        lon,
        heading_deg: heading as f32,
        speed_mps: (2.0 * PI * RADIUS_M / PERIOD_S) as f32,
    }
}
