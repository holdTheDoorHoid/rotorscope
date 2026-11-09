//! End-to-end: firmware-shaped JSON lines in (file and stdin), tracker
//! output out. No serial port is opened.

use std::io::Write;
use std::process::{Command, Stdio};

use rid::Transport;
use rid::message::*;
use serde_json::json;

const BIN: &str = env!("CARGO_BIN_EXE_rid-view");

fn basic(id: &str) -> Message {
    Message::BasicId(BasicId {
        id_type: IdType::SerialNumber,
        ua_type: UaType::HelicopterOrMultirotor,
        uas_id: FixedStr::new(id.as_bytes()).unwrap(),
    })
}

fn location(lat: f64, lon: f64) -> Message {
    let raw = rid::encode::message(&Message::Location(Location {
        status: Status::Airborne,
        height_type: HeightType::AboveTakeoff,
        direction: Some(45.0),
        speed_horizontal: Some(3.0),
        speed_vertical: Some(0.0),
        position: Some(LatLon { lat, lon }),
        altitude_pressure: None,
        altitude_geodetic: Some(100.0),
        height: Some(60.0),
        horizontal_accuracy: HorizontalAccuracy::M10,
        vertical_accuracy: VerticalAccuracy::M10,
        baro_accuracy: VerticalAccuracy::Unknown,
        speed_accuracy: SpeedAccuracy::Unknown,
        timestamp: None,
        timestamp_accuracy: None,
    }))
    .unwrap();
    decode(&raw).unwrap()
}

fn rid_line(tr: Transport, mac: &str, rssi: i8, msgs: &[Message]) -> String {
    json!({"t":"rid","tr":tr,"ch":6,"rssi":rssi,"mac":mac,"ctr":1,"up_us":1000,"msgs":msgs,"new_fw_field":true})
        .to_string()
}

/// A Wi-Fi node and a BLE node hearing the same drone (different MACs),
/// plus the line types a node prints that are not detections.
fn recording() -> String {
    [
        r#"{"t":"hello","node":"wifi24","fw":"0.1.0","chip":"esp32s3","proto":1}"#.to_string(),
        "garbage from a reset \u{fffd}".to_string(),
        r#"{"t":"log","lvl":"info","msg":"channel 6"}"#.to_string(),
        rid_line(
            Transport::BleLegacy,
            "c0:11:22:33:44:55",
            -80,
            &[location(INDEPENDENCE.0, INDEPENDENCE.1)],
        ),
        rid_line(
            Transport::WifiBeacon,
            "60:60:1f:00:00:01",
            -60,
            &[basic("SN-CLI-1"), location(INDEPENDENCE.0, INDEPENDENCE.1)],
        ),
        rid_line(
            Transport::BleLegacy,
            "c0:11:22:33:44:55",
            -79,
            &[basic("SN-CLI-1")],
        ),
        r#"{"t":"stats","up_us":5000000,"seen":120,"rid":3,"drop":0}"#.to_string(),
    ]
    .join("\n")
        + "\n"
}

const CITY_HALL: &str = "39.952583,-75.165222";
const INDEPENDENCE: (f64, f64) = (39.948889, -75.150000);

fn tracks(stdout: &[u8]) -> Vec<serde_json::Value> {
    String::from_utf8_lossy(stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
        .collect()
}

#[test]
fn file_input_merges_and_emits_hub_json() {
    let dir = std::env::temp_dir().join(format!("rid-view-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let rec = dir.join("rec.jsonl");
    let csv = dir.join("log.csv");
    std::fs::write(&rec, recording()).unwrap();
    let _ = std::fs::remove_file(&csv);

    let out = Command::new(BIN)
        .args(["--once", "--json", "--own-pos", CITY_HALL, "--log"])
        .arg(&csv)
        .arg(&rec)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let t = tracks(&out.stdout);
    assert_eq!(t.len(), 1, "one drone across two transports: {t:?}");
    assert_eq!(t[0]["ids"][0]["id"], "SN-CLI-1");
    assert_eq!(t[0]["links"].as_array().unwrap().len(), 2);
    assert_eq!(t[0]["own"]["dist_m"], 1363.9);
    assert_eq!(t[0]["loc"]["alt_geo"], 100.0);

    let log = std::fs::read_to_string(&csv).unwrap();
    let mut lines = log.lines();
    assert_eq!(lines.next().unwrap(), tracker::csv::HEADER.trim_end());
    let row = lines.next().unwrap();
    assert!(
        row.contains(",SN-CLI-1,SerialNumber,") && row.contains("WifiBeacon|BleLegacy"),
        "{row}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn stdin_table() {
    let mut child = Command::new(BIN)
        .args(["--once", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(recording().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("SN-CLI-1"), "{text}");
    assert!(text.contains("own position unknown"), "{text}");
    assert!(text.contains("WB+B4"), "{text}");
    assert!(text.contains("fw=0.1.0"), "hello shown: {text}");
    assert!(text.contains("unparsed: 1"), "{text}");
    assert!(text.contains("new drone: SN-CLI-1"), "{text}");
    assert!(!text.contains('\x1b'), "no ANSI when piped");
}

#[test]
fn demo_once() {
    let out = Command::new(BIN)
        .args(["--demo", "--once", "--demo-secs", "3", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let t = tracks(&out.stdout);
    assert_eq!(t.len(), 2);
    // No --own-pos: geometry is null (P4), not invented.
    assert!(t.iter().all(|t| t["own"].is_null()));
}

#[test]
fn bad_arguments() {
    let out = Command::new(BIN)
        .args(["--own-pos", "91,0"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let out = Command::new(BIN)
        .args(["--cot", "tcp://x:1", "--demo", "--once"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let out = Command::new(BIN).arg("--help").output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("--own-pos"));
}

#[test]
fn cot_over_udp() {
    // Unicast to localhost stands in for the multicast group.
    let rx = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    rx.set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    let url = format!("udp://{}", rx.local_addr().unwrap());
    let out = Command::new(BIN)
        .args([
            "--demo",
            "--once",
            "--demo-secs",
            "2",
            "--json",
            "--cot",
            &url,
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut seen = std::collections::BTreeSet::new();
    let mut buf = [0u8; 4096];
    while let Ok(n) = rx.recv(&mut buf) {
        let xml = String::from_utf8_lossy(&buf[..n]).to_string();
        assert!(
            xml.starts_with("<?xml") && xml.ends_with("</event>"),
            "{xml}"
        );
        let uid = xml
            .split("uid=\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap()
            .to_string();
        seen.insert(uid);
        if seen.len() == 4 {
            break;
        }
    }
    let want: std::collections::BTreeSet<String> = [
        "drone-DEMO-ALPHA-0001",
        "pilot-DEMO-ALPHA-0001",
        "drone-DEMO-BRAVO-0002",
        "pilot-DEMO-BRAVO-0002",
    ]
    .map(String::from)
    .into();
    assert_eq!(seen, want);
}

// ------------------------------------------------------------- --http --

/// Kills the server when the test ends, pass or fail.
struct Server(std::process::Child, String);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Start `rid-view --demo --http 127.0.0.1:0` and read back the port it got.
fn serve(extra: &[&str]) -> Server {
    use std::io::BufRead;
    let mut child = Command::new(BIN)
        .args(["--demo", "--http", "127.0.0.1:0"])
        .args(extra)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut err = std::io::BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    err.read_line(&mut line).unwrap();
    let addr = line
        .split("http://")
        .nth(1)
        .unwrap_or_else(|| panic!("no address in {line:?}"))
        .trim()
        .trim_end_matches('/')
        .to_string();
    Server(child, addr)
}

/// A minimal HTTP/1.1 client: returns (status, headers, body).
fn http(addr: &str, req: &str) -> (u16, String, Vec<u8>) {
    use std::io::Read;
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    s.write_all(req.as_bytes()).unwrap();
    let mut out = Vec::new();
    s.read_to_end(&mut out).unwrap();
    let split = out.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8(out[..split].to_vec()).unwrap();
    let status = head[9..12].parse().unwrap();
    (status, head, out[split + 4..].to_vec())
}

fn get(addr: &str, path: &str) -> (u16, String, Vec<u8>) {
    http(addr, &format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n"))
}

fn post(addr: &str, path: &str, body: &str) -> (u16, String, Vec<u8>) {
    http(
        addr,
        &format!(
            "POST {path} HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ),
    )
}

fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

/// Poll /api/state until `ok` holds (the demo and the main loop run live).
fn state_until(addr: &str, ok: impl Fn(&serde_json::Value) -> bool) -> serde_json::Value {
    let mut last = serde_json::Value::Null;
    for _ in 0..60 {
        let (status, head, body) = get(addr, "/api/state");
        assert_eq!(status, 200);
        assert_eq!(header(&head, "content-type"), Some("application/json"));
        if !body.is_empty() {
            last = serde_json::from_slice(&body).unwrap();
            if ok(&last) {
                return last;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("state never matched: {last}");
}

#[test]
fn http_serves_page_and_state() {
    let srv = serve(&["--own-pos", CITY_HALL]);
    let addr = &srv.1;

    // The page: gzip from the webui crate, revalidated by ETag.
    let (status, head, body) = get(addr, "/");
    assert_eq!(status, 200, "{head}");
    assert_eq!(header(&head, "content-encoding"), Some("gzip"));
    assert_eq!(header(&head, "content-type"), Some(webui::CONTENT_TYPE));
    assert_eq!(body, webui::INDEX_HTML_GZ);
    let etag = header(&head, "etag").unwrap().to_string();
    let (status, _, body) = http(
        addr,
        &format!("GET / HTTP/1.1\r\nHost: x\r\nIf-None-Match: {etag}\r\n\r\n"),
    );
    assert_eq!(status, 304);
    assert!(body.is_empty());
    assert_eq!(get(addr, "/nope").0, 404);
    assert_eq!(post(addr, "/api/state", "{}").0, 405);

    // The state: both demo drones, both demo nodes, our position.
    let v = state_until(addr, |v| {
        v["tracks"].as_array().is_some_and(|t| t.len() == 2)
    });
    assert_eq!(v["t"], "state");
    assert_eq!(v["v"], 1);
    assert_eq!(v["own"]["source"], "manual");
    assert_eq!(v["own"]["lat"], 39.952583);
    assert_eq!(v["gps"]["state"], "absent");
    assert_eq!(v["rec"], false);
    let roles: Vec<_> = v["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["role"].clone())
        .collect();
    assert_eq!(roles, [json!("Wifi24"), json!("Ble")]);
    assert!(
        v["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["t"] == "track" && t["own"]["dist_m"].is_number())
    );

    // The page moves our position, then clears it.
    let (status, _, body) = post(addr, "/api/own-pos", r#"{"lat":40.0,"lon":-75.5,"alt":12}"#);
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let v = state_until(addr, |v| v["own"]["lat"] == 40.0);
    assert_eq!(v["manual"], json!({"lat":40.0,"lon":-75.5,"alt":12.0}));
    assert_eq!(v["own"]["alt"], 12.0);

    let (status, _, body) = post(addr, "/api/own-pos", r#"{"lat":123,"lon":0}"#);
    assert_eq!(status, 400);
    let err: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(err["error"].as_str().unwrap().contains("lat"));

    assert_eq!(post(addr, "/api/own-pos", r#"{"clear":true}"#).0, 200);
    let v = state_until(addr, |v| v["own"].is_null());
    assert!(v["manual"].is_null());
    // P4: no position, so no geometry.
    assert!(
        v["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["own"].is_null())
    );
}

#[test]
fn http_with_once_is_refused() {
    let out = Command::new(BIN)
        .args(["--demo", "--once", "--http", "127.0.0.1:0"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}
