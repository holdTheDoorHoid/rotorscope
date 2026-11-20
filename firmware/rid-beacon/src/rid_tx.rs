//! Everything the beacon knows about the Remote ID wire format.
//!
//! This is the one place encoding happens. It is a thin layer over
//! `rid::encode` (the shared crate the receivers decode with), so a byte the
//! beacon sends means exactly what the receivers think it means. If the
//! encoder ever has to be swapped, only this file changes.
//!
//! Conduct: every identity field here is deliberately and obviously fake.
//! Do not change them to anything resembling a real serial number,
//! registration, operator ID or manufacturer.

use rid::message::{
    BasicId, Category, ClassificationType, DescType, FixedStr, HeightType, HorizontalAccuracy,
    IdType, LatLon, Location, Message, OperatorId, OperatorIdType, OperatorLocationType, SelfId,
    SpeedAccuracy, Status, System, UaClass, UaType, VerticalAccuracy,
};
use rid::{Error, MESSAGE_SIZE, RawMessage, encode};

use crate::flight::{Centre, Fix, GROUND_M, HEIGHT_AGL_M};

/// UAS ID. Sent as a "serial number" type because that is what receivers
/// display most prominently, but it is not a valid ANSI/CTA-2063-A serial:
/// the fifth character ('D') claims a 13-character serial and 14 follow.
pub const UAS_ID: &[u8] = b"TESTDRONE0000000001";
pub const OPERATOR_ID: &[u8] = b"TEST-OPERATOR";
/// Self-ID text. The field is 23 bytes, so "BENCH TEST - NOT AN AIRCRAFT"
/// (28) does not fit; this is the longest faithful abbreviation.
pub const SELF_ID: &[u8] = b"BENCH TEST-NOT AIRCRAFT";
/// The 802.11 SSID on the beacon.
pub const SSID: &[u8] = b"RID-TEST";

/// Messages per cycle, in the order they are packed and (for BLE 4) rotated.
pub const N_MESSAGES: usize = 5;
/// A Message Pack of all of them: 3-byte header + 25 per message.
pub const PACK_LEN: usize = 3 + MESSAGE_SIZE * N_MESSAGES;
/// Room for the largest frame we build (a beacon is ~190 bytes, NAN ~170).
pub const FRAME_MAX: usize = 256;
/// One legacy BLE AD structure is exactly 31 bytes; the extended one with a
/// pack is 6 + 128 = 134, comfortably inside one 251-byte HCI command.
pub const BLE_EXT_LEN: usize = 6 + PACK_LEN;

/// Short names for the rotated message types, for logs.
pub const MESSAGE_NAMES: [&str; N_MESSAGES] =
    ["basic_id", "location", "self_id", "system", "operator_id"];

/// Build the five messages for one instant. `uptime_s` stands in for the
/// clock: the beacon has no GPS or RTC, so the Location timestamp is
/// "seconds after the hour" taken from uptime, and the System timestamp is
/// left unknown.
pub fn messages(
    centre: &Centre,
    fix: &Fix,
    uptime_s: f64,
) -> Result<[RawMessage; N_MESSAGES], Error> {
    // The operator "stands" at the centre of the circle.
    let pilot = LatLon {
        lat: centre.lat,
        lon: centre.lon,
    };
    build(fix, HEIGHT_AGL_M, 0.0, Some((pilot, GROUND_M)), uptime_s)
}

/// The messages for one instant of a scripted path (feature `scripted`,
/// bench builds only; src/script.rs): the same five messages with the same
/// identity, but the path's height and climb rate, and no pilot position
/// (unknown, as F3411 allows).
#[cfg(feature = "scripted")]
pub fn messages_scripted(
    s: &crate::script::Sample,
    uptime_s: f64,
) -> Result<[RawMessage; N_MESSAGES], Error> {
    let fix = Fix {
        lat: s.lat,
        lon: s.lon,
        heading_deg: s.heading_deg,
        speed_mps: s.speed_mps,
    };
    build(&fix, s.height_m, s.vspeed_mps, None, uptime_s)
}

/// The five messages: `height_agl` above the ground (the geodetic height is
/// [`GROUND_M`] plus it), climbing at `vspeed`, the operator at `pilot`
/// (position and its geodetic height) or unknown.
fn build(
    fix: &Fix,
    height_agl: f32,
    vspeed: f32,
    pilot: Option<(LatLon, f32)>,
    uptime_s: f64,
) -> Result<[RawMessage; N_MESSAGES], Error> {
    let basic = Message::BasicId(BasicId {
        id_type: IdType::SerialNumber,
        ua_type: UaType::HelicopterOrMultirotor,
        uas_id: FixedStr::new(UAS_ID)?,
    });
    let location = Message::Location(Location {
        status: Status::Airborne,
        height_type: HeightType::AboveGround,
        direction: Some(fix.heading_deg),
        speed_horizontal: Some(fix.speed_mps),
        speed_vertical: Some(vspeed),
        position: Some(LatLon {
            lat: fix.lat,
            lon: fix.lon,
        }),
        altitude_pressure: None,
        altitude_geodetic: Some(GROUND_M + height_agl),
        height: Some(height_agl),
        horizontal_accuracy: HorizontalAccuracy::M10,
        vertical_accuracy: VerticalAccuracy::M10,
        baro_accuracy: VerticalAccuracy::Unknown,
        speed_accuracy: SpeedAccuracy::Mps1,
        // Tenths of a second resolution; 0..3600.
        timestamp: Some(((uptime_s % 3600.0) * 10.0) as u32 as f32 / 10.0),
        timestamp_accuracy: Some(0.1),
    });
    let self_id = Message::SelfId(SelfId {
        desc_type: DescType::Text,
        description: FixedStr::new(SELF_ID)?,
    });
    let system = Message::System(System {
        operator_location_type: OperatorLocationType::Takeoff,
        classification_type: ClassificationType::Undeclared,
        operator_position: pilot.map(|(p, _)| p),
        area_count: 1,
        area_radius: 0,
        area_ceiling: None,
        area_floor: None,
        category: Category::Undeclared,
        class: UaClass::Undeclared,
        operator_altitude: pilot.map(|(_, h)| h),
        timestamp: None,
    });
    let operator = Message::OperatorId(OperatorId {
        operator_id_type: OperatorIdType::OperatorId,
        operator_id: FixedStr::new(OPERATOR_ID)?,
    });
    Ok([
        encode::message(&basic)?,
        encode::message(&location)?,
        encode::message(&self_id)?,
        encode::message(&system)?,
        encode::message(&operator)?,
    ])
}

/// Message Pack of all five messages.
pub fn pack(msgs: &[RawMessage; N_MESSAGES]) -> Result<[u8; PACK_LEN], Error> {
    let mut out = [0u8; PACK_LEN];
    encode::pack(msgs, &mut out)?;
    Ok(out)
}

/// A complete 802.11 beacon (no FCS) carrying the pack in the ASTM vendor IE.
pub fn beacon(
    src: [u8; 6],
    channel: u8,
    counter: u8,
    pack: &[u8],
    out: &mut [u8; FRAME_MAX],
) -> Result<usize, Error> {
    encode::beacon_frame(src, SSID, channel, counter, pack, out)
}

/// A complete NAN Service Discovery action frame (no FCS) carrying the pack.
pub fn nan(
    src: [u8; 6],
    counter: u8,
    pack: &[u8],
    out: &mut [u8; FRAME_MAX],
) -> Result<usize, Error> {
    encode::nan_action_frame(src, counter, pack, out)
}

/// The NAN synchronisation beacon core-c sends just before each NAN action
/// frame. Some receivers (phone NAN stacks) need it to join the cluster.
pub fn nan_sync(src: [u8; 6], out: &mut [u8; FRAME_MAX]) -> Result<usize, Error> {
    encode::nan_sync_beacon(src, out)
}

/// BLE 4 legacy advertising data: one message, exactly 31 bytes. Legacy
/// adverts are too small for a pack, which is why BT4 Remote ID rotates.
pub fn ble_legacy(counter: u8, msg: &RawMessage) -> Result<[u8; 31], Error> {
    let mut out = [0u8; 31];
    encode::ble_service_data(counter, msg, &mut out)?;
    Ok(out)
}

/// BLE 5 extended advertising data (1M or coded): the whole pack.
pub fn ble_ext(counter: u8, pack: &[u8]) -> Result<[u8; BLE_EXT_LEN], Error> {
    let mut out = [0u8; BLE_EXT_LEN];
    encode::ble_service_data(counter, pack, &mut out)?;
    Ok(out)
}

/// Boot-time self check: decode what we encode with the receivers' decoder.
/// Catches a bad constant (e.g. an over-long ID) before anything is sent.
pub fn self_check(msgs: &[RawMessage; N_MESSAGES]) -> Result<(), Error> {
    for m in msgs {
        rid::message::decode(m)?;
    }
    let p = pack(msgs)?;
    if rid::message::raw_messages(&p)?.count() != N_MESSAGES {
        return Err(Error::BadPack);
    }
    Ok(())
}
