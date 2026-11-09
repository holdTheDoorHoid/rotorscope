//! Differential tests against the official opendroneid-core-c decoder/encoder.
#![allow(clippy::field_reassign_with_default)]

mod common;

use common::*;
use odid_ref as c;
use rid::message::{Message, decode, raw_messages};
use rid::{Error, encode};

const CASES: usize = 50_000;

/// Encode random valid inputs with core-c, then check (1) both decoders agree
/// field by field, (2) our encoder produces the same bytes from the same input,
/// (3) re-encoding our decoded message reproduces a message that decodes the same.
fn roundtrip(raw: [u8; 25], ours_input: Message) {
    assert!(check_against_c(&raw));
    let mine = encode::message(&ours_input).unwrap_or_else(|e| panic!("{e:?} for {ours_input:?}"));
    assert_eq!(mine, raw, "encoder differs from core-c for {ours_input:?}");
    let decoded = decode(&raw).unwrap();
    let again = encode::message(&decoded).unwrap();
    assert_eq!(decode(&again).unwrap(), decoded);
}

#[test]
fn basic_id_matches_core_c() {
    let mut r = Rng::new(1);
    for _ in 0..CASES {
        let d = gen_basic_id(&mut r);
        roundtrip(c_encode_basic_id(&d), ours_basic_id(&d));
    }
}

#[test]
fn location_matches_core_c() {
    let mut r = Rng::new(2);
    for _ in 0..CASES * 5 {
        let d = gen_location(&mut r);
        roundtrip(c_encode_location(&d), ours_location(&d));
    }
}

#[test]
fn auth_matches_core_c() {
    let mut r = Rng::new(3);
    for _ in 0..CASES {
        let page = if r.chance(3) { 0 } else { r.range(1, 15) as u8 };
        let d = gen_auth(&mut r, page);
        roundtrip(c_encode_auth(&d), ours_auth(&d));
    }
}

#[test]
fn self_id_matches_core_c() {
    let mut r = Rng::new(4);
    for _ in 0..CASES {
        let d = gen_self_id(&mut r);
        roundtrip(c_encode_self_id(&d), ours_self_id(&d));
    }
}

#[test]
fn system_matches_core_c() {
    let mut r = Rng::new(5);
    for _ in 0..CASES * 5 {
        let d = gen_system(&mut r);
        roundtrip(c_encode_system(&d), ours_system(&d));
    }
}

#[test]
fn operator_id_matches_core_c() {
    let mut r = Rng::new(6);
    for _ in 0..CASES {
        let d = gen_operator_id(&mut r);
        roundtrip(c_encode_operator_id(&d), ours_operator_id(&d));
    }
}

/// Every bit pattern, not just what an encoder would emit: random bytes under
/// each type nibble, decoded by both.
#[test]
fn arbitrary_bytes_decode_like_core_c() {
    let mut r = Rng::new(7);
    let mut decoded = 0;
    for i in 0..1_000_000 {
        let mut raw = [0u8; 25];
        r.fill(&mut raw);
        raw[0] = (raw[0] & 0x0F) | (((i % 16) as u8) << 4);
        if raw[0] >> 4 == 0xF {
            assert_eq!(decode(&raw), Err(Error::UnknownType(0xF)));
            continue;
        }
        // Bias the auth length fields towards the valid region so both branches run.
        if raw[0] >> 4 == 2 && r.chance(2) {
            raw[2] &= 0x0F;
        }
        if check_against_c(&raw) {
            decoded += 1;
        }
    }
    assert!(decoded > 300_000, "{decoded}");
}

/// Packs built by core-c split into the same messages, decode the same, and
/// our pack builder reproduces them byte for byte.
#[test]
fn packs_match_core_c() {
    let mut r = Rng::new(8);
    for _ in 0..CASES {
        let u = gen_uas(&mut r);
        let pack = c_build_pack(&u);
        let msgs: Vec<[u8; 25]> = raw_messages(&pack).unwrap().copied().collect();
        assert_eq!(msgs.len(), pack[2] as usize);
        assert_eq!(pack.len(), 3 + 25 * msgs.len());
        for (i, m) in msgs.iter().enumerate() {
            assert_eq!(&m[..], &pack[3 + 25 * i..28 + 25 * i]);
            check_against_c(m);
        }

        // core-c's own pack decoder fills the same values ours yields.
        let mut uas = c::uas_data();
        let n = unsafe { c::odid_message_process_pack(&mut uas, pack.as_ptr(), pack.len()) };
        assert_eq!(n as usize, pack.len());
        for m in &msgs {
            if let Ok(Message::Location(l)) = decode(m) {
                assert_eq!(uas.LocationValid, 1);
                assert_eq!(
                    l.speed_horizontal.unwrap_or(255.0),
                    uas.Location.SpeedHorizontal
                );
            }
            if let Ok(Message::System(s)) = decode(m) {
                assert_eq!(uas.SystemValid, 1);
                assert_eq!(s.area_count, uas.System.AreaCount);
            }
        }

        let mut out = [0u8; 300];
        let n = encode::pack(&msgs, &mut out).unwrap();
        assert_eq!(&out[..n], &pack[..]);
    }
}

/// Random pack-shaped byte strings: whenever core-c accepts one, we must yield
/// exactly its messages; whenever we reject one, core-c must too.
#[test]
fn arbitrary_packs_agree_with_core_c() {
    let mut r = Rng::new(9);
    let mut accepted = 0;
    for _ in 0..200_000 {
        let count = r.range(0, 11) as usize;
        let len = (3 + 25 * count).saturating_sub(r.below(3)) + r.below(2);
        let mut buf = r.bytes(len.max(3));
        buf[0] = 0xF0 | (buf[0] & 0x0F);
        buf[1] = if r.chance(10) { r.u8() } else { 25 };
        buf[2] = count as u8;
        for i in 0..count {
            if let Some(b) = buf.get_mut(3 + 25 * i) {
                *b = ((r.range(0, 6) as u8) << 4) | (*b & 0x0F);
            }
        }
        let mut uas = c::uas_data();
        let c_ok = unsafe { c::odid_message_process_pack(&mut uas, buf.as_ptr(), buf.len()) } >= 0;
        match raw_messages(&buf) {
            Ok(it) => {
                let n = it.count();
                assert_eq!(n, count);
                accepted += c_ok as usize;
            }
            Err(e) => assert!(
                !c_ok,
                "we rejected ({e:?}) a pack core-c accepts: {buf:02x?}"
            ),
        }
    }
    assert!(accepted > 10_000, "{accepted}");
}

#[test]
fn single_message_payload_yields_itself() {
    let mut raw = [0u8; 25];
    raw[0] = 0x12;
    let all: Vec<_> = raw_messages(&raw).unwrap().collect();
    assert_eq!(all, vec![&raw]);
    assert_eq!(raw_messages(&raw[..24]).err(), Some(Error::Truncated));
    assert_eq!(raw_messages(&[]).err(), Some(Error::Truncated));
    // Nested pack is refused.
    let mut pack = vec![0xF2, 25, 1];
    pack.extend_from_slice(&[0xF2; 25]);
    assert_eq!(raw_messages(&pack).err(), Some(Error::BadPack));
}

#[test]
fn json_output_is_readable() {
    let mut r = Rng::new(10);
    let mut d = gen_location(&mut r);
    d.Latitude = 39.95;
    d.Longitude = -75.16;
    d.Direction = 361.0;
    let raw = c_encode_location(&d);
    let json = serde_json::to_string(&decode(&raw).unwrap()).unwrap();
    assert!(json.contains("\"direction\":null"), "{json}");
    assert!(json.contains("\"lat\":39.95"), "{json}");

    let mut b = c::ODID_BasicID_data::default();
    b.IDType = 1;
    b.UAType = 2;
    set_cstr(&mut b.UASID, b"1581F5FJD123");
    let json = serde_json::to_string(&decode(&c_encode_basic_id(&b)).unwrap()).unwrap();
    assert_eq!(
        json,
        r#"{"BasicId":{"id_type":"SerialNumber","ua_type":"HelicopterOrMultirotor","uas_id":"1581F5FJD123"}}"#
    );

    b.IDType = 3;
    b.UASID = [0; 21];
    b.UASID[0] = 0xAB_u8 as i8;
    b.UASID[3] = 0x01;
    let json = serde_json::to_string(&decode(&c_encode_basic_id(&b)).unwrap()).unwrap();
    assert!(json.contains(r#""uas_id":"ab000001""#), "{json}");
}

#[test]
fn encoder_rejects_what_core_c_rejects() {
    let mut r = Rng::new(11);
    for _ in 0..CASES {
        let mut d = gen_location(&mut r);
        match r.below(5) {
            0 => d.Direction = r.f32_in(360.01, 360.99),
            1 => d.SpeedHorizontal = r.f32_in(254.26, 254.99),
            2 => d.SpeedVertical = r.f32_in(-1000.0, -62.01),
            3 => d.AltitudeGeo = r.f32_in(31767.6, 40000.0),
            _ => d.TimeStamp = r.f32_in(3600.01, 60000.0),
        }
        let mut out = [0u8; 25];
        assert_ne!(unsafe { c::encodeLocationMessage(out.as_mut_ptr(), &d) }, 0);
        assert_eq!(
            encode::message(&ours_location(&d)),
            Err(Error::Invalid),
            "{d:?}"
        );
    }
}
