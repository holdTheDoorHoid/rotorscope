//! OpenTimestamps proofs, written here (no crate): parse, build, replay and
//! check against a Bitcoin block header the person checking supplies.
//!
//! Format, as published by the OpenTimestamps project's own reference code
//! (python-opentimestamps, `opentimestamps/core/op.py`, `timestamp.py`,
//! `notary.py`, `serialize.py`; docs/EVIDENCE.md, "OpenTimestamps"):
//!
//! ```text
//! file      = MAGIC (31 bytes) | version 0x01 | hash op 0x08 (SHA-256) | 32-byte digest | timestamp
//! timestamp = ( 0xff item )* item          every item but the last is marked 0xff
//! item      = 0x00 attestation             attestations come first, then ops
//!           | op timestamp                 the op applied to this node's message
//! op        = 0xf0 varbytes (append) | 0xf1 varbytes (prepend) | 0xf2 (reverse) | 0xf3 (hexlify)
//!           | 0x08 (sha256) | 0x03 (ripemd160) | 0x02 (sha1) | 0x67 (keccak256)
//! attestation = 8-byte tag | varbytes payload
//!   pending: 83 df e3 0d 2e f9 0c 8e, payload = varbytes URI (A-Z a-z 0-9 - . _ / :, at most 1000)
//!   bitcoin: 05 88 96 0d 73 d7 19 01, payload = varuint block height
//! varuint   = LEB128, at most 9 bytes; varbytes = varuint length | bytes
//! ```
//!
//! A message is at most 4096 bytes, before and after every op, and an op
//! argument 1 to 4096 bytes (as the reference code). A Bitcoin attestation
//! says: the message reached here is the Merkle root of the block at this
//! height. Checking it needs that block's 80-byte header, which this code
//! never fetches: the person checking supplies it.
//!
//! **Hostile input.** A proof comes from a calendar (a third party) or a
//! bundle (anyone). Every read is bounds-checked; sizes, depth (256, as the
//! reference code) and the number of items are capped; recursion is bounded
//! by the depth cap; no input panics. SHA-1 and Keccak-256 are recognised
//! but not computed: a branch that uses them is reported, not followed.

use std::fmt;

/// `\0OpenTimestamps\0\0Proof\0` and 8 fixed bytes.
pub const MAGIC: &[u8; 31] = b"\x00OpenTimestamps\x00\x00Proof\x00\xbf\x89\xe2\xe8\x84\xe8\x92\x94";
pub const VERSION: u8 = 1;
pub const PENDING_TAG: [u8; 8] = [0x83, 0xdf, 0xe3, 0x0d, 0x2e, 0xf9, 0x0c, 0x8e];
pub const BITCOIN_TAG: [u8; 8] = [0x05, 0x88, 0x96, 0x0d, 0x73, 0xd7, 0x19, 0x01];
pub const LITECOIN_TAG: [u8; 8] = [0x06, 0x86, 0x9a, 0x0d, 0x73, 0xd7, 0x1b, 0x45];

/// Longest message before or after an op, and longest op argument.
pub const MAX_MSG: usize = 4096;
/// Deepest chain of ops (the reference code's recursion limit).
pub const MAX_DEPTH: usize = 256;
/// Most items (ops plus attestations) in one proof.
pub const MAX_ITEMS: usize = 4096;
/// Largest attestation payload, and longest pending URI.
pub const MAX_PAYLOAD: usize = 8192;
pub const MAX_URI: usize = 1000;
/// Largest calendar reply read (as the reference client).
pub const MAX_REPLY: usize = 10_000;
/// Largest proof file kept or checked.
pub const MAX_PROOF: usize = 64 << 10;
const URI_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._/:";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Op {
    Sha1,
    Ripemd160,
    Sha256,
    Keccak256,
    Append(Vec<u8>),
    Prepend(Vec<u8>),
    Reverse,
    Hexlify,
}

impl Op {
    pub fn tag(&self) -> u8 {
        match self {
            Op::Sha1 => 0x02,
            Op::Ripemd160 => 0x03,
            Op::Sha256 => 0x08,
            Op::Keccak256 => 0x67,
            Op::Append(_) => 0xf0,
            Op::Prepend(_) => 0xf1,
            Op::Reverse => 0xf2,
            Op::Hexlify => 0xf3,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Op::Sha1 => "sha1",
            Op::Ripemd160 => "ripemd160",
            Op::Sha256 => "sha256",
            Op::Keccak256 => "keccak256",
            Op::Append(_) => "append",
            Op::Prepend(_) => "prepend",
            Op::Reverse => "reverse",
            Op::Hexlify => "hexlify",
        }
    }

    /// The op applied to `msg`, within the length limits. SHA-1 and
    /// Keccak-256 are not computed here (`Err(Unsupported)`).
    pub fn apply(&self, msg: &[u8]) -> Result<Vec<u8>, OtsError> {
        if msg.len() > MAX_MSG {
            return Err(OtsError::TooLong("message"));
        }
        let out = match self {
            Op::Append(a) => [msg, a].concat(),
            Op::Prepend(p) => [p.as_slice(), msg].concat(),
            Op::Sha256 => crate::sha256(msg).to_vec(),
            Op::Ripemd160 => ripemd160(msg).to_vec(),
            Op::Reverse | Op::Hexlify if msg.is_empty() => return Err(OtsError::Malformed("reverse or hexlify of an empty message")),
            Op::Reverse => msg.iter().rev().copied().collect(),
            Op::Hexlify => {
                if msg.len() > MAX_MSG / 2 {
                    return Err(OtsError::TooLong("message"));
                }
                crate::verify::hex(msg).into_bytes()
            }
            Op::Sha1 | Op::Keccak256 => return Err(OtsError::Unsupported(self.name())),
        };
        if out.len() > MAX_MSG {
            return Err(OtsError::TooLong("result"));
        }
        Ok(out)
    }
}

impl fmt::Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Op::Append(a) | Op::Prepend(a) => write!(f, "{} {}", self.name(), crate::verify::hex(a)),
            _ => f.write_str(self.name()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Attestation {
    /// A calendar's promise to anchor this message later.
    Pending { uri: String },
    /// This message is the Merkle root of the Bitcoin block at `height`.
    Bitcoin { height: u64 },
    /// Any other (Litecoin, Ethereum, unknown): kept, not checked.
    Other { tag: [u8; 8], payload: Vec<u8> },
}

impl Attestation {
    fn tag_payload(&self) -> ([u8; 8], Vec<u8>) {
        match self {
            Attestation::Pending { uri } => (PENDING_TAG, varbytes(uri.as_bytes())),
            Attestation::Bitcoin { height } => (BITCOIN_TAG, varuint(*height)),
            Attestation::Other { tag, payload } => (*tag, payload.clone()),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Attestation::Pending { uri } => format!("pending at {uri}"),
            Attestation::Bitcoin { height } => format!("Bitcoin block {height}"),
            Attestation::Other { tag, .. } if *tag == LITECOIN_TAG => "Litecoin block (not checked)".into(),
            Attestation::Other { tag, .. } => format!("unknown attestation {} (not checked)", crate::verify::hex(tag)),
        }
    }
}

/// One node: what is attested to this node's message, and the ops that
/// lead on from it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Timestamp {
    pub attestations: Vec<Attestation>,
    pub ops: Vec<(Op, Timestamp)>,
}

/// A whole proof file: the digest it starts from, and the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proof {
    pub digest: [u8; 32],
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OtsError {
    Truncated,
    TooLong(&'static str),
    TooDeep,
    TooMany,
    Malformed(&'static str),
    BadUri,
    Unsupported(&'static str),
    Trailing,
}

impl fmt::Display for OtsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OtsError::Truncated => f.write_str("proof is cut short"),
            OtsError::TooLong(w) => write!(f, "{w} is longer than this checker allows"),
            OtsError::TooDeep => write!(f, "proof is nested deeper than {MAX_DEPTH} ops"),
            OtsError::TooMany => write!(f, "proof has more than {MAX_ITEMS} items"),
            OtsError::Malformed(w) => write!(f, "proof is malformed: {w}"),
            OtsError::BadUri => f.write_str("a pending attestation names a calendar URI with characters a calendar URI may not have"),
            OtsError::Unsupported(op) => write!(f, "the proof uses {op}, which this checker does not compute"),
            OtsError::Trailing => f.write_str("bytes after the end of the proof"),
        }
    }
}

// ---- reading -------------------------------------------------------------

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
    items: usize,
}

impl<'a> Reader<'a> {
    fn byte(&mut self) -> Result<u8, OtsError> {
        let v = *self.b.get(self.pos).ok_or(OtsError::Truncated)?;
        self.pos += 1;
        Ok(v)
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8], OtsError> {
        let end = self.pos.checked_add(n).ok_or(OtsError::Truncated)?;
        let v = self.b.get(self.pos..end).ok_or(OtsError::Truncated)?;
        self.pos = end;
        Ok(v)
    }

    fn varuint(&mut self) -> Result<u64, OtsError> {
        let mut v = 0u64;
        for i in 0..9 {
            let b = self.byte()?;
            v |= u64::from(b & 0x7f) << (7 * i);
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(OtsError::Malformed("varuint longer than 9 bytes"))
    }

    fn varbytes(&mut self, max: usize, min: usize) -> Result<&'a [u8], OtsError> {
        let n = self.varuint()?;
        let n = usize::try_from(n).ok().filter(|&n| n <= max).ok_or(OtsError::TooLong("field"))?;
        if n < min {
            return Err(OtsError::Malformed("field shorter than allowed"));
        }
        self.bytes(n)
    }

    fn count(&mut self) -> Result<(), OtsError> {
        self.items += 1;
        if self.items > MAX_ITEMS { Err(OtsError::TooMany) } else { Ok(()) }
    }

    fn op(&mut self, tag: u8) -> Result<Op, OtsError> {
        Ok(match tag {
            0x02 => Op::Sha1,
            0x03 => Op::Ripemd160,
            0x08 => Op::Sha256,
            0x67 => Op::Keccak256,
            0xf0 => Op::Append(self.varbytes(MAX_MSG, 1)?.to_vec()),
            0xf1 => Op::Prepend(self.varbytes(MAX_MSG, 1)?.to_vec()),
            0xf2 => Op::Reverse,
            0xf3 => Op::Hexlify,
            _ => return Err(OtsError::Malformed("unknown op")),
        })
    }

    fn attestation(&mut self) -> Result<Attestation, OtsError> {
        let tag: [u8; 8] = self.bytes(8)?.try_into().map_err(|_| OtsError::Truncated)?;
        let payload = self.varbytes(MAX_PAYLOAD, 0)?;
        let mut p = Reader { b: payload, pos: 0, items: 0 };
        let a = match tag {
            PENDING_TAG => {
                let uri = p.varbytes(MAX_URI, 0)?;
                if !uri.iter().all(|c| URI_CHARS.contains(c)) {
                    return Err(OtsError::BadUri);
                }
                Attestation::Pending { uri: String::from_utf8(uri.to_vec()).map_err(|_| OtsError::BadUri)? }
            }
            BITCOIN_TAG => {
                let h = p.varuint()?;
                if h > u64::from(u32::MAX) {
                    return Err(OtsError::Malformed("block height out of range"));
                }
                Attestation::Bitcoin { height: h }
            }
            _ => return Ok(Attestation::Other { tag, payload: payload.to_vec() }),
        };
        if p.pos != payload.len() {
            return Err(OtsError::Malformed("attestation payload has bytes left over"));
        }
        Ok(a)
    }

    /// One node and everything under it, at most `MAX_DEPTH - depth` deep.
    fn timestamp(&mut self, depth: usize) -> Result<Timestamp, OtsError> {
        if depth >= MAX_DEPTH {
            return Err(OtsError::TooDeep);
        }
        let mut t = Timestamp::default();
        loop {
            let first = self.byte()?;
            let more = first == 0xff;
            let tag = if more { self.byte()? } else { first };
            self.count()?;
            if tag == 0x00 {
                if !t.ops.is_empty() {
                    return Err(OtsError::Malformed("attestation after an op"));
                }
                t.attestations.push(self.attestation()?);
            } else {
                let op = self.op(tag)?;
                if t.ops.iter().any(|(o, _)| *o == op) {
                    return Err(OtsError::Malformed("op repeated at one node"));
                }
                let child = self.timestamp(depth + 1)?;
                t.ops.push((op, child));
            }
            if !more {
                return Ok(t);
            }
        }
    }
}

/// A calendar's reply: a timestamp (no file header), all of it.
pub fn parse_timestamp(b: &[u8]) -> Result<Timestamp, OtsError> {
    if b.len() > MAX_PROOF {
        return Err(OtsError::TooLong("proof"));
    }
    let mut r = Reader { b, pos: 0, items: 0 };
    let t = r.timestamp(0)?;
    if r.pos != b.len() {
        return Err(OtsError::Trailing);
    }
    Ok(t)
}

/// A proof file (`.ots`): SHA-256 digest only (the only kind made here).
pub fn parse_proof(b: &[u8]) -> Result<Proof, OtsError> {
    if b.len() > MAX_PROOF {
        return Err(OtsError::TooLong("proof"));
    }
    let mut r = Reader { b, pos: 0, items: 0 };
    if r.bytes(MAGIC.len())? != MAGIC {
        return Err(OtsError::Malformed("not an OpenTimestamps proof (header)"));
    }
    if r.byte()? != VERSION {
        return Err(OtsError::Malformed("unknown proof version"));
    }
    if r.byte()? != 0x08 {
        return Err(OtsError::Malformed("the proof's digest is not SHA-256"));
    }
    let digest: [u8; 32] = r.bytes(32)?.try_into().map_err(|_| OtsError::Truncated)?;
    let timestamp = r.timestamp(0)?;
    if r.pos != b.len() {
        return Err(OtsError::Trailing);
    }
    Ok(Proof { digest, timestamp })
}

// ---- writing ---------------------------------------------------------------

pub fn varuint(mut n: u64) -> Vec<u8> {
    let mut v = Vec::new();
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            v.push(b);
            return v;
        }
        v.push(b | 0x80);
    }
}

pub fn varbytes(b: &[u8]) -> Vec<u8> {
    let mut v = varuint(b.len() as u64);
    v.extend_from_slice(b);
    v
}

impl Timestamp {
    /// Serialised as the reference code does: attestations (sorted by tag and
    /// payload) before ops (sorted by tag and argument), `0xff` before every item but the last. (The order of
    /// several attestations or ops at one node follows this code's sort;
    /// it does not change what the proof proves.)
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut atts: Vec<([u8; 8], Vec<u8>)> = self.attestations.iter().map(Attestation::tag_payload).collect();
        atts.sort();
        let mut ops: Vec<&(Op, Timestamp)> = self.ops.iter().collect();
        ops.sort_by(|a, b| a.0.cmp(&b.0));
        let total = atts.len() + ops.len();
        let mut k = 0;
        for (tag, payload) in atts {
            k += 1;
            if k < total {
                out.push(0xff);
            }
            out.push(0x00);
            out.extend_from_slice(&tag);
            out.extend(varbytes(&payload));
        }
        for (op, child) in ops {
            k += 1;
            if k < total {
                out.push(0xff);
            }
            out.push(op.tag());
            if let Op::Append(a) | Op::Prepend(a) = op {
                out.extend(varbytes(a));
            }
            child.write(out);
        }
    }

    /// A single chain: `ops` in order, ending in `att`.
    pub fn chain(ops: Vec<Op>, att: Attestation) -> Timestamp {
        let mut t = Timestamp { attestations: vec![att], ops: Vec::new() };
        for op in ops.into_iter().rev() {
            t = Timestamp { attestations: Vec::new(), ops: vec![(op, t)] };
        }
        t
    }

    /// Every attestation reached from `msg`, with the message there; and
    /// every branch that could not be followed, with why.
    pub fn walk(&self, msg: &[u8]) -> Walk {
        let mut w = Walk::default();
        self.walk_into(msg, 0, &mut w);
        w
    }

    fn walk_into(&self, msg: &[u8], depth: usize, w: &mut Walk) {
        if depth >= MAX_DEPTH {
            w.skipped.push(OtsError::TooDeep.to_string());
            return;
        }
        for a in &self.attestations {
            w.reached.push((msg.to_vec(), a.clone()));
        }
        for (op, child) in &self.ops {
            match op.apply(msg) {
                Ok(m) => child.walk_into(&m, depth + 1, w),
                Err(e) => w.skipped.push(e.to_string()),
            }
        }
    }

    /// Add `upgrade` (a calendar's timestamp for `commitment`) at every node
    /// whose message is `commitment`, keeping everything already there.
    /// Returns how many nodes it was added at.
    pub fn graft(&mut self, msg: &[u8], commitment: &[u8], upgrade: &Timestamp) -> usize {
        self.graft_at(msg, commitment, upgrade, 0)
    }

    fn graft_at(&mut self, msg: &[u8], commitment: &[u8], upgrade: &Timestamp, depth: usize) -> usize {
        if depth >= MAX_DEPTH {
            return 0;
        }
        let mut n = 0;
        if msg == commitment {
            self.merge(upgrade, depth);
            n += 1;
        }
        for (op, child) in &mut self.ops {
            if let Ok(m) = op.apply(msg) {
                n += child.graft_at(&m, commitment, upgrade, depth + 1);
            }
        }
        n
    }

    fn merge(&mut self, other: &Timestamp, depth: usize) {
        if depth >= MAX_DEPTH {
            return;
        }
        for a in &other.attestations {
            if !self.attestations.contains(a) {
                self.attestations.push(a.clone());
            }
        }
        for (op, child) in &other.ops {
            match self.ops.iter_mut().find(|(o, _)| o == op) {
                Some((_, mine)) => mine.merge(child, depth + 1),
                None => self.ops.push((op.clone(), child.clone())),
            }
        }
    }
}

/// What [`Timestamp::walk`] found.
#[derive(Debug, Clone, Default)]
pub struct Walk {
    pub reached: Vec<(Vec<u8>, Attestation)>,
    pub skipped: Vec<String>,
}

impl Walk {
    /// The pending attestations: (calendar URI, the commitment to ask it for).
    pub fn pending(&self) -> Vec<(String, Vec<u8>)> {
        self.reached.iter().filter_map(|(m, a)| if let Attestation::Pending { uri } = a { Some((uri.clone(), m.clone())) } else { None }).collect()
    }

    /// The Bitcoin attestations: (height, the message reached).
    pub fn bitcoin(&self) -> Vec<(u64, Vec<u8>)> {
        self.reached.iter().filter_map(|(m, a)| if let Attestation::Bitcoin { height } = a { Some((*height, m.clone())) } else { None }).collect()
    }
}

impl Proof {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = MAGIC.to_vec();
        v.push(VERSION);
        v.push(0x08);
        v.extend_from_slice(&self.digest);
        self.timestamp.write(&mut v);
        v
    }

    pub fn walk(&self) -> Walk {
        self.timestamp.walk(&self.digest)
    }

    /// Complete: it reaches at least one Bitcoin attestation.
    pub fn is_anchored(&self) -> bool {
        !self.walk().bitcoin().is_empty()
    }
}

// ---- Bitcoin block headers -------------------------------------------------

/// An 80-byte Bitcoin block header, as the person checking supplies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockHeader(pub [u8; 80]);

/// The easiest target Bitcoin's main chain allows (compact `0x1d00ffff`).
pub const POW_LIMIT_BITS: u32 = 0x1d00_ffff;

impl BlockHeader {
    pub fn from_hex(s: &str) -> Option<Self> {
        crate::verify::unhex(s.trim()).and_then(|v| <[u8; 80]>::try_from(v).ok()).map(BlockHeader)
    }

    /// As stored in the header (the byte order an attestation commits to).
    pub fn merkle_root(&self) -> [u8; 32] {
        self.0[36..68].try_into().unwrap_or([0; 32])
    }

    fn u32_at(&self, i: usize) -> u32 {
        u32::from_le_bytes([self.0[i], self.0[i + 1], self.0[i + 2], self.0[i + 3]])
    }

    /// The block's time (seconds since 1970), as the miner set it.
    pub fn time(&self) -> u32 {
        self.u32_at(68)
    }

    pub fn bits(&self) -> u32 {
        self.u32_at(72)
    }

    /// SHA-256(SHA-256(header)), in the order block explorers show.
    pub fn hash_hex(&self) -> String {
        let mut h = crate::sha256(&crate::sha256(&self.0));
        h.reverse();
        crate::verify::hex(&h)
    }

    /// The header meets the target it states, and that target is no easier
    /// than Bitcoin's main chain has ever allowed. (This shows work was
    /// done, not that the header is on the real chain: a header at the
    /// easiest target can be made on one computer in minutes.)
    pub fn check_work(&self) -> Result<(), String> {
        let target = target_be(self.bits()).ok_or_else(|| format!("the header's target (bits {:08x}) is not a valid target", self.bits()))?;
        let limit = target_be(POW_LIMIT_BITS).unwrap_or([0; 32]);
        if target > limit {
            return Err(format!("the header's target (bits {:08x}) is easier than Bitcoin's main chain allows: not a main-chain block", self.bits()));
        }
        let mut h = crate::sha256(&crate::sha256(&self.0));
        h.reverse();
        if h > target {
            return Err(format!("the header's hash {} does not meet the target it states (bits {:08x}): not a valid block header", crate::verify::hex(&h), self.bits()));
        }
        Ok(())
    }
}

/// A compact target as a 256-bit big-endian number; `None` if negative,
/// zero or too large to represent.
pub fn target_be(bits: u32) -> Option<[u8; 32]> {
    let exp = (bits >> 24) as usize;
    let mant = bits & 0x007f_ffff;
    if bits & 0x0080_0000 != 0 {
        return None;
    }
    let mut t = [0u8; 32];
    if exp <= 3 {
        t[28..].copy_from_slice(&(mant >> (8 * (3 - exp))).to_be_bytes());
    } else {
        let m = mant.to_be_bytes();
        for k in 0..3 {
            // Mantissa byte k (most significant first) sits this many bytes
            // from the right.
            let pos = exp - 3 + (2 - k);
            match (pos < 32, m[1 + k]) {
                (true, b) => t[31 - pos] = b,
                (false, 0) => {}
                (false, _) => return None,
            }
        }
    }
    if t == [0; 32] { None } else { Some(t) }
}

// ---- RIPEMD-160 (early proofs use it; no crate) ------------------------------

/// RIPEMD-160 (Dobbertin, Bosselaers, Preneel, 1996).
pub fn ripemd160(data: &[u8]) -> [u8; 20] {
    const RL: [usize; 80] = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 7, 4, 13, 1, 10, 6, 15, 3, 12, 0, 9, 5, 2, 14, 11, 8, 3, 10, 14, 4, 9, 15, 8, 1, 2, 7, 0, 6, 13, 11, 5, 12, 1, 9, 11, 10, 0, 8, 12, 4, 13, 3, 7,
        15, 14, 5, 6, 2, 4, 0, 5, 9, 7, 12, 2, 10, 14, 1, 3, 8, 11, 6, 15, 13,
    ];
    const RR: [usize; 80] = [
        5, 14, 7, 0, 9, 2, 11, 4, 13, 6, 15, 8, 1, 10, 3, 12, 6, 11, 3, 7, 0, 13, 5, 10, 14, 15, 8, 12, 4, 9, 1, 2, 15, 5, 1, 3, 7, 14, 6, 9, 11, 8, 12, 2, 10, 0, 4, 13, 8, 6, 4, 1, 3, 11, 15, 0, 5, 12, 2,
        13, 9, 7, 10, 14, 12, 15, 10, 4, 1, 5, 8, 7, 6, 2, 13, 14, 0, 3, 9, 11,
    ];
    const SL: [u32; 80] = [
        11, 14, 15, 12, 5, 8, 7, 9, 11, 13, 14, 15, 6, 7, 9, 8, 7, 6, 8, 13, 11, 9, 7, 15, 7, 12, 15, 9, 11, 7, 13, 12, 11, 13, 6, 7, 14, 9, 13, 15, 14, 8, 13, 6, 5, 12, 7, 5, 11, 12, 14, 15, 14, 15, 9, 8, 9,
        14, 5, 6, 8, 6, 5, 12, 9, 15, 5, 11, 6, 8, 13, 12, 5, 12, 13, 14, 11, 8, 5, 6,
    ];
    const SR: [u32; 80] = [
        8, 9, 9, 11, 13, 15, 15, 5, 7, 7, 8, 11, 14, 14, 12, 6, 9, 13, 15, 7, 12, 8, 9, 11, 7, 7, 12, 7, 6, 15, 13, 11, 9, 7, 15, 11, 8, 6, 6, 14, 12, 13, 5, 14, 13, 13, 7, 5, 15, 5, 8, 11, 14, 14, 6, 14, 6, 9,
        12, 9, 12, 5, 15, 8, 8, 5, 12, 9, 12, 5, 14, 6, 8, 13, 6, 5, 15, 13, 11, 11,
    ];
    const KL: [u32; 5] = [0x0000_0000, 0x5a82_7999, 0x6ed9_eba1, 0x8f1b_bcdc, 0xa953_fd4e];
    const KR: [u32; 5] = [0x50a2_8be6, 0x5c4d_d124, 0x6d70_3ef3, 0x7a6d_76e9, 0x0000_0000];
    fn f(j: usize, x: u32, y: u32, z: u32) -> u32 {
        match j / 16 {
            0 => x ^ y ^ z,
            1 => (x & y) | (!x & z),
            2 => (x | !y) ^ z,
            3 => (x & z) | (y & !z),
            _ => x ^ (y | !z),
        }
    }
    let mut h: [u32; 5] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476, 0xc3d2_e1f0];
    let mut msg = data.to_vec();
    let bitlen = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bitlen.to_le_bytes());
    for block in msg.as_chunks::<64>().0 {
        let mut x = [0u32; 16];
        for (i, w) in block.as_chunks::<4>().0.iter().enumerate() {
            x[i] = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
        }
        let (mut al, mut bl, mut cl, mut dl, mut el) = (h[0], h[1], h[2], h[3], h[4]);
        let (mut ar, mut br, mut cr, mut dr, mut er) = (h[0], h[1], h[2], h[3], h[4]);
        for j in 0..80 {
            let t = al.wrapping_add(f(j, bl, cl, dl)).wrapping_add(x[RL[j]]).wrapping_add(KL[j / 16]).rotate_left(SL[j]).wrapping_add(el);
            al = el;
            el = dl;
            dl = cl.rotate_left(10);
            cl = bl;
            bl = t;
            let t = ar.wrapping_add(f(79 - j, br, cr, dr)).wrapping_add(x[RR[j]]).wrapping_add(KR[j / 16]).rotate_left(SR[j]).wrapping_add(er);
            ar = er;
            er = dr;
            dr = cr.rotate_left(10);
            cr = br;
            br = t;
        }
        let t = h[1].wrapping_add(cl).wrapping_add(dr);
        h[1] = h[2].wrapping_add(dl).wrapping_add(er);
        h[2] = h[3].wrapping_add(el).wrapping_add(ar);
        h[3] = h[4].wrapping_add(al).wrapping_add(br);
        h[4] = h[0].wrapping_add(bl).wrapping_add(cr);
        h[0] = t;
    }
    let mut out = [0u8; 20];
    for (i, w) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&w.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::hex;

    #[test]
    fn ripemd160_published_vectors() {
        // From the RIPEMD-160 authors' page; also checked against Python's
        // hashlib in tests/ots.rs (the fixtures use it).
        for (m, h) in [
            ("", "9c1185a5c5e9fc54612808977ee8f548b2258d31"),
            ("a", "0bdc9d2d256b3ee9daae347be6f4dc835a467ffe"),
            ("abc", "8eb208f7e05d987a9b044a8e98c6b087f15a0bfc"),
            ("message digest", "5d0689ef49d2fae572b881b123a85ffa21595f36"),
            ("abcdefghijklmnopqrstuvwxyz", "f71c27109c692c1b56bbdceb5b9d2865b3708dbc"),
            ("abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq", "12a053384a9c0c88e405a06c27dcf49ada62eb2b"),
        ] {
            assert_eq!(hex(&ripemd160(m.as_bytes())), h, "{m:?}");
        }
        assert_eq!(hex(&ripemd160(&[b'a'; 1_000_000])), "52783243c1697bdbe16d37f97f68f08325dc1528");
    }

    #[test]
    fn varuint_round_trip_and_limits() {
        for n in [0u64, 1, 127, 128, 300, 16_383, 16_384, u32::MAX as u64, (1 << 63) - 1] {
            let v = varuint(n);
            let mut r = Reader { b: &v, pos: 0, items: 0 };
            assert_eq!(r.varuint(), Ok(n));
        }
        assert_eq!(varuint(300), vec![0xac, 0x02]);
        let ten = [0x80u8; 10];
        assert!(Reader { b: &ten, pos: 0, items: 0 }.varuint().is_err());
    }

    #[test]
    fn targets() {
        let t = target_be(POW_LIMIT_BITS).unwrap();
        assert_eq!(hex(&t), "00000000ffff0000000000000000000000000000000000000000000000000000");
        assert_eq!(hex(&target_be(0x1b04_04cb).unwrap()), "00000000000404cb000000000000000000000000000000000000000000000000");
        assert!(target_be(0x0480_0000).is_none(), "negative");
        assert!(target_be(0x2300_0001).is_none(), "beyond 256 bits");
        assert!(target_be(0x2100_0001).is_some());
        assert!(target_be(0x2000_0000).is_none(), "zero");
        assert!(target_be(0x207f_ffff).unwrap() > t, "regtest's target is easier");
    }

    /// Bitcoin's genesis block header (published everywhere; height 0).
    pub const GENESIS: &str = "0100000000000000000000000000000000000000000000000000000000000000000000003ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a29ab5f49ffff001d1dac2b7c";

    #[test]
    fn genesis_header() {
        let h = BlockHeader::from_hex(GENESIS).unwrap();
        assert_eq!(h.hash_hex(), "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f");
        assert_eq!(h.time(), 1_231_006_505);
        h.check_work().unwrap();
        let mut bad = h;
        bad.0[79] ^= 1;
        assert!(bad.check_work().unwrap_err().contains("does not meet"));
    }
}
