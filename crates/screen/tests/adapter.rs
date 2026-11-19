//! `from_tracker`: a real tracker, fed decoded Remote ID messages, fills a
//! UiState that renders.

use rid::Transport;
use rid::message::*;
use screen::from_tracker::fill_from_tracker;
use screen::*;
use tracker::{Observation, Tracker};

// Philadelphia City Hall (the unit) and Independence Hall (the drone).
const CITY_HALL: (f64, f64) = (39.952_583, -75.165_222);
const INDEPENDENCE: (f64, f64) = (39.948_889, -75.150_000);

fn obs(transport: Transport, channel: u8, rssi: i8, mac: [u8; 6], rx_ms: u64) -> Observation {
    Observation {
        node: 0,
        transport,
        channel,
        rssi,
        mac,
        counter: 1,
        rx_ms,
    }
}

fn location(lat: f64, lon: f64) -> Message {
    Message::Location(Location {
        status: Status::Airborne,
        height_type: HeightType::AboveTakeoff,
        direction: Some(90.0),
        speed_horizontal: Some(5.25),
        speed_vertical: Some(-1.0),
        position: Some(LatLon { lat, lon }),
        altitude_pressure: Some(130.0),
        altitude_geodetic: Some(140.0),
        height: Some(50.0),
        horizontal_accuracy: HorizontalAccuracy::M3,
        vertical_accuracy: VerticalAccuracy::M10,
        baro_accuracy: VerticalAccuracy::M10,
        speed_accuracy: SpeedAccuracy::Mps1,
        timestamp: None,
        timestamp_accuracy: None,
    })
}

#[test]
fn summarises_aircraft() {
    use screen::from_tracker::aircraft_summary;
    let mut tr: Tracker = Tracker::default();
    tr.set_own_position(CITY_HALL.0, CITY_HALL.1, Some(10.0), 0);
    let report = |icao: u32, north_m: f64, baro_ft: i32, call: &str| adsb::Aircraft {
        source: adsb::Source::ModeS,
        icao,
        flags: 0xff | 1 << adsb::flag::UPDATED_POSITION,
        callsign: adsb::Callsign::new(call).unwrap(),
        squawk: None,
        category: 7,
        lat_deg: CITY_HALL.0 + north_m / 111_000.0,
        lon_deg: CITY_HALL.1,
        baro_alt_ft: baro_ft,
        gnss_alt_ft: baro_ft,
        direction_deg: 0.0,
        speed_kt: 90,
        baro_vrate_fpm: 0,
        gnss_vrate_fpm: 0,
        uat_emergency: 0,
        nicnac: 0,
        acdims: 0,
        version: 2,
        signal_dbm: -70,
        signal_quality: 5,
        frames_per_s: 2,
        es_frames_per_s: 2,
    };
    tr.ingest_aircraft(&report(0xA1B2C3, 1_500.0, 1_200, "N911PH"), 0);
    tr.ingest_aircraft(&report(0xC0FFEE, 9_000.0, 9_000, ""), 0);
    let ctx = tr.context(1_000, None);
    let a = aircraft_summary(tr.aircraft(), &ctx);
    assert_eq!((a.total, a.active, a.low_nearby), (2, 2, 1));
    let n = a.nearest.unwrap();
    assert_eq!(n.name.as_str(), "N911PH");
    assert_eq!(n.alt_ft, Some(1_200));
    assert!((n.distance_m - 1_500.0).abs() < 20.0);
    // Aircraft never become drones on the screen.
    let mut st = UiState::default();
    fill_from_tracker(&tr, &ctx, &mut st);
    assert!(st.drones.is_empty());
    // Stale ones are counted but not active.
    let ctx = tr.context(1_000 + tracker::aircraft::AIRCRAFT_STALE_MS, None);
    let a = aircraft_summary(tr.aircraft(), &ctx);
    assert_eq!((a.total, a.active, a.low_nearby), (2, 0, 0));
    assert!(a.nearest.is_none());
}

#[test]
fn fills_from_a_tracker() {
    let mut t: Tracker<8> = Tracker::default();
    t.set_own_position(CITY_HALL.0, CITY_HALL.1, Some(40.0), 1_000);

    let id = Message::BasicId(BasicId {
        id_type: IdType::SerialNumber,
        ua_type: UaType::HelicopterOrMultirotor,
        uas_id: FixedStr::new(b"1581F5FKD229400D").unwrap(),
    });
    let self_id = Message::SelfId(SelfId {
        desc_type: DescType::Text,
        description: FixedStr::new(b"Bench test").unwrap(),
    });
    let wifi = [0x60, 0x60, 0x1f, 0, 0, 1];
    let ble = [0xc0, 0x11, 0x22, 0x33, 0x44, 0x55];
    let loc = location(INDEPENDENCE.0, INDEPENDENCE.1);
    t.ingest_messages(
        &obs(Transport::WifiBeacon, 149, -61, wifi, 2_000),
        &[id.clone(), loc, self_id],
        0,
    );
    t.ingest_messages(&obs(Transport::BleLongRange, 37, -83, ble, 2_500), &[id], 0);
    // A second transmitter with no Basic ID yet: keyed by its MAC.
    t.ingest_messages(
        &obs(Transport::BleLegacy, 38, -90, [0xd0, 1, 2, 3, 4, 5], 2_600),
        &[location(39.95, -75.16)],
        0,
    );

    let mut st = UiState {
        gps: Gps {
            fix: Fix::Fix3D,
            sats: 9,
            ..Gps::default()
        },
        ..UiState::default()
    };
    fill_from_tracker(&t, &t.context(3_000, Some(1_000_000)), &mut st);

    assert_eq!(st.total_drones, 2);
    assert_eq!(st.utc_ms, Some(1_000_000));
    let d = st
        .drones
        .iter()
        .find(|d| d.id_kind == IdKind::Serial)
        .unwrap();
    assert_eq!(d.id.as_str(), "1581F5FKD229400D");
    assert_eq!(d.ua, UaKind::Multirotor);
    assert_eq!(d.self_id.as_str(), "Bench test");
    let pos = d.drone.unwrap();
    assert!((pos.distance_m - 1363.9).abs() < 1.0, "{}", pos.distance_m);
    assert!((pos.bearing_deg - 107.5).abs() < 0.5, "{}", pos.bearing_deg);
    assert_eq!(d.above_you_m, Some(100.0));
    // Channel 149 is 5 GHz: shown as "5", not "W".
    assert!(d.radios.contains(Radio::Wifi5));
    assert!(!d.radios.contains(Radio::WifiBeacon));
    assert!(d.radios.contains(Radio::BleLongRange));
    assert_eq!(d.rssi[Radio::Wifi5.index()], Some(-61));
    assert_eq!(d.rssi[Radio::BleLongRange.index()], Some(-83));

    let m = st.drones.iter().find(|d| d.id_kind == IdKind::Mac).unwrap();
    assert_eq!(m.id.as_str(), "d0:01:02:03:04:05");

    // Fix too old for the tracker: the screen switches to the P4 layout.
    fill_from_tracker(&t, &t.context(60_000, None), &mut st);
    assert_eq!(st.gps.fix, Fix::None);
    assert!(st.drones.iter().all(|d| d.drone.is_none()));
}

/// Protocol 4: DroneID tracks, RF emitters and Wi-Fi drones reach the
/// screen model.
#[test]
fn fills_droneid_and_signals() {
    use link::{DroneIdRecord, DroneIdSighting, RfKind, RfSighting, WifiDroneSighting};
    let mut tr: Tracker = Tracker::default();
    let rad = |d: f64| (d.to_radians() * 1e7).round() as i32;
    let mut serial = [0u8; 16];
    serial.copy_from_slice(b"TEST0SYNTH000001");
    tr.ingest_droneid(
        2,
        &DroneIdSighting {
            freq: 57565,
            rssi: -70,
            snr_db: 12,
            uptime_us: 0,
            record: DroneIdRecord::Flight {
                serial,
                lat_raw: rad(INDEPENDENCE.0),
                lon_raw: rad(INDEPENDENCE.1),
                altitude_raw: 328,
                height_raw: 164,
                v_north: 0,
                v_east: 0,
                v_up: 0,
                yaw_raw: 0,
                pilot_lat_raw: 0,
                pilot_lon_raw: 0,
                home_lat_raw: 0,
                home_lon_raw: 0,
                product_type: 68,
            },
        },
        1_000,
    );
    tr.ingest_droneid(
        2,
        &DroneIdSighting {
            freq: 24145,
            rssi: -85,
            snr_db: 5,
            uptime_us: 0,
            record: DroneIdRecord::Encrypted {
                key_id: [1, 2, 3, 4],
            },
        },
        1_000,
    );
    for (kind, f, snr) in [
        (RfKind::Energy, 24370, 30),
        (RfKind::AnalogVideo, 58060, 20),
    ] {
        let s = RfSighting {
            kind,
            freq: f,
            width_khz: 0,
            snr_db: snr,
            duty_pct: 90,
            line_hz: 0,
            uptime_us: 0,
        };
        tr.ingest_rf(3, &s, 1_000);
    }
    tr.ingest_wifi_drone(
        0,
        &WifiDroneSighting {
            maker: 1,
            mac: [0, 1, 2, 3, 4, 5],
            ssid: heapless::String::try_from("TELLO-\u{e9}5A1B2").unwrap(),
            channel: 6,
            rssi: -60,
            uptime_us: 0,
        },
        1_000,
    );
    tr.set_own_position(CITY_HALL.0, CITY_HALL.1, None, 1_000);
    let mut st = UiState::default();
    fill_from_tracker(&tr, &tr.context(1_500, None), &mut st);

    let dji = st
        .drones
        .iter()
        .find(|d| d.id_kind == IdKind::Serial)
        .unwrap();
    assert_eq!(dji.id.as_str(), "TEST0SYNTH000001");
    assert_eq!(dji.model.as_str(), "Mavic 3");
    assert!(dji.radios.contains(Radio::DroneId));
    assert_eq!(dji.rssi[Radio::DroneId.index()], Some(-70));
    assert!(dji.drone.is_some());
    let o4 = st
        .drones
        .iter()
        .find(|d| d.id_kind == IdKind::Presence)
        .unwrap();
    assert_eq!(o4.id.as_str(), "DJI O4 01020304");
    assert_eq!(st.total_emitters, 2);
    // Strongest first: the energy, then the video.
    assert_eq!(st.emitters[0].kind, EmitterKind::Energy);
    assert_eq!(st.emitters[1].kind, EmitterKind::AnalogVideo);
    assert_eq!(st.emitters[1].freq_mhz, 5806.0);
    assert_eq!(st.wifi_drones[0].maker.as_str(), "DJI Tello");
    assert_eq!(st.wifi_drones[0].ssid.as_str(), "TELLO-?5A1B2");

    // Identity hidden: the O4 key is not shown either.
    let mut tr2: Tracker = Tracker::new(tracker::Config {
        hide_droneid_ids: true,
        ..Default::default()
    });
    tr2.ingest_droneid(
        2,
        &DroneIdSighting {
            freq: 24145,
            rssi: -85,
            snr_db: 5,
            uptime_us: 0,
            record: DroneIdRecord::Encrypted {
                key_id: [1, 2, 3, 4],
            },
        },
        1_000,
    );
    fill_from_tracker(&tr2, &tr2.context(1_500, None), &mut st);
    assert_eq!(st.drones[0].id.as_str(), "DJI O4 2414.5");
}

/// Protocol 7: the DJI link kinds reach the signals page with short labels.
#[test]
fn dji_link_kinds_reach_the_screen() {
    use link::{RfKind, RfSighting};
    let cases = [
        (RfKind::DjiVideo, 57685, EmitterKind::DjiVideo),
        (RfKind::DjiO4Video, 52100, EmitterKind::DjiO4Video),
        (RfKind::DjiFpvAirUnit, 57350, EmitterKind::DjiFpvAirUnit),
        (RfKind::DjiController, 57461, EmitterKind::DjiController),
    ];
    let mut tr: Tracker = Tracker::default();
    for (i, (kind, freq, _)) in cases.iter().enumerate() {
        let s = RfSighting {
            kind: *kind,
            freq: *freq,
            width_khz: 18_000,
            snr_db: 30 - i as i8,
            duty_pct: 90,
            line_hz: 0,
            uptime_us: 0,
        };
        tr.ingest_rf(3, &s, 1_000);
    }
    let mut st = UiState::default();
    fill_from_tracker(&tr, &tr.context(1_500, None), &mut st);
    assert_eq!(st.total_emitters, 4);
    for (e, (_, _, want)) in st.emitters.iter().zip(cases.iter()) {
        assert_eq!(e.kind, *want);
        assert!(e.kind.label().len() <= 11, "{}", e.kind.label());
    }
}

/// Protocol 12: control links reach the SIGNALS rows, and a drone known
/// only from a link's telemetry is named after the link.
#[test]
fn control_links_reach_the_screen() {
    use ctrlwatch::content::{AltRef, Channels, Position, PositionKind, PositionSource};
    use ctrlwatch::{Confidence, ControlContent, ControlSighting, Direction, Probe, Region, System};
    use tracker::ctrllinks::ControlTable;
    let mut tr: Tracker = Tracker::default();
    tr.set_own_position(CITY_HALL.0, CITY_HALL.1, None, 0);
    let mut table = ControlTable::new();
    let mut ch = Channels::default();
    for (i, v) in [1500u16, 1500, 988, 1500].into_iter().enumerate() {
        ch.set(i, v);
    }
    let pos = Position {
        kind: PositionKind::Aircraft,
        lat_e7: (INDEPENDENCE.0 * 1e7) as i32,
        lon_e7: (INDEPENDENCE.1 * 1e7) as i32,
        alt_mm: Some((60_000, AltRef::Msl)),
        rel_alt_mm: None,
        ground_speed_cms: None,
        heading_cdeg: None,
        satellites: Some(12),
        fix_type: Some(3),
        source: PositionSource::CrsfGps,
    };
    let r = link::ControlReport {
        sighting: ControlSighting {
            system: System::ExpressLrs,
            region: Region::Fcc915,
            probe: Probe::Energy,
            freq_hz: 915_500_000,
            rssi_dbm: -64,
            snr_db: 8,
            t_us: 1,
            confidence: Confidence::Confirmed,
            direction: Direction::Uplink,
            id: None,
            version: Some(4),
        },
        best: Confidence::Confirmed,
        packets: 9,
        confirmed: 2,
        peak_rssi: -60,
        part: 1,
        content: [ControlContent::Channels(ch), ControlContent::Position(pos)].into_iter().collect(),
        raw: None,
    };
    table.ingest(&mut tr, 1, &r, 1_000);
    let ctx = tr.context(2_000, None);
    let mut st = UiState::default();
    fill_from_tracker(&tr, &ctx, &mut st);
    screen::from_tracker::fill_control(&table, &ctx, &mut st);
    assert_eq!(st.total_control, 1);
    let c = &st.control[0];
    assert_eq!((c.system.as_str(), c.band, c.sure, c.rssi), ("ELRS", "915", ControlSure::Confirmed, -64));
    assert_eq!(c.detail.as_str(), "1500 1500 988 1500");
    assert_eq!(st.drones.len(), 1);
    assert_eq!(st.drones[0].id.as_str(), "ExpressLRS link 915");
    assert!(st.drones[0].drone.is_some(), "it has a position: on the radar");
}

/// One frame from a forged drone `k`, claiming to sit on the hub.
fn fake(t: &mut Tracker, k: u32, now: u64) {
    let id = Message::BasicId(BasicId {
        id_type: IdType::SerialNumber,
        ua_type: UaType::HelicopterOrMultirotor,
        uas_id: FixedStr::new(format!("FAKE{k:012}").as_bytes()).unwrap(),
    });
    let mac = [0x02, 0, (k >> 16) as u8, (k >> 8) as u8, k as u8, 9];
    t.ingest_messages(&obs(Transport::WifiBeacon, 6, -40, mac, now), &[id, location(CITY_HALL.0, CITY_HALL.1)], 0);
}

/// A flood of forged drones claiming to be at the hub (200 new IDs a
/// second for 60 s) never pushes a real, confirmed drone off the screen:
/// it is always in the list, first, numbered, while the fakes are marked
/// unconfirmed and the bar says ID FLOOD.
#[test]
fn flood_of_fakes_at_the_hub_never_hides_a_real_drone() {
    let mut t: Tracker = Tracker::default();
    let id = Message::BasicId(BasicId {
        id_type: IdType::SerialNumber,
        ua_type: UaType::HelicopterOrMultirotor,
        uas_id: FixedStr::new(b"1581F5FKD229400D").unwrap(),
    });
    let wifi = [0x60, 0x60, 0x1f, 0, 0, 1];
    let mut st = UiState { gps: Gps { fix: Fix::Fix3D, sats: 9, ..Gps::default() }, ..UiState::default() };
    let mut k = 0;
    let mut now = 0;
    while now < 65_000 {
        t.set_own_position(CITY_HALL.0, CITY_HALL.1, Some(40.0), now);
        if now % 1_000 == 0 {
            t.ingest_messages(&obs(Transport::WifiBeacon, 6, -70, wifi, now), &[id.clone(), location(INDEPENDENCE.0, INDEPENDENCE.1)], 0);
        }
        if now >= 5_000 {
            fake(&mut t, k, now);
            k += 1;
        }
        if now % 250 == 0 && now >= 2_000 {
            let ctx = t.context(now, None);
            fill_from_tracker(&t, &ctx, &mut st);
            let order = st.order();
            let first = &st.drones[usize::from(order[0])];
            assert_eq!(first.id.as_str(), "1581F5FKD229400D", "at {now}");
            assert!(!first.unconfirmed);
            assert!(st.drones.iter().filter(|d| d.id.starts_with("FAKE")).all(|d| d.unconfirmed));
        }
        now += 5;
    }
    assert!(st.threats.flood_per_s >= 150, "{}", st.threats.flood_per_s);
    assert_eq!(st.drones.len(), MAX_DRONES);
}

/// The P4 saves theme changes as site-file lines and reads them back at
/// mount: every change survives the round trip, and only changes are
/// written.
#[test]
fn theme_settings_round_trip_through_site_lines() {
    use screen::from_tracker::{apply_site_theme, theme_changes};
    use screen::{Settings, SwitchAt, Theme, ThemeMode};
    use tracker::site::{SiteLine, SiteState, parse_line, write_theme};
    let before = Settings::default();
    let after = Settings {
        theme: ThemeMode::Auto,
        day_theme: Theme::GlassCockpit,
        night_theme: Theme::RedNight,
        switch_at: SwitchAt::Sunrise,
        brightness: 5,
        ..before
    };
    let mut text = std::string::String::new();
    theme_changes(&before, &after, |t| {
        let mut l: heapless::String<40> = heapless::String::new();
        write_theme(&mut l, t).unwrap();
        text.push_str(&l);
    });
    assert_eq!(text, "theme auto\ntheme-day glass-cockpit\ntheme-switch sunrise\n", "night theme unchanged: not written");
    let mut st = SiteState::default();
    for l in text.lines() {
        let p = parse_line(l).unwrap();
        assert!(matches!(p, SiteLine::Theme(_)));
        st.apply(p);
    }
    let loaded = apply_site_theme(&st.theme, Settings::default());
    assert_eq!(loaded, Settings { brightness: 3, ..after }, "brightness is not a site setting");
    theme_changes(&after, &after, |_| panic!("no change, no line"));
}
