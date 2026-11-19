//! Fuzz-style tests (crates/fuzzkit) for what rid-view reads from outside:
//! node JSON lines (stdin, a serial port or a recording), the Sniffle
//! sniffer's base64 lines and the adverts in them, BlueZ object paths and
//! service data, the bounded line reader, and its local HTTP server's
//! request head and access check.

use std::io::Cursor;
use std::net::SocketAddr;

use crate::http::{authority_is, check, parse_head};
use crate::input::{LineRead, read_line_bounded};
use crate::{ble, parse, sniffle};

const RID: &str = r#"{"t":"rid","tr":"WifiBeacon","ch":6,"rssi":-60,"mac":"02:00:00:00:00:01","ctr":3,"up_us":123456,"raw":["0212345678901234567890123456789012345678901234567890"],"msgs":[{"BasicId":{"id_type":"SerialNumber","ua_type":"HelicopterOrMultirotor","uas_id":"1581F5FHD00000000001"}},"00112233",{"error":"x","raw":"00"}]}"#;
const OPEN: &str = r#"{"t":"open","sys":"WfbNg","id":7669206,"port":0,"dir":"AirToGround","ch":161,"rssi":-60,"mac":"57:42:75:05:d6:00","up_us":5,"n":1234}"#;
const FRAME: &str = r#"{"t":"frame","frame":{"Rf":{"kind":"AnalogVideo","freq":58060,"width_khz":8000,"snr_db":20,"duty_pct":90,"line_hz":15734,"uptime_us":5000000}}}"#;

#[test]
fn fuzz_node_lines() {
    let seeds: [&[u8]; 5] = [RID.as_bytes(), OPEN.as_bytes(), FRAME.as_bytes(), br#"{"t":"hello","fw":"node-wifi"}"#, br#"{"t":"stats","frames":1}"#];
    let dict: [&[u8]; 8] = [b"\"t\":", b"\"msgs\":", b"\"raw\":", b"null", b"1e400", b"-", b"[", b"{"];
    let cfg = fuzzkit::Config::new().max_len(1500).dict(&dict);
    fuzzkit::run_with("rid-view::parse::line", &cfg, &seeds, 3_000, |bytes| {
        let text = fuzzkit::text(bytes);
        if let parse::Line::Rid(r) = parse::line(&text) {
            assert!(r.msgs.len() < 64);
        }
        let _ = parse::raw_hex(&text);
        let _ = parse::mac(&text);
        // Lines are read with a cap: never more than MAX_LINE kept.
        let mut c = Cursor::new(bytes);
        let mut out = Vec::new();
        let mut n = 0;
        loop {
            match read_line_bounded(&mut c, &mut out, 64).unwrap() {
                LineRead::Eof => break,
                LineRead::Line => assert!(out.len() <= 64),
                LineRead::TooLong => assert!(out.is_empty()),
            }
            n += 1;
            assert!(n <= bytes.len() + 1);
        }
    });
}

/// A packet line: words, type, ts, len|dir, event, rssi, chan|phy, PDU
/// (an ADV_NONCONN_IND with a Remote ID service-data structure).
fn sniffle_seed() -> String {
    let pdu = [0x02u8, 0x0D, 1, 2, 3, 4, 5, 6, 6, 0x16, 0xFA, 0xFF, 0x0D, 0x02, 0x12];
    let mut d = vec![0u8, 0x10];
    d.extend_from_slice(&123u32.to_le_bytes());
    d.extend_from_slice(&(pdu.len() as u16).to_le_bytes());
    d.extend_from_slice(&[0, 0, 0xC4, 37]);
    d.extend_from_slice(&pdu);
    // No padding: the PDU length must match; pick a length that is a whole
    // number of base64 words by trimming the PDU's tail if needed.
    while d.len() % 3 != 0 {
        d.pop();
        let n = d.len() - 12;
        d[6..8].copy_from_slice(&(n as u16).to_le_bytes());
    }
    d[0] = (d.len() / 3) as u8;
    sniffle::b64_encode(&d)
}

#[test]
fn seeds_are_accepted() {
    assert!(matches!(parse::line(RID), parse::Line::Rid(_)));
    assert!(matches!(parse::line(OPEN), parse::Line::Open(_)));
    assert!(matches!(parse::line(FRAME), parse::Line::Frame(_)));
    let s = sniffle_seed();
    assert!(matches!(sniffle::decode_line(s.as_bytes()), Ok(sniffle::Msg::Packet(_))), "{:?}", sniffle::decode_line(s.as_bytes()));
}

#[test]
fn fuzz_sniffle_lines() {
    let seed = sniffle_seed();
    fuzzkit::run_with("rid-view::sniffle::decode_line", &fuzzkit::Config::new().max_len(400), &[seed.as_bytes()], 8_000, |bytes| {
        if let Some(v) = sniffle::b64_decode(bytes) {
            assert_eq!(sniffle::b64_decode(sniffle::b64_encode(&v).as_bytes()), Some(v));
        }
        if let Ok(sniffle::Msg::Packet(p)) = sniffle::decode_line(bytes) {
            for coded in [false, true] {
                if let Some(a) = sniffle::advert(&p, coded) {
                    let (o, i) = (p.body.as_ptr_range(), a.data.as_ptr_range());
                    assert!(a.data.is_empty() || (o.start <= i.start && i.end <= o.end));
                }
            }
        }
    });
}

#[test]
fn fuzz_bluez_inputs() {
    let seeds: [&[u8]; 2] = [b"/org/bluez/hci1/dev_AA_BB_CC_DD_EE_FF", b"0000fffa-0000-1000-8000-00805f9b34fb"];
    fuzzkit::run("rid-view::ble", &seeds, 8_000, |bytes| {
        let text = fuzzkit::text(bytes);
        let _ = ble::mac_from_path(&text);
        let _ = ble::is_rid_uuid(&text);
        let _ = ble::rid_line([2, 0, 0, 0, 0, 1], -60, bytes);
    });
}

#[test]
fn fuzz_http_head() {
    let seeds: [&[u8]; 2] = [
        b"POST /api/own-pos?x=1 HTTP/1.1\r\nHost: 127.0.0.1:8080\r\nOrigin: http://127.0.0.1:8080\r\nContent-Type: application/json\r\nContent-Length: 30",
        b"GET / HTTP/1.1\r\nhost: localhost:8080\r\nIf-None-Match: \"x\"",
    ];
    let dict: [&[u8]; 6] = [b"\r\n", b"Host: ", b"Origin: ", b"Content-Length: ", b"[::1]", b"localhost"];
    let cfg = fuzzkit::Config::new().max_len(400).dict(&dict);
    let bound: SocketAddr = "127.0.0.1:8080".parse().unwrap();
    let any: SocketAddr = "0.0.0.0:8080".parse().unwrap();
    fuzzkit::run_with("rid-view::http::parse_head", &cfg, &seeds, 8_000, |bytes| {
        let text = fuzzkit::text(bytes);
        let (req, _len) = parse_head(&text);
        // The Host value as the parser picks it (the last one wins).
        let host = text.split("\r\n").skip(1).filter_map(|l| l.split_once(':')).filter(|(k, _)| k.eq_ignore_ascii_case("host")).last().map(|(_, v)| v.trim());
        for b in [bound, any] {
            if check(&req, b).is_ok() {
                // Allowed implies a Host naming this server.
                assert!(authority_is(host.unwrap(), b), "{host:?}");
            }
        }
    });
}
