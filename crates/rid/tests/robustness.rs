//! Nothing may panic or hang on hostile input: random byte strings and
//! mutations of valid frames go through every public entry point.
//!
//! Iteration counts are set by `RID_FUZZ_ITERS` (default 2,000,000 per test).

mod common;

use common::*;
use rid::message::{Message, decode, raw_messages};
use rid::{ble, encode, wifi};

fn iters() -> usize {
    std::env::var("RID_FUZZ_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2_000_000)
}

/// Run the whole receive pipeline on `bytes` as a Wi-Fi frame, as BLE
/// advertising data and as a bare payload. Returns how many messages decoded.
fn pipeline(bytes: &[u8]) -> usize {
    let mut decoded = 0;
    let mut payload = |p: &[u8]| {
        if let Ok(msgs) = raw_messages(p) {
            for m in msgs {
                if let Ok(msg) = decode(m) {
                    decoded += 1;
                    // Whatever decodes must re-encode, or be rejected cleanly.
                    if let Ok(again) = encode::message(&msg) {
                        let back = decode(&again).expect("re-encoded message must decode");
                        assert_eq!(back.kind(), msg.kind());
                    }
                }
            }
        }
    };
    if let Some(rid) = wifi::extract(bytes) {
        payload(rid.payload);
    }
    if let Some(rid) = ble::extract(bytes) {
        payload(rid.payload);
    }
    payload(bytes);
    let _ = wifi::elements(bytes).count();
    let _ = ble::ad_structures(bytes).count();
    decoded
}

#[test]
fn random_bytes() {
    let mut r = Rng::new(200);
    for _ in 0..iters() {
        let len = match r.below(4) {
            0 => r.below(8),
            1 => r.below(64),
            _ => r.below(400),
        };
        let mut buf = r.bytes(len);
        // Steer a share of inputs past the first checks of each parser.
        match r.below(6) {
            0 if len >= 2 => {
                buf[0] = 0x80;
                buf[1] = 0x00;
            }
            1 if len >= 10 => {
                buf[0] = 0xD0;
                buf[1] = 0x00;
                buf[4..10].copy_from_slice(&wifi::NAN_DEST);
            }
            2 if len >= 1 => buf[0] = 0xF0 | (buf[0] & 0x0F),
            _ => {}
        }
        pipeline(&buf);
    }
}

#[test]
fn mutated_valid_frames() {
    let mut r = Rng::new(201);
    let frames = sample_frames(&mut r);
    // Sanity: unmutated, every sample yields messages.
    for f in &frames {
        assert!(pipeline(f) > 0, "{f:02x?}");
    }
    for _ in 0..iters() {
        let mut f = frames[r.below(frames.len())].clone();
        for _ in 0..1 + r.below(6) {
            if f.is_empty() {
                break;
            }
            let at = r.below(f.len());
            match r.below(6) {
                0 => f[at] = r.u8(),
                1 => f[at] ^= 1 << r.below(8),
                2 => {
                    f.remove(at);
                }
                3 => f.insert(at, r.u8()),
                4 => f.truncate(at),
                _ => f[at] = [0x00, 0xFF, 0x7F, 0x80, 0x19, 0x09][r.below(6)],
            }
        }
        pipeline(&f);
    }
}

#[test]
fn every_raw_message_decodes_or_errors() {
    let mut r = Rng::new(202);
    let mut raw = [0u8; 25];
    for _ in 0..iters() {
        r.fill(&mut raw);
        let _ = decode(&raw);
    }
}

#[test]
fn encoders_never_panic() {
    let mut r = Rng::new(203);
    let mut raw = [0u8; 25];
    let mut out = vec![0u8; 600];
    for _ in 0..iters() / 4 {
        r.fill(&mut raw);
        raw[0] = (raw[0] & 0x0F) | ((r.below(6) as u8) << 4);
        let Ok(mut msg) = decode(&raw) else { continue };
        // Push fields outside their legal ranges.
        match &mut msg {
            Message::Location(l) => {
                l.direction = Some(r.f32_in(-1000.0, 1000.0));
                l.speed_horizontal = Some(r.f32_in(-10.0, 400.0));
                l.speed_vertical = if r.chance(2) {
                    Some(f32::NAN)
                } else {
                    Some(r.f32_in(-100.0, 100.0))
                };
                l.timestamp = Some(r.f32_in(-1.0, 70000.0));
                l.timestamp_accuracy = Some(r.f32_in(-1.0, 3.0));
                l.altitude_geodetic = Some(r.f32_in(-5000.0, 50000.0));
                l.status = rid::message::Status::from_code(r.u8());
            }
            Message::System(s) => {
                s.area_radius = r.u16();
                s.operator_location_type = rid::message::OperatorLocationType::from_code(r.u8());
            }
            Message::Auth(a) => {
                a.page = r.u8();
                a.auth_type = rid::message::AuthType::from_code(r.u8());
            }
            Message::BasicId(b) => b.id_type = rid::message::IdType::from_code(r.u8()),
            _ => {}
        }
        if let Ok(enc) = encode::message(&msg) {
            decode(&enc).expect("encoder output must decode");
        }
        let n = r.below(12);
        let msgs: Vec<[u8; 25]> = (0..n).map(|_| raw).collect();
        let cap = r.below(out.len());
        let _ = encode::pack(&msgs, &mut out[..cap]);
        let pack_len = r.below(300);
        let pack = r.bytes(pack_len);
        let cap = r.below(out.len());
        let _ = encode::beacon_ie(r.u8(), &pack, &mut out[..cap]);
        let ssid_len = r.below(40);
        let ssid = r.bytes(ssid_len);
        let _ = encode::beacon_frame([1; 6], &ssid, r.u8(), r.u8(), &pack, &mut out[..cap]);
        let _ = encode::nan_action_frame([1; 6], r.u8(), &pack, &mut out[..cap]);
        let _ = encode::ble_service_data(r.u8(), &pack, &mut out[..cap]);
        let _ = encode::nan_sync_beacon([1; 6], &mut out[..cap]);
    }
}
