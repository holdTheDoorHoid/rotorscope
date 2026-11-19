//! PLAN.md Phase 13 done-when, server side: records travel upload -> store
//! -> export -> verifier; tampering with an upload, the stored files or the
//! bundle makes the server or the verifier refuse and say where.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use evidence::format::{self, CheckpointReason, EventCode};
use evidence::sign::{KeyClass, SoftSigner};
use evidence::verify::{self, hex, Level};
use evidence::writer::{Chain, Context, Detection};
use evidence::{PosSource, Signer, TimeSource};
use evidence_server::store::Store;
use evidence_server::{router, App};
use http_body_util::BodyExt;
use tower::ServiceExt;

fn frame(n: u32) -> link::Frame {
    let mut basic = [0u8; 25];
    basic[0] = 0x02;
    basic[1] = 0x12;
    basic[2..16].copy_from_slice(b"RSTEST-SD-0001");
    let mut messages = heapless::Vec::new();
    messages.push(basic).unwrap();
    link::Frame::Detection(link::Detection {
        transport: rid::Transport::WifiBeacon,
        channel: 6,
        rssi: -45,
        mac: [2, 0x52, 0x53, 0x0d, 0, 1],
        counter: n as u8,
        uptime_us: u64::from(n),
        messages,
    })
}

fn ctx(t: u64) -> Context {
    Context {
        uptime_ms: t,
        utc_ms: Some(1_763_200_000_000 + t),
        time_source: TimeSource::Gps,
        gps_state: 3,
        pos_source: PosSource::Gps,
        lat_e7: 399_500_000,
        lon_e7: -751_650_000,
        ..Context::default()
    }
}

/// Records of one boot, each separately, plus the signer's key.
fn log(boot: u8, signer: &mut SoftSigner) -> Vec<Vec<u8>> {
    let mut c = Chain::new([boot; 8], [0xAA; 32]);
    let mut out = Vec::new();
    let mut b = [0u8; format::MAX_RECORD];
    let mut keep = |c: &mut Chain, n: usize, b: &[u8]| {
        out.push(b[..n].to_vec());
        c.commit();
    };
    let mut data = vec![signer.key_class() as u8];
    data.extend_from_slice(&signer.public_key());
    let n = c.event(&ctx(1), EventCode::Boot, b"test", &data, &mut b).unwrap();
    keep(&mut c, n, &b);
    for i in 0..12u32 {
        if i == 5 || i == 11 {
            let n = c.checkpoint(&ctx(u64::from(i) * 1000 + 500), CheckpointReason::Periodic, signer, &mut b).unwrap();
            keep(&mut c, n, &b);
            continue;
        }
        let f = frame(i);
        let mut raw = [0u8; 400];
        let raw = evidence::summary::raw_bytes(&f, &mut raw).unwrap();
        let info = evidence::summary::info(&f).unwrap();
        let mut s = evidence::summary::Summary::new();
        evidence::summary::write_summary(&f, &mut s).unwrap();
        let d = Detection {
            node: [2, 0x52, 0x53, 0, 0, 2],
            link_rssi: Some(-40),
            node_index: 1,
            band: info.band,
            frame_kind: info.frame_kind,
            channel: info.channel,
            freq_100khz: 0,
            rssi: info.rssi,
            raw_format: format::RAW_LINK_FRAME_V6,
            raw,
            summary: s.as_bytes(),
        };
        let n = c.detection(&ctx(u64::from(i) * 1000 + 100), &d, &mut b).unwrap();
        keep(&mut c, n, &b);
    }
    out
}

struct T {
    app: axum::Router,
    shared: Arc<App>,
    dir: std::path::PathBuf,
}

impl T {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("evsrv-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::open(&dir).unwrap();
        let app = Arc::new(App::new(store, None, true, "test".into(), None));
        T { app: router(app.clone()), shared: app, dir }
    }

    async fn call(&self, method: &str, path: &str, body: Body, ct: &str) -> (StatusCode, Vec<u8>, Option<String>) {
        let req = Request::builder().method(method).uri(path).header("content-type", ct).header("x-actor", "tester").body(body).unwrap();
        let r = self.app.clone().oneshot(req).await.unwrap();
        let status = r.status();
        let sha = r.headers().get("x-bundle-sha256").map(|v| v.to_str().unwrap().to_string());
        let b = r.into_body().collect().await.unwrap().to_bytes().to_vec();
        (status, b, sha)
    }

    async fn register(&self, key: &[u8; 65]) -> String {
        let body = serde_json::json!({ "public_key": hex(key), "name": "bench P4" }).to_string();
        let (s, b, _) = self.call("POST", "/api/devices", Body::from(body), "application/json").await;
        assert_eq!(s, StatusCode::CREATED, "{}", String::from_utf8_lossy(&b));
        String::from_utf8(evidence::fingerprint(key).to_vec()).unwrap()
    }

    async fn upload(&self, id: &str, recs: &[Vec<u8>]) -> (StatusCode, String) {
        let (s, b, _) = self.call("POST", &format!("/api/devices/{id}/batches"), Body::from(recs.concat()), "application/octet-stream").await;
        (s, String::from_utf8_lossy(&b).into_owned())
    }

    async fn export(&self, id: &str) -> Vec<u8> {
        let (s, b, sha) = self.call("GET", &format!("/api/devices/{id}/export"), Body::empty(), "application/json").await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(sha.unwrap(), hex(&evidence::sha256(&b)));
        b
    }
}

fn fails(r: &verify::Report) -> Vec<String> {
    r.findings.iter().filter(|f| f.level == Level::Fail).map(|f| format!("{}: {}", f.at, f.what)).collect()
}

#[tokio::test]
async fn upload_store_export_verify_and_tamper() {
    let t = T::new("main");
    let mut signer = SoftSigner::new(&[0x42; 32], KeyClass::DevelopmentSoftware).unwrap();
    let recs = log(1, &mut signer);
    assert_eq!(recs.len(), 13);
    let id = t.register(&signer.public_key()).await;

    // Uploads must end on a checkpoint (#6 and #12 are the checkpoints).
    let (s, m) = t.upload(&id, &recs[0..5]).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{m}");
    assert!(m.contains("end with a signed checkpoint"));

    // A record changed in transit is refused, and the reply names it.
    let mut bad = recs[0..7].to_vec();
    let n = bad[3].len();
    bad[3][n - 12] ^= 1;
    let (s, m) = t.upload(&id, &bad).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{m}");
    assert!(m.contains("record #3") || m.contains("record #4"), "{m}");

    // A record left out in transit is refused.
    let mut holed = recs[0..7].to_vec();
    holed.remove(2);
    let (s, m) = t.upload(&id, &holed).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{m}");

    // Signed by another key: refused.
    let mut other = SoftSigner::new(&[0x43; 32], KeyClass::DevelopmentSoftware).unwrap();
    let forged = log(1, &mut other);
    let (s, m) = t.upload(&id, &forged[0..7]).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{m}");

    // The real thing, in two batches.
    let (s, m) = t.upload(&id, &recs[0..7]).await;
    assert_eq!(s, StatusCode::CREATED, "{m}");
    let (s, m) = t.upload(&id, &recs[0..7]).await;
    assert_eq!(s, StatusCode::CONFLICT, "{m}");
    let (s, m) = t.upload(&id, &recs[7..13]).await;
    assert_eq!(s, StatusCode::CREATED, "{m}");

    // Look at it, annotate a record: both audited.
    let (s, b, _) = t.call("GET", &format!("/api/devices/{id}/detections"), Body::empty(), "").await;
    assert_eq!(s, StatusCode::OK);
    let pts: serde_json::Value = serde_json::from_slice(&b).unwrap();
    assert_eq!(pts.as_array().unwrap().len(), 10);
    assert_eq!(pts[0]["drone_id"], "RSTEST-SD-0001");
    let ann = serde_json::json!({ "boot": hex(&[1u8; 8]), "seq": 3, "text": "matches the bench transmitter" }).to_string();
    let (s, _, _) = t.call("POST", &format!("/api/devices/{id}/annotations"), Body::from(ann), "application/json").await;
    assert_eq!(s, StatusCode::CREATED);

    // Export -> verifier passes.
    let bundle = t.export(&id).await;
    let with_key = verify::Options { expect_key: Some(signer.public_key()), ..verify::Options::default() };
    let rep = verify::verify_bundle(&bundle, &with_key);
    assert_eq!(rep.exit_code(), 0, "{:#?}", rep.findings);
    assert_eq!(rep.records.len(), 13);
    let audit: Vec<String> = serde_json::from_slice::<serde_json::Value>(&bundle).unwrap()["audit"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| serde_json::from_str::<serde_json::Value>(l["entry"].as_str().unwrap()).unwrap()["action"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(audit, ["register", "upload", "upload", "view", "annotate"]);

    // Tamper with copies of the bundle.
    let v: serde_json::Value = serde_json::from_slice(&bundle).unwrap();
    let tamper = |f: &dyn Fn(&mut serde_json::Value)| {
        let mut c = v.clone();
        f(&mut c);
        verify::verify_bundle(&serde_json::to_vec(&c).unwrap(), &with_key)
    };
    let edit_record = |c: &mut serde_json::Value| {
        let mut b = evidence::b64::decode_vec(c["batches"][0]["records"].as_str().unwrap()).unwrap();
        let at = recs[0].len() + recs[1].len() + 140;
        b[at] ^= 1;
        c["batches"][0]["records"] = evidence::b64::encode_vec(&b).into();
    };
    let r = tamper(&edit_record);
    assert_eq!(r.exit_code(), 1);
    assert!(fails(&r).iter().any(|s| s.contains("batch 1")), "{:?}", fails(&r));
    // Edit, then also fix the batch hash: the audit log's upload entry still disagrees.
    let r = tamper(&|c| {
        edit_record(c);
        let b = evidence::b64::decode_vec(c["batches"][0]["records"].as_str().unwrap()).unwrap();
        c["batches"][0]["sha256"] = hex(&evidence::sha256(&b)).into();
    });
    assert!(fails(&r).iter().any(|s| s.contains("audit log's upload entry")), "{:?}", fails(&r));
    // ...and the record's own link breaks too.
    assert!(fails(&r).iter().any(|s| s.contains("record #2")), "{:?}", fails(&r));
    // Delete a record from a batch.
    let r = tamper(&|c| {
        let b = evidence::b64::decode_vec(c["batches"][1]["records"].as_str().unwrap()).unwrap();
        let cut = [&b[..recs[7].len()], &b[recs[7].len() + recs[8].len()..]].concat();
        c["batches"][1]["records"] = evidence::b64::encode_vec(&cut).into();
    });
    assert!(fails(&r).iter().any(|s| s.contains("batch 2")), "{:?}", fails(&r));
    // Truncate a batch.
    let r = tamper(&|c| {
        let b = evidence::b64::decode_vec(c["batches"][1]["records"].as_str().unwrap()).unwrap();
        c["batches"][1]["records"] = evidence::b64::encode_vec(&b[..b.len() - 30]).into();
    });
    assert_eq!(r.exit_code(), 1);
    // Drop a whole batch.
    let r = tamper(&|c| {
        c["batches"].as_array_mut().unwrap().remove(1);
    });
    assert!(fails(&r).iter().any(|s| s.contains("batches were removed")), "{:?}", fails(&r));
    // Reorder the batches.
    let r = tamper(&|c| {
        c["batches"].as_array_mut().unwrap().swap(0, 1);
    });
    assert_eq!(r.exit_code(), 1);
    // Edit, drop or reorder audit entries.
    let r = tamper(&|c| {
        let e = c["audit"][4]["entry"].as_str().unwrap().replace("bench transmitter", "someone else");
        c["audit"][4]["entry"] = e.into();
    });
    assert!(fails(&r).iter().any(|s| s.contains("audit entry 4")), "{:?}", fails(&r));
    let r = tamper(&|c| {
        c["audit"].as_array_mut().unwrap().remove(3);
    });
    assert_eq!(r.exit_code(), 1);
    let r = tamper(&|c| {
        c["audit"].as_array_mut().unwrap().swap(3, 4);
    });
    assert_eq!(r.exit_code(), 1);
    // Swap the device key for another.
    let r = tamper(&|c| {
        c["device"]["public_key"] = hex(&other.public_key()).into();
    });
    assert_eq!(r.exit_code(), 1);
    // Swap the key and its id consistently: the records still name the
    // real signer, and the bundle no longer names a key you supplied.
    let r = tamper(&|c| {
        c["device"]["public_key"] = hex(&other.public_key()).into();
        c["device"]["key_id"] = String::from_utf8(evidence::fingerprint(&other.public_key()).to_vec()).unwrap().into();
    });
    assert_eq!(r.exit_code(), 1);
    assert!(fails(&r).iter().any(|s| s.contains("device key")), "{:?}", fails(&r));

    // Restart from disk: everything re-checks clean...
    let again = Store::open(&t.dir).unwrap();
    assert!(again.devices[&id].load_problems.is_empty(), "{:?}", again.devices[&id].load_problems);
    assert_eq!(again.devices[&id].next_seq()[&hex(&[1u8; 8])], 13);
    // ...and a batch file changed on disk is reported at load.
    let p = t.dir.join("devices").join(&id).join("batches").join("000001.bin");
    let mut perm = std::fs::metadata(&p).unwrap().permissions();
    assert!(perm.readonly(), "stored batches are read-only");
    #[allow(clippy::permissions_set_readonly_false)]
    perm.set_readonly(false);
    std::fs::set_permissions(&p, perm).unwrap();
    let mut b = std::fs::read(&p).unwrap();
    b[200] ^= 1;
    std::fs::write(&p, b).unwrap();
    let again = Store::open(&t.dir).unwrap();
    assert!(!again.devices[&id].load_problems.is_empty());
    let _ = std::fs::remove_dir_all(&t.dir);
}

#[tokio::test]
async fn gaps_are_shown_not_hidden() {
    let t = T::new("gap");
    let mut signer = SoftSigner::new(&[0x44; 32], KeyClass::DevelopmentSoftware).unwrap();
    let recs = log(2, &mut signer);
    let id = t.register(&signer.public_key()).await;
    // Only the second half reached the server.
    let (s, m) = t.upload(&id, &recs[7..13]).await;
    assert_eq!(s, StatusCode::CREATED, "{m}");
    let bundle = t.export(&id).await;
    let key = Some(signer.public_key());
    // A gap fails by default...
    let rep = verify::verify_bundle(&bundle, &verify::Options { expect_key: key, ..verify::Options::default() });
    assert_eq!(rep.exit_code(), 1, "{:?}", rep.findings);
    // ...and with --allow-gaps it is shown, and may hide removed records.
    let gaps = verify::Options { expect_key: key, gaps_allowed: true, ..verify::Options::default() };
    let rep = verify::verify_bundle(&bundle, &gaps);
    assert_eq!(rep.exit_code(), 3, "{:?}", rep.findings);
    assert!(rep.findings.iter().any(|f| f.level == Level::Gap && f.what.contains("#0..#6") && f.what.contains("may have been removed")));
    // Filling the gap later must link at both ends.
    let (s, m) = t.upload(&id, &recs[0..7]).await;
    assert_eq!(s, StatusCode::CREATED, "{m}");
    let rep = verify::verify_bundle(&t.export(&id).await, &gaps);
    assert!(fails(&rep).is_empty(), "{:?}", fails(&rep));
    let _ = std::fs::remove_dir_all(&t.dir);
}

/// A bundle is only VERIFIED against a key supplied from outside it: the
/// device key it names is the server's word, and anyone can run a server.
#[tokio::test]
async fn a_forged_bundle_is_not_verified_without_an_outside_key() {
    let t = T::new("forged");
    // A key made up on the spot, claiming to be a hardware key.
    let mut f = SoftSigner::new(&[0x5A; 32], KeyClass::HardwareEfuse).unwrap();
    let recs = log(4, &mut f);
    let id = t.register(&f.public_key()).await;
    let (s, m) = t.upload(&id, &recs[0..7]).await;
    assert_eq!(s, StatusCode::CREATED, "{m}");
    let bundle = t.export(&id).await;

    let rep = verify::verify_bundle(&bundle, &verify::Options::default());
    assert!(fails(&rep).is_empty(), "{:?}", fails(&rep));
    assert_eq!(rep.exit_code(), 4, "{:?}", rep.findings);
    assert!(rep.verdict().starts_with("SELF-CONSISTENT, SIGNER NOT CHECKED"));
    assert!(rep.findings.iter().any(|f| f.what.contains("log claims hardware_efuse")), "{:?}", rep.findings);

    // The forger's key, supplied from outside: that is the trust decision.
    let rep = verify::verify_bundle(&bundle, &verify::Options { expect_key: Some(f.public_key()), ..verify::Options::default() });
    assert_eq!(rep.exit_code(), 0, "{:?}", rep.findings);

    // Another key: refused.
    let real = SoftSigner::new(&[0x11; 32], KeyClass::DevelopmentSoftware).unwrap();
    let rep = verify::verify_bundle(&bundle, &verify::Options { trusted_keys: vec![(real.public_key(), "unit 1".into())], ..verify::Options::default() });
    assert_eq!(rep.exit_code(), 1, "{:?}", rep.findings);
    let _ = std::fs::remove_dir_all(&t.dir);
}

/// The server stores a test-key device's uploads (bench hubs sign with it),
/// but its bundle is refused unless the test key is explicitly allowed.
#[tokio::test]
async fn a_test_key_bundle_is_refused_by_default() {
    let t = T::new("testkey");
    let mut k = SoftSigner::public_test();
    let recs = log(5, &mut k);
    let id = t.register(&k.public_key()).await;
    let (s, m) = t.upload(&id, &recs[0..7]).await;
    assert_eq!(s, StatusCode::CREATED, "{m}");
    let bundle = t.export(&id).await;
    let rep = verify::verify_bundle(&bundle, &verify::Options { expect_key: Some(k.public_key()), ..verify::Options::default() });
    assert_eq!(rep.exit_code(), 1, "{:?}", rep.findings);
    assert!(fails(&rep).iter().any(|s| s.contains("published test key")), "{:?}", fails(&rep));
    let rep = verify::verify_bundle(&bundle, &verify::Options { allow_test_key: true, ..verify::Options::default() });
    assert_eq!(rep.exit_code(), 0, "{:?}", rep.findings);
    let _ = std::fs::remove_dir_all(&t.dir);
}

/// A checkpoint or event rebuilt with another record number, previous hash
/// and (for a checkpoint) count, then signed again: what a registered key
/// holder can always do.
fn forge(signer: &mut SoftSigner, rec: &[u8], seq: u64, prev: [u8; 32]) -> Vec<u8> {
    use format::off;
    let mut b = rec.to_vec();
    b[off::SEQ..off::SEQ + 8].copy_from_slice(&seq.to_le_bytes());
    b[off::PREV..off::PREV + 32].copy_from_slice(&prev);
    if b[off::KIND] == format::Kind::Checkpoint as u8 {
        let count_at = format::HEADER + 2 + 65;
        b[count_at..count_at + 8].copy_from_slice(&seq.to_le_bytes());
        let signed_end = b.len() - format::TRAILER - format::SIG_LEN;
        let sig = signer
            .sign_digest(&evidence::sha256(&b[..signed_end]))
            .unwrap();
        b[signed_end..signed_end + format::SIG_LEN].copy_from_slice(&sig);
    }
    b
}

#[tokio::test]
async fn record_number_overflow_is_refused_and_the_server_keeps_answering() {
    let t = T::new("seqwrap");
    let mut signer = SoftSigner::new(&[0x45; 32], KeyClass::DevelopmentSoftware).unwrap();
    let recs = log(3, &mut signer);
    let id = t.register(&signer.public_key()).await;
    let (s, m) = t.upload(&id, &recs[0..7]).await;
    assert_eq!(s, StatusCode::CREATED, "{m}");

    // Record #u64::MAX, then a checkpoint numbered 0 that links to it: the
    // successor of u64::MAX wraps to 0, so the chain looks contiguous.
    let last = forge(&mut signer, &recs[1], u64::MAX, [9; 32]);
    let cp = forge(&mut signer, &recs[6], 0, evidence::sha256(&last));
    let (s, m) = t.upload(&id, &[last, cp]).await;
    assert!(
        s == StatusCode::UNPROCESSABLE_ENTITY || s == StatusCode::CONFLICT,
        "{s} {m}"
    );

    // Still serving, and the good data is intact.
    let (s, b, _) = t
        .call(
            "GET",
            &format!("/api/devices/{id}"),
            Body::empty(),
            "application/json",
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", String::from_utf8_lossy(&b));
    let (s, m) = t.upload(&id, &recs[7..13]).await;
    assert_eq!(s, StatusCode::CREATED, "{m}");
    let _ = std::fs::remove_dir_all(&t.dir);
}

#[tokio::test]
async fn a_poisoned_lock_does_not_take_the_server_down() {
    let t = T::new("poison");
    let app = t.shared.clone();
    let _ = std::thread::spawn(move || {
        let _g = app.store.lock().unwrap();
        panic!("a handler died holding the lock");
    })
    .join();
    assert!(t.shared.store.is_poisoned());
    let (s, _, _) = t
        .call("GET", "/api/devices", Body::empty(), "application/json")
        .await;
    assert_eq!(s, StatusCode::OK);
    let _ = std::fs::remove_dir_all(&t.dir);
}

fn app_with(name: &str, token: Option<&str>, open: bool) -> (axum::Router, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("evsrv-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Store::open(&dir).unwrap();
    let app = Arc::new(App::new(store, token.map(str::to_string), open, "test".into(), None));
    (router(app), dir)
}

async fn send(
    r: &axum::Router,
    method: &str,
    path: &str,
    auth: Option<&str>,
    body: String,
) -> StatusCode {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(a) = auth {
        req = req.header("authorization", format!("Bearer {a}"));
    }
    r.clone()
        .oneshot(req.body(Body::from(body)).unwrap())
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn registration_needs_the_token_unless_opened_explicitly() {
    let signer = SoftSigner::new(&[0x46; 32], KeyClass::DevelopmentSoftware).unwrap();
    let body =
        serde_json::json!({ "public_key": hex(&signer.public_key()), "name": "x" }).to_string();
    let id = String::from_utf8(evidence::fingerprint(&signer.public_key()).to_vec()).unwrap();

    // No token, not opened: refused.
    let (r, dir) = app_with("reg-closed", None, false);
    assert_eq!(
        send(&r, "POST", "/api/devices", None, body.clone()).await,
        StatusCode::FORBIDDEN
    );
    let _ = std::fs::remove_dir_all(dir);

    // No token, opened explicitly: allowed.
    let (r, dir) = app_with("reg-open", None, true);
    assert_eq!(
        send(&r, "POST", "/api/devices", None, body.clone()).await,
        StatusCode::CREATED
    );
    let _ = std::fs::remove_dir_all(dir);

    // With a token: it is required, and then registration works.
    let (r, dir) = app_with("reg-token", Some("s3cret"), false);
    assert_eq!(
        send(&r, "POST", "/api/devices", None, body.clone()).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(&r, "POST", "/api/devices", Some("s3cret"), body).await,
        StatusCode::CREATED
    );
    // The per-device status call needs it too.
    let path = format!("/api/devices/{id}");
    assert_eq!(
        send(&r, "GET", &path, None, String::new()).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(&r, "GET", &path, Some("s3cret"), String::new()).await,
        StatusCode::OK
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// The page is built with innerHTML, and a unit's name is whatever its
/// registrant typed: every value that came from the server must go through
/// `esc()` (there is no JS runner here, so this checks the source).
#[test]
fn page_escapes_server_text() {
    let page = include_str!("../src/static/page.js");
    assert!(!page.contains("${d.name}"), "the unit name must be escaped");
    assert!(page.contains("${esc(d.name)}"));
    for raw in ["${d.key_id}", "${p.time_source}", "p.hub_position +", "+ p.hub_position"] {
        assert!(!page.contains(raw), "{raw} is not escaped");
    }
    // Time-stamp lines: every server value escaped (the authority URL and a
    // failure reason come from configuration and from a third party).
    for raw in ["${s.authority}", "s.authority +", "${s.last_failure.reason}", "${n.audit_n}", "${s.outstanding.audit_n}"] {
        assert!(!page.contains(raw), "{raw} is not escaped");
    }
    assert!(page.contains("esc(s.authority)") && page.contains("esc(s.last_failure.reason)"));
}
