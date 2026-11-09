//! UTC. CoT events, the CSV `utc` column, SD file names and timestamps need
//! it; nothing else does.
//!
//! Sources, best first:
//! 1. GPS time (`gnss`). Anchored to the PPS edge when one arrived in the
//!    last second (`"utc_src":"gps_pps"`, sub-millisecond), otherwise to the
//!    time NAV-PVT arrived (`"gps"`, late by the receiver's output delay plus
//!    up to one 100 ms I2C poll).
//! 2. `demo` builds only: the build machine's clock plus uptime, so CoT can be
//!    exercised on the bench. Reported as `"utc_src":"build"` on USB so it is
//!    never mistaken for real time. A GPS, when present, overrides it.
//!
//! Without either, `utc_ms` is `None`: the phone page shows no clock and CoT
//! is skipped (counted as `cot_no_utc`). A later option is SNTP in station mode.

use crate::gnss;

/// Where UTC currently comes from, for the USB status line.
pub fn source(now_ms: u64) -> Option<&'static str> {
    if gnss::utc_ms(now_ms).is_some() {
        Some(if gnss::utc_from_pps() { "gps_pps" } else { "gps" })
    } else if demo_utc(now_ms).is_some() {
        Some("build")
    } else {
        None
    }
}

/// Unix milliseconds for hub time `now_ms`, when known.
pub fn utc_ms(now_ms: u64) -> Option<u64> {
    gnss::utc_ms(now_ms).or_else(|| demo_utc(now_ms))
}

#[cfg(feature = "demo")]
fn demo_utc(now_ms: u64) -> Option<u64> {
    let built: u64 = env!("HUB_BUILD_UNIX_MS").parse().ok()?;
    Some(built + now_ms)
}

#[cfg(not(feature = "demo"))]
fn demo_utc(_now_ms: u64) -> Option<u64> {
    None
}

/// A UTC calendar date and time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Civil {
    pub year: i32,
    /// 1..=12
    pub month: u8,
    /// 1..=31
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

/// Unix milliseconds to a UTC calendar date (proleptic Gregorian). Uses the
/// well-known days-to-civil algorithm by Howard Hinnant (public domain
/// description), the inverse of the one `crates/gnss` uses.
pub fn civil(unix_ms: u64) -> Civil {
    let secs = unix_ms / 1000;
    let days = (secs / 86_400) as i64;
    let sod = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let year = (yoe + era * 400 + i64::from(month <= 2)) as i32;
    Civil {
        year,
        month,
        day,
        hour: (sod / 3600) as u8,
        minute: (sod % 3600 / 60) as u8,
        second: (sod % 60) as u8,
    }
}
