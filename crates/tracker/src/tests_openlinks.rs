//! Open FPV links: kept apart from drones, merged per link, expired, and
//! formatted with their identifiers hidden unless the operator opts in.

extern crate std;
use std::string::String;
use std::vec::Vec;

use crate::openlinks::{MAX_LINK_TRACKS, MAX_STREAMS};
use crate::*;
use link::OpenLinkSighting;
use openlink::{Direction, OpenLink, System as Sys, WFB_DEFAULT_LINK_ID};

fn wfb(port: u8, channel: u8, rssi: i8, packets: u32) -> OpenLinkSighting {
    OpenLinkSighting {
        link: OpenLink {
            system: Sys::WfbNg,
            id: Some(WFB_DEFAULT_LINK_ID),
            port,
            direction: if port < 128 {
                Direction::AirToGround
            } else {
                Direction::GroundToAir
            },
            tx: [0x57, 0x42, 0x75, 0x05, 0xD6, port],
        },
        channel,
        rssi,
        uptime_us: 1,
        packets,
    }
}

fn other(system: Sys, id: Option<u32>, port: u8, dir: Direction) -> OpenLinkSighting {
    OpenLinkSighting {
        link: OpenLink {
            system,
            id,
            port,
            direction: dir,
            tx: [0x02, 0x11, 0x22, 0x33, 0x44, port],
        },
        channel: 149,
        rssi: -70,
        uptime_us: 1,
        packets: 10,
    }
}

fn s(buf: &[u8], n: usize) -> String {
    String::from_utf8(buf[..n].to_vec()).unwrap()
}

fn shown() -> Config {
    Config {
        show_link_ids: true,
        ..Config::default()
    }
}

#[test]
fn links_never_touch_drone_tracks() {
    let mut tr: Tracker = Tracker::default();
    tr.ingest_open_link(2, &wfb(0, 161, -60, 100), 1_000);
    assert_eq!(tr.len(), 0);
    assert_eq!(tr.open_links().len(), 1);
    assert_eq!(tr.take_new_drone(10_000), None, "no drone alert for a link");
    assert_eq!(tr.active_count(1_000), 0);
    assert_eq!(tr.active_link_count(1_000), 1);
}

#[test]
fn streams_of_one_link_are_one_track() {
    let mut tr: Tracker = Tracker::default();
    let a = tr.ingest_open_link(2, &wfb(0, 161, -60, 100), 1_000);
    let b = tr.ingest_open_link(2, &wfb(16, 161, -61, 5), 1_100);
    let c = tr.ingest_open_link(2, &wfb(144, 161, -75, 3), 1_200);
    // The same stream again: its count is replaced by the newer running total.
    let d = tr.ingest_open_link(2, &wfb(0, 161, -59, 900), 2_000);
    assert!(a == b && b == c && c == d);
    let t = &tr.open_links()[0];
    assert_eq!(t.streams.len(), 3);
    assert_eq!(t.packets(), 900 + 5 + 3);
    assert!(t.heard(Direction::AirToGround) && t.heard(Direction::GroundToAir));
    assert_eq!(t.sightings, 4);
    assert_eq!((t.first_seen_ms, t.last_seen_ms), (1_000, 2_000));
    assert_eq!(t.latest().unwrap().rssi, -59);
    // The same stream from a second node (another band) is kept apart.
    tr.ingest_open_link(0, &wfb(0, 11, -80, 7), 2_100);
    assert_eq!(tr.open_links()[0].streams.len(), 4);
}

#[test]
fn links_are_keyed_by_system_and_id() {
    let mut tr: Tracker = Tracker::default();
    tr.ingest_open_link(2, &wfb(0, 161, -60, 1), 0);
    let mut other_link = wfb(0, 161, -60, 1);
    other_link.link.id = Some(0x123456);
    tr.ingest_open_link(2, &other_link, 0);
    // OpenHD has no ID: air and ground units are one entry.
    tr.ingest_open_link(2, &other(Sys::OpenHd, None, 4, Direction::AirToGround), 0);
    tr.ingest_open_link(2, &other(Sys::OpenHd, None, 4, Direction::GroundToAir), 0);
    tr.ingest_open_link(
        2,
        &other(Sys::RubyFpv, Some(77), 0x0F, Direction::AirToGround),
        0,
    );
    tr.ingest_open_link(
        2,
        &other(Sys::RubyFpv, Some(77), 0x0E, Direction::GroundToAir),
        0,
    );
    let kinds: Vec<_> = tr
        .open_links()
        .iter()
        .map(|t| (t.system, t.link_id, t.streams.len()))
        .collect();
    assert_eq!(
        kinds,
        [
            (Sys::WfbNg, Some(WFB_DEFAULT_LINK_ID), 1),
            (Sys::WfbNg, Some(0x123456), 1),
            (Sys::OpenHd, None, 2),
            (Sys::RubyFpv, Some(77), 2),
        ]
    );
}

#[test]
fn stream_list_and_table_have_bounds() {
    let mut tr: Tracker = Tracker::default();
    for port in 0..=MAX_STREAMS as u8 {
        tr.ingest_open_link(2, &wfb(port, 161, -60, 1), u64::from(port));
    }
    let ports: Vec<u8> = tr.open_links()[0].streams.iter().map(|s| s.port).collect();
    // Port 0 was heard least recently and made room for the seventh.
    assert_eq!(ports, [6, 1, 2, 3, 4, 5]);

    let mut tr: Tracker = Tracker::default();
    for i in 0..=MAX_LINK_TRACKS as u32 {
        let mut l = wfb(0, 161, -60, 1);
        l.link.id = Some(i);
        tr.ingest_open_link(2, &l, u64::from(i) * 10);
    }
    assert_eq!(tr.open_links().len(), MAX_LINK_TRACKS);
    assert!(
        tr.open_links().iter().all(|t| t.link_id != Some(0)),
        "oldest evicted"
    );
}

#[test]
fn links_go_stale_then_expire() {
    let mut tr: Tracker = Tracker::default();
    tr.ingest_open_link(2, &wfb(0, 161, -60, 1), 1_000);
    let ctx = tr.context(60_999, None);
    assert_eq!(ctx.link_state(&tr.open_links()[0]), TrackState::Active);
    let ctx = tr.context(61_000, None);
    assert_eq!(ctx.link_state(&tr.open_links()[0]), TrackState::Stale);
    assert_eq!(tr.active_link_count(61_000), 0);
    tr.expire(600_999);
    assert_eq!(tr.open_links().len(), 1);
    tr.expire(601_000);
    assert!(tr.open_links().is_empty());
}

/// The project's privacy rule, made concrete: by default no output carries
/// the link ID, in any spelling, or the address that embeds it.
#[test]
fn identifiers_hidden_by_default() {
    let mut tr: Tracker = Tracker::default();
    tr.ingest_open_link(2, &wfb(0, 161, -60, 100), 1_000);
    tr.ingest_open_link(
        2,
        &other(Sys::RubyFpv, Some(1_234_567), 15, Direction::AirToGround),
        1_000,
    );
    tr.ingest_open_link(
        2,
        &other(Sys::DroneBridge, Some(200), 3, Direction::AirToGround),
        1_000,
    );
    let ctx = tr.context(1_500, Some(1_794_232_987_250));
    let mut buf = [0u8; 2048];
    let mut all = String::new();
    for t in tr.open_links() {
        let n = json::link_line(t, &ctx, &mut buf).unwrap();
        all += &s(&buf, n);
        let n = csv::link_row(t, &ctx, &mut buf).unwrap();
        all += &s(&buf, n);
    }
    let hub = json::HubStatus {
        own_source: json::OwnSource::Gps,
        manual: None,
        gps: json::GpsStatus {
            state: json::GpsState::Absent,
            sats: None,
        },
        recording: false,
        nodes: &[],
        adsb: None,
        fix_held: false,
        utc_held: false,
    };
    let mut big = std::vec![0u8; json::state_max_len(0, 3, 0, 0)];
    let n = json::state(
        tr.tracks(),
        tr.open_links(),
        tr.aircraft(),
        &ctx,
        &hub,
        &mut big,
    )
    .unwrap();
    all += &s(&big, n);
    for leak in [
        "7505d6",
        "7505D6",
        "7669206",
        "57:42",
        "1234567",
        "02:11:22",
        "link_id\":\"",
        ",200,",
    ] {
        assert!(!all.contains(leak), "{leak} leaked:\n{all}");
    }
    assert!(all.contains("\"ids_shown\":false"));
    assert!(all.contains("\"sys\":\"WfbNg\""));
}

#[test]
fn link_line_golden() {
    let mut tr: Tracker = Tracker::new(shown());
    tr.ingest_open_link(2, &wfb(0, 161, -60, 100), 1_000);
    tr.ingest_open_link(2, &wfb(144, 161, -72, 9), 1_400);
    let ctx = tr.context(1_500, None);
    let mut buf = [0u8; 1024];
    let n = json::link_line(&tr.open_links()[0], &ctx, &mut buf).unwrap();
    let want = concat!(
        r#"{"t":"link","id":1,"state":"active","sys":"WfbNg","label":"wfb-ng/OpenIPC","ids_shown":true,"#,
        r#""link_id":"7505d6","first_ms":1000,"last_ms":1400,"age_ms":100,"rssi":-72,"ch":161,"band":"5","#,
        r#""down":true,"up":true,"pkts":109,"streams":["#,
        r#"{"port":0,"kind":"video","dir":"AirToGround","ch":161,"band":"5","rssi":-60,"n":100,"mac":"57:42:75:05:d6:00","age_ms":500,"node":2},"#,
        r#"{"port":144,"kind":"mavlink","dir":"GroundToAir","ch":161,"band":"5","rssi":-72,"n":9,"mac":"57:42:75:05:d6:90","age_ms":100,"node":2}]}"#,
        "\n"
    );
    assert_eq!(s(&buf, n), want);
    serde_json::from_str::<serde_json::Value>(&s(&buf, n)).unwrap();

    // The same with the default (hidden) setting.
    let mut hidden: Tracker = Tracker::default();
    hidden.ingest_open_link(2, &wfb(0, 161, -60, 100), 1_000);
    let ctx = hidden.context(1_500, None);
    let n = json::link_line(&hidden.open_links()[0], &ctx, &mut buf).unwrap();
    let v: serde_json::Value = serde_json::from_str(&s(&buf, n)).unwrap();
    assert_eq!(v["link_id"], serde_json::Value::Null);
    assert_eq!(v["streams"][0]["mac"], serde_json::Value::Null);
    assert_eq!(v["ids_shown"], false);
    // Too small a buffer is an error, not a truncated line.
    assert_eq!(
        json::link_line(&hidden.open_links()[0], &ctx, &mut buf[..50]),
        Err(FormatError::BufferTooSmall)
    );
}

#[test]
fn ruby_and_dronebridge_ids_are_decimal() {
    let mut tr: Tracker = Tracker::new(shown());
    tr.ingest_open_link(
        2,
        &other(Sys::RubyFpv, Some(u32::MAX), 15, Direction::AirToGround),
        0,
    );
    tr.ingest_open_link(
        2,
        &other(Sys::DroneBridge, Some(7), 3, Direction::GroundToAir),
        0,
    );
    let ctx = tr.context(0, None);
    let mut buf = [0u8; 1024];
    let ids: Vec<serde_json::Value> = tr
        .open_links()
        .iter()
        .map(|t| {
            let n = json::link_line(t, &ctx, &mut buf).unwrap();
            serde_json::from_str::<serde_json::Value>(&s(&buf, n)).unwrap()["link_id"].clone()
        })
        .collect();
    assert_eq!(ids, ["4294967295", "7"]);
}

#[test]
fn link_worst_case_fits() {
    let mut tr: Tracker = Tracker::new(shown());
    for port in 0..MAX_STREAMS as u8 {
        let mut l = other(
            Sys::DroneBridge,
            Some(u32::MAX),
            250 - port,
            Direction::Unknown,
        );
        l.link.tx = [0xFF; 6];
        l.channel = 255;
        l.rssi = -128;
        l.packets = u32::MAX;
        tr.ingest_open_link(255, &l, u64::MAX / 4 + u64::from(port));
    }
    // A long label and wfb-ng stream kinds too: swap in the worst of each.
    let ctx = tr.context(u64::MAX / 2, None);
    let mut buf = [0u8; 4096];
    let n = json::link_line(&tr.open_links()[0], &ctx, &mut buf).unwrap();
    std::println!("worst-case link line: {n} bytes");
    assert!(
        n + 64 <= json::MAX_LINK_LINE,
        "{n}: keep headroom for `kind` and the label"
    );
    let ctx = tr.context(u64::MAX / 2, Some(u64::MAX / 4));
    tr.set_own_position(-89.123_456_7, -179.123_456_7, Some(-12_345.6), u64::MAX / 2);
    let ctx2 = tr.context(u64::MAX / 2, ctx.utc_ms);
    assert!(csv::link_row(&tr.open_links()[0], &ctx2, &mut buf).unwrap() <= csv::MAX_ROW);
}

#[test]
fn csv_link_rows() {
    let mut tr: Tracker = Tracker::default();
    tr.ingest_open_link(2, &wfb(0, 161, -60, 100), 4_000);
    tr.ingest_open_link(0, &wfb(16, 6, -80, 3), 4_100);
    tr.set_own_position(39.952_583, -75.165_222, Some(40.0), 4_900);
    let ctx = tr.context(5_000, Some(1_794_232_987_250));
    let mut buf = [0u8; 512];
    let n = csv::link_row(&tr.open_links()[0], &ctx, &mut buf).unwrap();
    let row = s(&buf, n);
    assert_eq!(
        row,
        "2026-11-09T14:03:07.250Z,5000,L1,39.9525830,-75.1652220,40.0,,,,WfbNg|WfbNg5G,,,,,,,,-80,,,,,\n"
    );
    assert_eq!(row.matches(',').count(), csv::HEADER.matches(',').count());

    let mut tr: Tracker = Tracker::new(shown());
    tr.ingest_open_link(2, &wfb(0, 161, -60, 100), 4_000);
    let ctx = tr.context(5_000, None);
    let n = csv::link_row(&tr.open_links()[0], &ctx, &mut buf).unwrap();
    assert_eq!(
        s(&buf, n),
        ",5000,L1,,,,7505d6,link_id,,WfbNg5G,,,,,,,,-60,,,,,\n"
    );
}

#[test]
fn state_lists_links_before_tracks() {
    let mut tr: Tracker = Tracker::default();
    tr.ingest_open_link(2, &wfb(0, 161, -60, 100), 1_000);
    tr.ingest_open_link(
        2,
        &other(Sys::OpenHd, None, 4, Direction::AirToGround),
        1_000,
    );
    let ctx = tr.context(1_500, None);
    let hub = json::HubStatus {
        own_source: json::OwnSource::Gps,
        manual: None,
        gps: json::GpsStatus {
            state: json::GpsState::Absent,
            sats: None,
        },
        recording: false,
        nodes: &[],
        adsb: None,
        fix_held: false,
        utc_held: false,
    };
    let mut buf = std::vec![0u8; json::state_max_len(0, 2, 0, 0)];
    let n = json::state(
        tr.tracks(),
        tr.open_links(),
        tr.aircraft(),
        &ctx,
        &hub,
        &mut buf,
    )
    .unwrap();
    let text = s(&buf, n);
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["open_links"].as_array().unwrap().len(), 2);
    assert_eq!(v["open_links"][1]["sys"], "OpenHd");
    assert_eq!(v["tracks"].as_array().unwrap().len(), 0);
    // Each link is exactly its link_line, so a server can stream them.
    let mut line = [0u8; 1024];
    let ln = json::link_line(&tr.open_links()[0], &ctx, &mut line).unwrap();
    assert!(text.contains(&s(&line, ln - 1)));
    assert!(text.ends_with("}],\"tracks\":[],\"aircraft\":[]}\n"));
    for len in 0..n {
        assert_eq!(
            json::state(
                tr.tracks(),
                tr.open_links(),
                tr.aircraft(),
                &ctx,
                &hub,
                &mut buf[..len]
            ),
            Err(FormatError::BufferTooSmall),
            "len {len}"
        );
    }
}
