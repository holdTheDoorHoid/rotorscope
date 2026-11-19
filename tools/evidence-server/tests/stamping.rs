//! WP-14c: automatic outside time-stamping, against fake authorities on
//! 127.0.0.1 only. Nothing here talks to a real time-stamp service.
//!
//! Checked: exactly what leaves the server (the bytes and headers), that a
//! reply which is not the stamp asked for is a failure and never a stamp,
//! that each service is asked independently, that uploads never wait, that
//! the queue survives a restart, the coalescing rule, and that the export
//! bundle carries stamps the checker accepts.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use evidence::verify::{self, bundle, hex, Level};
use evidence::Signer;
use evidence_server::stamp::{self, Authority, Config, Rfc3161, Stamper, Stamping};
use evidence_server::store::Store;
use evidence_server::{router, App};
use http_body_util::BodyExt;
use tower::ServiceExt;

#[path = "../../../crates/evidence/tests/common/unit.rs"]
#[allow(dead_code)]
mod unit;

// ---- a fake authority ----------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
enum Behave {
    Good,
    Refuse,
    Garbage,
    WrongImprint,
    WrongNonce,
    NoNonce,
    Oversize,
    /// The right reply, gzip-compressed (Content-Encoding: gzip).
    Gzipped,
    /// The right reply with no length: the body runs until the connection closes.
    Unframed,
    Http500,
    /// 302 to another listener (whose requests are counted).
    Redirect(u16),
    /// Answers after this long.
    Slow(u64),
}

struct Fake {
    port: u16,
    /// Every request received, raw.
    seen: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl Fake {
    fn start(b: Behave) -> Fake {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(c) = c else { continue };
                let seen = s2.clone();
                std::thread::spawn(move || serve(c, b, &seen));
            }
        });
        Fake { port, seen }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/tsr", self.port)
    }

    fn requests(&self) -> Vec<Vec<u8>> {
        self.seen.lock().unwrap().clone()
    }
}

fn read_request(c: &mut TcpStream) -> Option<Vec<u8>> {
    c.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut buf = Vec::new();
    let mut b = [0u8; 4096];
    loop {
        let n = c.read(&mut b).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&b[..n]);
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
            let len: usize = head.lines().find_map(|l| l.strip_prefix("content-length:")).map_or(0, |v| v.trim().parse().unwrap());
            while buf.len() < end + 4 + len {
                let n = c.read(&mut b).ok()?;
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&b[..n]);
            }
            return Some(buf);
        }
    }
}

fn body_of(req: &[u8]) -> &[u8] {
    let end = req.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    &req[end + 4..]
}

fn serve(mut c: TcpStream, b: Behave, seen: &Mutex<Vec<Vec<u8>>>) {
    let Some(req) = read_request(&mut c) else { return };
    seen.lock().unwrap().push(req.clone());
    let respond = |c: &mut TcpStream, status: &str, ct: &str, body: &[u8]| {
        let _ = write!(c, "HTTP/1.1 {status}\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
        let _ = c.write_all(body);
    };
    if req.starts_with(b"GET /ca.pem") {
        return respond(&mut c, "200 OK", "application/x-pem-file", b"-----BEGIN CERTIFICATE-----\nfake chain\n-----END CERTIFICATE-----\n");
    }
    if req.starts_with(b"GET /root.crl") {
        return respond(&mut c, "200 OK", "application/pkix-crl", &[0x30, 0x03, 0x02, 0x01, 0x00]);
    }
    let body = body_of(&req);
    let (imprint, nonce) = if body.len() == 69 { (body[24..56].to_vec(), body[58..66].to_vec()) } else { (vec![0; 32], vec![1]) };
    let ok = |imprint: &[u8], nonce: Option<&[u8]>| reply(0, imprint, nonce);
    match b {
        Behave::Good => respond(&mut c, "200 OK", "application/timestamp-reply", &ok(&imprint, Some(&nonce))),
        Behave::Refuse => respond(&mut c, "200 OK", "application/timestamp-reply", &[0x30, 0x05, 0x30, 0x03, 0x02, 0x01, 0x02]),
        Behave::Garbage => respond(&mut c, "200 OK", "application/timestamp-reply", b"<html>not a stamp</html>"),
        Behave::WrongImprint => respond(&mut c, "200 OK", "application/timestamp-reply", &ok(&[0x5A; 32], Some(&nonce))),
        Behave::WrongNonce => respond(&mut c, "200 OK", "application/timestamp-reply", &ok(&imprint, Some(&[0x7E; 8]))),
        Behave::NoNonce => respond(&mut c, "200 OK", "application/timestamp-reply", &ok(&imprint, None)),
        Behave::Gzipped => {
            let body = gzip_stored(&ok(&imprint, Some(&nonce)));
            let _ = write!(c, "HTTP/1.1 200 OK\r\nContent-Type: application/timestamp-reply\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = c.write_all(&body);
        }
        Behave::Unframed => {
            let _ = write!(c, "HTTP/1.1 200 OK\r\nContent-Type: application/timestamp-reply\r\nConnection: close\r\n\r\n");
            let _ = c.write_all(&ok(&imprint, Some(&nonce)));
        }
        Behave::Oversize => {
            let _ = write!(c, "HTTP/1.1 200 OK\r\nContent-Type: application/timestamp-reply\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", 1 << 20);
            let junk = vec![0x30u8; 64 << 10];
            for _ in 0..16 {
                if c.write_all(&junk).is_err() {
                    break;
                }
            }
        }
        Behave::Http500 => respond(&mut c, "500 Internal Server Error", "text/plain", b"down"),
        Behave::Redirect(port) => {
            let _ = write!(c, "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{port}/tsr\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        }
        Behave::Slow(ms) => {
            std::thread::sleep(Duration::from_millis(ms));
            respond(&mut c, "200 OK", "application/timestamp-reply", &ok(&imprint, Some(&nonce)));
        }
    }
}

/// `data` as a gzip member with one stored (uncompressed) deflate block:
/// valid gzip that any decoder accepts, built without a gzip library.
fn gzip_stored(data: &[u8]) -> Vec<u8> {
    let crc = {
        let mut c = !0u32;
        for &b in data {
            c ^= u32::from(b);
            for _ in 0..8 {
                c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
            }
        }
        !c
    };
    let n = data.len() as u16;
    let mut v = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff, 0x01];
    v.extend_from_slice(&n.to_le_bytes());
    v.extend_from_slice(&(!n).to_le_bytes());
    v.extend_from_slice(data);
    v.extend_from_slice(&crc.to_le_bytes());
    v.extend_from_slice(&(data.len() as u32).to_le_bytes());
    v
}

fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut v = vec![tag];
    match content.len() {
        n if n < 0x80 => v.push(n as u8),
        n if n < 0x100 => v.extend_from_slice(&[0x81, n as u8]),
        n => v.extend_from_slice(&[0x82, (n >> 8) as u8, n as u8]),
    }
    v.extend_from_slice(content);
    v
}

/// A structurally valid TimeStampResp (unsigned: the server does not check
/// signatures; the verifier's openssl step does, with the canned fixtures).
fn reply(status: u8, imprint: &[u8], nonce: Option<&[u8]>) -> Vec<u8> {
    use evidence::tsp::{OID_SHA256, OID_SIGNED_DATA, OID_TST_INFO};
    let now = verify::fmt_utc(evidence_server::store::now_ms());
    let gt: String = now.chars().filter(char::is_ascii_digit).take(14).collect::<String>() + "Z";
    let alg = tlv(0x30, &[tlv(0x06, OID_SHA256), vec![0x05, 0x00]].concat());
    let mi = tlv(0x30, &[alg, tlv(0x04, imprint)].concat());
    let mut tst = [tlv(0x02, &[1]), tlv(0x06, &[0x2A, 0x03]), mi, tlv(0x02, &[7]), tlv(0x18, gt.as_bytes())].concat();
    if let Some(n) = nonce {
        tst.extend(tlv(0x02, n));
    }
    let tst = tlv(0x30, &tst);
    let eci = tlv(0x30, &[tlv(0x06, OID_TST_INFO), tlv(0xA0, &tlv(0x04, &tst))].concat());
    let sd = tlv(0x30, &[tlv(0x02, &[3]), tlv(0x31, &[]), eci, tlv(0x31, &[])].concat());
    let ci = tlv(0x30, &[tlv(0x06, OID_SIGNED_DATA), tlv(0xA0, &sd)].concat());
    tlv(0x30, &[tlv(0x30, &tlv(0x02, &[status])), ci].concat())
}

// ---- a server with stamping ----------------------------------------------

struct T {
    app: Arc<App>,
    dir: std::path::PathBuf,
    id: String,
}

impl Drop for T {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn cfg(urls: &[String]) -> Config {
    Config {
        enabled: true,
        authorities: urls.iter().map(|u| Authority::parse(u).unwrap()).collect(),
        timeout: Duration::from_secs(2),
        min_gap_ms: 0,
        ..Config::default()
    }
}

fn dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("evsrv-stamp-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

impl T {
    fn new(name: &str, stamping: Option<Stamping>) -> T {
        let dir = dir(name);
        let mut store = Store::open(&dir).unwrap();
        let key = evidence::sign::SoftSigner::public_test().public_key();
        let info = store.register(&hex(&key), "bench P4 at the back garden", "tester").unwrap();
        let app = Arc::new(App::new(store, None, true, "test".into(), stamping));
        T { app, dir, id: info.key_id }
    }

    fn reopen(&mut self, stamping: Option<Stamping>) {
        let store = Store::open(&self.dir).unwrap();
        self.app = Arc::new(App::new(store, None, true, "test".into(), stamping));
    }

    /// Store a boot's records as the upload handler does, at `now`.
    fn upload(&self, boot: &unit::Boot, now: u64) {
        self.app.lock().device_mut(&self.id).unwrap().upload(&boot.bytes, "uploader").unwrap();
        if let Some(st) = &self.app.stamping {
            st.on_upload(&self.id, now);
        }
    }

    fn stamps(&self) -> Vec<bundle::Timestamp> {
        self.app.lock().device(&self.id).unwrap().timestamps.clone()
    }

    fn failures(&self) -> Vec<bundle::StampFailure> {
        self.app.lock().device(&self.id).unwrap().stamp_failures.clone()
    }

    fn queue_len(&self) -> usize {
        self.app.lock().device(&self.id).unwrap().queue.len()
    }

    async fn call(&self, method: &str, path: &str, body: Body) -> (StatusCode, Vec<u8>) {
        let req = Request::builder().method(method).uri(path).header("content-type", "application/octet-stream").header("x-actor", "tester").body(body).unwrap();
        let r = router(self.app.clone()).oneshot(req).await.unwrap();
        let s = r.status();
        (s, r.into_body().collect().await.unwrap().to_bytes().to_vec())
    }
}

fn boots() -> Vec<unit::Boot> {
    let mut u = unit::Unit::new();
    u.utc_base = Some(1_795_000_000_000);
    (1..=4).map(|i| u.boot(i, 6, 0, "x")).collect()
}

fn t0() -> u64 {
    evidence_server::store::now_ms()
}

// ---- what leaves the server ------------------------------------------------

#[test]
fn only_the_fingerprint_leaves_and_the_stamp_is_kept() {
    let fake = Fake::start(Behave::Good);
    let spec = format!("{},chain=http://127.0.0.1:{p}/ca.pem,crl=http://127.0.0.1:{p}/root.crl", fake.url(), p = fake.port);
    let t = T::new("bytes", Some(Stamping::new(cfg(&[spec]))));
    let b = boots();
    let now = t0();
    t.upload(&b[0], now);
    stamp::tick(&t.app, now);

    let reqs = fake.requests();
    let posts: Vec<&Vec<u8>> = reqs.iter().filter(|r| r.starts_with(b"POST ")).collect();
    assert_eq!(posts.len(), 1, "one request per stamp");
    let req = posts[0];
    let end = req.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = std::str::from_utf8(&req[..end]).unwrap();
    let mut lines = head.split("\r\n");
    assert_eq!(lines.next(), Some("POST /tsr HTTP/1.1"));
    let mut names = Vec::new();
    for l in lines {
        let (n, v) = l.split_once(':').unwrap();
        let (n, v) = (n.to_ascii_lowercase(), v.trim());
        match n.as_str() {
            "host" => assert_eq!(v, format!("127.0.0.1:{}", fake.port)),
            "user-agent" => assert_eq!(v, "rfc3161-client", "a generic user agent"),
            "accept" => assert_eq!(v, "*/*"),
            "content-type" => assert_eq!(v, "application/timestamp-query"),
            "content-length" => assert_eq!(v, "69"),
            // Fixed, so a workspace-wide build (where another crate turns on
            // ureq's gzip) sends the same bytes.
            "accept-encoding" => assert_eq!(v, "identity"),
            other => panic!("unexpected header {other}: {v}"),
        }
        names.push(n);
    }
    assert!(!names.iter().any(|n| n == "cookie" || n == "referer" || n == "authorization" || n == "x-actor"));
    // The body: a TimeStampReq of 69 bytes, the imprint, a nonce, nothing else.
    let body = &req[end + 4..];
    let (imprint, d_key_id) = {
        let s = t.app.lock();
        let d = s.device(&t.id).unwrap();
        let (n, h) = (1u64, verify::unhex(&d.audit[1].hash).unwrap());
        (bundle::audit_imprint(&d.info.key_id, n, &h.try_into().unwrap()), d.info.key_id.clone())
    };
    let nonce: [u8; 8] = body[58..66].try_into().unwrap();
    assert_eq!(body, evidence_server::tsa::request_der(&imprint, nonce).as_slice(), "exactly the request for the access-log imprint");
    for secret in [d_key_id.as_bytes(), b"bench P4", b"back garden", b"tester", b"uploader"] {
        assert!(!req.windows(secret.len()).any(|w| w == secret), "{:?} left the server", String::from_utf8_lossy(secret));
    }
    // The GETs for the chain and the list carry the same generic headers.
    for g in reqs.iter().filter(|r| r.starts_with(b"GET ")) {
        let h = String::from_utf8_lossy(g).to_ascii_lowercase();
        assert!(h.contains("user-agent: rfc3161-client") && !h.contains("cookie") && !h.contains("referer"), "{h}");
    }

    // Kept: one stamp, its audit entry, the chain and the list.
    let s = t.stamps();
    assert_eq!(s.len(), 1);
    let st = &s[0];
    assert_eq!((st.covers.as_str(), st.audit_n, st.kind.as_str()), (bundle::COVERS_AUDIT_HEAD, Some(1), "rfc3161"));
    assert_eq!(st.imprint, hex(&imprint));
    assert_eq!(st.nonce.as_deref(), Some(hex(&nonce).as_str()));
    assert!(st.certs.is_some() && st.revocation.is_some(), "{st:?}");
    assert_eq!(st.queued_utc_ms, Some(now));
    let token = evidence::b64::decode_vec(&st.token).unwrap();
    evidence::tsp::check_reply(&token, &imprint, &nonce).unwrap();
    let audit = t.app.lock().device(&t.id).unwrap().audit.clone();
    let last: bundle::AuditEntry = serde_json::from_str(&audit.last().unwrap().entry).unwrap();
    assert_eq!(last.action, "timestamp");
    assert_eq!(last.detail["token_sha256"], hex(&evidence::sha256(&token)));
    assert!(t.failures().is_empty());

    // Nothing more is owed: the next tick asks nothing.
    stamp::tick(&t.app, now + 1000);
    assert_eq!(fake.requests().iter().filter(|r| r.starts_with(b"POST ")).count(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_export_carries_the_stamps_and_the_checker_reads_them() {
    let good = Fake::start(Behave::Good);
    let bad = Fake::start(Behave::Http500);
    let t = T::new("export", Some(Stamping::new(cfg(&[good.url(), bad.url()]))));
    let b = boots();
    let now = t0();
    t.upload(&b[0], now);
    let app = t.app.clone();
    tokio::task::spawn_blocking(move || stamp::tick(&app, now)).await.unwrap();
    let (s, json) = t.call("GET", &format!("/api/devices/{}/export", t.id), Body::empty()).await;
    assert_eq!(s, StatusCode::OK);
    let bundle: bundle::Bundle = serde_json::from_slice(&json).unwrap();
    assert_eq!(bundle.format, bundle::FORMAT);
    let st = bundle.stamping.as_ref().unwrap();
    assert!(st.enabled);
    assert_eq!(st.outstanding.len(), 1, "{st:?}");
    assert_eq!(st.outstanding[0].authority, bad.url());
    assert_eq!(st.failures.len(), 1);
    assert!(st.failures[0].reason.contains("HTTP 500"), "{:?}", st.failures);
    let r = verify::verify_bundle(&json, &verify::Options { allow_test_key: true, ..Default::default() });
    let fails: Vec<_> = r.findings.iter().filter(|f| f.level == Level::Fail).collect();
    assert!(fails.is_empty(), "{fails:?}");
    let notes: Vec<&str> = r.findings.iter().filter(|f| f.level == Level::Note).map(|f| f.what.as_str()).collect();
    assert!(notes.iter().any(|n| n.contains("existed by") && n.contains("within a minute of the newest upload it covers was stored")), "{notes:?}");
    assert!(notes.iter().any(|n| n.contains("outstanding since")), "{notes:?}");
    assert!(notes.iter().any(|n| n.contains("1 attempt(s)") && n.contains("HTTP 500")), "{notes:?}");
}

// ---- a hostile or broken authority ---------------------------------------

#[test]
fn a_reply_that_is_not_the_stamp_asked_for_is_a_failure_never_a_stamp() {
    let elsewhere = Fake::start(Behave::Good);
    let cases = [
        (Behave::Refuse, "PKIStatus 2"),
        (Behave::Garbage, "not a TimeStampResp"),
        (Behave::WrongImprint, "not over the imprint"),
        (Behave::WrongNonce, "nonce"),
        (Behave::NoNonce, "nonce"),
        (Behave::Oversize, "longer than"),
        // Whether this build's HTTP client can decompress (a workspace-wide
        // build turns ureq's gzip on) must not matter: refused either way.
        (Behave::Gzipped, "compressed"),
        (Behave::Unframed, "no length"),
        (Behave::Http500, "HTTP 500"),
        (Behave::Redirect(elsewhere.port), "HTTP 302"),
        (Behave::Slow(4_000), "timed out"),
    ];
    for (i, (b, why)) in cases.into_iter().enumerate() {
        let fake = Fake::start(b);
        let t = T::new(&format!("hostile{i}"), Some(Stamping::new(cfg(&[fake.url()]))));
        let now = t0();
        t.upload(&boots()[0], now);
        let started = Instant::now();
        stamp::tick(&t.app, now);
        assert!(started.elapsed() < Duration::from_secs(4), "{b:?}: the deadline holds ({:?})", started.elapsed());
        assert!(t.stamps().is_empty(), "{b:?} was stored as a stamp");
        let f = t.failures();
        assert_eq!(f.len(), 1, "{b:?}");
        assert!(f[0].reason.to_lowercase().contains(&why.to_lowercase()), "{b:?}: {}", f[0].reason);
        assert_eq!(f[0].authority, fake.url());
        // The access log has no stamp entry.
        let audit = t.app.lock().device(&t.id).unwrap().audit.clone();
        assert!(!audit.iter().any(|l| l.entry.contains("\"timestamp\"")), "{b:?}");
        // And the queue still owes it.
        assert!(t.app.lock().device(&t.id).unwrap().outstanding_for(&fake.url()).is_some());
    }
    assert!(elsewhere.requests().is_empty(), "a redirect was followed to another host");
}

#[test]
fn each_service_is_asked_independently_with_its_own_back_off() {
    let good = Fake::start(Behave::Good);
    let slow = Fake::start(Behave::Slow(4_000));
    let t = T::new("independent", Some(Stamping::new(cfg(&[slow.url(), good.url()]))));
    let now = t0();
    t.upload(&boots()[0], now);
    stamp::tick(&t.app, now);
    let s = t.stamps();
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].authority, good.url());
    assert_eq!(t.failures().len(), 1);
    assert_eq!(t.failures()[0].authority, slow.url());
    // Within the back-off nothing is asked again; after it, it is.
    let before = slow.requests().len();
    stamp::tick(&t.app, now + 30_000);
    assert_eq!(slow.requests().len(), before);
    stamp::tick(&t.app, now + 61_000);
    assert_eq!(slow.requests().len(), before + 1);
    assert_eq!(t.failures().len(), 2);
    assert_eq!(good.requests().len(), 1, "what is received is not asked for again");
}

// ---- uploads never wait ----------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_upload_never_waits_for_a_service() {
    let hang = Fake::start(Behave::Slow(5_000));
    let mut c = cfg(&[hang.url()]);
    c.timeout = Duration::from_secs(5);
    let t = T::new("nowait", Some(Stamping::new(c)));
    let b = boots();
    let (s, _) = t.call("POST", &format!("/api/devices/{}/batches", t.id), Body::from(b[0].bytes.clone())).await;
    assert_eq!(s, StatusCode::CREATED);
    // The worker is now waiting on the service.
    let app = t.app.clone();
    let worker = std::thread::spawn(move || stamp::tick(&app, t0()));
    std::thread::sleep(Duration::from_millis(300));
    let started = Instant::now();
    let (s, body) = t.call("POST", &format!("/api/devices/{}/batches", t.id), Body::from(b[1].bytes.clone())).await;
    assert_eq!(s, StatusCode::CREATED, "{}", String::from_utf8_lossy(&body));
    assert!(started.elapsed() < Duration::from_secs(2), "the upload waited {:?}", started.elapsed());
    worker.join().unwrap();
}

// ---- the queue -------------------------------------------------------------

#[test]
fn the_queue_survives_a_restart() {
    let down = Fake::start(Behave::Http500);
    let mut t = T::new("restart", Some(Stamping::new(cfg(&[down.url()]))));
    let b = boots();
    let now = t0();
    t.upload(&b[0], now);
    stamp::tick(&t.app, now);
    assert_eq!((t.queue_len(), t.stamps().len(), t.failures().len()), (1, 0, 1));
    // Restart with the service back: the queued stamp is asked for again.
    let up = Fake::start(Behave::Good);
    // (Same URL key: point the new run's service at the working fake but keep
    // the outstanding entry by giving it the old URL's name.)
    t.reopen(Some(Stamping::with_stampers(cfg(&[down.url()]), vec![Box::new(Renamed(Rfc3161::new(Authority::parse(&up.url()).unwrap(), Duration::from_secs(2)), down.url()))])));
    assert_eq!((t.queue_len(), t.failures().len()), (1, 1), "queue and failures re-read");
    stamp::tick(&t.app, now + 1000);
    let s = t.stamps();
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].audit_n, Some(1));
    assert_eq!(t.queue_len(), 1, "nothing new queued: the old entry was answered");
}

/// A service under another name (to stand in for one that came back).
struct Renamed(Rfc3161, String);

impl Stamper for Renamed {
    fn authority(&self) -> &str {
        &self.1
    }
    fn stamp(&self, imprint: &[u8; 32]) -> Result<stamp::Stamped, String> {
        self.0.stamp(imprint)
    }
}

#[test]
fn an_upload_stored_just_before_a_restart_is_still_stamped() {
    let fake = Fake::start(Behave::Good);
    let mut t = T::new("unqueued", Some(Stamping::new(cfg(&[fake.url()]))));
    let now = t0();
    t.upload(&boots()[0], now);
    // The server stops before its worker ran.
    t.reopen(Some(Stamping::new(cfg(&[fake.url()]))));
    stamp::tick(&t.app, now + 1000);
    assert_eq!(t.queue_len(), 1);
    assert_eq!(t.stamps().len(), 1);
}

#[test]
fn coalescing_through_the_worker_with_a_fake_clock() {
    let fake = Fake::start(Behave::Good);
    let t = T::new("coalesce", Some(Stamping::new(cfg(&[fake.url()]))));
    let b = boots();
    let now = t0();
    t.upload(&b[0], now);
    stamp::tick(&t.app, now);
    assert_eq!(t.queue_len(), 1);
    t.upload(&b[1], now + 15_000);
    stamp::tick(&t.app, now + 16_000);
    assert_eq!(t.queue_len(), 1, "within ten minutes of the last: waits");
    stamp::tick(&t.app, now + 15_000 + 119_000);
    assert_eq!(t.queue_len(), 1);
    stamp::tick(&t.app, now + 15_000 + 120_000);
    assert_eq!(t.queue_len(), 2, "uploads stopped for two minutes: stamped");
    assert_eq!(t.stamps().len(), 2);
    // The newest stamp covers the newest upload.
    let s = t.stamps();
    let d = t.app.lock();
    let (n, _) = d.device(&t.id).unwrap().newest_upload().unwrap();
    assert!(s[1].audit_n.unwrap() >= n);
}

// ---- off unless turned on, and what the page is told ---------------------

#[tokio::test]
async fn stamping_is_off_unless_turned_on() {
    let fake = Fake::start(Behave::Good);
    let mut c = cfg(&[fake.url()]);
    c.enabled = false;
    let t = T::new("off", Some(Stamping::new(c)));
    t.upload(&boots()[0], t0());
    stamp::tick(&t.app, t0());
    assert_eq!(t.queue_len(), 0);
    assert!(fake.requests().is_empty());
    let (s, _) = t.call("POST", &format!("/api/devices/{}/timestamp", t.id), Body::empty()).await;
    assert_eq!(s, StatusCode::NOT_IMPLEMENTED);
    let t2 = T::new("none", None);
    let (s, body) = t2.call("GET", "/api/stamping", Body::empty()).await;
    assert_eq!(s, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["enabled"], false);
    assert!(v["warnings"][0].as_str().unwrap().contains("OFF"), "{v}");
    assert!(v["what_it_shows"].as_str().unwrap().contains("does not show when the records were written"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_page_is_told_about_too_few_services_backlog_failures_and_stale_stamps() {
    let down = Fake::start(Behave::Garbage);
    let mut c = cfg(&[down.url()]);
    c.alarm_ms = 60_000;
    let t = T::new("page", Some(Stamping::new(c)));
    let now = t0();
    t.upload(&boots()[0], now);
    let app = t.app.clone();
    tokio::task::spawn_blocking(move || stamp::tick(&app, now)).await.unwrap();
    let (_, body) = t.call("GET", "/api/stamping", Body::empty()).await;
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!((v["enabled"].as_bool(), v["independent"].as_u64()), (Some(true), Some(1)));
    assert!(v["warnings"][0].as_str().unwrap().contains("fewer than two"), "{v}");
    let (_, body) = t.call("GET", "/api/devices", Body::empty()).await;
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let st = &v[0]["stamping"];
    let svc = &st["services"][0];
    assert_eq!(svc["authority"], down.url());
    assert!(svc["outstanding"]["audit_n"].is_u64(), "{st}");
    assert_eq!(svc["failures"], 1);
    assert!(svc["last_failure"]["reason"].as_str().unwrap().contains("not a TimeStampResp"));
    // A minute later nothing has been stamped: stale (the device list uses
    // the real clock, so look through the library).
    let s = t.app.lock();
    let d = s.device(&t.id).unwrap();
    assert!(!t.app.stamping.as_ref().unwrap().stale(d, now + 59_000));
    assert!(t.app.stamping.as_ref().unwrap().stale(d, now + 61_000 + 60_000));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stamp_now_by_hand() {
    let fake = Fake::start(Behave::Good);
    let t = T::new("byhand", Some(Stamping::new(cfg(&[fake.url()]))));
    let now = t0();
    t.upload(&boots()[0], now);
    let app = t.app.clone();
    tokio::task::spawn_blocking(move || stamp::tick(&app, now)).await.unwrap();
    assert_eq!(t.queue_len(), 1);
    let (s, _) = t.call("POST", &format!("/api/devices/{}/timestamp", t.id), Body::empty()).await;
    assert_eq!(s, StatusCode::ACCEPTED);
    let app = t.app.clone();
    tokio::task::spawn_blocking(move || stamp::tick(&app, now + 1000)).await.unwrap();
    assert_eq!(t.queue_len(), 2, "queued at once, not after ten minutes");
    assert_eq!(t.stamps().len(), 2);
}
