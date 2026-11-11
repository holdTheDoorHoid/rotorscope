//! `--ble ADAPTER`: the pure half of the laptop Bluetooth input.
//!
//! BlueZ does not hand out raw advertising data over D-Bus. It parses each
//! advert and publishes the pieces as `org.bluez.Device1` properties; Remote
//! ID arrives as one `ServiceData` entry, keyed by the 128-bit form of UUID
//! `0xFFFA`, holding everything after the UUID: app code `0x0D`, the message
//! counter, then the payload. This module turns that back into the AD
//! structure the nodes see and runs it through [`rid::ble::extract`], so the
//! laptop and the ESP32 listener share one parser.
//!
//! The D-Bus plumbing is in `bluez.rs` (feature `ble`, Linux only).

use rid::Transport;
use rid::message::{decode, raw_messages};

use crate::parse::RidLine;

/// `0xFFFA` on the Bluetooth base UUID, as BlueZ writes it.
pub const RID_UUID: &str = "0000fffa-0000-1000-8000-00805f9b34fb";

/// True if `uuid` (any case) is the Remote ID service UUID.
pub fn is_rid_uuid(uuid: &str) -> bool {
    uuid.eq_ignore_ascii_case(RID_UUID) || uuid.eq_ignore_ascii_case("fffa")
}

/// A BlueZ device object path (`/org/bluez/hci1/dev_AA_BB_CC_DD_EE_FF`, or a
/// child of one) to its address, most significant byte first as printed.
pub fn mac_from_path(path: &str) -> Option<[u8; 6]> {
    let dev = path.split('/').find_map(|p| p.strip_prefix("dev_"))?;
    let bytes = dev
        .split('_')
        .map(|p| (p.len() == 2).then(|| u8::from_str_radix(p, 16).ok())?)
        .collect::<Option<Vec<u8>>>()?;
    bytes.try_into().ok()
}

/// One Remote ID advert as BlueZ reported it: the `ServiceData` value for
/// [`RID_UUID`], the sender's address and the last RSSI BlueZ reported for it
/// (BlueZ only signals RSSI when it changes).
pub fn rid_line(mac: [u8; 6], rssi: i16, service_data: &[u8]) -> Option<RidLine> {
    // Rebuild the AD structure (len, 0x16, FA FF, data) so the node parser
    // decides what counts as Remote ID.
    let len = u8::try_from(3 + service_data.len()).ok()?;
    let mut ad = Vec::with_capacity(usize::from(len) + 1);
    ad.extend_from_slice(&[len, rid::ble::AD_SERVICE_DATA_16]);
    ad.extend_from_slice(&rid::ble::UUID);
    ad.extend_from_slice(service_data);
    let found = rid::ble::extract(&ad)?;
    let mut out = RidLine {
        // A BT4 controller only receives legacy adverts. An extended advert
        // relayed by a BT5 adapter would carry a pack; the tracker treats
        // both alike apart from this label.
        transport: if found.payload.len() > rid::MESSAGE_SIZE {
            Transport::BleExtended
        } else {
            Transport::BleLegacy
        },
        channel: 0,
        rssi: rssi.clamp(i16::from(i8::MIN), 0) as i8,
        mac,
        counter: found.counter,
        up_us: None,
        msgs: Vec::new(),
        errors: 0,
    };
    match raw_messages(found.payload) {
        Ok(raws) => {
            for r in raws {
                match decode(r) {
                    Ok(m) => out.msgs.push(m),
                    Err(_) => out.errors += 1,
                }
            }
        }
        Err(_) => out.errors += 1,
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rid::message::*;

    fn basic() -> rid::RawMessage {
        rid::encode::message(&Message::BasicId(BasicId {
            id_type: IdType::SerialNumber,
            ua_type: UaType::HelicopterOrMultirotor,
            uas_id: FixedStr::new(b"TESTDRONE0000000001").unwrap(),
        }))
        .unwrap()
    }

    /// What BlueZ puts in ServiceData: the AD structure minus len, type, UUID.
    fn service_data(counter: u8, payload: &[u8]) -> Vec<u8> {
        let mut ad = [0u8; 256];
        let n = rid::encode::ble_service_data(counter, payload, &mut ad).unwrap();
        ad[4..n].to_vec()
    }

    #[test]
    fn path_to_mac() {
        assert_eq!(
            mac_from_path("/org/bluez/hci1/dev_C2_52_49_44_00_53"),
            Some([0xC2, 0x52, 0x49, 0x44, 0x00, 0x53])
        );
        assert_eq!(
            mac_from_path("/org/bluez/hci1/dev_C2_52_49_44_00_53/service0010"),
            Some([0xC2, 0x52, 0x49, 0x44, 0x00, 0x53])
        );
        assert_eq!(mac_from_path("/org/bluez/hci1"), None);
        assert_eq!(mac_from_path("/org/bluez/hci1/dev_C2_52_49"), None);
        assert_eq!(mac_from_path("/org/bluez/hci1/dev_C2_52_49_44_00_5"), None);
    }

    #[test]
    fn uuid_forms() {
        assert!(is_rid_uuid("0000FFFA-0000-1000-8000-00805F9B34FB"));
        assert!(is_rid_uuid(RID_UUID));
        assert!(!is_rid_uuid("0000fff9-0000-1000-8000-00805f9b34fb"));
    }

    #[test]
    fn legacy_advert_decodes() {
        let sd = service_data(0x41, &basic());
        assert_eq!(sd.len(), 27, "app code + counter + one message");
        let l = rid_line([1, 2, 3, 4, 5, 6], -71, &sd).unwrap();
        assert_eq!(l.transport, Transport::BleLegacy);
        assert_eq!((l.counter, l.rssi, l.mac), (0x41, -71, [1, 2, 3, 4, 5, 6]));
        assert_eq!(l.errors, 0);
        match &l.msgs[..] {
            [Message::BasicId(b)] => assert_eq!(b.uas_id.as_bytes(), b"TESTDRONE0000000001"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn pack_is_extended() {
        let raws = [basic(), basic()];
        let mut pack = [0u8; 64];
        let n = rid::encode::pack(&raws, &mut pack).unwrap();
        let l = rid_line([0; 6], -50, &service_data(9, &pack[..n])).unwrap();
        assert_eq!(l.transport, Transport::BleExtended);
        assert_eq!(l.msgs.len(), 2);
    }

    #[test]
    fn wrong_app_code_or_short_is_ignored() {
        let mut sd = service_data(1, &basic());
        sd[0] = 0x0C;
        assert!(rid_line([0; 6], -50, &sd).is_none());
        assert!(rid_line([0; 6], -50, &[0x0D]).is_none());
    }

    #[test]
    fn garbage_payload_counts_an_error() {
        let l = rid_line([0; 6], -50, &[0x0D, 7, 0x02]).unwrap();
        assert!(l.msgs.is_empty());
        assert_eq!(l.errors, 1);
    }

    #[test]
    fn rssi_is_clamped() {
        let sd = service_data(1, &basic());
        assert_eq!(rid_line([0; 6], -300, &sd).unwrap().rssi, -128);
        assert_eq!(rid_line([0; 6], 20, &sd).unwrap().rssi, 0);
    }
}
