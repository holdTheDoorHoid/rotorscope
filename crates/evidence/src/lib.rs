//! The Rotorscope evidence log (PLAN.md Phase 13).
//!
//! A hub writes one **record** for every data frame it receives from its
//! nodes (the raw bytes as received plus a short decode), and one for every
//! event that could matter later (boot, clock and GPS changes, SD card,
//! nodes, settings, operator notes). Records are **hash-chained**: each
//! carries the SHA-256 of the one before it, so changing, removing or
//! reordering any record breaks every later link. About once a minute the
//! hub adds a **checkpoint**: the chain head, the record count and the time,
//! signed with ECDSA P-256. A checkpoint vouches for every record before it.
//!
//! The byte layout is in `docs/EVIDENCE-FORMAT.md`, written so someone who
//! has never seen this code can check a log with their own tools. This
//! crate is the reference implementation:
//!
//! - [`format`]: the layout, constants and a parser ([`format::parse`]).
//! - [`writer`]: [`writer::Chain`] builds records (used by the hub, `no_std`).
//! - [`sign`]: the [`sign::Signer`] trait, key classes and (feature
//!   `soft-signer`) the software development signer.
//! - [`summary`] (feature `decode`): the decode summary of a stored frame.
//! - [`verify`] (feature `verify`, host): check a whole log.
//! - [`b64`]: base64 for the console stream and bundles.
#![cfg_attr(not(any(test, feature = "std")), no_std)]

pub mod b64;
pub mod format;
pub mod sign;
#[cfg(feature = "decode")]
pub mod summary;
#[cfg(feature = "verify")]
pub mod verify;
pub mod writer;

pub use format::{EventCode, Kind, PosSource, TimeSource};
pub use sign::{KeyClass, Signer};
pub use writer::{Chain, Context};

/// SHA-256 of `data`.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    sha2::Sha256::digest(data).into()
}

/// Short key fingerprint: the first 8 bytes of SHA-256 of the 65-byte
/// uncompressed public key, as 16 hex digits in four groups
/// (`a1b2-c3d4-e5f6-0718`). Printed at setup and on the server.
pub fn fingerprint(pubkey: &[u8; 65]) -> [u8; 19] {
    let h = sha256(pubkey);
    let mut out = [b'-'; 19];
    let hex = b"0123456789abcdef";
    let mut o = 0;
    for (i, b) in h[..8].iter().enumerate() {
        if i > 0 && i % 2 == 0 {
            o += 1;
        }
        out[o] = hex[(b >> 4) as usize];
        out[o + 1] = hex[(b & 15) as usize];
        o += 2;
    }
    out
}
