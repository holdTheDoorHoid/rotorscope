//! The signer abstraction (WP-14a): any [`Signer`] (the software key now, a
//! secure element later) goes through the same checkpoint writer, which
//! writes the signature in the one form the checker accepts. A secure
//! element returns raw ECDSA r || s, and about half of those are high-S.

use evidence::format::{self, CheckpointReason, EventCode};
use evidence::sign::{self, Algorithm, KeyClass, SignError, SoftSigner};
use evidence::verify::{self, Input, Level, Options};
use evidence::writer::{BuildError, Chain, Context};
use evidence::Signer;

/// Stands in for a secure element: signs with a software key, then returns
/// the high-S twin of the signature (r, n - s) when the low form would
/// have been written, as raw hardware output may.
struct RawSigner {
    inner: SoftSigner,
    high_seen: u32,
    alg: Algorithm,
}

impl Signer for RawSigner {
    fn key_class(&self) -> KeyClass {
        KeyClass::SecureElement
    }
    fn public_key(&self) -> [u8; 65] {
        self.inner.public_key()
    }
    fn algorithm(&self) -> Algorithm {
        self.alg
    }
    fn sign_digest(&mut self, digest: &[u8; 32]) -> Result<[u8; 64], SignError> {
        let mut sig = self.inner.sign_digest(digest)?;
        sign::negate_s(&mut sig);
        assert!(sign::is_high_s(&sig));
        self.high_seen += 1;
        Ok(sig)
    }
}

fn log<S: Signer>(signer: &mut S) -> Result<Vec<u8>, BuildError> {
    let mut c = Chain::new([7; 8], [0xF1; 32]);
    let mut out = Vec::new();
    let mut b = [0u8; format::MAX_RECORD];
    let mut data = vec![signer.key_class() as u8];
    data.extend_from_slice(&signer.public_key());
    data.extend_from_slice(&[1, 3]);
    let n = c.event(&Context::default(), EventCode::Boot, b"hub-p4 test", &data, &mut b)?;
    out.extend_from_slice(&b[..n]);
    c.commit();
    for i in 1..4u64 {
        let ctx = Context { uptime_ms: i * 60_000, ..Context::default() };
        let n = c.checkpoint(&ctx, CheckpointReason::Periodic, signer, &mut b)?;
        out.extend_from_slice(&b[..n]);
        c.commit();
    }
    Ok(out)
}

#[test]
fn a_signer_that_returns_high_s_still_writes_checkable_checkpoints() {
    let mut s = RawSigner { inner: SoftSigner::new(&[0x33; 32], KeyClass::SecureElement).unwrap(), high_seen: 0, alg: Algorithm::EcdsaP256Sha256 };
    let bytes = log(&mut s).unwrap();
    assert_eq!(s.high_seen, 3);
    let rep = verify::verify(&[Input::new("EV000001.EVL", bytes)], &Options { expect_key: Some(s.public_key()), ..Options::default() });
    let fails: Vec<_> = rep.findings.iter().filter(|f| f.level == Level::Fail).collect();
    assert!(fails.is_empty(), "{fails:?}");
    assert_eq!(rep.exit_code(), 0);
}

#[test]
fn a_signer_of_another_algorithm_is_refused_not_mislabelled() {
    // Format version 1 checkpoints are ECDSA P-256 / SHA-256 by definition:
    // a signer of anything else must not be written as one.
    let mut s = RawSigner { inner: SoftSigner::new(&[0x33; 32], KeyClass::SecureElement).unwrap(), high_seen: 0, alg: Algorithm::Other(9) };
    assert_eq!(log(&mut s).err(), Some(BuildError::Algorithm(Algorithm::Other(9))));
}

#[test]
fn negate_s_round_trips_and_low_s_is_canonical() {
    let mut soft = SoftSigner::new(&[0x44; 32], KeyClass::DevelopmentSoftware).unwrap();
    let pk = soft.public_key();
    for i in 0..20u8 {
        let digest = evidence::sha256(&[i]);
        let low = soft.sign_digest(&digest).unwrap();
        assert!(!sign::is_high_s(&low));
        let mut high = low;
        sign::negate_s(&mut high);
        assert!(sign::is_high_s(&high));
        assert!(!sign::verify_digest(&pk, &digest, &high), "high-S refused");
        let mut back = high;
        sign::normalize_low_s(&mut back);
        assert_eq!(back, low);
        let mut same = low;
        sign::normalize_low_s(&mut same);
        assert_eq!(same, low, "a low-S signature is left alone");
    }
}

#[test]
fn key_classes_are_named() {
    assert_eq!(KeyClass::from_u8(3), Some(KeyClass::SecureElement));
    assert!(KeyClass::SecureElement.note().contains("secure element"));
    assert_eq!(Algorithm::from_record_version(1), Some(Algorithm::EcdsaP256Sha256));
    assert_eq!(Algorithm::EcdsaP256Sha256.name(), "ecdsa-p256-sha256");
}
