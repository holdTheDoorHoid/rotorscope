//! End-to-end: logs written to a temporary folder, the tool run on them,
//! exit codes and verdict words checked. No hardware.

use std::path::{Path, PathBuf};
use std::process::Command;

use evidence::format::{self, CheckpointReason, EventCode};
use evidence::sign::{KeyClass, SoftSigner};
use evidence::verify::{bundle, hex};
use evidence::writer::{Chain, Context};
use evidence::Signer;

const BIN: &str = env!("CARGO_BIN_EXE_evidence-verify");
const FW: [u8; 32] = [0xF1; 32];

fn ctx(t: u64) -> Context {
    Context { uptime_ms: t, ..Context::default() }
}

/// One boot: boot event, notes, two checkpoints, the last record signed.
fn log(boot: u8, signer: &mut SoftSigner) -> Vec<u8> {
    let mut c = Chain::new([boot; 8], FW);
    let mut out = Vec::new();
    let mut b = [0u8; format::MAX_RECORD];
    let mut data = vec![signer.key_class() as u8];
    data.extend_from_slice(&signer.public_key());
    let n = c.event(&ctx(1), EventCode::Boot, b"hub-p4 test", &data, &mut b).unwrap();
    out.extend_from_slice(&b[..n]);
    c.commit();
    for i in 0..6u64 {
        let n = if i == 2 || i == 5 {
            c.checkpoint(&ctx(1000 * i + 500), CheckpointReason::Periodic, signer, &mut b).unwrap()
        } else {
            c.event(&ctx(1000 * i + 100), EventCode::Note, b"operator note", &[], &mut b).unwrap()
        };
        out.extend_from_slice(&b[..n]);
        c.commit();
    }
    out
}

fn forger() -> SoftSigner {
    SoftSigner::new(&[0x5A; 32], KeyClass::HardwareEfuse).unwrap()
}

fn unit() -> SoftSigner {
    SoftSigner::new(&[0x11; 32], KeyClass::DevelopmentSoftware).unwrap()
}

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("evverify-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// (exit code, stdout).
fn run(args: &[&str]) -> (i32, String) {
    let o = Command::new(BIN).args(args).output().unwrap();
    (o.status.code().unwrap(), String::from_utf8_lossy(&o.stdout).into_owned())
}

fn p(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[test]
fn forged_log_without_a_key_is_never_verified() {
    let d = dir("forged");
    let mut f = forger();
    std::fs::write(d.join("EV000001.EVL"), log(1, &mut f)).unwrap();

    let (code, out) = run(&[p(&d)]);
    assert_eq!(code, 4, "{out}");
    assert!(out.contains("SELF-CONSISTENT, SIGNER NOT CHECKED: anyone could have made this log"), "{out}");
    assert!(!out.lines().any(|l| l.starts_with("VERIFIED")), "{out}");
    assert!(out.contains("--key HEX or --keys FILE"), "{out}");
    assert!(out.contains("log claims: hardware key"), "{out}");
    assert!(out.contains("signer: NOT CHECKED"), "{out}");
    assert!(out.contains("NOT PROVEN: that the log is complete"), "{out}");

    // The forger's key given from outside: VERIFIED (that is the trust decision).
    let k = hex(&f.public_key());
    let (code, out) = run(&["--key", &k, p(&d)]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("\nVERIFIED: "), "{out}");
    assert!(out.contains("NOT PROVEN: that the log is complete"), "{out}");

    // A keys file naming another unit: FAIL.
    let keys = d.join("keys.txt");
    std::fs::write(&keys, format!("# lab units\n{} unit 1 (bench)\n", hex(&unit().public_key()))).unwrap();
    let (code, out) = run(&["--keys", p(&keys), p(&d)]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("FAILED"), "{out}");

    // A keys file naming the signer: VERIFIED, and the label is shown.
    std::fs::write(&keys, format!("{} unit 1\n\n{k}   forger's unit  # comment\n", hex(&unit().public_key()))).unwrap();
    let (code, out) = run(&["--keys", p(&keys), "--quiet", p(&d)]);
    assert_eq!(code, 0, "{out}");
    let (_, out) = run(&["--keys", p(&keys), p(&d)]);
    assert!(out.contains("signer: checked against forger's unit"), "{out}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn published_test_key_needs_the_opt_in() {
    let d = dir("testkey");
    let mut t = SoftSigner::public_test();
    std::fs::write(d.join("EV000001.EVL"), log(1, &mut t)).unwrap();
    let (code, out) = run(&[p(&d)]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("published test key") && out.contains("anyone can produce such a log"), "{out}");
    let (code, _) = run(&["--key", &hex(&t.public_key()), p(&d)]);
    assert_eq!(code, 1);
    let (code, out) = run(&["--allow-test-key", p(&d)]);
    assert_eq!(code, 0, "{out}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_missing_middle_file_fails_unless_gaps_are_allowed() {
    let d = dir("gap");
    let mut u = unit();
    std::fs::write(d.join("EV000001.EVL"), log(1, &mut u)).unwrap();
    std::fs::write(d.join("EV000003.EVL"), log(3, &mut u)).unwrap();
    let k = hex(&u.public_key());
    let (code, out) = run(&["--key", &k, p(&d)]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("EV000002..EV000002 were not supplied"), "{out}");
    let (code, out) = run(&["--key", &k, "--allow-gaps", p(&d)]);
    assert_eq!(code, 3, "{out}");
    assert!(out.contains("SIGNER CHECKED, WITH GAPS") && out.contains("may have been removed"), "{out}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn bad_arguments_and_key_files_exit_2() {
    let d = dir("args");
    assert_eq!(run(&[]).0, 2);
    assert_eq!(run(&["--key", "abcd", "x"]).0, 2);
    let keys = d.join("keys.txt");
    std::fs::write(&keys, "not-a-key label\n").unwrap();
    assert_eq!(run(&["--keys", p(&keys), p(&d)]).0, 2);
    std::fs::write(&keys, "# only comments\n").unwrap();
    assert_eq!(run(&["--keys", p(&keys), p(&d)]).0, 2);
    let _ = std::fs::remove_dir_all(&d);
}

/// A bundle as a server would export it, built by hand.
fn bundle_json(signer: &mut SoftSigner) -> Vec<u8> {
    let records = log(1, signer);
    let key_id = String::from_utf8(evidence::fingerprint(&signer.public_key()).to_vec()).unwrap();
    let sha = hex(&evidence::sha256(&records));
    let entries = [
        format!(r#"{{"n":0,"utc_ms":1,"actor":"t","action":"register","device":"{key_id}","detail":{{}}}}"#),
        format!(r#"{{"n":1,"utc_ms":2,"actor":"t","action":"upload","device":"{key_id}","detail":{{"batch":1,"sha256":"{sha}"}}}}"#),
    ];
    let mut prev = [0u8; 32];
    let mut audit = Vec::new();
    for e in entries {
        let h = bundle::audit_hash(&prev, &e);
        prev = h;
        audit.push(bundle::AuditLine { entry: e, hash: hex(&h) });
    }
    let audit_json: Vec<String> = audit
        .iter()
        .map(|a| format!(r#"{{"entry":{},"hash":"{}"}}"#, json_str(&a.entry), a.hash))
        .collect();
    format!(
        r#"{{"format":"{}","created_utc_ms":3,"server":"test","device":{{"key_id":"{key_id}","public_key":"{}","name":"bench","registered_utc_ms":1}},"batches":[{{"n":1,"received_utc_ms":2,"sha256":"{sha}","records":"{}"}}],"audit":[{}],"firmware_hashes":["{}"],"timestamps":[]}}"#,
        bundle::FORMAT,
        hex(&signer.public_key()),
        evidence::b64::encode_vec(&records),
        audit_json.join(","),
        hex(&FW),
    )
    .into_bytes()
}

fn json_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

#[test]
fn a_bundle_needs_an_outside_key_too() {
    let d = dir("bundle");
    let mut f = forger();
    let path = d.join("bundle.json");
    std::fs::write(&path, bundle_json(&mut f)).unwrap();
    let (code, out) = run(&[p(&path)]);
    assert_eq!(code, 4, "{out}");
    assert!(out.contains("SELF-CONSISTENT, SIGNER NOT CHECKED"), "{out}");
    let (code, out) = run(&["--key", &hex(&f.public_key()), p(&path)]);
    assert_eq!(code, 0, "{out}");
    let (code, out) = run(&["--key", &hex(&unit().public_key()), p(&path)]);
    assert_eq!(code, 1, "{out}");
    let _ = std::fs::remove_dir_all(&d);
}
