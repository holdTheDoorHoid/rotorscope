//! Dump lines as the firmware prints them -> a messy log -> rawcap-pull ->
//! files -> the DroneID decoder. With the public recordings present
//! (`DRONEID_TESTDATA`), a real burst goes the whole way as a Path B
//! `nib8` capture; otherwise a synthetic payload checks the bytes only.

use std::path::PathBuf;
use std::process::Command;

use rawcap::crc32::crc32;
use rawcap::dump::{Enc, Kind, LINE_MAX, Meta, chunk, data_line, end_line};
use rawcap::iq::{Window, pack};

fn lines(data: &[u8], kind: Kind, rate: u32, id: u32, enc: Enc) -> Vec<String> {
    let m = Meta {
        id,
        kind,
        rate,
        bytes: data.len() as u32,
        freq_khz: 5_756_000,
        gain: 40,
        lsb: 6,
        swap_iq: false,
        lpf: 12,
        enc,
        t_us: 1,
        pos: 0,
        crc: crc32(data),
    };
    let mut v = vec![format!("@H {m}")];
    let mut buf = [0u8; LINE_MAX];
    for s in 0..m.lines() {
        v.push(data_line(&mut buf, id, s, chunk(data, s), enc).to_string());
    }
    v.push(end_line(&mut buf, &m).to_string());
    v
}

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rawcap-pull-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn run(log: &str, out: &PathBuf) -> (bool, String) {
    let logf = out.join("serial.log");
    std::fs::write(&logf, log).unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_rawcap-pull")).arg(&logf).arg("--out").arg(out).output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).into_owned())
}

#[test]
fn bytes_survive_a_messy_log_and_resend() {
    let data: Vec<u8> = (0..10_000u32).map(|k| (k * 31 + k / 7) as u8).collect();
    let l = lines(&data, Kind::Nib8, 40_000_000, 5, Enc::Hex);
    let mut log = String::from("ESP-ROM:esp32c5\r\nI (31) boot: ESP-IDF\r\n{\"t\":\"hello\"}\r\n");
    for (k, line) in l.iter().enumerate() {
        if k == 10 {
            continue; // lost
        }
        log += line;
        log += "\r\n";
        if k == 50 {
            log += "{\"t\":\"stats\",\"up_us\":1}\r\n";
        }
    }
    let out = tmpdir("messy");
    let (ok, stdout) = run(&log, &out);
    assert!(!ok);
    assert!(stdout.contains("resend 9 9"), "{stdout}");
    // The operator types `resend 9`; the new line lands later in the log.
    log += &l[10];
    log += "\r\n";
    let (ok, stdout) = run(&log, &out);
    assert!(ok, "{stdout}");
    assert_eq!(std::fs::read(out.join("c5-5.nib8")).unwrap(), data);
    let cs16 = std::fs::read(out.join("c5-5.cs16")).unwrap();
    assert_eq!(cs16.len(), 4 * data.len());
    assert!(std::fs::read_to_string(out.join("c5-5.json")).unwrap().contains("\"kind\":\"nib8\""));
}

#[test]
fn a_real_burst_decodes_after_the_round_trip() {
    let dir = std::env::var("DRONEID_TESTDATA")
        .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/droneid").to_string());
    let Ok(raw) = std::fs::read(PathBuf::from(&dir).join("mavic_air_2")) else {
        eprintln!("SKIPPED: recordings not found; run tools/fetch-droneid-samples.sh");
        return;
    };
    use droneid::host::{Format, decode_recording, resample_f32, to_c16, to_f32_pairs};
    use droneid::rx::Receiver;
    // The recording at 40 MS/s (its first two bursts are within +-10 MHz),
    // scaled to ~300 codes and quantised through the clip window: what a
    // Path B `grab` would hold.
    let y = resample_f32(&to_f32_pairs(&raw), 50e6, 40e6, 9.5e6);
    let peak = y.iter().fold(0f32, |m, v| m.max(v.0.abs()).max(v.1.abs()));
    let g = 300.0 / peak as f64;
    let nib: Vec<u8> = y
        .iter()
        .map(|&(i, q)| {
            let c = |v: f32| ((v as f64 * g).floor().clamp(-512.0, 511.0)) as i16;
            pack(Window::CLIP.quantise(c(i)), Window::CLIP.quantise(c(q)))
        })
        .collect();
    let log = lines(&nib, Kind::Nib8, 40_000_000, 1, Enc::B64).join("\n");
    let out = tmpdir("real");
    let (ok, stdout) = run(&log, &out);
    assert!(ok, "{stdout}");
    assert!(stdout.contains("--rate 40000000 --format ci16"), "{stdout}");
    let x = to_c16(&std::fs::read(out.join("c5-1.cs16")).unwrap(), Format::Ci16);
    let mut rx = Box::new(Receiver::new(40_000_000).unwrap());
    let mut ok_bursts = 0;
    decode_recording(&mut rx, &x, |f| {
        if let Ok(b) = f.result
            && b.crc24_ok
        {
            ok_bursts += 1;
        }
    });
    assert!(ok_bursts >= 2, "decoded {ok_bursts}");
}

#[test]
fn spectrum_puts_a_positive_tone_on_the_positive_side() {
    // I + jQ = exp(+j 2 pi 5 MHz t) at 40 MS/s, clip window: the strongest
    // bin must be near +5 MHz (the bring-up orientation check relies on it).
    let nib: Vec<u8> = (0..8192)
        .map(|n| {
            let ph = 2.0 * std::f64::consts::PI * 5e6 * n as f64 / 40e6;
            let c = |v: f64| (300.0 * v).floor() as i16;
            pack(Window::CLIP.quantise(c(ph.cos())), Window::CLIP.quantise(c(ph.sin())))
        })
        .collect();
    let out = tmpdir("tone");
    let logf = out.join("serial.log");
    std::fs::write(&logf, lines(&nib, Kind::Nib8, 40_000_000, 2, Enc::B64).join("\n")).unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_rawcap-pull"))
        .arg(&logf)
        .arg("--out")
        .arg(&out)
        .arg("--spectrum")
        .output()
        .unwrap();
    let s = String::from_utf8_lossy(&o.stdout);
    let peak = s
        .lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            let f: f64 = w.next()?.parse().ok()?;
            (w.next()? == "MHz").then_some(())?;
            let db: f64 = w.next()?.parse().ok()?;
            Some((f, db))
        })
        .fold((0.0, -999.0), |a, b| if b.1 > a.1 { b } else { a });
    assert!((peak.0 - 5.0).abs() < 1.0, "peak at {} MHz\n{s}", peak.0);
}
