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
//! | `GET /static/...` | the page's script and style, and the map library (Leaflet 1.9.4, served from here) | - |
//! | `POST /api/devices` `{public_key, name}` | register a unit by its public key | register |
//! | `GET /api/devices` | units, stored ranges | - |
//! | `GET /api/devices/{id}` | one unit: next record number per boot (for uploaders; needs the token) | - |
//! | `POST /api/devices/{id}/batches` (octet-stream, at most [`MAX_BATCH_BYTES`]) | one boot's records back to back, ending in a signed checkpoint, or ending exactly at the record a later boot's stored continuity record names (a boot's unsigned tail) | upload |
//! | `GET /api/devices/{id}/detections` | detections for the map and list | view |
//! | `GET /api/devices/{id}/audit` | the unit's audit chain | view |
//! | `POST /api/devices/{id}/annotations` `{boot, seq, text}` | a note on a record | annotate |
//! | `GET /api/devices/{id}/export` | the export bundle (header `X-Bundle-SHA256`) | export |
//! | `POST /api/devices/{id}/timestamp` | ask for an outside time-stamp of the access log now (stamping must be on) | timestamp (when it arrives) |
//! | `GET /api/stamping` | outside time-stamping: on or off, RFC 3161 services, OpenTimestamps calendars, independent witnesses, warnings | - |
//!
//! Who: the `X-Actor` header names the person (bench: not authenticated).
//! With `--token`, every call except uploads needs
//! `Authorization: Bearer <token>`. Uploads are authenticated by the unit's
//! own signatures, not by the uploader. Without `--token`, registering a unit
//! is refused unless the server was started with `--open-registration`.
//!
//! **Limits.** A batch is checked (every link and signature) without holding
//! the store, at most [`App::verify_slots`] at a time, so a large or slow
//! upload does not stall other requests; only the final overlap and link
//! checks and the write hold it. Bodies are capped ([`MAX_BATCH_BYTES`],
//! [`MAX_JSON_BYTES`]); a unit's name and a note have length caps.
//!
//! **The page** loads its script, style and map library from this server
//! only, under a Content-Security-Policy that allows no other script source
//! (map tiles come from OpenStreetMap as images).

pub mod lines;
pub mod ots;
pub mod plan;
pub mod stamp;
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
    /// Each registered unit's public key, outside the store's lock, so an
    /// upload can be checked while the store is busy.
    keys: std::sync::RwLock<std::collections::HashMap<String, [u8; 65]>>,
    /// Uploads being checked at once (each holds its batch in memory and a
    /// CPU while it checks signatures).
    pub verify_slots: tokio::sync::Semaphore,
    pub token: Option<String>,
    /// Let anyone register a unit when there is no `--token`. Off by
    /// default: without a token, registration is refused.
    pub open_registration: bool,
    /// Name written into bundles (e.g. the server's URL).
    pub server_name: String,
    /// Outside time-stamping (`None`: not configured; see [`stamp`]).
    pub stamping: Option<stamp::Stamping>,
}

impl App {
    /// The store. A handler that panicked while holding the lock poisons it;
    /// every later request would panic too, so one bad request would take the
    /// server down. The store's writes are appends, so carrying on is safe.
    pub fn lock(&self) -> std::sync::MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The largest upload taken (records are at most 1 KiB each).
pub const MAX_BATCH_BYTES: usize = 16 << 20;
/// The largest JSON body taken (registration, notes).
pub const MAX_JSON_BYTES: usize = 64 << 10;
/// A unit's name, in characters.
pub const MAX_NAME_CHARS: usize = 100;
/// A note on a record, in characters.
pub const MAX_NOTE_CHARS: usize = 4000;

impl App {
    pub fn new(store: Store, token: Option<String>, open_registration: bool, server_name: String, stamping: Option<stamp::Stamping>) -> Self {
        if let Some(st) = &stamping {
            st.restore(&store);
        }
        let keys = store.devices.iter().map(|(id, d)| (id.clone(), d.key)).collect();
        let cpus = std::thread::available_parallelism().map_or(2, |n| n.get());
        App {
            store: Mutex::new(store),
            keys: std::sync::RwLock::new(keys),
            verify_slots: tokio::sync::Semaphore::new(cpus.clamp(1, 8)),
            token,
            open_registration,
            server_name,
            stamping,
        }
    }

    fn key_of(&self, id: &str) -> Option<[u8; 65]> {
        self.keys.read().unwrap_or_else(std::sync::PoisonError::into_inner).get(id).copied()
    }
}

pub type Shared = Arc<App>;

pub fn router(app: Shared) -> Router {
    use axum::extract::DefaultBodyLimit;
    Router::new()
        .route("/", get(page))
        .route("/static/{file}", get(static_file))
        .route("/api/devices", get(list_devices).post(register))
        .route("/api/devices/{id}", get(device))
        .route("/api/devices/{id}/batches", post(upload).layer(DefaultBodyLimit::max(MAX_BATCH_BYTES)))
        .route("/api/devices/{id}/detections", get(detections))
        .route("/api/devices/{id}/audit", get(audit))
        .route("/api/devices/{id}/annotations", post(annotate))
        .route("/api/devices/{id}/export", get(export))
        .route("/api/devices/{id}/timestamp", post(timestamp))
        .route("/api/stamping", get(stamping))
        .layer(DefaultBodyLimit::max(MAX_JSON_BYTES))
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

/// What the page may load: its own script and style from here, map tiles
/// as images from OpenStreetMap, data from here. No inline script, no other
/// script source, no framing.
pub const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data: https://tile.openstreetmap.org; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

fn hardened(mut r: Response) -> Response {
    let h = r.headers_mut();
    h.insert("content-security-policy", HeaderValue::from_static(CSP));
    h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    // Tile requests would otherwise tell OpenStreetMap this server's address.
    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    r
}

async fn page() -> Response {
    hardened(Html(include_str!("page.html")).into_response())
}

/// The page's own files and the vendored map library (Leaflet 1.9.4,
/// BSD-2-Clause, `static/LEAFLET-LICENSE`), compiled into the server.
async fn static_file(Path(file): Path<String>) -> Response {
    let (body, ct): (&'static [u8], &'static str) = match file.as_str() {
        "page.js" => (include_bytes!("static/page.js"), "text/javascript; charset=utf-8"),
        "page.css" => (include_bytes!("static/page.css"), "text/css; charset=utf-8"),
        "leaflet.js" => (include_bytes!("static/leaflet.js"), "text/javascript; charset=utf-8"),
        "leaflet.css" => (include_bytes!("static/leaflet.css"), "text/css; charset=utf-8"),
        "LEAFLET-LICENSE" => (include_bytes!("static/LEAFLET-LICENSE"), "text/plain; charset=utf-8"),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let mut r = hardened(body.into_response());
    r.headers_mut().insert("content-type", HeaderValue::from_static(ct));
    r
}

async fn list_devices(State(app): State<Shared>, h: HeaderMap) -> Response {
    if let Err(r) = authorized(&app, &h) {
        return r;
    }
    let s = app.lock();
    let v: Vec<_> = s
        .devices
        .values()
        .map(|d| {
            let stamps = app.stamping.as_ref().map(|st| st.status(d, store::now_ms()));
            serde_json::json!({ "key_id": d.info.key_id, "name": d.info.name, "registered_utc_ms": d.info.registered_utc_ms, "batches": d.batches.len(), "ranges": d.ranges(), "detections": d.points.len(), "problems": d.load_problems, "test_key": d.key == evidence::sign::public_test_key(), "stamping": stamps })
        })
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
    if app.token.is_none() && !app.open_registration {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "registration is disabled: start the server with --token (and send it), or with --open-registration on a bench network" })),
        )
            .into_response();
    }
    if r.name.chars().count() > MAX_NAME_CHARS || r.name.chars().any(char::is_control) {
        return err(StoreError::Rejected(format!("name: at most {MAX_NAME_CHARS} characters, no control characters")));
    }
    let who = actor(&h, c.0);
    let mut s = app.lock();
    match s.register(&r.public_key, &r.name, &who) {
        Ok(info) => {
            if let Some(d) = s.devices.get(&info.key_id) {
                app.keys.write().unwrap_or_else(std::sync::PoisonError::into_inner).insert(info.key_id.clone(), d.key);
            }
            (StatusCode::CREATED, Json(info)).into_response()
        }
        Err(e) => err(e),
    }
}

/// Status for uploaders: what the server already has (no record contents).
async fn device(State(app): State<Shared>, Path(id): Path<String>, h: HeaderMap) -> Response {
    if let Err(r) = authorized(&app, &h) {
        return r;
    }
    let s = app.lock();
    match s.device(&id) {
        Ok(d) => Json(serde_json::json!({ "key_id": d.info.key_id, "name": d.info.name, "next_seq": d.next_seq(), "ranges": d.ranges(), "batches": d.batches.len() })).into_response(),
        Err(e) => err(e),
    }
}

/// Checked without the store (signatures are the slow part), then kept
/// under it. Two uploads of the same records may both pass the first step;
/// the second then finds the first's records stored and is refused.
async fn upload(State(app): State<Shared>, Path(id): Path<String>, c: Conn, h: HeaderMap, body: Bytes) -> Response {
    let who = actor(&h, c.0);
    let Some(key) = app.key_of(&id) else {
        return err(StoreError::NotFound);
    };
    let checked = {
        let Ok(_slot) = app.verify_slots.acquire().await else {
            return err(StoreError::Io("shutting down".into()));
        };
        let b = body.clone();
        match tokio::task::spawn_blocking(move || store::check_batch(&b, &key)).await {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => return err(e),
            Err(e) => return err(StoreError::Io(e.to_string())),
        }
    };
    let committed = app.lock().device_mut(&id).and_then(|d| d.commit(&checked, &body, &who));
    match committed {
        Ok(res) => {
            // The store is released: stamping only marks the unit here; the
            // worker asks the services later.
            if let Some(st) = &app.stamping {
                st.on_upload(&id, store::now_ms());
            }
            (StatusCode::CREATED, Json(res)).into_response()
        }
        Err(e) => err(e),
    }
}

async fn detections(State(app): State<Shared>, Path(id): Path<String>, c: Conn, h: HeaderMap) -> Response {
    if let Err(r) = authorized(&app, &h) {
        return r;
    }
    let who = actor(&h, c.0);
    let mut s = app.lock();
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
    let mut s = app.lock();
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
    let mut s = app.lock();
    let d = match s.device_mut(&id) {
        Ok(d) => d,
        Err(e) => return err(e),
    };
    let Some(boot) = evidence::verify::unhex(&a.boot).and_then(|v| <[u8; 8]>::try_from(v).ok()) else {
        return err(StoreError::Rejected("boot must be 16 hex digits".into()));
    };
    if a.text.chars().count() > MAX_NOTE_CHARS {
        return err(StoreError::Rejected(format!("a note is at most {MAX_NOTE_CHARS} characters")));
    }
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
    // What to send is fixed under the store; reading the batch files and
    // building the bundle is not (stored batches never change).
    let plan = match app.lock().device(&id) {
        Ok(d) => d.bundle_plan(&app.server_name).stamping(app.stamping.as_ref().map(|st| st.report(d))),
        Err(e) => return err(e),
    };
    let built = tokio::task::spawn_blocking(move || {
        let bundle = plan.build()?;
        let body = serde_json::to_vec_pretty(&bundle).unwrap();
        Ok::<_, StoreError>((bundle.batches.len(), body))
    })
    .await;
    let (batches, body) = match built {
        Ok(Ok(b)) => b,
        Ok(Err(e)) => return err(e),
        Err(e) => return err(StoreError::Io(e.to_string())),
    };
    let sha = evidence::verify::hex(&evidence::sha256(&body));
    let mut s = app.lock();
    let d = match s.device_mut(&id) {
        Ok(d) => d,
        Err(e) => return err(e),
    };
    // Logged after it is built, so the bundle holds the chain up to here
    // and this entry records exactly what left the server.
    if let Err(e) = d.audit(&who, "export", serde_json::json!({ "bundle_sha256": sha, "bytes": body.len(), "batches": batches })) {
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

/// Stamp the access log at the next round of the worker (stamping on).
async fn timestamp(State(app): State<Shared>, Path(id): Path<String>, h: HeaderMap) -> Response {
    if let Err(r) = authorized(&app, &h) {
        return r;
    }
    let Some(st) = app.stamping.as_ref().filter(|s| s.config.enabled) else {
        return (StatusCode::NOT_IMPLEMENTED, Json(serde_json::json!({ "error": "outside time-stamping is off (the server's operator turns it on with --stamp); see docs/EVIDENCE.md" }))).into_response();
    };
    if let Err(e) = app.lock().device(&id) {
        return err(e);
    }
    st.stamp_soon(&id, store::now_ms());
    (StatusCode::ACCEPTED, Json(serde_json::json!({ "queued": true, "services": st.authorities() }))).into_response()
}

/// The stamping set-up, for the page.
async fn stamping(State(app): State<Shared>, h: HeaderMap) -> Response {
    if let Err(r) = authorized(&app, &h) {
        return r;
    }
    let cfg = app.stamping.as_ref().map(|s| s.config.clone()).unwrap_or_default();
    Json(serde_json::json!({
        "enabled": cfg.enabled,
        "services": cfg.authorities.iter().map(|a| a.url.clone()).collect::<Vec<_>>(),
        "ots_calendars": cfg.active_calendars(),
        "independent": cfg.independent(),
        "warnings": cfg.warnings(),
        "what_it_shows": stamp::WHAT_IT_SHOWS,
        "witnesses": stamp::WITNESSES,
        "ots_shows": stamp::OTS_SHOWS,
        "every_min": cfg.every_ms / 60_000,
        "alarm_h": cfg.alarm_ms / 3_600_000,
    }))
    .into_response()
}
