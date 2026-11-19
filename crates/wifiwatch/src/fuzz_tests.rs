//! Fuzz-style tests (crates/fuzzkit) for every reader of over-the-air bytes
//! here: 802.11 headers and elements, the BSS summary, the drone-link and
//! flood watchers, MAVLink over Wi-Fi (parse, then the MAVLink scanner),
//! and Bluetooth advert classification.

use std::vec::Vec;

use crate::mac::{self, build};
use crate::mavwifi::{self, MavWifi, Meta};
use crate::mavwifi::tests::{Opts, camera_info, frame, heartbeat, mav};
use crate::{Watch, ble};

const ITERS: usize = 8_000;

fn within(outer: &[u8], inner: &[u8]) -> bool {
    let (o, i) = (outer.as_ptr_range(), inner.as_ptr_range());
    inner.is_empty() || (o.start <= i.start && i.end <= o.end)
}

fn wifi_seeds() -> Vec<Vec<u8>> {
    let ap = [0x60, 0x60, 0x1F, 1, 2, 3];
    let sta = [0x02, 0, 0, 0, 0, 9];
    let mut v = std::vec![
        build::wpa2(ap, b"DJI-MAVIC-3-A1B2"),
        build::open(ap, b"TELLO-ABCDEF"),
        build::beacon_from(5, ap, b"Anafi-123", true, &build::ie(37, &[0, 11, 3])),
        build::kill(0xC, [0xFF; 6], ap, ap, 7),
        build::kill(0xA, sta, ap, ap, 3),
        build::data(sta, ap, true),
        build::with_seq(build::data(sta, ap, false), 77, true),
        build::mgmt(0xD, sta, ap, ap, &[0, 4, 0, 37, 3, 0, 11, 3]),
    ];
    let o = Opts::down();
    v.push(frame(&o, &heartbeat(1, 1, 1)));
    v.push(frame(&Opts::up([0x02, 0, 0, 0, 0, 7], [192, 168, 0, 2]), &heartbeat(2, 255, 190)));
    let cam = camera_info(b"../../../../../tmp", b"QGC01", b"http://192.168.2.64:8000/c.xml");
    v.push(frame(&o, &mav(9, 1, 100, mavwatch::msg::CAMERA_INFORMATION, &cam)));
    v
}

#[test]
fn fuzz_80211_parsers() {
    let seeds = wifi_seeds();
    let refs: Vec<&[u8]> = seeds.iter().map(|s| s.as_slice()).collect();
    fuzzkit::run_with("wifiwatch::mac", &fuzzkit::Config::new().max_len(700), &refs, ITERS * 2, |bytes| {
        let f = mac::rx_frame(bytes, 600);
        assert!(within(bytes, f));
        if let Some(h) = mac::header(f) {
            if let Some(b) = mac::bss_info(&h, f) {
                let _ = b;
            }
            let _ = mac::action_csa(&h, f);
        }
        for (_, body) in mac::elements(f) {
            assert!(within(f, body));
        }
        let _ = mac::data_link(f);
        match mavwifi::parse(f) {
            Ok(d) => {
                assert!(within(f, d.payload));
                let _ = mavwifi::plausible(&d);
            }
            Err(_) => {}
        }
        if let Ok(d) = mavwifi::candidate(f) {
            assert!(within(f, d.payload));
        }
    });
}

#[test]
fn fuzz_watchers() {
    let seeds = wifi_seeds();
    let refs: Vec<&[u8]> = seeds.iter().map(|s| s.as_slice()).collect();
    fuzzkit::run_with("wifiwatch::Watch", &fuzzkit::Config::new().max_len(700), &refs, ITERS, |bytes| {
        let mut w = Watch::new();
        let mut n = 0;
        // The same frame heard a few times, as a flood or a beacon train.
        for i in 0..4u64 {
            w.frame(bytes, -50, 6, 1_000_000 + i * 100_000, &mut |_| n += 1);
            w.data(bytes, -50, 6, 1_000_000 + i * 100_000);
        }
        w.poll(10_000_000, &mut |_| n += 1);
        assert!(n < 64);
    });
}

#[test]
fn fuzz_mavlink_over_wifi() {
    let seeds = wifi_seeds();
    let refs: Vec<&[u8]> = seeds[8..].iter().map(|s| s.as_slice()).collect();
    let base = mavwifi::parse(&seeds[8]).expect("the heartbeat seed is a datagram").flow;
    fuzzkit::run_with("wifiwatch::MavWifi::feed", &fuzzkit::Config::new().max_len(700), &refs, ITERS, |bytes| {
        let mut m = MavWifi::default();
        let Ok(d) = mavwifi::parse(bytes) else {
            // Not a datagram: still feed the bytes as a payload.
            let mut m2 = MavWifi::default();
            let meta = Meta { flow: base, channel: 6, rssi: -40, up_us: 1, truncated: false };
            m2.feed(&meta, bytes, &mut |_, _| {});
            return;
        };
        let meta = Meta { flow: d.flow, channel: 6, rssi: -40, up_us: 1_000, truncated: d.truncated };
        for k in 0..2 {
            let meta = Meta { up_us: meta.up_us + k * 1_000, ..meta };
            m.feed(&meta, d.payload, &mut |t, ev| {
                let _ = (t, ev);
            });
        }
        let _ = m.pressure();
    });
}

#[test]
fn fuzz_ble_adverts() {
    let apple = {
        let mut v = std::vec![2, 0x01, 0x06, 0x1E, 0xFF, 0x4C, 0x00, 0x07, 0x19];
        v.extend_from_slice(&[0x01; 25]);
        v
    };
    let seeds: [&[u8]; 3] = [&apple, &[5, 0xFF, 0x75, 0x00, 0x01, 0x00], &[6, 0xFF, 0x06, 0x00, 0x03, 0x00, 0x80]];
    fuzzkit::run_with("wifiwatch::ble", &fuzzkit::Config::new().max_len(255), &seeds, ITERS * 2, |bytes| {
        let _ = ble::classify(bytes);
        let mut w = ble::BleWatch::default();
        let mut n = 0;
        for i in 0..3u64 {
            let _ = w.advert(&[0xC0, 1, 2, 3, 4, i as u8], -60, bytes, i * 10_000);
        }
        w.poll(5_000_000, &mut |_| n += 1);
        assert!(n < 16);
    });
}

/// The camera seed reaches the MAVLink scanner and raises a finding.
#[test]
fn camera_seed_is_found() {
    let seeds = wifi_seeds();
    let d = mavwifi::parse(&seeds[10]).unwrap();
    let mut m = MavWifi::default();
    let mut n = 0;
    m.feed(&Meta { flow: d.flow, channel: 6, rssi: -40, up_us: 1_000, truncated: d.truncated }, d.payload, &mut |_, _| n += 1);
    assert!(n > 0);
}
