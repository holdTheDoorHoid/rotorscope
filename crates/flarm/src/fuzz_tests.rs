//! Fuzz-style tests (crates/fuzzkit): the OGN tracker packet (LDPC check,
//! dewhitening, fields) and, with feature `legacy`, the FLARM legacy decoder
//! under the fake test keys of tests_legacy.rs. The key-file parser too.

extern crate std;

use crate::{Keys, ogn};

#[test]
fn fuzz_ogn_packets() {
    let seed = crate::tests_ogn::encode(&crate::tests_ogn::glider());
    let cfg = fuzzkit::Config::new().max_len(64);
    fuzzkit::run_with("flarm::ogn", &cfg, &[&seed], 5_000, |bytes| {
        if ogn::open(bytes).is_ok() {
            let _ = ogn::decode(bytes);
        }
        let _ = ogn::decode(bytes);
    });
}

#[cfg(feature = "legacy")]
#[test]
fn fuzz_legacy_packets() {
    use crate::tests_legacy::{encode_v6, encode_v7, fake, rx_near, tug, with_crc};
    let keys = fake();
    let t = tug();
    let (a, b) = (with_crc(&encode_v6(&t, &keys)), with_crc(&encode_v7(&t, &keys)));
    let rx = rx_near(&t);
    let cfg = fuzzkit::Config::new().max_len(64);
    fuzzkit::run_with("flarm::legacy::decode", &cfg, &[&a, &b], 5_000, |bytes| {
        let _ = crate::legacy::decode(bytes, &keys, &rx);
        let _ = crate::legacy::crc16(bytes);
    });
}

#[test]
fn fuzz_key_file() {
    // Made-up values in the key file's shape (never the real constants).
    let text = b"# keys\nkey1 = 0x01234567\nkey2 = 0x89abcdef\nkey3 = 0x76543210\nkey4 = 0x55aa55aa\nkey5 = 11111111, 22222222, 33333333, 44444444\n";
    let dict: [&[u8]; 6] = [b"key", b" = ", b"0x", b",", b"\n", b"#"];
    let cfg = fuzzkit::Config::new().max_len(400).dict(&dict);
    fuzzkit::run_with("flarm::Keys::parse", &cfg, &[text], 5_000, |bytes| {
        if let Ok(k) = Keys::parse(&fuzzkit::text(bytes)) {
            // Debug never shows the values.
            let d = std::format!("{k:?}");
            assert!(!d.contains("1234567") && !d.contains("89abcdef"), "{d}");
        }
    });
}
