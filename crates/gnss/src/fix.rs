//! The receiver-independent navigation solution.

/// Kind of position solution. Mirrors UBX NAV-PVT `fixType`; NMEA input is mapped onto it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FixType {
    /// No position.
    None,
    /// Dead reckoning only (no satellites).
    DeadReckoning,
    /// 2D fix (altitude not solved).
    TwoD,
    /// 3D fix.
    ThreeD,
    /// GNSS combined with dead reckoning.
    GnssDeadReckoning,
    /// Time-only fix: UTC is good, position is not.
    TimeOnly,
}

impl FixType {
    pub(crate) fn from_ubx(v: u8) -> FixType {
        match v {
            1 => FixType::DeadReckoning,
            2 => FixType::TwoD,
            3 => FixType::ThreeD,
            4 => FixType::GnssDeadReckoning,
            5 => FixType::TimeOnly,
            _ => FixType::None,
        }
    }

    /// True if this kind of fix carries a position at all.
    pub fn has_position(self) -> bool {
        !matches!(self, FixType::None | FixType::TimeOnly)
    }

    /// True if altitude is solved.
    pub fn has_altitude(self) -> bool {
        matches!(self, FixType::ThreeD | FixType::GnssDeadReckoning)
    }
}

/// Where a [`Fix`] came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// UBX-NAV-PVT (full accuracy and time information).
    UbxNavPvt,
    /// NMEA GGA and/or RMC (no accuracy estimates).
    Nmea,
}

/// A UTC date and time. Only ever produced when the receiver says both date and time are valid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UtcTime {
    /// Four-digit year.
    pub year: u16,
    /// 1..=12.
    pub month: u8,
    /// 1..=31.
    pub day: u8,
    /// 0..=23.
    pub hour: u8,
    /// 0..=59.
    pub minute: u8,
    /// 0..=60 (60 during a leap second).
    pub second: u8,
    /// 0..=999.
    pub millis: u16,
}

impl UtcTime {
    /// Milliseconds since 1970-01-01T00:00:00Z (UTC, leap seconds not counted).
    pub fn unix_ms(&self) -> i64 {
        let days = days_from_civil(self.year as i64, self.month as i64, self.day as i64);
        let secs = days * 86_400
            + self.hour as i64 * 3600
            + self.minute as i64 * 60
            + self.second as i64;
        secs * 1000 + self.millis as i64
    }

    pub(crate) fn is_sane(&self) -> bool {
        (1..=12).contains(&self.month)
            && (1..=31).contains(&self.day)
            && self.hour < 24
            && self.minute < 60
            && self.second <= 60
            && self.millis < 1000
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// One navigation solution.
///
/// Optional fields are `None` when the source cannot supply them (every accuracy field when
/// the source is NMEA) or when they are meaningless for the current fix type.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fix {
    /// Which message(s) produced this.
    pub source: Source,
    /// Solution type.
    pub fix: FixType,
    /// The receiver considers the fix valid (NAV-PVT `gnssFixOK`; NMEA status/quality).
    pub fix_ok: bool,
    /// `lat`/`lon` hold a real position. When false they are 0.0; do not use them.
    pub position_valid: bool,
    /// Latitude, degrees, +N.
    pub lat: f64,
    /// Longitude, degrees, +E.
    pub lon: f64,
    /// Height above mean sea level, metres.
    pub alt_msl_m: Option<f32>,
    /// Height above the WGS-84 ellipsoid, metres. This is what Remote ID geodetic altitude
    /// compares against. From NMEA it is `MSL + geoid separation`.
    pub alt_ellipsoid_m: Option<f32>,
    /// Horizontal accuracy estimate, metres (UBX only).
    pub h_acc_m: Option<f32>,
    /// Vertical accuracy estimate, metres (UBX only).
    pub v_acc_m: Option<f32>,
    /// Satellites used in the solution.
    pub sats_used: u8,
    /// Ground speed, m/s.
    pub speed_mps: Option<f32>,
    /// Heading of motion, degrees true, 0..360. Noise when nearly stationary.
    pub heading_deg: Option<f32>,
    /// Position DOP (UBX only).
    pub pdop: Option<f32>,
    /// Horizontal DOP (NMEA GGA only).
    pub hdop: Option<f32>,
    /// UTC date and time; `Some` only when both date and time are valid.
    pub utc: Option<UtcTime>,
    /// UBX only: the time of day has no whole-second uncertainty.
    pub utc_fully_resolved: bool,
    /// UBX only: UTC accuracy estimate in nanoseconds.
    pub time_acc_ns: Option<u32>,
    /// UBX only: GPS time of week of the epoch, ms.
    pub itow_ms: Option<u32>,
}

impl Fix {
    /// The position as `(lat, lon)` if there is one.
    pub fn position(&self) -> Option<(f64, f64)> {
        if self.position_valid { Some((self.lat, self.lon)) } else { None }
    }

    /// Good enough to show direction and range: the receiver flags the fix valid and it is a
    /// 3D fix (or GNSS+dead-reckoning), or a 2D fix whose `h_acc_m` is known and below 50 m.
    /// A 2D fix from NMEA (no accuracy) is not usable.
    pub fn is_usable(&self) -> bool {
        if !(self.fix_ok && self.position_valid) {
            return false;
        }
        match self.fix {
            FixType::ThreeD | FixType::GnssDeadReckoning => true,
            FixType::TwoD => matches!(self.h_acc_m, Some(a) if a < 50.0),
            _ => false,
        }
    }

    pub(crate) fn empty(source: Source) -> Fix {
        Fix {
            source,
            fix: FixType::None,
            fix_ok: false,
            position_valid: false,
            lat: 0.0,
            lon: 0.0,
            alt_msl_m: None,
            alt_ellipsoid_m: None,
            h_acc_m: None,
            v_acc_m: None,
            sats_used: 0,
            speed_mps: None,
            heading_deg: None,
            pdop: None,
            hdop: None,
            utc: None,
            utc_fully_resolved: false,
            time_acc_ns: None,
            itow_ms: None,
        }
    }
}
