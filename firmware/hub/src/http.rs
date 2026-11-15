//! HTTP server (picoserve on embassy-net).
//!
//! Routes:
//! - `GET /`                 the phone page (`webui::INDEX_HTML_GZ`, gzip, ETag/304)
//! - `GET /api/state`        state JSON, streamed in chunks (see `api.rs`)
//! - `POST /api/own-pos`     `{lat,lon,alt}` or `{clear:true}`
//! - `GET /api/flags`         possible-violation flags from a P4 hub (`flags.rs`)
//! - `GET|POST /api/cot`     read / set the CoT multicast toggle (`{"on":bool}`)
//! - anything else           302 to `/`. This covers every OS's captive-portal
//!   probe (Android `/generate_204`, Apple `/hotspot-detect.html`, Windows
//!   `/connecttest.txt`, Firefox `/canonical.html`, ...): the captive DNS
//!   sends their hosts here, a redirect instead of the expected reply tells
//!   the OS "portal", and it opens our page. The `Location` is relative so it
//!   also works on the station-mode LAN, where the hub has another address.
//!
//! One socket per task; `AP_TASKS` tasks serve phones on the hotspot, so 2-3
//! phones (each with a page load plus a 1 Hz poll) are served concurrently.

use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

use embassy_net::Stack;
use picoserve::io::Read;
use picoserve::request::{Path, Request};
use picoserve::response::chunked::{ChunkWriter, ChunkedResponse, Chunks, ChunksWritten};
use picoserve::response::{IntoResponse, Response, ResponseWriter, StatusCode};
use picoserve::extract::FromRequestParts;
use picoserve::request::RequestParts;
use picoserve::routing::{PathRouterService, get, post};
use picoserve::{ResponseSent, Router};
use serde::Deserialize;

use crate::hub::{Hub, now_ms};
use crate::{api, cot};

/// HTTP tasks (= TCP sockets) on the hotspot. Measured: with 4, three
/// clients polling in parallel got ~1 in 4 connections refused (all sockets
/// busy finishing a previous request). 8 covers MAX_CLIENTS phones holding a
/// keep-alive poll connection plus a page load each. ~7 KB of RAM per task.
pub const AP_TASKS: usize = 8;
/// HTTP tasks on the station LAN, when station mode is built.
pub const STA_TASKS: usize = 1;

/// Requests served, for the USB status line.
pub static REQUESTS: AtomicU32 = AtomicU32::new(0);

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
struct CaptiveRedirect;

impl PathRouterService<()> for CaptiveRedirect {
    async fn call_path_router_service<R: Read, W: ResponseWriter<Error = R::Error>>(
        &self,
        _state: &(),
        _params: (),
        _path: Path<'_>,
        request: Request<'_, R>,
        response_writer: W,
    ) -> Result<ResponseSent, W::Error> {
        REQUESTS.fetch_add(1, Relaxed);
        (StatusCode::FOUND, ("Location", "/"), ("Cache-Control", "no-store"), "")
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

async fn serve(id: usize, stack: Stack<'static>, hub: &'static Hub) -> ! {
    let app = Router::from_service(CaptiveRedirect)
        .route(
            "/",
            get(async |cached: PageCached| {
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
                    .with_header("Content-Encoding", webui::CONTENT_ENCODING)
                    .with_header("ETag", webui::ETAG)
                    .with_header("Cache-Control", webui::CACHE_CONTROL)
            }),
        )
        .route(
            "/api/state",
            get(async move || {
                REQUESTS.fetch_add(1, Relaxed);
                ChunkedResponse::new(StateChunks(hub))
                    .into_response()
                    .with_header("Cache-Control", "no-store")
            }),
        )
        .route(
            "/api/flags",
            get(async move || {
                REQUESTS.fetch_add(1, Relaxed);
                ChunkedResponse::new(FlagsChunks(hub))
                    .into_response()
                    .with_header("Cache-Control", "no-store")
            }),
        )
        .route(
            "/api/own-pos",
            post(async move |body: Body| {
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
                Response::ok(out).with_content_type("application/json").with_status_code(status)
            }),
        )
        .route(
            "/api/cot",
            get(async || {
                REQUESTS.fetch_add(1, Relaxed);
                Response::ok(if cot::enabled() { r#"{"on":true}"# } else { r#"{"on":false}"# })
                    .with_content_type("application/json")
            })
            .post(async |body: Body| {
                REQUESTS.fetch_add(1, Relaxed);
                match serde_json_core::from_slice::<CotReq>(&body) {
                    Ok((req, _)) => {
                        cot::set(req.on);
                        Response::ok(if req.on { r#"{"on":true}"# } else { r#"{"on":false}"# })
                            .with_content_type("application/json")
                    }
                    Err(_) => Response::ok(r#"{"err":"bad json"}"#)
                        .with_content_type("application/json")
                        .with_status_code(StatusCode::BAD_REQUEST),
                }
            }),
        );

    // Keep-alive so a phone's 1 Hz poll reuses its connection (no TCP
    // handshake per poll). An idle persistent connection is closed after 3 s,
    // a little over two missed polls, which frees the socket for another phone.
    let config = picoserve::Config::new(picoserve::Timeouts {
        start_read_request: embassy_time::Duration::from_secs(5),
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
            .listen_and_serve(id, stack, 80, &mut rx, &mut tx)
            .await
            .into_never()
    }
}

#[embassy_executor::task(pool_size = AP_TASKS + STA_TASKS)]
pub async fn task(id: usize, stack: Stack<'static>, hub: &'static Hub) -> ! {
    serve(id, stack, hub).await
}
