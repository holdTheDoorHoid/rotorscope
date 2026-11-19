//! Fuzz-style tests (crates/fuzzkit) for the hub's text and byte readers in
//! `tracker`: the SITE.TXT site file (line splitter, line parser, writers),
//! the phone's own-position JSON, the CTA-2063 serial check, and Remote ID
//! bytes decoded and merged into tracks.

use rid::message::{decode, raw_messages};
use rid::Transport;
use tracker::site::{self, Lines, SiteLine, SiteState};
use tracker::{Config, Observation, Tracker, json, spoof};

const SITE: &str = "# site file\n\
NODE 02:00:00:00:00:1A  39.9521 -75.1638  roof # north\n\
node 02-00-00-00-00-1a clear\n\
node all here\n\
mute evil_twin source=02:11:22:33:44:55 ssid=Cafe WiFi Guest \n\
mute deauth source=02:00:00:00:00:01 target=0a:00:00:00:00:02\n\
mute control_contested target=00:00:12:34:56:78 mhz=915.5\n\
unmute all\n\
control ids on\n\
control content off\n\
home 39.95210 -75.16380 12.5 # roof\n\
home clear\n\
theme dark\n";

#[test]
fn fuzz_site_file() {
    let lines: Vec<&[u8]> = SITE.lines().map(str::as_bytes).collect();
    let mut seeds = lines.clone();
    seeds.push(SITE.as_bytes());
    let dict: [&[u8]; 10] = [b"node ", b"mute ", b"home ", b"control ", b"theme ", b"source=", b"target=", b"ssid=", b"mhz=", b"\n"];
    let cfg = fuzzkit::Config::new().max_len(1200).dict(&dict);
    fuzzkit::run_with("tracker::site", &cfg, &seeds, 6_000, |bytes| {
        // Whole file, in two pieces, as the SD card reader hands it over.
        let mut st = SiteState::default();
        let mut l = Lines::default();
        let cut = bytes.len() / 3;
        let mut refused = 0;
        site::read_into(&mut st, &mut l, &bytes[..cut], |_, _| refused += 1);
        site::read_into(&mut st, &mut l, &bytes[cut..], |_, _| refused += 1);
        site::finish_into(&mut st, &mut l, |_, _| refused += 1);
        assert!(st.used + st.ignored <= u32::try_from(bytes.len()).unwrap() + 1);
        // One line: what parses and is written back parses the same.
        let text = fuzzkit::text(bytes);
        let line = text.lines().next().unwrap_or("");
        match site::parse_line(line) {
            Ok(SiteLine::Mute(m)) => {
                let mut w = String::new();
                site::write_mute(&mut w, &m).unwrap();
                assert_eq!(site::parse_line(&w), Ok(SiteLine::Mute(m)), "{w:?}");
            }
            Ok(SiteLine::NodeAt { mac, lat, lon }) => {
                assert!((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon), "{lat} {lon}");
                let mut w = String::new();
                site::write_node(&mut w, &mac, lat, lon).unwrap();
                assert!(matches!(site::parse_line(&w), Ok(SiteLine::NodeAt { mac: m, .. }) if m == mac), "{w:?}");
            }
            Ok(SiteLine::Home { lat, lon, alt }) => {
                assert!((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon));
                assert!(alt.is_none_or(f64::is_finite));
            }
            _ => {}
        }
        let _ = site::parse_mac(line);
        let _ = site::kind_from_name(line);
    });
}

#[test]
fn fuzz_own_position_json() {
    let seeds: [&[u8]; 4] = [br#"{"lat":39.9526,"lon":-75.1652}"#, br#"{"lat":39.9526,"lon":-75.1652,"alt":null}"#, br#"{"lat":1,"lon":2,"alt":12.5}"#, br#"{"clear":true}"#];
    let dict: [&[u8]; 6] = [b"\"lat\":", b"\"lon\":", b"\"alt\":", b"1e308", b"-", b"NaN"];
    let cfg = fuzzkit::Config::new().max_len(200).dict(&dict);
    fuzzkit::run_with("tracker::json::parse_own_pos", &cfg, &seeds, 8_000, |bytes| {
        if let Ok(json::OwnPosRequest::Set(p)) = json::parse_own_pos(bytes) {
            assert!((-90.0..=90.0).contains(&p.lat) && (-180.0..=180.0).contains(&p.lon), "{p:?}");
            assert!(p.alt.is_none_or(f64::is_finite), "{p:?}");
        }
        let _ = spoof::cta2063_check(bytes);
    });
}

#[test]
fn fuzz_remote_id_into_tracks() {
    // A message pack from rid's own encoder (one of each kind).
    let mut seeds: Vec<Vec<u8>> = Vec::new();
    for t in [0u8, 1, 2, 3, 4, 5] {
        let mut m = [0u8; 25];
        m[0] = (t << 4) | 2;
        seeds.push(m.to_vec());
    }
    let mut pack = vec![0xF2, 25, 6];
    for s in &seeds {
        pack.extend_from_slice(s);
    }
    seeds.push(pack);
    let refs: Vec<&[u8]> = seeds.iter().map(|s| s.as_slice()).collect();
    fuzzkit::run_with("tracker::ingest_messages", &fuzzkit::Config::new().max_len(260), &refs, 3_000, |bytes| {
        let mut t: Tracker<8> = Tracker::new(Config::default());
        let Ok(raw) = raw_messages(bytes) else { return };
        let msgs: Vec<_> = raw.filter_map(|m| decode(m).ok()).collect();
        for k in 0..3u64 {
            let obs = Observation { node: 1, transport: Transport::BleLegacy, channel: 37, rssi: -60, mac: [0xC0, 1, 2, 3, 4, 5], counter: k as u8, rx_ms: 1_000 + k * 500 };
            let _ = t.ingest_messages(&obs, &msgs, 0);
        }
        let _ = t.take_new_drone(3_000);
    });
}
