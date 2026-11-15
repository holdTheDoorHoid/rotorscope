//! Who signs checkpoints, and how sure we can be it was this unit.
//!
//! Every checkpoint is signed with ECDSA over NIST P-256 with SHA-256
//! (`ecdsa-with-SHA256`), the curve the ESP32-P4's ECDSA peripheral and the
//! S3/C5 digital-signature path can use, so a hardware signer can replace
//! the software one without changing the format. The signer is behind the
//! [`Signer`] trait; the hub picks one at build time.
//!
//! The key's **class** is written into every checkpoint so nobody can
//! mistake a bench log for evidence-grade output:
//!
//! | class | meaning |
//! |---|---|
//! | 0 `PublicTest` | a key published in this repository. Anyone can sign with it. Tests only. |
//! | 1 `DevelopmentSoftware` | a key generated on a laptop and built into the firmware image (so it is in the unit's flash, readable by anyone holding the unit or the image). Not evidence-grade. |
//! | 2 `HardwareEfuse` | generated on the chip, held in eFuse by the signing peripheral, never readable by software. Needs eFuse burning: owner's per-unit go-ahead. |

/// See the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyClass {
    PublicTest = 0,
    DevelopmentSoftware = 1,
    HardwareEfuse = 2,
}

impl KeyClass {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::PublicTest,
            1 => Self::DevelopmentSoftware,
            2 => Self::HardwareEfuse,
            _ => return None,
        })
    }

    /// The note written into every checkpoint signed with this class.
    pub fn note(self) -> &'static str {
        match self {
            Self::PublicTest => "PUBLIC TEST KEY - ANYONE CAN SIGN WITH IT - NOT EVIDENCE",
            Self::DevelopmentSoftware => "DEVELOPMENT KEY, NOT EVIDENCE-GRADE (software key in flash)",
            Self::HardwareEfuse => "hardware key (eFuse, signing peripheral)",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::PublicTest => "public_test",
            Self::DevelopmentSoftware => "development_software",
            Self::HardwareEfuse => "hardware_efuse",
        }
    }

    /// Only a hardware key is evidence-grade.
    pub fn evidence_grade(self) -> bool {
        self == Self::HardwareEfuse
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
pub trait Signer {
    fn key_class(&self) -> KeyClass;
    /// SEC1 uncompressed: 0x04 || x || y.
    fn public_key(&self) -> [u8; 65];
    /// ECDSA P-256 over the 32-byte SHA-256 digest; r || s, big-endian.
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
            Ok(sig.to_bytes().into())
        }
    }

    /// Check an ECDSA P-256 signature over a SHA-256 digest.
    pub fn verify_digest(pubkey: &[u8; 65], digest: &[u8; 32], sig: &[u8; 64]) -> bool {
        let Ok(vk) = VerifyingKey::from_sec1_bytes(pubkey) else { return false };
        let Ok(sig) = Signature::from_slice(sig) else { return false };
        vk.verify_prehash(digest, &sig).is_ok()
    }

    /// Public key of a secret (for the key generator).
    pub fn public_of(secret: &[u8; 32]) -> Option<[u8; 65]> {
        SoftSigner::new(secret, KeyClass::DevelopmentSoftware).map(|s| s.public)
    }
}
