//! Fuzz-style tests (crates/fuzzkit) for the open digital FPV link
//! classifier (wfb-ng, OpenHD, RubyFPV, DroneBridge), seeded with the
//! crate's own fake frames.

use openlink::build::{MAX_LEN, fake_frame};
use openlink::{System, classify, classify_mpdu};

#[test]
fn fuzz_classify() {
    let mut seeds = Vec::new();
    for s in System::ALL {
        let mut out = [0u8; MAX_LEN];
        let n = fake_frame(s, 42, &mut out).unwrap();
        seeds.push(out[..n].to_vec());
    }
    let refs: Vec<&[u8]> = seeds.iter().map(|s| s.as_slice()).collect();
    for s in &seeds {
        assert!(classify_mpdu(s).is_some() || classify(s).is_some(), "seed not recognised");
    }
    fuzzkit::run_with("openlink::classify", &fuzzkit::Config::new().max_len(MAX_LEN + 64), &refs, 20_000, |bytes| {
        let _ = classify(bytes);
        if let Some(o) = classify_mpdu(bytes) {
            assert!(System::ALL.contains(&o.system));
        }
    });
}
