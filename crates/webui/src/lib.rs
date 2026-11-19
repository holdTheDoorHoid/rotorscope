//! The phone web page, ready to serve.
//!
//! `web/index.html` is a single self-contained file (inline CSS and JS, no
//! external requests: the hub's hotspot has no internet). The build script
//! gzips it, so the hub stores and sends the compressed bytes as they are:
//!
//! ```text
//! HTTP/1.1 200 OK
//! Content-Type: text/html; charset=utf-8      <- CONTENT_TYPE
//! Content-Encoding: gzip                      <- CONTENT_ENCODING
//! ETag: "…"                                   <- ETAG (answer 304 on If-None-Match)
//! Cache-Control: no-cache                     <- CACHE_CONTROL
//!
//! <INDEX_HTML_GZ>
//! ```
//!
//! Every phone browser accepts gzip, so there is no uncompressed copy.
//! The page talks to the API documented in `crates/tracker/README.md`.
//!
//! The page is sent with [`CSP`] (built from the page itself by build.rs:
//! only its own inline `<script>` and `<style>` run, by SHA-256), plus
//! [`NOSNIFF`] and [`REFERRER_POLICY`]. A hash-based policy also blocks
//! inline event handlers (`onclick="..."`) and `style="..."` attributes,
//! including ones a script writes with `innerHTML`; the tests refuse a page
//! that has any.
//!
//! Also here, because the hub firmware cannot be tested on the host:
//! [`request`] (which requests the hub answers), [`dhcp`] (the hotspot's
//! address pool) and [`hotspot`] (the password and its stored salt).
#![no_std]

pub mod dhcp;
pub mod hotspot;
pub mod request;

/// The page, gzip-compressed.
pub static INDEX_HTML_GZ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/index.html.gz"));

pub const CONTENT_TYPE: &str = "text/html; charset=utf-8";
pub const CONTENT_ENCODING: &str = "gzip";
/// Revalidate each load (cheap with the ETag), so a firmware update shows at once.
pub const CACHE_CONTROL: &str = "no-cache";
/// `X-Content-Type-Options` for every response: no MIME sniffing.
pub const NOSNIFF: &str = "nosniff";
/// `Referrer-Policy` for every response.
pub const REFERRER_POLICY: &str = "no-referrer";

include!(concat!(env!("OUT_DIR"), "/meta.rs"));

/// True when an `If-None-Match` header value matches this page, so the
/// server can answer `304 Not Modified` with no body.
pub fn etag_matches(if_none_match: &str) -> bool {
    if_none_match.trim() == "*"
        || if_none_match
            .split(',')
            .map(|t| t.trim().trim_start_matches("W/"))
            .any(|t| t == ETAG)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::io::Read;

    #[test]
    fn page_is_gzip_of_the_source() {
        assert_eq!(&INDEX_HTML_GZ[..2], &[0x1f, 0x8b], "gzip magic");
        // The trailer holds the uncompressed length (ISIZE, little endian).
        let n = INDEX_HTML_GZ.len();
        let isize = u32::from_le_bytes(INDEX_HTML_GZ[n - 4..].try_into().unwrap());
        assert_eq!(isize as usize, INDEX_HTML_LEN);
        let src =
            std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/index.html")).unwrap();
        assert_eq!(src.len(), INDEX_HTML_LEN);
        std::println!("index.html: {} bytes, {} gzipped", INDEX_HTML_LEN, n);
    }

    #[test]
    fn page_budget() {
        // Lives in the hub's flash; keep it small (target < 40 KB gzipped).
        assert!(INDEX_HTML_GZ.len() < 40 * 1024, "{}", INDEX_HTML_GZ.len());
    }

    #[test]
    fn page_is_self_contained() {
        let src =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/index.html"))
                .unwrap();
        // No network fetches beyond our own API: the hotspot has no internet.
        for bad in [
            "http://",
            "https://",
            "//cdn",
            "<link ",
            "@import",
            "src=\"http",
        ] {
            let hits: std::vec::Vec<_> = src
                .lines()
                .filter(|l| l.contains(bad) && !l.contains("www.w3.org/2000/svg"))
                .filter(|l| !l.contains("maps.apple.com"))
                .collect();
            assert!(hits.is_empty(), "{bad}: {hits:?}");
        }
        assert!(src.contains("/api/state") && src.contains("/api/own-pos"));
    }

    #[test]
    fn etag() {
        assert!(ETAG.starts_with('"') && ETAG.ends_with('"') && ETAG.len() == 18);
        assert!(etag_matches(ETAG));
        assert!(etag_matches(&std::format!("\"x\", W/{ETAG}")));
        assert!(etag_matches("*"));
        assert!(!etag_matches("\"0000000000000000\""));
    }

    fn page() -> std::string::String {
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/index.html")).unwrap()
    }

    /// The text of every `<tag>...</tag>` block (no attributes on the tag).
    fn blocks<'a>(src: &'a str, tag: &str) -> std::vec::Vec<&'a str> {
        let (open, close) = (std::format!("<{tag}>"), std::format!("</{tag}>"));
        let mut out = std::vec::Vec::new();
        let mut rest = src;
        while let Some(i) = rest.find(&open) {
            let body = &rest[i + open.len()..];
            let end = body.find(&close).unwrap();
            out.push(&body[..end]);
            rest = &body[end..];
        }
        out
    }

    fn sha256_b64(text: &str) -> std::string::String {
        use sha2::{Digest, Sha256};
        let d = Sha256::digest(text.as_bytes());
        const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = std::string::String::new();
        for c in d.chunks(3) {
            let n = (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
            for k in 0..4 {
                s.push(if k <= c.len() { A[(n >> (18 - 6 * k)) as usize & 63] as char } else { '=' });
            }
        }
        s
    }

    #[test]
    fn csp_allows_exactly_the_pages_own_blocks() {
        let src = page();
        // Every script and style block, with or without attributes on the tag.
        assert_eq!(src.matches("<script").count(), 1, "one inline <script> (no src=, no second block)");
        assert_eq!(src.matches("<style").count(), 1, "one inline <style>");
        let (script, style) = (blocks(&src, "script"), blocks(&src, "style"));
        assert_eq!((script.len(), style.len()), (1, 1), "plain <script> / <style> tags");
        let want_script = std::format!("script-src 'sha256-{}';", sha256_b64(script[0]));
        let want_style = std::format!("style-src 'sha256-{}';", sha256_b64(style[0]));
        assert!(CSP.contains(&want_script), "{CSP}");
        assert!(CSP.contains(&want_style), "{CSP}");
        for part in ["default-src 'none'", "img-src data:", "connect-src 'self'", "frame-ancestors 'none'", "base-uri 'none'", "form-action 'none'"] {
            assert!(CSP.contains(part), "{part}");
        }
        assert!(!CSP.contains("unsafe"), "{CSP}");
    }

    /// What a hash-based policy blocks silently: inline event handler
    /// attributes, `style="..."` attributes (also when a script builds them
    /// into `innerHTML`), `javascript:` URLs. Script that sets `el.onclick`
    /// or `el.style.x` through the DOM is fine and not matched here.
    #[test]
    fn page_has_nothing_the_policy_would_block() {
        let src = page();
        let lower = src.to_ascii_lowercase();
        let mut bad = std::vec::Vec::new();
        for (n, line) in lower.lines().enumerate() {
            let b = line.as_bytes();
            for (i, _) in line.match_indices("on") {
                // ` onclick="` / ` onload='` : an attribute, not `x.onclick =`.
                let before = if i == 0 { b' ' } else { b[i - 1] };
                let name_end = i + 2 + b[i + 2..].iter().take_while(|c| c.is_ascii_lowercase()).count();
                if name_end > i + 2
                    && matches!(before, b' ' | b'\t' | b'"' | b'\'')
                    && b.get(name_end) == Some(&b'=')
                    && matches!(b.get(name_end + 1), Some(b'"' | b'\'' | b'`') | Some(b'a'..=b'z'))
                {
                    bad.push(std::format!("line {}: event handler attribute: {}", n + 1, line.trim()));
                }
            }
            if line.contains("style=") {
                bad.push(std::format!("line {}: style attribute: {}", n + 1, line.trim()));
            }
            if line.contains("javascript:") {
                bad.push(std::format!("line {}: javascript: URL: {}", n + 1, line.trim()));
            }
            if line.contains("setattribute('style'") || line.contains("setattribute(\"style\"") {
                bad.push(std::format!("line {}: style attribute via setAttribute: {}", n + 1, line.trim()));
            }
        }
        assert!(bad.is_empty(), "web/index.html has inline code or styles the CSP blocks:\n{}", bad.join("\n"));
        // No resource loads the policy would refuse.
        for s in ["new worker(", "eval(", "new function(", "<iframe", "<object", "<embed", "<form"] {
            assert!(!lower.contains(s), "{s}");
        }
    }

    #[test]
    fn decompresses_to_the_source() {
        let mut out = std::vec::Vec::new();
        flate2::read::GzDecoder::new(INDEX_HTML_GZ)
            .read_to_end(&mut out)
            .unwrap();
        let src =
            std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/index.html")).unwrap();
        assert!(out == src, "gzip does not round-trip to web/index.html");
    }

    /// The `/api/state` reference (crates/tracker/README.md) as the page
    /// reads it, for the fields WP-09 added: every track's `confirmed`
    /// (missing on older hubs, so only an explicit `false` counts as
    /// unconfirmed) and the head's optional `flood` object. Unconfirmed
    /// tracks never alert, and the flood banner puts only numbers into the
    /// page (nothing from the air reaches it unescaped).
    #[test]
    fn page_follows_the_state_reference_for_standing_and_floods() {
        let src = page();
        assert!(src.contains("const isUnc = t => t.confirmed === false;"));
        // checkNew skips unconfirmed tracks before it remembers or alerts.
        let check = &src[src.find("function checkNew()").unwrap()..];
        let check = &check[..check.find("\n}\n").unwrap()];
        assert!(check.contains("|| isUnc(t)) continue;"), "{check}");
        // The list ranks standing before distance.
        let sorted = &src[src.find("function sorted()").unwrap()..];
        let sorted = &sorted[..sorted.find("\n}\n").unwrap()];
        let (unc, dist) = (sorted.find("isUnc(a)").unwrap(), sorted.find("dist(a)").unwrap());
        assert!(unc < dist, "{sorted}");
        // The flood banner: reads st.flood, interpolates numbers only.
        assert!(src.contains("const fl = st && st.flood;"));
        // `why` (the rule's reading) is itself built from numbers only.
        let why = &src[src.find("const why = fl.rule").unwrap()..];
        let why = &why[..why.find(";\n").unwrap()];
        let ban = &src[src.find("setHTML('flood'").unwrap()..];
        let ban = &ban[..ban.find(");").unwrap()];
        let interpolated = |text: &str| {
            let mut rest = text;
            let mut out = std::vec::Vec::new();
            while let Some(i) = rest.find("${") {
                let e = &rest[i + 2..];
                let end = e.find('}').unwrap();
                out.push(std::string::String::from(&e[..end]));
                rest = &e[end..];
            }
            out
        };
        let w = interpolated(why);
        assert_eq!(w, ["Number(fl.minute) | 0", "Number(fl.active) | 0", "Number(fl.new_per_s) | 0"], "{why}");
        let b = interpolated(ban);
        for expr in &b {
            assert!(["why", "unc"].contains(&expr.as_str()), "flood banner interpolates {expr:?}: numbers only, or esc()");
        }
        assert_eq!(b.len(), 2);
        // The unconfirmed marking is a class and plain text (no style
        // attribute, which the CSP would block).
        assert!(src.contains(".trk.unc{"));
        assert!(src.contains("${u ? ' unc' : ''}"));
    }

    /// The body of `function name(...) { ... }` in the page.
    fn function<'a>(src: &'a str, name: &str) -> &'a str {
        let f = &src[src.find(&std::format!("function {name}(")).unwrap_or_else(|| panic!("no function {name}"))..];
        &f[..f.find("\n}\n").unwrap()]
    }

    /// Protocol 14 (docs/LINK-KEYS-DESIGN.md 6.4): when the hub's GPS checks
    /// stop trusting the receiver, the hub holds its last trusted position
    /// and runs its clock on (`/api/state` `own.held`, `utc_held`). The page
    /// must never show a held position or time as live: no GPS tick, no
    /// "GPS time", and it says "held" wherever it would have said them.
    #[test]
    fn held_fix_not_shown_as_live() {
        let src = page();
        assert!(src.contains("const isHeld = o => !!(o && o.source === 'gps' && o.held);"));
        // The status strip: a held fix gets its own chip, checked before the
        // live one, and the live GPS tick is only for a fix that is not held.
        let strip = function(&src, "renderStrip");
        let (held, live) = (strip.find("if (isHeld(st.own))").unwrap(), strip.find("GPS &#10003;").unwrap());
        assert!(held < live, "{strip}");
        assert!(strip.contains("GPS HELD"), "{strip}");
        // Settings: the position in use says it is held.
        let settings = function(&src, "renderSettings");
        assert!(settings.contains("isHeld(own) ? 'hub GPS, held (last trusted position, not live)'"), "{settings}");
        // The hub clock: held time is not called GPS time.
        let hub = function(&src, "renderHub");
        let (h, g) = (hub.find("st.utc_held ?").unwrap(), hub.find("'GPS time'").unwrap());
        assert!(h < g, "{hub}");
        assert!(hub.contains("GPS time held (running on the hub clock)"), "{hub}");
    }

    /// The `/api/state` reference for WP-10's per-track `claims` object
    /// (crates/tracker/README.md): missing on older hubs, so only an
    /// explicit `claims.disputed` counts; disputed rows are tagged (a
    /// class, no style attribute); the list says once that nothing is
    /// verified; and the claim section puts only esc()'d strings and
    /// Number()s from the claim into the page (its source and what is
    /// disputed are strings from the hub).
    #[test]
    fn page_follows_the_state_reference_for_claims() {
        let src = page();
        assert!(src.contains("const isDisp = t => !!(t.claims && t.claims.disputed);"));
        assert!(src.contains("(isDisp(t) ? '<span class=\"tag disp\">DISPUTED</span>' : '')"));
        assert!(src.contains(".tag.disp{"));
        assert!(src.contains("IDs and positions are what drones broadcast; none is verified."));
        assert!(src.contains("o.push(...claimSection(t));"));
        assert!(src.contains("decrypted, not authenticated"));
        assert!(src.contains("message present, not checked"));
        let f = &src[src.find("function claimSection(t)").unwrap()..];
        let f = &f[..f.find("\n}\n").unwrap()];
        // Every interpolation: esc(), a Number(), a formatter of one, or
        // the notes list (itself built from Number()s and fixed text).
        let mut rest = f;
        let mut n = 0;
        while let Some(i) = rest.find("${") {
            let e = &rest[i + 2..];
            let end = e.find('}').unwrap();
            let expr = &e[..end];
            assert!(
                expr.starts_with("esc(")
                    || expr.starts_with("Number(")
                    || expr.starts_with("copyBtn(coord(Number(")
                    || expr == "notes.length ? '; ' + notes.join('; ') : ''",
                "claim section interpolates {expr:?}: esc() or numbers only"
            );
            rest = &e[end..];
            n += 1;
        }
        assert!(n >= 4, "{n}");
        // The hub's strings appear only inside esc().
        for (field, wrapped) in [("r.src", "esc(SRC[r.src] || r.src)"), ("c.what", "esc((c.what || [])")] {
            assert_eq!(f.matches(field).count(), f.matches(wrapped).count() * if field == "r.src" { 2 } else { 1 }, "{field}");
        }
        // Coordinates and distances go through Number().
        assert!(f.contains("coordHTML(Number(r.lat), Number(r.lon))"));
        assert!(f.contains("fmtDist(Number(r.apart_m))"));
    }
}
