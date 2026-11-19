//! `--http ADDR`: serve the phone page and its API, as the hub does, so the
//! page can be developed and tested on a laptop with no hardware.
//!
//! - `GET /` the page from the `webui` crate (gzip, with ETag/304)
//! - `GET /api/state` the latest `tracker::json::state` snapshot
//! - `POST /api/own-pos` `{"lat":..,"lon":..,"alt":..}` or `{"clear":true}`
//!
//! Plain `std::net`, one short-lived thread per connection (at most
//! [`MAX_CONNS`] at once), `Connection: close`, [`DEADLINE`] for a whole
//! request. The main loop owns the tracker; it publishes a state snapshot
//! here and receives position changes over a channel, so no lock is held
//! across tracker work.
//!
//! The page runs in a browser, and any other page in that browser can send
//! requests here too, so every request must name this server in `Host` (which
//! stops DNS rebinding) and, when it carries `Origin`, come from the same
//! place; a POST must be `application/json` (which a cross-site form cannot
//! send without a CORS preflight this server never grants).

use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

/// Connections served at once; more are told to come back.
pub const MAX_CONNS: usize = 32;
/// Time to read a whole request (a trickle of bytes cannot hold a slot).
pub const DEADLINE: Duration = Duration::from_secs(10);

/// A count of open connections, capped.
pub struct Limit {
    open: AtomicUsize,
    max: usize,
}

/// A held slot; dropping it frees the slot.
pub struct Permit(Arc<Limit>);

impl Limit {
    pub fn new(max: usize) -> Arc<Self> {
        Arc::new(Self {
            open: AtomicUsize::new(0),
            max,
        })
    }

    pub fn try_acquire(self: &Arc<Self>) -> Option<Permit> {
        let mut cur = self.open.load(Ordering::Relaxed);
        loop {
            if cur >= self.max {
                return None;
            }
            match self.open.compare_exchange_weak(
                cur,
                cur + 1,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(Permit(Arc::clone(self))),
                Err(now) => cur = now,
            }
        }
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::AcqRel);
    }
}

/// A warning for the console when the page is reachable from other machines
/// (it shows where drones and the operator are, and takes a position).
pub fn exposure_warning(addr: SocketAddr) -> Option<String> {
    (!addr.ip().is_loopback()).then(|| {
        format!(
            "rid-view: WARNING: --http is bound to {addr}, not loopback: anyone on that network can read the drone table and set your position. Use 127.0.0.1 unless you mean it."
        )
    })
}

/// Accept connections on a background thread, forever.
pub fn spawn(listener: TcpListener, shared: Arc<Shared>) {
    let bound = listener
        .local_addr()
        .unwrap_or_else(|_| SocketAddr::from(([127, 0, 0, 1], 0)));
    let limit = Limit::new(MAX_CONNS);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let Some(permit) = limit.try_acquire() else {
                // Over the cap: answer from here, briefly, and drop it.
                let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
                let _ = respond(&mut stream, "503 Service Unavailable", &[], b"", false);
                continue;
            };
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || {
                let _permit = permit;
                let _ = handle(stream, &shared, bound);
            });
        }
    });
}

#[derive(Debug, Default)]
pub(crate) struct Request {
    method: String,
    path: String,
    if_none_match: Option<String>,
    content_type: Option<String>,
    host: Option<String>,
    origin: Option<String>,
    body: Vec<u8>,
}

/// The request line and headers (no body). Also returns Content-Length.
pub(crate) fn parse_head(head: &str) -> (Request, usize) {
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let mut r = Request {
        method: first.next().unwrap_or("").to_string(),
        ..Request::default()
    };
    let target = first.next().unwrap_or("");
    r.path = target.split('?').next().unwrap_or("").to_string();
    let mut len = 0usize;
    for l in lines {
        let Some((k, v)) = l.split_once(':') else {
            continue;
        };
        let v = v.trim();
        let set = |name: &str, slot: &mut Option<String>| {
            if k.eq_ignore_ascii_case(name) {
                *slot = Some(v.to_string());
            }
        };
        set("if-none-match", &mut r.if_none_match);
        set("content-type", &mut r.content_type);
        set("host", &mut r.host);
        set("origin", &mut r.origin);
        if k.eq_ignore_ascii_case("content-length") {
            len = v.parse().unwrap_or(usize::MAX);
        }
    }
    (r, len)
}

/// Why a request is turned away before any route sees it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// `Host` (or `Origin`) is not this server.
    Origin,
    /// A POST that is not `application/json`.
    ContentType,
}

/// Is `authority` (`host`, `host:port`, `[v6]:port`) this server? The host
/// must be `localhost` or an IP literal, never another name: a name is how
/// DNS rebinding reaches a loopback server. A server bound to one address
/// answers to that address; one bound to a wildcard answers to any literal.
pub(crate) fn authority_is(authority: &str, bound: SocketAddr) -> bool {
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let Some((h, after)) = rest.split_once(']') else {
            return false;
        };
        (h, after.strip_prefix(':'))
    } else if authority.matches(':').count() > 1 {
        return false;
    } else {
        match authority.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (authority, None),
        }
    };
    let port = match port {
        Some(p) => match p.parse::<u16>() {
            Ok(p) => p,
            Err(_) => return false,
        },
        None => 80,
    };
    if port != bound.port() {
        return false;
    }
    if host.eq_ignore_ascii_case("localhost") {
        return bound.ip().is_loopback() || bound.ip().is_unspecified();
    }
    match host.parse::<IpAddr>() {
        Ok(ip) => bound.ip().is_unspecified() || ip == bound.ip(),
        Err(_) => false,
    }
}

/// The checks every request passes: `Host` names this server, `Origin` (when
/// sent) is this server's own page, and a POST is JSON.
pub(crate) fn check(req: &Request, bound: SocketAddr) -> Result<(), Refusal> {
    if !req.host.as_deref().is_some_and(|h| authority_is(h, bound)) {
        return Err(Refusal::Origin);
    }
    if let Some(o) = &req.origin {
        // "null" (sandboxed or file pages) and every other site fail here.
        let ours = o
            .get(..7)
            .filter(|p| p.eq_ignore_ascii_case("http://"))
            .is_some_and(|_| authority_is(o[7..].trim_end_matches('/'), bound));
        if !ours {
            return Err(Refusal::Origin);
        }
    }
    if req.method == "POST" {
        let json = req
            .content_type
            .as_deref()
            .and_then(|c| c.split(';').next())
            .is_some_and(|m| m.trim().eq_ignore_ascii_case("application/json"));
        if !json {
            return Err(Refusal::ContentType);
        }
    }
    Ok(())
}

/// One read, but never past the request's deadline.
fn read_by(s: &mut TcpStream, chunk: &mut [u8], deadline: Instant) -> io::Result<usize> {
    let left = deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "request took too long"))?;
    s.set_read_timeout(Some(left))?;
    s.read(chunk)
}

fn read_request(s: &mut TcpStream, deadline: Instant) -> io::Result<Option<Request>> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if buf.len() > MAX_HEAD {
            return Ok(None);
        }
        let n = read_by(s, &mut chunk, deadline)?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let (mut req, len) = parse_head(&head);
    if len > MAX_BODY {
        return Ok(None);
    }
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < len {
        let n = read_by(s, &mut chunk, deadline)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(len);
    req.body = body;
    Ok(Some(req))
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

fn handle(mut s: TcpStream, shared: &Shared, bound: SocketAddr) -> io::Result<()> {
    let deadline = Instant::now() + DEADLINE;
    s.set_write_timeout(Some(DEADLINE))?;
    let Some(req) = read_request(&mut s, deadline)? else {
        return respond(&mut s, "400 Bad Request", &[], b"", false);
    };
    match check(&req, bound) {
        Ok(()) => {}
        Err(Refusal::Origin) => {
            return respond(&mut s, "403 Forbidden", &[], b"wrong host or origin\n", false);
        }
        Err(Refusal::ContentType) => {
            return respond(
                &mut s,
                "415 Unsupported Media Type",
                &[],
                b"send application/json\n",
                false,
            );
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn bound(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    fn req(head: &str) -> Request {
        parse_head(&head.replace('\n', "\r\n")).0
    }

    #[test]
    fn host_must_be_this_server() {
        let b = bound("127.0.0.1:8080");
        for ok in ["127.0.0.1:8080", "localhost:8080", "LocalHost:8080"] {
            assert!(authority_is(ok, b), "{ok}");
        }
        for bad in [
            "evil.example:8080",
            "evil.example",
            "127.0.0.1:9",
            "127.0.0.1",
            "10.0.0.5:8080",
            "[::1]:8080",
            "127.0.0.1:8080:1",
            "127.0.0.1:x",
            "",
            "[::1",
        ] {
            assert!(!authority_is(bad, b), "{bad}");
        }
        assert!(authority_is("[::1]:80", bound("[::1]:80")));
        assert!(authority_is("[::1]", bound("[::1]:80")));
        // A wildcard bind is reached by its machine's address, still not by a name.
        let any = bound("0.0.0.0:8080");
        assert!(authority_is("192.168.1.20:8080", any));
        assert!(authority_is("localhost:8080", any));
        assert!(!authority_is("rebind.example:8080", any));
    }

    #[test]
    fn checks_on_a_request() {
        let b = bound("127.0.0.1:8080");
        let post = |extra: &str| {
            req(&format!(
                "POST /api/own-pos HTTP/1.1\n{extra}Content-Length: 2\n"
            ))
        };
        let ok = post("Host: 127.0.0.1:8080\nContent-Type: application/json\n");
        assert_eq!(check(&ok, b), Ok(()));
        let ok = post("host: localhost:8080\ncontent-type: Application/JSON; charset=utf-8\nOrigin: http://localhost:8080\n");
        assert_eq!(check(&ok, b), Ok(()));
        // No Host.
        assert_eq!(check(&post("Content-Type: application/json\n"), b), Err(Refusal::Origin));
        // Foreign and opaque origins.
        for o in ["http://evil.example", "http://127.0.0.1:9", "null", "https://127.0.0.1:8080", "http://127.0.0.1:8080.evil.example"] {
            let r = post(&format!("Host: 127.0.0.1:8080\nOrigin: {o}\nContent-Type: application/json\n"));
            assert_eq!(check(&r, b), Err(Refusal::Origin), "{o}");
        }
        // A form post.
        for ct in ["text/plain", "application/x-www-form-urlencoded", "multipart/form-data; boundary=x", "application/jsonx"] {
            let r = post(&format!("Host: 127.0.0.1:8080\nContent-Type: {ct}\n"));
            assert_eq!(check(&r, b), Err(Refusal::ContentType), "{ct}");
        }
        assert_eq!(check(&post("Host: 127.0.0.1:8080\n"), b), Err(Refusal::ContentType));
        // A GET needs no content type but still the right Host.
        let get = req("GET /api/state HTTP/1.1\nHost: 127.0.0.1:8080\n");
        assert_eq!(check(&get, b), Ok(()));
        let get = req("GET /api/state HTTP/1.1\nHost: evil.example:8080\n");
        assert_eq!(check(&get, b), Err(Refusal::Origin));
    }

    #[test]
    fn head_parsing() {
        let (r, len) = parse_head("POST /api/own-pos?x=1 HTTP/1.1\r\nHost: a\r\nContent-Length: 7\r\nIf-None-Match: \"e\"");
        assert_eq!((r.method.as_str(), r.path.as_str(), len), ("POST", "/api/own-pos", 7));
        assert_eq!(r.if_none_match.as_deref(), Some("\"e\""));
        let (_, len) = parse_head("POST / HTTP/1.1\r\nContent-Length: nope");
        assert_eq!(len, usize::MAX);
    }

    #[test]
    fn connections_are_capped_and_released() {
        let l = Limit::new(2);
        let a = l.try_acquire().unwrap();
        let b = l.try_acquire().unwrap();
        assert!(l.try_acquire().is_none());
        drop(a);
        let c = l.try_acquire().unwrap();
        assert!(l.try_acquire().is_none());
        drop((b, c));
        assert!(l.try_acquire().is_some());
    }

    #[test]
    fn only_loopback_is_quiet() {
        assert!(exposure_warning(bound("127.0.0.1:8080")).is_none());
        assert!(exposure_warning(bound("[::1]:8080")).is_none());
        assert!(exposure_warning(bound("0.0.0.0:8080")).unwrap().contains("not loopback"));
        assert!(exposure_warning(bound("192.168.1.2:8080")).is_some());
    }
}
