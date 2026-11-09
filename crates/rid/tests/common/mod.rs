//! Shared helpers for the differential and robustness tests: a seeded PRNG,
//! generators for random-but-valid core-c input structs, and comparisons
//! between core-c's decoded structs and ours.
#![allow(dead_code, clippy::field_reassign_with_default)]

use core::ffi::c_char;
use odid_ref as c;
use rid::message::*;

/// SplitMix64: tiny, seedable, good enough for test-case generation.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }
    pub fn u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    pub fn u8(&mut self) -> u8 {
        self.u64() as u8
    }
    pub fn u16(&mut self) -> u16 {
        self.u64() as u16
    }
    pub fn u32(&mut self) -> u32 {
        self.u64() as u32
    }
    /// Uniform in `lo..=hi`.
    pub fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.u64() % ((hi - lo + 1) as u64)) as i64
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.u64() % n as u64) as usize
    }
    pub fn chance(&mut self, one_in: u64) -> bool {
        self.u64().is_multiple_of(one_in)
    }
    pub fn unit(&mut self) -> f64 {
        (self.u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    /// Uniform float in `[lo, hi]`, hitting the endpoints now and then.
    pub fn f32_in(&mut self, lo: f32, hi: f32) -> f32 {
        match self.below(16) {
            0 => lo,
            1 => hi,
            _ => (lo as f64 + (hi as f64 - lo as f64) * self.unit()).clamp(lo as f64, hi as f64)
                as f32,
        }
    }
    pub fn f64_in(&mut self, lo: f64, hi: f64) -> f64 {
        match self.below(16) {
            0 => lo,
            1 => hi,
            2 => 0.0f64.clamp(lo, hi),
            _ => (lo + (hi - lo) * self.unit()).clamp(lo, hi),
        }
    }
    pub fn fill(&mut self, buf: &mut [u8]) {
        for b in buf {
            *b = self.u8();
        }
    }
    pub fn bytes(&mut self, n: usize) -> Vec<u8> {
        let mut v = vec![0; n];
        self.fill(&mut v);
        v
    }
    /// Printable ASCII of length `0..=max`.
    pub fn text(&mut self, max: usize) -> Vec<u8> {
        let n = self.below(max + 1);
        (0..n).map(|_| self.range(0x20, 0x7E) as u8).collect()
    }
}

pub fn set_cstr(dst: &mut [c_char], src: &[u8]) {
    dst.fill(0);
    for (d, s) in dst.iter_mut().zip(src) {
        *d = *s as c_char;
    }
}

/// C string content up to the first NUL.
pub fn cstr(src: &[c_char]) -> Vec<u8> {
    src.iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect()
}

/// Our text field up to the first NUL.
pub fn ours_str<const N: usize>(f: &FixedStr<N>) -> Vec<u8> {
    f.raw().iter().copied().take_while(|&b| b != 0).collect()
}

// --- random core-c inputs ----------------------------------------------------

const UNKNOWN_ALT: f32 = -1000.0;

fn alt(r: &mut Rng) -> f32 {
    if r.chance(6) {
        UNKNOWN_ALT
    } else {
        r.f32_in(-1000.0, 31767.5)
    }
}

pub fn gen_basic_id(r: &mut Rng) -> c::ODID_BasicID_data {
    let mut d = c::ODID_BasicID_data::default();
    d.IDType = r.range(0, 15) as i32;
    d.UAType = r.range(0, 15) as i32;
    if d.IDType == 1 || d.IDType == 2 {
        set_cstr(&mut d.UASID, &r.text(20));
    } else {
        set_cstr(&mut d.UASID, &r.bytes(20));
    }
    d
}

pub fn gen_location(r: &mut Rng) -> c::ODID_Location_data {
    let mut d = c::ODID_Location_data::default();
    unsafe { c::odid_initLocationData(&mut d) };
    d.Status = r.range(0, 15) as i32;
    d.Direction = if r.chance(8) {
        361.0
    } else {
        r.f32_in(0.0, 360.0)
    };
    d.SpeedHorizontal = if r.chance(8) {
        255.0
    } else {
        r.f32_in(0.0, 254.25)
    };
    d.SpeedVertical = if r.chance(8) {
        63.0
    } else {
        r.f32_in(-62.0, 62.0)
    };
    if !r.chance(8) {
        d.Latitude = r.f64_in(-90.0, 90.0);
        d.Longitude = r.f64_in(-180.0, 180.0);
    }
    d.AltitudeBaro = alt(r);
    d.AltitudeGeo = alt(r);
    d.HeightType = r.range(0, 1) as i32;
    d.Height = alt(r);
    d.HorizAccuracy = r.range(0, 15) as i32;
    d.VertAccuracy = r.range(0, 15) as i32;
    d.BaroAccuracy = r.range(0, 15) as i32;
    d.SpeedAccuracy = r.range(0, 15) as i32;
    d.TSAccuracy = r.range(0, 15) as i32;
    d.TimeStamp = if r.chance(8) {
        65535.0
    } else {
        r.f32_in(0.0, 3600.0)
    };
    d
}

pub fn gen_auth(r: &mut Rng, page: u8) -> c::ODID_Auth_data {
    let mut d = c::ODID_Auth_data::default();
    d.DataPage = page;
    d.AuthType = r.range(0, 15) as i32;
    if page == 0 {
        d.LastPageIndex = r.range(0, 15) as u8;
        let cap = (17 + 23 * d.LastPageIndex as i64).min(255);
        d.Length = r.range(0, cap) as u8;
        d.Timestamp = r.u32();
        r.fill(&mut d.AuthData[..17]);
    } else {
        r.fill(&mut d.AuthData[..23]);
    }
    d
}

pub fn gen_self_id(r: &mut Rng) -> c::ODID_SelfID_data {
    let mut d = c::ODID_SelfID_data::default();
    d.DescType = r.range(0, 255) as i32;
    set_cstr(&mut d.Desc, &r.text(23));
    d
}

pub fn gen_system(r: &mut Rng) -> c::ODID_System_data {
    let mut d = c::ODID_System_data::default();
    unsafe { c::odid_initSystemData(&mut d) };
    d.OperatorLocationType = r.range(0, 3) as i32;
    d.ClassificationType = r.range(0, 7) as i32;
    if !r.chance(8) {
        d.OperatorLatitude = r.f64_in(-90.0, 90.0);
        d.OperatorLongitude = r.f64_in(-180.0, 180.0);
    }
    d.AreaCount = r.u16();
    d.AreaRadius = r.range(0, 2550) as u16;
    d.AreaCeiling = alt(r);
    d.AreaFloor = alt(r);
    d.CategoryEU = r.range(0, 15) as i32;
    d.ClassEU = r.range(0, 15) as i32;
    d.OperatorAltitudeGeo = alt(r);
    d.Timestamp = if r.chance(8) { 0 } else { r.u32() };
    d
}

pub fn gen_operator_id(r: &mut Rng) -> c::ODID_OperatorID_data {
    let mut d = c::ODID_OperatorID_data::default();
    d.OperatorIdType = r.range(0, 255) as i32;
    set_cstr(&mut d.OperatorId, &r.text(20));
    d
}

/// A random UAS with a random subset of messages, at most nine in total.
pub fn gen_uas(r: &mut Rng) -> c::ODID_UAS_Data {
    let mut u = c::uas_data();
    let mut budget = 1 + r.below(9);
    let mut take = |r: &mut Rng| {
        if budget > 0 && r.chance(2) {
            budget -= 1;
            true
        } else {
            false
        }
    };
    for i in 0..2 {
        if take(r) {
            u.BasicID[i] = gen_basic_id(r);
            u.BasicIDValid[i] = 1;
        }
    }
    if take(r) {
        u.Location = gen_location(r);
        u.LocationValid = 1;
    }
    for p in 0..16 {
        if r.chance(4) && take(r) {
            u.Auth[p] = gen_auth(r, p as u8);
            u.AuthValid[p] = 1;
        }
    }
    if take(r) {
        u.SelfID = gen_self_id(r);
        u.SelfIDValid = 1;
    }
    if take(r) {
        u.System = gen_system(r);
        u.SystemValid = 1;
    }
    if take(r) {
        u.OperatorID = gen_operator_id(r);
        u.OperatorIDValid = 1;
    }
    if (0..2).all(|i| u.BasicIDValid[i] == 0) && u.LocationValid == 0 {
        // Never empty: core-c refuses to build an empty pack. Make room first
        // if all nine slots are taken.
        if budget == 0 {
            if u.SelfIDValid == 1 {
                u.SelfIDValid = 0;
            } else if u.SystemValid == 1 {
                u.SystemValid = 0;
            } else if u.OperatorIDValid == 1 {
                u.OperatorIDValid = 0;
            } else if let Some(p) = (0..16).rev().find(|&p| u.AuthValid[p] == 1) {
                u.AuthValid[p] = 0;
            }
        }
        u.Location = gen_location(r);
        u.LocationValid = 1;
    }
    u
}

// --- core-c input -> our struct (for encoder byte comparison) -------------------

fn opt_alt(a: f32) -> Option<f32> {
    (a != UNKNOWN_ALT).then_some(a)
}

fn opt_pos(lat: f64, lon: f64) -> Option<LatLon> {
    (lat != 0.0 || lon != 0.0).then_some(LatLon { lat, lon })
}

pub fn ours_basic_id(d: &c::ODID_BasicID_data) -> Message {
    let raw: Vec<u8> = d.UASID[..20].iter().map(|&c| c as u8).collect();
    Message::BasicId(BasicId {
        id_type: IdType::from_code(d.IDType as u8),
        ua_type: UaType::from_code(d.UAType as u8),
        uas_id: FixedStr::new(&raw).unwrap(),
    })
}

pub fn ours_location(d: &c::ODID_Location_data) -> Message {
    Message::Location(Location {
        status: Status::from_code(d.Status as u8),
        height_type: if d.HeightType == 1 {
            HeightType::AboveGround
        } else {
            HeightType::AboveTakeoff
        },
        direction: (d.Direction != 361.0).then_some(d.Direction),
        speed_horizontal: (d.SpeedHorizontal != 255.0).then_some(d.SpeedHorizontal),
        speed_vertical: (d.SpeedVertical != 63.0).then_some(d.SpeedVertical),
        position: opt_pos(d.Latitude, d.Longitude),
        altitude_pressure: opt_alt(d.AltitudeBaro),
        altitude_geodetic: opt_alt(d.AltitudeGeo),
        height: opt_alt(d.Height),
        horizontal_accuracy: HorizontalAccuracy::from_code(d.HorizAccuracy as u8),
        vertical_accuracy: VerticalAccuracy::from_code(d.VertAccuracy as u8),
        baro_accuracy: VerticalAccuracy::from_code(d.BaroAccuracy as u8),
        speed_accuracy: SpeedAccuracy::from_code(d.SpeedAccuracy as u8),
        timestamp: (d.TimeStamp != 65535.0).then_some(d.TimeStamp),
        timestamp_accuracy: (d.TSAccuracy != 0).then(|| d.TSAccuracy as f32 / 10.0),
    })
}

pub fn ours_auth(d: &c::ODID_Auth_data) -> Message {
    let (page_zero, data) = if d.DataPage == 0 {
        (
            Some(AuthPageZero {
                last_page_index: d.LastPageIndex,
                length: d.Length,
                timestamp: d.Timestamp,
            }),
            &d.AuthData[..17],
        )
    } else {
        (None, &d.AuthData[..23])
    };
    Message::Auth(Auth {
        auth_type: AuthType::from_code(d.AuthType as u8),
        page: d.DataPage,
        page_zero,
        data: AuthData::new(data).unwrap(),
    })
}

pub fn ours_self_id(d: &c::ODID_SelfID_data) -> Message {
    Message::SelfId(SelfId {
        desc_type: DescType::from_code(d.DescType as u8),
        description: FixedStr::new(&cstr(&d.Desc[..23])).unwrap(),
    })
}

pub fn ours_system(d: &c::ODID_System_data) -> Message {
    Message::System(System {
        operator_location_type: OperatorLocationType::from_code(d.OperatorLocationType as u8),
        classification_type: ClassificationType::from_code(d.ClassificationType as u8),
        operator_position: opt_pos(d.OperatorLatitude, d.OperatorLongitude),
        area_count: d.AreaCount,
        area_radius: d.AreaRadius,
        area_ceiling: opt_alt(d.AreaCeiling),
        area_floor: opt_alt(d.AreaFloor),
        category: Category::from_code(d.CategoryEU as u8),
        class: UaClass::from_code(d.ClassEU as u8),
        operator_altitude: opt_alt(d.OperatorAltitudeGeo),
        timestamp: (d.Timestamp != 0).then_some(d.Timestamp),
    })
}

pub fn ours_operator_id(d: &c::ODID_OperatorID_data) -> Message {
    Message::OperatorId(OperatorId {
        operator_id_type: OperatorIdType::from_code(d.OperatorIdType as u8),
        operator_id: FixedStr::new(&cstr(&d.OperatorId[..20])).unwrap(),
    })
}

// --- core-c encode wrappers ------------------------------------------------------

pub fn c_encode_basic_id(d: &c::ODID_BasicID_data) -> [u8; 25] {
    let mut out = [0u8; 25];
    assert_eq!(unsafe { c::encodeBasicIDMessage(out.as_mut_ptr(), d) }, 0);
    out
}
pub fn c_encode_location(d: &c::ODID_Location_data) -> [u8; 25] {
    let mut out = [0u8; 25];
    assert_eq!(
        unsafe { c::encodeLocationMessage(out.as_mut_ptr(), d) },
        0,
        "{d:?}"
    );
    out
}
pub fn c_encode_auth(d: &c::ODID_Auth_data) -> [u8; 25] {
    let mut out = [0u8; 25];
    assert_eq!(
        unsafe { c::encodeAuthMessage(out.as_mut_ptr(), d) },
        0,
        "{d:?}"
    );
    out
}
pub fn c_encode_self_id(d: &c::ODID_SelfID_data) -> [u8; 25] {
    let mut out = [0u8; 25];
    assert_eq!(unsafe { c::encodeSelfIDMessage(out.as_mut_ptr(), d) }, 0);
    out
}
pub fn c_encode_system(d: &c::ODID_System_data) -> [u8; 25] {
    let mut out = [0u8; 25];
    assert_eq!(
        unsafe { c::encodeSystemMessage(out.as_mut_ptr(), d) },
        0,
        "{d:?}"
    );
    out
}
pub fn c_encode_operator_id(d: &c::ODID_OperatorID_data) -> [u8; 25] {
    let mut out = [0u8; 25];
    assert_eq!(
        unsafe { c::encodeOperatorIDMessage(out.as_mut_ptr(), d) },
        0
    );
    out
}

/// core-c's pack for a UAS, as bytes.
pub fn c_build_pack(u: &c::ODID_UAS_Data) -> Vec<u8> {
    let mut buf = [0u8; 300];
    let n = unsafe { c::odid_message_build_pack(u, buf.as_mut_ptr(), buf.len()) };
    assert!(n > 0, "odid_message_build_pack failed: {n}");
    buf[..n as usize].to_vec()
}

// --- comparing core-c's decode with ours ---------------------------------------

fn same_alt(c: f32, ours: Option<f32>) -> bool {
    if c == UNKNOWN_ALT {
        ours.is_none()
    } else {
        ours == Some(c)
    }
}

fn same_pos(lat: f64, lon: f64, ours: Option<LatLon>) -> bool {
    let unknown = (lat == 0.0 && lon == 0.0)
        || !(-90.0..=90.0).contains(&lat)
        || !(-180.0..=180.0).contains(&lon);
    if unknown {
        ours.is_none()
    } else {
        ours == Some(LatLon { lat, lon })
    }
}

/// Decode `raw` with core-c's per-type decoder and with ours; assert they agree
/// on acceptance and on every field. Returns whether it decoded.
pub fn check_against_c(raw: &[u8; 25]) -> bool {
    let ours = decode(raw);
    let p = raw.as_ptr();
    match raw[0] >> 4 {
        0 => {
            let mut d = c::ODID_BasicID_data::default();
            assert_eq!(unsafe { c::decodeBasicIDMessage(&mut d, p) }, 0);
            let Ok(Message::BasicId(m)) = ours else {
                panic!("{raw:02x?}: {ours:?}")
            };
            assert_eq!(m.id_type.code() as i32, d.IDType);
            assert_eq!(m.ua_type.code() as i32, d.UAType);
            if d.IDType == 1 || d.IDType == 2 {
                assert_eq!(ours_str(&m.uas_id), cstr(&d.UASID), "{raw:02x?}");
            } else {
                let c_bytes: Vec<u8> = d.UASID[..20].iter().map(|&c| c as u8).collect();
                assert_eq!(&m.uas_id.raw()[..], &c_bytes[..]);
            }
        }
        1 => {
            let mut d = c::ODID_Location_data::default();
            assert_eq!(unsafe { c::decodeLocationMessage(&mut d, p) }, 0);
            let Ok(Message::Location(m)) = ours else {
                panic!("{ours:?}")
            };
            let ctx = format!("{raw:02x?}\nC: {d:?}\nours: {m:?}");
            assert_eq!(m.status.code() as i32, d.Status, "{ctx}");
            assert_eq!(
                m.height_type == HeightType::AboveGround,
                d.HeightType == 1,
                "{ctx}"
            );
            assert_eq!(
                m.direction,
                (d.Direction < 360.0).then_some(d.Direction),
                "{ctx}"
            );
            assert_eq!(
                m.speed_horizontal,
                (d.SpeedHorizontal != 255.0).then_some(d.SpeedHorizontal),
                "{ctx}"
            );
            assert_eq!(
                m.speed_vertical,
                (d.SpeedVertical.abs() <= 62.0).then_some(d.SpeedVertical),
                "{ctx}"
            );
            assert!(same_pos(d.Latitude, d.Longitude, m.position), "{ctx}");
            assert!(same_alt(d.AltitudeBaro, m.altitude_pressure), "{ctx}");
            assert!(same_alt(d.AltitudeGeo, m.altitude_geodetic), "{ctx}");
            assert!(same_alt(d.Height, m.height), "{ctx}");
            assert_eq!(
                m.horizontal_accuracy.code() as i32,
                d.HorizAccuracy,
                "{ctx}"
            );
            assert_eq!(m.vertical_accuracy.code() as i32, d.VertAccuracy, "{ctx}");
            assert_eq!(m.baro_accuracy.code() as i32, d.BaroAccuracy, "{ctx}");
            assert_eq!(m.speed_accuracy.code() as i32, d.SpeedAccuracy, "{ctx}");
            assert_eq!(
                m.timestamp,
                (d.TimeStamp <= 3600.0).then_some(d.TimeStamp),
                "{ctx}"
            );
            let ts_acc = m
                .timestamp_accuracy
                .map_or(0, |a| (a * 10.0).round() as i32);
            assert_eq!(ts_acc, d.TSAccuracy, "{ctx}");
        }
        2 => {
            let mut d = c::ODID_Auth_data::default();
            let c_ok = unsafe { c::decodeAuthMessage(&mut d, p) } == 0;
            assert_eq!(c_ok, ours.is_ok(), "{raw:02x?}: {ours:?}");
            if !c_ok {
                assert_eq!(ours, Err(rid::Error::Invalid));
                return false;
            }
            let Ok(Message::Auth(m)) = ours else { panic!() };
            assert_eq!(m.auth_type.code() as i32, d.AuthType);
            assert_eq!(m.page, d.DataPage);
            if d.DataPage == 0 {
                let z = m.page_zero.expect("page zero fields");
                assert_eq!(
                    (z.last_page_index, z.length, z.timestamp),
                    (d.LastPageIndex, d.Length, d.Timestamp)
                );
                assert_eq!(m.data.as_bytes(), &d.AuthData[..17]);
            } else {
                assert!(m.page_zero.is_none());
                assert_eq!(m.data.as_bytes(), &d.AuthData[..23]);
            }
        }
        3 => {
            let mut d = c::ODID_SelfID_data::default();
            assert_eq!(unsafe { c::decodeSelfIDMessage(&mut d, p) }, 0);
            let Ok(Message::SelfId(m)) = ours else {
                panic!()
            };
            assert_eq!(m.desc_type.code() as i32, d.DescType);
            assert_eq!(ours_str(&m.description), cstr(&d.Desc));
        }
        4 => {
            let mut d = c::ODID_System_data::default();
            assert_eq!(unsafe { c::decodeSystemMessage(&mut d, p) }, 0);
            let Ok(Message::System(m)) = ours else {
                panic!()
            };
            let ctx = format!("{raw:02x?}\nC: {d:?}\nours: {m:?}");
            assert_eq!(
                m.operator_location_type.code() as i32,
                d.OperatorLocationType,
                "{ctx}"
            );
            assert_eq!(
                m.classification_type.code() as i32,
                d.ClassificationType,
                "{ctx}"
            );
            assert!(
                same_pos(d.OperatorLatitude, d.OperatorLongitude, m.operator_position),
                "{ctx}"
            );
            assert_eq!(m.area_count, d.AreaCount, "{ctx}");
            assert_eq!(m.area_radius, d.AreaRadius, "{ctx}");
            assert!(same_alt(d.AreaCeiling, m.area_ceiling), "{ctx}");
            assert!(same_alt(d.AreaFloor, m.area_floor), "{ctx}");
            assert_eq!(m.category.code() as i32, d.CategoryEU, "{ctx}");
            assert_eq!(m.class.code() as i32, d.ClassEU, "{ctx}");
            assert!(
                same_alt(d.OperatorAltitudeGeo, m.operator_altitude),
                "{ctx}"
            );
            assert_eq!(m.timestamp.unwrap_or(0), d.Timestamp, "{ctx}");
        }
        5 => {
            let mut d = c::ODID_OperatorID_data::default();
            assert_eq!(unsafe { c::decodeOperatorIDMessage(&mut d, p) }, 0);
            let Ok(Message::OperatorId(m)) = ours else {
                panic!()
            };
            assert_eq!(m.operator_id_type.code() as i32, d.OperatorIdType);
            assert_eq!(ours_str(&m.operator_id), cstr(&d.OperatorId));
        }
        t => {
            assert_eq!(ours, Err(rid::Error::UnknownType(t)));
            return false;
        }
    }
    true
}

/// Valid Wi-Fi/BLE frames to mutate in the robustness tests.
pub fn sample_frames(r: &mut Rng) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    let mac = [0x02, 0x11, 0x22, 0x33, 0x44, 0x55];
    for _ in 0..8 {
        let u = gen_uas(r);
        let mut buf = [0u8; 400];
        let n = unsafe {
            c::odid_wifi_build_message_pack_beacon_frame(
                &u,
                mac.as_ptr() as *const c_char,
                c"RID".as_ptr(),
                3,
                100,
                r.u8(),
                buf.as_mut_ptr(),
                buf.len(),
            )
        };
        assert!(n > 0);
        frames.push(buf[..n as usize].to_vec());
        let n = unsafe {
            c::odid_wifi_build_message_pack_nan_action_frame(
                &u,
                mac.as_ptr() as *const c_char,
                r.u8(),
                buf.as_mut_ptr(),
                buf.len(),
            )
        };
        assert!(n > 0);
        frames.push(buf[..n as usize].to_vec());
        let pack = c_build_pack(&u);
        let n = rid::encode::ble_service_data(r.u8(), &pack, &mut buf).unwrap();
        frames.push(buf[..n].to_vec());
        let n = rid::encode::ble_service_data(r.u8(), &pack[3..28], &mut buf).unwrap();
        frames.push(buf[..n].to_vec());
    }
    frames
}
