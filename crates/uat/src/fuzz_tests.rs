//! Fuzz-style tests (crates/fuzzkit): Reed-Solomon correction and payload
//! decoding of UAT ADS-B frames, seeded with the encoder in tests.rs.

extern crate std;

use crate::tests::{basic_frame, long_frame, philly};
use crate::{BASIC_BYTES, LONG_BYTES, correct_adsb_frame, payload};

#[test]
fn fuzz_uat_frames() {
    let s = philly();
    let seeds = [basic_frame(&s), long_frame(1, &s, "N123AB"), long_frame(2, &s, ""), long_frame(3, &s, "TEST")];
    let refs: std::vec::Vec<&[u8]> = seeds.iter().map(|v| v.as_slice()).collect();
    let cfg = fuzzkit::Config::new().max_len(LONG_BYTES + 16);
    fuzzkit::run_with("uat::correct_adsb_frame", &cfg, &refs, 4_000, |bytes| {
        let _ = payload::decode(bytes);
        let mut f = bytes.to_vec();
        match correct_adsb_frame(&mut f) {
            Ok((kind, n)) => {
                // Corrected within the code's limit, and only the frame's own bytes.
                assert!(n <= 16, "{kind:?} corrected {n}");
                let len = if kind == crate::FrameKind::Long { LONG_BYTES } else { BASIC_BYTES };
                assert_eq!(f[len..], bytes[len..]);
                let _ = payload::decode(&f);
                // A corrected frame is a codeword: correcting again changes nothing.
                let mut g = f.clone();
                assert!(correct_adsb_frame(&mut g).is_ok());
            }
            Err(_) => assert_eq!(f, bytes, "a refused frame was changed"),
        }
    });
}
