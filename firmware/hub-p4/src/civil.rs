//! Unix milliseconds to a UTC calendar date (for SD file names, FAT
//! timestamps). Copied from the S3 hub (`firmware/hub/src/clock.rs`).

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
