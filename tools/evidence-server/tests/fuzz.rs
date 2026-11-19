//! Fuzz-style tests (crates/fuzzkit) for what the evidence server reads from
//! outside: uploaded batches (checked against the registered key, then
//! through the whole router), JSON bodies and path ids, the uploader's
//! console line reader, time-stamp service specs, calendar replies and
//! OpenTimestamps tokens. The device key is the published test key.

use std::io::Cursor;
use std::sync::Arc;

use axum::body::Body;
use axum::http::Request;
use evidence::Signer;
use evidence::verify::hex;
use evidence_server::lines::{Line, LineReader};
use evidence_server::store::{Store, check_batch};
use evidence_server::{App, ots, router, stamp, tsa};
use http_body_util::BodyExt;
use tower::ServiceExt;

#[path = "../../../crates/evidence/tests/common/unit.rs"]
#[allow(dead_code)]
mod unit;

fn fixture(p: &str) -> Vec<u8> {
    std::fs::read(format!("{}/../../crates/evidence/tests/fixtures/{p}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn boot() -> (Vec<u8>, [u8; 65]) {
    let mut u = unit::Unit::new();
    u.utc_base = Some(1_794_990_000_000);
    let b = u.boot(1, 4, 0, "fuzz seed");
    (b.bytes, u.signer.public_key())
}

#[test]
fn fuzz_check_batch() {
    let (bytes, key) = boot();
    assert!(check_batch(&bytes, &key).is_ok(), "the seed batch is accepted");
    let cfg = fuzzkit::Config::new().max_len(bytes.len() + 128).random_percent(5);
    fuzzkit::run_with("evidence-server::check_batch", &cfg, &[&bytes], 150, |b| {
        let _ = check_batch(b, &key);
    });
}

async fn call(app: &axum::Router, method: &str, path: &str, body: Vec<u8>, ct: &str) -> u16 {
    let req = Request::builder().method(method).uri(path).header("content-type", ct).header("x-actor", "fuzz").body(Body::from(body)).unwrap();
    let r = app.clone().oneshot(req).await.unwrap();
    let s = r.status().as_u16();
    let _ = r.into_body().collect().await.unwrap();
    s
}

#[test]
fn fuzz_router_bodies_and_paths() {
    let dir = std::env::temp_dir().join(format!("evsrv-fuzz-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Store::open(&dir).unwrap();
    let app = Arc::new(App::new(store, None, true, "fuzz".into(), None));
    let r = router(app);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let (batch, key) = boot();
    let id = String::from_utf8(evidence::fingerprint(&key).to_vec()).unwrap();
    let reg = serde_json::json!({ "public_key": hex(&key), "name": "bench P4" }).to_string();
    rt.block_on(async { assert_eq!(call(&r, "POST", "/api/devices", reg.clone().into_bytes(), "application/json").await, 201) });
    let note = serde_json::json!({ "text": "seen from the roof", "boot": "0101010101010101", "seq": 2 }).to_string();
    let seeds: [&[u8]; 3] = [reg.as_bytes(), note.as_bytes(), &batch];
    let dict: [&[u8]; 6] = [b"\"", b"{", b"}", b":", b"null", b"\\u0000"];
    let cfg = fuzzkit::Config::new().max_len(batch.len() + 64).dict(&dict).random_percent(5);
    fuzzkit::run_with("evidence-server::router", &cfg, &seeds, 120, |b| {
        rt.block_on(async {
            let p = |s: &str| s.to_string();
            for (m, path, ct) in [
                ("POST", p("/api/devices"), "application/json"),
                ("POST", format!("/api/devices/{id}/annotations"), "application/json"),
                ("POST", format!("/api/devices/{id}/batches"), "application/octet-stream"),
                ("POST", format!("/api/devices/{id}/timestamp"), "application/json"),
            ] {
                let s = call(&r, m, &path, b.to_vec(), ct).await;
                assert!(s != 500, "{m} {path}: 500");
            }
            // Any bytes as an id (percent-encoded so the URI is valid).
            let enc: String = b.iter().take(40).map(|c| format!("%{c:02X}")).collect();
            for path in [format!("/api/devices/{enc}"), format!("/api/devices/{enc}/export"), format!("/api/devices/{enc}/detections"), format!("/static/{enc}")] {
                let _ = call(&r, "GET", &path, Vec::new(), "").await;
            }
        });
    });
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fuzz_console_line_reader() {
    let seeds: [&[u8]; 2] = [b"@EV1 UlNFVgEAAAA=\r\nboot ok\n", b"no newline at all"];
    fuzzkit::run("evidence-server::lines::LineReader", &seeds, 6_000, |b| {
        let mut lr = LineReader::new(32);
        let mut c = Cursor::new(b);
        let mut n = 0;
        while let Some(l) = lr.read(&mut c).unwrap() {
            if let Line::Text(t) = l {
                assert!(t.len() <= 32);
            }
            n += 1;
            assert!(n <= b.len() + 1);
        }
        let _ = tsa::read_capped(Cursor::new(b), 16);
    });
}

#[test]
fn fuzz_stamping_inputs() {
    let pending = fixture("ots/pending.ots");
    let complete = fixture("ots/complete.ots");
    let submit = fixture("ots/submit-reply.bin");
    let seeds: [&[u8]; 4] = [&pending, &complete, &submit, b"https://tsa.example.org/tsr,chain=https://tsa.example.org/ca.pem"];
    let cfg = fuzzkit::Config::new().max_len(2048);
    fuzzkit::run_with("evidence-server::stamping inputs", &cfg, &seeds, 2_000, |b| {
        let text = fuzzkit::text(b);
        let _ = stamp::Authority::parse(&text);
        let _ = ots::parse_calendar(&text);
        let _ = ots::anchors(b);
        let _ = ots::state(b);
        let _ = ots::pending_calendars(b);
        let _ = ots::pending_proof("https://a.pool.opentimestamps.org", &[7; 32], &[1; ots::SALT_LEN], b);
        let _ = ots::upgraded_proof(b, "https://a.pool.opentimestamps.org", |_| Ok(Some(submit.clone())));
    });
}
