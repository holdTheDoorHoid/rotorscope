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

// ---- OpenTimestamps (WP-14d): offline, against headers the checker supplies --

const OTS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../crates/evidence/tests/fixtures/ots");

/// The fixed bundle with one OpenTimestamps proof (a fixture file) of the
/// stamped entry.
fn ots_bundle(d: &Path, proof: &str) -> PathBuf {
    let mut b = bundle::build();
    b.bundle.stamping.as_mut().unwrap().ots_calendars = vec!["https://alice.calendar.test".into()];
    let token = std::fs::read(format!("{OTS}/{proof}")).unwrap();
    let mut t = b.stamp(STAMPED_N, "https://alice.calendar.test", &token, "");
    t.kind = "ots".into();
    t.nonce = None;
    b.add_stamp(t);
    let p = d.join("bundle.json");
    std::fs::write(&p, b.json()).unwrap();
    p
}

fn header_line() -> (String, String) {
    let text = std::fs::read_to_string(format!("{OTS}/headers.txt")).unwrap();
    let l = text.lines().find(|l| !l.starts_with('#') && !l.trim().is_empty()).unwrap();
    let (h, x) = l.split_once(' ').unwrap();
    (h.to_string(), x.trim().to_string())
}

#[test]
fn an_opentimestamps_proof_is_checked_offline_against_a_supplied_header() {
    let d = dir("ots-ok");
    let b = ots_bundle(&d, "complete.ots");
    let out = d.join("stamps");
    let (code, text) = run(&["--allow-test-key", "--btc-headers", &format!("{OTS}/headers.txt"), "--check-timestamps", s(&out), s(&b)], Some(""));
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("existed by about") && text.contains("Bitcoin block 905000") && text.contains("block hash ") && text.contains("compare the block hash"), "{text}");
    assert!(out.join("stamp-0.ots").exists(), "{text}");
    eprintln!("{text}");
    assert_eq!(std::fs::read(out.join("stamp-0.ots")).unwrap(), std::fs::read(format!("{OTS}/complete.ots")).unwrap());
    // The same header on the command line.
    let (h, x) = header_line();
    let (code, text) = run(&["--allow-test-key", "--btc-header", &format!("{h}:{x}"), s(&b)], Some(""));
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("existed by about"), "{text}");
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_pending_or_unchecked_opentimestamps_proof_is_said_to_be_so() {
    let d = dir("ots-pending");
    let b = ots_bundle(&d, "pending.ots");
    let (code, text) = run(&["--allow-test-key", s(&b)], Some(""));
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("NOT YET ANCHORED") && !text.contains("existed by about"), "{text}");
    eprintln!("{text}");
    let b = ots_bundle(&d, "complete.ots");
    let (_, text) = run(&["--allow-test-key", s(&b)], Some(""));
    assert!(text.contains("NOT CHECKED, no header for block 905000"), "{text}");
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_bad_or_wrong_header_fails_and_bad_arguments_do_not_run() {
    let d = dir("ots-bad");
    let b = ots_bundle(&d, "complete.ots");
    let (h, x) = header_line();
    // The nonce changed: the header no longer meets its target.
    let mut bytes = verify_unhex(&x);
    bytes[76] ^= 1;
    let bad = format!("{h}:{}", evidence::verify::hex(&bytes));
    let (code, text) = run(&["--allow-test-key", "--btc-header", &bad, s(&b)], Some(""));
    assert_eq!(code, 1, "{text}");
    assert!(text.contains("is refused"), "{text}");
    // Bitcoin's genesis header supplied as block 905000: valid work, wrong root.
    let genesis = "0100000000000000000000000000000000000000000000000000000000000000000000003ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a29ab5f49ffff001d1dac2b7c";
    let (code, text) = run(&["--allow-test-key", "--btc-header", &format!("{h}:{genesis}"), s(&b)], Some(""));
    assert_eq!(code, 1, "{text}");
    assert!(text.contains("does not lead to the Merkle root"), "{text}");
    eprintln!("{text}");
    for bad_arg in [vec!["--btc-header", "905000:00"], vec!["--btc-header", "nope"], vec!["--btc-headers", "/nonexistent/headers.txt"]] {
        let mut a: Vec<&str> = bad_arg.clone();
        a.push(s(&b));
        let (code, text) = run(&a, Some(""));
        assert_eq!(code, 2, "{bad_arg:?}: {text}");
    }
    std::fs::write(d.join("x.bin"), b"").unwrap();
    let (code, _) = run(&["--btc-header", &format!("{h}:{x}"), s(&d.join("x.bin"))], Some(""));
    assert_eq!(code, 2, "headers go with a bundle");
    let _ = std::fs::remove_dir_all(d);
}

fn verify_unhex(x: &str) -> Vec<u8> {
    evidence::verify::unhex(x).unwrap()
}
