//! Claims and disputes: a frame is evidence about a transmitter, not a
//! command to the track (WP-10, docs/SPOOFING.md "Impersonation and
//! disputes").
//!
//! Nothing a drone broadcasts is authenticated. Remote ID serials go out in
//! clear, CRCs are public, a decrypted DJI O4 record proves only that
//! someone used the receiver's public key or replayed a recording. So
//! anyone in range can send a well-formed frame that carries a real drone's
//! ID and any position, operator position or text. Before WP-10 one such
//! frame moved the real drone, two gave *it* a spoof flag, and a frame
//! naming two IDs merged two drones for good.
//!
//! # The rule
//!
//! - **Established.** A confirmed track ([`crate::probation`]) with a
//!   position reported in the last [`ESTABLISHED_MAX_AGE_MS`] has an
//!   established path. A new position report is checked against it with
//!   the [`crate::spoof::Check::PositionJump`] limit (60 m + twice the
//!   declared accuracy + 100 m/s for the time between them + 2 s): a
//!   report inside it is the track's own and is applied as before.
//! - **Rival claim.** A report outside it is **not applied** (nor is
//!   anything else in its frame: operator position, texts, IDs). It is
//!   held as the track's rival claim: one compact position, when it was
//!   first and last heard, how many reports followed it plausibly, its
//!   source and address. A report that does not fit the rival either starts
//!   a new rival.
//! - **Dispute.** When the rival forms its own plausible path
//!   ([`RIVAL_MIN_FRAMES`] reports over [`RIVAL_MIN_SPAN_MS`]) while the
//!   original keeps reporting too (the reports switched between the two
//!   paths [`DISPUTE_MIN_SWITCHES`] times), the track is **disputed**: two
//!   transmitters claim one ID. It stays disputed until the rival has not
//!   been heard for [`RIVAL_TTL_MS`]. A dispute is not a spoof flag: it says
//!   nothing about which one is real.
//! - **Follow.** When the original has not reported for
//!   [`FOLLOW_AFTER_MS`] and the rival keeps reporting along its own path,
//!   the track follows the rival (its next report is applied). A real
//!   drone's GPS can glitch (seconds) and a real drone can stop
//!   transmitting (landed, out of range, switched off): 10 s is several
//!   times the 1 s Remote ID repetition, longer than the glitches seen in
//!   flight logs, and short enough that the operator does not watch a
//!   frozen marker for a drone that has really moved. Without a dispute
//!   that is a single transmitter that really jumped, and only then does the
//!   track get a [`crate::spoof::Check::PositionJump`] flag.
//! - **Text fields and IDs.** Self-ID, operator ID, and a different ID of a
//!   type the track already has (a new session ID) never replace the
//!   established value on one frame. The new value must be heard
//!   [`ADOPT_MIN_FRAMES`] times over [`ADOPT_MS`] without the old one coming
//!   back. Old and new alternating ([`TEXT_FLIPS_DISPUTE`] times) is a
//!   dispute over that field; a rival frame whose text differs is part of
//!   the dispute at once.
//! - **Merges.** Two existing tracks become one only after
//!   [`MERGE_MIN_FRAMES`] frames over [`MERGE_SPAN_MS`] named both (or an ID
//!   of one came from the other's address) and only when their positions
//!   are compatible and neither is disputed ([`PendingMerge`]). An
//!   address-only track (no ID yet: a Bluetooth 4 advert carries one message
//!   at a time) whose position is compatible still joins on the first frame
//!   that carries the ID from its address, as before. Several different IDs
//!   of one type in one frame is never sent by an honest drone: such a frame
//!   is counted and applied to nothing.
//!
//! # Cost
//!
//! [`Claims`] is 48 bytes per track (3 KB for 64 tracks). Per frame: one distance against the
//! established fix (two when a rival is alive), per matched track; the
//! merge table is four entries. Nothing grows with history.

use core::fmt;

use rid::message::LatLon;

use crate::spoof::{dist_m, jump_limit_m, worst_hacc};
use crate::text::IdText;
use crate::{Track, TrackId};

/// The original silent this long while the rival continues: follow it.
pub const FOLLOW_AFTER_MS: u64 = 10_000;
/// Reports a rival needs along its own path...
pub const RIVAL_MIN_FRAMES: u8 = 3;
/// ...spread over at least this long, to be a path rather than noise.
pub const RIVAL_MIN_SPAN_MS: u32 = 2_000;
/// Switches between the original's and the rival's path (with the rival
/// alive) that make it a dispute: the original reported between the
/// rival's reports at least twice. A glitch that recovers switches twice.
pub const DISPUTE_MIN_SWITCHES: u8 = 4;
/// A rival not heard for this long is forgotten; the dispute ends.
pub const RIVAL_TTL_MS: u32 = 15_000;
/// A position older than this is not an established path (the same
/// window as the position-jump check).
pub const ESTABLISHED_MAX_AGE_MS: u64 = crate::spoof::JUMP_WINDOW_MS;
/// A changed text field or ID is adopted after this long...
pub const ADOPT_MS: u32 = 5_000;
/// ...and this many frames carrying it, without the old value between.
pub const ADOPT_MIN_FRAMES: u8 = 3;
/// Old and new value alternating this many times: a dispute over the field.
pub const TEXT_FLIPS_DISPUTE: u8 = 3;
/// Two tracks are merged after this many frames saying they are one...
pub const MERGE_MIN_FRAMES: u8 = 3;
/// ...over at least this long, with compatible positions.
pub const MERGE_SPAN_MS: u32 = 2_000;
/// Pending merges remembered (tracker-wide).
pub const MAX_PENDING_MERGES: usize = 4;
/// Two frames closer than this count once towards a merge.
const MERGE_GAP_MS: u32 = 300;

/// Where a claimed position came from (JSON `src` of a rival claim).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[repr(u8)]
pub enum ClaimSrc {
    RemoteId = 0,
    /// OcuSync plaintext DroneID.
    DroneId = 1,
    /// DJI legacy Wi-Fi DroneID.
    DjiWifi = 2,
    ControlLink = 3,
    /// A DJI O4 record decrypted with the owner's key: decrypted, not
    /// authenticated.
    DroneIdO4 = 4,
}

impl ClaimSrc {
    fn from_u8(v: u8) -> ClaimSrc {
        match v {
            1 => ClaimSrc::DroneId,
            2 => ClaimSrc::DjiWifi,
            3 => ClaimSrc::ControlLink,
            4 => ClaimSrc::DroneIdO4,
            _ => ClaimSrc::RemoteId,
        }
    }

    /// Short words for screens and logs.
    pub const fn label(self) -> &'static str {
        match self {
            ClaimSrc::RemoteId => "Remote ID",
            ClaimSrc::DroneId => "DJI DroneID",
            ClaimSrc::DjiWifi => "DJI Wi-Fi DroneID",
            ClaimSrc::ControlLink => "control link",
            ClaimSrc::DroneIdO4 => "DJI O4, decrypted, not authenticated",
        }
    }
}

impl From<crate::Source> for ClaimSrc {
    fn from(s: crate::Source) -> Self {
        match s {
            crate::Source::RemoteId => ClaimSrc::RemoteId,
            crate::Source::DroneId => ClaimSrc::DroneId,
            crate::Source::DjiWifi => ClaimSrc::DjiWifi,
            crate::Source::ControlLink => ClaimSrc::ControlLink,
        }
    }
}

/// What a dispute is about (bits of [`Dispute::what`], the evidence data).
pub mod what {
    /// Two positions: the rival forms its own path while the original
    /// continues.
    pub const POSITION: u8 = 1;
    pub const SELF_ID: u8 = 2;
    pub const OPERATOR_ID: u8 = 4;
    /// A second ID of one type (e.g. two session IDs alternating).
    pub const ID: u8 = 8;
    pub const ALL: u8 = POSITION | SELF_ID | OPERATOR_ID | ID;
}

/// The track switched to the other claim during a dispute.
const B_FOLLOWED: u8 = 0x10;
/// Has ever been disputed.
const B_EVER: u8 = 0x20;
/// The latest position report was the rival's.
const B_LAST_RIVAL: u8 = 0x40;

/// Pending change of one text field or ID: 16 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Guard {
    hash: u32,
    since: u32,
    last: u32,
    /// Frames carrying the pending value (0: none pending).
    n: u8,
    /// 1 Self-ID, 2 operator ID, 0x10 | ID type code.
    kind: u8,
    /// Old and new value alternated.
    flips: u8,
}

pub(crate) const KIND_SELF_ID: u8 = 1;
pub(crate) const KIND_OPERATOR_ID: u8 = 2;
pub(crate) const fn kind_id(type_code: u8) -> u8 {
    0x10 | (type_code & 0x0f)
}

/// What a track knows about rival claims on its ID. 48 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Claims {
    /// The rival's latest position (1e-7 degrees).
    rival_pos: (i32, i32),
    /// Hub ms (low 32 bits) the rival was first and last heard.
    rival_first: u32,
    rival_last: u32,
    /// The rival's transmitter address, folded to 16 bits (0: none).
    rival_addr: u16,
    /// Reports along the rival's path (0: no rival).
    rival_frames: u8,
    rival_src: u8,
    /// Switches between the original's and the rival's path.
    switches: u8,
    /// [`what`] bits of the current dispute, plus `B_*`.
    bits: u8,
    /// Low nibble: [`what`] bits already handed out
    /// ([`crate::Tracker::take_dispute`]). High nibble: text fields that
    /// differed in the rival's frames, part of the dispute once it is one.
    marks: u8,
    /// Reports not applied because they did not fit the established path.
    rejected: u8,
    /// Frames naming this track's ID beside another ID of the same type.
    contradictory: u8,
    /// Hub ms (low 32 bits) the dispute was last fed, and how many whole
    /// seconds it had been open then.
    last: u32,
    open_s: u16,
    guard: Guard,
}

/// A rival claim, as formatters show it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rival {
    pub position: LatLon,
    pub src: ClaimSrc,
    /// Reports along its path.
    pub frames: u8,
    /// Hub ms it was first and last heard.
    pub first_ms: u64,
    pub last_ms: u64,
}

/// A dispute, as formatters show it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Dispute {
    /// [`what`] bits.
    pub what: u8,
    pub since_ms: u64,
    /// The other position, while it is being heard.
    pub rival: Option<Rival>,
    /// The track switched to the other claim after its original went quiet.
    pub followed: bool,
}

/// Full hub ms of a low-32-bit stamp in the past of `now`.
fn full(now: u64, t: u32) -> u64 {
    now.saturating_sub(u64::from((now as u32).wrapping_sub(t)))
}

fn e7(p: LatLon) -> (i32, i32) {
    ((p.lat * 1e7) as i32, (p.lon * 1e7) as i32)
}

fn from_e7(v: (i32, i32)) -> LatLon {
    LatLon { lat: f64::from(v.0) / 1e7, lon: f64::from(v.1) / 1e7 }
}

/// Whether a report at `p` (declared accuracy `hacc`, received `now`) can
/// follow one at `q` (accuracy `q_hacc`, received `q_ms`).
pub(crate) fn plausible(q: LatLon, q_hacc: Option<f32>, q_ms: u64, p: LatLon, hacc: Option<f32>, now: u64) -> bool {
    let dt = now.abs_diff(q_ms);
    dist_m(q, p) <= jump_limit_m(dt, worst_hacc(q_hacc, hacc))
}

/// How a position report relates to a track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The track's own (or nothing to judge it against): apply it.
    Apply,
    /// Not on its established path: a rival claim.
    Rival,
}

/// The established fix: position, declared accuracy, hub ms.
pub(crate) fn established(t: &Track, now: u64) -> Option<(LatLon, Option<f32>, u64)> {
    if !t.confirmed() {
        return None;
    }
    let l = t.location.as_ref()?;
    let p = l.value.position?;
    (now.saturating_sub(l.rx_ms) <= ESTABLISHED_MAX_AGE_MS).then_some((p, l.value.horizontal_accuracy.meters(), l.rx_ms))
}

/// Judge a position report against `t`'s established path.
pub(crate) fn judge(t: &Track, p: Option<LatLon>, hacc: Option<f32>, now: u64) -> Verdict {
    match (p, established(t, now)) {
        (Some(p), Some((q, qh, qms))) if !plausible(q, qh, qms, p, hacc, now) => Verdict::Rival,
        _ => Verdict::Apply,
    }
}

/// What to do with a rival report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Held as the rival claim; nothing applied.
    Held,
    /// The original has gone quiet and the rival has its own path: apply
    /// this report (the track follows it).
    Follow,
}

impl Claims {
    fn now32(now: u64) -> u32 {
        now as u32
    }

    fn rival_alive(&self, now: u64) -> bool {
        self.rival_frames > 0 && Self::now32(now).wrapping_sub(self.rival_last) < RIVAL_TTL_MS
    }

    fn active(&self, now: u64) -> u8 {
        if Self::now32(now).wrapping_sub(self.last) < RIVAL_TTL_MS { self.bits & what::ALL } else { 0 }
    }

    /// Forget a rival and end a dispute nobody has fed for [`RIVAL_TTL_MS`].
    fn tidy(&mut self, now: u64) {
        if self.rival_frames > 0 && !self.rival_alive(now) {
            self.rival_frames = 0;
            self.switches = 0;
            self.bits &= !B_LAST_RIVAL;
        }
        if self.bits & (what::ALL | B_FOLLOWED) != 0 && self.active(now) == 0 {
            self.bits &= !(what::ALL | B_FOLLOWED);
            self.marks = 0;
        }
    }

    fn dispute_on(&mut self, bit: u8, now: u64) {
        let now32 = Self::now32(now);
        if self.active(now) == 0 {
            self.open_s = 0;
            self.last = now32;
        } else {
            // Whole seconds move from `last` into `open_s`, so the start
            // (`last - open_s`) stays exact and `last` lags under a second.
            let whole = now32.wrapping_sub(self.last) / 1000;
            self.open_s = self.open_s.saturating_add(whole.min(u32::from(u16::MAX)) as u16);
            self.last = self.last.wrapping_add(whole * 1000);
        }
        self.bits |= bit | B_EVER;
    }

    /// A report on the established path.
    pub(crate) fn original_report(&mut self, now: u64) {
        self.tidy(now);
        if self.bits & B_LAST_RIVAL != 0 {
            self.bits &= !B_LAST_RIVAL;
            if self.rival_alive(now) {
                self.switches = self.switches.saturating_add(1);
            }
        }
    }

    /// A report off the established path, at `p`. `orig_ms`: when the
    /// original last reported. Returns whether to follow it, and whether
    /// it started a new rival (a jump in this ID's reports).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn rival_report(
        &mut self,
        p: LatLon,
        hacc: Option<f32>,
        src: ClaimSrc,
        mac: [u8; 6],
        orig_ms: u64,
        now: u64,
    ) -> (Outcome, bool) {
        self.tidy(now);
        let now32 = Self::now32(now);
        self.rejected = self.rejected.saturating_add(1);
        let continues = self.rival_frames > 0
            && plausible(from_e7(self.rival_pos), None, full(now, self.rival_last), p, hacc, now);
        if continues {
            self.rival_frames = self.rival_frames.saturating_add(1);
        } else {
            self.rival_frames = 1;
            self.rival_first = now32;
            self.marks &= 0x0f;
        }
        self.rival_pos = e7(p);
        self.rival_last = now32;
        self.rival_addr = addr16(&mac);
        self.rival_src = src as u8;
        if self.bits & B_LAST_RIVAL == 0 {
            self.bits |= B_LAST_RIVAL;
            self.switches = self.switches.saturating_add(1);
        }
        let path = self.rival_frames >= RIVAL_MIN_FRAMES && now32.wrapping_sub(self.rival_first) >= RIVAL_MIN_SPAN_MS;
        if path && (self.switches >= DISPUTE_MIN_SWITCHES || self.active(now) & what::POSITION != 0) {
            self.dispute_on(what::POSITION | (self.marks >> 4), now);
        } else if self.active(now) != 0 {
            // A dispute already open (over a text field) is fed by it too.
            self.dispute_on(0, now);
        }
        let follow = path && now.saturating_sub(orig_ms) >= FOLLOW_AFTER_MS;
        (if follow { Outcome::Follow } else { Outcome::Held }, !continues)
    }

    /// The track followed its rival. Returns true when that was a jump by
    /// a single transmitter (no dispute open), which earns a
    /// `PositionJump` flag.
    pub(crate) fn followed(&mut self, now: u64) -> bool {
        let disputed = self.active(now) & what::POSITION != 0;
        if disputed {
            self.bits |= B_FOLLOWED;
        }
        self.rival_frames = 0;
        self.switches = 0;
        self.bits &= !B_LAST_RIVAL;
        !disputed
    }

    /// A frame with no position from the rival's own address while the
    /// rival is alive (and the address is not one the track uses).
    pub(crate) fn from_rival_address(&self, mac: &[u8; 6], now: u64) -> bool {
        self.rival_alive(now) && self.rival_addr != 0 && self.rival_addr == addr16(mac)
    }

    /// A rival frame's text field (or ID) differs from the established
    /// one: part of the dispute, once there is one (one forged frame is
    /// not a dispute).
    pub(crate) fn rival_text_differs(&mut self, kind: u8, now: u64) {
        let bit = kind_bit(kind);
        self.marks |= bit << 4;
        if self.active(now) & what::POSITION != 0 {
            self.dispute_on(bit, now);
        }
    }

    /// The track's ID came in a frame beside another ID of the same type.
    pub(crate) fn contradictory_frame(&mut self) {
        self.contradictory = self.contradictory.saturating_add(1);
    }

    /// An applied frame carries `new` (a hash) for field `kind`, whose
    /// established value hashes to `est` (`None`: nothing established).
    /// Returns true when `new` should be applied now.
    pub(crate) fn text_change(&mut self, kind: u8, est: Option<u32>, new: u32, now: u64) -> bool {
        self.tidy(now);
        let now32 = Self::now32(now);
        let g = &mut self.guard;
        let Some(est) = est else {
            return true;
        };
        if est == new {
            // The established value again: a pending change was undone.
            if g.kind == kind && g.n > 0 {
                g.n = 0;
                g.flips = g.flips.saturating_add(1);
                if g.flips >= TEXT_FLIPS_DISPUTE {
                    self.dispute_on(kind_bit(kind), now);
                }
            }
            return false;
        }
        // Once two values of some field alternate, any other field that
        // differs is part of the same dispute.
        let texts_disputed = self.active(now) & (what::SELF_ID | what::OPERATOR_ID | what::ID) != 0;
        let g = &mut self.guard;
        if g.n > 0 && g.kind == kind && g.hash == new {
            g.n = g.n.saturating_add(1);
            g.last = now32;
            if g.n >= ADOPT_MIN_FRAMES && now32.wrapping_sub(g.since) >= ADOPT_MS && !texts_disputed {
                *g = Guard { flips: 0, ..Guard::default() };
                return true;
            }
            return false;
        }
        let free = g.n == 0 || g.kind == kind || now32.wrapping_sub(g.last) >= 2 * ADOPT_MS;
        if free {
            let flips = if g.kind == kind { g.flips } else { 0 };
            *g = Guard { hash: new, since: now32, last: now32, n: 1, kind, flips };
        }
        if texts_disputed {
            self.dispute_on(kind_bit(kind), now);
        }
        false
    }

    /// Fold another track's claims (the same drone) into this one.
    pub(crate) fn absorb(&mut self, o: &Claims) {
        self.bits |= o.bits & B_EVER;
        self.rejected = self.rejected.saturating_add(o.rejected);
        self.contradictory = self.contradictory.saturating_add(o.contradictory);
    }

    /// The rival claim, while it is being heard.
    pub fn rival(&self, now: u64) -> Option<Rival> {
        self.rival_alive(now).then(|| Rival {
            position: from_e7(self.rival_pos),
            src: ClaimSrc::from_u8(self.rival_src),
            frames: self.rival_frames,
            first_ms: full(now, self.rival_first),
            last_ms: full(now, self.rival_last),
        })
    }

    /// The dispute, while it lasts.
    pub fn dispute(&self, now: u64) -> Option<Dispute> {
        let w = self.active(now);
        (w != 0).then(|| Dispute {
            what: w,
            since_ms: full(now, self.last).saturating_sub(u64::from(self.open_s) * 1000),
            rival: self.rival(now),
            followed: self.bits & B_FOLLOWED != 0,
        })
    }

    pub fn disputed(&self, now: u64) -> bool {
        self.active(now) != 0
    }

    /// Ever disputed, in the life of the track.
    pub fn ever_disputed(&self) -> bool {
        self.bits & B_EVER != 0
    }

    /// Reports not applied because they did not fit the track's path.
    pub fn rejected(&self) -> u8 {
        self.rejected
    }

    /// Frames that named this track's ID beside another ID of the same
    /// type (applied to nothing).
    pub fn contradictory(&self) -> u8 {
        self.contradictory
    }

    /// Anything to say about claims on this track.
    pub fn any(&self, now: u64) -> bool {
        self.disputed(now) || self.rival_alive(now) || self.rejected > 0 || self.contradictory > 0 || self.ever_disputed()
    }

    /// Every field at its widest, a rival heard at `now` far away: for
    /// the formatters' worst-case tests.
    #[cfg(test)]
    pub(crate) fn worst_for_tests(now: u64) -> Self {
        let now32 = now as u32;
        Claims {
            rival_pos: e7(LatLon { lat: -89.123_456_7, lon: -179.123_456_7 }),
            rival_first: now32.wrapping_sub(1_000_000),
            rival_last: now32.wrapping_sub(99_999),
            rival_addr: 1,
            rival_frames: 255,
            rival_src: ClaimSrc::DroneIdO4 as u8,
            switches: 255,
            bits: what::ALL | B_FOLLOWED | B_EVER,
            marks: 0,
            rejected: 255,
            contradictory: 255,
            last: now32.wrapping_sub(14_999),
            open_s: u16::MAX,
            guard: Guard::default(),
        }
    }

    /// New dispute aspects to hand out: marks them handed out.
    pub(crate) fn take_new(&mut self, now: u64) -> Option<u8> {
        let w = self.active(now);
        let new = w & !(self.marks & 0x0f);
        (new != 0).then(|| {
            self.marks |= new;
            w
        })
    }
}

/// A transmitter address folded to 16 bits; 0 for none.
fn addr16(mac: &[u8; 6]) -> u16 {
    if *mac == [0; 6] {
        return 0;
    }
    (crate::probation::hash(&[mac]) as u16).max(1)
}

fn kind_bit(kind: u8) -> u8 {
    match kind {
        KIND_SELF_ID => what::SELF_ID,
        KIND_OPERATOR_ID => what::OPERATOR_ID,
        _ => what::ID,
    }
}

/// FNV-1a of a field's bytes, for [`Claims::text_change`].
pub(crate) fn hash_bytes(b: &[u8]) -> u32 {
    crate::probation::hash(&[b])
}

/// Two tracks a frame said were one drone, waiting for more evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PendingMerge {
    pub a: TrackId,
    pub b: TrackId,
    first: u32,
    last: u32,
    n: u8,
}

/// Note one more frame saying `a` and `b` are one drone. Returns true when
/// the evidence is enough (positions are checked by the caller).
pub(crate) fn merge_evidence(
    v: &mut heapless::Vec<PendingMerge, MAX_PENDING_MERGES>,
    a: TrackId,
    b: TrackId,
    now: u64,
) -> bool {
    let now32 = now as u32;
    let i = match v.iter().position(|m| (m.a == a && m.b == b) || (m.a == b && m.b == a)) {
        Some(i) => i,
        None => {
            let e = PendingMerge { a, b, first: now32, last: now32, n: 1 };
            if v.push(e).is_err() {
                // Full: give up the one fed longest ago.
                if let Some(old) = v.iter_mut().max_by_key(|m| now32.wrapping_sub(m.last)) {
                    *old = e;
                }
            }
            return false;
        }
    };
    let m = &mut v[i];
    if now32.wrapping_sub(m.last) >= MERGE_GAP_MS {
        m.n = m.n.saturating_add(1);
        m.last = now32;
    }
    m.n >= MERGE_MIN_FRAMES && m.last.wrapping_sub(m.first) >= MERGE_SPAN_MS
}

/// Two tracks' positions could be one drone's: both within 60 m plus twice
/// the declared accuracy plus 30 m/s for the time between them (or either
/// has no position).
pub(crate) fn same_place(a: &Track, b: &Track) -> bool {
    let fix = |t: &Track| t.location.as_ref().and_then(|l| l.value.position.map(|p| (p, l.value.horizontal_accuracy.meters(), l.rx_ms)));
    match (fix(a), fix(b)) {
        (Some((p, ph, pt)), Some((q, qh, qt))) => {
            let dt = pt.abs_diff(qt) as f64 / 1000.0;
            dist_m(p, q) <= 60.0 + 2.0 * worst_hacc(ph, qh) + 30.0 * (dt + 1.0)
        }
        _ => true,
    }
}

/// One dispute for the hub's evidence log ([`crate::Tracker::take_dispute`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisputeEvent {
    pub track: TrackId,
    pub dispute: Dispute,
    /// Metres from the shown position to the rival's, when both are known.
    pub apart_m: Option<f64>,
}

impl DisputeEvent {
    /// Evidence `IdDisputed` data: track id (u32 LE), what bits, rival source.
    pub fn data(&self) -> [u8; 6] {
        let id = self.track.0.to_le_bytes();
        let src = self.dispute.rival.map_or(0xff, |r| r.src as u8);
        [id[0], id[1], id[2], id[3], self.dispute.what, src]
    }
}

/// Evidence text: `track 7 SerialNumber 1ZZZ9FAKE00001: two transmitters
/// claim this ID; another Remote ID claim 2140 m from the shown one; Self-ID
/// differs. Neither is verified.`
pub fn event_text(ev: &DisputeEvent, track: Option<&Track>) -> heapless::String<192> {
    crate::text::render(EventText(ev, track))
}

struct EventText<'a>(&'a DisputeEvent, Option<&'a Track>);

impl fmt::Display for EventText<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ev = self.0;
        write!(f, "track {}", ev.track.0)?;
        if let Some(id) = self.1.and_then(Track::primary_id) {
            write!(f, " {:?} {}", id.id_type, IdText(&id.id))?;
        }
        f.write_str(": two transmitters claim this ID")?;
        write_what(f, &ev.dispute, ev.apart_m)?;
        f.write_str(". Neither is verified.")
    }
}

fn write_what(f: &mut fmt::Formatter<'_>, d: &Dispute, apart_m: Option<f64>) -> fmt::Result {
    if d.what & what::POSITION != 0 {
        match (d.rival, apart_m) {
            (Some(r), Some(m)) => write!(f, "; another {} claim {:.0} m from the shown one", r.src.label(), m)?,
            _ => f.write_str("; two positions")?,
        }
    }
    if d.what & what::SELF_ID != 0 {
        f.write_str("; Self-ID differs")?;
    }
    if d.what & what::OPERATOR_ID != 0 {
        f.write_str("; operator ID differs")?;
    }
    if d.what & what::ID != 0 {
        f.write_str("; a second ID")?;
    }
    if d.followed {
        f.write_str("; now showing the claim that continued")?;
    }
    Ok(())
}

/// One-line text for a track's dispute (screens, CoT remarks): `DISPUTED:
/// two transmitters claim this ID; another Remote ID claim 2140 m from the
/// shown one`. Empty when there is none.
pub struct Summary<'a>(pub &'a Track, pub u64);

impl fmt::Display for Summary<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Some(d) = self.0.claims.dispute(self.1) else { return Ok(()) };
        f.write_str("DISPUTED: two transmitters claim this ID")?;
        write_what(f, &d, apart_m(self.0, self.1))
    }
}

/// Metres between the shown position and the rival claim, while one is
/// heard.
pub fn apart_m(t: &Track, now: u64) -> Option<f64> {
    let r = t.claims.rival(now)?;
    let p = t.location.as_ref()?.value.position?;
    Some(dist_m(p, r.position))
}

