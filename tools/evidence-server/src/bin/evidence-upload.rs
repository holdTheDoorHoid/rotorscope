//! evidence-upload: the laptop between a Rotorscope hub and an
//! evidence-server. The P4 hub has no network of its own, so its records
//! reach the server through a laptop, one of three ways:
//!
//! ```text
//! evidence-upload --server URL --serial /dev/ttyUSB0 [--replay] [--seconds N] [--save FILE] [--send TEXT]
//!     live: read the hub's USB console (@EV1 lines); --replay first asks the
//!     hub (console `r`) to re-send its current card file; --send types
//!     console commands (e.g. "k" for a checkpoint now, "nSome note\r")
//! evidence-upload --server URL --capture console.log
//!     a console capture saved earlier
//! evidence-upload --server URL --card /media/.../ROTORSCP/EVIDENCE
//!     the SD card's evidence files, in a card reader
//!
//! common: [--register NAME] [--token T] [--actor NAME] [--allow-gap]
//!         [--ca cert.pem]  trust this certificate (a bench server's own)
//! ```
//!
//! It uploads each boot's records from the first one the server lacks, in
//! batches that end on a signed checkpoint (at most 15 MiB each). Records
//! after a boot's last checkpoint (its "tail") wait until a later boot's
//! `continuity` record, which names the tail's last record by number and
//! hash, is stored; then the tail is sent on its own and the server checks
//! it against that record. The laptop vouches for nothing: the server
//! checks the hub's signatures.
//!
//! What it reads from its peers is capped: console lines (a line with no
//! end is dropped, not held), records held in memory, and server replies.
//! Text from the server is printed without control characters.

use std::collections::BTreeMap;
use std::io::Read;
use std::time::{Duration, Instant};

use evidence::format::{self, Body, EventCode};
use evidence::verify::hex;
use evidence_server::lines::{Line, LineReader, MAX_LINE};
use evidence_server::plan;

/// A batch the uploader builds is at most this (the server takes 16 MiB).
const BATCH_BYTES: usize = 15 << 20;
/// A server reply read as JSON, and an error message, at most.
const MAX_REPLY: u64 = 1 << 20;
const MAX_ERROR: u64 = 4 << 10;

struct Opts {
    agent: ureq::Agent,
    server: String,
    token: Option<String>,
    actor: String,
    register: Option<String>,
    allow_gap: bool,
}

#[derive(Default)]
struct Collected {
    chains: plan::Chains,
    key: Option<[u8; 65]>,
    conflicts: u32,
    bad: u32,
    held: usize,
    dropped: u64,
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
            None if self.held + rec.len() > plan::MAX_HELD_BYTES => {
                if self.dropped == 0 {
                    eprintln!("evidence-upload: WARNING holding {} MiB of records: dropping new ones (upload, then run again)", plan::MAX_HELD_BYTES >> 20);
                }
                self.dropped += 1;
            }
            None => {
                self.held += rec.len();
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

fn agent(ca: Option<&str>) -> ureq::Agent {
    let mut b = ureq::AgentBuilder::new().timeout(Duration::from_secs(30));
    if let Some(ca) = ca {
        use rustls::pki_types::pem::PemObject;
        let mut roots = rustls::RootCertStore::empty();
        for c in rustls::pki_types::CertificateDer::pem_file_iter(ca).expect("read --ca") {
            roots.add(c.expect("certificate")).expect("add certificate");
        }
        let cfg = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        b = b.tls_config(std::sync::Arc::new(cfg));
    }
    b.build()
}

fn req(o: &Opts, method: &str, path: &str) -> ureq::Request {
    let mut r = o.agent.request(method, &format!("{}{path}", o.server.trim_end_matches('/'))).set("X-Actor", &o.actor);
    if let Some(t) = &o.token {
        r = r.set("Authorization", &format!("Bearer {t}"));
    }
    r
}

/// A server reply as JSON, read up to [`MAX_REPLY`].
fn json(r: ureq::Response) -> serde_json::Value {
    let mut b = Vec::new();
    let _ = r.into_reader().take(MAX_REPLY).read_to_end(&mut b);
    serde_json::from_slice(&b).unwrap_or_default()
}

/// Server text for the terminal: no control characters (a hostile server
/// could otherwise move the cursor or rewrite what was printed).
fn clean(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

fn send(o: &Opts, id: &str, b: &str, recs: &BTreeMap<u64, Vec<u8>>, first: u64, last: u64, what: &str) -> bool {
    let mut batch = Vec::new();
    for s in first..=last {
        batch.extend_from_slice(&recs[&s]);
    }
    match req(o, "POST", &format!("/api/devices/{id}/batches")).set("Content-Type", "application/octet-stream").send_bytes(&batch) {
        Ok(r) => {
            let v = json(r);
            println!("evidence-upload: boot {b}: records #{first}..#{last} ({what}) stored as batch {} (sha256 {})", v["batch"], clean(v["sha256"].as_str().unwrap_or("?")));
            true
        }
        Err(e) => {
            eprintln!("evidence-upload: boot {b}: records #{first}..#{last} ({what}) refused: {}", describe(e));
            false
        }
    }
}

/// What the server has: the next record it lacks, per boot.
fn status(o: &Opts, id: &str, key: &[u8; 65]) -> Option<serde_json::Value> {
    match req(o, "GET", &format!("/api/devices/{id}")).call() {
        Ok(r) => Some(json(r)),
        Err(ureq::Error::Status(404, _)) => {
            let Some(name) = &o.register else {
                eprintln!("evidence-upload: unit {id} is not registered. Check this fingerprint against the one the unit prints at boot, then run again with --register NAME");
                return None;
            };
            match req(o, "POST", "/api/devices").send_json(serde_json::json!({ "public_key": hex(key), "name": name })) {
                Ok(_) => println!("evidence-upload: registered unit {id} as {name:?}"),
                Err(e) => {
                    eprintln!("evidence-upload: registering failed: {}", describe(e));
                    return None;
                }
            }
            Some(serde_json::json!({}))
        }
        Err(e) => {
            eprintln!("evidence-upload: server: {}", describe(e));
            None
        }
    }
}

/// The first record to send for a boot, or `None` to skip it.
fn start_of(o: &Opts, status: &serde_json::Value, b: &str, recs: &BTreeMap<u64, Vec<u8>>, quiet: bool) -> Option<u64> {
    let start = status["next_seq"][b].as_u64().unwrap_or(0);
    if recs.contains_key(&start) {
        return Some(start);
    }
    match recs.range(start..).next() {
        Some((&s, _)) if o.allow_gap => {
            if !quiet {
                eprintln!("evidence-upload: boot {b}: records #{start}..#{} are missing here; uploading from #{s} (--allow-gap: the server will show the gap)", s - 1);
            }
            Some(s)
        }
        Some((&s, _)) => {
            if !quiet {
                eprintln!("evidence-upload: boot {b}: the server needs #{start} next but this laptop has #{s}.. only; replay the card file (--replay) or read the card (--card), or use --allow-gap");
            }
            None
        }
        None => None,
    }
}

/// Upload what is ready. Returns batches stored.
fn upload(o: &Opts, c: &Collected) -> u32 {
    let Some(key) = c.key else {
        eprintln!("evidence-upload: no boot event or checkpoint seen yet: unit's key unknown");
        return 0;
    };
    let id = String::from_utf8(evidence::fingerprint(&key).to_vec()).unwrap();
    let Some(st) = status(o, &id, &key) else { return 0 };
    let mut stored = 0;
    // First every boot's signed batches (a later boot's continuity record
    // must be stored before the earlier boot's tail can be)...
    for (boot, recs) in &c.chains {
        let b = hex(boot);
        let Some(start) = start_of(o, &st, &b, recs, false) else { continue };
        for (first, last) in plan::signed_batches(recs, start, BATCH_BYTES) {
            if !send(o, &id, &b, recs, first, last, "signed") {
                break;
            }
            stored += 1;
        }
    }
    // ...then the tails a later boot now covers.
    let tails: Vec<_> = c.chains.keys().filter(|b| plan::covered_through(&c.chains, b).is_some()).collect();
    if tails.is_empty() {
        return stored;
    }
    let Some(st) = status(o, &id, &key) else { return stored };
    for boot in tails {
        let recs = &c.chains[boot];
        let b = hex(boot);
        let Some(start) = start_of(o, &st, &b, recs, true) else { continue };
        if let Some((first, last)) = plan::tail(recs, start, plan::covered_through(&c.chains, boot))
            && send(o, &id, &b, recs, first, last, "tail, covered by the next boot's continuity record")
        {
            stored += 1;
        }
    }
    stored
}

fn describe(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, r) => {
            let mut b = Vec::new();
            let _ = r.into_reader().take(MAX_ERROR).read_to_end(&mut b);
            format!("{code} {}", clean(&String::from_utf8_lossy(&b)))
        }
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
        agent: agent(arg(&args, "--ca").as_deref()),
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
        let send = arg(&args, "--send").map(|s| s.replace("\\r", "\r"));
        serial(&o, &mut c, &port, args.iter().any(|a| a == "--replay"), arg(&args, "--seconds").and_then(|s| s.parse().ok()), arg(&args, "--save"), send);
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
        // Record numbers come from the hub: no arithmetic that can overflow.
        let missing = (u128::from(last) - u128::from(first) + 1).saturating_sub(m.len() as u128);
        println!("evidence-upload: boot {} records #{first}..#{last} ({} held, {missing} missing in between)", hex(b), m.len());
    }
    if c.bad + c.conflicts > 0 || c.dropped > 0 {
        println!("evidence-upload: {} unreadable or over-long lines/records, {} conflicting duplicates, {} records not held (memory cap)", c.bad, c.conflicts, c.dropped);
    }
}

fn serial(o: &Opts, c: &mut Collected, path: &str, replay: bool, seconds: Option<u64>, save: Option<String>, send: Option<String>) {
    // DTR and RTS drive the P4's EN and BOOT pins (CH340 auto-reset).
    // Linux asserts both at open, which is harmless; DTR released while RTS
    // is still asserted resets the chip. So release RTS first, then DTR.
    let mut port = serialport::new(path, 115_200).timeout(Duration::from_millis(200)).open().expect("open serial port");
    let _ = port.write_request_to_send(false);
    let _ = port.write_data_terminal_ready(false);
    let mut save = save.map(|f| std::fs::OpenOptions::new().append(true).create(true).open(f).expect("save file"));
    if replay {
        std::thread::sleep(Duration::from_millis(300));
        let _ = port.write_all(b"r");
        println!("evidence-upload: asked the hub to replay its current evidence file");
    }
    if let Some(t) = send {
        // One character at a time: the hub reads one command byte per pass.
        for b in t.bytes() {
            let _ = port.write_all(&[b]);
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let t0 = Instant::now();
    let mut last_up = Instant::now();
    let mut reader = std::io::BufReader::new(port);
    // Bounded: a console that never sends a newline cannot grow memory, and
    // a line split by a read timeout is kept, not dropped.
    let mut lines = LineReader::new(MAX_LINE);
    loop {
        if seconds.is_some_and(|s| t0.elapsed() >= Duration::from_secs(s)) {
            break;
        }
        match lines.read(&mut reader) {
            Ok(None) => {}
            Ok(Some(Line::TooLong)) => c.bad += 1,
            Ok(Some(Line::Text(line))) => {
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
