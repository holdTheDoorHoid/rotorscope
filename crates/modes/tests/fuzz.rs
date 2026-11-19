//! Fuzz-style tests (crates/fuzzkit) for the Mode S path: Beast binary and
//! ADSBee RAW line input, frame error correction and decoding, and the
//! aircraft table that ingests the result.

use modes::input::{beast_encode, parse_raw_line};
use modes::{BeastReader, Config, Frame, Received, Table, decode};

const VECTORS: [&str; 7] = [
    "8D4840D6202CC371C32CE0576098",
    "8D40621D58C382D690C8AC2863A7",
    "8D40621D58C386435CC412692AD6",
    "8D485020994409940838175B284F",
    "8DA05F219B06B6AF189400CBC33F",
    "8D406B902015A678D4D220AA4BDA",
    "5D4840D6A1B2C3",
];

fn frames() -> Vec<Frame> {
    VECTORS.iter().filter_map(|h| Frame::from_hex(h)).collect()
}

#[test]
fn fuzz_frame_decode_and_table() {
    let seeds: Vec<Vec<u8>> = frames().iter().map(|f| f.as_bytes().to_vec()).collect();
    assert!(seeds.len() >= 6);
    let refs: Vec<&[u8]> = seeds.iter().map(|s| s.as_slice()).collect();
    let cfg = fuzzkit::Config::new().max_len(20);
    fuzzkit::run_with("modes::decode", &cfg, &refs, 8_000, |bytes| {
        let Some(f) = Frame::new(bytes) else { return };
        assert_eq!(f.as_bytes(), bytes);
        let _ = modes::crc::syndrome(f.as_bytes());
        for fix in [0, 1, 2] {
            let mut g = f;
            let _ = decode(&mut g, fix);
        }
        let mut t: Table<8> = Table::new(Config::default());
        for (i, s) in seeds.iter().chain(std::iter::once(&bytes.to_vec())).enumerate() {
            if let Some(fr) = Frame::new(s) {
                let _ = t.ingest(&Received { frame: fr, signal_dbm: Some(-60), ticks_12mhz: Some(i as u64 * 1_000) }, 1_000 + i as u64);
            }
        }
        assert!(t.len() <= 8);
    });
}

#[test]
fn fuzz_beast_stream() {
    let mut stream = Vec::new();
    for (i, f) in frames().iter().enumerate() {
        let mut out = [0u8; 44];
        let n = beast_encode(f, 0x1A1A_1A1A + i as u64, 0x1A, &mut out);
        stream.extend_from_slice(&out[..n]);
    }
    let cfg = fuzzkit::Config::new().max_len(600);
    fuzzkit::run_with("modes::BeastReader", &cfg, &[&stream], 6_000, |bytes| {
        let mut r = BeastReader::new();
        let mut got = Vec::new();
        r.feed(bytes, |rx| got.push(rx));
        // Re-encoding what was read and reading it again gives it back.
        for rx in got {
            let mut out = [0u8; 44];
            let n = beast_encode(&rx.frame, rx.ticks_12mhz.unwrap_or(0), 0x40, &mut out);
            let mut r2 = BeastReader::new();
            let mut back = Vec::new();
            r2.feed(&out[..n], |x| back.push(x.frame));
            assert_eq!(back, vec![rx.frame]);
        }
    });
}

#[test]
fn fuzz_raw_lines() {
    let seeds: [&[u8]; 2] = [b"#MDS*8D4840D6202CC371C32CE0576098;(0,-62,12,1A2B3C4D5E6F)\r\n", b"#MDS*5D4840D6A1B2C3;(1,-80,3,0)\n"];
    let dict: [&[u8]; 5] = [b"#MDS*", b";(", b")", b",", b"-"];
    let cfg = fuzzkit::Config::new().max_len(120).dict(&dict);
    fuzzkit::run_with("modes::parse_raw_line", &cfg, &seeds, 8_000, |bytes| {
        if let Some(rx) = parse_raw_line(bytes) {
            assert!(matches!(rx.frame.as_bytes().len(), 7 | 14));
        }
        let _ = Frame::from_hex(&fuzzkit::text(bytes));
    });
}
