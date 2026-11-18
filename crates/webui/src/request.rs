//! Which HTTP requests the hub answers: the Host, Origin and Content-Type
//! rules, as one pure decision the firmware calls on every route.
//!
//! The threats, both from an ordinary web page open in a browser on a phone
//! that is joined to the hotspot (or on any machine on the LAN in station
//! mode):
//!
//! - **Cross-site writes.** A page on any site can send a "simple" POST
//!   (`text/plain`, no preflight) to `http://192.168.4.1/api/cot`. picoserve's
//!   body extractor ignores Content-Type, so before these rules that switched
//!   the TAK feed off or moved the operator's position. Writes now need
//!   `Content-Type: application/json` (which a cross-site page can only send
//!   after a CORS preflight the hub never answers) and an `Origin` that is
//!   absent (curl, tools) or the hub's own.
//! - **DNS rebinding.** A page on `evil.example` re-points its own name at
//!   192.168.4.1; the browser then treats the hub as same-origin with the
//!   attacker and lets the script read `/api/state` (tracks and the
//!   operator's own position). The request still says `Host: evil.example`,
//!   so every route refuses a Host that is not the hub's own address.
//!
//! The captive-portal redirect is the one route that answers any Host (an
//! OS probe arrives as `Host: connectivitycheck.gstatic.com` and must be
//! sent to the page); it returns no data and redirects to the hub's own
//! address, so it gives a rebinding attacker nothing.

use core::net::Ipv4Addr;

/// What a route does with the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Returns data (the page, `/api/state`, ...).
    Read,
    /// Changes the hub's state (`POST /api/own-pos`, `POST /api/cot`).
    Write,
}

/// The decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    /// `Host` missing, repeated, or not this hub's address on this network.
    WrongHost,
    /// A write on a network where writes are off (station mode by default).
    WritesOff,
    /// A write without `Content-Type: application/json`.
    NotJson,
    /// A write whose `Origin` is present and is not this hub.
    CrossOrigin,
}

impl Verdict {
    /// Short reason for the error body and logs.
    pub const fn as_str(self) -> &'static str {
        match self {
            Verdict::Allow => "ok",
            Verdict::WrongHost => "wrong host",
            Verdict::WritesOff => "changes are off on this network",
            Verdict::NotJson => "content-type must be application/json",
            Verdict::CrossOrigin => "cross-origin request",
        }
    }
}

/// The request headers the decision reads, as the client sent them.
#[derive(Debug, Clone, Copy, Default)]
pub struct Headers<'a> {
    /// The first `Host` header, if any.
    pub host: Option<&'a str>,
    /// How many `Host` headers there were (more than one is refused).
    pub host_count: usize,
    pub origin: Option<&'a str>,
    pub content_type: Option<&'a str>,
}

/// Which rule a request falls under, by method and path: `None` for the
/// captive-portal redirect (every path but `/` and `/api/...`), which
/// answers any Host; `Read` for GET and HEAD; `Write` for anything else
/// (POST, and methods no route takes, which then fail on the safe side).
pub fn route_access(method: &str, path: &str) -> Option<Access> {
    if path != "/" && !path.starts_with("/api/") {
        return None;
    }
    Some(if method == "GET" || method == "HEAD" { Access::Read } else { Access::Write })
}

/// Decide a request. `own` is the hub's address on the network the request
/// came in on (`None` while it has none, e.g. a station before DHCP);
/// `writes` whether that network may change anything.
pub fn check(own: Option<Ipv4Addr>, access: Access, writes: bool, h: &Headers<'_>) -> Verdict {
    let Some(own) = own else { return Verdict::WrongHost };
    let host_ok = h.host_count == 1 && h.host.is_some_and(|v| host_is(own, v));
    if !host_ok {
        return Verdict::WrongHost;
    }
    if access == Access::Read {
        return Verdict::Allow;
    }
    if !writes {
        return Verdict::WritesOff;
    }
    if !h.content_type.is_some_and(is_json) {
        return Verdict::NotJson;
    }
    match h.origin {
        None => Verdict::Allow,
        Some(o) if origin_is(own, o) => Verdict::Allow,
        Some(_) => Verdict::CrossOrigin,
    }
}

/// `Host` names `own`: `a.b.c.d` or `a.b.c.d:80` (the server listens on 80
/// only). No names: the hub has no DNS name a browser could rely on, and a
/// name is exactly what a rebinding attack brings.
pub fn host_is(own: Ipv4Addr, host: &str) -> bool {
    let host = host.trim_matches(|c: char| c == ' ' || c == '\t');
    let addr = host.strip_suffix(":80").unwrap_or(host);
    addr.parse::<Ipv4Addr>().is_ok_and(|a| a == own)
}

/// `Origin` is this hub's page: `http://a.b.c.d` (optionally `:80`). The
/// hub has no TLS, so an `https://` origin is never its own; `null` (a
/// sandboxed frame, a `data:` page) is refused.
pub fn origin_is(own: Ipv4Addr, origin: &str) -> bool {
    let origin = origin.trim_matches(|c: char| c == ' ' || c == '\t');
    origin.strip_prefix("http://").is_some_and(|rest| !rest.contains('/') && host_is(own, rest))
}

/// `Content-Type` is JSON: `application/json`, any case, parameters such
/// as `; charset=utf-8` allowed.
pub fn is_json(content_type: &str) -> bool {
    let media = content_type.split(';').next().unwrap_or("");
    media.trim_matches(|c: char| c == ' ' || c == '\t').eq_ignore_ascii_case("application/json")
}

#[cfg(test)]
mod tests {
    use super::*;

    const AP: Ipv4Addr = Ipv4Addr::new(192, 168, 4, 1);

    fn h<'a>(host: Option<&'a str>, origin: Option<&'a str>, ct: Option<&'a str>) -> Headers<'a> {
        Headers { host, host_count: usize::from(host.is_some()), origin, content_type: ct }
    }

    const JSON: Option<&str> = Some("application/json");

    #[test]
    fn the_page_itself_is_allowed() {
        // What a phone's browser sends for the page's own fetch() calls.
        let get = h(Some("192.168.4.1"), None, None);
        assert_eq!(check(Some(AP), Access::Read, true, &get), Verdict::Allow);
        let post = h(Some("192.168.4.1"), Some("http://192.168.4.1"), JSON);
        assert_eq!(check(Some(AP), Access::Write, true, &post), Verdict::Allow);
        // curl / tools send no Origin.
        let curl = h(Some("192.168.4.1:80"), None, Some("Application/JSON; charset=utf-8"));
        assert_eq!(check(Some(AP), Access::Write, true, &curl), Verdict::Allow);
    }

    #[test]
    fn dns_rebinding_is_refused_on_every_route() {
        for host in ["evil.example", "evil.example:80", "192.168.4.1.evil.example", "192.168.4.10", "192.168.4.1:8080", ""] {
            for access in [Access::Read, Access::Write] {
                let req = h(Some(host), None, JSON);
                assert_eq!(check(Some(AP), access, true, &req), Verdict::WrongHost, "{host}");
            }
        }
        // No Host at all (HTTP/1.0), and two of them.
        assert_eq!(check(Some(AP), Access::Read, true, &h(None, None, None)), Verdict::WrongHost);
        let two = Headers { host: Some("192.168.4.1"), host_count: 2, origin: None, content_type: None };
        assert_eq!(check(Some(AP), Access::Read, true, &two), Verdict::WrongHost);
        // No address yet (station before DHCP).
        assert_eq!(check(None, Access::Read, true, &h(Some("192.168.4.1"), None, None)), Verdict::WrongHost);
    }

    #[test]
    fn cross_site_simple_post_is_refused() {
        // A form or fetch(..., {mode:'no-cors'}) from another site: text/plain.
        let simple = h(Some("192.168.4.1"), Some("http://evil.example"), Some("text/plain;charset=UTF-8"));
        assert_eq!(check(Some(AP), Access::Write, true, &simple), Verdict::NotJson);
        // Form encodings, and no Content-Type at all.
        for ct in ["application/x-www-form-urlencoded", "multipart/form-data; boundary=x", "application/jsonp", "text/json"] {
            let req = h(Some("192.168.4.1"), None, Some(ct));
            assert_eq!(check(Some(AP), Access::Write, true, &req), Verdict::NotJson, "{ct}");
        }
        assert_eq!(check(Some(AP), Access::Write, true, &h(Some("192.168.4.1"), None, None)), Verdict::NotJson);
        // JSON but from another origin (only possible after a preflight the
        // hub never answers, but refuse it anyway).
        for o in ["http://evil.example", "null", "https://192.168.4.1", "http://192.168.4.1.evil.example", "http://192.168.4.1/x"] {
            let req = h(Some("192.168.4.1"), Some(o), JSON);
            assert_eq!(check(Some(AP), Access::Write, true, &req), Verdict::CrossOrigin, "{o}");
        }
    }

    #[test]
    fn routes() {
        assert_eq!(route_access("GET", "/"), Some(Access::Read));
        assert_eq!(route_access("GET", "/api/state"), Some(Access::Read));
        assert_eq!(route_access("HEAD", "/api/flags"), Some(Access::Read));
        assert_eq!(route_access("POST", "/api/cot"), Some(Access::Write));
        assert_eq!(route_access("POST", "/api/own-pos"), Some(Access::Write));
        assert_eq!(route_access("PUT", "/api/cot"), Some(Access::Write));
        // Captive-portal probes: redirected whatever the Host.
        for p in ["/generate_204", "/hotspot-detect.html", "/connecttest.txt", "/canonical.html", "/api", "/apix"] {
            assert_eq!(route_access("GET", p), None, "{p}");
        }
    }

    #[test]
    fn writes_off_on_the_station_side() {
        let sta = Ipv4Addr::new(10, 0, 0, 23);
        let get = h(Some("10.0.0.23"), None, None);
        assert_eq!(check(Some(sta), Access::Read, false, &get), Verdict::Allow);
        let post = h(Some("10.0.0.23"), Some("http://10.0.0.23"), JSON);
        assert_eq!(check(Some(sta), Access::Write, false, &post), Verdict::WritesOff);
        assert_eq!(check(Some(sta), Access::Write, true, &post), Verdict::Allow);
        // The hotspot address is not the station's.
        let wrong = h(Some("192.168.4.1"), None, None);
        assert_eq!(check(Some(sta), Access::Read, false, &wrong), Verdict::WrongHost);
    }
}
