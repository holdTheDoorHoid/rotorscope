//! Fuzz-style tests (crates/fuzzkit) for what the LR2021 node accepts from
//! its receive FIFO before sending it up (ES1090, UAT, FLARM and OGN chips),
//! and the Manchester codec.

use link::AirProto;
use lr2021cfg::{accept, manchester};

#[test]
fn fuzz_accept() {
    // Seeds: a Manchester-coded DF17 frame (ES1090 is sent inverted) and
    // plain bytes the other paths read.
    let df17: Vec<u8> = (0..14u8).map(|i| 0x8D ^ i.wrapping_mul(37)).collect();
    let mut chips = vec![0u8; 28];
    manchester::encode(&df17, &mut chips);
    let inv: Vec<u8> = chips.iter().map(|b| !b).collect();
    let seeds: [&[u8]; 3] = [&chips, &inv, &[0x55; 52]];
    fuzzkit::run_with("lr2021cfg::accept", &fuzzkit::Config::new().max_len(120), &seeds, 20_000, |bytes| {
        for p in [AirProto::Es1090, AirProto::Uat978, AirProto::Flarm, AirProto::Ogn] {
            if let Ok(raw) = accept::accept(p, bytes) {
                assert!(raw.len() <= link::MAX_AIR_RAW);
            }
        }
        let _ = accept::crc_ccitt(0xFFFF, bytes);
        let _ = lr2021cfg::cmd::cmd_status(bytes);
        // Manchester: decoding an encoding gives the data back, cleanly.
        let mut c = vec![0u8; bytes.len() * 2];
        manchester::encode(bytes, &mut c);
        let mut back = vec![0u8; bytes.len()];
        assert_eq!(manchester::decode(&c, &mut back, false), 0);
        assert_eq!(back, bytes);
        let mut out = vec![0u8; bytes.len() / 2];
        let _ = manchester::decode(bytes, &mut out, true);
    });
}
