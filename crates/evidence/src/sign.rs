//! Who signs checkpoints, and how sure we can be it was this unit.
//!
//! Every checkpoint is signed with ECDSA over NIST P-256 with SHA-256
//! (`ecdsa-with-SHA256`), the curve the ESP32-P4's ECDSA peripheral and the
//! S3/C5 digital-signature path can use, so a hardware signer can replace
//! the software one without changing the format. The signer is behind the
//! [`Signer`] trait; the hub picks one at build time.
//!
//! The key's **class** is written into every checkpoint so an honest bench
//! log says plainly that it is not evidence-grade. It is a claim, not a
//! proof: whoever signs writes the byte, and a key made on any laptop can
//! be labelled class 2. Only a key confirmed outside the log says who
//! signed (see `verify`).
//!
//! | class | meaning |
//! |---|---|
//! | 0 `PublicTest` | a key published in this repository. Anyone can sign with it. Tests only. |
//! | 1 `DevelopmentSoftware` | a key generated on a laptop and built into the firmware image (so it is in the unit's flash, readable by anyone holding the unit or the image). Not evidence-grade. |
//! | 2 `HardwareEfuse` | generated on the chip, held in eFuse by the signing peripheral, never readable by software. Needs eFuse burning: owner's per-unit go-ahead. |
//! | 3 `SecureElement` | generated inside a separate secure-element chip on the hub and never leaves it (planned; the chip is not chosen yet). |
//!
//! **Algorithm.** A checkpoint of record format version 1 is ECDSA P-256
//! with SHA-256, a 65-byte SEC1 key and a 64-byte r || s signature, by
//! definition: the record version is the algorithm tag ([`Algorithm`]). A
//! signer of any other algorithm is refused by the writer rather than
//! written as version 1; adding one means a new checkpoint version with its
//! own layout, which old logs never carry, so they keep verifying as they
//! are. A checker that meets a record version it does not know says so (it
//! never guesses).
//!
//! **Low-S.** The writer, not the signer, puts every signature in the low-S
//! form the checker requires ([`normalize_low_s`]), so a signer that returns
//! raw r || s (a secure element) needs no care of its own.

/// See the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyClass {
    PublicTest = 0,
    DevelopmentSoftware = 1,
    HardwareEfuse = 2,
    SecureElement = 3,
}

impl KeyClass {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::PublicTest,
            1 => Self::DevelopmentSoftware,
            2 => Self::HardwareEfuse,
            3 => Self::SecureElement,
            _ => return None,
        })
    }

    /// The note written into every checkpoint signed with this class.
    pub fn note(self) -> &'static str {
        match self {
            Self::PublicTest => "PUBLIC TEST KEY - ANYONE CAN SIGN WITH IT - NOT EVIDENCE",
            Self::DevelopmentSoftware => "DEVELOPMENT KEY, NOT EVIDENCE-GRADE (software key in flash)",
            Self::HardwareEfuse => "hardware key (eFuse, signing peripheral)",
            Self::SecureElement => "hardware key (secure element chip)",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::PublicTest => "public_test",
            Self::DevelopmentSoftware => "development_software",
            Self::HardwareEfuse => "hardware_efuse",
            Self::SecureElement => "secure_element",
        }
    }

    /// Only a hardware key is evidence-grade.
    pub fn evidence_grade(self) -> bool {
        matches!(self, Self::HardwareEfuse | Self::SecureElement)
    }
}

/// The signature algorithm of a checkpoint. In format version 1 there is
/// one, and the record version names it (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    /// ECDSA over NIST P-256 with SHA-256; SEC1 uncompressed key (65
    /// bytes), r || s big-endian (64 bytes), low-S.
    EcdsaP256Sha256,
    /// Anything else (a future signer). Not writable as format version 1.
    Other(u8),
}

impl Algorithm {
    /// The algorithm a checkpoint of record format `version` is signed with.
    pub fn from_record_version(version: u8) -> Option<Self> {
        (version == 1).then_some(Self::EcdsaP256Sha256)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::EcdsaP256Sha256 => "ecdsa-p256-sha256",
            Self::Other(_) => "other",
        }
    }
}

/// The P-256 group order n, big-endian.
const P256_N: [u8; 32] = [
    0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xBC, 0xE6, 0xFA, 0xAD,
    0xA7, 0x17, 0x9E, 0x84, 0xF3, 0xB9, 0xCA, 0xC2, 0xFC, 0x63, 0x25, 0x51,
];
/// floor(n / 2), big-endian: s above this is the high form.
const P256_HALF_N: [u8; 32] = [
    0x7F, 0xFF, 0xFF, 0xFF, 0x80, 0x00, 0x00, 0x00, 0x7F, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xDE, 0x73, 0x7D, 0x56,
    0xD3, 0x8B, 0xCF, 0x42, 0x79, 0xDC, 0xE5, 0x61, 0x7E, 0x31, 0x92, 0xA8,
];

/// s of a P-256 signature (r || s) is above n/2. Plain byte arithmetic,
/// no curve library: the hub writer uses it for any signer.
pub fn s_is_high(sig: &[u8; 64]) -> bool {
    sig[32..] > P256_HALF_N[..]
}

/// Replace s by n - s (turns a low-S signature into its high-S twin and
/// back). For s in 1..n.
pub fn negate_s(sig: &mut [u8; 64]) {
    let mut borrow = 0i16;
    for i in (0..32).rev() {
        let d = i16::from(P256_N[i]) - i16::from(sig[32 + i]) - borrow;
        borrow = i16::from(d < 0);
        sig[32 + i] = d.rem_euclid(256) as u8;
    }
}

/// Put a P-256 signature in the low-S form (s <= n/2), the only one the
/// checker accepts. A signature already low is left alone.
pub fn normalize_low_s(sig: &mut [u8; 64]) {
    if s_is_high(sig) {
        negate_s(sig);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignError {
    /// The signer is not ready (hardware busy, key missing).
    Unavailable,
}

/// Signs the SHA-256 digest of a checkpoint. A hardware signer (P4 ECDSA
/// peripheral) takes the digest exactly like this, so it fits the same
/// trait.
///
/// The hub calls a signer only through this trait (`firmware/hub-p4`
/// `evlog.rs`), so a secure element replaces the software key without a
/// format change: same algorithm, its own [`KeyClass`], and raw r || s is
/// fine (the writer normalises s).
pub trait Signer {
    fn key_class(&self) -> KeyClass;
    /// SEC1 uncompressed: 0x04 || x || y.
    fn public_key(&self) -> [u8; 65];
    /// What [`sign_digest`](Self::sign_digest) computes. The writer refuses
    /// anything but [`Algorithm::EcdsaP256Sha256`] for format version 1.
    fn algorithm(&self) -> Algorithm {
        Algorithm::EcdsaP256Sha256
    }
    /// ECDSA P-256 over the 32-byte SHA-256 digest; r || s, big-endian.
    /// Either form of s.
    fn sign_digest(&mut self, digest: &[u8; 32]) -> Result<[u8; 64], SignError>;
}

/// The published test key's secret scalar: SHA-256 of a fixed phrase. It is
/// in this source file, so a signature with it proves nothing.
pub fn public_test_secret() -> [u8; 32] {
    crate::sha256(b"rotorscope evidence public test key - not secret - do not trust")
}

#[cfg(any(feature = "soft-signer", feature = "verify"))]
pub use soft::*;

#[cfg(any(feature = "soft-signer", feature = "verify"))]
mod soft {
    use super::*;
    use p256::ecdsa::signature::hazmat::{PrehashSigner, PrehashVerifier};
    use p256::ecdsa::{Signature, SigningKey, VerifyingKey};

    /// A software key (class 0 or 1). RFC 6979 deterministic nonces, so no
    /// random number generator is needed to sign.
    pub struct SoftSigner {
        key: SigningKey,
        class: KeyClass,
        public: [u8; 65],
    }

    impl SoftSigner {
        /// `None` if `secret` is not a valid P-256 scalar (zero or >= n).
        pub fn new(secret: &[u8; 32], class: KeyClass) -> Option<Self> {
            let key = SigningKey::from_bytes(secret.into()).ok()?;
            let point = key.verifying_key().to_encoded_point(false);
            let public = point.as_bytes().try_into().ok()?;
            Some(Self { key, class, public })
        }

        pub fn public_test() -> Self {
            Self::new(&public_test_secret(), KeyClass::PublicTest).unwrap()
        }
    }

    impl Signer for SoftSigner {
        fn key_class(&self) -> KeyClass {
            self.class
        }
        fn public_key(&self) -> [u8; 65] {
            self.public
        }
        fn sign_digest(&mut self, digest: &[u8; 32]) -> Result<[u8; 64], SignError> {
            let sig: Signature = self.key.sign_prehash(digest).map_err(|_| SignError::Unavailable)?;
            // Low-S form: (r, s) and (r, n - s) are both valid ECDSA, so
            // anyone could turn one checkpoint into a second, byte-different
            // valid one. Only the low form is written and accepted.
            Ok(sig.normalize_s().unwrap_or(sig).to_bytes().into())
        }
    }

    /// Check an ECDSA P-256 signature over a SHA-256 digest. Only the low-S
    /// form is accepted (s <= n/2), so a valid signature has exactly one
    /// byte form and two genuine copies of a log cannot be made to differ.
    pub fn verify_digest(pubkey: &[u8; 65], digest: &[u8; 32], sig: &[u8; 64]) -> bool {
        let Ok(vk) = VerifyingKey::from_sec1_bytes(pubkey) else { return false };
        let Ok(sig) = Signature::from_slice(sig) else { return false };
        sig.normalize_s().is_none() && vk.verify_prehash(digest, &sig).is_ok()
    }

    /// The signature is in high-S form (s > n/2): valid ECDSA, but refused
    /// here (see [`verify_digest`]). For a clearer message.
    pub fn is_high_s(sig: &[u8; 64]) -> bool {
        Signature::from_slice(sig).is_ok_and(|s| s.normalize_s().is_some())
    }

    /// The published test key's public half (see [`public_test_secret`]).
    pub fn public_test_key() -> [u8; 65] {
        SoftSigner::public_test().public
    }

    /// The bytes are a valid uncompressed point on P-256.
    pub fn key_is_valid(pubkey: &[u8; 65]) -> bool {
        pubkey[0] == 4 && VerifyingKey::from_sec1_bytes(pubkey).is_ok()
    }

    /// Public key of a secret (for the key generator).
    pub fn public_of(secret: &[u8; 32]) -> Option<[u8; 65]> {
        SoftSigner::new(secret, KeyClass::DevelopmentSoftware).map(|s| s.public)
    }
}
