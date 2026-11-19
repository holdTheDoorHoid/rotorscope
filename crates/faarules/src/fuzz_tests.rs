//! Fuzz-style tests (crates/fuzzkit): the rule engine over a damaged
//! airspace pack (from the hub's SD card), with the CRC fixed up so the
//! damage reaches the lookups, and the text written for each flag.

extern crate std;

use std::vec::Vec;

use airspace::{Pack, crc32};
use tracker::Tracker;

use crate::tests::{NIGHT_MS, heard, pack_bytes};
use crate::{Engine, text};

fn fix_up(b: &mut [u8]) {
    if b.len() < 40 {
        return;
    }
    let header_len = usize::from(u16::from_le_bytes([b[6], b[7]]));
    let total = b.len() as u32;
    b[32..36].copy_from_slice(&total.to_le_bytes());
    if (40..=b.len()).contains(&header_len) {
        let c = crc32(&b[header_len..]);
        b[36..40].copy_from_slice(&c.to_le_bytes());
    }
}

#[test]
fn fuzz_rules_over_damaged_pack() {
    let seed = pack_bytes();
    let cfg = fuzzkit::Config::new().max_len(seed.len() + 64).random_percent(2);
    // The same drones every time: one high, one low, near the test areas.
    let mut tr: Tracker<8, 4> = Tracker::default();
    for (i, agl) in [(1u32, 160.0f32), (2, 20.0)] {
        heard(&mut tr, i, "1581F5FHD00000000001", agl, 1_000);
    }
    let mut parsed = 0;
    fuzzkit::run_with("faarules::Engine::update", &cfg, &[&seed], 400, |bytes| {
        let mut b: Vec<u8> = bytes.to_vec();
        fix_up(&mut b);
        let Ok(pack) = Pack::parse(&b) else { return };
        parsed += 1;
        let mut eng: Engine<8> = Engine::default();
        for k in 0..3u64 {
            eng.update(&tr, Some(&pack), 1_000 + k * 2_000, Some(NIGHT_MS + k * 2_000));
        }
        for e in eng.entries() {
            let mut s = heapless::String::<2048>::new();
            let _ = text::write_json(&mut s, e, Some(&pack));
            if let Some(fl) = e.worst() {
                let mut s = heapless::String::<400>::new();
                let _ = text::write_flag(&mut s, fl, Some(&pack));
                let _ = (text::screen_text(fl), text::screen_words(fl), text::item_value(fl));
            }
        }
    });
    assert!(parsed > 0);
}
