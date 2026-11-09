//! DJI legacy Wi-Fi DroneID, from hand-built frames.

mod common;

use common::Rng;
use rid::dji::{self, FlightInfo, Record};

const SRC: [u8; 6] = [0x60, 0x60, 0x1F, 0x12, 0x34, 0x56];

fn beacon(vendor_ie_body: &[u8]) -> Vec<u8> {
    let mut f = vec![0x80, 0x00, 0, 0];
    f.extend_from_slice(&[0xFF; 6]);
    f.extend_from_slice(&SRC);
    f.extend_from_slice(&SRC);
    f.extend_from_slice(&[0, 0]);
    f.extend_from_slice(&[0; 8]);
    f.extend_from_slice(&[0x64, 0x00, 0x31, 0x04]);
    f.extend_from_slice(&[0, 9]);
    f.extend_from_slice(b"Spark-RC1");
    f.extend_from_slice(&[1, 1, 0x82]);
    f.push(221);
    f.push(vendor_ie_body.len() as u8);
    f.extend_from_slice(vendor_ie_body);
    f
}

fn header(sub: u8) -> Vec<u8> {
    let mut b = dji::OUI.to_vec();
    b.extend_from_slice(&[0x58, 0x62, 0x13, sub]);
    b
}

fn rad_e7(deg: f64) -> i32 {
    (deg.to_radians() * 1e7).round() as i32
}

fn flight_info_body() -> Vec<u8> {
    let mut b = header(dji::FLIGHT_INFO);
    b.push(2); // version
    b.extend_from_slice(&0x1234u16.to_le_bytes());
    b.extend_from_slice(&0x0F77u16.to_le_bytes());
    b.extend_from_slice(b"1581F4XFB2210ABC");
    b.extend_from_slice(&rad_e7(-75.1652).to_le_bytes());
    b.extend_from_slice(&rad_e7(39.9526).to_le_bytes());
    for v in [120i16, 50, -250, 300, 75, 150, -200, 9000] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b.extend_from_slice(&rad_e7(-75.1600).to_le_bytes());
    b.extend_from_slice(&rad_e7(39.9500).to_le_bytes());
    b.push(16); // product type
    b.push(4); // uuid length
    b.extend_from_slice(b"abcd");
    b.extend_from_slice(&[0; 16]);
    b
}

#[test]
fn flight_info_decodes() {
    let frame = beacon(&flight_info_body());
    let got = dji::extract(&frame).expect("DJI element not found");
    assert_eq!(got.src, SRC);
    let Record::FlightInfo(fi) = got.record.clone() else {
        panic!("{:?}", got.record)
    };
    assert_eq!((fi.version, fi.seq, fi.state), (2, 0x1234, 0x0F77));
    assert_eq!(fi.serial.as_str(), Some("1581F4XFB2210ABC"));
    assert!(fi.motor_on() && fi.in_air() && fi.gps_valid() && !fi.privacy_enabled());
    assert!(fi.state & FlightInfo::STATE_PITCH_ROLL_VALID != 0);
    // Wire resolution is 1e-7 radian, about 5.7e-6 degree.
    let p = fi.position.unwrap();
    assert!(
        (p.lat - 39.9526).abs() < 1e-5 && (p.lon + 75.1652).abs() < 1e-5,
        "{p:?}"
    );
    let ext = fi.ext.expect("extended fields");
    assert_eq!(
        (ext.altitude, ext.height, ext.v_north, ext.v_east, ext.v_up),
        (120, 50, -250, 300, 75)
    );
    assert_eq!((ext.pitch, ext.roll, ext.yaw), (1.5, -2.0, 90.0));
    let h = ext.home.unwrap();
    assert!((h.lat - 39.95).abs() < 1e-5 && (h.lon + 75.16).abs() < 1e-5);
    assert_eq!(ext.product_type, 16);
    assert_eq!(ext.uuid.as_bytes(), b"abcd");

    // DJI frames are not ASTM Remote ID, and vice versa.
    assert_eq!(rid::wifi::extract(&frame), None);

    let json = serde_json::to_string(&got).unwrap();
    assert!(json.contains(r#""serial":"1581F4XFB2210ABC""#), "{json}");
}

#[test]
fn short_records() {
    let body = flight_info_body();
    // Only the 29 bytes Kismet relies on: no extension.
    let frame = beacon(&body[..7 + 29]);
    let Record::FlightInfo(fi) = dji::extract(&frame).unwrap().record else {
        panic!()
    };
    assert!(fi.position.is_some() && fi.ext.is_none());
    // One byte short of that: not decodable, reported as unknown.
    let frame = beacon(&body[..7 + 28]);
    assert_eq!(
        dji::extract(&frame).unwrap().record,
        Record::Unknown { subcommand: 0x10 }
    );
    // Privacy mode: zero coordinates.
    let mut zeroed = body.clone();
    zeroed[7 + 21..7 + 29].fill(0);
    let Record::FlightInfo(fi) = dji::extract(&beacon(&zeroed)).unwrap().record else {
        panic!()
    };
    assert!(fi.position.is_none());
    // Header only.
    assert_eq!(dji::extract(&beacon(&body[..6])), None);
    assert_eq!(
        dji::extract(&beacon(&body[..7])).unwrap().record,
        Record::Unknown { subcommand: 0x10 }
    );
}

#[test]
fn flight_purpose_decodes() {
    let mut b = header(dji::FLIGHT_PURPOSE);
    b.extend_from_slice(b"1581F4XFB2210ABC");
    b.push(6);
    b.extend_from_slice(b"MYDRON\0\0\0\0");
    b.push(11);
    b.extend_from_slice(b"survey work");
    let frame = beacon(&b);
    let Record::FlightPurpose(p) = dji::extract(&frame).unwrap().record else {
        panic!()
    };
    assert_eq!(p.serial.as_str(), Some("1581F4XFB2210ABC"));
    assert_eq!(p.drone_id.as_str(), Some("MYDRON"));
    assert_eq!(p.purpose.as_str(), Some("survey work"));

    // DJI sometimes over-states the purpose length: take what is there.
    let n = b.len();
    b[n - 12] = 200;
    let frame = beacon(&b);
    let Record::FlightPurpose(p) = dji::extract(&frame).unwrap().record else {
        panic!()
    };
    assert_eq!(p.purpose.as_str(), Some("survey work"));
}

#[test]
fn other_vendor_elements_are_ignored() {
    let mut b = flight_info_body();
    b[0] = 0x00;
    assert_eq!(dji::extract(&beacon(&b)), None);
    // Unknown subcommand.
    let mut b = flight_info_body();
    b[6] = 0x42;
    assert_eq!(
        dji::extract(&beacon(&b)).unwrap().record,
        Record::Unknown { subcommand: 0x42 }
    );
}

#[test]
fn mutated_dji_frames_never_panic() {
    let mut r = Rng::new(400);
    let base = beacon(&flight_info_body());
    for _ in 0..500_000 {
        let mut f = base.clone();
        for _ in 0..1 + r.below(4) {
            if f.is_empty() {
                break;
            }
            let at = r.below(f.len());
            match r.below(4) {
                0 => f[at] = r.u8(),
                1 => f.truncate(at),
                2 => {
                    f.remove(at);
                }
                _ => f.insert(at, r.u8()),
            }
        }
        let _ = dji::extract(&f);
        if f.len() > 38 {
            let _ = dji::parse_element(&f[38..]);
        }
    }
}
