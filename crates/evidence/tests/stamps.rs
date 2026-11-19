//! Outside time-stamps in export bundles (WP-14c): the access-log imprint,
//! recomputed from the bundle; canned RFC 3161 tokens from a throwaway test
//! authority (tests/fixtures/tsa); the minimal DER reader against hostile
//! input. Nothing here touches the network.

mod common;

use common::bundle::{self, STAMPED_N};
use evidence::verify::{self, Level, Options};

fn opts() -> Options {
    Options { allow_test_key: true, ..Options::default() }
}

#[test]
#[ignore = "prints the imprint the fixture tokens must stamp (see tests/fixtures/tsa/README.md)"]
fn print_fixture_imprint() {
    println!("IMPRINT {}", verify::hex(&bundle::build().imprint(STAMPED_N)));
}

const FIX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/tsa");

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{FIX}/{name}")).unwrap()
}

const NONCE_A: &str = "1122334455667788";
const NONCE_B: &str = "0102030405060708";

fn fails(r: &verify::Report) -> Vec<String> {
    r.findings.iter().filter(|f| f.level == Level::Fail).map(|f| format!("{}: {}", f.at, f.what)).collect()
}

fn notes(r: &verify::Report) -> Vec<String> {
    r.findings.iter().filter(|f| f.level == Level::Note).map(|f| f.what.clone()).collect()
}

/// The fixed bundle with both canned stamps (and their audit entries).
fn stamped() -> bundle::Built {
    let mut b = bundle::build();
    let imprint = std::fs::read_to_string(format!("{FIX}/imprint.hex")).unwrap();
    assert_eq!(
        verify::hex(&b.imprint(STAMPED_N)),
        imprint.trim(),
        "the fixed test bundle no longer yields the imprint the canned tokens stamp: remake them (tests/fixtures/tsa/README.md)"
    );
    let ta = b.stamp(STAMPED_N, "https://tsa-a.test/tsr", &fixture("tsa-a/bundle.tsr"), NONCE_A);
    let tb = b.stamp(STAMPED_N, "https://tsa-b.test/tsr", &fixture("tsa-b/bundle.tsr"), NONCE_B);
    b.add_stamp(ta);
    b.add_stamp(tb);
    b
}

#[test]
fn the_imprint_is_stable() {
    // Changing how the imprint is formed breaks every stored stamp.
    let h = [0x11u8; 32];
    assert_eq!(
        verify::hex(&verify::bundle::audit_imprint("abcd-ef01-2345-6789", 7, &h)),
        verify::hex(&evidence::sha256(&[b"rotorscope audit head v1".as_slice(), &[19], b"abcd-ef01-2345-6789", &7u64.to_le_bytes(), &h].concat()))
    );
    assert_ne!(verify::bundle::audit_imprint("a", 7, &h), verify::bundle::audit_imprint("a", 8, &h));
    assert_ne!(verify::bundle::audit_imprint("a", 7, &h), verify::bundle::audit_imprint("b", 7, &h));
}

#[test]
fn a_stamped_bundle_verifies_and_says_what_the_stamp_shows() {
    let b = stamped();
    let r = verify::verify_bundle(&b.json(), &opts());
    assert!(fails(&r).is_empty(), "{:?}", fails(&r));
    assert_eq!(r.exit_code(), 0, "{:?}", r.findings);
    let n = notes(&r);
    let stamps: Vec<&String> = n.iter().filter(|n| n.contains("existed by")).collect();
    assert_eq!(stamps.len(), 2, "{n:?}");
    for s in stamps {
        assert!(s.contains("the access log through entry #2"), "{s}");
        assert!(s.contains("2 uploaded batches"), "{s}");
        assert!(s.contains("after the newest upload it covers was stored"), "{s}");
        assert!(s.contains("the newest GPS time"), "{s}");
        assert!(s.contains("does not show when the records in it were written"), "{s}");
        assert!(s.contains("--check-timestamps"), "{s}");
    }
    assert!(!n.iter().any(|n| n.contains("independent service")), "{n:?}");
}

#[test]
fn a_stamp_naming_a_missing_or_edited_audit_entry_fails() {
    // The access log cut back to before the stamped entry.
    let mut b = stamped();
    b.bundle.audit.truncate(2);
    let r = verify::verify_bundle(&b.json(), &opts());
    assert!(fails(&r).iter().any(|f| f.contains("audit entry #2, which is not in the bundle")), "{:?}", fails(&r));
    // A stamp claiming another entry than the one it was made for.
    let mut b = stamped();
    b.bundle.timestamps[0].audit_n = Some(1);
    let r = verify::verify_bundle(&b.json(), &opts());
    assert!(fails(&r).iter().any(|f| f.contains("is not the one that was time-stamped")), "{:?}", fails(&r));
    // The stamped entry rewritten (and the chain re-hashed from there on, so
    // only the stamp can tell).
    let mut b = bundle::build();
    let ta = b.stamp(STAMPED_N, "https://tsa-a.test/tsr", &fixture("tsa-a/bundle.tsr"), NONCE_A);
    b.bundle.audit.truncate(2);
    b.hashes.truncate(2);
    let batch = &b.bundle.batches[1];
    let detail = serde_json::json!({ "batch": batch.n, "sha256": batch.sha256, "bytes": 1 });
    b.push("someone else", "upload", detail, bundle::UPLOAD_MS[1]);
    b.add_stamp(ta);
    let r = verify::verify_bundle(&b.json(), &opts());
    assert!(fails(&r).iter().any(|f| f.contains("is not the one that was time-stamped")), "{:?}", fails(&r));
    // The imprint field edited to match nothing.
    let mut b = stamped();
    b.bundle.timestamps[1].imprint = verify::hex(&[7; 32]);
    let r = verify::verify_bundle(&b.json(), &opts());
    assert!(fails(&r).iter().any(|f| f.contains("imprint does not match audit entry #2")), "{:?}", fails(&r));
}

#[test]
fn a_token_over_another_hash_fails() {
    let mut b = bundle::build();
    let t = b.stamp(STAMPED_N, "https://tsa-a.test/tsr", &fixture("tsa-a/other.tsr"), NONCE_A);
    b.add_stamp(t);
    let r = verify::verify_bundle(&b.json(), &opts());
    assert!(fails(&r).iter().any(|f| f.contains("not over the imprint stored with it")), "{:?}", fails(&r));
    // A token edited where it holds the imprint.
    let mut tok = fixture("tsa-a/bundle.tsr");
    let imprint = evidence::verify::unhex(std::fs::read_to_string(format!("{FIX}/imprint.hex")).unwrap().trim()).unwrap();
    let at = tok.windows(32).position(|w| w == imprint).unwrap();
    tok[at] ^= 1;
    let mut b = bundle::build();
    let t = b.stamp(STAMPED_N, "https://tsa-a.test/tsr", &tok, NONCE_A);
    b.add_stamp(t);
    let r = verify::verify_bundle(&b.json(), &opts());
    assert!(fails(&r).iter().any(|f| f.contains("not over the imprint stored with it")), "{:?}", fails(&r));
}

#[test]
fn a_stamp_only_failure_says_so_in_the_verdict() {
    let mut b = bundle::build();
    let t = b.stamp(STAMPED_N, "https://tsa-a.test/tsr", &fixture("tsa-a/other.tsr"), NONCE_A);
    b.add_stamp(t);
    // Records and signatures check against the supplied key; only the stamp fails.
    let key = evidence::sign::public_test_key();
    let with_key = Options { expect_key: Some(key), ..opts() };
    let r = verify::verify_bundle(&b.json(), &with_key);
    assert_eq!(r.exit_code(), 1, "still a failure");
    let v = r.verdict();
    assert!(v.starts_with("FAILED (TIME-STAMP): the log's own records and signatures check out against a key you supplied; but a time-stamp did not check out: time-stamp 0 (https://tsa-a.test/tsr)."), "{v}");
    assert!(!v.contains("not what its signer wrote"), "{v}");
    // Signer not checked (no outside key matched): it says so.
    let mut unchecked = verify::Report::default();
    unchecked.findings.push(verify::Finding { level: Level::Fail, at: verify::Where { source: "time-stamp 3 (https://cal.test)".into(), ..Default::default() }, what: "x".into() });
    let v = unchecked.verdict();
    assert!(v.starts_with("FAILED (TIME-STAMP): the log's own records link and are signed consistently, but the signer was not checked") && v.contains("time-stamp 3 (https://cal.test)"), "{v}");
    // A record source that merely starts with the words is not a stamp.
    unchecked.findings[0].at.source = "time-stamp notes.bin".into();
    assert!(unchecked.verdict().starts_with("FAILED: the log is not what its signer wrote"));
    // A record failure as well: the old sentence.
    b.bundle.batches[0].sha256 = "00".repeat(32);
    let r = verify::verify_bundle(&b.json(), &with_key);
    assert_eq!(r.exit_code(), 1);
    assert!(r.verdict().starts_with("FAILED: the log is not what its signer wrote"), "{}", r.verdict());
    // A record failure alone: the old sentence.
    let mut c = bundle::build();
    c.bundle.batches[0].sha256 = "00".repeat(32);
    let r = verify::verify_bundle(&c.json(), &with_key);
    assert!(r.verdict().starts_with("FAILED: the log is not what its signer wrote"), "{}", r.verdict());
}

#[test]
fn a_removed_stamp_shows() {
    let mut b = stamped();
    b.bundle.timestamps.remove(1);
    let r = verify::verify_bundle(&b.json(), &opts());
    assert!(fails(&r).iter().any(|f| f.contains("tsa-b.test") && f.contains("not in the bundle (removed)")), "{:?}", fails(&r));
}

#[test]
fn outstanding_failed_and_single_service_are_reported() {
    let mut b = bundle::build();
    let t = b.stamp(STAMPED_N, "https://tsa-a.test/tsr", &fixture("tsa-a/bundle.tsr"), NONCE_A);
    b.add_stamp(t);
    let st = b.bundle.stamping.as_mut().unwrap();
    st.authorities = vec!["https://tsa-a.test/tsr".into(), "http://TSA-A.test:80/other".into()];
    st.outstanding.push(verify::bundle::Outstanding { authority: "https://tsa-b.test/tsr".into(), audit_n: 2, queued_utc_ms: bundle::UPLOAD_MS[1] });
    for i in 0..3 {
        st.failures.push(verify::bundle::StampFailure { authority: "https://tsa-b.test/tsr".into(), audit_n: 2, utc_ms: bundle::UPLOAD_MS[1] + i * 60_000, reason: format!("timed out {i}") });
    }
    let r = verify::verify_bundle(&b.json(), &opts());
    assert!(fails(&r).is_empty(), "{:?}", fails(&r));
    let n = notes(&r);
    assert!(n.iter().any(|n| n.contains("1 independent witness(es)")), "{n:?}");
    assert!(n.iter().any(|n| n.contains("outstanding since")), "{n:?}");
    assert!(n.iter().any(|n| n.contains("3 attempt(s)") && n.contains("timed out 2")), "{n:?}");
    // OpenTimestamps on (any number of calendars) counts as one more witness.
    b.bundle.stamping.as_mut().unwrap().ots_calendars = vec!["https://cal-a.test".into(), "https://cal-b.test".into()];
    let n = notes(&verify::verify_bundle(&b.json(), &opts()));
    assert!(!n.iter().any(|n| n.contains("independent witness")), "{n:?}");
    assert_eq!(verify::independent_witnesses(&["https://a.test/x".into()], &["https://c.test".into(), "https://d.test".into()]), 2);
    assert_eq!(verify::independent_witnesses(&["https://a.test/x".into(), "http://A.test:80/y".into()], &[]), 1);
}

#[test]
fn version_1_bundles_still_verify() {
    let mut b = bundle::build();
    b.bundle.format = verify::bundle::FORMAT_V1.into();
    b.bundle.stamping = None;
    let mut v: serde_json::Value = serde_json::from_slice(&b.json()).unwrap();
    v.as_object_mut().unwrap().remove("stamping");
    let r = verify::verify_bundle(&serde_json::to_vec(&v).unwrap(), &opts());
    assert!(fails(&r).is_empty(), "{:?}", fails(&r));
    assert_eq!(r.exit_code(), 0);
    // An unknown version is refused, not read as if it were a known one.
    v["format"] = "rotorscope-evidence-bundle/3".into();
    let r = verify::verify_bundle(&serde_json::to_vec(&v).unwrap(), &opts());
    assert!(fails(&r).iter().any(|f| f.contains("unknown bundle format")), "{:?}", fails(&r));
}

// ---- the minimal DER reader --------------------------------------------

use evidence::tsp;

fn imprint() -> [u8; 32] {
    evidence::verify::unhex(std::fs::read_to_string(format!("{FIX}/imprint.hex")).unwrap().trim()).unwrap().try_into().unwrap()
}

#[test]
fn canned_replies_read_and_check() {
    let a = fixture("tsa-a/bundle.tsr");
    let i = tsp::check_reply(&a, &imprint(), &[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]).unwrap();
    assert_eq!(i.hash_alg, tsp::OID_SHA256);
    assert_eq!(i.gen_time.len(), 15, "{:?}", std::str::from_utf8(i.gen_time));
    assert!(i.gen_time_ms > 1_795_000_000_000);
    // Another nonce: a replayed or someone else's token.
    assert_eq!(tsp::check_reply(&a, &imprint(), &[1, 2, 3, 4, 5, 6, 7, 8]).unwrap_err(), tsp::Refused::WrongNonce);
    // Another imprint.
    assert_eq!(tsp::check_reply(&fixture("tsa-a/other.tsr"), &imprint(), &[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]).unwrap_err(), tsp::Refused::WrongImprint);
    // A refusal (PKIStatus 2, no token).
    assert_eq!(tsp::check_reply(&[0x30, 0x05, 0x30, 0x03, 0x02, 0x01, 0x02], &imprint(), &[1]).unwrap_err(), tsp::Refused::Status(2));
    // The bare token reads too (what openssl calls -token_out).
    let t = tsp::parse_response(&a).unwrap().token.unwrap();
    assert_eq!(tsp::tst_info(t).unwrap().imprint, imprint());
}

#[test]
fn malformed_input_is_refused_without_panicking() {
    let cases: &[(&str, &[u8])] = &[
        ("empty", &[]),
        ("one byte", &[0x30]),
        ("indefinite length", &[0x30, 0x80, 0x30, 0x03, 0x02, 0x01, 0x00, 0x00, 0x00]),
        ("length past the end", &[0x30, 0x7F, 0x30]),
        ("huge long-form length", &[0x30, 0x84, 0xFF, 0xFF, 0xFF, 0xFF, 0x30]),
        ("five-byte length", &[0x30, 0x85, 0, 0, 0, 0, 5]),
        ("multi-byte tag", &[0x3F, 0x81, 0x01, 0x00]),
        ("not a sequence", &[0x04, 0x01, 0x00]),
        ("status not an integer", &[0x30, 0x05, 0x30, 0x03, 0x04, 0x01, 0x00]),
        ("status out of range", &[0x30, 0x05, 0x30, 0x03, 0x02, 0x01, 0x09]),
        ("status two bytes", &[0x30, 0x06, 0x30, 0x04, 0x02, 0x02, 0x00, 0x00]),
        ("granted without token", &[0x30, 0x05, 0x30, 0x03, 0x02, 0x01, 0x00]),
        ("trailing bytes", &[0x30, 0x05, 0x30, 0x03, 0x02, 0x01, 0x00, 0xFF]),
        ("token not a ContentInfo", &[0x30, 0x08, 0x30, 0x03, 0x02, 0x01, 0x00, 0x04, 0x01, 0x00]),
        ("ContentInfo of another type", &[0x30, 0x0F, 0x30, 0x03, 0x02, 0x01, 0x00, 0x30, 0x08, 0x06, 0x03, 0x2A, 0x03, 0x04, 0xA0, 0x01, 0x00]),
        ("empty ContentInfo", &[0x30, 0x07, 0x30, 0x03, 0x02, 0x01, 0x00, 0x30, 0x00]),
    ];
    for (what, bytes) in cases {
        assert!(tsp::check_reply(bytes, &imprint(), &[1]).is_err(), "{what}");
        let _ = tsp::tst_info(bytes);
        let _ = tsp::parse_tst_info(bytes);
    }
    // Every truncation of a valid reply.
    let a = fixture("tsa-a/bundle.tsr");
    for n in 0..a.len() {
        assert!(tsp::check_reply(&a[..n], &imprint(), &[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]).is_err(), "cut at {n}");
        let _ = tsp::tst_info(&a[..n]);
    }
}

#[test]
fn thousands_of_mutated_tokens_never_panic() {
    let a = fixture("tsa-a/bundle.tsr");
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let mut accepted = 0;
    for _ in 0..5000 {
        let mut m = a.clone();
        for _ in 0..(next() % 4 + 1) {
            let i = (next() as usize) % m.len();
            match next() % 4 {
                0 => m[i] ^= 1 << (next() % 8),
                1 => m[i] = next() as u8,
                2 => m.truncate(i.max(1)),
                _ => m.insert(i, next() as u8),
            }
        }
        if tsp::check_reply(&m, &imprint(), &[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]).is_ok() {
            // Only a change outside what is read (the signature, the
            // certificates) can still pass this reader: openssl catches it.
            accepted += 1;
        }
        let _ = tsp::tst_info(&m);
    }
    assert!(accepted < 5000);
}

/// A minimal unsigned TimeStampResp over `imprint` (the reader does not
/// check signatures; this stands in for a version 1 bundle's token).
fn unsigned_reply(imprint: &[u8]) -> Vec<u8> {
    fn tlv(tag: u8, c: &[u8]) -> Vec<u8> {
        let mut v = vec![tag];
        if c.len() < 0x80 {
            v.push(c.len() as u8);
        } else {
            v.extend_from_slice(&[0x82, (c.len() >> 8) as u8, c.len() as u8]);
        }
        v.extend_from_slice(c);
        v
    }
    let alg = tlv(0x30, &[tlv(0x06, tsp::OID_SHA256), vec![5, 0]].concat());
    let tst = tlv(0x30, &[tlv(2, &[1]), tlv(6, &[0x2A, 3]), tlv(0x30, &[alg, tlv(4, imprint)].concat()), tlv(2, &[9]), tlv(0x18, b"20251115120000Z")].concat());
    let eci = tlv(0x30, &[tlv(6, tsp::OID_TST_INFO), tlv(0xA0, &tlv(4, &tst))].concat());
    let sd = tlv(0x30, &[tlv(2, &[3]), tlv(0x31, &[]), eci, tlv(0x31, &[])].concat());
    tlv(0x30, &[tlv(0x30, &tlv(2, &[0])), tlv(0x30, &[tlv(6, tsp::OID_SIGNED_DATA), tlv(0xA0, &sd)].concat())].concat())
}

#[test]
fn a_version_1_chain_head_stamp_still_reads() {
    let b = bundle::build();
    let recs = evidence::b64::decode_vec(&b.bundle.batches[1].records).unwrap();
    let last = evidence::format::records(&recs).filter_map(|(_, r)| r.ok()).last().unwrap();
    let (boot, seq, head) = (last.header.boot_id, last.header.seq, last.hash());
    let imprint = verify::bundle::head_imprint(&boot, seq, &head);
    // As a version 1 server wrote it: no "covers", no audit fields.
    let mut v: serde_json::Value = serde_json::from_slice(&b.json()).unwrap();
    v["format"] = verify::bundle::FORMAT_V1.into();
    v.as_object_mut().unwrap().remove("stamping");
    v["timestamps"] = serde_json::json!([{ "kind": "rfc3161", "authority": "http://timestamp.example", "imprint": verify::hex(&imprint), "boot": verify::hex(&boot), "seq": seq, "head": verify::hex(&head), "token": evidence::b64::encode_vec(&unsigned_reply(&imprint)) }]);
    let r = verify::verify_bundle(&serde_json::to_vec(&v).unwrap(), &opts());
    assert!(fails(&r).is_empty(), "{:?}", fails(&r));
    assert!(notes(&r).iter().any(|n| n.contains(&format!("records #0..#{seq} existed by 2025-11-15T12:00:00"))), "{:?}", notes(&r));
    // Its head edited: refused.
    v["timestamps"][0]["head"] = verify::hex(&[1; 32]).into();
    let r = verify::verify_bundle(&serde_json::to_vec(&v).unwrap(), &opts());
    assert!(fails(&r).iter().any(|f| f.contains("does not match the chain head")), "{:?}", fails(&r));
}
