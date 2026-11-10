//! Decoding the two public DJI recordings from RUB-SysSec/DroneSecurity
//! (50 MS/s complex64). Fetch them with `tools/fetch-droneid-samples.sh`;
//! these tests skip, with a message, when the files are absent.
//!
//! Privacy: the recordings carry a real serial number and UUID. These tests
//! check their format and CRC only and never print or hardcode them.

use droneid::host::{Format, decode_recording, resample_f32, to_c16, to_f32_pairs};
use droneid::math::C16;
use droneid::payload::{FlightInfo, Record};
use droneid::rx::{Burst, Path, Receiver};

fn load(name: &str) -> Option<Vec<u8>> {
    let dir = std::env::var("DRONEID_TESTDATA")
        .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/droneid").to_string());
    let path = std::path::Path::new(&dir).join(name);
    match std::fs::read(&path) {
        Ok(b) => Some(b),
        Err(_) => {
            eprintln!("SKIPPED: {} not found; run tools/fetch-droneid-samples.sh", path.display());
            None
        }
    }
}

fn decode(x: &[C16], fs: u32, try_systematic: bool) -> Vec<Burst> {
    let mut rx = Box::new(Receiver::new(fs).unwrap());
    rx.config.try_systematic = try_systematic;
    let mut v = vec![];
    decode_recording(&mut rx, x, |f| {
        if let Ok(b) = f.result {
            v.push(b);
        }
    });
    v
}

fn flights(b: &[Burst]) -> Vec<FlightInfo> {
    b.iter()
        .filter(|b| b.crc24_ok)
        .filter_map(|b| match &b.parsed.as_ref()?.record {
            Record::FlightInfo(f) => Some(f.clone()),
            _ => None,
        })
        .collect()
}

/// Serial: 14 printable characters (`strict`: upper-case letters and
/// digits, as on the Mavic; the Mini 2 in the recording sends a mixed-case
/// placeholder instead). UUID: printable, as long as its length byte says.
fn check_identity_format(f: &FlightInfo, strict: bool) {
    let s = f.serial_str().expect("serial is printable ASCII");
    assert_eq!(s.len(), 14, "serial length");
    if strict {
        assert!(s.bytes().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()), "serial charset");
    }
    let u = f.uuid_str().expect("uuid is printable ASCII");
    assert_eq!(u.len(), f.uuid_len as usize);
}

fn near(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

#[test]
fn mavic_air_2() {
    let Some(raw) = load("mavic_air_2") else { return };
    let x = to_c16(&raw, Format::Cf32);
    let bursts = decode(&x, 50_000_000, true);
    assert_eq!(bursts.len(), 3, "bursts demodulated");
    assert!(bursts.iter().all(|b| b.crc24_ok && b.path == Some(Path::Systematic)));
    assert!(bursts.iter().all(|b| b.parsed.as_ref().unwrap().crc16_ok == Some(true)));
    let f = flights(&bursts);
    assert_eq!(f.len(), 2, "flight-info records");
    for f in &f {
        check_identity_format(f, true);
        assert_eq!(f.version, 2);
        assert_eq!(f.product_name(), Some("Mavic Air 2"));
        assert_eq!(f.uuid_len, 19);
        // The paper's screenshot (a different packet of the same flight):
        // drone 51.4463 N 7.2672 E, altitude 40.8 m, height 3.66 m,
        // home 51.4464 N 7.2672 E, app 51.4463 N 7.2671 E.
        assert!(near(f.lat(), 51.4463, 3e-4) && near(f.lon(), 7.2672, 3e-4), "drone position");
        assert!(near(f.home_lat(), 51.4464, 3e-4) && near(f.home_lon(), 7.2672, 3e-4), "home position");
        assert!(near(f.app_lat(), 51.4463, 3e-4) && near(f.app_lon(), 7.2671, 3e-4), "app position");
        assert!(near(f.altitude_m(), 40.8, 5.0), "altitude {}", f.altitude_m());
        // Height differs from the screenshot's 3.66 m: the aircraft is
        // climbing in these packets (v_up > 0); only check it is plausible.
        assert!((0.0..30.0).contains(&f.height_m()), "height {}", f.height_m());
        assert!(f.v_up > 0);
    }
    // Self-consistency across packets: same aircraft, same home, rising.
    assert_eq!(f[0].serial, f[1].serial);
    assert_eq!((f[0].home_lat_raw, f[0].home_lon_raw), (f[1].home_lat_raw, f[1].home_lon_raw));
    assert!(f[1].seq > f[0].seq && f[1].height_raw >= f[0].height_raw);
    // The third record is the 0x11 type, carrying the same serial.
    let other: Vec<_> = bursts
        .iter()
        .filter_map(|b| match &b.parsed.as_ref()?.record {
            Record::Info11 { serial, len } => Some((*serial, *len)),
            _ => None,
        })
        .collect();
    assert_eq!(other.len(), 1);
    assert_eq!(other[0], (f[0].serial, 141));
}

#[test]
fn mini_2() {
    let Some(raw) = load("mini2_sm") else { return };
    let x = to_c16(&raw, Format::Cf32);
    let bursts = decode(&x, 50_000_000, true);
    assert_eq!(bursts.len(), 10, "bursts demodulated");
    assert!(bursts.iter().all(|b| b.crc24_ok), "every burst CRC-valid");
    let f = flights(&bursts);
    assert_eq!(f.len(), 10);
    for g in &f {
        check_identity_format(g, false);
        assert_eq!(g.version, 2);
        assert_eq!(g.product_name(), Some("Mini 2"));
        // No GPS fix yet: drone and home are zero, the pilot's phone is set.
        assert_eq!((g.lat_raw, g.lon_raw, g.home_lat_raw, g.home_lon_raw), (0, 0, 0, 0));
        assert!(near(g.app_lat(), 51.4472, 3e-4) && near(g.app_lon(), 7.2665, 3e-4));
        assert_eq!(g.serial, f[0].serial);
    }
    // Sequence numbers never go backwards (repeats are retransmissions).
    assert!(f.windows(2).all(|w| w[1].seq >= w[0].seq));
}

#[test]
fn turbo_path_decodes_the_recordings_too() {
    for (name, n) in [("mavic_air_2", 3), ("mini2_sm", 10)] {
        let Some(raw) = load(name) else { return };
        let x = to_c16(&raw, Format::Cf32);
        let fast = decode(&x, 50_000_000, true);
        let turbo = decode(&x, 50_000_000, false);
        assert_eq!(turbo.len(), n);
        for (a, b) in fast.iter().zip(&turbo) {
            assert_eq!(b.path, Some(Path::Turbo), "{name}");
            assert_eq!(a.block, b.block, "{name}: the two paths disagree");
        }
    }
}

#[test]
fn other_capture_rates() {
    // Resample the 50 MS/s recording to every other supported rate.
    let Some(raw) = load("mavic_air_2") else { return };
    let f = to_f32_pairs(&raw);
    for fs in [40_000_000u32, 30_720_000, 20_000_000, 15_360_000] {
        let y = resample_f32(&f, 50e6, fs as f64, 0.45 * fs as f64);
        let bytes: Vec<u8> = y.iter().flat_map(|&(a, b)| [a.to_le_bytes(), b.to_le_bytes()]).flatten().collect();
        let x = to_c16(&bytes, Format::Cf32);
        let b = decode(&x, fs, true);
        // The third burst sits 12.5 MHz off centre; below 40 MS/s it is
        // outside the capture, so only the first two remain.
        let want = if fs >= 40_000_000 { 3 } else { 2 };
        assert_eq!(b.iter().filter(|b| b.crc24_ok).count(), want, "{fs}");
    }
}
