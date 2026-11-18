//! Hardware-in-the-loop DroneID test for `firmware/node-sdr`.
//!
//! We may never transmit DroneID, so the C5's receiver is fed the public
//! recordings instead (RUB-SysSec, fetched by tools/fetch-droneid-samples.sh;
//! AGPL, never committed). Every burst the reference decoder finds in a
//! recording is turned into exactly what the C5's Path B capture would hold:
//! moved to +0.5 MHz (a C5 tuned to the whole MHz below a DroneID channel),
//! band-limited to the analog low-pass and resampled to 40 MS/s, receiver
//! noise added, quantised by the 4-bit clip window and packed into PARL_IO
//! bytes, with ~410 µs of noise before the burst so the trigger learns its
//! floor. Each 53,760-byte block (one ring grab) is sent over USB as `@F`
//! lines into the board's staging buffer, checked by CRC-32, and run through
//! the chip's live path (`hil run`: trigger, length gate, half-band, DC
//! removal, decoder, hub sighting).
//!
//! Identities are never printed: only counts and record types. The board
//! redacts its own `burst` lines (`reveal` stays off), and its HIL sightings
//! are built and checked but never sent to the hub.
//!
//! The board must run a node-sdr bench build with the feature `bringup`
//! (`@F`, `hil run`, `hop` and `trig` are refused otherwise; BRINGUP.md
//! "Bring-up builds"):
//!
//! ```sh
//! export ROTORSCOPE_BENCH=1
//! (cd firmware/node-sdr/c5 && cargo fw --features bringup)
//! tools/dev-board.sh --run 'cargo run --release -p droneid-hil -- --port "$BOARD_PORT"' 600 c5b
//! ```
//! Options: `--pre "trig 3"` console commands first, `--rms 200,100,60` burst levels (codes RMS; receiver noise is
//! `--noise 48`, the board's measured noise at gain 83), `--dir` recordings.
//!
//! Hostile-signal scenarios (docs/BENCH.md "node-sdr under hostile signals"; firmware
//! `radio/hilsim.rs`), instead of the replay:
//!
//! - `--train N,GAP_MS` with `--pulse burst|noise`: stage one pulse (the first recorded burst at
//!   the first `--rms` level, or 600 µs of white noise at that level, which needs no recording)
//!   and replay it on the chip as N pulses GAP_MS apart, once through the decode budget and once
//!   without (`nobudget`, the loop as it was). Prints both `hil_train` results.
//! - `--forge SECS[,CASE]`: run a forged-session scenario on the live follower (the node hops its
//!   DroneID plan meanwhile; CASE `start` (default), `flood` or `late`, firmware
//!   `radio/hilsim.rs`), print the board's `forge` report and `follow` lines, and one `expect`
//!   line: the case's expectation (docs/BENCH.md step 4) and whether this run met it.

use std::io::{BufRead, BufReader, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use droneid::host::{Format, decode_recording, resample_f32, to_c16, to_f32_pairs};
use droneid::rx::Receiver;
use rawcap::iq::{Window, pack};

const BLOCK: usize = 53_760;
const LEAD: usize = 16_384;

struct Rng(u64);
impl Rng {
    fn u(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
    }
    fn g(&mut self) -> f64 {
        let (a, b) = (self.u().max(1e-300), self.u());
        (-2.0 * a.ln()).sqrt() * (2.0 * std::f64::consts::PI * b).cos()
    }
}

/// Every burst the reference decoder finds (CRC-valid or not) at +0.5 MHz,
/// 40 MS/s, 50 µs margins; with whether the reference decoded it.
fn bursts(path: &std::path::Path) -> Vec<(Vec<(f32, f32)>, bool)> {
    let raw = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e} (run tools/fetch-droneid-samples.sh)", path.display()));
    let f = to_f32_pairs(&raw);
    let x = to_c16(&raw, Format::Cf32);
    let mut rx = Box::new(Receiver::new(50_000_000).unwrap());
    let mut out = vec![];
    decode_recording(&mut rx, &x, |r| {
        if let Ok(b) = r.result {
            let a = (r.chunk_start + b.span.start).saturating_sub(2500);
            let e = (r.chunk_start + b.span.end + 2500).min(f.len());
            let shift = 0.5e6 - b.cfo_hz;
            let mixed: Vec<(f32, f32)> = f[a..e]
                .iter()
                .enumerate()
                .map(|(n, &(i, q))| {
                    let ph = 2.0 * std::f64::consts::PI * shift * n as f64 / 50e6;
                    let (s, c) = ph.sin_cos();
                    ((i as f64 * c - q as f64 * s) as f32, (i as f64 * s + q as f64 * c) as f32)
                })
                .collect();
            out.push((resample_f32(&mixed, 50e6, 40e6, 9.5e6), b.crc24_ok));
        }
    });
    out
}

/// One ring grab: noise, the burst at `rms` codes, noise.
fn block(b: &[(f32, f32)], noise: f64, rms: f64, seed: u64) -> Vec<u8> {
    let mut rng = Rng(seed);
    let w = Window::CLIP;
    let p = (b.iter().map(|&(i, q)| (i as f64).powi(2) + (q as f64).powi(2)).sum::<f64>() / b.len() as f64).sqrt();
    let g = rms / p;
    (0..BLOCK)
        .map(|k| {
            let s = k.checked_sub(LEAD).and_then(|j| b.get(j)).map(|&(i, q)| (i as f64 * g, q as f64 * g)).unwrap_or((0.0, 0.0));
            let i = (s.0 + noise * rng.g()).floor().clamp(-512.0, 511.0) as i16;
            let q = (s.1 + noise * rng.g()).floor().clamp(-512.0, 511.0) as i16;
            pack(w.quantise(i), w.quantise(q))
        })
        .collect()
}

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in data.chunks(3) {
        let v = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..4 {
            if k <= c.len() {
                s.push(T[(v >> (18 - 6 * k) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

/// A JSON field as text (no JSON crate: these lines are flat).
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let k = format!("\"{key}\":");
    let i = line.find(&k)? + k.len();
    let rest = &line[i..];
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    Some(rest[..end].trim_matches('"'))
}

fn main() {
    let mut port = std::env::var("BOARD_PORT").ok();
    let mut dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/droneid").to_string();
    let mut rms_list = vec![200.0, 100.0, 60.0];
    let mut noise = 48.0;
    let mut pre: Vec<String> = vec![];
    let mut train: Option<(u32, u32)> = None;
    let mut pulse = String::from("burst");
    let mut forge: Option<(u32, String)> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--port" => port = args.next(),
            "--dir" => dir = args.next().expect("--dir"),
            "--rms" => rms_list = args.next().expect("--rms").split(',').map(|v| v.parse().expect("rms")).collect(),
            "--pre" => pre.push(args.next().expect("--pre")),
            "--noise" => noise = args.next().expect("--noise").parse().expect("noise"),
            "--train" => {
                let v = args.next().expect("--train N,GAP_MS");
                let (n, g) = v.split_once(',').expect("--train N,GAP_MS");
                train = Some((n.parse().expect("N"), g.parse().expect("GAP_MS")));
            }
            "--pulse" => pulse = args.next().expect("--pulse burst|noise"),
            "--forge" => {
                let v = args.next().expect("--forge SECS[,start|flood|late]");
                let (secs, case) = v.split_once(',').unwrap_or((&v, "start"));
                assert!(["start", "flood", "late"].contains(&case), "--forge SECS[,start|flood|late]");
                forge = Some((secs.parse().expect("SECS"), case.to_string()));
            }
            _ => panic!("unknown argument {a}"),
        }
    }
    let debug = std::env::var_os("HIL_DEBUG").is_some();
    let port = port.expect("--port or $BOARD_PORT (run under tools/dev-board.sh --run)");

    if train.is_some() || forge.is_some() {
        return scenarios(&port, &dir, train, &pulse, forge, noise, rms_list[0], &pre);
    }

    let mut all = vec![];
    for name in ["mavic_air_2", "mini2_sm"] {
        let b = bursts(&std::path::Path::new(&dir).join(name));
        eprintln!("{name}: {} bursts found by the reference decoder ({} CRC-valid)", b.len(), b.iter().filter(|x| x.1).count());
        all.extend(b);
    }

    // Raw tty: no echo, no line editing.
    let st = std::process::Command::new("stty").args(["-F", &port, "raw", "-echo", "115200"]).status().expect("stty");
    assert!(st.success(), "stty failed");
    let mut w = std::fs::OpenOptions::new().write(true).open(&port).expect("open port for writing");
    let r = std::fs::File::open(&port).expect("open port for reading");
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(r).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let send = |w: &mut std::fs::File, s: &str| {
        w.write_all(s.as_bytes()).expect("write");
        w.write_all(b"\n").expect("write");
    };
    // Park on one channel so the live path is quiet between runs.
    send(&mut w, "hop off");
    for c in &pre {
        // e.g. --pre "trig 3": console commands before the runs (restored by reboot).
        std::thread::sleep(Duration::from_millis(100));
        send(&mut w, c);
    }
    std::thread::sleep(Duration::from_millis(300));
    while rx.try_recv().is_ok() {}

    println!("rms,burst,ref_crc,rises,accepted,matched,crc_ok,frames,decode_ms,total_ms,record");
    let mut summary = vec![];
    for &rms in &rms_list {
        let (mut fed, mut det, mut crc, mut frames, mut ms) = (0, 0, 0, 0, vec![]);
        for (k, (b, ref_ok)) in all.iter().enumerate() {
            let data = block(b, noise, rms, k as u64 + 1);
            let crc32 = rawcap::crc32::crc32(&data);
            let t = Instant::now();
            for (j, chunk) in data.chunks(48).enumerate() {
                send(&mut w, &format!("@F {:x} {}", j * 48, b64(chunk)));
            }
            send(&mut w, &format!("hil run {} {crc32:08x}", data.len()));
            let deadline = Instant::now() + Duration::from_secs(20);
            let mut record = String::from("-");
            let mut result = None;
            while Instant::now() < deadline && result.is_none() {
                let Ok(line) = rx.recv_timeout(Duration::from_millis(200)) else { continue };
                if debug && (line.contains("\"t\":\"ev\"") || line.contains("\"t\":\"hil\"")) {
                    eprintln!("board: {line}");
                }
                if debug && line.contains("\"t\":\"burst\"") {
                    // Decoder metrics only: cut before any record field.
                    let cut = line.find("\"crc16\"").unwrap_or(line.len());
                    eprintln!("board: {}", &line[..cut]);
                }
                if line.contains("\"t\":\"burst\"") {
                    // Record types only; identities are never printed.
                    record = ["flight_info", "o4_encrypted", "info11", "unknown"]
                        .iter()
                        .find(|t| line.contains(&format!("\"type\":\"{t}\"")))
                        .map(|t| t.to_string())
                        .unwrap_or_else(|| if line.contains("\"crc\":false") { "detected".into() } else { "-".into() });
                } else if line.contains("\"t\":\"hil\"") {
                    result = Some(line);
                } else if line.contains("\"t\":\"err\"") {
                    eprintln!("board: {line}");
                }
            }
            let Some(res) = result else {
                eprintln!("burst {k}: no hil result within 20 s");
                continue;
            };
            if field(&res, "intact") != Some("true") {
                eprintln!("burst {k}: ring changed during the run: {res}");
            }
            if field(&res, "ok") != Some("true") {
                eprintln!("burst {k}: {res}");
                continue;
            }
            let g = |key| field(&res, key).and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
            fed += 1;
            det += (g("matched") > 0) as u32;
            crc += (g("crc_ok") > 0) as u32;
            frames += g("frames");
            if g("matched") > 0 {
                ms.push(g("decode_ms"));
            }
            println!(
                "{rms},{k},{ref_ok},{},{},{},{},{},{},{},{record}",
                g("rises"),
                g("accepted"),
                g("matched"),
                g("crc_ok"),
                g("frames"),
                g("decode_ms"),
                g("total_ms")
            );
            eprintln!("  rms {rms} burst {k}: {:.1} s on the wire+chip", t.elapsed().as_secs_f32());
        }
        ms.sort();
        let med = ms.get(ms.len() / 2).copied().unwrap_or(0);
        let max = ms.last().copied().unwrap_or(0);
        summary.push(format!(
            "rms {rms} (noise {noise}): fed {fed}, detected {det}, CRC-valid {crc}, sightings built {frames}, decode ms median {med} max {max}"
        ));
    }
    send(&mut w, "hop on");
    for s in summary {
        println!("# {s}");
    }
}

/// One ring grab holding a 600 µs pulse of white noise (the whole 40 MS/s
/// capture: a 20-40 MHz wide pulse) at `rms` codes over receiver noise.
fn noise_block(noise: f64, rms: f64, seed: u64) -> Vec<u8> {
    let mut rng = Rng(seed);
    let w = Window::CLIP;
    let len = 24_000; // 600 µs at 40 MS/s
    let g = rms / std::f64::consts::SQRT_2;
    (0..BLOCK)
        .map(|k| {
            let on = (LEAD..LEAD + len).contains(&k);
            let (si, sq) = if on { (g * rng.g(), g * rng.g()) } else { (0.0, 0.0) };
            let i = (si + noise * rng.g()).floor().clamp(-512.0, 511.0) as i16;
            let q = (sq + noise * rng.g()).floor().clamp(-512.0, 511.0) as i16;
            pack(w.quantise(i), w.quantise(q))
        })
        .collect()
}

/// The hostile-signal scenarios (see the module docs).
#[allow(clippy::too_many_arguments)]
fn scenarios(port: &str, dir: &str, train: Option<(u32, u32)>, pulse: &str, forge: Option<(u32, String)>, noise: f64, rms: f64, pre: &[String]) {
    let st = std::process::Command::new("stty").args(["-F", port, "raw", "-echo", "115200"]).status().expect("stty");
    assert!(st.success(), "stty failed");
    let mut w = std::fs::OpenOptions::new().write(true).open(port).expect("open port for writing");
    let r = std::fs::File::open(port).expect("open port for reading");
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(r).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let send = |w: &mut std::fs::File, s: &str| {
        w.write_all(s.as_bytes()).expect("write");
        w.write_all(b"\n").expect("write");
    };
    // Wait for a line of type `t` (and, if given, containing `has`).
    let wait = |t: &str, has: &str, secs: u64| -> Option<String> {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            let Ok(line) = rx.recv_timeout(Duration::from_millis(200)) else { continue };
            if line.contains("\"t\":\"err\"") || line.contains("\"t\":\"follow\"") {
                eprintln!("board: {line}");
            }
            if line.contains(&format!("\"t\":\"{t}\"")) && line.contains(has) {
                return Some(line);
            }
        }
        None
    };
    for c in pre {
        std::thread::sleep(Duration::from_millis(100));
        send(&mut w, c);
    }
    if let Some((n, gap)) = train {
        send(&mut w, "hop off");
        std::thread::sleep(Duration::from_millis(300));
        let data = match pulse {
            "noise" => noise_block(noise, rms, 7),
            _ => {
                let mut b = vec![];
                for name in ["mavic_air_2", "mini2_sm"] {
                    b.extend(bursts(&std::path::Path::new(dir).join(name)));
                }
                let (first, _) = b.first().expect("no burst in the recordings");
                block(first, noise, rms, 1)
            }
        };
        let crc32 = rawcap::crc32::crc32(&data);
        for (j, chunk) in data.chunks(48).enumerate() {
            send(&mut w, &format!("@F {:x} {}", j * 48, b64(chunk)));
        }
        for budget in [true, false] {
            let flag = if budget { "" } else { " nobudget" };
            send(&mut w, &format!("hil train {n} {gap}{flag} {} {crc32:08x}", data.len()));
            // Up to ~160 ms a decode on the chip.
            match wait("hil_train", "", 30 + n as u64 / 4) {
                Some(l) => println!("{pulse} rms {rms} budget {budget}: {l}"),
                None => eprintln!("no hil_train result (budget {budget})"),
            }
        }
        send(&mut w, "hop on");
    }
    if let Some((secs, case)) = forge {
        send(&mut w, "role droneid");
        std::thread::sleep(Duration::from_millis(300));
        send(&mut w, "hop on");
        std::thread::sleep(Duration::from_millis(300));
        send(&mut w, &format!("hil forge {secs} {case}"));
        match wait("forge", "\"done\":true", secs as u64 + 20) {
            Some(l) => {
                println!("forge {case} {secs} s: {l}");
                println!("{}", forge_verdict(&case, &l));
            }
            None => eprintln!("no forge report within {} s", secs + 20),
        }
        send(&mut w, "role auto");
    }
}

/// A field of a flat JSON line, arrays whole (`"real":[true,3,0]` -> `[true,3,0]`).
fn jfield<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let at = line.find(&format!("\"{key}\":"))? + key.len() + 3;
    let rest = &line[at..];
    let end = if rest.starts_with('[') { rest.find(']')? + 1 } else { rest.find([',', '}'])? };
    Some(&rest[..end])
}

/// The expectation for one `hil forge` case (docs/BENCH.md step 4; the host
/// model `crates/rawcap/tests/follow_forge.rs` gives the odds) and whether
/// the report meets it.
fn forge_verdict(case: &str, line: &str) -> String {
    let num = |k: &str| jfield(line, k).and_then(|v| v.parse::<i64>().ok()).unwrap_or(-1);
    let real = jfield(line, "real").unwrap_or("[]");
    let followed = real.starts_with("[true");
    let contra = jfield(line, "contra").unwrap_or("[]").trim_matches(['[', ']']).split(',').map(|v| v.parse::<i64>().unwrap_or(-1)).collect::<Vec<_>>();
    let real_contra = real.trim_matches(['[', ']']).rsplit(',').next().and_then(|v| v.parse::<i64>().ok()).unwrap_or(-1);
    let common = num("evicted") == 0 && num("active") <= 4 && num("share_permille") <= 550;
    let ignored = contra.len() == 2 && contra[0] == contra[1] && contra[1] == real_contra;
    let (want, ok) = match case {
        "flood" => (
            "real followed within ~60 s (host model: 95 % by 60 s) and at the end; evicted 0; every wrong-time packet ignored",
            followed && (0..=90).contains(&num("followed_s")) && common && ignored,
        ),
        "late" => (
            "real followed before the forgers arrive and still followed at the end (host model: 99.5 %); evicted 0; every wrong-time packet ignored",
            followed && num("followed_s") >= 0 && common && ignored,
        ),
        _ => (
            "real followed only if the plan confirmed it before the fourth forged session (host model: 46 % with these phases); otherwise blocked true; evicted 0",
            (followed || jfield(line, "blocked") == Some("true") || num("followed_s") < 0) && common,
        ),
    };
    format!("expect {case}: {want}: {}", if ok { "MET" } else { "NOT MET" })
}

