//! DJI O4 DroneID decryption with an owner-supplied key (PLAN.md Phase 10;
//! owner decision 2026-11-16).
//!
//! O4 aircraft send two kinds of encrypted DroneID record (SPEC.md facts,
//! see `docs/DRONEID-O4.md`):
//!
//! - **CRYP** (selector 0x13): a fresh 16-byte AES session key `K`, wrapped
//!   with SM2 to the receiver's public key, plus 32 bytes of encrypted
//!   auxiliary data (serial and aircraft position).
//! - **INFP** (selectors 0x10 and 0x11): telemetry or a purpose text,
//!   AES-128-CTR under `K`.
//!
//! Both carry `key_id = SHA-256(K)[0..4]` in the clear, which links a
//! session's records together.
//!
//! This crate unwraps `K` from a CRYP record with the receiver's SM2 private
//! scalar `d` ([`unwrap_cryp`]), keeps recent session keys by `key_id`
//! ([`Decryptor`]), and decrypts records into the plaintext types of
//! [`droneid::o4plain`]. **It ships no key.** `d` is read at run time from a
//! file the owner provides (on the hub: `ROTORSCP/O4KEY.TXT` on the SD
//! card). Without one, nothing here runs and O4 stays detect-only.
//!
//! Key hygiene: [`PrivateKey`] and [`SessionKey`] have no `Debug` or
//! `Display`, are wiped on drop, and no function here returns or formats
//! their bytes. Nothing in this crate prints anything.
//!
//! `no_std`, no heap. Unwrapping one CRYP record is one P-256-sized scalar
//! multiplication (a few ms on the P4); INFP records cost one AES-CTR pass.
//!
//! # Decrypted is not authenticated
//!
//! Nothing in the scheme proves who sent a record (docs/SPOOFING.md
//! "Impersonation and disputes"): anyone holding the receiver's public key
//! can mint a session, a recorded exchange can be replayed, and AES-CTR is
//! malleable (flip ciphertext bits, the same plaintext bits flip, and the
//! range checks still pass). What this crate can do, it does: within a
//! session an INFP-10 record whose sequence number or GPS time does not
//! move forward is refused ([`Error::Replay`]), and one that jumps further
//! ahead than the time since the last one allows is refused without moving
//! the session on ([`Error::TooFarAhead`]), so a forged "future" record
//! cannot lock the real one out. A CRYP whose session is already known
//! costs no curve operation, and unwraps are rate-limited
//! ([`UNWRAP_PER_S`]). The tracker treats every decrypted position as a
//! claim, not a fact.
#![no_std]

use aes::cipher::{KeyIvInit, StreamCipher};
use droneid::o4plain::{self, CrypAux, Infp10, Infp11};
use sha2::Sha256;
use sm2::elliptic_curve::sec1::{FromEncodedPoint, ToEncodedPoint};
use sm2::{AffinePoint, EncodedPoint, ProjectivePoint, SecretKey};
use sm3::{Digest, Sm3};
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, ZeroizeOnDrop};

type Aes128Ctr = ctr::Ctr128BE<aes::Aes128>;

/// Core lengths and selectors (SPEC.md "CRYP core record", "INFP core
/// records").
pub const CRYP_LEN: usize = 163;
pub const INFP10_LEN: usize = 128;
pub const INFP11_LEN: usize = 160;
const SEL_CRYP: u8 = 0x13;
const SEL_INFP10: u8 = 0x10;
const SEL_INFP11: u8 = 0x11;

/// The receiver's SM2 private scalar `d`. Wiped on drop; never printable.
pub struct PrivateKey(SecretKey);

impl PrivateKey {
    /// From 32 big-endian bytes. `None` for zero or `d >= n`.
    pub fn from_bytes(b: &[u8; 32]) -> Option<Self> {
        SecretKey::from_slice(b).ok().map(Self)
    }

    /// From a key file's text: 64 hex digits, optionally with whitespace,
    /// a `0x` prefix, and `#` comment lines. The text buffer is the
    /// caller's to wipe.
    pub fn from_hex_text(text: &[u8]) -> Result<Self, KeyError> {
        let mut raw = [0u8; 32];
        let mut n = 0usize;
        let mut hi: Option<u8> = None;
        let mut in_comment = false;
        let mut prev = 0u8;
        let mut result = Ok(());
        for &c in text {
            if in_comment {
                in_comment = c != b'\n';
                prev = c;
                continue;
            }
            match c {
                b'#' => in_comment = true,
                b' ' | b'\t' | b'\r' | b'\n' | b':' => {}
                b'x' | b'X' if prev == b'0' && n == 0 && hi == Some(0) => hi = None,
                _ => {
                    let Some(v) = hex_val(c) else {
                        result = Err(KeyError::NotHex);
                        break;
                    };
                    match hi.take() {
                        None => hi = Some(v),
                        Some(h) => {
                            if n == 32 {
                                result = Err(KeyError::Length);
                                break;
                            }
                            raw[n] = h << 4 | v;
                            n += 1;
                        }
                    }
                }
            }
            prev = c;
        }
        if result.is_ok() && (n != 32 || hi.is_some()) {
            result = Err(KeyError::Length);
        }
        let key = result.and_then(|()| Self::from_bytes(&raw).ok_or(KeyError::OutOfRange));
        raw.zeroize();
        key
    }

    /// A short fingerprint of the matching public key (first 4 bytes of
    /// SHA-256 over its x||y), to show which key is installed without
    /// revealing it.
    pub fn fingerprint(&self) -> [u8; 4] {
        let p = self.0.public_key().as_affine().to_encoded_point(false);
        let mut h = Sha256::new();
        h.update(p.x().expect("not identity"));
        h.update(p.y().expect("not identity"));
        let d = h.finalize();
        [d[0], d[1], d[2], d[3]]
    }

    #[cfg(test)]
    fn public(&self) -> AffinePoint {
        *self.0.public_key().as_affine()
    }
}

impl ZeroizeOnDrop for PrivateKey {}

/// Why a key file was refused. Carries no key bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyError {
    NotHex,
    /// Not exactly 32 bytes (64 hex digits).
    Length,
    /// Zero, or not below the SM2 group order.
    OutOfRange,
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// A 16-byte AES session key. Wiped on drop; never printable.
#[derive(Clone)]
pub struct SessionKey([u8; 16]);

impl SessionKey {
    /// A key supplied by hand (bench tests). Its key ID is computed, not
    /// trusted from anywhere.
    pub fn from_bytes(k: [u8; 16]) -> Self {
        Self(k)
    }
    /// `SHA-256(K)[0..4]`, the cleartext session identifier.
    pub fn key_id(&self) -> [u8; 4] {
        let d = Sha256::digest(self.0);
        [d[0], d[1], d[2], d[3]]
    }
    fn ctr(&self, iv: &[u8], buf: &mut [u8]) {
        let mut counter = [0u8; 16];
        counter[..8].copy_from_slice(iv);
        let mut c = Aes128Ctr::new(&self.0.into(), &counter.into());
        c.apply_keystream(buf);
    }
}

impl Drop for SessionKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Why a record did not decrypt. Carries no key or plaintext bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Not a recognised CRYP / INFP core (selector, magic, length).
    Shape,
    /// `C1` is not a point on the SM2 curve.
    BadPoint,
    /// `d * C1` is the point at infinity.
    Infinity,
    /// The SM3 check value `C3` does not match: wrong key, or a damaged or
    /// forged record.
    C3Mismatch,
    /// The unwrapped key does not hash to the record's key ID.
    KeyIdMismatch,
    /// An INFP record whose session key is not known (its CRYP not yet
    /// heard, or no private key installed).
    NoSession,
    /// No private key installed.
    NoKey,
    /// Decrypted, but the plaintext failed the spec's structural checks
    /// (wrong key for a colliding key ID, or an unknown layout).
    Plain(o4plain::Reject),
    /// An INFP-10 record whose sequence number or GPS time does not move
    /// forward within its session: a replay, or an altered copy of one.
    Replay,
    /// An INFP-10 record further ahead (sequence or GPS time) than the time
    /// since the session's last one allows. Not applied, and the session's
    /// position is not moved on, so the real records still decrypt.
    TooFarAhead,
    /// A new CRYP while the unwrap budget ([`UNWRAP_PER_S`]) is spent.
    RateLimited,
}

/// The core record inside a logical object (`[len][core][7 pad][crc16]`,
/// as [`link::DroneIdRecord::O4`] carries it). Length checks only; the
/// node already checked the CRCs and padding.
pub fn core_of(object: &[u8]) -> Result<&[u8], Error> {
    let sel = *object.get(1).ok_or(Error::Shape)?;
    let n = match sel {
        SEL_CRYP => CRYP_LEN,
        SEL_INFP10 => INFP10_LEN,
        SEL_INFP11 => INFP11_LEN,
        _ => return Err(Error::Shape),
    };
    object.get(1..1 + n).ok_or(Error::Shape)
}

fn check_shape(core: &[u8]) -> Result<(), Error> {
    let (len, magic, len_at, ct) = match core.first() {
        Some(&SEL_CRYP) => (CRYP_LEN, b"CRYP", 129, 32u16),
        Some(&SEL_INFP10) => (INFP10_LEN, b"INFP", 17, Infp10::LEN as u16),
        Some(&SEL_INFP11) => (INFP11_LEN, b"INFP", 17, Infp11::LEN as u16),
        _ => return Err(Error::Shape),
    };
    if core.len() != len || &core[1..5] != magic || core[len_at..len_at + 2] != ct.to_le_bytes() {
        return Err(Error::Shape);
    }
    Ok(())
}

/// The cleartext key ID of a CRYP or INFP core.
pub fn key_id_of(core: &[u8]) -> Result<[u8; 4], Error> {
    check_shape(core)?;
    Ok([core[5], core[6], core[7], core[8]])
}

/// Unwrap the session key from a CRYP core with the private scalar `d`
/// (SPEC.md "SM2 session-key encryption", receiver side): validate `C1`,
/// `S = d*C1`, `t = SM3(x2||y2||00000001)[0..16]`, `K = C2 xor t`, then
/// require `SM3(x2||K||y2) == C3` and `SHA-256(K)[0..4] == key_id`.
pub fn unwrap_cryp(core: &[u8], d: &PrivateKey) -> Result<SessionKey, Error> {
    check_shape(core)?;
    if core[0] != SEL_CRYP {
        return Err(Error::Shape);
    }
    let (x1, y1) = (&core[9..41], &core[41..73]);
    let (c3, c2) = (&core[73..105], &core[105..121]);
    let ep = EncodedPoint::from_affine_coordinates(x1.into(), y1.into(), false);
    // Decoding checks both coordinates are below p and on the curve.
    let c1: Option<AffinePoint> = AffinePoint::from_encoded_point(&ep).into();
    let c1 = c1.ok_or(Error::BadPoint)?;
    let s = (ProjectivePoint::from(c1) * *d.0.to_nonzero_scalar()).to_affine();
    let enc = s.to_encoded_point(false);
    let (Some(x2), Some(y2)) = (enc.x(), enc.y()) else {
        return Err(Error::Infinity);
    };
    let mut t = Sm3::new().chain_update(x2).chain_update(y2).chain_update([0, 0, 0, 1]).finalize();
    let mut k = [0u8; 16];
    for i in 0..16 {
        k[i] = c2[i] ^ t[i];
    }
    t.zeroize();
    let check = Sm3::new().chain_update(x2).chain_update(k).chain_update(y2).finalize();
    let key = SessionKey(k);
    k.zeroize();
    if !bool::from(check[..].ct_eq(c3)) {
        return Err(Error::C3Mismatch);
    }
    if !bool::from(key.key_id().ct_eq(&[core[5], core[6], core[7], core[8]])) {
        return Err(Error::KeyIdMismatch);
    }
    Ok(key)
}

/// What a record decrypted to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plain {
    /// The CRYP auxiliary field: serial and aircraft position.
    Cryp(CrypAux),
    /// Flight telemetry.
    Infp10(Infp10),
    /// Serial and flight-purpose text.
    Infp11(Infp11),
}

/// Decrypt a CRYP / INFP core with a known session key and parse the
/// plaintext. Does not check that `key` belongs to the record's key ID;
/// [`Decryptor`] does.
pub fn decrypt_core(core: &[u8], key: &SessionKey) -> Result<Plain, Error> {
    check_shape(core)?;
    match core[0] {
        SEL_CRYP => {
            let mut b = [0u8; CrypAux::LEN];
            b.copy_from_slice(&core[131..163]);
            key.ctr(&core[121..129], &mut b);
            let r = o4plain::parse_cryp_aux(&b).map(Plain::Cryp).map_err(Error::Plain);
            b.zeroize();
            r
        }
        SEL_INFP10 => {
            let mut b = [0u8; Infp10::LEN];
            b.copy_from_slice(&core[19..INFP10_LEN]);
            key.ctr(&core[9..17], &mut b);
            let r = o4plain::parse_infp10(&b).map(Plain::Infp10).map_err(Error::Plain);
            b.zeroize();
            r
        }
        _ => {
            let mut b = [0u8; Infp11::LEN];
            b.copy_from_slice(&core[19..INFP11_LEN]);
            key.ctr(&core[9..17], &mut b);
            let r = o4plain::parse_infp11(&b).map(Plain::Infp11).map_err(Error::Plain);
            b.zeroize();
            r
        }
    }
}

/// A session not used for this long is forgotten. Sessions change on
/// every aircraft restart; a flight's CRYP repeats while it flies.
pub const SESSION_TTL_MS: u64 = 30 * 60 * 1000;
/// New session keys unwrapped (curve operations) a second, on average...
pub const UNWRAP_PER_S: u32 = 2;
/// ...and in a burst. A real aircraft needs one per flight.
pub const UNWRAP_BURST: u32 = 4;
/// INFP-10 GPS time may run ahead of hub time by this much between two
/// records of a session (clock rates, reception delays).
pub const GPS_AHEAD_SLACK_MS: u64 = 10_000;
/// INFP-10 sequence numbers a second, at most, between two records
/// (generous: records come a few times a second), plus a burst.
pub const SEQ_PER_S: u64 = 50;

/// The latest INFP-10 accepted in a session.
#[derive(Clone, Copy)]
struct Latest {
    seq: u16,
    gps_ms: u64,
    hub_ms: u64,
    /// FNV-1a of the fields a duplicate repeats (position, altitudes,
    /// velocity, GPS time).
    hash: u32,
}

struct Slot {
    key_id: [u8; 4],
    key: SessionKey,
    last_ms: u64,
    latest: Option<Latest>,
}

fn infp10_hash(r: &Infp10) -> u32 {
    let mut h = 0x811c_9dc5u32;
    let mut eat = |b: &[u8]| {
        for x in b {
            h = (h ^ u32::from(*x)).wrapping_mul(0x0100_0193);
        }
    };
    eat(&r.lat_raw.to_le_bytes());
    eat(&r.lon_raw.to_le_bytes());
    eat(&r.altitude_m.to_le_bytes());
    eat(&r.height_dm.to_le_bytes());
    eat(&r.v_north.to_le_bytes());
    eat(&r.v_east.to_le_bytes());
    eat(&r.v_up.to_le_bytes());
    eat(&r.state.to_le_bytes());
    eat(&r.gps_time_ms.to_le_bytes());
    h
}

impl Slot {
    /// Whether INFP-10 `r`, received at `now`, moves the session forward.
    fn fresh(&mut self, r: &Infp10, now: u64) -> Result<(), Error> {
        let h = infp10_hash(r);
        let Some(l) = self.latest else {
            self.latest = Some(Latest { seq: r.seq, gps_ms: r.gps_time_ms, hub_ms: now, hash: h });
            return Ok(());
        };
        let dseq = r.seq.wrapping_sub(l.seq) as i16;
        if dseq == 0 {
            // The same record again (two nodes, or a repeat): fine if it is
            // identical; an altered copy is refused.
            return if h == l.hash { Ok(()) } else { Err(Error::Replay) };
        }
        if dseq < 0 || r.gps_time_ms < l.gps_ms {
            return Err(Error::Replay);
        }
        let dt = now.saturating_sub(l.hub_ms);
        if u64::from(dseq as u16) > SEQ_PER_S * (dt / 1000 + 1)
            || r.gps_time_ms - l.gps_ms > dt + GPS_AHEAD_SLACK_MS
        {
            return Err(Error::TooFarAhead);
        }
        self.latest = Some(Latest { seq: r.seq, gps_ms: r.gps_time_ms, hub_ms: now, hash: h });
        Ok(())
    }
}

/// Unwrap budget: milli-tokens and when they were counted.
#[derive(Clone, Copy, Debug)]
struct Bucket {
    milli: u32,
    at_ms: u64,
}

impl Bucket {
    const fn full() -> Self {
        Self { milli: UNWRAP_BURST * 1000, at_ms: 0 }
    }
    fn take(&mut self, now: u64) -> bool {
        let add = now.saturating_sub(self.at_ms).saturating_mul(u64::from(UNWRAP_PER_S)).min(u64::from(u32::MAX));
        self.milli = self.milli.saturating_add(add as u32).min(UNWRAP_BURST * 1000);
        self.at_ms = self.at_ms.max(now);
        if self.milli >= 1000 {
            self.milli -= 1000;
            true
        } else {
            false
        }
    }
}

/// The hub's O4 decryptor: the owner's private key (if installed) and the
/// `N` most recent session keys.
pub struct Decryptor<const N: usize> {
    key: Option<PrivateKey>,
    slots: [Option<Slot>; N],
    bucket: Bucket,
    pub stats: Stats,
}

/// Counters for the hub's status line. No key material.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub sessions_unwrapped: u32,
    pub records_decrypted: u32,
    pub no_session: u32,
    pub rejected: u32,
    /// Scalar multiplications done (one per new session unwrapped, or
    /// attempted: a forged CRYP costs one too).
    pub curve_ops: u32,
    /// INFP-10 records refused as not newer than their session's last
    /// ([`Error::Replay`]).
    pub replayed: u32,
    /// INFP-10 records refused as too far ahead ([`Error::TooFarAhead`]).
    pub too_far_ahead: u32,
    /// New CRYPs refused while the unwrap budget was spent.
    pub rate_limited: u32,
}

impl<const N: usize> Default for Decryptor<N> {
    fn default() -> Self {
        Self { key: None, slots: [const { None }; N], bucket: Bucket::full(), stats: Stats::default() }
    }
}

impl<const N: usize> Decryptor<N> {
    pub fn new(key: Option<PrivateKey>) -> Self {
        Self { key, ..Self::default() }
    }

    pub fn has_key(&self) -> bool {
        self.key.is_some()
    }

    /// Anything to decrypt with: a private key or a hand-supplied session.
    pub fn has_key_or_sessions(&self) -> bool {
        self.key.is_some() || self.slots.iter().any(Option::is_some)
    }

    /// Install (or replace) the private key.
    pub fn set_key(&mut self, key: PrivateKey) {
        self.key = Some(key);
    }

    /// The installed key's public fingerprint ([`PrivateKey::fingerprint`]).
    pub fn key_fingerprint(&self) -> Option<[u8; 4]> {
        self.key.as_ref().map(PrivateKey::fingerprint)
    }

    /// Add a session key supplied by hand, filed under its computed key ID.
    pub fn add_session(&mut self, key: SessionKey, now_ms: u64) {
        let id = key.key_id();
        self.store(id, key, now_ms);
    }

    /// Known session key IDs (not keys), for status output.
    pub fn sessions(&self) -> impl Iterator<Item = [u8; 4]> + '_ {
        self.slots.iter().flatten().map(|s| s.key_id)
    }

    fn store(&mut self, key_id: [u8; 4], key: SessionKey, now_ms: u64) {
        let i = self
            .slots
            .iter()
            .position(|s| s.as_ref().is_some_and(|s| s.key_id == key_id))
            .or_else(|| self.slots.iter().position(Option::is_none))
            .unwrap_or_else(|| {
                (0..N).min_by_key(|&i| self.slots[i].as_ref().map_or(0, |s| s.last_ms)).unwrap_or(0)
            });
        if N > 0 {
            self.slots[i] = Some(Slot { key_id, key, last_ms: now_ms, latest: None });
        }
    }

    fn expire(&mut self, now_ms: u64) {
        for s in self.slots.iter_mut() {
            if s.as_ref().is_some_and(|x| now_ms.saturating_sub(x.last_ms) > SESSION_TTL_MS) {
                *s = None;
            }
        }
    }

    /// Decrypt one logical object. A CRYP record (with a key installed)
    /// first unwraps and files its session key; any record then decrypts
    /// with its session's key.
    pub fn process(&mut self, object: &[u8], now_ms: u64) -> Result<Plain, Error> {
        let r = self.process_inner(object, now_ms);
        let s = &mut self.stats;
        let c = match r {
            Ok(_) => &mut s.records_decrypted,
            Err(Error::NoSession) | Err(Error::NoKey) => &mut s.no_session,
            Err(Error::Replay) => &mut s.replayed,
            Err(Error::TooFarAhead) => &mut s.too_far_ahead,
            Err(Error::RateLimited) => &mut s.rate_limited,
            Err(_) => &mut s.rejected,
        };
        *c = c.saturating_add(1);
        r
    }

    fn process_inner(&mut self, object: &[u8], now_ms: u64) -> Result<Plain, Error> {
        self.expire(now_ms);
        let core = core_of(object)?;
        let id = key_id_of(core)?;
        if core[0] == SEL_CRYP {
            // A session already unwrapped: its key decrypts the aux, with no
            // curve operation. (A CRYP whose C1/C2/C3 were altered is not
            // noticed then; nothing is lost, since its aux was never
            // authenticated either.)
            if let Some(slot) = self.slots.iter_mut().flatten().find(|s| s.key_id == id) {
                let plain = decrypt_core(core, &slot.key)?;
                slot.last_ms = now_ms;
                return Ok(plain);
            }
            let d = self.key.as_ref().ok_or(Error::NoKey)?;
            if !self.bucket.take(now_ms) {
                return Err(Error::RateLimited);
            }
            self.stats.curve_ops = self.stats.curve_ops.saturating_add(1);
            let k = unwrap_cryp(core, d)?;
            let plain = decrypt_core(core, &k)?;
            self.stats.sessions_unwrapped += 1;
            self.store(id, k, now_ms);
            return Ok(plain);
        }
        let slot = self
            .slots
            .iter_mut()
            .flatten()
            .find(|s| s.key_id == id)
            .ok_or(Error::NoSession)?;
        let plain = decrypt_core(core, &slot.key)?;
        if let Plain::Infp10(r) = &plain {
            slot.fresh(r, now_ms)?;
        }
        slot.last_ms = now_ms;
        Ok(plain)
    }
}

#[cfg(test)]
pub(crate) mod tests;
#[cfg(test)]
mod fuzz_tests;
