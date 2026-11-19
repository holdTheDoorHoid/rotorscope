//! Where the sun is, for the automatic day / night theme.
//!
//! NOAA's solar position algorithm (the one in its "Solar Calculations"
//! spreadsheet, after Meeus, *Astronomical Algorithms*): the sun's
//! elevation at an instant, from latitude, longitude and UTC. Good to well
//! under a minute of sunrise time between 1901 and 2099, which is far more
//! than a theme switch needs. Pure, `no_std`, f64 via `libm` (a Julian date
//! does not fit an f32).
//!
//! Sunrise and sunset are the moments the sun's centre is 0.833° below the
//! horizon (refraction plus its own radius); civil dawn and dusk are 6°
//! below.

use crate::ui::SwitchAt;

/// Elevation of the sun's centre (degrees) that counts as "day" for each
/// switching rule.
pub const fn threshold_deg(at: SwitchAt) -> f64 {
    match at {
        SwitchAt::Sunrise => -0.833,
        SwitchAt::Twilight => -6.0,
    }
}

fn rad(d: f64) -> f64 {
    d * (core::f64::consts::PI / 180.0)
}
fn deg(r: f64) -> f64 {
    r * (180.0 / core::f64::consts::PI)
}

/// The sun's geometric elevation above the horizon, degrees, at `utc_ms`
/// (Unix milliseconds) seen from `lat`, `lon` (degrees, east positive). No
/// refraction: the thresholds include it.
pub fn elevation_deg(utc_ms: i64, lat: f64, lon: f64) -> f64 {
    let days = utc_ms as f64 / 86_400_000.0;
    let jd = days + 2_440_587.5;
    let t = (jd - 2_451_545.0) / 36_525.0;
    let l0 = libm::fmod(280.46646 + t * (36_000.76983 + t * 0.000_303_2), 360.0);
    let m = 357.529_11 + t * (35_999.050_29 - 0.000_153_7 * t);
    let e = 0.016_708_634 - t * (0.000_042_037 + 0.000_000_126_7 * t);
    let c = libm::sin(rad(m)) * (1.914_602 - t * (0.004_817 + 0.000_014 * t))
        + libm::sin(rad(2.0 * m)) * (0.019_993 - 0.000_101 * t)
        + libm::sin(rad(3.0 * m)) * 0.000_289;
    let true_long = l0 + c;
    let omega = 125.04 - 1_934.136 * t;
    let app_long = true_long - 0.005_69 - 0.004_78 * libm::sin(rad(omega));
    let eps0 = 23.0 + (26.0 + (21.448 - t * (46.815 + t * (0.000_59 - t * 0.001_813))) / 60.0) / 60.0;
    let eps = eps0 + 0.002_56 * libm::cos(rad(omega));
    let decl = libm::asin(libm::sin(rad(eps)) * libm::sin(rad(app_long)));
    let y = libm::tan(rad(eps / 2.0));
    let y = y * y;
    let (l0r, mr) = (rad(l0), rad(m));
    // Equation of time, minutes.
    let eot = 4.0
        * deg(y * libm::sin(2.0 * l0r) - 2.0 * e * libm::sin(mr) + 4.0 * e * y * libm::sin(mr) * libm::cos(2.0 * l0r)
            - 0.5 * y * y * libm::sin(4.0 * l0r)
            - 1.25 * e * e * libm::sin(2.0 * mr));
    let minutes = (days - libm::floor(days)) * 1_440.0;
    let tst = libm::fmod(minutes + eot + 4.0 * lon, 1_440.0);
    let ha = rad(tst / 4.0 - 180.0);
    let latr = rad(lat);
    let cz = libm::sin(latr) * libm::sin(decl) + libm::cos(latr) * libm::cos(decl) * libm::cos(ha);
    90.0 - deg(libm::acos(cz.clamp(-1.0, 1.0)))
}

/// Day by the chosen rule: the sun is above the threshold.
pub fn is_day(utc_ms: i64, lat: f64, lon: f64, at: SwitchAt) -> bool {
    elevation_deg(utc_ms, lat, lon) > threshold_deg(at)
}

/// The next time (Unix ms, to about 10 s) day turns to night or night to
/// day after `utc_ms`, by the chosen rule; `None` if it does not within
/// 26 hours (polar day or night). Steps of 15 minutes, then halving: about
/// 115 sun positions.
pub fn next_change(utc_ms: i64, lat: f64, lon: f64, at: SwitchAt) -> Option<i64> {
    const STEP: i64 = 15 * 60_000;
    let now = is_day(utc_ms, lat, lon, at);
    let mut lo = utc_ms;
    let mut hi = utc_ms;
    let mut found = false;
    for _ in 0..(26 * 4) {
        hi = lo + STEP;
        if is_day(hi, lat, lon, at) != now {
            found = true;
            break;
        }
        lo = hi;
    }
    if !found {
        return None;
    }
    while hi - lo > 10_000 {
        let mid = lo + (hi - lo) / 2;
        if is_day(mid, lat, lon, at) == now {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Some(hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unix ms of a UTC date and time.
    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        // Days from civil (Howard Hinnant's algorithm).
        let y = if mo <= 2 { y - 1 } else { y } as i64;
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let mp = (mo as i64 + 9) % 12;
        let doy = (153 * mp + 2) / 5 + d as i64 - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = era * 146_097 + doe - 719_468;
        (days * 86_400 + h as i64 * 3600 + mi as i64 * 60) * 1000
    }

    /// How far (minutes) a found change is from the expected time.
    fn off_min(got: Option<i64>, want: i64) -> f64 {
        (got.expect("a change") - want).abs() as f64 / 60_000.0
    }

    const PHL: (f64, f64) = (39.95, -75.16);

    /// US Naval Observatory times for Philadelphia (aa.usno.navy.mil,
    /// rounded to the minute), all within two minutes.
    #[test]
    fn philadelphia_matches_the_naval_observatory() {
        let (lat, lon) = PHL;
        // (date, civil dawn, sunrise, sunset, civil dusk) in UTC.
        let cases = [
            ((2026, 3, 20), (10, 37), (11, 4), (23, 13), (23, 40)),
            ((2026, 11, 18), (11, 20), (11, 49), (21, 42), (22, 11)),
            ((2026, 12, 21), (11, 48), (12, 19), (21, 39), (22, 9)),
        ];
        for ((y, mo, d), dawn, rise, set, dusk) in cases {
            let midnight = utc(y, mo, d, 0, 0) + 4 * 3_600_000; // local night
            let at = |hm: (u32, u32)| utc(y, mo, d, hm.0, hm.1);
            let r = next_change(midnight, lat, lon, SwitchAt::Sunrise);
            assert!(off_min(r, at(rise)) <= 2.0, "{y}-{mo}-{d} sunrise");
            let s = next_change(r.unwrap() + 60_000, lat, lon, SwitchAt::Sunrise);
            assert!(off_min(s, at(set)) <= 2.0, "{y}-{mo}-{d} sunset");
            let a = next_change(midnight, lat, lon, SwitchAt::Twilight);
            assert!(off_min(a, at(dawn)) <= 2.0, "{y}-{mo}-{d} dawn");
            let k = next_change(a.unwrap() + 60_000, lat, lon, SwitchAt::Twilight);
            assert!(off_min(k, at(dusk)) <= 2.0, "{y}-{mo}-{d} dusk");
        }
        // Midsummer: the sun sets at 00:33 UTC on the 21st (20:33 local on
        // the 20th) and rises at 09:32; dusk 01:05, dawn 08:59.
        let noon = utc(2026, 6, 20, 17, 0);
        assert!(off_min(next_change(noon, lat, lon, SwitchAt::Sunrise), utc(2026, 6, 21, 0, 33)) <= 2.0);
        assert!(off_min(next_change(noon, lat, lon, SwitchAt::Twilight), utc(2026, 6, 21, 1, 5)) <= 2.0);
        let night = utc(2026, 6, 21, 4, 0);
        assert!(off_min(next_change(night, lat, lon, SwitchAt::Sunrise), utc(2026, 6, 21, 9, 32)) <= 2.0);
        assert!(off_min(next_change(night, lat, lon, SwitchAt::Twilight), utc(2026, 6, 21, 8, 59)) <= 2.0);
        assert!(is_day(utc(2026, 6, 21, 17, 0), lat, lon, SwitchAt::Sunrise));
        assert!(!is_day(utc(2026, 6, 21, 4, 0), lat, lon, SwitchAt::Twilight));
    }

    /// Southern hemisphere, east longitude: Sydney at midwinter (USNO:
    /// sets 06:54 UTC, rises 21:00 UTC).
    #[test]
    fn sydney_midwinter() {
        let (lat, lon) = (-33.87, 151.21);
        let r = next_change(utc(2026, 6, 21, 3, 0), lat, lon, SwitchAt::Sunrise);
        assert!(off_min(r, utc(2026, 6, 21, 6, 54)) <= 2.0);
        let r = next_change(utc(2026, 6, 21, 12, 0), lat, lon, SwitchAt::Sunrise);
        assert!(off_min(r, utc(2026, 6, 21, 21, 0)) <= 2.0);
    }

    /// Tromsø: midnight sun in June (no change at all), polar night in
    /// December with a few hours of civil twilight (USNO: 08:31 to 12:53).
    #[test]
    fn polar_day_and_night() {
        let (lat, lon) = (69.65, 18.96);
        let june = utc(2026, 6, 21, 0, 0);
        for at in [SwitchAt::Sunrise, SwitchAt::Twilight] {
            assert_eq!(next_change(june, lat, lon, at), None);
            assert!(is_day(june, lat, lon, at), "the sun is up at midnight");
        }
        let dec = utc(2026, 12, 21, 0, 0);
        assert_eq!(next_change(dec, lat, lon, SwitchAt::Sunrise), None);
        assert!(!is_day(utc(2026, 12, 21, 10, 45), lat, lon, SwitchAt::Sunrise), "no sunrise at noon");
        let dawn = next_change(dec, lat, lon, SwitchAt::Twilight);
        assert!(off_min(dawn, utc(2026, 12, 21, 8, 31)) <= 2.0);
        let dusk = next_change(dawn.unwrap() + 60_000, lat, lon, SwitchAt::Twilight);
        assert!(off_min(dusk, utc(2026, 12, 21, 12, 53)) <= 2.0);
    }
}
