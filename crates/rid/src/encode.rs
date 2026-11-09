//! Encoding: the inverse of [`crate::message::decode`] plus frame builders,
//! for test transmitters. Enabled by the `encode` feature.
//!
//! Message encoding mirrors core-c's `encode*Message` arithmetic exactly
//! (including which steps run in `float` and which in `double`), so the same
//! input produces the same bytes; the differential tests check that. Inputs
//! outside what the standard can represent are rejected with
//! [`Error::Invalid`], matching core-c's checks, except that NaN is rejected
//! too (core-c lets it through into undefined behaviour).
//!
//! Frame builders write into a caller-supplied buffer and return the length
//! used, or [`Error::BufferTooSmall`].

use crate::message::{
    AUTH_PAGE_DATA, AUTH_PAGE_ZERO_DATA, Auth, BasicId, HeightType, LatLon, Location, Message,
    OperatorId, PROTOCOL_VERSION, SelfId, System, kind,
};
use crate::{Error, MAX_PACK, MESSAGE_SIZE, RawMessage, ble, wifi};

// --- rounding that matches C's round()/roundf() (half away from zero) ------

fn round_f64(x: f64) -> f64 {
    // Every |x| >= 2^52 is already an integer.
    if !(x > -4_503_599_627_370_496.0 && x < 4_503_599_627_370_496.0) {
        return x;
    }
    let t = x as i64 as f64;
    let d = x - t;
    if d >= 0.5 {
        t + 1.0
    } else if d <= -0.5 {
        t - 1.0
    } else {
        t
    }
}

fn round_f32(x: f32) -> f32 {
    // Every |x| >= 2^23 is already an integer.
    if !(x > -8_388_608.0 && x < 8_388_608.0) {
        return x;
    }
    let t = x as i32 as f32;
    let d = x - t;
    if d >= 0.5 {
        t + 1.0
    } else if d <= -0.5 {
        t - 1.0
    } else {
        t
    }
}

fn clamp(v: i64, lo: i64, hi: i64) -> i64 {
    v.max(lo).min(hi)
}

fn check(ok: bool) -> Result<(), Error> {
    if ok { Ok(()) } else { Err(Error::Invalid) }
}

const MIN_ALT: f32 = -1000.0;
const MAX_ALT: f32 = 31767.5;

fn header(kind: u8) -> u8 {
    (kind << 4) | PROTOCOL_VERSION
}

/// Altitude to code; `None` is the "unknown" code 0 (-1000 m).
fn altitude(alt: Option<f32>) -> Result<u16, Error> {
    let Some(a) = alt else { return Ok(0) };
    check((MIN_ALT..=MAX_ALT).contains(&a))?;
    // core-c: (int) round((Alt + 1000.0f) / 0.5f), float arithmetic.
    Ok(clamp(round_f64(f64::from((a + 1000.0) / 0.5)) as i64, 0, 0xFFFF) as u16)
}

fn lat_lon(p: Option<LatLon>) -> Result<(i32, i32), Error> {
    let Some(p) = p else { return Ok((0, 0)) };
    check((-90.0..=90.0).contains(&p.lat) && (-180.0..=180.0).contains(&p.lon))?;
    let enc = |v: f64| {
        clamp(
            round_f64(v * 10_000_000.0) as i64,
            -1_800_000_000,
            1_800_000_000,
        ) as i32
    };
    Ok((enc(p.lat), enc(p.lon)))
}

fn put_u16(out: &mut RawMessage, at: usize, v: u16) {
    out[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

fn put_u32(out: &mut RawMessage, at: usize, v: u32) {
    out[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

/// Encode one message (inverse of [`crate::message::decode`]), writing
/// protocol version [`PROTOCOL_VERSION`].
pub fn message(msg: &Message) -> Result<RawMessage, Error> {
    match msg {
        Message::BasicId(m) => basic_id(m),
        Message::Location(m) => location(m),
        Message::Auth(m) => auth(m),
        Message::SelfId(m) => Ok(self_id(m)),
        Message::System(m) => system(m),
        Message::OperatorId(m) => Ok(operator_id(m)),
    }
}

fn basic_id(m: &BasicId) -> Result<RawMessage, Error> {
    check(m.id_type.code() <= 0x0F)?;
    let mut out = [0u8; MESSAGE_SIZE];
    out[0] = header(kind::BASIC_ID);
    out[1] = (m.id_type.code() << 4) | (m.ua_type.code() & 0x0F);
    out[2..22].copy_from_slice(m.uas_id.raw());
    Ok(out)
}

fn location(m: &Location) -> Result<RawMessage, Error> {
    let mut out = [0u8; MESSAGE_SIZE];
    out[0] = header(kind::LOCATION);

    // Direction: None is the "unknown" code 361.
    let direction = m.direction.unwrap_or(361.0);
    check((0.0..=360.0).contains(&direction) || direction == 361.0)?;
    let mut dir = round_f32(direction) as u32;
    if dir == 360 {
        dir = 0;
    }
    let east_west = dir >= 180;
    if east_west {
        dir -= 180;
    }

    // Horizontal speed: None is the "unknown" code 255 m/s.
    let speed = m.speed_horizontal.unwrap_or(255.0);
    check((0.0..=254.25).contains(&speed) || speed == 255.0)?;
    let (speed_code, speed_mult) = if speed <= 63.75 {
        (round_f64(f64::from(speed / 0.25)) as u8, false)
    } else {
        (
            clamp(round_f64(f64::from((speed - 63.75) / 0.75)) as i64, 0, 255) as u8,
            true,
        )
    };

    let vspeed = m.speed_vertical.unwrap_or(63.0);
    check((-62.0..=62.0).contains(&vspeed) || vspeed == 63.0)?;
    let vspeed_code = clamp(round_f64(f64::from(vspeed / 0.5)) as i64, -128, 127) as i8;

    let timestamp = match m.timestamp {
        None => 0xFFFF,
        Some(t) => {
            check((0.0..=3600.0).contains(&t))?;
            clamp(round_f32(t * 10.0) as i64, 0, 36_000) as u16
        }
    };
    let ts_accuracy = match m.timestamp_accuracy {
        None => 0,
        Some(a) => {
            let code = round_f32(a * 10.0);
            check((1.0..=15.0).contains(&code))?;
            code as u8
        }
    };
    let nibble = |c: u8| -> Result<u8, Error> {
        check(c <= 0x0F)?;
        Ok(c)
    };
    let status = nibble(m.status.code())?;
    let (lat, lon) = lat_lon(m.position)?;

    out[1] = (status << 4)
        | if m.height_type == HeightType::AboveGround {
            0x04
        } else {
            0
        }
        | if east_west { 0x02 } else { 0 }
        | if speed_mult { 0x01 } else { 0 };
    out[2] = clamp(i64::from(dir), 0, 255) as u8;
    out[3] = speed_code;
    out[4] = vspeed_code as u8;
    out[5..9].copy_from_slice(&lat.to_le_bytes());
    out[9..13].copy_from_slice(&lon.to_le_bytes());
    put_u16(&mut out, 13, altitude(m.altitude_pressure)?);
    put_u16(&mut out, 15, altitude(m.altitude_geodetic)?);
    put_u16(&mut out, 17, altitude(m.height)?);
    out[19] = (nibble(m.vertical_accuracy.code())? << 4) | nibble(m.horizontal_accuracy.code())?;
    out[20] = (nibble(m.baro_accuracy.code())? << 4) | nibble(m.speed_accuracy.code())?;
    put_u16(&mut out, 21, timestamp);
    out[23] = ts_accuracy;
    Ok(out)
}

fn auth(m: &Auth) -> Result<RawMessage, Error> {
    let mut out = [0u8; MESSAGE_SIZE];
    check(m.page < 16 && m.auth_type.code() <= 0x0F && (m.page == 0) == m.page_zero.is_some())?;
    out[0] = header(kind::AUTH);
    out[1] = (m.auth_type.code() << 4) | m.page;
    let data = m.data.as_bytes();
    if let Some(p0) = m.page_zero {
        let capacity = AUTH_PAGE_ZERO_DATA + usize::from(p0.last_page_index) * AUTH_PAGE_DATA;
        check(
            p0.last_page_index < 16
                && usize::from(p0.length) <= capacity
                && data.len() <= AUTH_PAGE_ZERO_DATA,
        )?;
        out[2] = p0.last_page_index;
        out[3] = p0.length;
        put_u32(&mut out, 4, p0.timestamp);
        out[8..8 + data.len()].copy_from_slice(data);
    } else {
        out[2..2 + data.len()].copy_from_slice(data);
    }
    Ok(out)
}

fn self_id(m: &SelfId) -> RawMessage {
    let mut out = [0u8; MESSAGE_SIZE];
    out[0] = header(kind::SELF_ID);
    out[1] = m.desc_type.code();
    out[2..25].copy_from_slice(m.description.raw());
    out
}

fn system(m: &System) -> Result<RawMessage, Error> {
    let mut out = [0u8; MESSAGE_SIZE];
    let olt = m.operator_location_type.code();
    let class_type = m.classification_type.code();
    check(olt <= 3 && class_type <= 7 && m.category.code() <= 0x0F && m.class.code() <= 0x0F)?;
    check(m.area_radius <= 2550)?;
    let (lat, lon) = lat_lon(m.operator_position)?;
    out[0] = header(kind::SYSTEM);
    out[1] = (class_type << 2) | olt;
    out[2..6].copy_from_slice(&lat.to_le_bytes());
    out[6..10].copy_from_slice(&lon.to_le_bytes());
    put_u16(&mut out, 10, m.area_count);
    out[12] = (m.area_radius / 10) as u8;
    put_u16(&mut out, 13, altitude(m.area_ceiling)?);
    put_u16(&mut out, 15, altitude(m.area_floor)?);
    out[17] = (m.category.code() << 4) | m.class.code();
    put_u16(&mut out, 18, altitude(m.operator_altitude)?);
    put_u32(&mut out, 20, m.timestamp.unwrap_or(0));
    Ok(out)
}

fn operator_id(m: &OperatorId) -> RawMessage {
    let mut out = [0u8; MESSAGE_SIZE];
    out[0] = header(kind::OPERATOR_ID);
    out[1] = m.operator_id_type.code();
    out[2..22].copy_from_slice(m.operator_id.raw());
    out
}

// --- packs and frames -------------------------------------------------------

/// A bounds-checked writer over a caller's buffer.
struct Out<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl<'a> Out<'a> {
    fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, len: 0 }
    }

    fn put(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let end = self
            .len
            .checked_add(bytes.len())
            .ok_or(Error::BufferTooSmall)?;
        self.buf
            .get_mut(self.len..end)
            .ok_or(Error::BufferTooSmall)?
            .copy_from_slice(bytes);
        self.len = end;
        Ok(())
    }

    fn byte(&mut self, b: u8) -> Result<(), Error> {
        self.put(&[b])
    }
}

/// Build a Message Pack (type 0xF) from 1-9 messages. Returns its length,
/// `3 + 25 * messages.len()`. Fails with [`Error::BadPack`] for 0 or more than
/// 9 messages, or if one of them is itself a pack.
pub fn pack(messages: &[RawMessage], out: &mut [u8]) -> Result<usize, Error> {
    if messages.is_empty()
        || messages.len() > MAX_PACK
        || messages.iter().any(|m| m[0] >> 4 == kind::MESSAGE_PACK)
    {
        return Err(Error::BadPack);
    }
    let mut w = Out::new(out);
    w.put(&[
        header(kind::MESSAGE_PACK),
        MESSAGE_SIZE as u8,
        messages.len() as u8,
    ])?;
    for m in messages {
        w.put(m)?;
    }
    Ok(w.len)
}

/// The complete beacon vendor-specific element: `221, len, FA:0B:BC, 0x0D,
/// counter, pack`. `pack` is normally a Message Pack (core-c and ASTM always
/// send one in beacons). Returns its length.
pub fn beacon_ie(counter: u8, pack: &[u8], out: &mut [u8]) -> Result<usize, Error> {
    let len = u8::try_from(5 + pack.len()).map_err(|_| Error::Invalid)?;
    let mut w = Out::new(out);
    w.put(&[221, len])?;
    w.put(&wifi::BEACON_OUI)?;
    w.put(&[wifi::BEACON_OUI_TYPE, counter])?;
    w.put(pack)?;
    Ok(w.len)
}

/// Beacon capability field core-c uses: short slot time | short preamble.
pub const BEACON_CAPABILITY: u16 = 0x0420;
/// Beacon interval written by [`beacon_frame`], in TU (1.024 ms).
pub const BEACON_INTERVAL_TU: u16 = 100;

fn mgmt_header(
    w: &mut Out<'_>,
    subtype: u8,
    da: &[u8; 6],
    sa: &[u8; 6],
    bssid: &[u8; 6],
) -> Result<(), Error> {
    w.put(&[subtype << 4, 0x00, 0x00, 0x00])?; // frame control, duration
    w.put(da)?;
    w.put(sa)?;
    w.put(bssid)?;
    w.put(&[0x00, 0x00]) // sequence control
}

/// A whole 802.11 beacon (no FCS): header (broadcast, SA = BSSID = `src`),
/// timestamp 0, interval 100 TU, capability [`BEACON_CAPABILITY`], SSID
/// element (0-32 bytes), supported rates (6 Mbps), DS parameter set
/// (`channel`), then the Remote ID element. Returns its length.
///
/// Matches core-c's `odid_wifi_build_message_pack_beacon_frame` byte for byte,
/// except that core-c fills in a live timestamp and omits the DS element.
pub fn beacon_frame(
    src: [u8; 6],
    ssid: &[u8],
    channel: u8,
    counter: u8,
    pack: &[u8],
    out: &mut [u8],
) -> Result<usize, Error> {
    let ssid_len = u8::try_from(ssid.len())
        .ok()
        .filter(|&l| l <= 32)
        .ok_or(Error::Invalid)?;
    let mut w = Out::new(out);
    mgmt_header(&mut w, 0x8, &[0xFF; 6], &src, &src)?;
    w.put(&[0; 8])?; // timestamp: the radio overwrites it on transmit
    w.put(&BEACON_INTERVAL_TU.to_le_bytes())?;
    w.put(&BEACON_CAPABILITY.to_le_bytes())?;
    w.put(&[0, ssid_len])?;
    w.put(ssid)?;
    w.put(&[1, 1, 0x8C])?; // supported rates: 6 Mbps (basic)
    w.put(&[3, 1, channel])?; // DS parameter set
    let used = w.len;
    let n = beacon_ie(
        counter,
        pack,
        w.buf.get_mut(used..).ok_or(Error::BufferTooSmall)?,
    )?;
    Ok(used + n)
}

/// A NAN Service Discovery action frame carrying `counter` and `pack`, to
/// `51:6F:9A:01:00:00` with BSSID set to the fixed Remote ID cluster ID.
/// Byte-identical to core-c's `odid_wifi_build_message_pack_nan_action_frame`.
/// Returns its length.
pub fn nan_action_frame(
    src: [u8; 6],
    counter: u8,
    pack: &[u8],
    out: &mut [u8],
) -> Result<usize, Error> {
    let service_info_len = u8::try_from(1 + pack.len()).map_err(|_| Error::Invalid)?;
    let mut w = Out::new(out);
    mgmt_header(&mut w, 0xD, &wifi::NAN_DEST, &src, &wifi::NAN_CLUSTER_ID)?;
    // Public action, vendor specific, WFA OUI, NAN.
    w.put(&[0x04, 0x09])?;
    w.put(&wifi::WFA_OUI)?;
    w.byte(wifi::NAN_OUI_TYPE)?;
    // Service Descriptor attribute.
    w.byte(0x03)?;
    w.put(&(10 + u16::from(service_info_len)).to_le_bytes())?;
    w.put(&wifi::NAN_SERVICE_ID)?;
    w.put(&[0x01, 0x00, 0x10, service_info_len])?; // instance, requestor, control (service info present), length
    w.byte(counter)?;
    w.put(pack)?;
    // Service Descriptor Extension attribute.
    w.put(&[0x0E, 0x04, 0x00, 0x01])?; // id, length 4, instance
    w.put(&0x0200u16.to_le_bytes())?; // control
    w.byte(counter)?; // service update indicator
    Ok(w.len)
}

/// The NAN synchronisation beacon core-c sends before each action frame
/// (`odid_wifi_build_nan_sync_beacon_frame`), with a zero timestamp. Some
/// receivers need it to join the cluster. Returns its length.
pub fn nan_sync_beacon(src: [u8; 6], out: &mut [u8]) -> Result<usize, Error> {
    let mut w = Out::new(out);
    mgmt_header(&mut w, 0x8, &[0xFF; 6], &src, &wifi::NAN_CLUSTER_ID)?;
    w.put(&[0; 8])?;
    w.put(&0x0200u16.to_le_bytes())?; // interval 512 TU
    w.put(&BEACON_CAPABILITY.to_le_bytes())?;
    w.put(&[221, 0x22])?;
    w.put(&wifi::WFA_OUI)?;
    w.byte(wifi::NAN_OUI_TYPE)?;
    w.put(&[0x00, 0x02, 0x00, 0xFE, 0xEA])?; // master indication: preference, random factor
    w.put(&[0x01, 0x0D, 0x00])?; // cluster attribute
    w.put(&src)?;
    w.put(&[0xEA, 0xFE, 0x00, 0, 0, 0, 0])?;
    w.put(&[0x02, 0x06, 0x00])?; // service ID list
    w.put(&wifi::NAN_SERVICE_ID)?;
    Ok(w.len)
}

/// The full BLE AD structure for Remote ID: `len, 0x16, FA FF, 0x0D, counter,
/// payload`. `payload` is one 25-byte message for BLE 4 legacy advertising
/// (the structure is then exactly 31 bytes) or a Message Pack for BLE 5
/// extended advertising. Returns its length.
pub fn ble_service_data(counter: u8, payload: &[u8], out: &mut [u8]) -> Result<usize, Error> {
    let len = u8::try_from(5 + payload.len()).map_err(|_| Error::Invalid)?;
    let mut w = Out::new(out);
    w.put(&[len, ble::AD_SERVICE_DATA_16])?;
    w.put(&ble::UUID)?;
    w.put(&[ble::APP_CODE, counter])?;
    w.put(payload)?;
    Ok(w.len)
}
