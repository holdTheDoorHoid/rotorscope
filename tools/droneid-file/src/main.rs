//! Decode DJI DroneID bursts from a raw IQ recording.
//!
//! ```text
//! droneid-file <file> --rate <samples/s> [--format cf32|ci8|ci16]
//!              [--redact] [--no-turbo] [--failures] [--llr-dump <file>]
//! ```
//!
//! Prints one JSON line per burst found: where it is, frequency offset,
//! SNR, which decode path worked (`systematic` or `turbo`), CRC status and
//! the decoded fields. `--redact` masks the serial number and UUID (use it
//! when sharing output: they identify a real aircraft and its owner).
//! `--failures` also prints spans that failed synchronisation.

use std::io::Write;
use std::process::ExitCode;

/// Print a line; exit quietly when the reader has gone (`| head`).
fn emit(v: &Value) {
    if writeln!(std::io::stdout().lock(), "{v}").is_err() {
        std::process::exit(0);
    }
}

use droneid::host::{Format, decode_recording, to_c16};
use droneid::payload::{EncKind, Parsed, Record};
use droneid::rx::{Path, Receiver};
use serde_json::{Value, json};

fn usage() -> ExitCode {
    eprintln!(
        "usage: droneid-file <file> --rate <samples/s> [--format cf32|ci8|ci16] [--redact] [--no-turbo] [--failures]"
    );
    ExitCode::from(2)
}

/// Keep the first three characters, mask the rest, keep the length.
pub fn redact(s: &str) -> String {
    s.chars().enumerate().map(|(i, c)| if i < 3 { c } else { '*' }).collect()
}

fn text(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    let t = &b[..end];
    if t.iter().all(|c| (0x20..0x7f).contains(c)) {
        String::from_utf8_lossy(t).into_owned()
    } else {
        t.iter().map(|c| format!("{c:02x}")).collect()
    }
}

fn record_json(p: &Parsed, redacted: bool) -> Value {
    let hide = |s: String| if redacted { redact(&s) } else { s };
    let crc16 = p.crc16_ok;
    match &p.record {
        Record::FlightInfo(f) => json!({
            "type": "flight_info",
            "crc16_ok": crc16,
            "version": f.version,
            "seq": f.seq,
            "state": format!("0x{:04x}", f.state),
            "serial": hide(text(&f.serial)),
            "lat": f.lat(), "lon": f.lon(),
            "altitude_m": f.altitude_m(), "height_m": f.height_m(),
            "altitude_raw": f.altitude_raw, "height_raw": f.height_raw,
            "v_north": f.v_north, "v_east": f.v_east, "v_up": f.v_up,
            "yaw_deg": f.yaw_deg(),
            "gps_time_ms": f.gps_time_ms,
            "app_lat": f.app_lat(), "app_lon": f.app_lon(),
            "home_lat": f.home_lat(), "home_lon": f.home_lon(),
            "product_type": f.product_type,
            "product": f.product_name(),
            "uuid_len": f.uuid_len,
            "uuid": hide(text(&f.uuid[..(f.uuid_len as usize).min(20)])),
        }),
        Record::Encrypted(e) => json!({
            "type": "o4_encrypted",
            "crc16_ok": crc16,
            "record": e.kind.name(),
            "key_id": e.key_id.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            "declared_len": e.declared_len,
            "layout_ok": e.layout_ok,
            "note": if e.kind == EncKind::Cryp { "session key record (not decrypted)" } else { "encrypted telemetry (not decrypted)" },
        }),
        Record::Info11 { serial, len } => json!({
            "type": "info_0x11", "crc16_ok": crc16, "len": len, "serial": hide(text(serial)),
        }),
        Record::Unknown { selector, len } => json!({
            "type": "unknown", "crc16_ok": crc16, "selector": selector, "len": len,
        }),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut file = None;
    let mut rate = None;
    let mut fmt = Format::Cf32;
    let mut redacted = false;
    let mut turbo = true;
    let mut failures = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--rate" | "-r" => {
                i += 1;
                rate = args.get(i).and_then(|v| v.parse::<f64>().ok()).map(|v| v as u32);
            }
            "--format" | "-f" => {
                i += 1;
                match args.get(i).and_then(|v| Format::parse(v)) {
                    Some(f) => fmt = f,
                    None => return usage(),
                }
            }
            "--redact" => redacted = true,
            "--no-turbo" => turbo = false,
            "--failures" => failures = true,
            "-h" | "--help" => return usage(),
            a if file.is_none() && !a.starts_with('-') => file = Some(a.to_string()),
            _ => return usage(),
        }
        i += 1;
    }
    let (Some(file), Some(rate)) = (file, rate) else { return usage() };
    let bytes = match std::fs::read(&file) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("{file}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let x = to_c16(&bytes, fmt);
    let mut rx = match Receiver::new(rate) {
        Ok(r) => Box::new(r),
        Err(e) => {
            eprintln!("unsupported sample rate {rate}: {e:?}");
            return ExitCode::FAILURE;
        }
    };
    if !turbo {
        rx.config.max_iterations = 0;
    }
    let (mut found, mut ok) = (0, 0);
    decode_recording(&mut rx, &x, |f| {
        let fs = rate as f64;
        match f.result {
            Ok(b) => {
                found += 1;
                if b.crc24_ok {
                    ok += 1;
                }
                let v = json!({
                    "t_ms": (f.chunk_start as f64 + b.start) / fs * 1e3,
                    "layout": b.layout.name(),
                    "cfo_hz": b.cfo_hz.round(),
                    "snr_db": (b.snr_db * 10.0).round() / 10.0,
                    "cp_metric": b.cp_metric as f64 / 1000.0,
                    "zc600": b.zc_metric as f64 / 1000.0,
                    "zc_pair": b.zc_pair_metric as f64 / 1000.0,
                    "mean_llr": b.mean_llr,
                    "path": b.path.map(|p| match p { Path::Systematic => "systematic", Path::Turbo => "turbo" }),
                    "crc24_ok": b.crc24_ok,
                    "turbo_half_iterations": b.half_iterations,
                    "systematic_bit_errors": if b.crc24_ok { Some(b.systematic_errors) } else { None },
                    "record": if b.crc24_ok { b.parsed.as_ref().map(|p| record_json(p, redacted)) } else { None },
                });
                emit(&v);
            }
            Err((span, e)) => {
                if failures {
                    let v = json!({
                        "t_ms": (f.chunk_start + span.start) as f64 / fs * 1e3,
                        "error": format!("{e:?}"),
                    });
                    emit(&v);
                }
            }
        }
    });
    eprintln!("{found} bursts demodulated, {ok} CRC-valid");
    ExitCode::SUCCESS
}
