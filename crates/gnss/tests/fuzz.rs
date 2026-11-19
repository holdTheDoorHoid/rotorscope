//! Fuzz-style tests (crates/fuzzkit) for the GNSS byte stream: UBX frames
//! and NMEA / Quectel sentences through `Parser`, then the integrity
//! monitor that consumes its events. The receiver is on a wire the board
//! owns, but a faulty or hostile module (or spoofed sky) shapes the bytes.

use gnss::{Event, Parser, build_frame, integrity::Monitor};

fn nmea(body: &str) -> Vec<u8> {
    let x = body.bytes().fold(0u8, |a, b| a ^ b);
    format!("${body}*{x:02X}\r\n").into_bytes()
}

fn ubx(class: u8, id: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; payload.len() + 8];
    let n = build_frame(&mut out, class, id, payload).unwrap();
    out.truncate(n);
    out
}

fn seeds() -> Vec<Vec<u8>> {
    let mut s = vec![
        b"$GPGGA,092750.000,5321.6802,N,00630.3372,W,1,8,1.03,61.7,M,55.2,M,,*76\r\n".to_vec(),
        b"$GPRMC,083559.00,A,4717.11437,N,00833.91522,E,0.004,77.52,091202,,,A*57\r\n".to_vec(),
        b"$GNGSA,A,3,08,195,194,199,27,07,21,16,09,04,,,1.03,0.67,0.78,1*3E\r\n".to_vec(),
        b"$GPGSV,3,1,11,08,76,353,46,195,68,113,44,194,64,080,42,199,51,162,37,1*5C\r\n".to_vec(),
        b"$GBGSV,4,3,13,57,,,37,01,,,39,02,,,36,60,,,40,1*7B\r\n".to_vec(),
        b"$PAIR001,062,0*3F\r\n".to_vec(),
        nmea("PAIRSPF,1"),
        nmea("PQTMVERNO,LC29HAANR11A03S_RSA,2023/01/01,00:00:00"),
    ];
    // UBX: NAV-PVT (92 bytes), NAV-STATUS, NAV-CLOCK, MON-RF, MON-VER,
    // ACK-ACK and NAK, CFG-VALGET, with plausible fields.
    let mut pvt = vec![0u8; 92];
    pvt[4..6].copy_from_slice(&2026u16.to_le_bytes());
    pvt[6] = 11;
    pvt[7] = 19;
    pvt[11] = 0x07;
    pvt[20] = 3;
    pvt[21] = 0x01;
    pvt[23] = 12;
    pvt[24..28].copy_from_slice(&(-751_652_220i32).to_le_bytes());
    pvt[28..32].copy_from_slice(&399_525_830i32.to_le_bytes());
    s.push(ubx(0x01, 0x07, &pvt));
    s.push(ubx(0x01, 0x03, &[0u8; 16]));
    s.push(ubx(0x01, 0x22, &[0u8; 20]));
    let mut rf = vec![0u8; 4 + 24 * 2];
    rf[1] = 2;
    s.push(ubx(0x0A, 0x38, &rf));
    let mut ver = vec![0u8; 40 + 30 * 2];
    ver[..10].copy_from_slice(b"ROM SPG 5.");
    ver[40..48].copy_from_slice(b"PROTVER=");
    s.push(ubx(0x0A, 0x04, &ver));
    s.push(ubx(0x05, 0x01, &[0x06, 0x8A]));
    s.push(ubx(0x05, 0x00, &[0x06, 0x8A]));
    s.push(ubx(0x06, 0x8B, &[1, 0, 0, 0, 0x01, 0x00, 0x21, 0x30, 0xE8, 0x03]));
    s
}

#[test]
fn fuzz_gnss_stream() {
    let seeds = seeds();
    let refs: Vec<&[u8]> = seeds.iter().map(|s| s.as_slice()).collect();
    let dict: [&[u8]; 6] = [b"$", b"*", b",", b"\r\n", b"\xB5\x62", b"GN"];
    let cfg = fuzzkit::Config::new().max_len(1200).dict(&dict);
    fuzzkit::run_with("gnss::Parser", &cfg, &refs, 6_000, |bytes| {
        let mut p = Parser::new();
        let mut mon = Monitor::new();
        let mut events = Vec::new();
        for &b in bytes {
            if let Some(e) = p.push(b) {
                events.push(e);
            }
        }
        // Byte by byte or in one call: the same events.
        let mut q = Parser::new();
        let again: Vec<Event> = q.feed(bytes).collect();
        assert_eq!(format!("{events:?}"), format!("{again:?}"));
        for (i, e) in events.iter().enumerate() {
            if let Event::Fix(f) = e {
                assert!(f.lat.is_nan() || (-90.0..=90.0).contains(&f.lat), "lat {}", f.lat);
                assert!(f.lon.is_nan() || (-180.0..=180.0).contains(&f.lon), "lon {}", f.lon);
            }
            let _ = mon.on_event(e, p.sats(), 1_000 + i as u64 * 100);
        }
        let _ = p.sky().total_in_view();
        let _ = p.stats();
    });
}

/// The UBX seeds reach the deep paths.
#[test]
fn seeds_are_accepted() {
    let s = seeds();
    let mut p = Parser::new();
    let ev: Vec<Event> = s.iter().flat_map(|b| p.feed(b).collect::<Vec<_>>()).collect();
    let has = |f: fn(&Event) -> bool| ev.iter().any(f);
    assert!(has(|e| matches!(e, Event::Fix(_))), "{ev:?}");
    assert!(has(|e| matches!(e, Event::Ack { .. })) && has(|e| matches!(e, Event::Nak { .. })));
    assert!(has(|e| matches!(e, Event::Quectel(_))), "{ev:?}");
}
