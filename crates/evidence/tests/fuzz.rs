//! Fuzz-style tests (crates/fuzzkit) for every reader of outside bytes in
//! this crate: log records, stored raw link frames, the continuity state and
//! record, base64, the RFC 3161 DER reader, OpenTimestamps proofs and
//! calendar replies, the whole-log verifier and the export-bundle checker.
//! Seeds are the logs and bundles tests/common builds (signed with the
//! published test key) and the fixtures in tests/fixtures.

mod common;

use common::unit::Unit;
use evidence::continuity::{self, Continuity};
use evidence::format::{self, MAX_RECORD};
use evidence::verify::{self, Input, Options};
use evidence::{b64, ots, summary, tsp};

const ITERS: usize = 3_000;

fn fixture(path: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/fixtures/{path}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// One boot's log with notes, checkpoints and an unsigned tail.
fn log() -> Vec<u8> {
    let mut u = Unit::new();
    u.utc_base = Some(1_794_990_000_000);
    u.boot(1, 4, 1, "fuzz seed").bytes
}

fn within(outer: &[u8], inner: &[u8]) -> bool {
    let (o, i) = (outer.as_ptr_range(), inner.as_ptr_range());
    inner.is_empty() || (o.start <= i.start && i.end <= o.end)
}

#[test]
fn fuzz_record_parsers() {
    let l = log();
    // Single records as seeds too, so mutations reach the body decoders.
    let mut seeds: Vec<Vec<u8>> = format::records(&l).filter_map(|(at, r)| r.ok().map(|r| l[at..at + r.header.len].to_vec())).collect();
    assert!(seeds.len() >= 5, "the seed log parses");
    seeds.push(l.clone());
    let refs: Vec<&[u8]> = seeds.iter().map(|s| s.as_slice()).collect();
    let cfg = fuzzkit::Config::new().max_len(4 * MAX_RECORD);
    fuzzkit::run_with("evidence::format::parse", &cfg, &refs, ITERS * 2, |bytes| {
        if let Ok(n) = format::peek_len(bytes) {
            assert!(n <= MAX_RECORD);
        }
        if let Ok(r) = format::parse(bytes) {
            assert!(within(bytes, r.bytes));
            assert_eq!(r.bytes.len(), r.header.len);
            let _ = r.hash();
            let _ = verify::describe(&r);
        }
        let mut total = 0;
        for (at, r) in format::records(bytes) {
            assert!(at <= bytes.len());
            if let Ok(r) = r {
                assert!(at + r.header.len <= bytes.len());
                total += r.header.len;
            }
        }
        assert!(total <= bytes.len());
        if let Some((at, _)) = format::last_complete(bytes) {
            assert!(at < bytes.len());
            assert!(format::parse(&bytes[at..]).is_ok());
        }
    });
}

#[test]
fn fuzz_raw_frame_summary() {
    // Stored raw frames are postcard link frames: take the seeds from the
    // detection records of the seed log.
    let mut buf = [0u8; 512];
    let f = link::Frame::Rf(link::RfSighting { kind: link::RfKind::AnalogVideo, freq: 58_060, width_khz: 8_000, snr_db: 20, duty_pct: 90, line_hz: 15_734, uptime_us: 5_000_000 });
    let raw = summary::raw_bytes(&f, &mut buf).map(|b| b.to_vec()).unwrap_or_default();
    let seeds: [&[u8]; 1] = [&raw];
    fuzzkit::run("evidence::summary::decode_raw", &seeds, ITERS, |bytes| {
        for fmt in [format::RAW_LINK_FRAME_V6, 0, 0xFF] {
            if let Ok((_, s, frame)) = summary::decode_raw(fmt, bytes) {
                let _ = s.as_bytes();
                let mut again = [0u8; 1024];
                if let Some(b) = summary::raw_bytes(&frame, &mut again) {
                    // Re-encoding a decoded frame gives the bytes that decoded.
                    assert_eq!(&b[..], bytes);
                }
            }
        }
    });
}

#[test]
fn fuzz_continuity_and_base64() {
    let st = continuity::State { counter: 9, boot_id: [3; 8], records: 77, head: [0xAB; 32] };
    let c = Continuity { source: 1, status: 2, counter: 9, prev_boot: [2; 8], prev_records: 76, prev_head: [0xCD; 32] };
    let (a, b) = (st.encode(), c.encode());
    let text = b64::encode_vec(&b);
    let seeds: [&[u8]; 3] = [&a, &b, text.as_bytes()];
    fuzzkit::run("evidence::continuity+b64", &seeds, ITERS * 2, |bytes| {
        if let Some(s) = continuity::State::decode(bytes) {
            assert_eq!(continuity::State::decode(&s.encode()), Some(s));
        }
        if let Some(c) = Continuity::decode(bytes) {
            assert_eq!(Continuity::decode(&c.encode()), Some(c));
        }
        let mut out = [0u8; 1024];
        if let Some(n) = b64::decode(bytes, &mut out) {
            assert!(n <= out.len());
            let again = b64::encode_vec(&out[..n]);
            assert_eq!(b64::decode_vec(&again).as_deref(), Some(&out[..n]));
        }
        let _ = b64::decode_vec(&fuzzkit::text(bytes));
    });
}

#[test]
fn fuzz_rfc3161_der() {
    let a = fixture("tsa/tsa-a/bundle.tsr");
    let b = fixture("tsa/tsa-b/bundle.tsr");
    let o = fixture("tsa/tsa-a/other.tsr");
    let seeds: [&[u8]; 3] = [&a, &b, &o];
    let imprint = [0u8; 32];
    let cfg = fuzzkit::Config::new().max_len(a.len().max(b.len()) + 64);
    fuzzkit::run_with("evidence::tsp", &cfg, &seeds, ITERS, |bytes| {
        if let Ok((tlv, rest)) = tsp::read(bytes) {
            assert!(within(bytes, rest));
            let _ = tlv;
        }
        if let Ok(r) = tsp::parse_response(bytes) {
            let _ = r;
        }
        let _ = tsp::parse_token(bytes);
        let _ = tsp::parse_tst_info(bytes);
        let _ = tsp::tst_info(bytes);
        let _ = tsp::check_reply(bytes, &imprint, &[1, 2, 3]);
        let _ = tsp::generalized_time_ms(bytes);
        let _ = tsp::same_integer(bytes, &bytes[bytes.len() / 2..]);
    });
}

#[test]
fn fuzz_opentimestamps() {
    let complete = fixture("ots/complete.ots");
    let pending = fixture("ots/pending.ots");
    let submit = fixture("ots/submit-reply.bin");
    let upgrade = fixture("ots/upgrade-reply.bin");
    let seeds: [&[u8]; 4] = [&complete, &pending, &submit, &upgrade];
    let cfg = fuzzkit::Config::new().max_len(2048);
    fuzzkit::run_with("evidence::ots", &cfg, &seeds, ITERS, |bytes| {
        if let Ok(p) = ots::parse_proof(bytes) {
            let again = p.to_bytes();
            let back = ots::parse_proof(&again).expect("a written proof parses");
            assert_eq!(back.to_bytes(), again);
            let _ = p.walk();
            let _ = p.is_anchored();
        }
        if let Ok(t) = ots::parse_timestamp(bytes) {
            let again = t.to_bytes();
            assert_eq!(ots::parse_timestamp(&again).map(|t| t.to_bytes()).as_deref(), Ok(&again[..]));
            let _ = t.walk(&[0u8; 32]);
            let mut g = t.clone();
            let _ = g.graft(&[0u8; 32], &[1u8; 32], &t);
        }
        if bytes.len() >= 80 {
            let h = ots::BlockHeader::from_hex(&verify::hex(&bytes[..80])).unwrap();
            let _ = h.check_work();
            let _ = ots::check_floor(h.bits(), u64::from(h.time()));
        }
    });
}

#[test]
fn fuzz_verify_whole_log() {
    // Verifying signs nothing but checks ECDSA signatures: slow in a debug
    // build, so fewer inputs.
    let l = log();
    let seeds: [&[u8]; 1] = [&l];
    let opts = Options { allow_test_key: true, ..Options::default() };
    let cfg = fuzzkit::Config::new().max_len(l.len() + 256).random_percent(5);
    fuzzkit::run_with("evidence::verify::verify", &cfg, &seeds, ITERS / 20, |bytes| {
        let rep = verify::verify(&[Input::new("EV000001.EVL", bytes.to_vec())], &opts);
        let _ = rep.verdict();
        let _ = rep.exit_code();
        for (input, at, r) in &rep.records {
            assert_eq!(*input, 0);
            assert!(at + r.bytes.len() <= bytes.len());
            assert_eq!(&bytes[*at..at + r.bytes.len()], &r.bytes[..]);
        }
    });
}

#[test]
fn fuzz_verify_bundle() {
    let json = common::bundle::build().json();
    let seeds: [&[u8]; 1] = [&json];
    let dict: [&[u8]; 8] = [b"\"", b"{", b"}", b"[", b"]", b",", b":", b"null"];
    let cfg = fuzzkit::Config::new().max_len(json.len() + 256).dict(&dict).random_percent(5);
    let opts = Options { allow_test_key: true, ..Options::default() };
    fuzzkit::run_with("evidence::verify::verify_bundle", &cfg, &seeds, ITERS / 30, |bytes| {
        let rep = verify::verify_bundle(bytes, &opts);
        let _ = rep.verdict();
    });
}

#[test]
fn fuzz_verify_text_helpers() {
    let seeds: [&[u8]; 5] = [b"EV000123.EVL", b"https://tsa.example.org:8443/tsr", b"boot 12; bench: gwforge, flip; x", b"0aFF10", b"20261119120000.123Z"];
    fuzzkit::run("evidence::verify text", &seeds, ITERS * 2, |bytes| {
        let s = fuzzkit::text(bytes);
        let _ = verify::file_number(&s);
        if let Some(v) = verify::unhex(&s) {
            // Surrounding whitespace is ignored; what decodes re-encodes.
            assert_eq!(v.len() * 2, s.trim().len());
            assert_eq!(verify::unhex(&verify::hex(&v)), Some(v));
        }
        let _ = verify::boot_bench_list(bytes);
        let _ = verify::url_host(&s);
        let _ = tsp::generalized_time_ms(bytes);
        let _ = ots::BlockHeader::from_hex(&s);
    });
}

/// The seeds reach the deep paths.
#[test]
fn seeds_are_accepted() {
    let mut buf = [0u8; 512];
    let f = link::Frame::Rf(link::RfSighting { kind: link::RfKind::AnalogVideo, freq: 58_060, width_khz: 8_000, snr_db: 20, duty_pct: 90, line_hz: 15_734, uptime_us: 5_000_000 });
    let raw = summary::raw_bytes(&f, &mut buf).expect("an Rf sighting has raw bytes").to_vec();
    assert!(summary::decode_raw(format::RAW_LINK_FRAME_V6, &raw).is_ok());
    assert!(ots::parse_proof(&fixture("ots/complete.ots")).is_ok());
    assert!(ots::parse_timestamp(&fixture("ots/upgrade-reply.bin")).is_ok());
    assert!(tsp::parse_response(&fixture("tsa/tsa-a/bundle.tsr")).is_ok());
    let opts = Options { allow_test_key: true, ..Options::default() };
    assert!(!verify::verify(&[Input::new("EV000001.EVL", log())], &opts).records.is_empty());
}
