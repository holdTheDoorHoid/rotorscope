//! Fuzz-style tests (crates/fuzzkit) for the raw-capture dump collector that
//! rawcap-pull runs over a serial log: `@H` headers (flat JSON), `@D` data
//! lines (base64 or hex), `@E` end lines, in any order and damaged.

use rawcap::dump::{Collector, Enc, Kind, LINE_MAX, Meta, b64_decode, chunk, data_line, end_line, hex_decode, parse_meta, to_ci16};

fn log(enc: Enc) -> Vec<String> {
    let data: Vec<u8> = (0..300usize).map(|k| (k * 7 + k / 13) as u8).collect();
    let m = Meta { id: 7, kind: Kind::Nib8, rate: 40_000_000, bytes: data.len() as u32, freq_khz: 5_756_000, gain: 60, lsb: 6, swap_iq: false, lpf: 12, enc, t_us: 123_456_789_012, pos: 987_654, crc: rawcap::crc32::crc32(&data) };
    let mut v = vec![format!("@H {m}")];
    let mut buf = [0u8; LINE_MAX];
    for s in 0..m.lines() {
        v.push(data_line(&mut buf, m.id, s, chunk(&data, s), enc).to_string());
    }
    v.push(end_line(&mut buf, &m).to_string());
    v
}

#[test]
fn fuzz_dump_collector() {
    let (a, b) = (log(Enc::B64), log(Enc::Hex));
    let mut seeds: Vec<Vec<u8>> = a.iter().chain(&b).map(|l| l.clone().into_bytes()).collect();
    seeds.push(a.join("\n").into_bytes());
    seeds.push(b.join("\r\n").into_bytes());
    let refs: Vec<&[u8]> = seeds.iter().map(|s| s.as_slice()).collect();
    let dict: [&[u8]; 8] = [b"@H ", b"@D ", b"@E ", b"\"bytes\":", b"\"kind\":\"w32\"", b"=", b",", b"\n"];
    let cfg = fuzzkit::Config::new().max_len(4000).dict(&dict);
    fuzzkit::run_with("rawcap::dump::Collector", &cfg, &refs, 3_000, |bytes| {
        let text = fuzzkit::text(bytes);
        let mut c = Collector::default();
        for line in text.split('\n') {
            c.feed(line);
            if let Some(j) = line.strip_prefix("@H ") {
                let _ = parse_meta(j);
            }
            let _ = b64_decode(line);
            let _ = hex_decode(line);
        }
        for p in c.dumps.values() {
            let _ = p.missing();
            if let Ok(payload) = p.payload() {
                assert_eq!(payload.len(), p.meta.bytes as usize);
                let _ = to_ci16(&p.meta, &payload);
            }
        }
    });
}
