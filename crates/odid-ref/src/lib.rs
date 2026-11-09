//! FFI to the vendored opendroneid-core-c (Apache-2.0), for differential tests.
//!
//! Hand-written mirrors of the C structs in `opendroneid.h`, built with the
//! library's defaults (`ODID_BASIC_ID_MAX_MESSAGES = 2`,
//! `ODID_AUTH_MAX_PAGES = 16`). C enums are `int` on every host we test on.
//! [`check_layout`] compares every mirror against the compiler's `sizeof`, and
//! the crate's own test runs it, so a drift fails loudly.
//!
//! Host-only. Never a dependency of firmware.
#![allow(non_snake_case, non_camel_case_types)]

use core::ffi::{c_char, c_int};

pub const ODID_MESSAGE_SIZE: usize = 25;
pub const ODID_ID_SIZE: usize = 20;
pub const ODID_STR_SIZE: usize = 23;
pub const ODID_AUTH_MAX_PAGES: usize = 16;
pub const ODID_BASIC_ID_MAX_MESSAGES: usize = 2;
pub const ODID_PACK_MAX_MESSAGES: usize = 9;
pub const ODID_SUCCESS: c_int = 0;

pub const ODID_MESSAGETYPE_BASIC_ID: c_int = 0;
pub const ODID_MESSAGETYPE_LOCATION: c_int = 1;
pub const ODID_MESSAGETYPE_AUTH: c_int = 2;
pub const ODID_MESSAGETYPE_SELF_ID: c_int = 3;
pub const ODID_MESSAGETYPE_SYSTEM: c_int = 4;
pub const ODID_MESSAGETYPE_OPERATOR_ID: c_int = 5;
pub const ODID_MESSAGETYPE_PACKED: c_int = 0xF;
pub const ODID_MESSAGETYPE_INVALID: c_int = 0xFF;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ODID_BasicID_data {
    pub UAType: c_int,
    pub IDType: c_int,
    pub UASID: [c_char; ODID_ID_SIZE + 1],
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ODID_Location_data {
    pub Status: c_int,
    pub Direction: f32,
    pub SpeedHorizontal: f32,
    pub SpeedVertical: f32,
    pub Latitude: f64,
    pub Longitude: f64,
    pub AltitudeBaro: f32,
    pub AltitudeGeo: f32,
    pub HeightType: c_int,
    pub Height: f32,
    pub HorizAccuracy: c_int,
    pub VertAccuracy: c_int,
    pub BaroAccuracy: c_int,
    pub SpeedAccuracy: c_int,
    pub TSAccuracy: c_int,
    pub TimeStamp: f32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ODID_Auth_data {
    pub DataPage: u8,
    pub AuthType: c_int,
    pub LastPageIndex: u8,
    pub Length: u8,
    pub Timestamp: u32,
    pub AuthData: [u8; 24],
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ODID_SelfID_data {
    pub DescType: c_int,
    pub Desc: [c_char; ODID_STR_SIZE + 1],
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ODID_System_data {
    pub OperatorLocationType: c_int,
    pub ClassificationType: c_int,
    pub OperatorLatitude: f64,
    pub OperatorLongitude: f64,
    pub AreaCount: u16,
    pub AreaRadius: u16,
    pub AreaCeiling: f32,
    pub AreaFloor: f32,
    pub CategoryEU: c_int,
    pub ClassEU: c_int,
    pub OperatorAltitudeGeo: f32,
    pub Timestamp: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ODID_OperatorID_data {
    pub OperatorIdType: c_int,
    pub OperatorId: [c_char; ODID_ID_SIZE + 1],
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ODID_UAS_Data {
    pub BasicID: [ODID_BasicID_data; ODID_BASIC_ID_MAX_MESSAGES],
    pub Location: ODID_Location_data,
    pub Auth: [ODID_Auth_data; ODID_AUTH_MAX_PAGES],
    pub SelfID: ODID_SelfID_data,
    pub System: ODID_System_data,
    pub OperatorID: ODID_OperatorID_data,
    pub BasicIDValid: [u8; ODID_BASIC_ID_MAX_MESSAGES],
    pub LocationValid: u8,
    pub AuthValid: [u8; ODID_AUTH_MAX_PAGES],
    pub SelfIDValid: u8,
    pub SystemValid: u8,
    pub OperatorIDValid: u8,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ODID_MessagePack_data {
    pub SingleMessageSize: u8,
    pub MsgPackSize: u8,
    pub Messages: [[u8; ODID_MESSAGE_SIZE]; ODID_PACK_MAX_MESSAGES],
}

/// Encoded (wire) message pack: 3-byte header plus up to nine messages.
pub type ODID_MessagePack_encoded = [u8; 3 + ODID_MESSAGE_SIZE * ODID_PACK_MAX_MESSAGES];
/// Any single encoded message. The C side takes typed pointers; all are 25 bytes.
pub type ODID_Message_encoded = [u8; ODID_MESSAGE_SIZE];

// Pointers to the encoded C structs are passed as `*mut u8`/`*const u8`: every
// one of them is a byte-packed 25-byte (or 228-byte pack) struct.
unsafe extern "C" {
    pub fn odid_initBasicIDData(data: *mut ODID_BasicID_data);
    pub fn odid_initLocationData(data: *mut ODID_Location_data);
    pub fn odid_initAuthData(data: *mut ODID_Auth_data);
    pub fn odid_initSelfIDData(data: *mut ODID_SelfID_data);
    pub fn odid_initSystemData(data: *mut ODID_System_data);
    pub fn odid_initOperatorIDData(data: *mut ODID_OperatorID_data);
    pub fn odid_initMessagePackData(data: *mut ODID_MessagePack_data);
    pub fn odid_initUasData(data: *mut ODID_UAS_Data);

    pub fn encodeBasicIDMessage(out: *mut u8, data: *const ODID_BasicID_data) -> c_int;
    pub fn encodeLocationMessage(out: *mut u8, data: *const ODID_Location_data) -> c_int;
    pub fn encodeAuthMessage(out: *mut u8, data: *const ODID_Auth_data) -> c_int;
    pub fn encodeSelfIDMessage(out: *mut u8, data: *const ODID_SelfID_data) -> c_int;
    pub fn encodeSystemMessage(out: *mut u8, data: *const ODID_System_data) -> c_int;
    pub fn encodeOperatorIDMessage(out: *mut u8, data: *const ODID_OperatorID_data) -> c_int;
    pub fn encodeMessagePack(out: *mut u8, data: *const ODID_MessagePack_data) -> c_int;

    pub fn decodeBasicIDMessage(out: *mut ODID_BasicID_data, enc: *const u8) -> c_int;
    pub fn decodeLocationMessage(out: *mut ODID_Location_data, enc: *const u8) -> c_int;
    pub fn decodeAuthMessage(out: *mut ODID_Auth_data, enc: *const u8) -> c_int;
    pub fn decodeSelfIDMessage(out: *mut ODID_SelfID_data, enc: *const u8) -> c_int;
    pub fn decodeSystemMessage(out: *mut ODID_System_data, enc: *const u8) -> c_int;
    pub fn decodeOperatorIDMessage(out: *mut ODID_OperatorID_data, enc: *const u8) -> c_int;
    pub fn decodeMessagePack(uas: *mut ODID_UAS_Data, pack: *const u8) -> c_int;
    pub fn decodeMessageType(byte: u8) -> c_int;
    /// Reads `ODID_MESSAGE_SIZE` bytes, or a whole pack if the type is 0xF.
    pub fn decodeOpenDroneID(uas: *mut ODID_UAS_Data, msg: *const u8) -> c_int;

    pub fn odid_message_build_pack(uas: *const ODID_UAS_Data, pack: *mut u8, buflen: usize) -> c_int;
    pub fn odid_message_process_pack(uas: *mut ODID_UAS_Data, pack: *const u8, buflen: usize) -> c_int;

    pub fn odid_wifi_build_nan_sync_beacon_frame(mac: *const c_char, buf: *mut u8, size: usize) -> c_int;
    pub fn odid_wifi_build_message_pack_nan_action_frame(
        uas: *const ODID_UAS_Data,
        mac: *const c_char,
        send_counter: u8,
        buf: *mut u8,
        size: usize,
    ) -> c_int;
    pub fn odid_wifi_build_message_pack_beacon_frame(
        uas: *const ODID_UAS_Data,
        mac: *const c_char,
        ssid: *const c_char,
        ssid_len: usize,
        interval_tu: u16,
        send_counter: u8,
        buf: *mut u8,
        size: usize,
    ) -> c_int;
    pub fn odid_wifi_receive_message_pack_nan_action_frame(
        uas: *mut ODID_UAS_Data,
        mac: *mut c_char,
        buf: *const u8,
        size: usize,
    ) -> c_int;

    fn odid_ref_sizeof(which: c_int) -> usize;
}

/// A freshly initialised `ODID_UAS_Data` (all invalid, C defaults).
pub fn uas_data() -> ODID_UAS_Data {
    let mut d = ODID_UAS_Data::default();
    // SAFETY: valid, exclusively borrowed struct with the C layout.
    unsafe { odid_initUasData(&mut d) };
    d
}

/// Panics if any Rust mirror's size or a key offset disagrees with the C compiler.
pub fn check_layout() {
    use core::mem::{offset_of, size_of};
    let expect: [(c_int, usize, &str); 19] = [
        (0, size_of::<ODID_BasicID_data>(), "BasicID_data"),
        (1, size_of::<ODID_Location_data>(), "Location_data"),
        (2, size_of::<ODID_Auth_data>(), "Auth_data"),
        (3, size_of::<ODID_SelfID_data>(), "SelfID_data"),
        (4, size_of::<ODID_System_data>(), "System_data"),
        (5, size_of::<ODID_OperatorID_data>(), "OperatorID_data"),
        (6, size_of::<ODID_UAS_Data>(), "UAS_Data"),
        (7, size_of::<ODID_MessagePack_data>(), "MessagePack_data"),
        (8, size_of::<ODID_MessagePack_encoded>(), "MessagePack_encoded"),
        (9, offset_of!(ODID_UAS_Data, Location), "UAS.Location"),
        (10, offset_of!(ODID_UAS_Data, Auth), "UAS.Auth"),
        (11, offset_of!(ODID_UAS_Data, SelfID), "UAS.SelfID"),
        (12, offset_of!(ODID_UAS_Data, System), "UAS.System"),
        (13, offset_of!(ODID_UAS_Data, OperatorID), "UAS.OperatorID"),
        (14, offset_of!(ODID_UAS_Data, BasicIDValid), "UAS.BasicIDValid"),
        (15, offset_of!(ODID_UAS_Data, OperatorIDValid), "UAS.OperatorIDValid"),
        (16, offset_of!(ODID_Location_data, TimeStamp), "Location.TimeStamp"),
        (17, offset_of!(ODID_System_data, Timestamp), "System.Timestamp"),
        (18, offset_of!(ODID_Auth_data, AuthData), "Auth.AuthData"),
    ];
    for (which, rust, name) in expect {
        // SAFETY: pure function returning a constant.
        let c = unsafe { odid_ref_sizeof(which) };
        assert_eq!(rust, c, "layout mismatch for {name}");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn layout_matches_c() {
        super::check_layout();
    }
}
