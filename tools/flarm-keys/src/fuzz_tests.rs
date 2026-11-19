//! Fuzz-style tests (crates/fuzzkit) for flarm-keys' reading of a C header
//! from elsewhere: `#define` values with line continuations and comments,
//! and the hex words in them. Made-up values only.

use crate::{define, hex_words};

#[test]
fn fuzz_header_text() {
    let seeds: [&[u8]; 2] = [b"#define KEY1 0x01234567 /* made up */\n#define KEY5 { 0x11111111, \\\n 0x22222222 } // x\n", b"#define  KEY2\t0xFFFFFFFF\n"];
    let dict: [&[u8]; 6] = [b"#define ", b"KEY1", b"\\\n", b"0x", b"/*", b"//"];
    let cfg = fuzzkit::Config::new().max_len(300).dict(&dict);
    fuzzkit::run_with("flarm-keys::define", &cfg, &seeds, 8_000, |b| {
        let text = fuzzkit::text(b);
        let lines: Vec<String> = text.lines().map(str::to_string).collect();
        for name in ["KEY1", "KEY5", ""] {
            if let Some(v) = define(&lines, name) {
                if let Ok(w) = hex_words(&v) {
                    assert!(w.len() <= v.len() / 3 + 1);
                }
            }
        }
        let _ = hex_words(&text);
    });
}
