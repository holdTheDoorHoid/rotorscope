//! WP-14b: the server under load and against hostile peers. Verification
//! does not hold the store, bodies are capped, the page loads nothing from
//! elsewhere, a boot's unsigned tail is taken once the next boot's signed
//! continuity record covers it, and a stored file changed on disk is not
//! shown on the map as if it were the log.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use evidence::verify::{self, hex, Level};
use evidence::Signer;
use evidence_server::store::Store;
use evidence_server::{router, App};
use http_body_util::BodyExt;
use tower::ServiceExt;

#[path = "../../../crates/evidence/tests/common/unit.rs"]
#[allow(dead_code)]
mod unit;

struct T {
    app: axum::Router,
    shared: Arc<App>,
    dir: std::path::PathBuf,
}

impl Drop for T {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl T {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("evsrv-hard-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::open(&dir).unwrap();
        let app = Arc::new(App::new(store, None, true, "test".into(), None));
        T { app: router(app.clone()), shared: app, dir }
    }

    async fn call(&self, method: &str, path: &str, body: Body, ct: &str) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
        call(&self.app, method, path, body, ct).await
    }

    async fn register(&self, key: &[u8; 65]) -> String {
        let body = serde_json::json!({ "public_key": hex(key), "name": "bench P4" }).to_string();
        let (s, _, b) = self.call("POST", "/api/devices", Body::from(body), "application/json").await;
        assert_eq!(s, StatusCode::CREATED, "{}", String::from_utf8_lossy(&b));
        String::from_utf8(evidence::fingerprint(key).to_vec()).unwrap()
    }

    async fn upload(&self, id: &str, bytes: Vec<u8>) -> (StatusCode, String) {
        let (s, _, b) = self.call("POST", &format!("/api/devices/{id}/batches"), Body::from(bytes), "application/octet-stream").await;
        (s, String::from_utf8_lossy(&b).into_owned())
    }

    async fn export(&self, id: &str) -> Vec<u8> {
        let (s, _, b) = self.call("GET", &format!("/api/devices/{id}/export"), Body::empty(), "").await;
        assert_eq!(s, StatusCode::OK);
        b
    }
}

async fn call(app: &axum::Router, method: &str, path: &str, body: Body, ct: &str) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let req = Request::builder().method(method).uri(path).header("content-type", ct).header("x-actor", "tester").body(body).unwrap();
    let r = app.clone().oneshot(req).await.unwrap();
    let status = r.status();
    let headers = r.headers().clone();
    let b = r.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, headers, b)
}

/// Records `from..=to` of a simulated boot.
fn recs(b: &unit::Boot, from: usize, to: usize) -> Vec<u8> {
    let start = if from == 0 { 0 } else { b.ends[from - 1] };
    b.bytes[start..b.ends[to]].to_vec()
}

fn fails(r: &verify::Report) -> Vec<String> {
    r.findings.iter().filter(|f| f.level == Level::Fail).map(|f| format!("{}: {}", f.at, f.what)).collect()
}

// ---- 1. Verification does not hold the store -----------------------------

/// While something else holds the store (a slow export, another upload's
/// write), an upload is still checked; a bad one is refused without waiting.
/// Before WP-14b the handler took the store's lock first and checked every
/// signature while holding it, so this request hung.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::await_holding_lock)] // holding the store across the upload is the point
async fn a_bad_upload_is_refused_while_the_store_is_busy() {
    let t = T::new("busy");
    let mut u = unit::Unit::new();
    let a = u.boot(1, 6, 0, "note");
    let id = t.register(&u.signer.public_key()).await;
    let mut bad = recs(&a, 0, 5);
    let n = bad.len();
    bad[n - 80] ^= 1;

    let guard = t.shared.store.lock().unwrap();
    let app = t.app.clone();
    let path = format!("/api/devices/{id}/batches");
    let h = tokio::spawn(async move { call(&app, "POST", &path, Body::from(bad), "application/octet-stream").await });
    let r = tokio::time::timeout(Duration::from_secs(10), h).await;
    drop(guard);
    let (s, _, b) = r.expect("an upload that fails its checks must not wait for the store").unwrap();
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{}", String::from_utf8_lossy(&b));

    // A good one waits for the store, then is kept.
    let (s, m) = t.upload(&id, recs(&a, 0, 5)).await;
    assert_eq!(s, StatusCode::CREATED, "{m}");
}

/// Two uploads of the same records at once: both are checked side by side,
/// one is kept, the other is told it overlaps. Nothing is stored twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_uploads_of_the_same_records_race_and_one_is_kept() {
    let t = T::new("race");
    let mut u = unit::Unit::new();
    let a = u.boot(1, 30, 0, "note");
    let id = t.register(&u.signer.public_key()).await;
    let last = a.ends.len() - 1;
    let mut hs = Vec::new();
    for _ in 0..6 {
        let app = t.app.clone();
        let path = format!("/api/devices/{id}/batches");
        let body = recs(&a, 0, last);
        hs.push(tokio::spawn(async move { call(&app, "POST", &path, Body::from(body), "application/octet-stream").await.0 }));
    }
    let mut codes = Vec::new();
    for h in hs {
        codes.push(h.await.unwrap());
    }
    assert_eq!(codes.iter().filter(|c| **c == StatusCode::CREATED).count(), 1, "{codes:?}");
    assert_eq!(codes.iter().filter(|c| **c == StatusCode::CONFLICT).count(), 5, "{codes:?}");
    let files = std::fs::read_dir(t.dir.join("devices").join(&id).join("batches")).unwrap().count();
    assert_eq!(files, 1);
    let rep = verify::verify_bundle(&t.export(&id).await, &verify::Options { allow_test_key: true, ..Default::default() });
    assert!(fails(&rep).is_empty(), "{:?}", fails(&rep));
}

// ---- 2. The page loads nothing from elsewhere ----------------------------

/// The page and its scripts come from the server itself, with a policy that
/// forbids scripts from anywhere else; the map library is the pinned copy.
#[tokio::test]
async fn the_page_loads_its_scripts_from_the_server_only() {
    let t = T::new("csp");
    let (s, h, page) = t.call("GET", "/", Body::empty(), "").await;
    assert_eq!(s, StatusCode::OK);
    let page = String::from_utf8(page).unwrap();
    for bad in ["<script src=\"http", "<script src=\"//", "<link rel=\"stylesheet\" href=\"http", "unpkg.com", "<script>"] {
        assert!(!page.contains(bad), "page still has {bad:?}");
    }
    let csp = h.get("content-security-policy").expect("a Content-Security-Policy").to_str().unwrap().to_string();
    for part in ["default-src 'none'", "script-src 'self'", "style-src 'self'", "connect-src 'self'", "frame-ancestors 'none'", "base-uri 'none'"] {
        assert!(csp.contains(part), "CSP lacks {part}: {csp}");
    }
    assert!(!csp.contains("unsafe-inline") && !csp.contains("unsafe-eval"), "{csp}");
    assert_eq!(h.get("x-content-type-options").map(|v| v.to_str().unwrap()), Some("nosniff"));

    // Leaflet 1.9.4 exactly as published (the SRI hashes on leafletjs.com).
    let (s, h, js) = t.call("GET", "/static/leaflet.js", Body::empty(), "").await;
    assert_eq!(s, StatusCode::OK);
    assert!(h["content-type"].to_str().unwrap().starts_with("text/javascript"));
    assert_eq!(evidence::b64::encode_vec(&evidence::sha256(&js)), "20nQCchB9co0qIjJZRGuk2/Z9VM+kNiyxNV1lvTlZBo=");
    let (s, h, css) = t.call("GET", "/static/leaflet.css", Body::empty(), "").await;
    assert_eq!(s, StatusCode::OK);
    assert!(h["content-type"].to_str().unwrap().starts_with("text/css"));
    assert_eq!(evidence::b64::encode_vec(&evidence::sha256(&css)), "p4NxAoJBhIIN+hmNHrzRCf9tD/miZyoHS5obTRR9BMY=");
    for p in ["/static/page.js", "/static/page.css"] {
        let (s, _, _) = t.call("GET", p, Body::empty(), "").await;
        assert_eq!(s, StatusCode::OK, "{p}");
    }
    let (s, _, _) = t.call("GET", "/static/../Cargo.toml", Body::empty(), "").await;
    assert_ne!(s, StatusCode::OK);
}

// ---- 3. Size caps on what a peer sends -----------------------------------

#[tokio::test]
async fn json_bodies_and_text_fields_are_capped() {
    let t = T::new("caps");
    let mut u = unit::Unit::new();
    let a = u.boot(1, 3, 0, "note");
    let id = t.register(&u.signer.public_key()).await;
    let (s, m) = t.upload(&id, recs(&a, 0, a.ends.len() - 1)).await;
    assert_eq!(s, StatusCode::CREATED, "{m}");

    // A megabyte of JSON to an endpoint that takes a short note.
    let ann = serde_json::json!({ "boot": hex(&[1u8; 8]), "seq": 1, "text": "x".repeat(1 << 20) }).to_string();
    let (s, _, _) = t.call("POST", &format!("/api/devices/{id}/annotations"), Body::from(ann), "application/json").await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
    // Under the body cap but over the note's length: refused, not stored.
    let ann = serde_json::json!({ "boot": hex(&[1u8; 8]), "seq": 1, "text": "x".repeat(5000) }).to_string();
    let (s, _, _) = t.call("POST", &format!("/api/devices/{id}/annotations"), Body::from(ann), "application/json").await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let ok = serde_json::json!({ "boot": hex(&[1u8; 8]), "seq": 1, "text": "seen from the roof" }).to_string();
    let (s, _, _) = t.call("POST", &format!("/api/devices/{id}/annotations"), Body::from(ok), "application/json").await;
    assert_eq!(s, StatusCode::CREATED);

    // A unit's name: short, no control characters (it is shown and logged).
    let other = evidence::sign::SoftSigner::new(&[0x47; 32], evidence::sign::KeyClass::DevelopmentSoftware).unwrap();
    for name in ["n".repeat(500), "bench\u{1b}[2J".to_string()] {
        let body = serde_json::json!({ "public_key": hex(&other.public_key()), "name": name }).to_string();
        let (s, _, _) = t.call("POST", "/api/devices", Body::from(body), "application/json").await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    }

    // A batch over the upload cap is refused before anything is checked.
    let big = vec![0u8; evidence_server::MAX_BATCH_BYTES + 1];
    let (s, _) = t.upload(&id, big).await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
}

// ---- 4. The unsigned tail, once the next boot covers it ------------------

/// Boot 1 ends with two records after its last checkpoint. The server takes
/// them only once boot 2's signed continuity record (naming boot 1's record
/// count and last hash) is stored, and only exactly up to that head. The
/// export then verifies with no gap.
#[tokio::test]
async fn a_tail_is_taken_once_the_next_boot_covers_it() {
    let t = T::new("tail");
    let mut u = unit::Unit::new();
    // Boot 1: #0 boot, #1 continuity, notes with checkpoints at #5 and #9,
    // then #10 and #11 unsigned.
    let a = u.boot(1, 6, 2, "note");
    let b = u.boot(2, 3, 0, "note");
    assert_eq!(a.ends.len(), 12);
    let id = t.register(&u.signer.public_key()).await;
    let (s, m) = t.upload(&id, recs(&a, 0, 9)).await;
    assert_eq!(s, StatusCode::CREATED, "{m}");

    // Nothing covers the tail yet: refused.
    let (s, m) = t.upload(&id, recs(&a, 10, 11)).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{m}");
    assert!(m.contains("end with a signed checkpoint"), "{m}");

    let (s, m) = t.upload(&id, recs(&b, 0, b.ends.len() - 1)).await;
    assert_eq!(s, StatusCode::CREATED, "{m}");

    // Short of the covered head: refused (it would end unsigned).
    let (s, m) = t.upload(&id, recs(&a, 10, 10)).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{m}");
    // The tail with a record altered: refused.
    let mut bad = recs(&a, 10, 11);
    bad[150] ^= 1;
    let (s, m) = t.upload(&id, bad).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{m}");

    // The real tail: kept.
    let (s, m) = t.upload(&id, recs(&a, 10, 11)).await;
    assert_eq!(s, StatusCode::CREATED, "{m}");
    let v: serde_json::Value = serde_json::from_str(&m).unwrap();
    assert!(v["covered_by"].is_object(), "{m}");
    let (s, m) = t.upload(&id, recs(&a, 10, 11)).await;
    assert_eq!(s, StatusCode::CONFLICT, "{m}");

    let opts = verify::Options { allow_test_key: true, expect_key: Some(u.signer.public_key()), ..Default::default() };
    let rep = verify::verify_bundle(&t.export(&id).await, &opts);
    assert_eq!(rep.exit_code(), 0, "{:#?}", rep.findings);
    assert!(rep.findings.iter().any(|f| f.what.contains("#10..#11") && f.what.contains("signature covers them")), "{:#?}", rep.findings);

    // Restart: the stored tail loads clean and the uploader's next record
    // for boot 1 is #12.
    let again = Store::open(&t.dir).unwrap();
    assert!(again.devices[&id].load_problems.is_empty(), "{:?}", again.devices[&id].load_problems);
    assert_eq!(again.devices[&id].next_seq()[&hex(&[1u8; 8])], 12);
    // The newest checkpoint (for an outside time-stamp) is boot 2's, not
    // the tail batch's last record.
    let (boot, seq, _) = again.devices[&id].latest_checkpoint().unwrap();
    assert_eq!((boot, seq), ([2u8; 8], 5));
}

/// A boot's records that end unsigned and that no stored continuity record
/// names are refused, even when a later boot is stored.
#[tokio::test]
async fn a_tail_no_continuity_record_names_is_refused() {
    let t = T::new("tail-uncovered");
    let mut u = unit::Unit::new();
    let a = u.boot(1, 6, 3, "note");
    let id = t.register(&u.signer.public_key()).await;
    let (s, m) = t.upload(&id, recs(&a, 0, 9)).await;
    assert_eq!(s, StatusCode::CREATED, "{m}");
    // Boot 2 names boot 1's #12 as its last stored record.
    let b = u.boot(2, 3, 0, "note");
    let (s, m) = t.upload(&id, recs(&b, 0, b.ends.len() - 1)).await;
    assert_eq!(s, StatusCode::CREATED, "{m}");
    // Going past the covered head is refused: a record #13 that links to
    // #12 (anyone can make one: an unsigned record is only a hash link).
    let mut longer = recs(&a, 10, 12);
    let r12 = recs(&a, 12, 12);
    let mut r13 = r12.clone();
    r13[evidence::format::off::SEQ..evidence::format::off::SEQ + 8].copy_from_slice(&13u64.to_le_bytes());
    r13[evidence::format::off::PREV..evidence::format::off::PREV + 32].copy_from_slice(&evidence::sha256(&r12));
    longer.extend_from_slice(&r13);
    // On its own it is well formed and links (so only the cover refuses it).
    assert!(verify::verify(&[verify::Input::new("x", longer.clone())], &verify::Options { gaps_allowed: true, ..Default::default() }).findings.iter().all(|f| f.level != Level::Fail));
    let (s, m) = t.upload(&id, longer).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{m}");
    // The covered part goes in.
    let (s, m) = t.upload(&id, recs(&a, 10, 12)).await;
    assert_eq!(s, StatusCode::CREATED, "{m}");
    // Another device's continuity record does not cover this one's tail.
    let t2 = T::new("tail-otherdev");
    let mut v = unit::Unit::new();
    v.signer = evidence::sign::SoftSigner::new(&[0x48; 32], evidence::sign::KeyClass::DevelopmentSoftware).unwrap();
    let id2 = t2.register(&v.signer.public_key()).await;
    let (s, _) = t2.upload(&id2, recs(&a, 10, 12)).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
}

// ---- 5. A stored file changed on disk is not shown as the log ------------

#[tokio::test]
async fn a_batch_changed_on_disk_is_not_shown_on_the_map() {
    let t = T::new("disk");
    let mut u = unit::Unit::new();
    let a = u.boot(1, 6, 0, "note");
    let id = t.register(&u.signer.public_key()).await;
    let (s, m) = t.upload(&id, recs(&a, 0, a.ends.len() - 1)).await;
    assert_eq!(s, StatusCode::CREATED, "{m}");
    let p = t.dir.join("devices").join(&id).join("batches").join("000001.bin");
    let mut perm = std::fs::metadata(&p).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perm.set_readonly(false);
    std::fs::set_permissions(&p, perm).unwrap();
    // Change a note's text.
    let mut bytes = std::fs::read(&p).unwrap();
    let at = a.ends[3] - 40;
    bytes[at] ^= 1;
    std::fs::write(&p, bytes).unwrap();
    let store = Store::open(&t.dir).unwrap();
    let d = &store.devices[&id];
    assert!(!d.load_problems.is_empty());
    assert!(d.unshown_batches().contains(&1), "{:?}", d.unshown_batches());
}
