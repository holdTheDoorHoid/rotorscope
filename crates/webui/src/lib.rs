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
#![no_std]

/// The page, gzip-compressed.
pub static INDEX_HTML_GZ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/index.html.gz"));

pub const CONTENT_TYPE: &str = "text/html; charset=utf-8";
pub const CONTENT_ENCODING: &str = "gzip";
/// Revalidate each load (cheap with the ETag), so a firmware update shows at once.
pub const CACHE_CONTROL: &str = "no-cache";

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
