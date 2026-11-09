//! Gzip `web/index.html` into OUT_DIR, plus an ETag derived from the bytes,
//! so the hub serves a pre-compressed page straight from flash.

use std::io::Write;
use std::path::PathBuf;

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
             pub const INDEX_HTML_LEN: usize = {};\n",
            html.len()
        ),
    )
    .unwrap();
}
