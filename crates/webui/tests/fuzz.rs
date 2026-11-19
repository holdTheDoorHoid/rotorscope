//! Fuzz-style tests (crates/fuzzkit) for the web head's request rules (the
//! header values come from any phone on the network), the page's ETag
//! check, and the hotspot salt record and partition-table search. The HTTP
//! head itself is parsed by picoserve in firmware/hub, outside this repo's
//! crates.

use core::net::Ipv4Addr;

use webui::hotspot::{self, Stored};
use webui::request::{self, Access, Headers, Verdict};

const OWN: Ipv4Addr = Ipv4Addr::new(192, 168, 4, 1);

/// Header text: fields separated by a newline, in the order method, path,
/// Host, Origin, Content-Type, If-None-Match.
#[test]
fn fuzz_request_rules() {
    let seeds: [&[u8]; 3] = [
        b"POST\n/api/own-pos\n192.168.4.1\nhttp://192.168.4.1\napplication/json; charset=utf-8\n*",
        b"GET\n/\n192.168.4.1:80\n\n\nW/\"abc\"",
        b"POST\n/api/cot\nevil.example\nhttps://192.168.4.1\ntext/plain\n",
    ];
    let dict: [&[u8]; 7] = [b"192.168.4.1", b":80", b"http://", b"application/json", b"\n", b"/api/", b" \t"];
    let cfg = fuzzkit::Config::new().max_len(200).dict(&dict);
    fuzzkit::run_with("webui::request::check", &cfg, &seeds, 20_000, |bytes| {
        let text = fuzzkit::text(bytes);
        let mut f = text.split('\n');
        let (method, path) = (f.next().unwrap_or(""), f.next().unwrap_or(""));
        let host = f.next();
        let origin = f.next().filter(|s| !s.is_empty());
        let ct = f.next().filter(|s| !s.is_empty());
        let _ = webui::etag_matches(f.next().unwrap_or(""));
        let access = request::route_access(method, path).unwrap_or(Access::Write);
        for count in [0, 1, 2] {
            let h = Headers { host, host_count: count, origin, content_type: ct };
            for writes in [false, true] {
                let v = request::check(Some(OWN), access, writes, &h);
                if v == Verdict::Allow {
                    // Allowed implies: exactly one Host naming this hub...
                    assert_eq!(count, 1);
                    assert!(request::host_is(OWN, host.unwrap()));
                    if access == Access::Write {
                        // ...and for a write: writes on, JSON, no foreign Origin.
                        assert!(writes);
                        assert!(request::is_json(ct.unwrap()));
                        assert!(origin.is_none_or(|o| request::origin_is(OWN, o)));
                    }
                }
                assert_eq!(request::check(None, access, writes, &h), Verdict::WrongHost);
            }
        }
    });
}

#[test]
fn fuzz_hotspot_record_and_table() {
    let rec = hotspot::encode_record(&[7, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);
    let mut table = Vec::new();
    for (t, s, off, len, name) in [(1u8, 2u8, 0x9000u32, 0x6000u32, "nvs"), (1, 1, 0xF000, 0x1000, "phy_init"), (0, 0, 0x10000, 0x3F0000, "factory")] {
        let mut e = [0u8; 32];
        e[..4].copy_from_slice(&[0xAA, 0x50, t, s]);
        e[4..8].copy_from_slice(&off.to_le_bytes());
        e[8..12].copy_from_slice(&len.to_le_bytes());
        e[12..12 + name.len()].copy_from_slice(name.as_bytes());
        table.extend_from_slice(&e);
    }
    table.extend_from_slice(&[0xFF; 32]);
    let seeds: [&[u8]; 2] = [&rec, &table];
    fuzzkit::run("webui::hotspot", &seeds, 10_000, |bytes| {
        let mut r = [0xFFu8; hotspot::RECORD_LEN];
        let n = bytes.len().min(r.len());
        r[..n].copy_from_slice(&bytes[..n]);
        if let Stored::Salt(s) = hotspot::decode_record(&r) {
            assert!(hotspot::salt_usable(&s));
            assert_eq!(hotspot::encode_record(&s), r);
        }
        let entries = bytes.chunks_exact(32).map(|c| <[u8; 32]>::try_from(c).unwrap());
        if let Some(sector) = hotspot::salt_sector(entries) {
            assert_eq!(sector % hotspot::SECTOR, 0);
        }
    });
}
