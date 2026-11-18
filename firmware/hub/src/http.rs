//! HTTP server (picoserve on embassy-net).
//!
//! Routes:
//! - `GET /`                 the phone page (`webui::INDEX_HTML_GZ`, gzip, ETag/304)
//! - `GET /api/state`        state JSON, streamed in chunks (see `api.rs`)
//! - `POST /api/own-pos`     `{lat,lon,alt}` or `{clear:true}`
//! - `GET /api/flags`         possible-violation flags from a P4 hub (`flags.rs`)
//! - `GET|POST /api/cot`     read / set the CoT multicast toggle (`{"on":bool}`)
//! - anything else           302 to the page. This covers every OS's
//!   captive-portal probe (Android `/generate_204`, Apple
//!   `/hotspot-detect.html`, Windows `/connecttest.txt`, Firefox
//!   `/canonical.html`, ...): the captive DNS sends their hosts here, a
//!   redirect instead of the expected reply tells the OS "portal", and it
//!   opens our page. The `Location` is absolute, the hub's own address on
//!   that network (192.168.4.1, or its station address), because the probe
//!   came under another name and the page is only served under the hub's.
//!
//! Every route but that redirect checks the request first
//! (`webui::request`): `Host` must be the hub's own address (DNS rebinding),
//! and the two writes also need `Content-Type: application/json` and no
//! foreign `Origin` (cross-site POSTs). On the station LAN writes are off
//! unless `wifi.local.toml` says `allow_writes = true`. Refusals answer 403
//! (421 for a wrong Host) and count in `REFUSED`.
//!
//! Every response carries `X-Content-Type-Options: nosniff` and
//! `Referrer-Policy: no-referrer`; the page also its Content-Security-Policy
//! (`webui::CSP`: only its own inline script and style, by hash).
//!
//! One socket per task; `AP_TASKS` tasks serve phones on the hotspot, so 2-3
//! phones (each with a page load plus a 1 Hz poll) are served concurrently.

use core::fmt::Write as _;
use core::net::Ipv4Addr;
use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

use embassy_net::Stack;
use picoserve::io::Read;
use picoserve::request::{Path, Request};
use picoserve::response::chunked::{ChunkWriter, ChunkedResponse, Chunks, ChunksWritten};
use picoserve::response::{Connection, IntoResponse, Response, ResponseWriter, StatusCode};
use picoserve::extract::FromRequestParts;
use picoserve::request::RequestParts;
use picoserve::routing::{PathRouterService, get, post};
use picoserve::{ResponseSent, Router};
use serde::Deserialize;
use webui::request::{self, Access, Verdict};

use crate::hub::{Hub, now_ms};
use crate::{api, cot, hotspot};

/// HTTP tasks (= TCP sockets) on the hotspot. Measured: with 4, three
/// clients polling in parallel got ~1 in 4 connections refused (all sockets
/// busy finishing a previous request). 8 covers MAX_CLIENTS phones holding a
/// keep-alive poll connection plus a page load each. ~7 KB of RAM per task.
pub const AP_TASKS: usize = 8;
/// HTTP tasks on the station LAN, when station mode is built.
pub const STA_TASKS: usize = 1;

/// Requests served, for the USB status line.
pub static REQUESTS: AtomicU32 = AtomicU32::new(0);
/// Requests refused by `webui::request::check`.
pub static REFUSED: AtomicU32 = AtomicU32::new(0);

/// Station mode lets the LAN change things only when `wifi.local.toml` has
/// `allow_writes = true` (`build.rs`): by default anyone on someone else's
/// network may look, not switch the TAK feed off or move the position.
const STA_WRITES: bool = cfg!(hub_station_writes);

/// The network a server task answers on: the router state.
#[derive(Clone, Copy)]
pub struct Side {
    stack: Stack<'static>,
    hotspot: bool,
}

impl Side {
    pub fn hotspot(stack: Stack<'static>) -> Self {
        Self { stack, hotspot: true }
    }

    #[cfg_attr(not(hub_station), allow(dead_code))]
    pub fn station(stack: Stack<'static>) -> Self {
        Self { stack, hotspot: false }
    }

    /// The hub's address on this network, as clients must name it.
    fn own(&self) -> Option<Ipv4Addr> {
        if self.hotspot { Some(hotspot::IP) } else { self.stack.config_v4().map(|c| c.address.address()) }
    }

    fn writes(&self) -> bool {
        self.hotspot || STA_WRITES
    }
}

/// Route guard: a handler that takes `Allowed` runs only if
/// `webui::request::check` allows the request (a read for GET, a write for
/// POST: `webui::request::route_access`); otherwise the client gets the
/// refusal and the handler never runs. One type for every route, so one
/// state machine: a picoserve `Layer` around the router did the same with
/// ~3.7 KB more static RAM across the HTTP tasks.
struct Allowed;

impl<'r> FromRequestParts<'r, Side> for Allowed {
    type Rejection = Refused;

    async fn from_request_parts(side: &'r Side, parts: &RequestParts<'r>) -> Result<Self, Self::Rejection> {
        let headers = parts.headers();
        let text = |name: &str| headers.get(name).and_then(|v| v.as_str().ok());
        let seen = request::Headers {
            host: text("Host"),
            host_count: headers.iter().filter(|(n, _)| *n == "Host").count(),
            origin: text("Origin"),
            content_type: text("Content-Type"),
        };
        let path = parts.path().encoded();
        let access = request::route_access(parts.method(), path).unwrap_or(Access::Write);
        match request::check(side.own(), access, side.writes(), &seen) {
            Verdict::Allow => Ok(Self),
            // The page under another name (someone typed a site's address,
            // or an OS opened its probe host's root): send the browser to the
            // hub's own address, as the captive redirect does. The page holds
            // no data, so this gives a rebinding attacker nothing.
            Verdict::WrongHost if path == "/" && access == Access::Read => Err(Refused::ToPage(side.own())),
            v => {
                REFUSED.fetch_add(1, Relaxed);
                Err(Refused::Deny(v))
            }
        }
    }
}

/// A refused request: 421 Misdirected Request for a Host that is not the
/// hub, 403 otherwise, with the reason as `{"error":"..."}` (the page shows
/// it); or, for the page itself under a foreign name, a redirect.
enum Refused {
    Deny(Verdict),
    ToPage(Option<Ipv4Addr>),
}

impl IntoResponse for Refused {
    async fn write_to<R: Read, W: ResponseWriter<Error = R::Error>>(
        self,
        connection: Connection<'_, R>,
        response_writer: W,
    ) -> Result<ResponseSent, W::Error> {
        let verdict = match self {
            Refused::Deny(v) => v,
            Refused::ToPage(own) => {
                let url = own_url(own);
                return Response::ok("")
                    .with_status_code(StatusCode::FOUND)
                    .with_header("Location", url.as_str())
                    .with_headers(API_HEADERS)
                    .write_to(connection, response_writer)
                    .await;
            }
        };
        // Fixed bodies: nothing formatted, nothing extra held across the write.
        let (status, body) = match verdict {
            Verdict::WrongHost => (StatusCode::MISDIRECTED_REQUEST, r#"{"error":"wrong host"}"#),
            Verdict::WritesOff => (StatusCode::FORBIDDEN, r#"{"error":"changes are off on this network"}"#),
            Verdict::NotJson => (StatusCode::FORBIDDEN, r#"{"error":"content-type must be application/json"}"#),
            Verdict::CrossOrigin | Verdict::Allow => (StatusCode::FORBIDDEN, r#"{"error":"cross-origin request"}"#),
        };
        Response::ok(body)
            .with_content_type("application/json")
            .with_status_code(status)
            .with_headers(API_HEADERS)
            .write_to(connection, response_writer)
            .await
    }
}

/// Headers of every API response and refusal. One static list rather than
/// a chain of `with_header` calls: each link of a chain adds its own state
/// to every HTTP task's future (static RAM, times `AP_TASKS + STA_TASKS`).
const API_HEADERS: &[(&str, &str)] = &[
    ("Cache-Control", "no-store"),
    ("X-Content-Type-Options", webui::NOSNIFF),
    ("Referrer-Policy", webui::REFERRER_POLICY),
];

/// Headers of the page.
const PAGE_HEADERS: &[(&str, &str)] = &[
    ("Content-Encoding", webui::CONTENT_ENCODING),
    ("ETag", webui::ETAG),
    ("Cache-Control", webui::CACHE_CONTROL),
    ("Content-Security-Policy", webui::CSP),
    ("X-Content-Type-Options", webui::NOSNIFF),
    ("Referrer-Policy", webui::REFERRER_POLICY),
];

/// `http://<the hub's own address>/`.
fn own_url(own: Option<Ipv4Addr>) -> heapless::String<24> {
    let mut url = heapless::String::new();
    let ip = own.unwrap_or(hotspot::IP);
    let _ = write!(url, "http://{ip}/");
    url
}

/// Streams `/api/state` one piece per chunk.
struct StateChunks(&'static Hub);

impl Chunks for StateChunks {
    fn content_type(&self) -> &'static str {
        "application/json"
    }

    async fn write_chunks<W: picoserve::io::Write>(
        self,
        mut writer: ChunkWriter<W>,
    ) -> Result<ChunksWritten, W::Error> {
        let hub = self.0;
        let mut w = api::StateWriter::new(hub, now_ms());
        let mut piece = [0u8; tracker::json::MAX_LINE];
        // The tracker is borrowed only inside `next_piece`, never across the
        // socket write, so a slow phone cannot hold up ingest.
        while let Some(n) = w.next_piece(hub, &mut piece) {
            writer.write_chunk(&piece[..n]).await?;
            // The write usually completes at once (socket buffer); yield
            // anyway so formatting many tracks does not starve other tasks.
            embassy_futures::yield_now().await;
        }
        writer.finalize().await
    }
}

/// Streams `/api/flags` one report per chunk (`flags.rs`).
struct FlagsChunks(&'static Hub);

impl Chunks for FlagsChunks {
    fn content_type(&self) -> &'static str {
        "application/json"
    }

    async fn write_chunks<W: picoserve::io::Write>(
        self,
        mut writer: ChunkWriter<W>,
    ) -> Result<ChunksWritten, W::Error> {
        let hub = self.0;
        let now = now_ms();
        let mut piece = [0u8; tracker::json::MAX_LINE];
        writer.write_chunk(b"{\"flags\":[").await?;
        let mut first = true;
        for i in 0..crate::flags::MAX_REPORTS {
            // Borrow the table only while formatting one report.
            let n = crate::flags::report_json(&hub.flags.borrow(), i, now, first, &mut piece);
            match n {
                None => break,
                Some(0) => {}
                Some(n) => {
                    writer.write_chunk(&piece[..n]).await?;
                    first = false;
                }
            }
        }
        writer.write_chunk(b"]}").await?;
        writer.finalize().await
    }
}

/// Everything not routed: redirect to the page (captive-portal probes).
/// Answers any Host: it returns nothing but the hub's own URL.
struct CaptiveRedirect;

impl PathRouterService<Side> for CaptiveRedirect {
    async fn call_path_router_service<R: Read, W: ResponseWriter<Error = R::Error>>(
        &self,
        side: &Side,
        _params: (),
        _path: Path<'_>,
        request: Request<'_, R>,
        response_writer: W,
    ) -> Result<ResponseSent, W::Error> {
        REQUESTS.fetch_add(1, Relaxed);
        let url = own_url(side.own());
        Response::ok("")
            .with_status_code(StatusCode::FOUND)
            .with_header("Location", url.as_str())
            .with_headers(API_HEADERS)
            .write_to(request.body_connection.finalize().await?, response_writer)
            .await
    }
}

/// Request bodies are small JSON objects; 256 bytes is plenty and bounds RAM.
type Body = heapless::Vec<u8, 256>;

/// `If-None-Match` matches the page's ETag.
struct PageCached(bool);

impl<'r, S> FromRequestParts<'r, S> for PageCached {
    type Rejection = core::convert::Infallible;

    async fn from_request_parts(_state: &'r S, parts: &RequestParts<'r>) -> Result<Self, Self::Rejection> {
        let hit = parts
            .headers()
            .get("If-None-Match")
            .and_then(|v| v.as_str().ok())
            .is_some_and(webui::etag_matches);
        Ok(Self(hit))
    }
}

#[derive(Deserialize)]
struct CotReq {
    on: bool,
}

async fn serve(id: usize, side: Side, hub: &'static Hub) -> ! {
    let app = Router::from_service(CaptiveRedirect)
        .route(
            "/",
            get(async |_: Allowed, cached: PageCached| {
                REQUESTS.fetch_add(1, Relaxed);
                // A 304 with no body when the phone already has this build's
                // page: saves ~16 KB per reload over a slow hotspot.
                let (status, body): (StatusCode, &[u8]) = if cached.0 {
                    (StatusCode::NOT_MODIFIED, &[])
                } else {
                    (StatusCode::OK, webui::INDEX_HTML_GZ)
                };
                Response::ok(body)
                    .with_content_type(webui::CONTENT_TYPE)
                    .with_status_code(status)
                    .with_headers(PAGE_HEADERS)
            }),
        )
        .route(
            "/api/state",
            get(async move |_: Allowed| {
                REQUESTS.fetch_add(1, Relaxed);
                ChunkedResponse::new(StateChunks(hub)).into_response().with_headers(API_HEADERS)
            }),
        )
        .route(
            "/api/flags",
            get(async move |_: Allowed| {
                REQUESTS.fetch_add(1, Relaxed);
                ChunkedResponse::new(FlagsChunks(hub)).into_response().with_headers(API_HEADERS)
            }),
        )
        .route(
            "/api/own-pos",
            post(async move |_: Allowed, body: Body| {
                REQUESTS.fetch_add(1, Relaxed);
                let mut out: heapless::String<160> = heapless::String::new();
                let status = match api::own_pos_request(hub, &body, now_ms()) {
                    Ok(()) => {
                        let _ = out.push_str(r#"{"ok":true}"#);
                        StatusCode::OK
                    }
                    Err(e) => {
                        // The messages are fixed text; escape the quotes they contain.
                        let _ = out.push_str(r#"{"error":""#);
                        for c in e.chars() {
                            if c == '"' {
                                let _ = out.push('\\');
                            }
                            let _ = out.push(c);
                        }
                        let _ = out.push_str("\"}");
                        StatusCode::BAD_REQUEST
                    }
                };
                Response::ok(out).with_content_type("application/json").with_status_code(status).with_headers(API_HEADERS)
            }),
        )
        .route(
            "/api/cot",
            get(async |_: Allowed| {
                REQUESTS.fetch_add(1, Relaxed);
                Response::ok(if cot::enabled() { r#"{"on":true}"# } else { r#"{"on":false}"# })
                    .with_content_type("application/json")
                    .with_headers(API_HEADERS)
            })
            .post(async |_: Allowed, body: Body| {
                REQUESTS.fetch_add(1, Relaxed);
                let (status, text) = match serde_json_core::from_slice::<CotReq>(&body) {
                    Ok((req, _)) => {
                        cot::set(req.on);
                        (StatusCode::OK, if req.on { r#"{"on":true}"# } else { r#"{"on":false}"# })
                    }
                    Err(_) => (StatusCode::BAD_REQUEST, r#"{"error":"bad json"}"#),
                };
                Response::ok(text).with_content_type("application/json").with_status_code(status).with_headers(API_HEADERS)
            }),
        )
        .with_state(side);

    // Keep-alive so a phone's 1 Hz poll reuses its connection (no TCP
    // handshake per poll). An idle persistent connection is closed after 3 s,
    // a little over two missed polls, which frees the socket for another phone.
    // A new connection must send its request within 2 s: all `AP_TASKS`
    // sockets can be held open by idle connections, and a phone's browser
    // sends at once, so a short wait frees them for real clients sooner.
    let config = picoserve::Config::new(picoserve::Timeouts {
        start_read_request: embassy_time::Duration::from_secs(2),
        persistent_start_read_request: embassy_time::Duration::from_secs(3),
        read_request: embassy_time::Duration::from_secs(3),
        write: embassy_time::Duration::from_secs(3),
    })
    .keep_connection_alive();

    let mut rx = [0u8; 1024];
    let mut tx = [0u8; 2048];
    let mut http = [0u8; 1536];
    loop {
        picoserve::Server::new(&app, &config, &mut http)
            .listen_and_serve(id, side.stack, 80, &mut rx, &mut tx)
            .await
            .into_never()
    }
}

#[embassy_executor::task(pool_size = AP_TASKS + STA_TASKS)]
pub async fn task(id: usize, side: Side, hub: &'static Hub) -> ! {
    serve(id, side, hub).await
}
