//! What goes in the advert, independent of BlueZ.
//!
//! ## Layout: one legacy advert, exactly 31 bytes, no Flags
//!
//! ```text
//! byte  0      1     2  3    4     5        6 .. 30
//!       len=30 0x16  FA FF   0x0D  counter  25-byte Remote ID message
//!       |      |     UUID    app   |
//!       |      Service Data, 16-bit UUID
//! ```
//!
//! That is the whole 31-byte legacy advertising payload, so nothing else
//! fits: no Flags (3 bytes), no name, no TX Power Level, no appearance.
//! BlueZ is asked for exactly the [`service_data`] bytes (`0x0D`, counter,
//! message) under UUID `0xFFFA`, with advert type "broadcast" (non-
//! connectable), `Includes` empty and **no `Discoverable` property at
//! all**. Found on BlueZ 5.72: offering `Discoverable` = false is not the
//! same as leaving it out. With the property present, bluetoothd has the
//! kernel reserve 3 bytes for Flags, the 31-byte advert is "too long" and
//! registration fails ("Advertising data too long or couldn't be
//! generated", MGMT status 0xea). An LE-only adapter (BR/EDR disabled) may
//! hit the same limit through the kernel's "BR/EDR not supported" flag.
//! ASTM F3411 BT4 broadcasts carry no Flags either.

use rid::{Error, MESSAGE_SIZE, RawMessage};

use crate::flight::{self, Centre, Fix};
use crate::rid_tx;

/// Bytes BlueZ is given as the `0xFFFA` service data.
pub const SERVICE_DATA_LEN: usize = 2 + MESSAGE_SIZE;
/// The legacy advertising payload limit.
pub const LEGACY_MAX: usize = 31;
// len + type + 16-bit UUID + service data must fill it exactly.
const _: () = assert!(4 + SERVICE_DATA_LEN == LEGACY_MAX);

/// The UUID as BlueZ wants it in `ServiceData` (it shortens SIG-base UUIDs
/// to 16 bits on air).
pub const RID_UUID: &str = "0000fffa-0000-1000-8000-00805f9b34fb";

/// App code, counter, message: the AD structure from `rid` minus its
/// length, type and UUID bytes (which BlueZ writes itself).
pub fn service_data(counter: u8, msg: &RawMessage) -> Result<[u8; SERVICE_DATA_LEN], Error> {
    let ad = rid_tx::ble_legacy(counter, msg)?;
    let mut out = [0u8; SERVICE_DATA_LEN];
    out.copy_from_slice(&ad[4..]);
    Ok(out)
}

/// The test centre from `RID_TEST_LAT` / `RID_TEST_LON` (read at run time
/// here; `rid-beacon` reads them at build time), with the same default and
/// range checks.
pub fn centre(lat: Option<&str>, lon: Option<&str>) -> Centre {
    let mut c = Centre {
        lat: flight::DEFAULT_LAT,
        lon: flight::DEFAULT_LON,
        from_env_ok: true,
    };
    match lat.map(|s| s.trim().parse::<f64>()) {
        Some(Ok(v)) if (-89.0..=89.0).contains(&v) => c.lat = v,
        Some(_) => c.from_env_ok = false,
        None => {}
    }
    match lon.map(|s| s.trim().parse::<f64>()) {
        Some(Ok(v)) if (-180.0..=180.0).contains(&v) => c.lon = v,
        Some(_) => c.from_env_ok = false,
        None => {}
    }
    c
}

/// One advert's worth: which message (rotating Basic ID, Location, Self-ID,
/// System, Operator ID, as a BT4 broadcaster does), built for wall-clock
/// time `t_s` (Unix seconds, so the Location timestamp is the real seconds
/// after the hour).
pub struct Frame {
    pub kind: &'static str,
    pub data: [u8; SERVICE_DATA_LEN],
    pub fix: Fix,
}

pub fn frame(c: &Centre, t_s: f64, counter: u8, step: usize) -> Result<Frame, Error> {
    let fix = flight::at(c, t_s);
    let msgs = rid_tx::messages(c, &fix, t_s)?;
    let i = step % rid_tx::N_MESSAGES;
    Ok(Frame {
        kind: rid_tx::MESSAGE_NAMES[i],
        data: service_data(counter, &msgs[i])?,
        fix,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rid::message::{Message, decode};

    const T: f64 = 1_762_870_000.25;

    #[test]
    fn whole_advert_is_exactly_the_legacy_limit() {
        let c = centre(None, None);
        let f = frame(&c, T, 7, 0).unwrap();
        // BlueZ adds len (1) + type (1) + 16-bit UUID (2).
        assert_eq!(4 + f.data.len(), LEGACY_MAX);
        let mut ad = vec![30, 0x16, 0xFA, 0xFF];
        ad.extend_from_slice(&f.data);
        let found = rid::ble::extract(&ad).unwrap();
        assert_eq!(found.counter, 7);
        assert_eq!(found.payload.len(), MESSAGE_SIZE);
    }

    #[test]
    fn rotates_through_all_five_with_fake_identity() {
        let c = centre(None, None);
        let mut kinds = Vec::new();
        for step in 0..5 {
            let f = frame(&c, T, step as u8, step).unwrap();
            assert_eq!(f.data[0], 0x0D);
            assert_eq!(f.data[1], step as u8);
            let raw: RawMessage = f.data[2..].try_into().unwrap();
            let m = decode(&raw).unwrap();
            match &m {
                Message::BasicId(b) => assert_eq!(b.uas_id.as_bytes(), b"TESTDRONE0000000001"),
                Message::OperatorId(o) => assert_eq!(o.operator_id.as_bytes(), b"TEST-OPERATOR"),
                Message::SelfId(s) => {
                    assert_eq!(s.description.as_bytes(), b"BENCH TEST-NOT AIRCRAFT")
                }
                Message::Location(l) => {
                    let p = l.position.unwrap();
                    // Within the 50 m test circle around City Hall.
                    assert!((p.lat - flight::DEFAULT_LAT).abs() < 0.001);
                    assert!((p.lon - flight::DEFAULT_LON).abs() < 0.001);
                }
                Message::System(_) => {}
                other => panic!("unexpected {other:?}"),
            }
            kinds.push(f.kind);
        }
        assert_eq!(kinds, rid_tx::MESSAGE_NAMES);
        assert_eq!(frame(&c, T, 0, 5).unwrap().kind, "basic_id");
    }

    #[test]
    fn centre_from_env_strings() {
        let c = centre(Some(" 40.0 "), Some("-75.5"));
        assert_eq!((c.lat, c.lon, c.from_env_ok), (40.0, -75.5, true));
        let c = centre(Some("north"), Some("200"));
        assert_eq!(
            (c.lat, c.lon, c.from_env_ok),
            (flight::DEFAULT_LAT, flight::DEFAULT_LON, false)
        );
        let c = centre(Some("12.5"), None);
        assert_eq!((c.lat, c.lon), (12.5, flight::DEFAULT_LON));
    }

    #[test]
    fn location_uses_the_centre() {
        let c = centre(Some("51.5"), Some("-0.12"));
        let f = frame(&c, T, 0, 1).unwrap();
        let raw: RawMessage = f.data[2..].try_into().unwrap();
        let Message::Location(l) = decode(&raw).unwrap() else {
            panic!()
        };
        assert!((l.position.unwrap().lat - 51.5).abs() < 0.001);
    }
}
