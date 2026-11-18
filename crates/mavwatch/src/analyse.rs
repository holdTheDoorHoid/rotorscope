//! The per-link analyser: CRC-valid frames in, [`Finding`]s out.
//!
//! See the crate documentation for the kind table, and each constant below
//! for its threshold and why.

use heapless::{Deque, Vec};

use crate::frame::Frame;
use crate::msg::{self, ftp};
use crate::text::{self, Problem};

/// What was found. The number is the threat's `detail` on the link
/// (`link::ThreatKind::MavlinkAttack`); never renumber, only append.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Kind {
    /// A URI the ground station will fetch (camera definition, component
    /// metadata, video stream) with path traversal: `../`, `..\`, or
    /// percent-encoded forms. QGC-01/02, MAVSDK-02.
    UriTraversal = 1,
    /// Such a URI with a scheme the ground station must not follow
    /// (`file://`, `qrc:`, anything off the field's list), a bare absolute
    /// or UNC path, or control characters. QGC-02, MAVSDK-01.
    UriScheme = 2,
    /// CAMERA_INFORMATION vendor or model name with a path separator, `..`
    /// or control characters (QGC builds a file path from them). QGC-01.
    CameraName = 3,
    /// VIDEO_STREAM_INFORMATION URI carrying a GStreamer pipeline (`gst://`
    /// or ` ! `). MP-02.
    VideoPipeline = 4,
    /// STATUSTEXT carrying Mission Planner's `[link;target;label]` markup,
    /// which it turns into a clickable `Process.Start`. MP-03.
    StatusLink = 5,
    /// STATUSTEXT with control characters (terminal escapes and the like),
    /// in at least [`STATUS_CONTROL_HITS`] messages within
    /// [`STATUS_CONTROL_WINDOW_MS`].
    StatusControl = 6,
    /// A MAVLink FTP directory listing (sent to the ground station) with a
    /// name containing a path separator or `..`. QGC-03, MP-01.
    FtpTraversal = 7,
    /// A MAVLink FTP read acknowledgement at an offset of
    /// [`FTP_ABSURD_OFFSET`] or more (QGC seeks there and fills the disk).
    /// QGC-03.
    FtpOffset = 8,
    /// A malformed MAVLink FTP message: size field over 239, an open
    /// acknowledgement longer than 4 bytes or a write acknowledgement
    /// claiming more than 239 bytes written (MAVROS-02), or a request path
    /// of [`FTP_LONG_PATH`] bytes or more (OWASP: bound FTP path lengths).
    FtpMalformed = 9,
    /// PARAM_VALUE `RALLY_TOTAL` / `FENCE_TOTAL` that is not a whole number
    /// from 0 to [`TOTAL_MAX`] (Mission Planner's `int.Parse` on connect).
    /// MP-04.
    ParamTotal = 10,
    /// PARAM_VALUE from one system with far more distinct parameter names
    /// than any autopilot has ([`PARAM_FLOOD_BITS`]). MAVROS-01.
    ParamFlood = 11,
    /// PARAM_SET of a floating-point parameter to NaN or infinity (OWASP:
    /// reject non-finite parameter values).
    ParamSetNonFinite = 12,
    /// SERIAL_CONTROL on the air: remote shell / serial-port access (PX4
    /// NSH, CVE-2026-1579). Also what the owner's own MAVLink console
    /// sends: mute it if that is you.
    SerialControl = 13,
    /// SERIAL_CONTROL whose `count` exceeds its 70-byte data field
    /// (ArduPilot's out-of-bounds read, CVE-2026-38971).
    SerialControlMalformed = 14,
    /// SETUP_SIGNING on the air: the link's 32-byte signing key travelling
    /// in the clear (it is meant for a USB cable), or someone re-keying the
    /// vehicle.
    SetupSigning = 15,
    /// Unsigned commands from a system that is sending signed frames on the
    /// same link at the same time (CVE-2020-10283). Needs link IDs.
    SigningDowngrade = 16,
    /// Two interleaved sequence-number streams claiming one system and
    /// component: a second transmitter injecting frames. Needs link IDs.
    SeqInterleave = 17,
    /// A signed frame whose timestamp is not newer than one already seen
    /// on the same signing stream (a replay; receivers drop these, but
    /// someone is trying). Needs link IDs.
    SignedReplay = 18,
    /// A checksum-valid frame of a known message with a payload length no
    /// conforming sender uses (longer than the message, or a MAVLink 1
    /// frame not exactly its length). Lenient parsers still decode it, cut
    /// or zero-filled ([`crate::frame`]), and the frame is also analysed
    /// that way; the shape itself is the finding.
    OddLength = 19,
}

impl Kind {
    pub const ALL: [Kind; 19] = [
        Kind::UriTraversal,
        Kind::UriScheme,
        Kind::CameraName,
        Kind::VideoPipeline,
        Kind::StatusLink,
        Kind::StatusControl,
        Kind::FtpTraversal,
        Kind::FtpOffset,
        Kind::FtpMalformed,
        Kind::ParamTotal,
        Kind::ParamFlood,
        Kind::ParamSetNonFinite,
        Kind::SerialControl,
        Kind::SerialControlMalformed,
        Kind::SetupSigning,
        Kind::SigningDowngrade,
        Kind::SeqInterleave,
        Kind::SignedReplay,
        Kind::OddLength,
    ];

    const LABELS: [&'static str; 19] = [
        "URI path traversal",
        "URI with a forbidden scheme or path",
        "Camera name with a file path",
        "Video URI with a GStreamer pipeline",
        "Status text with a clickable link",
        "Status text with control characters",
        "File listing with path traversal",
        "File transfer at an absurd offset",
        "Malformed file transfer",
        "Rally/fence count out of range",
        "Parameter name flood",
        "Parameter set to NaN/infinity",
        "Remote shell / serial access",
        "Malformed serial access (overflow)",
        "Signing key sent over the air",
        "Unsigned commands on a signed link",
        "Injected frames (two sequence streams)",
        "Replayed signed frames",
        "Frame of a length no sender uses",
    ];

    /// The `detail` number.
    pub const fn code(self) -> u32 {
        self as u32
    }

    pub fn from_code(code: u32) -> Option<Kind> {
        Kind::ALL.iter().copied().find(|k| k.code() == code)
    }

    /// Short plain-English description.
    pub const fn label(self) -> &'static str {
        Self::LABELS[self as usize - 1]
    }

    /// Kinds that fire on a single frame. One such frame, uncorroborated,
    /// is reported as a suspicion ([`Finding::suspect`]); the others
    /// already need repeats.
    pub const fn single_frame(self) -> bool {
        !matches!(
            self,
            Kind::StatusControl
                | Kind::ParamFlood
                | Kind::SigningDowngrade
                | Kind::SeqInterleave
                | Kind::SignedReplay
        )
    }

    /// Link-integrity kinds: only meaningful when the frames are known to
    /// come from one radio link ([`Link::distinct`]).
    pub const fn needs_link_id(self) -> bool {
        matches!(
            self,
            Kind::SigningDowngrade | Kind::SeqInterleave | Kind::SignedReplay
        )
    }
}

// ------------------------------------------------------------ thresholds --

/// A finding that keeps happening is reported again at most this often
/// (with the count since the last report), so the hub's threat stays
/// "active" (it expires after 10 s without a report) without flooding the
/// link.
pub const REPORT_GAP_MS: u64 = 2_000;
/// Byte-identical frames within this long are one frame heard twice (SiK
/// resends a packet that was not acknowledged; mLRS repeats), not a replay
/// or a second sender.
pub const RESEND_MS: u64 = 3_000;
/// Status text with control characters: this many messages...
pub const STATUS_CONTROL_HITS: u32 = 2;
/// ...within this window. One garbled message from a buggy component is a
/// benign oddity; a run of them is someone writing terminal escapes into
/// the ground station's log and message panes.
pub const STATUS_CONTROL_WINDOW_MS: u64 = 60_000;
/// MAVLink FTP offsets at or above 3.75 GiB. FAT32 (every autopilot SD
/// card) cannot hold a file this large; QGC-03's PoC uses 0xFFFF_FFF0.
pub const FTP_ABSURD_OFFSET: u32 = 0xF000_0000;
/// A request path this long. Autopilot paths are short (`/APM/LOGS/
/// 00000042.BIN`, `@PARAM/param.pck`, `/fs/microsd/log/...`); ArduPilot's
/// FTP server caps a path at the 239-byte data field. 200 leaves room for
/// any real directory tree.
pub const FTP_LONG_PATH: usize = 200;
/// ArduPilot's `RALLY_TOTAL` and `FENCE_TOTAL` are 8-bit parameters
/// (AP_Int8): nothing above this is a real count.
pub const TOTAL_MAX: f32 = 255.0;
/// Bits in each per-system parameter-name set (8192 bits, 1 KB).
pub const PARAM_BITS: usize = 8192;
/// Set bits that mean a flood: about 3,100 distinct names. ArduPilot has
/// roughly 1,300-1,800 parameters, PX4 about 1,100; 2,000 names set about
/// 1,770 bits. A full parameter download on every reconnect re-sends the
/// same names and sets no new bits.
pub const PARAM_FLOOD_BITS: u32 = 2_600;
/// The name sets start over this often, so a long day of reconnects and
/// firmware changes never adds up to a flood.
pub const PARAM_WINDOW_MS: u64 = 600_000;
/// Signing downgrade: an unsigned command counts when the same system sent
/// a signed frame on the same link this recently. (An operator who turns
/// signing off stops sending signed frames; after this long their unsigned
/// commands no longer count.)
pub const DOWNGRADE_RECENT_MS: u64 = 10_000;
/// Unsigned commands needed...
pub const DOWNGRADE_HITS: u32 = 2;
/// ...within this window.
pub const DOWNGRADE_WINDOW_MS: u64 = 30_000;
/// Replayed signed frames needed...
pub const REPLAY_HITS: u32 = 2;
/// ...within this window.
pub const REPLAY_WINDOW_MS: u64 = 60_000;
/// A sequence track not continued for this long is dropped (a sender that
/// rebooted and restarted its count is one stream again after this).
pub const TRACK_STALE_MS: u64 = 2_000;
/// Switches between two live sequence tracks needed...
pub const INTERLEAVE_SWITCHES: u32 = 10;
/// ...within this window, with at least [`INTERLEAVE_MIN_FRAMES`] frames on
/// each track. A lossy link or a reboot causes one or two switches; an
/// injector running alongside the real sender causes one per injected
/// frame.
pub const INTERLEAVE_WINDOW_MS: u64 = 10_000;
pub const INTERLEAVE_MIN_FRAMES: u32 = 5;
/// A sequence step of up to this many is "forward" (frames this scanner
/// cannot check, and lost packets, leave gaps).
pub const SEQ_FORWARD: u8 = 127;

/// `detail` bit of a `MavlinkAttack` threat ([`Finding::detail`]): a
/// suspicion, not yet an alarm. Every field of a MAVLink frame is the
/// sender's claim and a checksum is not a signature, so one checksum-valid
/// frame of a one-frame kind, with nothing else to back it, is reported
/// with this bit. It is backed (and reported without the bit) when:
///
/// - the same finding comes again from the same claimed sender in another
///   frame within [`SUSPECT_WINDOW_MS`] ([`CONFIRM_HITS`]), or
/// - another finding on the same link, from another frame, came within
///   [`SUSPECT_WINDOW_MS`], or
/// - the claimed system has a history on the link: heard at least
///   [`HISTORY_FRAMES`] frames over at least [`HISTORY_MS`] before (a real
///   vehicle or ground station sending the bait once, which is how the
///   exploit payloads are delivered).
///
/// A suspicion that becomes backed is reported again at once. The kind is
/// in the low byte either way.
pub const DETAIL_SUSPECT: u32 = 1 << 24;
/// A claimed system's history that backs a one-frame finding: frames...
pub const HISTORY_FRAMES: u32 = 5;
/// ...over at least this long.
pub const HISTORY_MS: u64 = 5_000;
/// Frames with the same finding from one claimed sender that escalate it.
pub const CONFIRM_HITS: u32 = 2;
/// ...within this long; also how recent a corroborating finding must be.
pub const SUSPECT_WINDOW_MS: u64 = 60_000;
/// New alarms (a kind from a claimed sender not reported before) per link:
/// a burst of this many...
pub const NEW_ALARM_BURST: u32 = 3;
/// ...then one every this long. A forger inventing system IDs gets a new
/// alarm out of each frame; this caps the storm, and every alarm held back
/// is counted ([`Pressure::throttled`]).
pub const NEW_ALARM_REFILL_MS: u64 = 10_000;
/// The same over all links of the analyser (a forger can invent links too,
/// when link IDs are on).
pub const NEW_ALARM_BURST_ALL: u32 = 6;
pub const NEW_ALARM_REFILL_ALL_MS: u64 = 5_000;
/// Links with a new-alarm budget of their own (the stalest makes room).
pub const MAX_BUDGETS: usize = 4;

/// Systems / components followed at once.
pub const MAX_SOURCES: usize = 12;
/// Parameter-name sets held at once (one per system sending PARAM_VALUE).
pub const MAX_PARAM_SETS: usize = 2;
/// Findings followed for the report rate limit.
pub const MAX_REPORTS: usize = 16;
/// Recent frame fingerprints for the resend check.
pub const MAX_RECENT: usize = 32;

// ----------------------------------------------------------------- types --

/// One check that fired: kind, target, excerpt.
type Hit = (Kind, Option<(u8, u8)>, heapless::String<32>);

/// Which radio link frames came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Link {
    /// Any number the caller uses to tell links apart.
    pub key: u32,
    /// The key identifies one radio link (a SiK NET ID, an mLRS sync
    /// word). When false, several links may share the key, so the
    /// link-integrity kinds ([`Kind::needs_link_id`]) are not checked: two
    /// honest drones with the default system ID 1 on two SiK links would
    /// look like an injector.
    pub distinct: bool,
}

/// One finding, at most every [`REPORT_GAP_MS`] per kind and sender.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub kind: Kind,
    pub link: Link,
    /// The sender as claimed in the frame.
    pub sysid: u8,
    pub compid: u8,
    /// Who the message is aimed at, when it says.
    pub target: Option<(u8, u8)>,
    pub msgid: u32,
    /// Hits since the previous report (the first report: 1, or the
    /// threshold count).
    pub hits: u32,
    /// When this kind from this sender was first seen.
    pub first_ms: u64,
    /// The offending text, printable characters only (a URI, a file name,
    /// shell input), or a short description. Never key material.
    pub excerpt: heapless::String<32>,
    /// One frame of a one-frame kind with nothing yet to back it (see
    /// [`DETAIL_SUSPECT`]).
    pub suspect: bool,
}

impl Finding {
    /// The threat's `detail`: the [`Kind`] code, plus [`DETAIL_SUSPECT`].
    pub fn detail(&self) -> u32 {
        self.kind.code() | if self.suspect { DETAIL_SUSPECT } else { 0 }
    }
}

/// Signs that a forger is pushing the analyser's limits, for a node's
/// stats line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Pressure {
    /// New alarms held back by the new-alarm budget.
    pub throttled: u32,
}

/// A token bucket.
#[derive(Debug, Clone, Copy)]
struct Bucket {
    link: u32,
    tokens: u32,
    at_ms: u64,
}

impl Bucket {
    fn take(&mut self, now_ms: u64, burst: u32, refill_ms: u64) -> bool {
        if self.tokens >= burst {
            self.at_ms = now_ms;
        } else {
            let add = now_ms.saturating_sub(self.at_ms) / refill_ms;
            if add > 0 {
                self.tokens = (self.tokens as u64 + add).min(burst as u64) as u32;
                self.at_ms += add * refill_ms;
            }
        }
        if self.tokens == 0 {
            return false;
        }
        self.tokens -= 1;
        true
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Window {
    n: u32,
    since_ms: u64,
}

impl Window {
    /// Count one event in a window that starts at the first event and
    /// lasts `span_ms`; returns the count.
    fn add(&mut self, now_ms: u64, span_ms: u64) -> u32 {
        if self.n == 0 || now_ms.saturating_sub(self.since_ms) > span_ms {
            self.n = 0;
            self.since_ms = now_ms;
        }
        self.n += 1;
        self.n
    }
}

#[derive(Debug, Clone, Copy)]
struct Track {
    seq: u8,
    last_ms: u64,
    frames: u32,
}

#[derive(Debug, Clone)]
struct Source {
    link: u32,
    sysid: u8,
    compid: u8,
    first_ms: u64,
    last_ms: u64,
    frames: u32,
    tracks: [Option<Track>; 2],
    last_track: u8,
    switches: Window,
    last_signed_ms: Option<u64>,
    /// Newest signing timestamp per signing link ID (two link IDs).
    stamps: [Option<(u8, u64)>; 2],
    downgrade: Window,
    replay: Window,
    status_ctrl: Window,
    /// The end of the previous STATUSTEXT, to catch `[link;` split across
    /// two messages.
    status_tail: Vec<u8, 5>,
}

impl Source {
    fn new(link: u32, sysid: u8, compid: u8, now_ms: u64) -> Self {
        Self {
            link,
            sysid,
            compid,
            first_ms: now_ms,
            last_ms: now_ms,
            frames: 0,
            tracks: [None; 2],
            last_track: 0,
            switches: Window::default(),
            last_signed_ms: None,
            stamps: [None; 2],
            downgrade: Window::default(),
            replay: Window::default(),
            status_ctrl: Window::default(),
            status_tail: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
struct ParamSet {
    link: u32,
    sysid: u8,
    since_ms: u64,
    last_ms: u64,
    set: u32,
    bits: [u8; PARAM_BITS / 8],
}

#[derive(Debug, Clone)]
struct Report {
    kind: Kind,
    link: u32,
    sysid: u8,
    compid: u8,
    first_ms: u64,
    last_report_ms: Option<u64>,
    last_ms: u64,
    hits: u32,
    /// Hits in the window that started at `since_ms`.
    total: u32,
    since_ms: u64,
    /// Escalated (or a kind that needs repeats).
    confirmed: bool,
    /// Hash of the frame behind the latest hit.
    frame_h: u32,
}

/// The analyser. Fixed memory (about 4 KB); no allocation.
#[derive(Debug, Clone)]
pub struct Analyser {
    sources: Vec<Source, MAX_SOURCES>,
    params: Vec<ParamSet, MAX_PARAM_SETS>,
    reports: Vec<Report, MAX_REPORTS>,
    recent: Deque<(u32, u64), MAX_RECENT>,
    budgets: Vec<Bucket, MAX_BUDGETS>,
    budget_all: Bucket,
    pressure: Pressure,
}

impl Default for Analyser {
    fn default() -> Self {
        Self::new()
    }
}

/// FNV-1a.
fn fnv(data: &[u8], seed: u32) -> u32 {
    let mut h = 0x811C_9DC5u32 ^ seed;
    for &b in data {
        h = (h ^ b as u32).wrapping_mul(0x0100_0193);
    }
    h
}

fn excerpt(s: &[u8]) -> heapless::String<32> {
    text::excerpt(s)
}

fn describe(args: core::fmt::Arguments<'_>) -> heapless::String<32> {
    use core::fmt::Write;
    let mut s = heapless::String::new();
    let _ = s.write_fmt(args);
    s
}

impl Analyser {
    pub const fn new() -> Self {
        Self {
            sources: Vec::new(),
            params: Vec::new(),
            reports: Vec::new(),
            recent: Deque::new(),
            budgets: Vec::new(),
            budget_all: Bucket {
                link: 0,
                tokens: NEW_ALARM_BURST_ALL,
                at_ms: 0,
            },
            pressure: Pressure { throttled: 0 },
        }
    }

    /// Counters for a node's stats line.
    pub fn pressure(&self) -> Pressure {
        self.pressure
    }

    /// A token for a new alarm on `link` (and one from the shared bucket).
    fn new_alarm(&mut self, link: u32, now_ms: u64) -> bool {
        let i = match self.budgets.iter().position(|b| b.link == link) {
            Some(i) => i,
            None => {
                let b = Bucket {
                    link,
                    tokens: NEW_ALARM_BURST,
                    at_ms: now_ms,
                };
                if self.budgets.is_full() {
                    let i = (0..self.budgets.len())
                        .min_by_key(|&i| self.budgets[i].at_ms)
                        .unwrap_or(0);
                    self.budgets[i] = b;
                    i
                } else {
                    let _ = self.budgets.push(b);
                    self.budgets.len() - 1
                }
            }
        };
        // Look before taking, so a refusal costs neither bucket a token.
        let mut one = self.budgets[i];
        let mut all = self.budget_all;
        if !(one.take(now_ms, NEW_ALARM_BURST, NEW_ALARM_REFILL_MS)
            && all.take(now_ms, NEW_ALARM_BURST_ALL, NEW_ALARM_REFILL_ALL_MS))
        {
            return false;
        }
        self.budgets[i] = one;
        self.budget_all = all;
        true
    }

    /// Analyse one CRC-valid frame from `link`. `out` is called with each
    /// finding due for a report and the frame that triggered it.
    pub fn frame(
        &mut self,
        link: Link,
        f: &Frame<'_>,
        now_ms: u64,
        out: &mut dyn FnMut(&Finding, &Frame<'_>),
    ) {
        // A frame heard twice (radio resend) is analysed once.
        let h = fnv(f.bytes, link.key);
        while self
            .recent
            .front()
            .is_some_and(|r| now_ms.saturating_sub(r.1) > RESEND_MS)
        {
            self.recent.pop_front();
        }
        if self.recent.iter().any(|r| r.0 == h) {
            return;
        }
        if self.recent.is_full() {
            self.recent.pop_front();
        }
        let _ = self.recent.push_back((h, now_ms));

        let si = self.source(link.key, f.sysid, f.compid, now_ms);
        // History of the claimed system before this frame.
        let known = self.sources.iter().any(|s| {
            s.link == link.key
                && s.sysid == f.sysid
                && s.frames >= HISTORY_FRAMES
                && now_ms.saturating_sub(s.first_ms) >= HISTORY_MS
        });
        let s = &mut self.sources[si];
        s.frames = s.frames.saturating_add(1);
        let mut hits: Vec<Hit, 4> = Vec::new();
        let mut hit = |k: Kind, t: Option<(u8, u8)>, e: heapless::String<32>| {
            let _ = hits.push((k, t, e));
        };

        if link.distinct {
            self.integrity(si, link, f, now_ms, &mut hit);
        }
        if let Some((sent, want)) = f.odd_length() {
            let name = crate::frame::info(f.msgid).map_or("?", |m| m.name);
            hit(
                Kind::OddLength,
                target_of(f),
                describe(format_args!("v{} {name} len {sent}/{want}", f.version)),
            );
        }
        self.payload(si, link, f, now_ms, &mut hit);

        for (k, t, e) in hits {
            self.report(k, link, f, h, known, t, e, now_ms, out);
        }
    }

    /// Index of the source for (link, sysid, compid), created if new (the
    /// least recently heard one makes room).
    fn source(&mut self, link: u32, sysid: u8, compid: u8, now_ms: u64) -> usize {
        if let Some(i) = self
            .sources
            .iter()
            .position(|s| s.link == link && s.sysid == sysid && s.compid == compid)
        {
            self.sources[i].last_ms = now_ms;
            return i;
        }
        let s = Source::new(link, sysid, compid, now_ms);
        if self.sources.is_full() {
            let i = (0..self.sources.len())
                .min_by_key(|&i| self.sources[i].last_ms)
                .unwrap_or(0);
            self.sources[i] = s;
            i
        } else {
            let _ = self.sources.push(s);
            self.sources.len() - 1
        }
    }

    // ------------------------------------------------- link integrity --

    fn integrity(
        &mut self,
        si: usize,
        link: Link,
        f: &Frame<'_>,
        now_ms: u64,
        hit: &mut dyn FnMut(Kind, Option<(u8, u8)>, heapless::String<32>),
    ) {
        // Sequence tracks.
        {
            let s = &mut self.sources[si];
            for t in s.tracks.iter_mut() {
                if t.is_some_and(|t| now_ms.saturating_sub(t.last_ms) > TRACK_STALE_MS) {
                    *t = None;
                }
            }
            let deltas = s.tracks.map(|t| t.map(|t| f.seq.wrapping_sub(t.seq)));
            if !deltas.contains(&Some(0)) {
                let best = (0..2)
                    .filter_map(|i| {
                        deltas[i]
                            .filter(|&d| (1..=SEQ_FORWARD).contains(&d))
                            .map(|d| (d, i))
                    })
                    .min();
                let i = match best {
                    Some((_, i)) => i,
                    None => match s.tracks.iter().position(Option::is_none) {
                        Some(i) => i,
                        None => (0..2)
                            .min_by_key(|&i| s.tracks[i].map_or(0, |t| t.last_ms))
                            .unwrap_or(0),
                    },
                };
                let frames = match (best, s.tracks[i]) {
                    (Some(_), Some(t)) => t.frames + 1,
                    _ => 1,
                };
                s.tracks[i] = Some(Track {
                    seq: f.seq,
                    last_ms: now_ms,
                    frames,
                });
                let both = s.tracks.iter().all(Option::is_some);
                if both && s.last_track as usize != i {
                    let n = s.switches.add(now_ms, INTERLEAVE_WINDOW_MS);
                    let enough = s
                        .tracks
                        .iter()
                        .all(|t| t.is_some_and(|t| t.frames >= INTERLEAVE_MIN_FRAMES));
                    if n >= INTERLEAVE_SWITCHES && enough {
                        let (a, b) = (
                            s.tracks[0].map_or(0, |t| t.seq),
                            s.tracks[1].map_or(0, |t| t.seq),
                        );
                        hit(
                            Kind::SeqInterleave,
                            None,
                            describe(format_args!("seq {a} and {b}, {n} switches")),
                        );
                    }
                }
                s.last_track = i as u8;
            }
        }

        // Signing.
        if let Some(sig) = f.signature {
            let s = &mut self.sources[si];
            s.last_signed_ms = Some(now_ms);
            let slot = s
                .stamps
                .iter()
                .position(|x| x.is_some_and(|x| x.0 == sig.link_id));
            match slot {
                Some(j) => {
                    let (_, last) = s.stamps[j].unwrap_or((0, 0));
                    if sig.timestamp <= last {
                        let n = s.replay.add(now_ms, REPLAY_WINDOW_MS);
                        if n >= REPLAY_HITS {
                            let back = last - sig.timestamp;
                            hit(
                                Kind::SignedReplay,
                                None,
                                describe(format_args!(
                                    "link {} ts -{} ms",
                                    sig.link_id,
                                    back / 100
                                )),
                            );
                        }
                    } else {
                        s.stamps[j] = Some((sig.link_id, sig.timestamp));
                    }
                }
                None => {
                    let j = s.stamps.iter().position(Option::is_none).unwrap_or(1);
                    s.stamps[j] = Some((sig.link_id, sig.timestamp));
                }
            }
        } else if msg::COMMANDS.contains(&f.msgid) {
            let signed_recently = self.sources.iter().any(|o| {
                o.link == link.key
                    && o.sysid == f.sysid
                    && o.last_signed_ms
                        .is_some_and(|t| now_ms.saturating_sub(t) <= DOWNGRADE_RECENT_MS)
            });
            if signed_recently {
                let n = self.sources[si].downgrade.add(now_ms, DOWNGRADE_WINDOW_MS);
                if n >= DOWNGRADE_HITS {
                    let name = crate::frame::info(f.msgid).map_or("?", |m| m.name);
                    hit(
                        Kind::SigningDowngrade,
                        target_of(f),
                        describe(format_args!("unsigned {name}")),
                    );
                }
            }
        }
    }

    // --------------------------------------------------------- payloads --

    fn payload(
        &mut self,
        si: usize,
        link: Link,
        f: &Frame<'_>,
        now_ms: u64,
        hit: &mut dyn FnMut(Kind, Option<(u8, u8)>, heapless::String<32>),
    ) {
        let uri_kind = |p: Problem| match p {
            Problem::Traversal => Kind::UriTraversal,
            Problem::Scheme => Kind::UriScheme,
            Problem::Pipeline => Kind::VideoPipeline,
        };
        match f.msgid {
            msg::CAMERA_INFORMATION => {
                use msg::camera_information as c;
                let uri = f.text(c::URI, c::URI_LEN);
                if let Some(p) = text::metadata_uri(uri) {
                    hit(uri_kind(p), None, excerpt(uri));
                }
                for off in [c::VENDOR, c::MODEL] {
                    let name = f.text(off, c::NAME_LEN);
                    if text::file_name(name).is_some() {
                        hit(Kind::CameraName, None, excerpt(name));
                        break;
                    }
                }
            }
            msg::VIDEO_STREAM_INFORMATION => {
                use msg::video_stream_information as v;
                let uri = f.text(v::URI, v::URI_LEN);
                if let Some(p) = text::video_uri(uri) {
                    hit(uri_kind(p), None, excerpt(uri));
                }
            }
            msg::COMPONENT_INFORMATION => {
                use msg::component_information as c;
                for off in [c::GENERAL_URI, c::PERIPHERALS_URI] {
                    let uri = f.text(off, c::URI_LEN);
                    if let Some(p) = text::metadata_uri(uri) {
                        hit(uri_kind(p), None, excerpt(uri));
                        break;
                    }
                }
            }
            msg::COMPONENT_METADATA => {
                use msg::component_metadata as c;
                let uri = f.text(c::URI, c::URI_LEN);
                if let Some(p) = text::metadata_uri(uri) {
                    hit(uri_kind(p), None, excerpt(uri));
                }
            }
            msg::STATUSTEXT => {
                use msg::statustext as st;
                let t = f.text(st::TEXT, st::TEXT_LEN);
                let s = &mut self.sources[si];
                let mut joined: Vec<u8, 55> = Vec::new();
                let _ = joined.extend_from_slice(&s.status_tail);
                let _ = joined.extend_from_slice(t);
                if text::link_markup(&joined) {
                    hit(Kind::StatusLink, None, excerpt(t));
                }
                s.status_tail.clear();
                let _ = s
                    .status_tail
                    .extend_from_slice(&t[t.len().saturating_sub(5)..]);
                if text::text_control(t) > 0 {
                    let n = s.status_ctrl.add(now_ms, STATUS_CONTROL_WINDOW_MS);
                    if n >= STATUS_CONTROL_HITS {
                        hit(Kind::StatusControl, None, excerpt(t));
                    }
                }
            }
            msg::FILE_TRANSFER_PROTOCOL => ftp_checks(f, hit),
            msg::PARAM_VALUE => self.param_value(link, f, now_ms, hit),
            msg::PARAM_SET => {
                use msg::param_set as p;
                let ty = f.u8(p::TYPE);
                let v = f.f32(p::VALUE);
                if (ty == msg::PARAM_TYPE_REAL32 || ty == msg::PARAM_TYPE_REAL64) && !v.is_finite()
                {
                    let id = excerpt(f.text(p::ID, 16));
                    hit(
                        Kind::ParamSetNonFinite,
                        Some((f.u8(p::TARGET_SYSTEM), f.u8(p::TARGET_COMPONENT))),
                        describe(format_args!("{}={}", id, v)),
                    );
                }
            }
            msg::SERIAL_CONTROL => {
                use msg::serial_control as sc;
                let count = f.u8(sc::COUNT) as usize;
                let dev = f.u8(sc::DEVICE);
                let target = (f.payload.len() > sc::TARGET_SYSTEM)
                    .then(|| (f.u8(sc::TARGET_SYSTEM), f.u8(sc::TARGET_COMPONENT)));
                let data = f.raw(sc::DATA, count.min(sc::DATA_LEN));
                let mut e: heapless::String<32> = describe(format_args!("dev {dev} n {count}: "));
                for c in excerpt(data).chars() {
                    if e.push(c).is_err() {
                        break;
                    }
                }
                let kind = if count > sc::DATA_LEN {
                    Kind::SerialControlMalformed
                } else {
                    Kind::SerialControl
                };
                hit(kind, target, e);
            }
            msg::SETUP_SIGNING => {
                use msg::setup_signing as s;
                // The excerpt must not carry the key.
                hit(
                    Kind::SetupSigning,
                    Some((f.u8(s::TARGET_SYSTEM), f.u8(s::TARGET_COMPONENT))),
                    describe(format_args!("signing key in the clear")),
                );
            }
            _ => {}
        }
    }

    fn param_value(
        &mut self,
        link: Link,
        f: &Frame<'_>,
        now_ms: u64,
        hit: &mut dyn FnMut(Kind, Option<(u8, u8)>, heapless::String<32>),
    ) {
        use msg::param_value as p;
        let id = f.text(p::ID, 16);
        let v = f.f32(p::VALUE);
        if id == b"RALLY_TOTAL" || id == b"FENCE_TOTAL" {
            let whole = v.is_finite() && (0.0..=TOTAL_MAX).contains(&v) && v == (v as u32) as f32;
            if !whole {
                hit(
                    Kind::ParamTotal,
                    None,
                    describe(format_args!("{}={}", excerpt(id), v)),
                );
            }
        }
        // Distinct-name set for this system.
        let k = match self
            .params
            .iter()
            .position(|s| s.link == link.key && s.sysid == f.sysid)
        {
            Some(k) => k,
            None => {
                let fresh = ParamSet {
                    link: link.key,
                    sysid: f.sysid,
                    since_ms: now_ms,
                    last_ms: now_ms,
                    set: 0,
                    bits: [0; PARAM_BITS / 8],
                };
                if self.params.is_full() {
                    let k = (0..self.params.len())
                        .min_by_key(|&k| self.params[k].last_ms)
                        .unwrap_or(0);
                    self.params[k] = fresh;
                    k
                } else {
                    let _ = self.params.push(fresh);
                    self.params.len() - 1
                }
            }
        };
        let s = &mut self.params[k];
        if now_ms.saturating_sub(s.since_ms) > PARAM_WINDOW_MS {
            s.bits = [0; PARAM_BITS / 8];
            s.set = 0;
            s.since_ms = now_ms;
        }
        s.last_ms = now_ms;
        let b = fnv(id, 0) as usize % PARAM_BITS;
        if s.bits[b / 8] & (1 << (b % 8)) == 0 {
            s.bits[b / 8] |= 1 << (b % 8);
            s.set += 1;
        }
        if s.set >= PARAM_FLOOD_BITS {
            hit(
                Kind::ParamFlood,
                None,
                describe(format_args!("{} names, e.g. {}", s.set, excerpt(id))),
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn report(
        &mut self,
        kind: Kind,
        link: Link,
        f: &Frame<'_>,
        frame_h: u32,
        known: bool,
        target: Option<(u8, u8)>,
        excerpt: heapless::String<32>,
        now_ms: u64,
        out: &mut dyn FnMut(&Finding, &Frame<'_>),
    ) {
        let i = match self.reports.iter().position(|r| {
            r.kind == kind && r.link == link.key && r.sysid == f.sysid && r.compid == f.compid
        }) {
            Some(i) => i,
            None => {
                let r = Report {
                    kind,
                    link: link.key,
                    sysid: f.sysid,
                    compid: f.compid,
                    first_ms: now_ms,
                    last_report_ms: None,
                    last_ms: now_ms,
                    hits: 0,
                    total: 0,
                    since_ms: now_ms,
                    confirmed: false,
                    frame_h,
                };
                if self.reports.is_full() {
                    let i = (0..self.reports.len())
                        .min_by_key(|&i| self.reports[i].last_ms)
                        .unwrap_or(0);
                    self.reports[i] = r;
                    i
                } else {
                    let _ = self.reports.push(r);
                    self.reports.len() - 1
                }
            }
        };
        // Another finding on this link, from another frame, recently.
        let corroborated = self.reports.iter().enumerate().any(|(j, o)| {
            j != i
                && o.link == link.key
                && o.frame_h != frame_h
                && now_ms.saturating_sub(o.last_ms) <= SUSPECT_WINDOW_MS
        });
        let r = &mut self.reports[i];
        r.hits += 1;
        r.last_ms = now_ms;
        if now_ms.saturating_sub(r.since_ms) > SUSPECT_WINDOW_MS {
            r.total = 0;
            r.since_ms = now_ms;
        }
        r.total += 1;
        let was = r.confirmed;
        r.confirmed |= !kind.single_frame() || r.total >= CONFIRM_HITS || corroborated || known;
        r.frame_h = frame_h;
        let escalated = !was && r.confirmed && r.last_report_ms.is_some();
        if !escalated
            && r.last_report_ms
                .is_some_and(|t| now_ms.saturating_sub(t) < REPORT_GAP_MS)
        {
            return;
        }
        if r.last_report_ms.is_none() && !self.new_alarm(link.key, now_ms) {
            self.pressure.throttled = self.pressure.throttled.saturating_add(1);
            return;
        }
        let r = &mut self.reports[i];
        let finding = Finding {
            kind,
            link,
            sysid: f.sysid,
            compid: f.compid,
            target,
            msgid: f.msgid,
            hits: r.hits,
            first_ms: r.first_ms,
            excerpt,
            suspect: !r.confirmed,
        };
        r.hits = 0;
        r.last_report_ms = Some(now_ms);
        out(&finding, f);
    }
}

/// (target_system, target_component) of a command-like message.
fn target_of(f: &Frame<'_>) -> Option<(u8, u8)> {
    use msg::*;
    match f.msgid {
        COMMAND_LONG | COMMAND_INT => Some((
            f.u8(command::TARGET_SYSTEM),
            f.u8(command::TARGET_COMPONENT),
        )),
        PARAM_SET => Some((
            f.u8(param_set::TARGET_SYSTEM),
            f.u8(param_set::TARGET_COMPONENT),
        )),
        FILE_TRANSFER_PROTOCOL => Some((f.u8(ftp::TARGET_SYSTEM), f.u8(ftp::TARGET_COMPONENT))),
        _ => None,
    }
}

fn ftp_checks(f: &Frame<'_>, hit: &mut dyn FnMut(Kind, Option<(u8, u8)>, heapless::String<32>)) {
    let target = Some((f.u8(ftp::TARGET_SYSTEM), f.u8(ftp::TARGET_COMPONENT)));
    let op = f.u8(ftp::OPCODE);
    let size = f.u8(ftp::SIZE) as usize;
    let req = f.u8(ftp::REQ_OPCODE);
    let offset = f.u32(ftp::OFFSET);
    if size > ftp::DATA_LEN {
        hit(
            Kind::FtpMalformed,
            target,
            describe(format_args!("op {op} size {size}")),
        );
        return;
    }
    // Zero-extended data (MAVLink 2 trims trailing zeros).
    let mut data = [0u8; ftp::DATA_LEN];
    let sent = f.raw(ftp::DATA, size);
    data[..sent.len()].copy_from_slice(sent);
    let data = &data[..size];
    match op {
        ftp::ACK => match req {
            ftp::LIST_DIRECTORY => {
                for entry in data.split(|&b| b == 0) {
                    let Some((&ty, rest)) = entry.split_first() else {
                        continue;
                    };
                    let name = match ty {
                        b'F' => rest.split(|&b| b == b'\t').next().unwrap_or(rest),
                        b'D' => rest,
                        _ => continue,
                    };
                    // "." and ".." as whole entries are what a directory
                    // listing may contain; inside a name they are not.
                    if name == b"." || name == b".." {
                        continue;
                    }
                    if text::file_name(name) == Some(Problem::Traversal) {
                        hit(Kind::FtpTraversal, target, excerpt(name));
                        break;
                    }
                }
            }
            ftp::READ_FILE | ftp::BURST_READ_FILE if offset >= FTP_ABSURD_OFFSET => {
                hit(
                    Kind::FtpOffset,
                    target,
                    describe(format_args!("read ack at {offset:#010x}")),
                );
            }
            ftp::OPEN_FILE_RO | ftp::OPEN_FILE_WO if size > 4 => {
                hit(
                    Kind::FtpMalformed,
                    target,
                    describe(format_args!("open ack size {size}")),
                );
            }
            ftp::WRITE_FILE if size == 4 => {
                let n = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                if n > ftp::DATA_LEN as u32 {
                    hit(
                        Kind::FtpMalformed,
                        target,
                        describe(format_args!("write ack {n} bytes")),
                    );
                }
            }
            _ => {}
        },
        op if ftp::PATH_OPCODES.contains(&op) => {
            let path = &data[..data.iter().position(|&b| b == 0).unwrap_or(data.len())];
            if path.len() >= FTP_LONG_PATH {
                hit(
                    Kind::FtpMalformed,
                    target,
                    describe(format_args!("path of {} bytes", path.len())),
                );
            }
        }
        _ => {}
    }
}
