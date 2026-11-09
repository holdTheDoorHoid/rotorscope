//! UTC. CoT events and the CSV `utc` column need it; nothing else does.
//!
//! Sources, best first:
//! 1. GPS time (`gnss` seam; none until the MAX-M10S is integrated).
//! 2. `demo` builds only: the build machine's clock plus uptime, so CoT can be
//!    exercised on the bench. Reported as `"utc_src":"build"` on USB so it is
//!    never mistaken for real time.
//!
//! Without either, `utc_ms` is `None`: the phone page shows no clock and CoT
//! is skipped (counted as `cot_no_utc`). A later option is SNTP in station mode.

use crate::gnss;

/// Where UTC currently comes from, for the USB status line.
pub fn source(now_ms: u64) -> Option<&'static str> {
    if gnss::utc_ms(now_ms).is_some() {
        Some("gps")
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
