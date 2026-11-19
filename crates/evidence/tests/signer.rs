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

fn unhex<const N: usize>(s: &str) -> [u8; N] {
    let mut out = [0u8; N];
    assert_eq!(s.len(), 2 * N);
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap();
    }
    out
}

/// The published example of RFC 6979, appendix A.2.5 (P-256, SHA-256,
/// message "sample"): the standard gives the key, the public point and the
/// signature. It does not depend on any version of the curve library, so it
/// holds the signer to the standard across library upgrades: the same key
/// and the same digest must always give these bytes. The standard's s is the
/// high form, so what is written is its low twin, n - s.
#[test]
fn the_rfc_6979_example_signs_to_the_published_bytes() {
    let secret: [u8; 32] = unhex("C9AFA9D845BA75166B5C215767B1D6934E50C3DB36E89B127B8A622B120F6721");
    let ux = "60FED4BA255A9D31C961EB74C6356D68C049B8923B61FA6CE669622E60F29FB6";
    let uy = "7903FE1008B8BC99A41AE9E95628BC64F2F1B20C2D7E9F5177A3C294D4462299";
    let r = "EFD48B2AACB6A8FD1140DD9CD45E81D69D2C877B56AAF991C34D0EA84EAF3716";
    let s_published = "F7CB1C942D657C41D436C7A1B6E29F65F3E900DBB9AFF4064DC4AB2F843ACDA8";
    let s_low = "0834E36AD29A83BF2BC9385E491D6099C8FDF9D1ED67AA7EA5F51F93782857A9";

    let mut soft = SoftSigner::new(&secret, KeyClass::DevelopmentSoftware).unwrap();
    let mut public = [0u8; 65];
    public[0] = 4;
    public[1..33].copy_from_slice(&unhex::<32>(ux));
    public[33..].copy_from_slice(&unhex::<32>(uy));
    assert_eq!(soft.public_key(), public);

    let digest = evidence::sha256(b"sample");
    assert_eq!(digest, unhex::<32>("AF2BDBE1AA9B6EC1E2ADE1D694F41FC71A831D0268E9891562113D8A62ADD1BF"));
    let sig = soft.sign_digest(&digest).unwrap();
    let mut low = [0u8; 64];
    low[..32].copy_from_slice(&unhex::<32>(r));
    low[32..].copy_from_slice(&unhex::<32>(s_low));
    assert_eq!(sig, low, "RFC 6979 nonce, then the low-S form");
    assert!(sign::verify_digest(&public, &digest, &sig));

    // The signature exactly as the standard prints it is valid ECDSA in the
    // high form: recognised as such and refused.
    let mut high = low;
    high[32..].copy_from_slice(&unhex::<32>(s_published));
    assert!(sign::is_high_s(&high) && sign::s_is_high(&high));
    assert!(!sign::verify_digest(&public, &digest, &high));
    let mut twin = low;
    sign::negate_s(&mut twin);
    assert_eq!(twin, high);
}

#[test]
fn key_classes_are_named() {
    assert_eq!(KeyClass::from_u8(3), Some(KeyClass::SecureElement));
    assert!(KeyClass::SecureElement.note().contains("secure element"));
    assert_eq!(Algorithm::from_record_version(1), Some(Algorithm::EcdsaP256Sha256));
    assert_eq!(Algorithm::EcdsaP256Sha256.name(), "ecdsa-p256-sha256");
}
