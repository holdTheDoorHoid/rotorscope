//! Fuzz-style tests (crates/fuzzkit) for evidence-verify's own readers: the
//! `--btc-header` argument and headers file lines, and trusted-key lines.
//! The logs and bundles themselves go through `crates/evidence`, which has
//! its own fuzz tests.

use crate::{parse_header_arg, parse_key};

#[test]
fn fuzz_header_and_key_text() {
    let hdr = format!("840000:{}", "00".repeat(80));
    let key = format!("04{}", "ab".repeat(64));
    let seeds: [&[u8]; 2] = [hdr.as_bytes(), key.as_bytes()];
    fuzzkit::run("evidence-verify::args", &seeds, 6_000, |b| {
        let t = fuzzkit::text(b);
        if let Some((_, h)) = parse_header_arg(&t) {
            let _ = h.check_work();
        }
        let _ = parse_key(&t);
    });
}
