//! evidence-server: receives evidence batches from Rotorscope units (via a
//! laptop uploader for now), checks every link and signature on arrival,
//! keeps them write-once, keeps its own hash-chained access audit log, shows
//! a map and list, and hands out export bundles `evidence-verify` accepts.
//!
//! HTTP API (JSON unless noted):
//!
//! | method, path | what | audit entry |
//! |---|---|---|
//! | `GET /` | map + list page | (the data call below is logged) |
//! | `POST /api/devices` `{public_key, name}` | register a unit by its public key | register |
//! | `GET /api/devices` | units, stored ranges | - |
//! | `GET /api/devices/{id}` | one unit: next record number per boot (for uploaders) | - |
//! | `POST /api/devices/{id}/batches` (octet-stream) | records back to back, ending in a signed checkpoint | upload |
//! | `GET /api/devices/{id}/detections` | detections for the map and list | view |
//! | `GET /api/devices/{id}/audit` | the unit's audit chain | view |
//! | `POST /api/devices/{id}/annotations` `{boot, seq, text}` | a note on a record | annotate |
//! | `GET /api/devices/{id}/export` | the export bundle (header `X-Bundle-SHA256`) | export |
//! | `POST /api/devices/{id}/timestamp` | outside time-stamp of the newest checkpoint | timestamp |
//!
//! Who: the `X-Actor` header names the person (bench: not authenticated).
//! With `--token`, every call except uploads and the uploader's status call
//! needs `Authorization: Bearer <token>`. Uploads are authenticated by the
//! unit's own signatures, not by the uploader.

pub mod store;
pub mod tsa;

use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use store::{Store, StoreError};

pub struct App {
    pub store: Mutex<Store>,
    pub token: Option<String>,
    /// Name written into bundles (e.g. the server's URL).
    pub server_name: String,
    pub tsa_url: Option<String>,
}

pub type Shared = Arc<App>;

pub fn router(app: Shared) -> Router {
    Router::new()
        .route("/", get(page))
        .route("/api/devices", get(list_devices).post(register))
        .route("/api/devices/{id}", get(device))
        .route("/api/devices/{id}/batches", post(upload))
        .route("/api/devices/{id}/detections", get(detections))
        .route("/api/devices/{id}/audit", get(audit))
        .route("/api/devices/{id}/annotations", post(annotate))
        .route("/api/devices/{id}/export", get(export))
        .route("/api/devices/{id}/timestamp", post(timestamp))
        .layer(axum::extract::DefaultBodyLimit::max(16 << 20))
        .with_state(app)
}

fn err(e: StoreError) -> Response {
    let (code, msg) = match e {
        StoreError::NotFound => (StatusCode::NOT_FOUND, "no such device (register it first)".to_string()),
        StoreError::Conflict(m) => (StatusCode::CONFLICT, m),
        StoreError::Rejected(m) => (StatusCode::UNPROCESSABLE_ENTITY, m),
        StoreError::Io(m) => (StatusCode::INTERNAL_SERVER_ERROR, format!("storage: {m}")),
    };
    (code, Json(serde_json::json!({ "error": msg }))).into_response()
}

/// `X-Actor` plus where the call came from.
fn actor(h: &HeaderMap, from: Option<std::net::SocketAddr>) -> String {
    let who = h.get("x-actor").and_then(|v| v.to_str().ok()).unwrap_or("anonymous");
    let who: String = who.chars().filter(|c| !c.is_control()).take(64).collect();
    match from {
        Some(a) => format!("{who} @{}", a.ip()),
        None => who,
    }
}

#[allow(clippy::result_large_err)]
fn authorized(app: &App, h: &HeaderMap) -> Result<(), Response> {
    let Some(t) = &app.token else { return Ok(()) };
    let ok = h.get("authorization").and_then(|v| v.to_str().ok()).is_some_and(|v| v.strip_prefix("Bearer ") == Some(t.as_str()));
    if ok { Ok(()) } else { Err((StatusCode::UNAUTHORIZED, Json(serde_json::json!({ "error": "needs Authorization: Bearer <token>" }))).into_response()) }
}

/// The caller's address, when the server knows it.
pub struct Conn(Option<std::net::SocketAddr>);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for Conn {
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(parts: &mut axum::http::request::Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Conn(parts.extensions.get::<ConnectInfo<std::net::SocketAddr>>().map(|c| c.0)))
    }
}

async fn page() -> Html<&'static str> {
    Html(include_str!("page.html"))
}

async fn list_devices(State(app): State<Shared>, h: HeaderMap) -> Response {
    if let Err(r) = authorized(&app, &h) {
        return r;
    }
    let s = app.store.lock().unwrap();
    let v: Vec<_> = s
        .devices
        .values()
        .map(|d| serde_json::json!({ "key_id": d.info.key_id, "name": d.info.name, "registered_utc_ms": d.info.registered_utc_ms, "batches": d.batches.len(), "ranges": d.ranges(), "detections": d.points.len(), "problems": d.load_problems }))
        .collect();
    Json(v).into_response()
}

#[derive(Deserialize)]
struct Register {
    public_key: String,
    name: String,
}

async fn register(State(app): State<Shared>, c: Conn, h: HeaderMap, Json(r): Json<Register>) -> Response {
    if let Err(r) = authorized(&app, &h) {
        return r;
    }
    let who = actor(&h, c.0);
    match app.store.lock().unwrap().register(&r.public_key, &r.name, &who) {
        Ok(info) => (StatusCode::CREATED, Json(info)).into_response(),
        Err(e) => err(e),
    }
}

/// Status for uploaders: what the server already has (no record contents).
async fn device(State(app): State<Shared>, Path(id): Path<String>) -> Response {
    let s = app.store.lock().unwrap();
    match s.device(&id) {
        Ok(d) => Json(serde_json::json!({ "key_id": d.info.key_id, "name": d.info.name, "next_seq": d.next_seq(), "ranges": d.ranges(), "batches": d.batches.len() })).into_response(),
        Err(e) => err(e),
    }
}

async fn upload(State(app): State<Shared>, Path(id): Path<String>, c: Conn, h: HeaderMap, body: Bytes) -> Response {
    let who = actor(&h, c.0);
    let mut s = app.store.lock().unwrap();
    let r = s.device_mut(&id).and_then(|d| d.upload(&body, &who));
    match r {
        Ok(res) => (StatusCode::CREATED, Json(res)).into_response(),
        Err(e) => err(e),
    }
}

async fn detections(State(app): State<Shared>, Path(id): Path<String>, c: Conn, h: HeaderMap) -> Response {
    if let Err(r) = authorized(&app, &h) {
        return r;
    }
    let who = actor(&h, c.0);
    let mut s = app.store.lock().unwrap();
    let d = match s.device_mut(&id) {
        Ok(d) => d,
        Err(e) => return err(e),
    };
    if let Err(e) = d.audit(&who, "view", serde_json::json!({ "what": "detections", "count": d.points.len() })) {
        return err(e);
    }
    Json(&d.points).into_response()
}

async fn audit(State(app): State<Shared>, Path(id): Path<String>, c: Conn, h: HeaderMap) -> Response {
    if let Err(r) = authorized(&app, &h) {
        return r;
    }
    let who = actor(&h, c.0);
    let mut s = app.store.lock().unwrap();
    let d = match s.device_mut(&id) {
        Ok(d) => d,
        Err(e) => return err(e),
    };
    if let Err(e) = d.audit(&who, "view", serde_json::json!({ "what": "audit log" })) {
        return err(e);
    }
    Json(&d.audit).into_response()
}

#[derive(Deserialize)]
struct Annotation {
    boot: String,
    seq: u64,
    text: String,
}

async fn annotate(State(app): State<Shared>, Path(id): Path<String>, c: Conn, h: HeaderMap, Json(a): Json<Annotation>) -> Response {
    if let Err(r) = authorized(&app, &h) {
        return r;
    }
    let who = actor(&h, c.0);
    let mut s = app.store.lock().unwrap();
    let d = match s.device_mut(&id) {
        Ok(d) => d,
        Err(e) => return err(e),
    };
    let Some(boot) = evidence::verify::unhex(&a.boot).and_then(|v| <[u8; 8]>::try_from(v).ok()) else {
        return err(StoreError::Rejected("boot must be 16 hex digits".into()));
    };
    if !d.has_record(&boot, a.seq) {
        return err(StoreError::Rejected(format!("no record #{} for boot {}", a.seq, a.boot)));
    }
    match d.audit(&who, "annotate", serde_json::json!({ "boot": a.boot, "seq": a.seq, "text": a.text })) {
        Ok(line) => (StatusCode::CREATED, Json(line)).into_response(),
        Err(e) => err(e),
    }
}

async fn export(State(app): State<Shared>, Path(id): Path<String>, c: Conn, h: HeaderMap) -> Response {
    if let Err(r) = authorized(&app, &h) {
        return r;
    }
    let who = actor(&h, c.0);
    let mut s = app.store.lock().unwrap();
    let d = match s.device_mut(&id) {
        Ok(d) => d,
        Err(e) => return err(e),
    };
    let bundle = match d.bundle(&app.server_name) {
        Ok(b) => b,
        Err(e) => return err(e),
    };
    let body = serde_json::to_vec_pretty(&bundle).unwrap();
    let sha = evidence::verify::hex(&evidence::sha256(&body));
    // Logged after it is built, so the bundle holds the chain up to here
    // and this entry records exactly what left the server.
    if let Err(e) = d.audit(&who, "export", serde_json::json!({ "bundle_sha256": sha, "bytes": body.len(), "batches": bundle.batches.len() })) {
        return err(e);
    }
    let mut r = body.into_response();
    r.headers_mut().insert("content-type", HeaderValue::from_static("application/json"));
    r.headers_mut().insert("x-bundle-sha256", HeaderValue::from_str(&sha).unwrap());
    r.headers_mut().insert(
        "content-disposition",
        HeaderValue::from_str(&format!("attachment; filename=\"evidence-{}.json\"", d.info.key_id)).unwrap(),
    );
    r
}

async fn timestamp(State(app): State<Shared>, Path(id): Path<String>, c: Conn, h: HeaderMap) -> Response {
    if let Err(r) = authorized(&app, &h) {
        return r;
    }
    let who = actor(&h, c.0);
    let Some(url) = app.tsa_url.clone() else {
        return (StatusCode::NOT_IMPLEMENTED, Json(serde_json::json!({ "error": "no time-stamp authority configured (start with --tsa URL); see docs/EVIDENCE.md" }))).into_response();
    };
    let head = {
        let s = app.store.lock().unwrap();
        match s.device(&id) {
            Ok(d) => d.latest_checkpoint(),
            Err(e) => return err(e),
        }
    };
    let Some((boot, seq, hash)) = head else {
        return err(StoreError::Rejected("no checkpoint stored yet".into()));
    };
    let imprint = evidence::verify::bundle::head_imprint(&boot, seq, &hash);
    let u = url.clone();
    let token = match tokio::task::spawn_blocking(move || tsa::request(&u, &imprint)).await {
        Ok(Ok(t)) => t,
        Ok(Err(e)) => return (StatusCode::BAD_GATEWAY, Json(serde_json::json!({ "error": format!("time-stamp authority: {e}") }))).into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": e.to_string() }))).into_response(),
    };
    let t = evidence::verify::bundle::Timestamp {
        kind: "rfc3161".into(),
        authority: url,
        imprint: evidence::verify::hex(&imprint),
        boot: evidence::verify::hex(&boot),
        seq,
        head: evidence::verify::hex(&hash),
        token: evidence::b64::encode_vec(&token),
    };
    let mut s = app.store.lock().unwrap();
    let d = match s.device_mut(&id) {
        Ok(d) => d,
        Err(e) => return err(e),
    };
    if let Err(e) = d.add_timestamp(t.clone()) {
        return err(e.into());
    }
    if let Err(e) = d.audit(&who, "timestamp", serde_json::json!({ "authority": t.authority, "seq": seq, "boot": t.boot, "imprint": t.imprint, "token_sha256": evidence::verify::hex(&evidence::sha256(&token)) })) {
        return err(e);
    }
    (StatusCode::CREATED, Json(t)).into_response()
}
