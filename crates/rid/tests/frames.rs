//! Whole frames: core-c builds beacons and NAN action frames, we extract and
//! decode them; our frame builders are compared byte for byte with core-c's
//! and fed back through both receivers.

mod common;

use common::*;
use core::ffi::c_char;
use odid_ref as c;
use rid::message::{decode, raw_messages};
use rid::{Transport, ble, encode, wifi};

const CASES: usize = 20_000;

fn c_beacon(u: &c::ODID_UAS_Data, mac: &[u8; 6], ssid: &[u8], counter: u8) -> Vec<u8> {
    let mut buf = [0u8; 512];
    let n = unsafe {
        c::odid_wifi_build_message_pack_beacon_frame(
            u,
            mac.as_ptr() as *const c_char,
            ssid.as_ptr() as *const c_char,
            ssid.len(),
            encode::BEACON_INTERVAL_TU,
            counter,
            buf.as_mut_ptr(),
            buf.len(),
        )
    };
    assert!(n > 0, "{n}");
    buf[..n as usize].to_vec()
}

fn c_nan(u: &c::ODID_UAS_Data, mac: &[u8; 6], counter: u8) -> Vec<u8> {
    let mut buf = [0u8; 512];
    let n = unsafe {
        c::odid_wifi_build_message_pack_nan_action_frame(
            u,
            mac.as_ptr() as *const c_char,
            counter,
            buf.as_mut_ptr(),
            buf.len(),
        )
    };
    assert!(n > 0, "{n}");
    buf[..n as usize].to_vec()
}

/// Every message in `payload` decodes the same with core-c and with us.
fn check_payload(payload: &[u8], expect_pack: &[u8]) {
    assert_eq!(payload, expect_pack);
    let msgs: Vec<_> = raw_messages(payload).unwrap().collect();
    assert_eq!(msgs.len(), expect_pack[2] as usize);
    for m in msgs {
        assert!(check_against_c(m));
    }
}

#[test]
fn core_c_beacons_extract_and_decode() {
    let mut r = Rng::new(100);
    for _ in 0..CASES {
        let u = gen_uas(&mut r);
        let mut mac = [0u8; 6];
        r.fill(&mut mac);
        let ssid = {
            let mut s = r.text(31);
            s.push(b'X');
            s
        };
        let counter = r.u8();
        let frame = c_beacon(&u, &mac, &ssid, counter);
        let pack = c_build_pack(&u);

        let rid = wifi::extract(&frame).expect("beacon not recognised");
        assert_eq!(rid.transport, Transport::WifiBeacon);
        assert_eq!(rid.src, mac);
        assert_eq!(rid.counter, counter);
        check_payload(rid.payload, &pack);

        // Our builder: same bytes, except core-c stamps a live timestamp and
        // has no DS parameter element.
        let mut out = [0u8; 512];
        let n = encode::beacon_frame(mac, &ssid, 6, counter, &pack, &mut out).unwrap();
        let ours = &out[..n];
        let ds_at = 24 + 12 + 2 + ssid.len() + 3;
        assert_eq!(&ours[..24], &frame[..24]);
        assert_eq!(&ours[24..32], &[0; 8]);
        assert_eq!(&ours[32..ds_at], &frame[32..ds_at]);
        assert_eq!(&ours[ds_at..ds_at + 3], &[3, 1, 6]);
        assert_eq!(&ours[ds_at + 3..], &frame[ds_at..]);
        let back = wifi::extract(ours).unwrap();
        assert_eq!(
            (back.src, back.counter, back.payload),
            (mac, counter, &pack[..])
        );

        // And core-c's pack parser accepts the element payload we built.
        let mut uas = c::uas_data();
        let used = unsafe {
            c::odid_message_process_pack(&mut uas, back.payload.as_ptr(), back.payload.len())
        };
        assert_eq!(used as usize, pack.len());
    }
}

#[test]
fn core_c_nan_frames_extract_and_decode() {
    let mut r = Rng::new(101);
    for _ in 0..CASES {
        let u = gen_uas(&mut r);
        let mut mac = [0u8; 6];
        r.fill(&mut mac);
        let counter = r.u8();
        let frame = c_nan(&u, &mac, counter);
        let pack = c_build_pack(&u);

        let rid = wifi::extract(&frame).expect("NAN not recognised");
        assert_eq!(rid.transport, Transport::WifiNan);
        assert_eq!(rid.src, mac);
        assert_eq!(rid.counter, counter);
        check_payload(rid.payload, &pack);

        let mut out = [0u8; 512];
        let n = encode::nan_action_frame(mac, counter, &pack, &mut out).unwrap();
        assert_eq!(&out[..n], &frame[..], "NAN frame differs from core-c");

        let mut uas = c::uas_data();
        let mut got_mac = [0 as c_char; 6];
        let rc = unsafe {
            c::odid_wifi_receive_message_pack_nan_action_frame(
                &mut uas,
                got_mac.as_mut_ptr(),
                out.as_ptr(),
                n,
            )
        };
        assert_eq!(rc, 0);
        assert_eq!(got_mac.map(|b| b as u8), mac);
    }
}

#[test]
fn nan_sync_beacon_matches_core_c_and_is_not_rid() {
    let mac = [0x02, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE];
    let mut c_buf = [0u8; 128];
    let n = unsafe {
        c::odid_wifi_build_nan_sync_beacon_frame(
            mac.as_ptr() as *const c_char,
            c_buf.as_mut_ptr(),
            c_buf.len(),
        )
    };
    assert!(n > 0);
    let mut ours = [0u8; 128];
    let m = encode::nan_sync_beacon(mac, &mut ours).unwrap();
    assert_eq!(m, n as usize);
    assert_eq!(&ours[..24], &c_buf[..24]);
    assert_eq!(&ours[32..m], &c_buf[32..m]);
    assert_eq!(wifi::extract(&ours[..m]), None);
}

/// Real transmitters may add optional Service Descriptor fields; they must be skipped.
#[test]
fn nan_optional_fields_are_skipped() {
    let mut r = Rng::new(102);
    let u = gen_uas(&mut r);
    let pack = c_build_pack(&u);
    let frame = c_nan(&u, &[2, 0, 0, 0, 0, 1], 7);
    // Frame: 24 header + 6 action header; SDA starts at 30; control at 30+3+8.
    let sda = 30;
    let mut f = frame[..sda].to_vec();
    // A leading unrelated attribute.
    f.extend_from_slice(&[0x00, 0x02, 0x00, 0xFE, 0xEA]);
    let mut attr = frame[sda + 3..sda + 3 + 9].to_vec(); // service id, instance, requestor, control
    attr[8] = 0x10 | 0x20 | 0x04 | 0x08;
    attr.extend_from_slice(&[0x12, 0x34]); // binding bitmap
    attr.extend_from_slice(&[2, 0xAA, 0xBB]); // matching filter
    attr.extend_from_slice(&[1, 0xCC]); // service response filter
    attr.push((1 + pack.len()) as u8);
    attr.push(7);
    attr.extend_from_slice(&pack);
    f.push(0x03);
    f.extend_from_slice(&(attr.len() as u16).to_le_bytes());
    f.extend_from_slice(&attr);
    let rid = wifi::extract(&f).expect("optional fields not skipped");
    assert_eq!((rid.counter, rid.payload), (7, &pack[..]));

    // Without the service-info-present bit there is no payload.
    let ctrl_at = f.len() - attr.len() + 8;
    f[ctrl_at] &= !0x10;
    assert_eq!(wifi::extract(&f), None);
}

#[test]
fn ht_control_and_non_rid_frames() {
    let mut r = Rng::new(103);
    let u = gen_uas(&mut r);
    let mut frame = c_beacon(&u, &[2, 0, 0, 0, 0, 9], b"x", 1);
    // Order bit: 4-byte HT Control after the header.
    frame[1] |= 0x80;
    frame.splice(24..24, [0u8; 4]);
    assert!(wifi::extract(&frame).is_some());
    // Protected frames are ignored.
    frame[1] |= 0x40;
    assert_eq!(wifi::extract(&frame), None);
    // Data frames are ignored.
    let mut nan = c_nan(&u, &[2, 0, 0, 0, 0, 9], 1);
    assert!(wifi::extract(&nan).is_some());
    nan[0] = 0x08;
    assert_eq!(wifi::extract(&nan), None);
    // NAN to the wrong address is ignored.
    let mut nan = c_nan(&u, &[2, 0, 0, 0, 0, 9], 1);
    nan[4] = 0xFF;
    assert_eq!(wifi::extract(&nan), None);
}

#[test]
fn ble_legacy_and_extended() {
    let mut r = Rng::new(104);
    for _ in 0..CASES {
        let u = gen_uas(&mut r);
        let pack = c_build_pack(&u);
        let counter = r.u8();
        let mut ad = Vec::new();
        // Flags structure first, as real adverts have.
        ad.extend_from_slice(&[0x02, 0x01, 0x06]);
        let mut buf = [0u8; 300];

        // Legacy: one message, exactly 31 bytes of AD structure.
        let single = &pack[3..28];
        let n = encode::ble_service_data(counter, single, &mut buf).unwrap();
        assert_eq!(n, 31);
        let mut legacy = ad.clone();
        legacy.extend_from_slice(&buf[..n]);
        let got = ble::extract(&legacy).unwrap();
        assert_eq!((got.counter, got.payload), (counter, single));
        let msgs: Vec<_> = raw_messages(got.payload).unwrap().collect();
        assert_eq!(msgs.len(), 1);
        assert!(check_against_c(msgs[0]));

        // Extended: whole pack, with trailing zero padding after it.
        let n = encode::ble_service_data(counter, &pack, &mut buf).unwrap();
        let mut ext = ad.clone();
        ext.extend_from_slice(&buf[..n]);
        ext.extend_from_slice(&[0, 0, 0]);
        let got = ble::extract(&ext).unwrap();
        assert_eq!((got.counter, got.payload), (counter, &pack[..]));
        check_payload(got.payload, &pack);
    }
}

#[test]
fn ble_rejects_other_service_data() {
    assert_eq!(ble::extract(&[]), None);
    assert_eq!(ble::extract(&[0x00, 0x16, 0xFA, 0xFF, 0x0D]), None);
    // Wrong UUID.
    assert_eq!(ble::extract(&[0x05, 0x16, 0xFB, 0xFF, 0x0D, 0x01]), None);
    // Wrong app code.
    assert_eq!(ble::extract(&[0x05, 0x16, 0xFA, 0xFF, 0x0E, 0x01]), None);
    // Truncated structure.
    assert_eq!(ble::extract(&[0x1E, 0x16, 0xFA, 0xFF, 0x0D, 0x01]), None);
    // Zero length ends the data even if more follows.
    assert_eq!(
        ble::extract(&[0x00, 0x05, 0x16, 0xFA, 0xFF, 0x0D, 0x01]),
        None
    );
    // Minimal valid: counter and empty payload.
    let got = ble::extract(&[0x05, 0x16, 0xFA, 0xFF, 0x0D, 0x09]).unwrap();
    assert_eq!((got.counter, got.payload.len()), (9, 0));
}

/// Encode a random message with our encoder, wrap it in every carrier we
/// build, and check core-c reads it back.
#[test]
fn our_frames_decode_with_core_c() {
    let mut r = Rng::new(105);
    for _ in 0..CASES {
        let u = gen_uas(&mut r);
        let pack = c_build_pack(&u);
        let msgs: Vec<[u8; 25]> = raw_messages(&pack)
            .unwrap()
            .map(|m| encode::message(&decode(m).unwrap()).unwrap())
            .collect();
        let mut packed = [0u8; 300];
        let n = encode::pack(&msgs, &mut packed).unwrap();
        let mut c_uas = c::uas_data();
        assert_eq!(
            unsafe { c::odid_message_process_pack(&mut c_uas, packed.as_ptr(), n) } as usize,
            n
        );
        for m in &msgs {
            assert!(check_against_c(m));
        }
        let mut out = [0u8; 512];
        let len = encode::nan_action_frame([2, 1, 2, 3, 4, 5], 3, &packed[..n], &mut out).unwrap();
        let mut mac = [0 as c_char; 6];
        assert_eq!(
            unsafe {
                c::odid_wifi_receive_message_pack_nan_action_frame(
                    &mut c_uas,
                    mac.as_mut_ptr(),
                    out.as_ptr(),
                    len,
                )
            },
            0
        );
    }
}

#[test]
fn builders_report_small_buffers() {
    let pack = [0xF2, 25, 1]
        .iter()
        .copied()
        .chain([0x12; 25])
        .collect::<Vec<_>>();
    let mut full = [0u8; 512];
    let need = encode::beacon_frame([2; 6], b"ssid", 6, 0, &pack, &mut full).unwrap();
    for len in 0..need {
        let mut buf = vec![0u8; len];
        assert_eq!(
            encode::beacon_frame([2; 6], b"ssid", 6, 0, &pack, &mut buf),
            Err(rid::Error::BufferTooSmall)
        );
    }
    let need = encode::nan_action_frame([2; 6], 0, &pack, &mut full).unwrap();
    for len in 0..need {
        let mut buf = vec![0u8; len];
        assert_eq!(
            encode::nan_action_frame([2; 6], 0, &pack, &mut buf),
            Err(rid::Error::BufferTooSmall)
        );
    }
    assert_eq!(
        encode::beacon_frame([2; 6], &[b'a'; 33], 6, 0, &pack, &mut full),
        Err(rid::Error::Invalid)
    );
    assert_eq!(encode::pack(&[], &mut full), Err(rid::Error::BadPack));
    assert_eq!(
        encode::pack(&[[0x12; 25]; 10], &mut full),
        Err(rid::Error::BadPack)
    );
    assert_eq!(
        encode::ble_service_data(0, &[0; 251], &mut full),
        Err(rid::Error::Invalid)
    );
}
