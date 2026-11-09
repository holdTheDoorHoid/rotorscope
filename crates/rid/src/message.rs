//! ASTM F3411 message decoding.
//!
//! Byte layouts follow ASTM F3411-22a and ASD-STAN EN 4709-002, cross-checked
//! field by field against opendroneid-core-c (`opendroneid.h`). All multi-byte
//! fields are little-endian. Values are decoded to SI units; every sentinel the
//! standard defines for "invalid, no value or unknown" becomes `None`.
//!
//! Decoding is deliberately lenient where the standard reserves values: an
//! unknown enum code becomes a `Reserved(u8)` variant rather than an error, so
//! a newer transmitter still yields everything an older receiver understands.
//! The only hard errors are an unknown message type and an Authentication
//! page whose length fields contradict each other (core-c rejects those too).

use crate::{Error, MAX_PACK, MESSAGE_SIZE, RawMessage};

/// Message type nibbles (high nibble of byte 0).
pub mod kind {
    pub const BASIC_ID: u8 = 0x0;
    pub const LOCATION: u8 = 0x1;
    pub const AUTH: u8 = 0x2;
    pub const SELF_ID: u8 = 0x3;
    pub const SYSTEM: u8 = 0x4;
    pub const OPERATOR_ID: u8 = 0x5;
    pub const MESSAGE_PACK: u8 = 0xF;
}

/// Protocol version this crate writes when encoding (F3411-22a). Decoding
/// accepts any version, as core-c does: the standard only ever adds fields.
pub const PROTOCOL_VERSION: u8 = 2;

/// Seconds from the Unix epoch to 2019-01-01T00:00:00Z, the epoch of the
/// System and Authentication timestamps.
pub const EPOCH_2019_UNIX: u64 = 1_546_300_800;

/// Number of pages an Authentication message set can span (4-bit page index).
pub const AUTH_MAX_PAGES: u8 = 16;
/// Authentication data bytes carried by page 0.
pub const AUTH_PAGE_ZERO_DATA: usize = 17;
/// Authentication data bytes carried by pages 1-15.
pub const AUTH_PAGE_DATA: usize = 23;

/// One decoded Remote ID message.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Message {
    BasicId(BasicId),
    Location(Location),
    Auth(Auth),
    SelfId(SelfId),
    System(System),
    OperatorId(OperatorId),
}

impl Message {
    /// The message type nibble this message is encoded with (see [`kind`]).
    pub fn kind(&self) -> u8 {
        match self {
            Message::BasicId(_) => kind::BASIC_ID,
            Message::Location(_) => kind::LOCATION,
            Message::Auth(_) => kind::AUTH,
            Message::SelfId(_) => kind::SELF_ID,
            Message::System(_) => kind::SYSTEM,
            Message::OperatorId(_) => kind::OPERATOR_ID,
        }
    }
}

/// Message type nibble of a raw message (0xF for a Message Pack header).
pub fn message_kind(raw: &[u8]) -> Option<u8> {
    raw.first().map(|b| b >> 4)
}

/// Protocol version nibble of a raw message: 0 = F3411-19, 1 = EN 4709-002 /
/// F3411 v1.1 draft, 2 = F3411-22a.
pub fn protocol_version(raw: &RawMessage) -> u8 {
    raw[0] & 0x0F
}

// ---------------------------------------------------------------------------
// Small value types
// ---------------------------------------------------------------------------

/// A fixed-width text field (UAS ID, Self-ID description, Operator ID),
/// stored exactly as transmitted: NUL-padded, not necessarily NUL-terminated.
///
/// With `serde`, it serializes as a JSON string when the content is printable
/// ASCII followed only by NUL padding (the normal case), and otherwise as a
/// lowercase hex string of [`FixedStr::as_bytes`]. A binary UAS ID (UTM UUID,
/// specific session ID) therefore shows as hex; its `id_type` tells which.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct FixedStr<const N: usize>(pub [u8; N]);

impl<const N: usize> FixedStr<N> {
    /// Build from text or bytes, NUL-padding to `N`. Fails if longer than `N`.
    pub fn new(bytes: &[u8]) -> Result<Self, Error> {
        let mut out = [0u8; N];
        out.get_mut(..bytes.len())
            .ok_or(Error::Invalid)?
            .copy_from_slice(bytes);
        Ok(Self(out))
    }

    /// All `N` bytes as transmitted.
    pub fn raw(&self) -> &[u8; N] {
        &self.0
    }

    /// The content with trailing NUL padding removed. Interior NULs (possible
    /// in binary IDs) are kept.
    pub fn as_bytes(&self) -> &[u8] {
        let end = self.0.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
        &self.0[..end]
    }

    /// The text up to the first NUL, if it is valid UTF-8. The standard only
    /// allows ASCII, so `None` means a malformed or binary field.
    pub fn as_str(&self) -> Option<&str> {
        let end = self.0.iter().position(|&b| b == 0).unwrap_or(N);
        core::str::from_utf8(&self.0[..end]).ok()
    }

    /// True for printable ASCII followed only by NUL padding.
    pub fn is_clean_text(&self) -> bool {
        let end = self.0.iter().position(|&b| b == 0).unwrap_or(N);
        self.0[..end].iter().all(|b| (0x20..0x7F).contains(b))
            && self.0[end..].iter().all(|&b| b == 0)
    }
}

impl<const N: usize> Default for FixedStr<N> {
    fn default() -> Self {
        Self([0; N])
    }
}

impl<const N: usize> core::fmt::Debug for FixedStr<N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.is_clean_text() {
            write!(f, "{:?}", self.as_str().unwrap_or(""))
        } else {
            write!(f, "hex:{}", Hex(self.as_bytes()))
        }
    }
}

/// Lowercase hex rendering of a byte slice, without allocating.
pub(crate) struct Hex<'a>(pub &'a [u8]);

impl core::fmt::Display for Hex<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

#[cfg(feature = "serde")]
impl<const N: usize> serde::Serialize for FixedStr<N> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self.as_str() {
            Some(text) if self.is_clean_text() => s.serialize_str(text),
            _ => s.collect_str(&Hex(self.as_bytes())),
        }
    }
}

/// A WGS-84 position in degrees. Transmitted as signed 32-bit integers in
/// units of 1e-7 degree; converted exactly as core-c does (`raw / 1e7`).
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct LatLon {
    pub lat: f64,
    pub lon: f64,
}

impl LatLon {
    /// The standard's "unknown" is lat = lon = 0; out-of-range values are
    /// invalid. Both give `None`.
    fn from_raw(lat: i32, lon: i32) -> Option<Self> {
        if (lat == 0 && lon == 0)
            || !(-900_000_000..=900_000_000).contains(&lat)
            || !(-1_800_000_000..=1_800_000_000).contains(&lon)
        {
            return None;
        }
        Some(Self {
            lat: f64::from(lat) / 1e7,
            lon: f64::from(lon) / 1e7,
        })
    }
}

/// Authentication payload bytes of one page: 17 on page 0, 23 on others.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct AuthData {
    bytes: [u8; AUTH_PAGE_DATA],
    len: u8,
}

impl AuthData {
    /// Wrap up to 23 bytes. Fails if longer.
    pub fn new(data: &[u8]) -> Result<Self, Error> {
        let mut bytes = [0u8; AUTH_PAGE_DATA];
        bytes
            .get_mut(..data.len())
            .ok_or(Error::Invalid)?
            .copy_from_slice(data);
        Ok(Self {
            bytes,
            len: data.len() as u8,
        })
    }

    /// The page's data bytes (including any trailing zero padding).
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len).min(AUTH_PAGE_DATA)]
    }
}

impl core::fmt::Debug for AuthData {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "hex:{}", Hex(self.as_bytes()))
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for AuthData {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(&Hex(self.as_bytes()))
    }
}

code_enum! {
    /// Basic ID: what kind of identifier `uas_id` holds.
    IdType {
        None = 0,
        /// ANSI/CTA-2063-A serial number (ASCII).
        SerialNumber = 1,
        /// Civil aviation authority registration (ASCII).
        CaaRegistration = 2,
        /// UTM-assigned UUID (16 binary bytes).
        UtmAssigned = 3,
        /// Specific session ID: first byte is an ICAO-managed type, rest binary.
        SpecificSession = 4,
    }
}

code_enum! {
    /// Operational status of the aircraft.
    Status {
        Undeclared = 0,
        Ground = 1,
        Airborne = 2,
        Emergency = 3,
        RemoteIdSystemFailure = 4,
    }
}

code_enum! {
    /// Horizontal position accuracy (95 %).
    HorizontalAccuracy {
        Unknown = 0,
        Nm10 = 1,
        Nm4 = 2,
        Nm2 = 3,
        Nm1 = 4,
        Nm0_5 = 5,
        Nm0_3 = 6,
        Nm0_1 = 7,
        Nm0_05 = 8,
        M30 = 9,
        M10 = 10,
        M3 = 11,
        M1 = 12,
    }
}

impl HorizontalAccuracy {
    /// Upper bound in metres, `None` when unknown or reserved.
    pub fn meters(self) -> Option<f32> {
        Some(match self {
            Self::Nm10 => 18520.0,
            Self::Nm4 => 7408.0,
            Self::Nm2 => 3704.0,
            Self::Nm1 => 1852.0,
            Self::Nm0_5 => 926.0,
            Self::Nm0_3 => 555.6,
            Self::Nm0_1 => 185.2,
            Self::Nm0_05 => 92.6,
            Self::M30 => 30.0,
            Self::M10 => 10.0,
            Self::M3 => 3.0,
            Self::M1 => 1.0,
            Self::Unknown | Self::Reserved(_) => return None,
        })
    }
}

code_enum! {
    /// Vertical (geodetic or barometric) accuracy (95 %).
    VerticalAccuracy {
        Unknown = 0,
        M150 = 1,
        M45 = 2,
        M25 = 3,
        M10 = 4,
        M3 = 5,
        M1 = 6,
    }
}

impl VerticalAccuracy {
    /// Upper bound in metres, `None` when unknown or reserved.
    pub fn meters(self) -> Option<f32> {
        Some(match self {
            Self::M150 => 150.0,
            Self::M45 => 45.0,
            Self::M25 => 25.0,
            Self::M10 => 10.0,
            Self::M3 => 3.0,
            Self::M1 => 1.0,
            Self::Unknown | Self::Reserved(_) => return None,
        })
    }
}

code_enum! {
    /// Horizontal speed accuracy (95 %).
    SpeedAccuracy {
        Unknown = 0,
        Mps10 = 1,
        Mps3 = 2,
        Mps1 = 3,
        Mps0_3 = 4,
    }
}

impl SpeedAccuracy {
    /// Upper bound in m/s, `None` when unknown or reserved.
    pub fn meters_per_second(self) -> Option<f32> {
        Some(match self {
            Self::Mps10 => 10.0,
            Self::Mps3 => 3.0,
            Self::Mps1 => 1.0,
            Self::Mps0_3 => 0.3,
            Self::Unknown | Self::Reserved(_) => return None,
        })
    }
}

code_enum! {
    /// Authentication method.
    AuthType {
        None = 0,
        UasIdSignature = 1,
        OperatorIdSignature = 2,
        MessageSetSignature = 3,
        /// Authentication provided by Network Remote ID.
        NetworkRemoteId = 4,
        /// Specific method: first data byte is an ICAO-managed type.
        SpecificAuthentication = 5,
    }
}

code_enum! {
    /// Self-ID description type. 201-255 are for private use.
    DescType {
        Text = 0,
        /// Clarifies an Emergency status.
        Emergency = 1,
        /// Clarifies a non-emergency status.
        ExtendedStatus = 2,
    }
}

code_enum! {
    /// Where the System message's operator position comes from.
    OperatorLocationType {
        Takeoff = 0,
        LiveGnss = 1,
        Fixed = 2,
    }
}

code_enum! {
    /// Which classification scheme `category`/`class` follow.
    ClassificationType {
        Undeclared = 0,
        /// European Union (EN 4709-002).
        Eu = 1,
    }
}

code_enum! {
    /// EU operation category. Meaningful only with [`ClassificationType::Eu`].
    Category {
        Undeclared = 0,
        Open = 1,
        Specific = 2,
        Certified = 3,
    }
}

code_enum! {
    /// EU UA class. Meaningful only with [`ClassificationType::Eu`].
    UaClass {
        Undeclared = 0,
        Class0 = 1,
        Class1 = 2,
        Class2 = 3,
        Class3 = 4,
        Class4 = 5,
        Class5 = 6,
        Class6 = 7,
    }
}

code_enum! {
    /// Operator ID type. 201-255 are for private use.
    OperatorIdType {
        OperatorId = 0,
    }
}

/// Aircraft type. Every 4-bit code is assigned, so there is no escape value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum UaType {
    None,
    /// Fixed wing.
    Aeroplane,
    HelicopterOrMultirotor,
    Gyroplane,
    /// Fixed wing that takes off vertically.
    HybridLift,
    Ornithopter,
    Glider,
    Kite,
    FreeBalloon,
    CaptiveBalloon,
    /// E.g. a blimp.
    Airship,
    /// Unpowered.
    FreeFallParachute,
    Rocket,
    TetheredPoweredAircraft,
    GroundObstacle,
    Other,
}

impl UaType {
    const ALL: [UaType; 16] = [
        Self::None,
        Self::Aeroplane,
        Self::HelicopterOrMultirotor,
        Self::Gyroplane,
        Self::HybridLift,
        Self::Ornithopter,
        Self::Glider,
        Self::Kite,
        Self::FreeBalloon,
        Self::CaptiveBalloon,
        Self::Airship,
        Self::FreeFallParachute,
        Self::Rocket,
        Self::TetheredPoweredAircraft,
        Self::GroundObstacle,
        Self::Other,
    ];

    /// Decode the low nibble of `code`.
    pub fn from_code(code: u8) -> Self {
        Self::ALL[usize::from(code & 0x0F)]
    }

    /// The wire code (0-15).
    pub fn code(self) -> u8 {
        self as u8
    }
}

/// What the Location message's `height` is measured from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum HeightType {
    AboveTakeoff,
    AboveGround,
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// Basic ID (type 0x0): who the aircraft is. A UA may send up to two (e.g. a
/// serial number and a registration), distinguished by `id_type`.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct BasicId {
    pub id_type: IdType,
    pub ua_type: UaType,
    /// 20 bytes. ASCII for serial numbers and CAA registrations; binary for
    /// UTM UUIDs and specific session IDs (use [`FixedStr::as_bytes`]).
    pub uas_id: FixedStr<20>,
}

/// Location/Vector (type 0x1): where the aircraft is and how it moves.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Location {
    pub status: Status,
    pub height_type: HeightType,
    /// Track over ground, degrees clockwise from true north, 0 to <360.
    /// `None` for the "unknown" code (361) or any other value >= 360.
    pub direction: Option<f32>,
    /// Ground speed, m/s (0.25 m/s steps to 63.75, then 0.75 m/s steps).
    /// `None` for the "unknown" code (255 m/s).
    pub speed_horizontal: Option<f32>,
    /// Vertical speed, m/s, positive up, 0.5 m/s steps. `None` for the
    /// "unknown" code (63 m/s) or anything outside +/-62.
    pub speed_vertical: Option<f32>,
    /// `None` when unknown (0, 0) or out of range.
    pub position: Option<LatLon>,
    /// Pressure altitude, metres (ref 29.92 inHg). `None` when unknown.
    pub altitude_pressure: Option<f32>,
    /// Geodetic altitude, metres WGS-84 HAE. `None` when unknown.
    pub altitude_geodetic: Option<f32>,
    /// Height above takeoff or ground (see `height_type`), metres.
    pub height: Option<f32>,
    pub horizontal_accuracy: HorizontalAccuracy,
    pub vertical_accuracy: VerticalAccuracy,
    pub baro_accuracy: VerticalAccuracy,
    pub speed_accuracy: SpeedAccuracy,
    /// Seconds after the start of the current UTC hour, 0.1 s steps.
    /// `None` for the "unknown" code (0xFFFF) or anything past 3600 s.
    pub timestamp: Option<f32>,
    /// Timestamp accuracy, seconds (0.1 to 1.5). `None` when unknown.
    pub timestamp_accuracy: Option<f32>,
}

/// Authentication (type 0x2): one page of a multi-page signature.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Auth {
    pub auth_type: AuthType,
    /// Page index, 0-15.
    pub page: u8,
    /// Header fields present only on page 0.
    pub page_zero: Option<AuthPageZero>,
    /// 17 bytes on page 0, 23 on other pages.
    pub data: AuthData,
}

/// The page-0-only fields of an Authentication message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct AuthPageZero {
    /// Index of the last page in this set (0-15).
    pub last_page_index: u8,
    /// Total authentication data bytes across all pages (excluding any
    /// forward-error-correction bytes that may follow).
    pub length: u8,
    /// Seconds since 2019-01-01T00:00:00Z (add [`EPOCH_2019_UNIX`] for Unix time).
    pub timestamp: u32,
}

/// Self-ID (type 0x3): free text from the operator.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct SelfId {
    pub desc_type: DescType,
    pub description: FixedStr<23>,
}

/// System (type 0x4): operator position, group/area and classification.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct System {
    pub operator_location_type: OperatorLocationType,
    pub classification_type: ClassificationType,
    /// `None` when unknown (0, 0) or out of range.
    pub operator_position: Option<LatLon>,
    /// Number of aircraft in the area/group (1 for a single UA).
    pub area_count: u16,
    /// Area radius, metres (10 m steps, max 2550).
    pub area_radius: u16,
    /// Area ceiling, metres WGS-84 HAE. `None` when unknown.
    pub area_ceiling: Option<f32>,
    /// Area floor, metres WGS-84 HAE. `None` when unknown.
    pub area_floor: Option<f32>,
    pub category: Category,
    pub class: UaClass,
    /// Operator geodetic altitude, metres WGS-84 HAE. `None` when unknown.
    pub operator_altitude: Option<f32>,
    /// Seconds since 2019-01-01T00:00:00Z (add [`EPOCH_2019_UNIX`] for Unix
    /// time). `None` when zero, which pre-F3411-22a senders transmit.
    pub timestamp: Option<u32>,
}

/// Operator ID (type 0x5): the operator's registration, e.g. an EU operator number.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct OperatorId {
    pub operator_id_type: OperatorIdType,
    pub operator_id: FixedStr<20>,
}

// ---------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------

fn u16_at(raw: &RawMessage, at: usize) -> u16 {
    u16::from_le_bytes([raw[at], raw[at + 1]])
}

fn i32_at(raw: &RawMessage, at: usize) -> i32 {
    i32::from_le_bytes([raw[at], raw[at + 1], raw[at + 2], raw[at + 3]])
}

fn u32_at(raw: &RawMessage, at: usize) -> u32 {
    u32::from_le_bytes([raw[at], raw[at + 1], raw[at + 2], raw[at + 3]])
}

/// Up to `N` bytes from `at`, zero-filled past the end of the message.
fn bytes_at<const N: usize>(raw: &RawMessage, at: usize) -> [u8; N] {
    let mut out = [0u8; N];
    let src = raw.get(at..).unwrap_or(&[]);
    let n = src.len().min(N);
    out[..n].copy_from_slice(&src[..n]);
    out
}

/// Altitude code: 0.5 m steps offset by -1000 m; code 0 (-1000 m) is "unknown".
fn altitude(code: u16) -> Option<f32> {
    (code != 0).then(|| f32::from(code) * 0.5 - 1000.0)
}

/// Decode one 25-byte message.
pub fn decode(raw: &RawMessage) -> Result<Message, Error> {
    match raw[0] >> 4 {
        kind::BASIC_ID => Ok(Message::BasicId(BasicId {
            id_type: IdType::from_code(raw[1] >> 4),
            ua_type: UaType::from_code(raw[1]),
            uas_id: FixedStr(bytes_at(raw, 2)),
        })),
        kind::LOCATION => Ok(Message::Location(decode_location(raw))),
        kind::AUTH => decode_auth(raw).map(Message::Auth),
        kind::SELF_ID => Ok(Message::SelfId(SelfId {
            desc_type: DescType::from_code(raw[1]),
            description: FixedStr(bytes_at(raw, 2)),
        })),
        kind::SYSTEM => Ok(Message::System(decode_system(raw))),
        kind::OPERATOR_ID => Ok(Message::OperatorId(OperatorId {
            operator_id_type: OperatorIdType::from_code(raw[1]),
            operator_id: FixedStr(bytes_at(raw, 2)),
        })),
        other => Err(Error::UnknownType(other)),
    }
}

fn decode_location(raw: &RawMessage) -> Location {
    let flags = raw[1];
    let east_west = flags & 0x02 != 0;
    let speed_mult = flags & 0x01 != 0;

    let direction = f32::from(raw[2]) + if east_west { 180.0 } else { 0.0 };
    let speed_h = if speed_mult {
        f32::from(raw[3]) * 0.75 + 63.75
    } else {
        f32::from(raw[3]) * 0.25
    };
    let speed_v = f32::from(raw[4] as i8) * 0.5;
    let timestamp = u16_at(raw, 21);
    let ts_accuracy = raw[23] & 0x0F;

    Location {
        status: Status::from_code(flags >> 4),
        height_type: if flags & 0x04 != 0 {
            HeightType::AboveGround
        } else {
            HeightType::AboveTakeoff
        },
        direction: (direction < 360.0).then_some(direction),
        speed_horizontal: (speed_h != 255.0).then_some(speed_h),
        speed_vertical: (-62.0..=62.0).contains(&speed_v).then_some(speed_v),
        position: LatLon::from_raw(i32_at(raw, 5), i32_at(raw, 9)),
        altitude_pressure: altitude(u16_at(raw, 13)),
        altitude_geodetic: altitude(u16_at(raw, 15)),
        height: altitude(u16_at(raw, 17)),
        horizontal_accuracy: HorizontalAccuracy::from_code(raw[19] & 0x0F),
        vertical_accuracy: VerticalAccuracy::from_code(raw[19] >> 4),
        baro_accuracy: VerticalAccuracy::from_code(raw[20] >> 4),
        speed_accuracy: SpeedAccuracy::from_code(raw[20] & 0x0F),
        timestamp: (timestamp <= 36_000).then(|| f32::from(timestamp) / 10.0),
        timestamp_accuracy: (ts_accuracy != 0).then(|| f32::from(ts_accuracy) / 10.0),
    }
}

fn decode_auth(raw: &RawMessage) -> Result<Auth, Error> {
    let auth_type = AuthType::from_code(raw[1] >> 4);
    let page = raw[1] & 0x0F;
    if page == 0 {
        let last_page_index = raw[2];
        let length = raw[3];
        // Same consistency rule as core-c: the pages announced must be able to
        // hold the announced length.
        let capacity = AUTH_PAGE_ZERO_DATA + usize::from(last_page_index) * AUTH_PAGE_DATA;
        if last_page_index >= AUTH_MAX_PAGES || usize::from(length) > capacity {
            return Err(Error::Invalid);
        }
        let data = AuthData {
            bytes: bytes_at(raw, 8),
            len: AUTH_PAGE_ZERO_DATA as u8,
        };
        Ok(Auth {
            auth_type,
            page,
            page_zero: Some(AuthPageZero {
                last_page_index,
                length,
                timestamp: u32_at(raw, 4),
            }),
            data,
        })
    } else {
        Ok(Auth {
            auth_type,
            page,
            page_zero: None,
            data: AuthData {
                bytes: bytes_at(raw, 2),
                len: AUTH_PAGE_DATA as u8,
            },
        })
    }
}

fn decode_system(raw: &RawMessage) -> System {
    let flags = raw[1];
    let timestamp = u32_at(raw, 20);
    System {
        operator_location_type: OperatorLocationType::from_code(flags & 0x03),
        classification_type: ClassificationType::from_code((flags >> 2) & 0x07),
        operator_position: LatLon::from_raw(i32_at(raw, 2), i32_at(raw, 6)),
        area_count: u16_at(raw, 10),
        area_radius: u16::from(raw[12]) * 10,
        area_ceiling: altitude(u16_at(raw, 13)),
        area_floor: altitude(u16_at(raw, 15)),
        category: Category::from_code(raw[17] >> 4),
        class: UaClass::from_code(raw[17] & 0x0F),
        operator_altitude: altitude(u16_at(raw, 18)),
        timestamp: (timestamp != 0).then_some(timestamp),
    }
}

/// Split a payload into its raw messages. A single message yields itself; a
/// Message Pack (type 0xF) yields each message it contains, in order.
///
/// A pack must declare a message size of 25 and a count of 1-9, and must not
/// contain another pack ([`Error::BadPack`]); it must be at least as long as
/// it declares ([`Error::Truncated`]). Bytes after the declared end are
/// ignored. Unlike core-c, a pack is not rejected for holding an unknown
/// message type or more than one Location: those are passed through, so
/// [`decode`] can reject just the bad message and keep the rest.
pub fn raw_messages(payload: &[u8]) -> Result<impl Iterator<Item = &RawMessage>, Error> {
    let first = *payload.first().ok_or(Error::Truncated)?;
    let body = if first >> 4 == kind::MESSAGE_PACK {
        let size = *payload.get(1).ok_or(Error::Truncated)?;
        let count = usize::from(*payload.get(2).ok_or(Error::Truncated)?);
        if usize::from(size) != MESSAGE_SIZE || count == 0 || count > MAX_PACK {
            return Err(Error::BadPack);
        }
        payload
            .get(3..3 + count * MESSAGE_SIZE)
            .ok_or(Error::Truncated)?
    } else {
        payload.get(..MESSAGE_SIZE).ok_or(Error::Truncated)?
    };
    let (messages, _) = body.as_chunks::<MESSAGE_SIZE>();
    if messages.iter().any(|m| m[0] >> 4 == kind::MESSAGE_PACK) {
        return Err(Error::BadPack);
    }
    Ok(messages.iter())
}
