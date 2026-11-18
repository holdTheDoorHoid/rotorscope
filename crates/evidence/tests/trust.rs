//! What the checker may claim: VERIFIED only when the signing key came from
//! outside the log; never for the published test key unless a test opts in;
//! a missing file is a failure by default; high-S signatures are refused.

use evidence::format::{self, CheckpointReason, EventCode};
use evidence::sign::{KeyClass, SoftSigner};
use evidence::verify::{self, Input, Level, Options};
use evidence::writer::{Chain, Context};
use evidence::Signer;

const FW: [u8; 32] = [0xF1; 32];

fn ctx(t: u64) -> Context {
    Context { uptime_ms: t, ..Context::default() }
}

/// One boot: boot event, notes, two checkpoints. Events only (no decode).
fn log(boot: u8, signer: &mut SoftSigner) -> Vec<u8> {
    log_text(boot, signer, b"hub-p4 test")
}

/// [`log`] with this boot event text.
fn log_text(boot: u8, signer: &mut SoftSigner, text: &[u8]) -> Vec<u8> {
    let mut c = Chain::new([boot; 8], FW);
    let mut out = Vec::new();
    let mut b = [0u8; format::MAX_RECORD];
    let mut data = vec![signer.key_class() as u8];
    data.extend_from_slice(&signer.public_key());
    let n = c.event(&ctx(1), EventCode::Boot, text, &data, &mut b).unwrap();
    out.extend_from_slice(&b[..n]);
    c.commit();
    for i in 0..6u64 {
        if i == 2 || i == 5 {
            let n = c.checkpoint(&ctx(1000 * i + 500), CheckpointReason::Periodic, signer, &mut b).unwrap();
            out.extend_from_slice(&b[..n]);
            c.commit();
            continue;
        }
        let n = c.event(&ctx(1000 * i + 100), EventCode::Note, b"operator note", &[], &mut b).unwrap();
        out.extend_from_slice(&b[..n]);
        c.commit();
    }
    out
}

/// A forger's key that claims to be a hardware (eFuse) key.
fn forger() -> SoftSigner {
    SoftSigner::new(&[0x5A; 32], KeyClass::HardwareEfuse).unwrap()
}

fn one(bytes: Vec<u8>, opts: &Options) -> verify::Report {
    verify::verify(&[Input::new("EV000001.EVL", bytes)], opts)
}

fn fails(rep: &verify::Report) -> Vec<String> {
    rep.findings.iter().filter(|f| f.level == Level::Fail).map(|f| format!("{}: {}", f.at, f.what)).collect()
}

#[test]
fn forged_log_is_not_verified_without_an_outside_key() {
    let mut f = forger();
    let rep = one(log(1, &mut f), &Options::default());
    assert!(fails(&rep).is_empty(), "{:?}", rep.findings);
    assert_ne!(rep.exit_code(), 0, "a log naming its own key must not be VERIFIED");
    assert_eq!(rep.exit_code(), 4);
    assert!(rep.verdict().starts_with("SELF-CONSISTENT, SIGNER NOT CHECKED"), "{}", rep.verdict());
    assert!(!rep.verdict().starts_with("VERIFIED"));
    // The class byte is shown as a claim, never as a fact.
    assert!(rep.findings.iter().any(|f| f.what.contains("log claims") && f.what.contains("hardware")), "{:?}", rep.findings);
}

#[test]
fn forged_log_verifies_against_the_forgers_key_given_from_outside() {
    // Passing a key is the trust decision; the checker then says the log
    // was signed by that key, nothing more.
    let mut f = forger();
    let rep = one(log(1, &mut f), &Options { expect_key: Some(f.public_key()), ..Options::default() });
    assert_eq!(rep.exit_code(), 0, "{:?}", rep.findings);
    assert!(rep.verdict().starts_with("VERIFIED"));
    // The same through a trusted-keys list.
    let rep = one(log(1, &mut f), &Options { trusted_keys: vec![(f.public_key(), "bench unit 3".into())], ..Options::default() });
    assert_eq!(rep.exit_code(), 0, "{:?}", rep.findings);
    assert_eq!(rep.chains[0].trusted_as.as_deref(), Some("bench unit 3"));
}

#[test]
fn forged_log_fails_against_a_different_outside_key() {
    let mut f = forger();
    let real = SoftSigner::new(&[0x11; 32], KeyClass::DevelopmentSoftware).unwrap();
    let rep = one(log(1, &mut f), &Options { expect_key: Some(real.public_key()), ..Options::default() });
    assert_eq!(rep.exit_code(), 1, "{:?}", rep.findings);
    let rep = one(log(1, &mut f), &Options { trusted_keys: vec![(real.public_key(), "unit 1".into())], ..Options::default() });
    assert_eq!(rep.exit_code(), 1, "{:?}", rep.findings);
    assert!(fails(&rep).iter().any(|s| s.contains("not one of the keys you supplied")), "{:?}", fails(&rep));
}

#[test]
fn published_test_key_is_refused_unless_a_test_opts_in() {
    let mut t = SoftSigner::public_test();
    let bytes = log(1, &mut t);
    let rep = one(bytes.clone(), &Options::default());
    assert_eq!(rep.exit_code(), 1, "{:?}", rep.findings);
    assert!(fails(&rep).iter().any(|s| s.contains("published test key") && s.contains("anyone")), "{:?}", fails(&rep));
    // Even when someone passes it as the outside key.
    let rep = one(bytes.clone(), &Options { expect_key: Some(t.public_key()), ..Options::default() });
    assert_eq!(rep.exit_code(), 1, "{:?}", rep.findings);
    // Only the explicit opt-in accepts it, and still says what it is.
    let rep = one(bytes, &Options { allow_test_key: true, ..Options::default() });
    assert_eq!(rep.exit_code(), 0, "{:?}", rep.findings);
    assert!(rep.findings.iter().any(|f| f.level == Level::Note && f.what.contains("published test key")), "{:?}", rep.findings);
}

#[test]
fn a_missing_middle_file_fails_by_default() {
    let mut s = SoftSigner::new(&[0x11; 32], KeyClass::DevelopmentSoftware).unwrap();
    let files = vec![Input::new("EV000001.EVL", log(1, &mut s)), Input::new("EV000003.EVL", log(3, &mut s))];
    let key = Some(s.public_key());
    let rep = verify::verify(&files, &Options { expect_key: key, ..Options::default() });
    assert_eq!(rep.exit_code(), 1, "{:?}", rep.findings);
    assert!(fails(&rep).iter().any(|s| s.contains("EV000002")), "{:?}", fails(&rep));
    // Allowed on request, and then the verdict says what a gap can hide.
    let rep = verify::verify(&files, &Options { expect_key: key, gaps_allowed: true, ..Options::default() });
    assert_eq!(rep.exit_code(), 3, "{:?}", rep.findings);
    assert!(rep.findings.iter().any(|f| f.level == Level::Gap && f.what.contains("may have been removed")), "{:?}", rep.findings);
    assert!(rep.verdict().contains("removed"), "{}", rep.verdict());
}

/// A boot event naming bench features (as the P4 hub writes it) gives a
/// NOTE, never a failure or a pass of its own; `bench: none` gives none.
#[test]
fn a_bench_build_is_named_in_a_note() {
    let mut s = SoftSigner::new(&[0x11; 32], KeyClass::DevelopmentSoftware).unwrap();
    let key = Some(s.public_key());
    let bench = log_text(1, &mut s, b"hub-p4 0.1.0 abc last reset: power-on; bench: fakedrone,faulttest");
    let rep = one(bench, &Options { expect_key: key, ..Options::default() });
    assert_eq!(rep.exit_code(), 0, "{:?}", rep.findings);
    assert!(
        rep.findings.iter().any(|f| f.level == Level::Note && f.what.contains("BENCH build") && f.what.contains("fakedrone,faulttest")),
        "{:?}",
        rep.findings
    );
    let normal = log_text(1, &mut s, b"hub-p4 0.1.0 abc last reset: power-on; bench: none");
    let rep = one(normal, &Options { expect_key: key, ..Options::default() });
    assert!(!rep.findings.iter().any(|f| f.what.contains("BENCH")), "{:?}", rep.findings);
    assert_eq!(verify::boot_bench_list(b"hub-p4 test"), None);
    // The text as the P4 hub writes it since the audio note (WP-07).
    assert_eq!(verify::boot_bench_list(b"hub-p4 0.1.0  last reset: power_on; bench: none; audio clips off at boot"), None);
    assert_eq!(
        verify::boot_bench_list(b"hub-p4 0.1.0  last reset: watchdog; bench: faulttest; audio clips off at boot"),
        Some("faulttest")
    );
}

#[test]
fn every_verdict_says_completeness_is_not_proven() {
    let mut s = SoftSigner::new(&[0x11; 32], KeyClass::DevelopmentSoftware).unwrap();
    let rep = one(log(1, &mut s), &Options { expect_key: Some(s.public_key()), ..Options::default() });
    assert_eq!(rep.exit_code(), 0);
    assert!(verify::NOT_PROVEN.contains("complete"));
}

/// P-256 group order, big-endian.
const N: [u8; 32] = [
    0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xBC, 0xE6, 0xFA, 0xAD, 0xA7, 0x17, 0x9E, 0x84, 0xF3, 0xB9, 0xCA, 0xC2, 0xFC, 0x63,
    0x25, 0x51,
];

/// n - s, big-endian.
fn negate(s: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let mut borrow = 0i16;
    for i in (0..32).rev() {
        let mut d = i16::from(N[i]) - i16::from(s[i]) - borrow;
        borrow = 0;
        if d < 0 {
            d += 256;
            borrow = 1;
        }
        out[i] = d as u8;
    }
    out
}

fn is_high(s: &[u8]) -> bool {
    // s > n/2  <=>  s > n - s
    s > &negate(s)[..]
}

#[test]
fn signatures_are_low_s() {
    let mut s = SoftSigner::new(&[0x11; 32], KeyClass::DevelopmentSoftware).unwrap();
    for i in 0..64u8 {
        let sig = s.sign_digest(&[i; 32]).unwrap();
        assert!(!is_high(&sig[32..]), "digest {i}: high-S signature");
        assert!(evidence::sign::verify_digest(&s.public_key(), &[i; 32], &sig));
        // The other form of the same signature is refused.
        let mut alt = sig;
        alt[32..].copy_from_slice(&negate(&sig[32..]));
        assert!(!evidence::sign::verify_digest(&s.public_key(), &[i; 32], &alt), "digest {i}: high-S accepted");
    }
}

#[test]
fn a_checkpoint_flipped_to_high_s_fails() {
    let mut s = SoftSigner::new(&[0x11; 32], KeyClass::DevelopmentSoftware).unwrap();
    let mut bytes = log(1, &mut s);
    // The last record is a checkpoint: flip its s to n - s (still a valid
    // ECDSA signature to a lax checker).
    let end = bytes.len() - format::TRAILER;
    let s_at = end - 32;
    let flipped = negate(&bytes[s_at..end]);
    bytes[s_at..end].copy_from_slice(&flipped);
    let rep = one(bytes, &Options { expect_key: Some(s.public_key()), ..Options::default() });
    assert_eq!(rep.exit_code(), 1, "{:?}", rep.findings);
    assert!(fails(&rep).iter().any(|f| f.contains("record #6") && f.contains("signature")), "{:?}", fails(&rep));
}
