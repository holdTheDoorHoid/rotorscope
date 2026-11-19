//! Fuzz-style tests (crates/fuzzkit) for every control-link packet reader:
//! ExpressLRS, mLRS, SiK and OpenLRSng recognition on the receive configs
//! the scan plans use, the serial (MAVLink) extraction, the contested-link
//! watcher, CRSF frames and MAVLink content decoding. Seeds come from the
//! crate's test-only transmitter models.

extern crate std;

use std::vec::Vec;

use crate::elrs::{OtaVersion, testenc::Tx};
use crate::{Config, Region, RxConfig, RxMeta, System, crsf, recognise, serial_bytes};

const ITERS: usize = 1_500;

fn configs() -> Vec<RxConfig> {
    let mut v = Vec::new();
    for sys in [System::ExpressLrs, System::Mlrs, System::Sik, System::OpenLrsNg] {
        for region in [Region::Fcc915, Region::Eu868] {
            // A handful of steps each: the probes differ by rate and length.
            v.extend(crate::scan::plan(sys, region).step_by(7).take(4).map(|s| s.config));
        }
    }
    assert!(v.len() >= 8);
    v
}

fn seeds() -> Vec<Vec<u8>> {
    let mut s = Vec::new();
    for version in [OtaVersion::V4, OtaVersion::V3] {
        let tx = Tx { uid: [1, 2, 3, 4, 5, 6], version, model_id: None };
        s.push(tx.sync(false, 3, 7, 2, 1, 0));
        s.push(tx.sync(true, 3, 7, 2, 1, 0));
    }
    let mut hb = [0u8; 300];
    let n = mavwatch::frame::encode::v1(&mut hb, 1, 1, 1, 0, &[0, 0, 0, 0, 2, 3, 0x81, 4, 3]);
    s.push(crate::sik::encode(crate::sik::DEFAULT_NETID, &hb[..n], 0x0123));
    s.push(crate::mlrs::testenc::tx_frame(0x1234, 1, [1000; 8], &hb[..n]));
    s.push(crate::mlrs::testenc::rx_frame(0x1234, 2, &hb[..n]));
    s.push(crsf::encode(0xC8, 0x16, &[0x55; 22]));
    s
}

#[test]
fn fuzz_recognise_every_probe() {
    let cfgs = configs();
    let seeds = seeds();
    let refs: Vec<&[u8]> = seeds.iter().map(|s| s.as_slice()).collect();
    let cfg = Config { read_ids: true, decode_content: true };
    fuzzkit::run_with("ctrlwatch::recognise", &fuzzkit::Config::new().max_len(260), &refs, ITERS, |bytes| {
        let mut watch: crate::contested::Watch<4> = Default::default();
        for (i, rx) in cfgs.iter().enumerate() {
            let meta = RxMeta { rssi_dbm: -70, snr_db: 5, t_us: 1_000 + i as u64 };
            if let Some(r) = recognise(bytes, rx, meta, &cfg) {
                let _ = std::format!("{r:?}");
            }
            let mut out = [0u8; 258];
            if let Some((s, _)) = serial_bytes(bytes, rx, &mut out) {
                assert!(s.len() <= 258);
                let _ = mavwatch::frames(s).count();
            }
            watch.observe(bytes, rx, meta, &cfg);
        }
    });
}

#[test]
fn fuzz_crsf_and_mavlink_content() {
    let mut hb = [0u8; 300];
    let n = mavwatch::frame::encode::v2(&mut hb, 1, 1, 1, 33, &[0u8; 28], None);
    let seeds: [&[u8]; 2] = [&crsf::encode(0xC8, 0x02, &[0x11; 15]), &hb[..n]];
    fuzzkit::run("ctrlwatch::crsf+mavlink", &seeds, ITERS * 4, |bytes| {
        if let Some(n) = crsf::frame_len(bytes) {
            assert!(n <= 64);
        }
        let _ = crsf::decode(bytes);
        for f in crate::mavlink::frames(bytes) {
            let _ = crate::mavlink::decode(&f);
        }
        let _ = crate::mavlink::decode_all(bytes, |_| {});
        // Any channel layout over any bytes (`c.set` must bound the index).
        let config = bytes.first().map_or(0, |&b| usize::from(b));
        let _ = crate::openlrs::unpack(config, bytes);
    });
}

/// Each family's seed is recognised on at least one of the configs.
#[test]
fn seeds_are_recognised() {
    let cfgs = configs();
    let cfg = Config { read_ids: true, decode_content: true };
    let meta = RxMeta { rssi_dbm: -70, snr_db: 5, t_us: 1 };
    let hits: Vec<usize> = seeds().iter().enumerate().filter(|(_, s)| cfgs.iter().any(|rx| recognise(s, rx, meta, &cfg).is_some())).map(|(i, _)| i).collect();
    // ELRS (V4 and V3, short and full sync), SiK and mLRS.
    assert!(hits.len() >= 5, "{hits:?}");
}
