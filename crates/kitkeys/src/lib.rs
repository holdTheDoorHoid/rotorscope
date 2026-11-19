//! Kit keys (docs/LINK-KEYS-DESIGN.md, section 4): one random master per
//! kit, and per ESP-NOW node a key derived from the master and the node's
//! identity (roster slot, generation, station MAC).
//!
//! ```text
//! kit_id    = HMAC(M, "rotorscope/kit-id/v1")[0..4]                      not secret
//! K_node    = HMAC(M, "rotorscope/node-key/v1" | slot:u8 | gen:u16 LE | mac:[6])
//! K_up      = HMAC(K_node, "rotorscope/up/v1")      node to hub
//! K_down    = HMAC(K_node, "rotorscope/down/v1")    hub to node
//! key_check = HMAC(K_node, "rotorscope/check/v1")[0..4]                  not secret
//! ```
//!
//! What lives where:
//! - [`KeySource`] is the only thing that can use the master. The software
//!   one ([`SoftKeySource`]) holds it as a keyed HMAC state; a secure element
//!   on I2C can implement the same trait by computing the HMAC inside the
//!   element (it gets [`kit_id_input`] / [`node_key_input`] and returns the
//!   32-byte result), so the master never leaves it.
//! - [`record`]: the node's `rskey` partition (its own `K_node` only).
//! - [`hub`]: the hub's `rskit` partition (master and roster).
//! - [`roster`]: slots 0..15, generations, revocation (no secrets).
//! - [`bootlog`]: the hub's `rsboot` boot counter (epochs, evidence).
//!
//! Secrets ([`MasterKey`], [`NodeKey`], [`LinkKey`]) are not `Copy` or
//! `Clone`, print as `..` in `Debug`, have no `Display`, and overwrite
//! themselves when dropped. That is best effort: a move can leave a copy in
//! a stack slot the compiler chose, which Rust gives no way to wipe without
//! extra crates.
//!
//! The HMAC is airlink's (`crates/airlink/src/hmac.rs`), compiled into this
//! crate by path, the same way airlink's `build.rs` uses it. A crate
//! dependency is not possible: airlink will depend on this crate for its key
//! types (package 13b), and the primitive stays one file.
#![cfg_attr(not(test), no_std)]

#[path = "../../airlink/src/hmac.rs"]
mod hmac;

pub mod bootlog;
pub mod hub;
pub mod record;
pub mod roster;

use hmac::Hmac;

/// Label for [`KitId`].
pub const LABEL_KIT_ID: &[u8] = b"rotorscope/kit-id/v1";
/// Label for the node key.
pub const LABEL_NODE_KEY: &[u8] = b"rotorscope/node-key/v1";
/// Label for the node-to-hub key.
pub const LABEL_UP: &[u8] = b"rotorscope/up/v1";
/// Label for the hub-to-node key.
pub const LABEL_DOWN: &[u8] = b"rotorscope/down/v1";
/// Label for [`KeyCheck`].
pub const LABEL_CHECK: &[u8] = b"rotorscope/check/v1";

/// Length of every key here.
pub const KEY_LEN: usize = 32;
/// Number of roster slots (and of beacon tags).
pub const SLOTS: usize = 16;

/// A station MAC address, as the radio reports it.
pub type Mac = [u8; 6];

/// Overwrite `b` with zeros in a way the optimiser keeps.
pub(crate) fn wipe(b: &mut [u8]) {
    b.fill(0);
    core::hint::black_box(&*b);
}

/// CRC-32 (IEEE 802.3, reflected, as zlib), over the records here.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

// ------------------------------------------------------------ public ids --

/// Names a kit without revealing its master: the first four bytes of
/// `HMAC(M, "rotorscope/kit-id/v1")`. Safe to print.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct KitId(pub [u8; 4]);

/// Names a node key without revealing it: the first four bytes of
/// `HMAC(K_node, "rotorscope/check/v1")`. Safe to print; the board prints
/// its own at boot and `link-key show` prints the roster's.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct KeyCheck(pub [u8; 4]);

macro_rules! hex4 {
    ($t:ty) => {
        impl core::fmt::Display for $t {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                for b in self.0 {
                    write!(f, "{b:02x}")?;
                }
                Ok(())
            }
        }
        impl core::fmt::Debug for $t {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write!(f, "{}({self})", stringify!($t))
            }
        }
    };
}
hex4!(KitId);
hex4!(KeyCheck);

/// What a node key is derived from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeIdent {
    /// Roster slot, 0..15: the node's number on the hub and its beacon tag index.
    pub slot: u8,
    /// Bumped each time the slot is provisioned again.
    pub generation: u16,
    /// Station MAC from eFuse.
    pub mac: Mac,
}

/// Length of [`node_key_input`].
pub const NODE_KEY_INPUT_LEN: usize = LABEL_NODE_KEY.len() + 1 + 2 + 6;

/// The exact message the master is HMAC'd over for `ident`'s node key: what
/// a secure element is asked to MAC (it returns the 32-byte node key).
pub fn node_key_input(ident: &NodeIdent) -> [u8; NODE_KEY_INPUT_LEN] {
    let mut m = [0u8; NODE_KEY_INPUT_LEN];
    let l = LABEL_NODE_KEY.len();
    m[..l].copy_from_slice(LABEL_NODE_KEY);
    m[l] = ident.slot;
    m[l + 1..l + 3].copy_from_slice(&ident.generation.to_le_bytes());
    m[l + 3..].copy_from_slice(&ident.mac);
    m
}

/// The message the master is HMAC'd over for the kit id (first 4 bytes kept).
pub fn kit_id_input() -> &'static [u8] {
    LABEL_KIT_ID
}

// ---------------------------------------------------------------- secrets --

/// The kit master `M`. Only a [`KeySource`] uses it; outside this crate its
/// bytes are reachable only through the record and kit-file encoders.
pub struct MasterKey([u8; KEY_LEN]);

impl MasterKey {
    /// `None` for all zeros: in the hub record that means "held by the
    /// secure element", so it can never be a real master.
    pub fn from_bytes(mut b: [u8; KEY_LEN]) -> Option<Self> {
        let r = if b.iter().all(|&x| x == 0) { None } else { Some(MasterKey(b)) };
        wipe(&mut b);
        r
    }

    /// The kit id of this master.
    pub fn kit_id(&self) -> KitId {
        let m = Hmac::new(&self.0).mac(&[LABEL_KIT_ID]);
        KitId([m[0], m[1], m[2], m[3]])
    }

    pub(crate) fn bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }

    /// Length of [`MasterKey::to_kit_file`]'s output.
    pub const KIT_FILE_LEN: usize = KIT_FILE_HEADER.len() + 2 * KEY_LEN + 1;

    /// The kit file (`tools/kit.key`): a header line, then 64 hex digits.
    /// The caller writes it to a 0600 file and wipes the buffer.
    pub fn to_kit_file(&self) -> KitFileText {
        let mut t = [0u8; Self::KIT_FILE_LEN];
        let h = KIT_FILE_HEADER.len();
        t[..h].copy_from_slice(KIT_FILE_HEADER);
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for (i, b) in self.0.iter().enumerate() {
            t[h + 2 * i] = HEX[usize::from(b >> 4)];
            t[h + 2 * i + 1] = HEX[usize::from(b & 15)];
        }
        t[Self::KIT_FILE_LEN - 1] = b'\n';
        KitFileText(t)
    }

    /// Parse a kit file. Whitespace around the hex digits is ignored.
    pub fn from_kit_file(text: &[u8]) -> Result<Self, KitFileError> {
        let rest = text.strip_prefix(KIT_FILE_HEADER).ok_or(KitFileError::NotAKitFile)?;
        let mut key = [0u8; KEY_LEN];
        let mut n = 0usize;
        let mut half: Option<u8> = None;
        for &c in rest {
            if c.is_ascii_whitespace() {
                continue;
            }
            let v = match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                b'A'..=b'F' => c - b'A' + 10,
                _ => {
                    wipe(&mut key);
                    return Err(KitFileError::BadKey);
                }
            };
            match half.take() {
                None => half = Some(v),
                Some(hi) => {
                    if n == KEY_LEN {
                        wipe(&mut key);
                        return Err(KitFileError::BadKey);
                    }
                    key[n] = hi << 4 | v;
                    n += 1;
                }
            }
        }
        if n != KEY_LEN || half.is_some() {
            wipe(&mut key);
            return Err(KitFileError::BadKey);
        }
        MasterKey::from_bytes(key).ok_or(KitFileError::ZeroKey)
    }
}

/// First line of a kit file.
pub const KIT_FILE_HEADER: &[u8] = b"rotorscope kit master v1 (secret: never commit, never print)\n";

/// A kit file's text; wiped when dropped. No `Debug`.
pub struct KitFileText([u8; MasterKey::KIT_FILE_LEN]);

impl KitFileText {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for KitFileText {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

/// Why a kit file was refused (none of these carry key material).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KitFileError {
    /// The header line is missing.
    NotAKitFile,
    /// Not exactly 64 hex digits after the header.
    BadKey,
    /// All zeros: not a master.
    ZeroKey,
}

impl Drop for MasterKey {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

impl core::fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("MasterKey(..)")
    }
}

/// One node's key `K_node`. The node holds only this; the hub derives it
/// per slot at boot. Use [`NodeKey::up`] and [`NodeKey::down`] for traffic.
pub struct NodeKey([u8; KEY_LEN]);

impl NodeKey {
    /// From a node record (or a secure element's answer).
    pub fn from_bytes(mut b: [u8; KEY_LEN]) -> Self {
        let k = NodeKey(b);
        wipe(&mut b);
        k
    }

    /// The node-to-hub key (keyed HMAC state).
    pub fn up(&self) -> LinkKey {
        self.sub(LABEL_UP)
    }

    /// The hub-to-node key (keyed HMAC state).
    pub fn down(&self) -> LinkKey {
        self.sub(LABEL_DOWN)
    }

    /// Four public bytes naming this key.
    pub fn check(&self) -> KeyCheck {
        let m = Hmac::new(&self.0).mac(&[LABEL_CHECK]);
        KeyCheck([m[0], m[1], m[2], m[3]])
    }

    fn sub(&self, label: &[u8]) -> LinkKey {
        let mut k = Hmac::new(&self.0).mac(&[label]);
        let l = LinkKey(Hmac::new(&k));
        wipe(&mut k);
        l
    }

    pub(crate) fn bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }

    /// Constant-time comparison (both are secret).
    pub fn ct_eq(&self, other: &NodeKey) -> bool {
        ct_eq(&self.0, &other.0)
    }
}

impl Drop for NodeKey {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

impl core::fmt::Debug for NodeKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("NodeKey(..)")
    }
}

/// `K_up` or `K_down`, both HMAC pads already absorbed (what sits in RAM
/// for the hot path; one tag costs the message's blocks plus one).
pub struct LinkKey(Hmac);

impl LinkKey {
    /// Full 32-byte HMAC of the concatenation of `parts`; callers truncate.
    pub fn mac(&self, parts: &[&[u8]]) -> [u8; 32] {
        self.0.mac(parts)
    }
}

impl Drop for LinkKey {
    fn drop(&mut self) {
        // Replace the keyed state with one keyed by zeros, in place.
        self.0 = Hmac::new(&[0u8; KEY_LEN]);
        core::hint::black_box(&self.0);
    }
}

impl core::fmt::Debug for LinkKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("LinkKey(..)")
    }
}

/// Constant-time equality of two byte strings of the same length.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut d = 0u8;
    for (x, y) in a.iter().zip(b) {
        d |= x ^ y;
    }
    core::hint::black_box(d) == 0
}

// ------------------------------------------------------------- key source --

/// The one place the kit master is used. The hub calls [`KeySource::derive`]
/// once per roster slot at boot and keeps only the derived [`LinkKey`]s; the
/// host tool calls it to provision a node. No code outside an implementation
/// of this trait reads the master.
///
/// A secure element implements this by MACing [`kit_id_input`] or
/// [`node_key_input`] with the master it holds (an HMAC-SHA256 "derive by
/// label" command) and returning the result; it never exports the master.
pub trait KeySource {
    type Error;

    /// The kit id (public).
    fn kit_id(&mut self) -> Result<KitId, Self::Error>;

    /// `K_node` for `ident`.
    fn derive(&mut self, ident: &NodeIdent) -> Result<NodeKey, Self::Error>;

    /// Fill `out` from a true random source if this key source has one
    /// (`Ok(true)`). The software source has none (`Ok(false)`): the hub has
    /// no proven random source until an element is fitted, and nothing may
    /// rely on hub randomness for security (it uses the boot counter for
    /// uniqueness instead).
    fn random(&mut self, out: &mut [u8]) -> Result<bool, Self::Error> {
        let _ = out;
        Ok(false)
    }
}

/// The master in RAM as a keyed HMAC state (the raw bytes are dropped and
/// wiped when this is made).
pub struct SoftKeySource(Hmac);

impl SoftKeySource {
    pub fn new(master: MasterKey) -> Self {
        SoftKeySource(Hmac::new(master.bytes()))
        // `master` is dropped (and wiped) here.
    }
}

impl Drop for SoftKeySource {
    fn drop(&mut self) {
        self.0 = Hmac::new(&[0u8; KEY_LEN]);
        core::hint::black_box(&self.0);
    }
}

impl core::fmt::Debug for SoftKeySource {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("SoftKeySource(..)")
    }
}

impl KeySource for SoftKeySource {
    type Error = core::convert::Infallible;

    fn kit_id(&mut self) -> Result<KitId, Self::Error> {
        let m = self.0.mac(&[kit_id_input()]);
        Ok(KitId([m[0], m[1], m[2], m[3]]))
    }

    fn derive(&mut self, ident: &NodeIdent) -> Result<NodeKey, Self::Error> {
        let mut input = node_key_input(ident);
        let k = NodeKey::from_bytes(self.0.mac(&[&input]));
        wipe(&mut input); // not secret, but cheap
        Ok(k)
    }
}

/// Derive `entry`'s key and check it against the roster's `key_check`: the
/// hub does this per slot at boot, so a roster from another kit (or a
/// different element) shows up as a mismatch, not as silent bad tags.
pub fn derive_checked<S: KeySource>(
    src: &mut S,
    ident: &NodeIdent,
    expected: KeyCheck,
) -> Result<Result<NodeKey, CheckMismatch>, S::Error> {
    let k = src.derive(ident)?;
    let got = k.check();
    Ok(if got == expected { Ok(k) } else { Err(CheckMismatch { expected, got }) })
}

/// A derived key did not match the roster's check value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckMismatch {
    pub expected: KeyCheck,
    pub got: KeyCheck,
}

#[cfg(test)]
mod tests;
