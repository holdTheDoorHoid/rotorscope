//! Record layout, byte for byte. `docs/EVIDENCE-FORMAT.md` is the
//! human-readable version of this file; keep the two in step.
//!
//! All integers are little-endian. Every record is:
//!
//! ```text
//! offset size  field
//!      0    4  magic "RSEV"
//!      4    1  format version (1)
//!      5    1  kind: 1 event, 2 detection, 3 checkpoint
//!      6    2  total record length in bytes, trailer included (u16)
//!      8    8  boot id (random per boot: names one chain)
//!     16    8  sequence number (u64; 0 = the boot event that starts the chain)
//!     24   32  SHA-256 of the previous record (all zero for sequence 0)
//!     56    8  hub uptime, ms (u64)
//!     64    8  UTC, Unix ms (u64; 0 = unknown)
//!     72    1  time source: 0 none, 1 free-running, 2 gps, 3 gps_pps
//!     73    1  GPS receiver: 0 absent, 1 searching, 2 2D fix, 3 3D fix
//!     74    1  position source: 0 none, 1 gps, 2 manual (typed in)
//!     75    1  satellites used (255 unknown)
//!     76    4  time age: ms since the UTC was last set from GPS (u32; max = n/a)
//!     80    4  hub latitude, degrees x 1e7 (i32; 0 when no position)
//!     84    4  hub longitude, degrees x 1e7 (i32)
//!     88    4  hub height above WGS-84 ellipsoid, mm (i32; i32::MIN unknown)
//!     92    2  horizontal accuracy, dm (u16; max unknown)
//!     94    2  HDOP x 100 (u16; max unknown)
//!     96    4  position age, ms (u32; max = n/a)
//!    100   32  firmware hash (SHA-256 of the firmware ELF file)
//!    132    n  body (by kind, below)
//!  len-4    4  total record length again (u32), for reading backwards
//! ```
//!
//! The record's hash is SHA-256 over all `len` bytes. The next record
//! carries it at offset 24.

/// "RSEV".
pub const MAGIC: [u8; 4] = *b"RSEV";
pub const VERSION: u8 = 1;
pub const HEADER: usize = 132;
pub const TRAILER: usize = 4;
/// No record is longer (a link frame is at most `link::MAX_FRAME`).
pub const MAX_RECORD: usize = 1024;
/// ECDSA P-256 signature, r || s, 32 bytes each, big-endian.
pub const SIG_LEN: usize = 64;
/// SEC1 uncompressed public key (0x04 || x || y).
pub const PUBKEY_LEN: usize = 65;
/// Longest decode summary the hub writes.
pub const MAX_SUMMARY: usize = 200;

pub mod off {
    pub const MAGIC: usize = 0;
    pub const VERSION: usize = 4;
    pub const KIND: usize = 5;
    pub const LEN: usize = 6;
    pub const BOOT_ID: usize = 8;
    pub const SEQ: usize = 16;
    pub const PREV: usize = 24;
    pub const UPTIME: usize = 56;
    pub const UTC: usize = 64;
    pub const TIME_SRC: usize = 72;
    pub const GPS_STATE: usize = 73;
    pub const POS_SRC: usize = 74;
    pub const SATS: usize = 75;
    pub const TIME_AGE: usize = 76;
    pub const LAT: usize = 80;
    pub const LON: usize = 84;
    pub const ALT: usize = 88;
    pub const HACC: usize = 92;
    pub const HDOP: usize = 94;
    pub const FIX_AGE: usize = 96;
    pub const FW: usize = 100;
    pub const BODY: usize = 132;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Event = 1,
    Detection = 2,
    Checkpoint = 3,
}

impl Kind {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Kind::Event),
            2 => Some(Kind::Detection),
            3 => Some(Kind::Checkpoint),
            _ => None,
        }
    }
}

/// Where the record's UTC came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimeSource {
    /// No UTC at all: only the uptime counts. UTC field is 0.
    #[default]
    None = 0,
    /// UTC carried forward on the hub's own crystal since the GPS last
    /// supplied it (`time_age_ms` says how long ago).
    FreeRunning = 1,
    /// GPS time from the receiver's messages, not locked to the PPS pulse
    /// (tens of ms uncertain).
    Gps = 2,
    /// GPS time anchored to the PPS edge (sub-millisecond at the edge).
    GpsPps = 3,
    /// No UTC: the receiver gives a time but the hub does not trust it (GPS
    /// spoofing suspected with no earlier trusted time to hold, or a time
    /// before the build date or the remembered time, WP-12). UTC field is 0.
    Untrusted = 4,
}

impl TimeSource {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::None,
            1 => Self::FreeRunning,
            2 => Self::Gps,
            3 => Self::GpsPps,
            4 => Self::Untrusted,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::FreeRunning => "free_running",
            Self::Gps => "gps",
            Self::GpsPps => "gps_pps",
            Self::Untrusted => "untrusted",
        }
    }
}

/// Where the hub position in the record came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PosSource {
    #[default]
    None = 0,
    Gps = 1,
    /// Typed in by the operator: not a measurement.
    Manual = 2,
}

impl PosSource {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::None,
            1 => Self::Gps,
            2 => Self::Manual,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Gps => "gps",
            Self::Manual => "manual",
        }
    }
}

/// GPS receiver state (header offset 73).
pub fn gps_state_name(v: u8) -> &'static str {
    match v {
        0 => "absent",
        1 => "searching",
        2 => "fix_2d",
        3 => "fix_3d",
        _ => "?",
    }
}

/// Event codes (event body offset 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventCode {
    /// Sequence 0 of every chain. Data: key class (1), public key (65),
    /// chip revision major (1), minor (1). Text: firmware name and version.
    Boot = 1,
    /// The SD card was mounted and a new evidence file opened. Data: this
    /// file's number (u32), previous file's number (u32, 0 = none),
    /// previous file's length (u32), SHA-256 of the previous file's last
    /// complete record (32), previous file status (1: 0 none, 1 complete,
    /// 2 ends in an incomplete record, 3 unreadable).
    CardMounted = 2,
    /// The card stopped answering. Data: bytes of records not yet written (u32).
    /// Those records go to the next file, unchanged.
    CardLost = 3,
    /// A setting changed. Text says which, from what, to what.
    Setting = 4,
    /// UTC set or corrected. Data: old UTC (u64, 0 none), new UTC (u64),
    /// step ms (i64), PPS-locked (1).
    Clock = 5,
    GpsFix = 6,
    GpsLost = 7,
    /// Position jumped further than a vehicle could move: possible spoofing
    /// or a bad fix. Data: distance m (u32), seconds since last fix (u32).
    GpsJump = 8,
    /// A node was heard for the first time. Data: station address (6).
    NodeJoin = 9,
    /// A node fell silent. Data: station address (6).
    NodeLeave = 10,
    /// Airspace pack loaded. Data: build date Unix s (u32), length (u32),
    /// pack format version (u16).
    Pack = 11,
    /// Free text from the operator.
    Note = 12,
    /// Frames received but not logged (queue full or no card yet).
    /// Data: count (u32).
    NotLogged = 13,
    /// The operator asked to stop logging (card eject). A shutdown
    /// checkpoint follows.
    Shutdown = 14,
    GpsReceiver = 15,
    /// An O4 DroneID record was decrypted with the owner's key (derived
    /// from the raw frame record just before it, which keeps the
    /// ciphertext). Data: session key ID (4), record selector (1). Text:
    /// what it decrypted to (serial, positions). Never the key.
    O4Decrypted = 16,
    /// An attack or jamming reported by a node (link protocol 9
    /// `Frame::Threat`, kept raw in the frame record just before it). Data:
    /// kind (1, `link::ThreatKind` index), node station address (6). Text:
    /// what it was, where, how strong.
    Threat = 17,
    /// The hub's own GPS shows signs of jamming. Data: kind (1, receiver
    /// specific indicator), level (i16). Text: the evidence.
    GnssJamming = 18,
    /// The hub's own GPS shows signs of spoofing. Data: kind (1), level
    /// (i32). Text: the evidence.
    GnssSpoofing = 19,
    /// A drone track failed a consistency check that suggests its
    /// broadcast is forged or replayed. Data: check (1), confidence 0-100
    /// (1). Text: the track and the evidence.
    SpoofSuspect = 20,
    /// Frames that claimed to come from one of our own nodes failed
    /// authentication on the ESP-NOW link. Data: claimed station address
    /// (6), count (u32).
    LinkForgery = 21,
    /// A drone close to a crewed aircraft (tracker `conflict`), once per
    /// pair and level. Data: level (1: 1 advisory, 2 caution, 3 warning),
    /// drone track id (u32), aircraft address (u32), aircraft source (1,
    /// `adsb::Source` index). Text: the pair, distance, height difference
    /// and predicted closest approach.
    AircraftConflict = 22,
    /// Two transmitters claim one drone ID (tracker `claims`): a rival
    /// claim formed its own path while the original continued, or a text
    /// field alternated. Not an accusation of either. Data: drone track id
    /// (u32 LE), what is disputed (1: bit 0 position, 1 Self-ID, 2 operator
    /// ID, 3 a second ID), the other claim's source (1: 0 Remote ID, 1
    /// DroneID, 2 DJI Wi-Fi, 3 control link, 4 decrypted O4, 255 none).
    /// Text: the ID, how far apart, and that neither is verified.
    /// (Codes 23 and 24 are taken by the acoustic work: `sound`, `sound_clip`.)
    IdDisputed = 25,
    /// An announced threat episode ended (WP-14a): quiet long enough, or
    /// its alarm entry removed while it may still be going on. Closes the
    /// `threat` record that raised it. Data: [`threat_ended`] layout. Text:
    /// what it totalled.
    ThreatEnded = 26,
}

/// Data of a `threat` event (17). The first 7 bytes are the original
/// layout; firmware from WP-14a on appends the episode fields.
pub mod threat {
    /// kind (1), node station address (6)
    pub const BASE: usize = 7;
    /// + episode number for this alarm entry (u32, 1 = first), how it was
    /// raised (1: [`RAISE_FIRST`], [`RAISE_AGAIN`], [`RAISE_AFTER_SILENCE`]),
    /// raises within the last minute (1).
    pub const WITH_EPISODE: usize = BASE + 4 + 1 + 1;
    pub const RAISE_FIRST: u8 = 0;
    pub const RAISE_AGAIN: u8 = 1;
    pub const RAISE_AFTER_SILENCE: u8 = 2;
}

/// Data of a `threat_ended` event (26).
pub mod threat_ended {
    /// kind (1), node station address (6), episode (u32), active ms (u64),
    /// frames (u64), reports (u32), peak level (i8), raises in the episode
    /// (u32), flags (1: [`HELD`], [`REMOVED`]), end records lost before
    /// this one (u32).
    pub const LEN: usize = 1 + 6 + 4 + 8 + 8 + 4 + 1 + 4 + 1 + 4;
    /// Held as flapping: several raises reported as this one episode.
    pub const HELD: u8 = 1;
    /// Its alarm entry was removed before it went quiet.
    pub const REMOVED: u8 = 2;
}

impl EventCode {
    pub fn from_u16(v: u16) -> Option<Self> {
        use EventCode::*;
        Some(match v {
            1 => Boot,
            2 => CardMounted,
            3 => CardLost,
            4 => Setting,
            5 => Clock,
            6 => GpsFix,
            7 => GpsLost,
            8 => GpsJump,
            9 => NodeJoin,
            10 => NodeLeave,
            11 => Pack,
            12 => Note,
            13 => NotLogged,
            14 => Shutdown,
            15 => GpsReceiver,
            16 => O4Decrypted,
            17 => Threat,
            18 => GnssJamming,
            19 => GnssSpoofing,
            20 => SpoofSuspect,
            21 => LinkForgery,
            22 => AircraftConflict,
            25 => IdDisputed,
            26 => ThreatEnded,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        use EventCode::*;
        match self {
            Boot => "boot",
            CardMounted => "card_mounted",
            CardLost => "card_lost",
            Setting => "setting",
            Clock => "clock",
            GpsFix => "gps_fix",
            GpsLost => "gps_lost",
            GpsJump => "gps_jump",
            NodeJoin => "node_join",
            NodeLeave => "node_leave",
            Pack => "pack",
            Note => "note",
            NotLogged => "not_logged",
            Shutdown => "shutdown",
            GpsReceiver => "gps_receiver",
            O4Decrypted => "o4_decrypted",
            Threat => "threat",
            GnssJamming => "gnss_jamming",
            GnssSpoofing => "gnss_spoofing",
            SpoofSuspect => "spoof_suspect",
            LinkForgery => "link_forgery",
            AircraftConflict => "aircraft_conflict",
            IdDisputed => "id_disputed",
            ThreatEnded => "threat_ended",
        }
    }
}

/// Checkpoint reasons (checkpoint body offset 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointReason {
    Periodic = 0,
    Shutdown = 1,
    /// Asked for (e.g. before an upload).
    Requested = 2,
}

impl CheckpointReason {
    pub fn name(v: u8) -> &'static str {
        match v {
            0 => "periodic",
            1 => "shutdown",
            2 => "requested",
            _ => "?",
        }
    }
}

/// Detection body: what kind of frame the raw bytes are.
pub mod frame_kind {
    pub const UNKNOWN: u8 = 0;
    pub const REMOTE_ID: u8 = 1;
    pub const OPEN_LINK: u8 = 2;
    pub const DJI_DRONEID: u8 = 3;
    pub const RF: u8 = 4;
    pub const DJI_WIFI: u8 = 5;
    pub const WIFI_DRONE: u8 = 6;
    /// Link protocol 11: one raw aircraft frame from an aero node
    /// (`link::Frame::Air`: 1090 ES, UAT, FLARM or OGN), band `RF`.
    pub const AIRCRAFT: u8 = 7;
    /// Link protocol 12: a drone control link heard by an LR2021 node
    /// (`link::Frame::Control`: system, signal, confidence; decoded
    /// content and the raw packet only with the owner's opt-in), band `RF`.
    pub const CONTROL: u8 = 8;
    /// Link protocol 12: one energy window of a control band
    /// (`link::Frame::ControlEnergy`: hoppers, steady carriers), band `RF`.
    pub const CONTROL_ENERGY: u8 = 9;
    /// Link protocol 13: the MAVLink frame behind a hostile-telemetry
    /// threat (`link::Frame::MavlinkEvidence`, `crates/mavwatch`), band
    /// `RF` (it came over a drone telemetry radio). Logged only with control
    /// content decoding on.
    pub const MAVLINK: u8 = 10;
}

/// Detection body: band.
pub mod band {
    pub const UNKNOWN: u8 = 0;
    pub const WIFI_24: u8 = 1;
    pub const WIFI_5: u8 = 2;
    pub const BLE: u8 = 3;
    /// Raw-capture receiver: see the frequency field.
    pub const RF: u8 = 4;

    pub fn name(v: u8) -> &'static str {
        match v {
            WIFI_24 => "wifi_2.4",
            WIFI_5 => "wifi_5",
            BLE => "ble",
            RF => "rf",
            _ => "unknown",
        }
    }
}

/// Detection body: raw byte format. 1 = one `link::Frame` serialized with
/// postcard (no COBS, no delimiter), link protocol 6 (`crates/link`).
pub const RAW_LINK_FRAME_V6: u8 = 1;

/// "Not available" markers.
pub const RSSI_NONE: i8 = 127;
pub const ALT_NONE: i32 = i32::MIN;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// Fewer bytes than a header, or than the record's own length field.
    Truncated,
    BadMagic,
    BadVersion(u8),
    BadKind(u8),
    /// Length field shorter than a header + trailer, or over `MAX_RECORD`.
    BadLength(u16),
    /// The trailer does not repeat the length.
    BadTrailer,
    /// A body field runs past the end of the record, or holds a value the
    /// format does not allow. The text names the field.
    BadBody(&'static str),
}

impl core::fmt::Display for ParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Truncated => write!(f, "record is cut short"),
            Self::BadMagic => write!(f, "no \"RSEV\" record marker"),
            Self::BadVersion(v) => write!(f, "unknown format version {v}"),
            Self::BadKind(k) => write!(f, "unknown record kind {k}"),
            Self::BadLength(l) => write!(f, "impossible record length {l}"),
            Self::BadTrailer => write!(f, "trailer does not repeat the record length"),
            Self::BadBody(w) => write!(f, "bad body field: {w}"),
        }
    }
}

/// The common header, decoded.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Header {
    pub kind: Kind,
    pub len: usize,
    pub boot_id: [u8; 8],
    pub seq: u64,
    pub prev: [u8; 32],
    pub uptime_ms: u64,
    pub utc_ms: Option<u64>,
    pub time_source: u8,
    pub gps_state: u8,
    pub pos_source: u8,
    pub sats: Option<u8>,
    pub time_age_ms: Option<u32>,
    pub lat_e7: i32,
    pub lon_e7: i32,
    pub alt_hae_mm: Option<i32>,
    pub hacc_dm: Option<u16>,
    pub hdop_x100: Option<u16>,
    pub fix_age_ms: Option<u32>,
    pub fw_hash: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DetectionBody<'a> {
    pub node: [u8; 6],
    pub link_rssi: Option<i8>,
    pub node_index: u8,
    pub band: u8,
    pub frame_kind: u8,
    pub channel: u16,
    pub freq_100khz: u32,
    pub rssi: Option<i8>,
    pub raw_format: u8,
    pub raw: &'a [u8],
    pub summary: &'a [u8],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EventBody<'a> {
    pub code: u16,
    pub text: &'a [u8],
    pub data: &'a [u8],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CheckpointBody<'a> {
    pub reason: u8,
    pub key_class: u8,
    pub pubkey: [u8; PUBKEY_LEN],
    pub count: u64,
    pub note: &'a [u8],
    pub signature: [u8; SIG_LEN],
    /// The bytes the signature covers: the record from offset 0 up to the
    /// signature. The signer signs SHA-256 of these.
    pub signed: &'a [u8],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Body<'a> {
    Event(EventBody<'a>),
    Detection(DetectionBody<'a>),
    Checkpoint(CheckpointBody<'a>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Record<'a> {
    pub header: Header,
    pub body: Body<'a>,
    /// All `header.len` bytes.
    pub bytes: &'a [u8],
}

impl Record<'_> {
    /// SHA-256 of the record: what the next record's `prev` must be.
    pub fn hash(&self) -> [u8; 32] {
        crate::sha256(self.bytes)
    }
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}
fn arr<const N: usize>(b: &[u8], at: usize) -> [u8; N] {
    b[at..at + N].try_into().unwrap()
}

/// The length a record starting at `buf[0]` claims, after checking its
/// marker, version and length bounds. Needs at least 8 bytes.
pub fn peek_len(buf: &[u8]) -> Result<usize, ParseError> {
    if buf.len() < 8 {
        return Err(ParseError::Truncated);
    }
    if buf[..4] != MAGIC {
        return Err(ParseError::BadMagic);
    }
    if buf[off::VERSION] != VERSION {
        return Err(ParseError::BadVersion(buf[off::VERSION]));
    }
    let len = u16_at(buf, off::LEN);
    if (len as usize) < HEADER + TRAILER || len as usize > MAX_RECORD {
        return Err(ParseError::BadLength(len));
    }
    Ok(len as usize)
}

/// A cursor over a body, failing with the field's name if it runs out.
struct Cur<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize, what: &'static str) -> Result<&'a [u8], ParseError> {
        let s = self.b.get(self.at..self.at + n).ok_or(ParseError::BadBody(what))?;
        self.at += n;
        Ok(s)
    }
    fn u8(&mut self, what: &'static str) -> Result<u8, ParseError> {
        Ok(self.take(1, what)?[0])
    }
    fn u16(&mut self, what: &'static str) -> Result<u16, ParseError> {
        let s = self.take(2, what)?;
        Ok(u16::from_le_bytes([s[0], s[1]]))
    }
    fn u32(&mut self, what: &'static str) -> Result<u32, ParseError> {
        Ok(u32::from_le_bytes(self.take(4, what)?.try_into().unwrap()))
    }
    fn u64(&mut self, what: &'static str) -> Result<u64, ParseError> {
        Ok(u64::from_le_bytes(self.take(8, what)?.try_into().unwrap()))
    }
    /// A u16 length, then that many bytes.
    fn lv(&mut self, what: &'static str) -> Result<&'a [u8], ParseError> {
        let n = self.u16(what)? as usize;
        self.take(n, what)
    }
}

fn opt_i8(v: u8) -> Option<i8> {
    (v as i8 != RSSI_NONE).then_some(v as i8)
}

/// Parse one record at the start of `buf` (which may hold more after it).
pub fn parse(buf: &[u8]) -> Result<Record<'_>, ParseError> {
    let len = peek_len(buf)?;
    if buf.len() < len {
        return Err(ParseError::Truncated);
    }
    let b = &buf[..len];
    if u32_at(b, len - TRAILER) != len as u32 {
        return Err(ParseError::BadTrailer);
    }
    let kind = Kind::from_u8(b[off::KIND]).ok_or(ParseError::BadKind(b[off::KIND]))?;
    let utc = u64_at(b, off::UTC);
    let alt = u32_at(b, off::ALT) as i32;
    let header = Header {
        kind,
        len,
        boot_id: arr(b, off::BOOT_ID),
        seq: u64_at(b, off::SEQ),
        prev: arr(b, off::PREV),
        uptime_ms: u64_at(b, off::UPTIME),
        utc_ms: (utc != 0).then_some(utc),
        time_source: b[off::TIME_SRC],
        gps_state: b[off::GPS_STATE],
        pos_source: b[off::POS_SRC],
        sats: (b[off::SATS] != u8::MAX).then_some(b[off::SATS]),
        time_age_ms: Some(u32_at(b, off::TIME_AGE)).filter(|&v| v != u32::MAX),
        lat_e7: u32_at(b, off::LAT) as i32,
        lon_e7: u32_at(b, off::LON) as i32,
        alt_hae_mm: (alt != ALT_NONE).then_some(alt),
        hacc_dm: Some(u16_at(b, off::HACC)).filter(|&v| v != u16::MAX),
        hdop_x100: Some(u16_at(b, off::HDOP)).filter(|&v| v != u16::MAX),
        fix_age_ms: Some(u32_at(b, off::FIX_AGE)).filter(|&v| v != u32::MAX),
        fw_hash: arr(b, off::FW),
    };
    if TimeSource::from_u8(header.time_source).is_none() {
        return Err(ParseError::BadBody("time source"));
    }
    if PosSource::from_u8(header.pos_source).is_none() {
        return Err(ParseError::BadBody("position source"));
    }
    let mut c = Cur { b: &b[..len - TRAILER], at: off::BODY };
    let body = match kind {
        Kind::Event => {
            let code = c.u16("event code")?;
            let text = c.lv("event text")?;
            let data = c.lv("event data")?;
            Body::Event(EventBody { code, text, data })
        }
        Kind::Detection => {
            let node = arr(c.take(6, "node address")?, 0);
            let link_rssi = opt_i8(c.u8("link rssi")?);
            let node_index = c.u8("node index")?;
            let band = c.u8("band")?;
            let frame_kind = c.u8("frame kind")?;
            let channel = c.u16("channel")?;
            let freq_100khz = c.u32("frequency")?;
            let rssi = opt_i8(c.u8("rssi")?);
            let raw_format = c.u8("raw format")?;
            let raw = c.lv("raw bytes")?;
            let summary = c.lv("summary")?;
            Body::Detection(DetectionBody {
                node,
                link_rssi,
                node_index,
                band,
                frame_kind,
                channel,
                freq_100khz,
                rssi,
                raw_format,
                raw,
                summary,
            })
        }
        Kind::Checkpoint => {
            let reason = c.u8("checkpoint reason")?;
            let key_class = c.u8("key class")?;
            let pubkey = arr(c.take(PUBKEY_LEN, "public key")?, 0);
            let count = c.u64("record count")?;
            let note = c.lv("note")?;
            let signed_end = c.at;
            let signature = arr(c.take(SIG_LEN, "signature")?, 0);
            Body::Checkpoint(CheckpointBody { reason, key_class, pubkey, count, note, signature, signed: &b[..signed_end] })
        }
    };
    if c.at != len - TRAILER {
        return Err(ParseError::BadBody("bytes left over after the body"));
    }
    Ok(Record { header, body, bytes: b })
}

/// Iterate the records in a byte stream (a log file, a batch). Stops at
/// the first error, which it yields with the byte offset.
pub fn records(buf: &[u8]) -> Records<'_> {
    Records { buf, at: 0, done: false }
}

pub struct Records<'a> {
    buf: &'a [u8],
    at: usize,
    done: bool,
}

impl<'a> Iterator for Records<'a> {
    /// (byte offset, record or error)
    type Item = (usize, Result<Record<'a>, ParseError>);
    fn next(&mut self) -> Option<Self::Item> {
        if self.done || self.at >= self.buf.len() {
            return None;
        }
        let at = self.at;
        match parse(&self.buf[at..]) {
            Ok(r) => {
                self.at += r.header.len;
                Some((at, Ok(r)))
            }
            Err(e) => {
                self.done = true;
                Some((at, Err(e)))
            }
        }
    }
}

/// The last complete record in `tail`, which is the end of a file whose
/// last bytes may be an incomplete record (power cut). Returns the record's
/// start offset in `tail` and whether bytes follow it (an incomplete one).
pub fn last_complete(tail: &[u8]) -> Option<(usize, bool)> {
    // Fast path: the file ends exactly on a record.
    if tail.len() >= HEADER + TRAILER {
        let n = u32_at(tail, tail.len() - TRAILER) as usize;
        if n <= tail.len() && n >= HEADER + TRAILER && parse(&tail[tail.len() - n..]).is_ok() {
            return Some((tail.len() - n, false));
        }
    }
    // Otherwise the last marker that starts a whole record.
    let mut best = None;
    let mut i = 0;
    while i + 4 <= tail.len() {
        if tail[i..i + 4] == MAGIC
            && let Ok(r) = parse(&tail[i..])
        {
            best = Some((i, i + r.header.len < tail.len()));
        }
        i += 1;
    }
    best
}
