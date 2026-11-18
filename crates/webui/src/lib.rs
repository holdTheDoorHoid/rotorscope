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
}
