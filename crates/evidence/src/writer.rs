//! Building records (the hub side). `no_std`, no allocation.
//!
//! Two steps, so a record is never chained unless it was kept:
//! 1. `Chain::event` / `detection` / `checkpoint` write a complete record
//!    into the caller's buffer (sequence number and previous hash filled
//!    in) and remember its hash. The chain does not move yet.
//! 2. [`Chain::commit`] once the caller has stored the bytes. If the
//!    caller had no room, it drops the bytes and never commits: the next
//!    record reuses the same sequence number, so the chain has no hole.

use crate::format::{self, off, Kind, PosSource, TimeSource, HEADER, MAX_RECORD, TRAILER};
use crate::sign::{SignError, Signer};

/// What the hub knows about time and its own position when it writes a
/// record. Filled in fresh for every record.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Context {
    pub uptime_ms: u64,
    pub utc_ms: Option<u64>,
    pub time_source: TimeSource,
    /// Since the UTC was last set from the GPS.
    pub time_age_ms: Option<u32>,
    /// 0 absent, 1 searching, 2 2D fix, 3 3D fix.
    pub gps_state: u8,
    pub pos_source: PosSource,
    pub sats: Option<u8>,
    pub lat_e7: i32,
    pub lon_e7: i32,
    pub alt_hae_mm: Option<i32>,
    pub hacc_dm: Option<u16>,
    pub hdop_x100: Option<u16>,
    pub fix_age_ms: Option<u32>,
}

/// The detection fields that are not the header.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Detection<'a> {
    /// Station address of the node that heard it (zero for a node wired to
    /// the hub).
    pub node: [u8; 6],
    /// RSSI of the node's own link to the gateway (ESP-NOW), if relayed.
    pub link_rssi: Option<i8>,
    pub node_index: u8,
    pub band: u8,
    pub frame_kind: u8,
    pub channel: u16,
    pub freq_100khz: u32,
    /// Signal strength of the drone's transmission at the node.
    pub rssi: Option<i8>,
    pub raw_format: u8,
    pub raw: &'a [u8],
    pub summary: &'a [u8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildError {
    /// The record would not fit the buffer or `MAX_RECORD`.
    TooLong,
    Sign(SignError),
    /// The signer's algorithm cannot be written as a format version 1
    /// checkpoint (see `sign`).
    Algorithm(crate::sign::Algorithm),
}

pub struct Chain {
    boot_id: [u8; 8],
    fw_hash: [u8; 32],
    seq: u64,
    head: [u8; 32],
    /// The hash of the record built last and not yet committed.
    staged: Option<[u8; 32]>,
}

struct Out<'a> {
    b: &'a mut [u8],
    at: usize,
}

impl Out<'_> {
    fn put(&mut self, s: &[u8]) -> Result<(), BuildError> {
        let end = self.at + s.len();
        // Keep room for the trailer.
        if end + TRAILER > self.b.len().min(MAX_RECORD) {
            return Err(BuildError::TooLong);
        }
        self.b[self.at..end].copy_from_slice(s);
        self.at = end;
        Ok(())
    }
    fn lv(&mut self, s: &[u8]) -> Result<(), BuildError> {
        let n = u16::try_from(s.len()).map_err(|_| BuildError::TooLong)?;
        self.put(&n.to_le_bytes())?;
        self.put(s)
    }
}

impl Chain {
    /// A new chain. `boot_id` should be random (different every boot);
    /// `fw_hash` is SHA-256 of the running firmware's ELF file.
    pub fn new(boot_id: [u8; 8], fw_hash: [u8; 32]) -> Self {
        Self { boot_id, fw_hash, seq: 0, head: [0; 32], staged: None }
    }

    /// The next record's sequence number (= records committed so far).
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// The hash of the last committed record (zero before the first).
    pub fn head(&self) -> [u8; 32] {
        self.head
    }

    pub fn boot_id(&self) -> [u8; 8] {
        self.boot_id
    }

    /// Accept the record built last: it is now part of the chain.
    pub fn commit(&mut self) {
        if let Some(h) = self.staged.take() {
            self.head = h;
            self.seq += 1;
        }
    }

    fn header(&self, kind: Kind, ctx: &Context, b: &mut [u8]) -> Result<(), BuildError> {
        if b.len() < HEADER + TRAILER {
            return Err(BuildError::TooLong);
        }
        b[off::MAGIC..off::MAGIC + 4].copy_from_slice(&format::MAGIC);
        b[off::VERSION] = format::VERSION;
        b[off::KIND] = kind as u8;
        b[off::BOOT_ID..off::BOOT_ID + 8].copy_from_slice(&self.boot_id);
        b[off::SEQ..off::SEQ + 8].copy_from_slice(&self.seq.to_le_bytes());
        b[off::PREV..off::PREV + 32].copy_from_slice(&self.head);
        b[off::UPTIME..off::UPTIME + 8].copy_from_slice(&ctx.uptime_ms.to_le_bytes());
        b[off::UTC..off::UTC + 8].copy_from_slice(&ctx.utc_ms.unwrap_or(0).to_le_bytes());
        // No UTC means no time source, whatever the caller said, except
        // "untrusted": GPS gives a time the hub withholds (WP-12).
        b[off::TIME_SRC] = match (ctx.utc_ms, ctx.time_source) {
            (Some(_), t) | (None, t @ TimeSource::Untrusted) => t as u8,
            (None, _) => TimeSource::None as u8,
        };
        b[off::GPS_STATE] = ctx.gps_state;
        b[off::POS_SRC] = ctx.pos_source as u8;
        b[off::SATS] = ctx.sats.unwrap_or(u8::MAX);
        b[off::TIME_AGE..off::TIME_AGE + 4].copy_from_slice(&ctx.time_age_ms.unwrap_or(u32::MAX).to_le_bytes());
        let (lat, lon) = if ctx.pos_source == PosSource::None { (0, 0) } else { (ctx.lat_e7, ctx.lon_e7) };
        b[off::LAT..off::LAT + 4].copy_from_slice(&lat.to_le_bytes());
        b[off::LON..off::LON + 4].copy_from_slice(&lon.to_le_bytes());
        b[off::ALT..off::ALT + 4].copy_from_slice(&ctx.alt_hae_mm.unwrap_or(format::ALT_NONE).to_le_bytes());
        b[off::HACC..off::HACC + 2].copy_from_slice(&ctx.hacc_dm.unwrap_or(u16::MAX).to_le_bytes());
        b[off::HDOP..off::HDOP + 2].copy_from_slice(&ctx.hdop_x100.unwrap_or(u16::MAX).to_le_bytes());
        b[off::FIX_AGE..off::FIX_AGE + 4].copy_from_slice(&ctx.fix_age_ms.unwrap_or(u32::MAX).to_le_bytes());
        b[off::FW..off::FW + 32].copy_from_slice(&self.fw_hash);
        Ok(())
    }

    /// Write the length into the header and trailer, hash, stage. Returns
    /// the record length.
    fn finish(&mut self, b: &mut [u8], body_end: usize) -> usize {
        let len = body_end + TRAILER;
        b[off::LEN..off::LEN + 2].copy_from_slice(&(len as u16).to_le_bytes());
        b[body_end..len].copy_from_slice(&(len as u32).to_le_bytes());
        self.staged = Some(crate::sha256(&b[..len]));
        len
    }

    pub fn event(&mut self, ctx: &Context, code: format::EventCode, text: &[u8], data: &[u8], buf: &mut [u8]) -> Result<usize, BuildError> {
        self.staged = None;
        self.header(Kind::Event, ctx, buf)?;
        let mut o = Out { b: buf, at: HEADER };
        o.put(&(code as u16).to_le_bytes())?;
        o.lv(text)?;
        o.lv(data)?;
        let end = o.at;
        Ok(self.finish(buf, end))
    }

    pub fn detection(&mut self, ctx: &Context, d: &Detection<'_>, buf: &mut [u8]) -> Result<usize, BuildError> {
        self.staged = None;
        self.header(Kind::Detection, ctx, buf)?;
        let mut o = Out { b: buf, at: HEADER };
        o.put(&d.node)?;
        o.put(&[d.link_rssi.unwrap_or(format::RSSI_NONE) as u8, d.node_index, d.band, d.frame_kind])?;
        o.put(&d.channel.to_le_bytes())?;
        o.put(&d.freq_100khz.to_le_bytes())?;
        o.put(&[d.rssi.unwrap_or(format::RSSI_NONE) as u8, d.raw_format])?;
        o.lv(d.raw)?;
        o.lv(&d.summary[..d.summary.len().min(format::MAX_SUMMARY)])?;
        let end = o.at;
        Ok(self.finish(buf, end))
    }

    /// A signed checkpoint over everything committed so far: the chain
    /// head (in the header's previous-hash field), the count (= this
    /// record's sequence number) and the time (header).
    pub fn checkpoint<S: Signer>(&mut self, ctx: &Context, reason: format::CheckpointReason, signer: &mut S, buf: &mut [u8]) -> Result<usize, BuildError> {
        self.staged = None;
        if signer.algorithm() != crate::sign::Algorithm::EcdsaP256Sha256 {
            return Err(BuildError::Algorithm(signer.algorithm()));
        }
        self.header(Kind::Checkpoint, ctx, buf)?;
        let class = signer.key_class();
        let mut o = Out { b: buf, at: HEADER };
        o.put(&[reason as u8, class as u8])?;
        o.put(&signer.public_key())?;
        o.put(&self.seq.to_le_bytes())?;
        o.lv(class.note().as_bytes())?;
        let signed_end = o.at;
        // Reserve the signature, then fill it in.
        o.put(&[0u8; format::SIG_LEN])?;
        let end = o.at;
        // The length field is inside the signed part: set it now.
        buf[off::LEN..off::LEN + 2].copy_from_slice(&((end + TRAILER) as u16).to_le_bytes());
        let digest = crate::sha256(&buf[..signed_end]);
        let mut sig = signer.sign_digest(&digest).map_err(BuildError::Sign)?;
        // Whatever the signer returned, only the low-S form is written.
        crate::sign::normalize_low_s(&mut sig);
        buf[signed_end..end].copy_from_slice(&sig);
        Ok(self.finish(buf, end))
    }
}
