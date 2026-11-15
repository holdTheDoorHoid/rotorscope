//! evidence-upload: the laptop between a Rotorscope hub and an
//! evidence-server. The P4 hub has no network of its own, so its records
//! reach the server through a laptop, one of three ways:
//!
//! ```text
//! evidence-upload --server URL --serial /dev/ttyUSB0 [--replay] [--seconds N] [--save FILE]
//!     live: read the hub's USB console (@EV1 lines); --replay first asks the
//!     hub (console `r`) to re-send its current card file
//! evidence-upload --server URL --capture console.log
//!     a console capture saved earlier
//! evidence-upload --server URL --card /media/.../ROTORSCP/EVIDENCE
//!     the SD card's evidence files, in a card reader
//!
//! common: [--register NAME] [--token T] [--actor NAME] [--allow-gap]
//! ```
//!
//! It uploads each boot's records from the first one the server lacks, in
//! batches that end on a signed checkpoint (the server refuses anything
//! else). Records after the last checkpoint wait for the next one. The
//! laptop vouches for nothing: the server checks the hub's signatures.

use std::collections::BTreeMap;
use std::io::{BufRead, Read};
use std::time::{Duration, Instant};

use evidence::format::{self, Body, EventCode, Kind};
use evidence::verify::hex;

struct Opts {
    server: String,
    token: Option<String>,
    actor: String,
    register: Option<String>,
    allow_gap: bool,
}

#[derive(Default)]
struct Collected {
    chains: BTreeMap<[u8; 8], BTreeMap<u64, Vec<u8>>>,
    key: Option<[u8; 65]>,
    conflicts: u32,
    bad: u32,
}

impl Collected {
    fn add(&mut self, rec: &[u8]) {
        let Ok(r) = format::parse(rec) else {
            self.bad += 1;
            return;
        };
        if rec.len() != r.header.len {
            self.bad += 1;
            return;
        }
        match r.body {
            Body::Event(e) if e.code == EventCode::Boot as u16 && e.data.len() >= 66 => {
                self.key = e.data[1..66].try_into().ok();
            }
            Body::Checkpoint(c) if self.key.is_none() => self.key = Some(c.pubkey),
            _ => {}
        }
        let m = self.chains.entry(r.header.boot_id).or_default();
        match m.get(&r.header.seq) {
            Some(old) if old.as_slice() != rec => {
                self.conflicts += 1;
                eprintln!("evidence-upload: WARNING two different records #{} for boot {}", r.header.seq, hex(&r.header.boot_id));
            }
            Some(_) => {}
            None => {
                m.insert(r.header.seq, rec.to_vec());
            }
        }
    }

    fn add_stream(&mut self, bytes: &[u8]) {
        for (off, r) in format::records(bytes) {
            match r {
                Ok(r) => self.add(r.bytes),
                Err(e) => {
                    eprintln!("evidence-upload: stopped at byte {off}: {e}");
                    self.bad += 1;
                }
            }
        }
    }

    fn add_line(&mut self, line: &str) -> bool {
        let Some(b64) = line.trim_end().strip_prefix("@EV1 ") else { return false };
        match evidence::b64::decode_vec(b64) {
            Some(b) => self.add(&b),
            None => self.bad += 1,
        }
        true
    }
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(Duration::from_secs(30)).build()
}

fn req(o: &Opts, method: &str, path: &str) -> ureq::Request {
    let mut r = agent().request(method, &format!("{}{path}", o.server.trim_end_matches('/'))).set("X-Actor", &o.actor);
    if let Some(t) = &o.token {
        r = r.set("Authorization", &format!("Bearer {t}"));
    }
    r
}

/// Upload what is ready. Returns batches stored.
fn upload(o: &Opts, c: &Collected) -> u32 {
    let Some(key) = c.key else {
        eprintln!("evidence-upload: no boot event or checkpoint seen yet: unit's key unknown");
        return 0;
    };
    let id = String::from_utf8(evidence::fingerprint(&key).to_vec()).unwrap();
    let status = match req(o, "GET", &format!("/api/devices/{id}")).call() {
        Ok(r) => r.into_json::<serde_json::Value>().unwrap_or_default(),
        Err(ureq::Error::Status(404, _)) => {
            let Some(name) = &o.register else {
                eprintln!("evidence-upload: unit {id} is not registered. Check this fingerprint against the one the unit prints at boot, then run again with --register NAME");
                return 0;
            };
            match req(o, "POST", "/api/devices").send_json(serde_json::json!({ "public_key": hex(&key), "name": name })) {
                Ok(_) => println!("evidence-upload: registered unit {id} as {name:?}"),
                Err(e) => {
                    eprintln!("evidence-upload: registering failed: {}", describe(e));
                    return 0;
                }
            }
            serde_json::json!({})
        }
        Err(e) => {
            eprintln!("evidence-upload: server: {}", describe(e));
            return 0;
        }
    };
    let mut stored = 0;
    for (boot, recs) in &c.chains {
        let b = hex(boot);
        let mut start = status["next_seq"][&b].as_u64().unwrap_or(0);
        if !recs.contains_key(&start) {
            match recs.range(start..).next() {
                Some((&s, _)) if o.allow_gap => {
                    eprintln!("evidence-upload: boot {b}: records #{start}..#{} are missing here; uploading from #{s} (--allow-gap: the server will show the gap)", s - 1);
                    start = s;
                }
                Some((&s, _)) => {
                    eprintln!("evidence-upload: boot {b}: the server needs #{start} next but this laptop has #{s}.. only; replay the card file (--replay) or read the card (--card), or use --allow-gap");
                    continue;
                }
                None => continue,
            }
        }
        let mut end = None;
        let mut s = start;
        while let Some(r) = recs.get(&s) {
            if format::parse(r).is_ok_and(|p| p.header.kind == Kind::Checkpoint) {
                end = Some(s);
            }
            s += 1;
        }
        let Some(end) = end else {
            continue;
        };
        let mut batch = Vec::new();
        for s in start..=end {
            batch.extend_from_slice(&recs[&s]);
        }
        match req(o, "POST", &format!("/api/devices/{id}/batches")).set("Content-Type", "application/octet-stream").send_bytes(&batch) {
            Ok(r) => {
                let v: serde_json::Value = r.into_json().unwrap_or_default();
                println!("evidence-upload: boot {b}: records #{start}..#{end} stored as batch {} (sha256 {})", v["batch"], v["sha256"].as_str().unwrap_or("?"));
                stored += 1;
            }
            Err(e) => eprintln!("evidence-upload: boot {b}: records #{start}..#{end} refused: {}", describe(e)),
        }
    }
    stored
}

fn describe(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, r) => format!("{code} {}", r.into_string().unwrap_or_default()),
        e => e.to_string(),
    }
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(server) = arg(&args, "--server") else {
        eprintln!("usage: evidence-upload --server URL (--serial PORT [--replay] [--seconds N] [--save FILE] | --capture FILE | --card DIR) [--register NAME] [--token T] [--actor NAME] [--allow-gap]");
        std::process::exit(2);
    };
    let o = Opts {
        server,
        token: arg(&args, "--token").or_else(|| std::env::var("EVIDENCE_TOKEN").ok()),
        actor: arg(&args, "--actor").unwrap_or_else(|| "evidence-upload".into()),
        register: arg(&args, "--register"),
        allow_gap: args.iter().any(|a| a == "--allow-gap"),
    };
    let mut c = Collected::default();
    if let Some(f) = arg(&args, "--capture") {
        let text = std::fs::read(&f).expect("read capture");
        let n = String::from_utf8_lossy(&text).lines().filter(|l| c.add_line(l)).count();
        println!("evidence-upload: {n} @EV1 lines in {f}");
    } else if let Some(d) = arg(&args, "--card") {
        let mut files: Vec<_> = std::fs::read_dir(&d).expect("read card folder").filter_map(|e| e.ok().map(|e| e.path())).filter(|p| evidence::verify::file_number(&p.to_string_lossy()).is_some()).collect();
        files.sort();
        for f in &files {
            c.add_stream(&std::fs::read(f).expect("read file"));
        }
        println!("evidence-upload: {} card files read", files.len());
    } else if let Some(port) = arg(&args, "--serial") {
        serial(&o, &mut c, &port, args.iter().any(|a| a == "--replay"), arg(&args, "--seconds").and_then(|s| s.parse().ok()), arg(&args, "--save"));
    } else {
        eprintln!("evidence-upload: give --serial, --capture or --card");
        std::process::exit(2);
    }
    report(&c);
    let n = upload(&o, &c);
    println!("evidence-upload: {n} batches stored");
}

fn report(c: &Collected) {
    for (b, m) in &c.chains {
        let first = m.keys().next().copied().unwrap_or(0);
        let last = m.keys().next_back().copied().unwrap_or(0);
        let missing = (last - first + 1) as usize - m.len();
        println!("evidence-upload: boot {} records #{first}..#{last} ({} held, {missing} missing in between)", hex(b), m.len());
    }
    if c.bad + c.conflicts > 0 {
        println!("evidence-upload: {} unreadable lines/records, {} conflicting duplicates", c.bad, c.conflicts);
    }
}

fn serial(o: &Opts, c: &mut Collected, path: &str, replay: bool, seconds: Option<u64>, save: Option<String>) {
    // Hold DTR and RTS released: on the CrowPanel they reset the P4.
    let mut port = serialport::new(path, 115_200).timeout(Duration::from_millis(200)).dtr_on_open(false).open().expect("open serial port");
    let _ = port.write_request_to_send(false);
    let _ = port.write_data_terminal_ready(false);
    let mut save = save.map(|f| std::fs::OpenOptions::new().append(true).create(true).open(f).expect("save file"));
    if replay {
        std::thread::sleep(Duration::from_millis(300));
        let _ = port.write_all(b"r");
        println!("evidence-upload: asked the hub to replay its current evidence file");
    }
    let t0 = Instant::now();
    let mut last_up = Instant::now();
    let mut reader = std::io::BufReader::new(port);
    let mut line = Vec::new();
    loop {
        if seconds.is_some_and(|s| t0.elapsed() >= Duration::from_secs(s)) {
            break;
        }
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) => {}
            Ok(_) => {
                let l = String::from_utf8_lossy(&line);
                if let Some(f) = save.as_mut() {
                    use std::io::Write;
                    let _ = f.write_all(&line);
                }
                if !c.add_line(&l) && (l.starts_with("hub-p4: evidence") || l.starts_with("hub-p4: cmd")) {
                    print!("{l}");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => {
                eprintln!("evidence-upload: serial: {e}");
                break;
            }
        }
        if last_up.elapsed() >= Duration::from_secs(15) {
            last_up = Instant::now();
            upload(o, c);
        }
    }
    let _ = reader.get_mut().read(&mut [0u8; 0]);
}
