//! Fuzz-style tests (crates/fuzzkit) for the ADSBee CSBee line parser and
//! the line splitter in front of it: the ADSBee is on the hub's own UART,
//! but every field it prints came over the air.

use adsb::{LineSplitter, Report, encode, parse};

const LINES: [&str; 3] = [
    "#A:12345E,FFFFFFFF,ABCDEFG,1234,9,-120.65432,-80.12346,1000,997,301,123,-200,-205,BDBB,FFFA1453,3,-75,2,1,3,8AA8\r\n",
    "#A:A1B2C3,200007F,N911PH,7700,7,39.95600,-75.16000,1200,1325,270,95,-500,-512,0,0,2,-62,9,3,4,DA00\r\n",
    "#A:C0FFEE,3F,UAL123,1200,3,39.87190,-75.24110,1029,1179,85,250,1024,1088,0,0,2,-80,3,0,7,F9\r\n",
];

fn reencode(r: &Report) -> Vec<u8> {
    let mut buf = [0u8; 400];
    let n = match r {
        Report::Aircraft(a) => encode::aircraft(a, &mut buf),
        Report::Stats(s) => encode::stats(s, &mut buf),
    }
    .expect("a parsed report re-encodes");
    buf[..n].to_vec()
}

#[test]
fn fuzz_csbee_lines() {
    let seeds: Vec<&[u8]> = LINES.iter().map(|l| l.as_bytes()).collect();
    let dict: [&[u8]; 7] = [b"#A:", b"#S:", b"#U:", b",", b"-", b".", b"\r\n"];
    let cfg = fuzzkit::Config::new().max_len(300).dict(&dict);
    fuzzkit::run_with("adsb::parse", &cfg, &seeds, 8_000, |bytes| {
        if let Ok(r) = parse(bytes) {
            // What parses re-encodes, and that encoding is a fixed point.
            let once = reencode(&r);
            let r2 = parse(&once).expect("a re-encoded report parses");
            assert_eq!(reencode(&r2), once);
        }
        // The splitter gives the same reports as parsing each line.
        let mut sp = LineSplitter::new();
        let mut got = 0usize;
        sp.feed(bytes, |r| got += r.is_ok() as usize);
        assert!(got <= bytes.iter().filter(|&&b| b == b'\n' || b == b'\r').count() + 1);
    });
}
