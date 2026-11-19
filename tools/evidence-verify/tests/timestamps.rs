//! `--check-timestamps`: each RFC 3161 token in a bundle checked with the
//! system's `openssl ts -verify` against the authority's root, or the
//! command printed when openssl is not installed. Canned tokens from two
//! throwaway test authorities (crates/evidence/tests/fixtures/tsa); no
//! network.

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "../../../crates/evidence/tests/common/mod.rs"]
mod common;

use common::bundle::{self, STAMPED_N};

const BIN: &str = env!("CARGO_BIN_EXE_evidence-verify");
const FIX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../crates/evidence/tests/fixtures/tsa");

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{FIX}/{name}")).unwrap()
}

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("evverify-ts-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn have_openssl() -> bool {
    Command::new("openssl").arg("version").output().is_ok_and(|o| o.status.success())
}

/// The fixed bundle with both canned stamps; `edit` may change token a.
fn write_bundle(d: &Path, edit: impl Fn(&mut Vec<u8>)) -> PathBuf {
    let mut b = bundle::build();
    let mut ta = fixture("tsa-a/bundle.tsr");
    edit(&mut ta);
    let a = b.stamp(STAMPED_N, "https://tsa-a.test/tsr", &ta, "1122334455667788");
    let bb = b.stamp(STAMPED_N, "https://tsa-b.test/tsr", &fixture("tsa-b/bundle.tsr"), "0102030405060708");
    b.add_stamp(a);
    b.add_stamp(bb);
    let p = d.join("bundle.json");
    std::fs::write(&p, b.json()).unwrap();
    p
}

fn run(args: &[&str], path_env: Option<&str>) -> (i32, String) {
    let mut c = Command::new(BIN);
    c.args(args);
    if let Some(p) = path_env {
        c.env("PATH", p);
    }
    let o = c.output().unwrap();
    (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into_owned() + &String::from_utf8_lossy(&o.stderr))
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

#[test]
fn tokens_check_out_against_the_authorities_roots() {
    if !have_openssl() {
        eprintln!("openssl not installed: skipped");
        return;
    }
    let d = dir("ok");
    let b = write_bundle(&d, |_| {});
    let out = d.join("stamps");
    let (code, text) = run(&["--allow-test-key", "--check-timestamps", s(&out), "--tsa-ca", &format!("{FIX}/tsa-a/ca.pem"), "--tsa-ca", &format!("{FIX}/tsa-b/ca.pem"), s(&b)], None);
    assert_eq!(code, 0, "{text}");
    assert_eq!(text.matches("signature checks out against the roots given with --tsa-ca").count(), 2, "{text}");
    assert!(text.contains("openssl ts -verify -in "), "the command is shown: {text}");
    assert!(out.join("stamp-0.tsr").exists() && out.join("stamp-1.tsr").exists());
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_token_with_a_broken_signature_fails() {
    if !have_openssl() {
        eprintln!("openssl not installed: skipped");
        return;
    }
    let d = dir("sig");
    // The last bytes of the reply are the signature value: the reader still
    // finds the right imprint and nonce, only the signature check can tell.
    let b = write_bundle(&d, |t| {
        let n = t.len();
        t[n - 3] ^= 0x55;
    });
    let out = d.join("stamps");
    let (code, text) = run(&["--allow-test-key", "--check-timestamps", s(&out), "--tsa-ca", &format!("{FIX}/tsa-a/ca.pem"), "--tsa-ca", &format!("{FIX}/tsa-b/ca.pem"), s(&b)], None);
    assert_eq!(code, 1, "{text}");
    assert!(text.contains("FAIL") && text.contains("openssl ts -verify failed"), "{text}");
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_token_from_an_authority_the_checker_does_not_trust_fails() {
    if !have_openssl() {
        eprintln!("openssl not installed: skipped");
        return;
    }
    let d = dir("ca");
    let b = write_bundle(&d, |_| {});
    let out = d.join("stamps");
    let (code, text) = run(&["--allow-test-key", "--check-timestamps", s(&out), "--tsa-ca", &format!("{FIX}/tsa-b/ca.pem"), s(&b)], None);
    assert_eq!(code, 1, "{text}");
    assert!(text.contains("tsa-a.test") && text.contains("openssl ts -verify failed"), "{text}");
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn without_openssl_the_commands_are_printed() {
    let d = dir("noopenssl");
    let b = write_bundle(&d, |_| {});
    let out = d.join("stamps");
    let (code, text) = run(&["--allow-test-key", "--check-timestamps", s(&out), "--tsa-ca", &format!("{FIX}/tsa-a/ca.pem"), s(&b)], Some(""));
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("openssl not found"), "{text}");
    assert_eq!(text.matches("openssl ts -verify -in ").count(), 2, "{text}");
    assert!(out.join("stamp-0.tsr").exists());
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn without_a_root_of_ones_own_it_says_the_check_is_not_from_outside() {
    // No certificates stored with these stamps and no --tsa-ca: nothing to
    // check against.
    let d = dir("noroot");
    let b = write_bundle(&d, |_| {});
    let (code, text) = run(&["--allow-test-key", "--check-timestamps", s(&d.join("stamps")), s(&b)], Some(""));
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("no certificate to check it against"), "{text}");
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn check_timestamps_needs_a_bundle() {
    let d = dir("args");
    std::fs::write(d.join("x.bin"), b"").unwrap();
    let (code, text) = run(&["--check-timestamps", s(&d.join("o")), s(&d.join("x.bin"))], None);
    assert_eq!(code, 2, "{text}");
    let _ = std::fs::remove_dir_all(d);
}
