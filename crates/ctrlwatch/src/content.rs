//! Decoded control-link content. Only produced when
//! [`crate::Config::decode_content`] is set (the owner's opt-in of
//! 2026-11-17); see the crate docs for the legal line.

/// Max RC channels carried in one [`Channels`].
pub const MAX_CHANNELS: usize = 16;

/// One item of decoded content.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ControlContent {
    /// Stick / switch positions sent to the aircraft.
    Channels(Channels),
    /// Telemetry from the aircraft (or link statistics).
    Telemetry(Telemetry),
    /// A position: the aircraft's own GPS, or its home point. Shaped so the
    /// tracker can take it as a drone position (WGS-84, 1e-7 degrees).
    Position(Position),
    /// The packet is known to carry content, but it cannot be read: it is
    /// encrypted, or in a format this crate does not decode. Nothing is
    /// attempted against encryption.
    Opaque(Opaque),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Opaque {
    /// The system's encryption is on (SiK AES, mLRS privacy).
    Encrypted,
    /// Payload present but no recognisable frames (could be encrypted, a
    /// compressed MAVLink variant, or a partial frame).
    Unparsed,
    /// FLRC / FSK link: content needs the binding phrase.
    NeedsBindPhrase,
}

/// RC channel values in microseconds of servo pulse (1500 = centre).
/// `present` has bit `n` set for each channel this packet carried; the
/// others are 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Channels {
    pub us: [u16; MAX_CHANNELS],
    pub present: u32,
    /// The link's own arm flag, where it sends one (ELRS).
    pub armed: Option<bool>,
}

impl Channels {
    pub fn set(&mut self, ch: usize, us: u16) {
        if ch < MAX_CHANNELS {
            self.us[ch] = us;
            self.present |= 1 << ch;
        }
    }

    pub fn get(&self, ch: usize) -> Option<u16> {
        (ch < MAX_CHANNELS && self.present & (1 << ch) != 0).then(|| self.us[ch])
    }
}

/// Altitude reference of a [`Position`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum AltRef {
    /// Above mean sea level (MAVLink GPS_RAW_INT / GLOBAL_POSITION_INT
    /// `alt`, CRSF GPS altitude).
    Msl,
    /// Above the home / take-off point.
    Home,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PositionKind {
    /// Where the aircraft is.
    Aircraft,
    /// The aircraft's home point (often where the pilot is).
    Home,
}

/// A WGS-84 position decoded from telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Position {
    pub kind: PositionKind,
    pub lat_e7: i32,
    pub lon_e7: i32,
    /// Altitude, mm, with its reference.
    pub alt_mm: Option<(i32, AltRef)>,
    /// Second altitude above home, mm, when the message has both.
    pub rel_alt_mm: Option<i32>,
    pub ground_speed_cms: Option<u16>,
    /// Course / heading, centidegrees.
    pub heading_cdeg: Option<u16>,
    pub satellites: Option<u8>,
    /// GPS fix type (MAVLink numbering: 0-1 none, 2 = 2D, 3 = 3D, ...).
    pub fix_type: Option<u8>,
    pub source: PositionSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PositionSource {
    CrsfGps,
    MavGpsRawInt,
    MavGlobalPositionInt,
    MavHomePosition,
}

impl Position {
    /// Is this a plausible fix (not the 0,0 a GPS reports before lock)?
    pub fn is_plausible(&self) -> bool {
        (self.lat_e7 != 0 || self.lon_e7 != 0)
            && self.lat_e7.unsigned_abs() <= 900_000_000
            && self.lon_e7.unsigned_abs() <= 1_800_000_000
            && self.fix_type.is_none_or(|f| f >= 2)
    }
}

/// Telemetry items.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Telemetry {
    Battery {
        /// mV.
        voltage_mv: u32,
        /// mA, if reported.
        current_ma: Option<i32>,
        /// mAh used, if reported.
        used_mah: Option<u32>,
        /// Remaining, %, if reported.
        remaining_pct: Option<u8>,
    },
    /// Link statistics as the link reports them about itself.
    Link {
        uplink_rssi_dbm: i16,
        uplink_lq: u8,
        uplink_snr_db: i8,
        downlink_rssi_dbm: Option<i16>,
        downlink_lq: Option<u8>,
    },
    /// CRSF flight-mode string / MAVLink-derived mode name.
    FlightMode(heapless::String<16>),
    Attitude {
        /// Centidegrees.
        pitch_cdeg: i32,
        roll_cdeg: i32,
        yaw_cdeg: i32,
    },
    BaroAltitude {
        alt_dm: i32,
        vario_cms: Option<i16>,
    },
    Vario {
        cms: i16,
    },
    /// MAVLink HEARTBEAT.
    Heartbeat {
        sysid: u8,
        compid: u8,
        mav_type: u8,
        autopilot: u8,
        base_mode: u8,
        custom_mode: u32,
        system_status: u8,
    },
    /// Some other frame was decoded (its CRC passed); only its type is kept.
    Other {
        /// CRSF frame type or MAVLink message ID.
        kind: u32,
    },
}
