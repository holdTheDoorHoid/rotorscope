//! One decoded DroneID burst as a JSON object.
//!
//! Identifiers (serial number, app UUID, O4 key ID) are redacted by default
//! the way `tools/droneid-file --redact` does it: the first three characters
//! stay, the rest become `*`, the length is kept. `reveal on` on the console
//! prints them in full. Positions are printed: DroneID is a broadcast meant
//! for third-party receivers (PLAN.md Phase 10 decisions).

use core::fmt::Write;

use droneid::payload::Record;
use droneid::rx::{Burst, Path};

/// Printable ASCII up to the first NUL, or `None`.
fn text(b: &[u8]) -> Option<&str> {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    let t = &b[..end];
    if t.iter().all(|c| (0x20..0x7f).contains(c) && *c != b'"' && *c != b'\\') { core::str::from_utf8(t).ok() } else { None }
}

/// Write `s` as a JSON string, redacted unless `reveal`.
fn ident(w: &mut impl Write, s: Option<&str>, reveal: bool) {
    let Some(s) = s else {
        let _ = w.write_str("null");
        return;
    };
    let _ = w.write_char('"');
    for (i, c) in s.chars().enumerate() {
        let c = if c == '"' || c == '\\' { '?' } else { c };
        let _ = w.write_char(if reveal || i < 3 { c } else { '*' });
    }
    let _ = w.write_char('"');
}

pub fn burst_json(w: &mut impl Write, b: &Burst, reveal: bool) {
    let path = match b.path {
        Some(Path::Systematic) => "systematic",
        Some(Path::Turbo) => "turbo",
        // The turbo decoder was not tried (`radio::TURBO_MIN_SNR_DB`).
        None if b.turbo_skipped => "gated",
        None => "none",
    };
    let _ = write!(
        w,
        r#"{{"crc":{},"path":"{path}","layout":"{}","snr_db":{:.1},"cfo_khz":{:.1},"zc":{}"#,
        b.crc24_ok,
        b.layout.name(),
        b.snr_db,
        b.cfo_hz / 1000.0,
        b.zc_metric
    );
    if !b.crc24_ok {
        let _ = w.write_char('}');
        return;
    }
    let Some(p) = &b.parsed else {
        let _ = w.write_char('}');
        return;
    };
    let crc16 = match p.crc16_ok {
        Some(true) => "true",
        Some(false) => "false",
        None => "null",
    };
    let _ = write!(w, r#","crc16":{crc16}"#);
    match &p.record {
        Record::FlightInfo(f) => {
            let _ = write!(w, r#","type":"flight_info","seq":{},"serial":"#, f.seq);
            ident(w, f.serial_str(), reveal);
            let _ = write!(
                w,
                r#","product":"{}","lat":{:.6},"lon":{:.6},"alt_m":{:.1},"height_m":{:.1},"v_mps":[{},{},{}],"yaw":{:.0},"home":[{:.6},{:.6}],"app":[{:.6},{:.6}],"uuid":"#,
                f.product_name().unwrap_or("?"),
                f.lat(),
                f.lon(),
                f.altitude_m(),
                f.height_m(),
                f.v_north,
                f.v_east,
                f.v_up,
                f.yaw_deg(),
                f.home_lat(),
                f.home_lon(),
                f.app_lat(),
                f.app_lon()
            );
            ident(w, f.uuid_str(), reveal);
        }
        Record::Encrypted(e) => {
            // O4: cleartext header only; the ciphertext is never touched
            // here. The key ID links one session's packets; treat it as an
            // identifier.
            let _ = write!(
                w,
                r#","type":"o4_encrypted","record":"{}","valid":{},"fail":"{}","key_id":"#,
                e.kind.name(),
                e.valid(),
                e.checks.first_failure().unwrap_or("")
            );
            let mut hex = [0u8; 8];
            for (k, b) in e.key_id.iter().enumerate() {
                hex[2 * k] = b"0123456789abcdef"[(b >> 4) as usize];
                hex[2 * k + 1] = b"0123456789abcdef"[(b & 15) as usize];
            }
            ident(w, core::str::from_utf8(&hex).ok(), reveal);
        }
        Record::Info11 { serial, len } => {
            let _ = write!(w, r#","type":"info11","len":{len},"serial":"#);
            ident(w, text(serial), reveal);
        }
        Record::Unknown { selector, len } => {
            let _ = write!(w, r#","type":"unknown","selector":{selector},"len":{len}"#);
        }
    }
    let _ = w.write_char('}');
}
