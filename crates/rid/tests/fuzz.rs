//! Fuzz-style tests (crates/fuzzkit) for the Remote ID parsers. These sit
//! beside `robustness.rs` (long random runs) and add seed mutation, the DJI
//! legacy DroneID element and the GB 46750 packet finder and decoder.

mod common;

use odid_ref as c;
use rid::message::{decode, message_kind, raw_messages};
use rid::{ble, cn, dji, encode, wifi};

const ITERS: usize = 15_000;

fn within(outer: &[u8], inner: &[u8]) -> bool {
    let (o, i) = (outer.as_ptr_range(), inner.as_ptr_range());
    o.start <= i.start && i.end <= o.end
}

fn sample_pack() -> Vec<u8> {
    let mut r = common::Rng::new(7);
    let uas = common::gen_uas(&mut r);
    let mut out = [0u8; 255];
    let msgs: Vec<_> = [common::ours_basic_id(&uas.BasicID[0]), common::ours_location(&uas.Location), common::ours_system(&uas.System)]
        .iter()
        .map(|m| encode::message(m).unwrap())
        .collect();
    let used = encode::pack(&msgs, &mut out).unwrap();
    out[..used].to_vec()
}

fn frame_seeds() -> Vec<Vec<u8>> {
    let pack = sample_pack();
    let mut seeds = Vec::new();
    let mut out = [0u8; 400];
    let n = encode::beacon_frame([2, 0, 0, 0, 0, 1], b"rid-test", 6, 9, &pack, &mut out).unwrap();
    seeds.push(out[..n].to_vec());
    let n = encode::nan_action_frame([2, 0, 0, 0, 0, 1], 9, &pack, &mut out).unwrap();
    seeds.push(out[..n].to_vec());
    let n = encode::nan_sync_beacon([2, 0, 0, 0, 0, 1], &mut out).unwrap();
    seeds.push(out[..n].to_vec());
    let n = encode::ble_service_data(3, &pack[2..2 + 25], &mut out).unwrap();
    seeds.push(out[..n].to_vec());
    seeds.push(pack);
    seeds
}

#[test]
fn fuzz_wifi_ble_and_messages() {
    let seeds = frame_seeds();
    let refs: Vec<&[u8]> = seeds.iter().map(|s| s.as_slice()).collect();
    fuzzkit::run("rid::wifi+ble+message", &refs, ITERS, |bytes| {
        let mut payloads = Vec::new();
        if let Some(w) = wifi::extract(bytes) {
            assert!(within(bytes, w.payload));
            payloads.push(w.payload);
        }
        if let Some(b) = ble::extract(bytes) {
            assert!(within(bytes, b.payload));
            payloads.push(b.payload);
        }
        for (_, body) in wifi::elements(bytes) {
            assert!(within(bytes, body));
        }
        for (_, data) in ble::ad_structures(bytes) {
            assert!(within(bytes, data));
        }
        payloads.push(bytes);
        let _ = message_kind(bytes);
        for p in payloads {
            if let Ok(msgs) = raw_messages(p) {
                for m in msgs {
                    if let Ok(msg) = decode(m) {
                        if let Ok(again) = encode::message(&msg) {
                            assert_eq!(decode(&again).unwrap().kind(), msg.kind());
                        }
                    }
                }
            }
        }
    });
}

fn dji_seeds() -> Vec<Vec<u8>> {
    let mut seeds = Vec::new();
    // A flight-information element with the extension, built by hand from
    // the layout dji::parse_element reads (header, 29 + 55 + uuid bytes).
    let mut info = dji::OUI.to_vec();
    info.extend_from_slice(&[0x58, 0x62, 0x13, dji::FLIGHT_INFO]);
    info.push(2); // version
    info.extend_from_slice(&7u16.to_le_bytes());
    info.extend_from_slice(&0x1F0Fu16.to_le_bytes());
    info.extend_from_slice(b"1ZNBH3M00C00TEST");
    info.extend_from_slice(&(-13_119_000i32).to_le_bytes()); // lon
    info.extend_from_slice(&6_972_000i32.to_le_bytes()); // lat
    for v in [120i16, 50, -3, 4, 1, 250, -120, 9000] {
        info.extend_from_slice(&v.to_le_bytes());
    }
    info.extend_from_slice(&(-13_118_000i32).to_le_bytes());
    info.extend_from_slice(&6_971_000i32.to_le_bytes());
    info.push(16);
    info.push(4);
    info.extend_from_slice(b"uuid");
    seeds.push(info.clone());
    let mut out = [0u8; 400];
    let n = encode::dji_flight_purpose(b"1ZNBH3M00C00TEST", b"DRONE1", b"survey", &mut out).unwrap();
    seeds.push(out[..n].to_vec());
    // The same elements inside a beacon.
    for el in seeds.clone() {
        let n = encode::beacon_frame([0x60, 0x60, 0x1F, 0, 0, 1], b"dji", 6, 0, &[], &mut out).unwrap();
        let mut f = out[..n].to_vec();
        f.push(221);
        f.push(el.len() as u8);
        f.extend_from_slice(&el);
        seeds.push(f);
    }
    seeds
}

#[test]
fn fuzz_dji_legacy() {
    let seeds = dji_seeds();
    let refs: Vec<&[u8]> = seeds.iter().map(|s| s.as_slice()).collect();
    fuzzkit::run("rid::dji", &refs, ITERS, |bytes| {
        let _ = dji::extract(bytes);
        if let Some(rec) = dji::parse_element(bytes) {
            if let dji::Record::FlightInfo(info) = rec {
                // Re-encoding what was decoded must work and decode the same
                // way, except that an absent position is sent as 0,0.
                let mut out = [0u8; 400];
                if let Ok(n) = encode::dji_flight_info(&info, &mut out) {
                    match dji::parse_element(&out[..n]) {
                        Some(dji::Record::FlightInfo(back)) => {
                            assert_eq!(back.seq, info.seq);
                            assert_eq!(back.serial, info.serial);
                            assert_eq!(back.position.is_some(), info.position.is_some());
                        }
                        other => panic!("re-encoded flight info parsed as {other:?}"),
                    }
                }
            }
        }
    });
}

fn cn_seed() -> Vec<u8> {
    // Fixed field values, as tests/cn.rs's generator would pick them.
    let mut d = c::DroneRIDData_t::default();
    common::set_cstr(&mut d.sn, b"SN-TEST-0001");
    common::set_cstr(&mut d.uin, b"UIN00001");
    d.has_op_category = true;
    d.op_category = 2;
    d.drone_class = 1;
    d.gcs_pos_type = 1;
    (d.gcs_lat, d.gcs_lon, d.gcs_alt) = (39.95, -75.16, 20.0);
    (d.drone_lat, d.drone_lon, d.drone_alt) = (39.951, -75.161, 60.0);
    d.track_angle = 90.0;
    d.ground_speed = 5.0;
    d.has_rel_alt = true;
    d.rel_alt = 40.0;
    d.has_v_speed = true;
    d.vertical_speed = 1.0;
    d.has_pressure_alt = true;
    d.pressure_alt = 61.0;
    d.op_status = 2;
    d.timestamp_ms = 1_700_000_000_000;
    let mut buf = [0u8; 160];
    let mut len = 0usize;
    let rc = unsafe { c::CN46750_RID_Encode(&d, buf.as_mut_ptr(), buf.len(), &mut len) };
    if rc == c::RID_OK { buf[..len].to_vec() } else { vec![cn::PACKET_TYPE, 0x20, 4, 0x80, 0, 0, 0, 0] }
}

#[test]
fn fuzz_cn_gb46750() {
    let seed = cn_seed();
    fuzzkit::run("rid::cn", &[&seed], ITERS, |bytes| {
        if let Some(p) = cn::find(bytes) {
            assert!(within(bytes, p));
            let _ = cn::decode(p);
        }
        let _ = cn::decode(bytes);
    });
}

/// The seeds reach the deep paths: otherwise the mutations above test less.
#[test]
fn seeds_are_accepted() {
    let f = frame_seeds();
    assert!(wifi::extract(&f[0]).is_some() && wifi::extract(&f[1]).is_some());
    assert!(ble::extract(&f[3]).is_some());
    let d = dji_seeds();
    assert!(matches!(dji::parse_element(&d[0]), Some(dji::Record::FlightInfo(_))));
    assert!(matches!(dji::parse_element(&d[1]), Some(dji::Record::FlightPurpose(_))));
    assert!(dji::extract(&d[2]).is_some());
    assert!(cn::decode(&cn_seed()).is_ok(), "{:02x?}", cn_seed());
}
