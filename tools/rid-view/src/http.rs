//! `--http ADDR`: serve the phone page and its API, as the hub does, so the
//! page can be developed and tested on a laptop with no hardware.
//!
//! - `GET /` the page from the `webui` crate (gzip, with ETag/304)
//! - `GET /api/state` the latest `tracker::json::state` snapshot
//! - `POST /api/own-pos` `{"lat":..,"lon":..,"alt":..}` or `{"clear":true}`
//!
//! Plain `std::net`, one short-lived thread per connection, `Connection:
//! close`. The main loop owns the tracker; it publishes a state snapshot
//! here and receives position changes over a channel, so no lock is held
//! across tracker work.

use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tracker::json::{self, OwnPosRequest};

/// What the server shares with the main loop.
pub struct Shared {
    /// The latest `/api/state` body.
    pub state: Mutex<Vec<u8>>,
    /// Position changes from the page, applied by the main loop.
    pub own_pos: Mutex<Sender<OwnPosRequest>>,
}

/// Largest request head and body accepted. The API's bodies are tiny.
const MAX_HEAD: usize = 8 * 1024;
const MAX_BODY: usize = 4 * 1024;

/// Accept connections on a background thread, forever.
pub fn spawn(listener: TcpListener, shared: Arc<Shared>) {
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || {
                let _ = handle(stream, &shared);
            });
        }
    });
}

struct Request {
    method: String,
    path: String,
    if_none_match: Option<String>,
    body: Vec<u8>,
}

fn read_request(s: &mut TcpStream) -> io::Result<Option<Request>> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if buf.len() > MAX_HEAD {
            return Ok(None);
        }
        let n = s.read(&mut chunk)?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let method = first.next().unwrap_or("").to_string();
    let target = first.next().unwrap_or("");
    let path = target.split('?').next().unwrap_or("").to_string();
    let mut len = 0usize;
    let mut if_none_match = None;
    for l in lines {
        let Some((k, v)) = l.split_once(':') else {
            continue;
        };
        let v = v.trim();
        if k.eq_ignore_ascii_case("content-length") {
            len = v.parse().unwrap_or(usize::MAX);
        } else if k.eq_ignore_ascii_case("if-none-match") {
            if_none_match = Some(v.to_string());
        }
    }
    if len > MAX_BODY {
        return Ok(None);
    }
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < len {
        let n = s.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(len);
    Ok(Some(Request {
        method,
        path,
        if_none_match,
        body,
    }))
}

fn respond(
    s: &mut TcpStream,
    status: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    head_only: bool,
) -> io::Result<()> {
    let mut out = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    let mut bytes = out.into_bytes();
    if !head_only {
        bytes.extend_from_slice(body);
    }
    s.write_all(&bytes)?;
    s.flush()
}

const JSON: (&str, &str) = ("Content-Type", "application/json");
const NO_STORE: (&str, &str) = ("Cache-Control", "no-store");

fn handle(mut s: TcpStream, shared: &Shared) -> io::Result<()> {
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    s.set_write_timeout(Some(Duration::from_secs(5)))?;
    let Some(req) = read_request(&mut s)? else {
        return respond(&mut s, "400 Bad Request", &[], b"", false);
    };
    let head = req.method == "HEAD";
    let get = req.method == "GET" || head;
    match (req.path.as_str(), get, req.method.as_str()) {
        ("/" | "/index.html", true, _) => {
            if req
                .if_none_match
                .as_deref()
                .is_some_and(webui::etag_matches)
            {
                return respond(
                    &mut s,
                    "304 Not Modified",
                    &[("ETag", webui::ETAG)],
                    b"",
                    true,
                );
            }
            respond(
                &mut s,
                "200 OK",
                &[
                    ("Content-Type", webui::CONTENT_TYPE),
                    ("Content-Encoding", webui::CONTENT_ENCODING),
                    ("Cache-Control", webui::CACHE_CONTROL),
                    ("ETag", webui::ETAG),
                ],
                webui::INDEX_HTML_GZ,
                head,
            )
        }
        ("/api/state", true, _) => {
            let body = shared.state.lock().map(|b| b.clone()).unwrap_or_default();
            respond(&mut s, "200 OK", &[JSON, NO_STORE], &body, head)
        }
        ("/api/own-pos", _, "POST") => match json::parse_own_pos(&req.body) {
            Ok(r) => {
                let sent = shared.own_pos.lock().is_ok_and(|tx| tx.send(r).is_ok());
                if sent {
                    respond(&mut s, "200 OK", &[JSON, NO_STORE], b"{\"ok\":true}", false)
                } else {
                    respond(
                        &mut s,
                        "503 Service Unavailable",
                        &[JSON],
                        b"{\"error\":\"shutting down\"}",
                        false,
                    )
                }
            }
            Err(e) => {
                let body = serde_json::json!({ "error": e.message() }).to_string();
                respond(
                    &mut s,
                    "400 Bad Request",
                    &[JSON, NO_STORE],
                    body.as_bytes(),
                    false,
                )
            }
        },
        ("/" | "/index.html" | "/api/state" | "/api/own-pos", _, _) => {
            respond(&mut s, "405 Method Not Allowed", &[], b"", false)
        }
        _ => respond(
            &mut s,
            "404 Not Found",
            &[("Content-Type", "text/plain")],
            b"not found\n",
            head,
        ),
    }
}
