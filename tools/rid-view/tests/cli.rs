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

fn rid_line(tr: Transport, mac: &str, rssi: i8, up_us: u64, msgs: &[Message]) -> String {
    json!({"t":"rid","tr":tr,"ch":6,"rssi":rssi,"mac":mac,"ctr":1,"up_us":up_us,"msgs":msgs,"new_fw_field":true})
        .to_string()
}

/// A Wi-Fi node and a BLE node hearing the same drone (different MACs),
/// plus the line types a node prints that are not detections. The node's
/// clock (`up_us`) spans 1.4 s of detections, as a real capture does: the
/// tracker confirms a drone only once it is heard again a second later.
fn recording() -> String {
    recording_at(&[1_000_000, 1_200_000, 2_400_000])
}

/// The same lines with the three detections at the given node times.
fn recording_at(up: &[u64; 3]) -> String {
    [
        r#"{"t":"hello","node":"wifi24","fw":"0.1.0","chip":"esp32s3","proto":1}"#.to_string(),
        "garbage from a reset \u{fffd}".to_string(),
        r#"{"t":"log","lvl":"info","msg":"channel 6"}"#.to_string(),
        rid_line(
            Transport::BleLegacy,
            "c0:11:22:33:44:55",
            -80,
            up[0],
            &[location(INDEPENDENCE.0, INDEPENDENCE.1)],
        ),
        rid_line(
            Transport::WifiBeacon,
            "60:60:1f:00:00:01",
            -60,
            up[1],
            &[basic("SN-CLI-1"), location(INDEPENDENCE.0, INDEPENDENCE.1)],
        ),
        rid_line(
            Transport::BleLegacy,
            "c0:11:22:33:44:55",
            -79,
            up[2],
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
    assert_eq!(t[0]["confirmed"], true, "replayed by its own clock");
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

/// A recording whose detections all carry the same node time is one
/// instant: the drone was heard only once, so it is listed as unconfirmed
/// and never announced (replay follows the recording's clock, not how fast
/// the file was read).
#[test]
fn same_instant_recording_is_unconfirmed_and_not_announced() {
    let run = |args: &[&str]| {
        let mut child = Command::new(BIN)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(recording_at(&[1_000_000; 3]).as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap()
    };
    let text = run(&["--once", "-"]);
    assert!(text.contains("SN-CLI-1"), "still listed: {text}");
    assert!(!text.contains("new drone"), "not announced: {text}");
    assert!(text.contains("? unconfirmed"), "{text}");
    let t = tracks(run(&["--once", "--json", "-"]).as_bytes());
    assert_eq!(t.len(), 1, "{t:?}");
    assert_eq!(t[0]["confirmed"], false);
}

#[test]
fn demo_once() {
    let out = Command::new(BIN)
        .args(["--demo", "--once", "--demo-secs", "3", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let all = tracks(&out.stdout);
    let t: Vec<_> = all.iter().filter(|l| l["t"] == "track").collect();
    // ALPHA, BRAVO; CHARLIE (DroneID + Remote ID, one track), the O4 drone,
    // the undecoded DroneID bursts and DELTA (DJI Wi-Fi).
    assert_eq!(t.len(), 6);
    let charlie = t
        .iter()
        .find(|t| t["ids"][0]["id"] == "DEMO0CHARLIE0003")
        .unwrap();
    assert_eq!(charlie["dji"]["model"], "Mavic 3");
    assert_eq!(charlie["links"][0]["tr"], "WifiBeacon");
    assert!(charlie["dji"]["ocusync"]["n"].as_u64().unwrap() > 1);
    assert!(
        t.iter()
            .any(|t| t["dji"]["key_id"] == "dec00004" && t["loc"].is_null())
    );
    assert!(t.iter().any(|t| t["dji"]["undecoded"] == true));
    let delta = t
        .iter()
        .find(|t| t["ids"][0]["id"] == "DEMO0DELTA000004")
        .unwrap();
    assert_eq!(delta["loc_src"], "DjiWifi");
    // RF emitters and the Wi-Fi drone: their own lines.
    let rf: Vec<_> = all.iter().filter(|l| l["t"] == "rf").collect();
    assert_eq!(rf.len(), 3, "{all:?}");
    assert_eq!(rf[0]["kind"], "AnalogVideo");
    assert_eq!(rf[0]["line_hz"], 15734);
    assert_eq!(rf[2]["alerts"], false);
    let w: Vec<_> = all.iter().filter(|l| l["t"] == "wifidrone").collect();
    assert_eq!(w.len(), 1);
    assert_eq!(w[0]["maker"], "DJI Tello");
    assert!(w[0]["mac"].is_null());
    // No --own-pos: geometry is null (P4), not invented.
    assert!(t.iter().all(|t| t["own"].is_null()));
    // The demo FPV link is listed apart from the drones, its ID hidden.
    let l: Vec<_> = all.iter().filter(|l| l["t"] == "link").collect();
    assert_eq!(l.len(), 1, "{all:?}");
    assert_eq!(l[0]["sys"], "WfbNg");
    assert_eq!(l[0]["ids_shown"], false);
    assert!(l[0]["link_id"].is_null());
    assert!(l[0]["streams"][0]["mac"].is_null());
    // The demo aircraft: their own lines, never tracks.
    let a: Vec<_> = all.iter().filter(|l| l["t"] == "aircraft").collect();
    assert_eq!(a.len(), 3, "{all:?}");
    assert_eq!(a[0]["call"], "DEMOHELI");
    assert_eq!(a[0]["kind"], "Rotorcraft");
    assert!(a[2]["lat"].is_null(), "the third has no position");
    assert_eq!(all.len(), 14);

    // Opting in shows the link ID.
    let out = Command::new(BIN)
        .args([
            "--demo",
            "--once",
            "--demo-secs",
            "3",
            "--json",
            "--show-link-ids",
        ])
        .output()
        .unwrap();
    let all = tracks(&out.stdout);
    let l = all.iter().find(|l| l["t"] == "link").unwrap();
    assert_eq!(l["link_id"], "7505d6");
    assert_eq!(l["streams"][0]["mac"], "57:42:75:05:d6:00");
}

/// A node's `"t":"open"` lines become one open link, kept apart from drones,
/// logged to CSV with the identifier left out.
/// Protocol-4 frames from a node as `{"t":"frame","frame":{...}}` lines:
/// decoded with the link crate's own types and printed in the table.
#[test]
fn frame_lines_from_a_node() {
    let lines = [
        r#"{"t":"frame","frame":{"Status":{"role":"DroneId","can":16,"plan":5}}}"#,
        r#"{"t":"frame","frame":{"DroneId":{"freq":57565,"rssi":-80,"snr_db":6,"uptime_us":1,"record":{"Encrypted":{"key_id":[1,2,3,4]}}}}}"#,
        r#"{"t":"frame","frame":{"DroneId":{"freq":24145,"rssi":-90,"snr_db":2,"uptime_us":2,"record":"Detected"}}}"#,
        r#"{"t":"frame","frame":{"Rf":{"kind":"AnalogVideo","freq":58060,"width_khz":8000,"snr_db":20,"duty_pct":100,"line_hz":15734,"uptime_us":3}}}"#,
        r#"{"t":"frame","frame":{"WifiDrone":{"maker":2,"mac":[144,3,183,1,2,3],"ssid":"Bebop2_123456","channel":6,"rssi":-61,"uptime_us":4}}}"#,
        r#"{"t":"frame","frame":{"Rf":{"kind":"NoSuchKind"}}}"#,
    ]
    .join("\n");
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
        .write_all(lines.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("O4 01020304 5756.5"), "{text}");
    assert!(text.contains("DroneID? 2414.5"), "{text}");
    assert!(text.contains("RADIO SIGNALS"), "{text}");
    assert!(
        text.contains("Analog video") && text.contains("5806.0") && text.contains("15734"),
        "{text}"
    );
    assert!(
        text.contains("WI-FI DRONES") && text.contains("Parrot") && text.contains("Bebop2_123456"),
        "{text}"
    );
    assert!(text.contains("hidden"), "addresses hidden: {text}");
    assert!(text.contains("role DroneId, can 0x10, plan 5"), "{text}");
    assert!(
        text.contains("other frames: 5") && text.contains("unparsed: 1"),
        "{text}"
    );
    assert!(
        text.contains("new RF signal: Analog video 5806.0 MHz"),
        "{text}"
    );
}

#[test]
fn open_link_lines_from_a_node() {
    let dir = std::env::temp_dir().join(format!("rid-view-open-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let csv = dir.join("log.csv");
    let _ = std::fs::remove_file(&csv);
    let lines = [
        r#"{"t":"hello","fw":"node-wifi","role":"Wifi5","proto":2}"#,
        r#"{"t":"open","sys":"WfbNg","id":7669206,"port":0,"dir":"AirToGround","ch":161,"rssi":-60,"mac":"57:42:75:05:d6:00","up_us":1000000,"n":900}"#,
        r#"{"t":"open","sys":"WfbNg","id":7669206,"port":144,"dir":"GroundToAir","ch":161,"rssi":-75,"mac":"57:42:75:05:d6:90","up_us":1100000,"n":20}"#,
        r#"{"t":"open","sys":"OpenHd","id":null,"port":4,"dir":"AirToGround","ch":149,"rssi":-70,"mac":"01:aa:bb:cc:dd:04","up_us":1200000,"n":5}"#,
    ]
    .join("\n");
    let mut child = Command::new(BIN)
        .args(["--once", "--json", "--log"])
        .arg(&csv)
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(lines.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let all = tracks(&out.stdout);
    assert!(
        all.iter().all(|l| l["t"] == "link"),
        "no drone tracks: {all:?}"
    );
    assert_eq!(all.len(), 2);
    assert_eq!(all[0]["pkts"], 920);
    assert_eq!(
        (all[0]["down"].clone(), all[0]["up"].clone()),
        (json!(true), json!(true))
    );
    assert_eq!(all[1]["sys"], "OpenHd");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        !text.contains("7505d6") && !text.contains("57:42"),
        "{text}"
    );

    let log = std::fs::read_to_string(&csv).unwrap();
    let rows: Vec<&str> = log.lines().skip(1).collect();
    assert_eq!(rows.len(), 2, "{log}");
    assert!(rows[0].contains(",L1,,,,,,,WfbNg5G,"), "{log}");
    assert!(!log.contains("7505d6"));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `--adsbee` with a recorded CSBee file: good lines become aircraft, a
/// corrupted line and console text are counted and ignored, and the CSV log
/// gets aircraft rows. (The lines are the `adsb` crate's test fixtures.)
#[test]
fn adsbee_file_input() {
    let dir = std::env::temp_dir().join(format!("rid-view-adsb-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let rec = dir.join("adsbee.txt");
    let csv = dir.join("log.csv");
    let _ = std::fs::remove_file(&csv);
    std::fs::write(
        &rec,
        concat!(
            "INFO: ADSBee 1090 console\r\n",
            "#A:A1B2C3,200007F,N911PH,7700,7,39.95600,-75.16000,1200,1325,270,95,-500,-512,0,0,2,-62,9,3,4,DA00\r\n",
            // One digit changed: refused by the CRC.
            "#A:C0FFEE,3F,UAL123,1200,3,39.87190,-75.24110,1028,1179,85,250,1024,1088,0,0,2,-80,3,0,7,F9\r\n",
            "#S:2,10,20,18,10,8,5,4,10,123456,654321,9F57\r\n",
        ),
    )
    .unwrap();
    let out = Command::new(BIN)
        .args(["--once", "--json", "--own-pos", CITY_HALL, "--log"])
        .arg(&csv)
        .arg("--adsbee")
        .arg(&rec)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let all = tracks(&out.stdout);
    assert_eq!(all.len(), 1, "{all:?}");
    let a = &all[0];
    assert_eq!(a["t"], "aircraft");
    assert_eq!(a["icao"], "A1B2C3");
    assert_eq!(a["emerg"], "emergency");
    assert!(a["own"]["dist_m"].as_f64().unwrap() > 300.0);
    let log = std::fs::read_to_string(&csv).unwrap();
    let rows: Vec<&str> = log.lines().skip(1).collect();
    assert_eq!(rows.len(), 1, "{log}");
    assert!(rows[0].contains(",ICAO-A1B2C3,"), "{log}");
    assert!(
        rows[0].contains(",N911PH,Callsign,Rotorcraft,Adsb1090,"),
        "{log}"
    );

    // The table says what was refused.
    let out = Command::new(BIN)
        .args(["--once", "--adsbee"])
        .arg(&rec)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("AIRCRAFT (ADS-B)"), "{text}");
    assert!(
        text.contains("N911PH") && text.contains("EMERGENCY"),
        "{text}"
    );
    assert!(
        text.contains("bad lines: 1") && text.contains("other text: 1"),
        "{text}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
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
        if seen.len() == 9 {
            break;
        }
    }
    // The two demo aircraft with a position; the third has none to plot.
    // CHARLIE once (DroneID and Remote ID are one track), DELTA without a
    // pilot (the Wi-Fi element has no pilot position); the O4 and
    // undecoded DroneID tracks have no position, so no event.
    let want: std::collections::BTreeSet<String> = [
        "drone-DEMO-ALPHA-0001",
        "pilot-DEMO-ALPHA-0001",
        "drone-DEMO-BRAVO-0002",
        "pilot-DEMO-BRAVO-0002",
        "drone-DEMO0CHARLIE0003",
        "pilot-DEMO0CHARLIE0003",
        "drone-DEMO0DELTA000004",
        "ICAO-DE0001",
        "ICAO-DE0002",
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
    http(addr, &format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\n\r\n"))
}

fn post(addr: &str, path: &str, body: &str) -> (u16, String, Vec<u8>) {
    http(
        addr,
        &format!(
            "POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
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
        &format!("GET / HTTP/1.1\r\nHost: {addr}\r\nIf-None-Match: {etag}\r\n\r\n"),
    );
    assert_eq!(status, 304);
    assert!(body.is_empty());
    assert_eq!(get(addr, "/nope").0, 404);
    assert_eq!(post(addr, "/api/state", "{}").0, 405);

    // The state: the demo drones (six with DroneID), the demo nodes, our position.
    let v = state_until(addr, |v| {
        v["tracks"].as_array().is_some_and(|t| t.len() == 6)
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
    assert_eq!(
        roles,
        [
            json!("Wifi24"),
            json!("Ble"),
            json!("Wifi5"),
            json!("DroneId"),
            json!("Fpv")
        ]
    );
    // Status frames reached the node health.
    assert_eq!(v["nodes"][3]["plan"], 2);
    let v3 = state_until(addr, |v| {
        v["rf"].as_array().is_some_and(|r| r.len() == 3)
            && v["wifi_drones"].as_array().is_some_and(|w| w.len() == 1)
    });
    assert_eq!(v3["wifi_drones"][0]["ssid"], "TELLO-DE0005");
    let v = state_until(addr, |v| {
        v["open_links"].as_array().is_some_and(|l| l.len() == 1)
    });
    // The demo ADSBee: three aircraft after the tracks, and its health.
    let v2 = state_until(addr, |v| {
        v["aircraft"].as_array().is_some_and(|a| a.len() == 3)
    });
    assert_eq!(v2["adsb"]["alive"], true);
    assert_eq!(v2["adsb"]["aircraft"], 3);
    assert_eq!(v2["aircraft"][0]["own"]["low"], true);
    assert_eq!(v["open_links"][0]["t"], "link");
    assert!(v["open_links"][0]["link_id"].is_null());
    assert!(
        v["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| !t["loc"].is_null())
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

/// A page in the user's browser must not be able to move "own position",
/// either by posting from another site or by rebinding a name to 127.0.0.1.
#[test]
fn http_refuses_cross_site_and_rebound_requests() {
    let srv = serve(&[]);
    let addr = &srv.1;
    let body = r#"{"lat":40.0,"lon":-75.5}"#;
    let req = |extra: &str, ct: &str| {
        format!(
            "POST /api/own-pos HTTP/1.1\r\n{extra}Content-Type: {ct}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
    };
    // A form post from another site: simple content type, foreign Origin.
    let evil = req(&format!("Host: {addr}\r\nOrigin: http://evil.example\r\n"), "text/plain");
    assert_eq!(http(addr, &evil).0, 403);
    let evil = req(&format!("Host: {addr}\r\nOrigin: http://evil.example\r\n"), "application/json");
    assert_eq!(http(addr, &evil).0, 403);
    // DNS rebinding: the browser connects to us but the Host is the attacker's name.
    let port = addr.rsplit(':').next().unwrap();
    let rebound = req(&format!("Host: evil.example:{port}\r\n"), "application/json");
    assert_eq!(http(addr, &rebound).0, 403);
    assert_eq!(
        http(addr, &format!("GET /api/state HTTP/1.1\r\nHost: evil.example:{port}\r\n\r\n")).0,
        403
    );
    // Right host, wrong content type.
    let plain = req(&format!("Host: {addr}\r\n"), "text/plain");
    assert_eq!(http(addr, &plain).0, 415);
    // The page's own request (same origin, JSON) still works.
    let ok = req(&format!("Host: {addr}\r\nOrigin: http://{addr}\r\n"), "application/json; charset=utf-8");
    assert_eq!(http(addr, &ok).0, 200);
    // `localhost` on the right port is the same machine.
    let local = req(&format!("Host: localhost:{port}\r\n"), "application/json");
    assert_eq!(http(addr, &local).0, 200);
}
