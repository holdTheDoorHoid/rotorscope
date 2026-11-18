//! Gzip `web/index.html` into OUT_DIR, plus an ETag derived from the bytes,
//! so the hub serves a pre-compressed page straight from flash; and the
//! page's Content-Security-Policy, with the SHA-256 of every inline
//! `<script>` and `<style>` block computed here from the same bytes, so the
//! policy can never go stale when the page changes.

use std::io::Write;
use std::path::PathBuf;

use sha2::{Digest, Sha256};

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let page = manifest.join("../../web/index.html");
    println!("cargo:rerun-if-changed={}", page.display());
    let html = std::fs::read(&page).unwrap_or_else(|e| panic!("{}: {e}", page.display()));

    // Header mtime is zero (flate2's default), so the output depends only on
    // the page: reproducible builds and a stable ETag.
    let mut gz = flate2::GzBuilder::new().write(Vec::new(), flate2::Compression::best());
    gz.write_all(&html).unwrap();
    let gz = gz.finish().unwrap();

    // FNV-1a 64 of the compressed bytes: changes whenever the page does.
    let hash = gz.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
    });

    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    std::fs::write(out.join("index.html.gz"), &gz).unwrap();
    std::fs::write(
        out.join("meta.rs"),
        format!(
            "/// Strong ETag of the page (quoted, as sent in the header).\n\
             pub const ETAG: &str = \"\\\"{hash:016x}\\\"\";\n\
             /// Size of the page before compression, in bytes.\n\
             pub const INDEX_HTML_LEN: usize = {};\n\
             /// Content-Security-Policy for the page (see lib.rs).\n\
             pub const CSP: &str = \"{}\";\n",
            html.len(),
            csp(&html)
        ),
    )
    .unwrap();
}

/// The policy: nothing but this page's own inline script and style (by
/// hash), `data:` images, and fetches to the hub itself.
fn csp(html: &[u8]) -> String {
    let scripts = hashes(html, b"script");
    let styles = hashes(html, b"style");
    assert!(!scripts.is_empty() && !styles.is_empty(), "web/index.html: expected an inline <script> and <style>");
    format!(
        "default-src 'none'; script-src {}; style-src {}; img-src data:; connect-src 'self'; \
         frame-ancestors 'none'; base-uri 'none'; form-action 'none'",
        scripts.join(" "),
        styles.join(" ")
    )
}

/// `'sha256-...'` of the text inside every `<tag ...>...</tag>` block, as
/// the browser hashes it (the exact bytes between the tags).
fn hashes(html: &[u8], tag: &[u8]) -> Vec<String> {
    let open = [b"<", tag].concat();
    let close = [b"</", tag, b">"].concat();
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(i) = find(&html[at..], &open) {
        let start = at + i;
        let after = html.get(start + open.len()).copied();
        // `<script>` or `<script ...>`, not `<scripts`.
        if !matches!(after, Some(b'>' | b' ' | b'\t' | b'\n' | b'\r')) {
            at = start + open.len();
            continue;
        }
        let body = start + find(&html[start..], b">").expect("unterminated tag") + 1;
        let end = body + find(&html[body..], &close).expect("unclosed inline block");
        let digest = Sha256::digest(&html[body..end]);
        out.push(format!("'sha256-{}'", base64(&digest)));
        at = end + close.len();
    }
    out
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in bytes.chunks(3) {
        let n = (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        s.push(A[(n >> 18) as usize & 63] as char);
        s.push(A[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 { A[(n >> 6) as usize & 63] as char } else { '=' });
        s.push(if c.len() > 2 { A[n as usize & 63] as char } else { '=' });
    }
    s
}
