//! OpenTimestamps proofs (WP-14d): the format against fixtures made
//! independently in Python (tests/fixtures/ots, README.md there), replay to
//! a synthetic Bitcoin block header, the bundle checker's messages, and
//! hostile input through every parser. Nothing here touches the network.

mod common;

use common::bundle::{self, STAMPED_N, UPLOAD_MS};
use evidence::ots::{self, Attestation, BlockHeader, Op, OtsError, Proof, Timestamp};
use evidence::verify::{self, hex, unhex, Level, Options};

const FIX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/ots");

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{FIX}/{name}")).unwrap_or_else(|e| panic!("{FIX}/{name}: {e}"))
}

/// `name value` lines of expected.txt.
fn expected(name: &str) -> String {
    let text = String::from_utf8(fixture("expected.txt")).unwrap();
    text.lines().find_map(|l| l.strip_prefix(name).and_then(|r| r.strip_prefix(' '))).unwrap_or_else(|| panic!("expected.txt has no {name}")).to_string()
}

fn ex(name: &str) -> Vec<u8> {
    unhex(&expected(name)).unwrap()
}

fn header() -> (u64, BlockHeader) {
    let text = String::from_utf8(fixture("headers.txt")).unwrap();
    let l = text.lines().find(|l| !l.starts_with('#') && !l.trim().is_empty()).unwrap();
    let (h, x) = l.split_once(' ').unwrap();
    (h.parse().unwrap(), BlockHeader::from_hex(x).unwrap())
}

// The same fixed values as make-fixtures.py.
fn h(s: &[u8]) -> Vec<u8> {
    evidence::sha256(s).to_vec()
}
fn salt() -> Vec<u8> {
    unhex("00112233445566778899aabbccddeeff").unwrap()
}
const URI: &str = "https://alice.calendar.test";
const HEIGHT: u64 = 500_000;

fn server_ops() -> Vec<Op> {
    vec![Op::Append(salt()), Op::Sha256]
}
fn calendar_ops() -> Vec<Op> {
    vec![Op::Prepend(unhex("5f2a1c00").unwrap()), Op::Append(unhex("a1a2a3a4a5a6a7a8").unwrap()), Op::Sha256]
}
fn upgrade_ops() -> Vec<Op> {
    let tx_prefix = [unhex("0100000001").unwrap(), vec![0x11; 32], unhex("00000000").unwrap(), vec![0x16, 0x6a, 0x14]].concat();
    vec![
        Op::Append(h(b"another submission in the calendar's tree")),
        Op::Sha256,
        Op::Ripemd160,
        Op::Prepend(tx_prefix),
        Op::Append(unhex("ffffffff0100000000000000000000000000").unwrap()),
        Op::Sha256,
        Op::Sha256,
        Op::Append(h(b"a sibling transaction")),
        Op::Sha256,
        Op::Sha256,
        Op::Prepend(h(b"another sibling transaction")),
        Op::Sha256,
        Op::Sha256,
    ]
}

fn imprint() -> [u8; 32] {
    ex("imprint").try_into().unwrap()
}

fn pending_proof() -> Proof {
    let ops = [server_ops(), calendar_ops()].concat();
    Proof { digest: imprint(), timestamp: Timestamp::chain(ops, Attestation::Pending { uri: URI.into() }) }
}

// ---- the format, against the independent fixtures ---------------------------

#[test]
fn the_fixtures_match_the_test_bundle() {
    let tsa = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/tsa/imprint.hex")).unwrap();
    assert_eq!(expected("imprint"), tsa.trim());
    assert_eq!(hex(&bundle::build().imprint(STAMPED_N)), tsa.trim(), "the fixed test bundle changed: remake the fixtures (tests/fixtures/ots/README.md)");
}

#[test]
fn proofs_built_here_are_byte_for_byte_the_independent_ones() {
    assert_eq!(pending_proof().to_bytes(), fixture("pending.ots"), "pending proof");
    let submit = Timestamp::chain(calendar_ops(), Attestation::Pending { uri: URI.into() });
    assert_eq!(submit.to_bytes(), fixture("submit-reply.bin"));
    let upgrade = Timestamp::chain(upgrade_ops(), Attestation::Bitcoin { height: HEIGHT });
    assert_eq!(upgrade.to_bytes(), fixture("upgrade-reply.bin"));
    // The completed proof is the pending one with the calendar's upgrade
    // grafted on at the commitment.
    let mut done = pending_proof();
    let n = done.timestamp.graft(&done.digest.clone(), &ex("commitment"), &upgrade);
    assert_eq!(n, 1);
    assert_eq!(done.to_bytes(), fixture("complete.ots"), "completed proof");
}

#[test]
fn the_fixtures_parse_and_replay_to_what_python_computed() {
    for f in ["pending.ots", "complete.ots"] {
        let b = fixture(f);
        let p = ots::parse_proof(&b).unwrap();
        assert_eq!(p.to_bytes(), b, "{f} round trip");
        assert_eq!(p.digest, imprint());
        let w = p.walk();
        assert!(w.skipped.is_empty(), "{f}: {:?}", w.skipped);
        assert_eq!(w.pending(), vec![(URI.to_string(), ex("commitment"))], "{f}");
    }
    let p = ots::parse_proof(&fixture("complete.ots")).unwrap();
    assert!(p.is_anchored());
    assert!(!ots::parse_proof(&fixture("pending.ots")).unwrap().is_anchored());
    assert_eq!(p.walk().bitcoin(), vec![(HEIGHT, ex("merkle_root"))]);
    let (height, hd) = header();
    assert_eq!(height, HEIGHT);
    assert_eq!(hd.merkle_root().to_vec(), ex("merkle_root"));
    assert_eq!(hd.hash_hex(), expected("block_hash"));
    assert_eq!(hd.time().to_string(), expected("block_time"));
    hd.check_work().unwrap();
    // A calendar reply starts at what was sent.
    let sent = ots::Op::Sha256.apply(&[imprint().as_slice(), &salt()].concat()).unwrap();
    assert_eq!(sent, ex("submitted"));
    let r = ots::parse_timestamp(&fixture("submit-reply.bin")).unwrap();
    assert_eq!(r.walk(&sent).pending()[0].1, ex("commitment"));
}

// ---- the bundle checker -----------------------------------------------------

fn ots_line(b: &bundle::Built, proof: &[u8]) -> verify::bundle::Timestamp {
    verify::bundle::Timestamp {
        kind: verify::bundle::KIND_OTS.into(),
        token: evidence::b64::encode_vec(proof),
        nonce: None,
        authority: "https://alice.calendar.test".into(),
        ..b.stamp(STAMPED_N, "https://alice.calendar.test", proof, "")
    }
}

fn check(proofs: &[Vec<u8>], headers: Vec<(u64, BlockHeader)>) -> verify::Report {
    let mut b = bundle::build();
    b.bundle.stamping.as_mut().unwrap().ots_calendars = vec!["https://alice.calendar.test".into()];
    for p in proofs {
        let t = ots_line(&b, p);
        b.add_stamp(t);
    }
    verify::verify_bundle(&b.json(), &Options { allow_test_key: true, btc_headers: headers, ..Options::default() })
}

fn fails(r: &verify::Report) -> Vec<String> {
    r.findings.iter().filter(|f| f.level == Level::Fail).map(|f| format!("{}: {}", f.at, f.what)).collect()
}

fn notes(r: &verify::Report) -> Vec<String> {
    r.findings.iter().filter(|f| f.level == Level::Note).map(|f| f.what.clone()).collect()
}

#[test]
fn a_pending_proof_is_a_note_never_a_pass() {
    let r = check(&[fixture("pending.ots")], vec![header()]);
    assert!(fails(&r).is_empty(), "{:?}", fails(&r));
    let n = notes(&r);
    assert!(n.iter().any(|x| x.contains("NOT YET ANCHORED") && x.contains("https://alice.calendar.test")), "{n:?}");
    assert!(n.iter().any(|x| x.contains("no readable outside time-stamp")), "a pending proof is not a stamp: {n:?}");
    assert!(!n.iter().any(|x| x.contains("existed by")), "{n:?}");
}

#[test]
fn a_completed_proof_checks_against_the_supplied_header() {
    let r = check(&[fixture("pending.ots"), fixture("complete.ots")], vec![header()]);
    assert!(fails(&r).is_empty(), "{:?}", fails(&r));
    let n = notes(&r);
    let ok = n.iter().find(|x| x.contains("existed by about")).unwrap_or_else(|| panic!("{n:?}"));
    for want in [
        &format!("Bitcoin block {HEIGHT}"),
        &expected("block_hash"),
        &verify::fmt_utc(expected("block_time").parse::<u64>().unwrap() * 1000),
        &"header you supplied".to_string(),
        &"compare the block hash with a source you trust".to_string(),
        &"coarse".to_string(),
        &"after the newest upload it covers was stored".to_string(),
    ] {
        assert!(ok.contains(want.as_str()), "{want:?} missing from {ok}");
    }
    assert!(!n.iter().any(|x| x.contains("no readable outside time-stamp")), "{n:?}");
    // Two independent witnesses would be needed; here only OpenTimestamps
    // (and the bundle's two test RFC 3161 names): the banner counts it once.
    assert!(!n.iter().any(|x| x.contains("independent witness")), "{n:?}");
    let _ = UPLOAD_MS;
}

#[test]
fn without_a_header_a_completed_proof_is_not_checked() {
    let r = check(&[fixture("complete.ots")], vec![]);
    assert!(fails(&r).is_empty(), "{:?}", fails(&r));
    let n = notes(&r);
    assert!(n.iter().any(|x| x.contains("NOT CHECKED") && x.contains(&format!("--btc-header {HEIGHT}:")) && x.contains("bitcoin-cli getblockhash 500000")), "{n:?}");
    assert!(n.iter().any(|x| x.contains("no readable outside time-stamp")), "{n:?}");
}

#[test]
fn a_header_that_does_not_meet_its_own_target_is_refused() {
    let (height, mut hd) = header();
    hd.0[76] ^= 0x01;
    assert!(hd.check_work().is_err());
    let r = check(&[fixture("complete.ots")], vec![(height, hd)]);
    assert!(fails(&r).iter().any(|f| f.contains("is refused") && f.contains("does not meet the target")), "{:?}", fails(&r));
    // A header that meets an easier target than Bitcoin's main chain allows.
    let (_, mut easy) = header();
    easy.0[72..76].copy_from_slice(&0x207f_ffffu32.to_le_bytes());
    let r = check(&[fixture("complete.ots")], vec![(height, easy)]);
    assert!(fails(&r).iter().any(|f| f.contains("easier than Bitcoin's main chain allows")), "{:?}", fails(&r));
}

#[test]
fn an_easy_header_is_refused_at_a_height_with_a_difficulty_floor() {
    // The fixture header (difficulty 1, made on a laptop) for a proof that
    // names block 850,000: refused, whatever its Merkle root.
    let (_, hd) = header();
    let mut p = ots::parse_proof(&fixture("complete.ots")).unwrap();
    p.timestamp = Timestamp::chain([server_ops(), calendar_ops(), upgrade_ops()].concat(), Attestation::Bitcoin { height: 850_000 });
    let r = check(&[p.to_bytes()], vec![(850_000, hd)]);
    let f = fails(&r);
    assert!(f.iter().any(|f| f.contains("block 850000 is refused") && f.contains("below the floor") && f.contains("from height 800000")), "{f:?}");
    // The same header at the height the fixture names (500,000, before the
    // first floor) passes the work check, and the checker says no floor
    // applied that far back.
    let r = check(&[fixture("complete.ots")], vec![header()]);
    assert!(fails(&r).is_empty(), "{:?}", fails(&r));
    let n = notes(&r);
    assert!(n.iter().any(|x| x.contains("existed by about") && x.contains("no height-based floor") && x.contains("compare the block hash")), "{n:?}");
    // At a floored height a header must meet the floor for that height
    // (exactly at the floor: ots::tests::difficulty_floors, on targets).
    assert_eq!(ots::floor_for(599_999), None);
    assert_eq!(ots::floor_for(600_000), Some((600_000, 1_000_000_000_000)));
    assert_eq!(ots::floor_for(2_000_000), Some((800_000, 10_000_000_000_000)));
}

#[test]
fn a_proof_that_does_not_lead_to_the_headers_merkle_root_fails() {
    // A real header with valid work (Bitcoin's genesis block), supplied for
    // the height the proof names: its Merkle root is not the proof's.
    let genesis = BlockHeader::from_hex("0100000000000000000000000000000000000000000000000000000000000000000000003ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a29ab5f49ffff001d1dac2b7c").unwrap();
    genesis.check_work().unwrap();
    let r = check(&[fixture("complete.ots")], vec![(HEIGHT, genesis)]);
    assert!(fails(&r).iter().any(|f| f.contains("does not lead to the Merkle root")), "{:?}", fails(&r));
    // A proof whose path was altered (one sibling changed) no longer leads there.
    let mut p = ots::parse_proof(&fixture("complete.ots")).unwrap();
    let mut ops = [server_ops(), calendar_ops(), upgrade_ops()].concat();
    let k = ops.len() - 3;
    ops[k] = Op::Prepend(h(b"a different sibling"));
    p.timestamp = Timestamp::chain(ops, Attestation::Bitcoin { height: HEIGHT });
    let r = check(&[p.to_bytes()], vec![header()]);
    assert!(fails(&r).iter().any(|f| f.contains("does not lead to the Merkle root")), "{:?}", fails(&r));
}

#[test]
fn a_completed_proof_over_a_different_digest_fails() {
    let mut p = ots::parse_proof(&fixture("complete.ots")).unwrap();
    p.digest[0] ^= 1;
    let r = check(&[p.to_bytes()], vec![header()]);
    assert!(fails(&r).iter().any(|f| f.contains("not over the imprint stored with it")), "{:?}", fails(&r));
    // A proof of the right digest stored for another audit entry: the
    // imprint stored with it does not match that entry.
    let mut b = bundle::build();
    let mut t = ots_line(&b, &fixture("complete.ots"));
    t.audit_n = Some(1);
    t.audit_hash = Some(hex(&b.hashes[1]));
    b.add_stamp(t);
    let r = verify::verify_bundle(&b.json(), &Options { allow_test_key: true, btc_headers: vec![header()], ..Options::default() });
    assert!(fails(&r).iter().any(|f| f.contains("imprint does not match audit entry #1")), "{:?}", fails(&r));
}

#[test]
fn a_removed_proof_shows() {
    let mut b = bundle::build();
    let t = ots_line(&b, &fixture("pending.ots"));
    b.add_stamp(t);
    b.bundle.timestamps.clear();
    let r = verify::verify_bundle(&b.json(), &Options { allow_test_key: true, ..Options::default() });
    assert!(fails(&r).iter().any(|f| f.contains("not in the bundle (removed)")), "{:?}", fails(&r));
}

// ---- hostile input -----------------------------------------------------------

fn xorshift(s: &mut u64) -> u64 {
    *s ^= *s << 13;
    *s ^= *s >> 7;
    *s ^= *s << 17;
    *s
}

#[test]
fn truncated_proofs_and_replies_are_refused() {
    for f in ["pending.ots", "complete.ots"] {
        let b = fixture(f);
        for n in 0..b.len() {
            assert!(ots::parse_proof(&b[..n]).is_err(), "{f} cut at {n}");
        }
        let mut more = b.clone();
        more.push(0);
        assert_eq!(ots::parse_proof(&more), Err(OtsError::Trailing));
    }
    for f in ["submit-reply.bin", "upgrade-reply.bin"] {
        let b = fixture(f);
        for n in 0..b.len() {
            assert!(ots::parse_timestamp(&b[..n]).is_err(), "{f} cut at {n}");
        }
    }
}

#[test]
fn oversized_deep_and_crowded_proofs_are_refused() {
    // Deeper than the cap: 300 nested appends.
    let mut deep = Vec::new();
    for _ in 0..300 {
        deep.extend_from_slice(&[0xf0, 0x01, 0x00]);
    }
    deep.push(0x00);
    deep.extend_from_slice(&ots::BITCOIN_TAG);
    deep.extend_from_slice(&[0x01, 0x00]);
    assert_eq!(ots::parse_timestamp(&deep), Err(OtsError::TooDeep));
    // A million levels: refused, no stack overflow.
    let mut very = vec![0x08u8; 1 << 20];
    very.push(0);
    assert!(ots::parse_timestamp(&very).is_err());
    // Too many items at one node.
    let mut crowd = Vec::new();
    for i in 0..5000u32 {
        crowd.extend_from_slice(&[0xff, 0x00]);
        crowd.extend_from_slice(&[9, 9, 9, 9, 9, 9, 9, 9]);
        crowd.push(4);
        crowd.extend_from_slice(&i.to_le_bytes());
    }
    crowd.extend_from_slice(&[0x00, 0x05, 0x88, 0x96, 0x0d, 0x73, 0xd7, 0x19, 0x01, 0x01, 0x00]);
    assert!(matches!(ots::parse_timestamp(&crowd), Err(OtsError::TooMany | OtsError::TooLong(_))));
    // Larger than any proof kept.
    assert_eq!(ots::parse_timestamp(&vec![0x08; ots::MAX_PROOF + 1]), Err(OtsError::TooLong("proof")));
    // A length that claims more than there is, or more than allowed.
    assert!(ots::parse_timestamp(&[0xf0, 0xff, 0xff, 0xff, 0xff, 0x0f, 0x00]).is_err());
    assert!(ots::parse_timestamp(&[0xf0, 0x81, 0x40, 0x00]).is_err(), "an argument over 4096 bytes");
    assert!(ots::parse_timestamp(&[0xf0, 0x00, 0x00]).is_err(), "an empty argument");
    // A varuint that never ends.
    assert!(ots::parse_timestamp(&[0xf0, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01]).is_err());
    // An op repeated at one node, an unknown op, an attestation after an op.
    assert!(ots::parse_timestamp(&[0xff, 0x08, 0x00, 0x05, 0x88, 0x96, 0x0d, 0x73, 0xd7, 0x19, 0x01, 0x01, 0x00, 0x08, 0x00, 0x05, 0x88, 0x96, 0x0d, 0x73, 0xd7, 0x19, 0x01, 0x01, 0x00]).is_err());
    assert!(ots::parse_timestamp(&[0x55]).is_err());
    // A pending URI with characters a calendar URI may not have.
    let mut bad = vec![0x00];
    bad.extend_from_slice(&ots::PENDING_TAG);
    bad.extend_from_slice(&[0x06, 0x05]);
    bad.extend_from_slice(b"a?b=c");
    assert_eq!(ots::parse_timestamp(&bad), Err(OtsError::BadUri));
    // A message that would grow past 4096 bytes: that branch is not followed.
    let big = Timestamp::chain(vec![Op::Append(vec![1; 4096]), Op::Append(vec![2; 10])], Attestation::Bitcoin { height: 1 });
    let w = big.walk(&[0; 32]);
    assert!(w.reached.is_empty() && !w.skipped.is_empty(), "{w:?}");
    // SHA-1 and Keccak-256 are recognised, not followed.
    let sha1 = Timestamp::chain(vec![Op::Sha1], Attestation::Bitcoin { height: 1 });
    let w = ots::parse_timestamp(&sha1.to_bytes()).unwrap().walk(&[0; 32]);
    assert!(w.reached.is_empty() && w.skipped[0].contains("sha1"), "{w:?}");
}

#[test]
fn random_mutations_never_panic_and_are_never_wrongly_accepted() {
    let base = fixture("complete.ots");
    let (height, hd) = header();
    let root = hd.merkle_root();
    let mut seed = 0x005E_ED0F_0754_u64;
    let mut accepted = 0;
    for round in 0..20_000 {
        let mut b = base.clone();
        let edits = 1 + (xorshift(&mut seed) % 4) as usize;
        for _ in 0..edits {
            let i = (xorshift(&mut seed) as usize) % b.len();
            match xorshift(&mut seed) % 4 {
                0 => b[i] ^= 1 << (xorshift(&mut seed) % 8),
                1 => b[i] = xorshift(&mut seed) as u8,
                2 => {
                    b.remove(i);
                }
                _ => b.insert(i, xorshift(&mut seed) as u8),
            }
        }
        let _ = ots::parse_timestamp(&b);
        let Ok(p) = ots::parse_proof(&b) else { continue };
        let w = p.walk();
        // Accepted only when it still truly leads from the imprint to the
        // header's root: the digest is unchanged and replay reaches it.
        let anchored = w.bitcoin().iter().any(|(h, m)| *h == height && m[..] == root);
        if anchored {
            assert_eq!(p.digest, imprint(), "round {round}: a proof over another digest was accepted");
            accepted += 1;
        }
    }
    // Most single-byte changes break the proof; those that do not touch
    // only what does not matter (e.g. the pending calendar's name).
    assert!(accepted < 20_000 / 4, "{accepted} of 20000 mutated proofs still anchored");
}

#[test]
fn random_bytes_never_panic() {
    let mut seed = 0xBAD_5EED_u64;
    for _ in 0..20_000 {
        let n = (xorshift(&mut seed) % 300) as usize;
        let mut b: Vec<u8> = (0..n).map(|_| xorshift(&mut seed) as u8).collect();
        let _ = ots::parse_timestamp(&b);
        let _ = ots::parse_proof(&b);
        b.splice(0..0, ots::MAGIC.iter().copied().chain([1, 8]).chain([0; 32]));
        if let Ok(p) = ots::parse_proof(&b) {
            let _ = p.walk();
        }
        let _ = BlockHeader::from_hex(&hex(&b[..b.len().min(80)])).map(|h| h.check_work());
    }
}

// ---- interoperability with the reference client (needs a real file) ---------

/// Interop with proofs made by the reference OpenTimestamps client is
/// UNVERIFIED until a real `.ots` file is put here: nothing in this project
/// may download one. To run it, put in `tests/fixtures/ots/reference/`:
/// `proof.ots` (made by `ots stamp FILE` and later `ots upgrade`), `digest.hex`
/// (the SHA-256 of FILE, from `sha256sum FILE`), and, to check the anchor,
/// `headers.txt` (`HEIGHT HEX` per line, the header of each block the proof
/// names, from a Bitcoin node you trust), then run
/// `cargo test -p evidence --test ots -- --ignored reference`.
#[test]
#[ignore = "needs a real .ots proof from the reference client in tests/fixtures/ots/reference/ (see the comment above this test)"]
fn reference_client_proof() {
    let dir = format!("{FIX}/reference");
    let bytes = std::fs::read(format!("{dir}/proof.ots")).unwrap_or_else(|_| panic!("put a proof made by the reference client at {dir}/proof.ots"));
    let p = ots::parse_proof(&bytes).expect("the reference client's proof parses");
    assert_eq!(p.to_bytes(), bytes, "re-serialised byte for byte");
    let digest = std::fs::read_to_string(format!("{dir}/digest.hex")).expect("digest.hex: the SHA-256 of the stamped file");
    assert_eq!(hex(&p.digest), digest.trim());
    let w = p.walk();
    println!("pending: {:?}\nbitcoin: {:?}\nskipped: {:?}", w.pending(), w.bitcoin().iter().map(|(h, _)| *h).collect::<Vec<_>>(), w.skipped);
    if let Ok(text) = std::fs::read_to_string(format!("{dir}/headers.txt")) {
        for line in text.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
            let (h, x) = line.split_once(char::is_whitespace).unwrap();
            let (h, hd): (u64, BlockHeader) = (h.parse().unwrap(), BlockHeader::from_hex(x).unwrap());
            hd.check_work().unwrap();
            let m = w.bitcoin().into_iter().find(|(k, _)| *k == h).expect("the proof names this block").1;
            assert_eq!(m, hd.merkle_root().to_vec(), "block {h}: the proof leads to its Merkle root");
        }
    }
}
