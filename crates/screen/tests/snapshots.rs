//! Render every screen into an RGB565 framebuffer and write it to
//! `crates/screen/snapshots/*.png`, so the layouts can be reviewed without
//! hardware. Also checks that nothing is drawn off the panel.

use std::path::PathBuf;

use embedded_graphics::{pixelcolor::Rgb565, prelude::*};
use heapless::String;
use screen::*;

// ---------------------------------------------------------------------------
// A host framebuffer that writes PNGs
// ---------------------------------------------------------------------------

struct Fb {
    px: Vec<Rgb565>,
    off_panel: usize,
}

impl Fb {
    fn new() -> Self {
        Self {
            px: vec![Rgb565::BLACK; (WIDTH * HEIGHT) as usize],
            off_panel: 0,
        }
    }

    fn rgb8(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.px.len() * 3);
        for c in &self.px {
            // Expand 5/6/5 bits to 8 the way panels do (replicate high bits).
            let (r, g, b) = (c.r(), c.g(), c.b());
            out.push((r << 3) | (r >> 2));
            out.push((g << 2) | (g >> 4));
            out.push((b << 3) | (b >> 2));
        }
        out
    }

    fn save(&self, name: &str) {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("snapshots");
        std::fs::create_dir_all(&dir).unwrap();
        let file = std::fs::File::create(dir.join(format!("{name}.png"))).unwrap();
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), WIDTH as u32, HEIGHT as u32);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()
            .unwrap()
            .write_image_data(&self.rgb8())
            .unwrap();
    }
}

impl OriginDimensions for Fb {
    fn size(&self) -> Size {
        Size::new(WIDTH as u32, HEIGHT as u32)
    }
}

impl DrawTarget for Fb {
    type Color = Rgb565;
    type Error = core::convert::Infallible;

    fn draw_iter<I: IntoIterator<Item = Pixel<Rgb565>>>(
        &mut self,
        pixels: I,
    ) -> Result<(), Self::Error> {
        for Pixel(p, c) in pixels {
            if (0..WIDTH).contains(&p.x) && (0..HEIGHT).contains(&p.y) {
                self.px[(p.y * WIDTH + p.x) as usize] = c;
            } else {
                self.off_panel += 1;
            }
        }
        Ok(())
    }
}

fn shot(name: &str, ui: &Ui, s: &UiState) -> Fb {
    let mut fb = Fb::new();
    render(ui, s, &mut fb).unwrap();
    fb.save(name);
    assert_eq!(
        fb.off_panel, 0,
        "{name}: {} pixels drawn off the panel",
        fb.off_panel
    );
    fb
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const NOW: u64 = 3_725_000; // 1 h 02 min of uptime
const UTC: u64 = 1_793_023_380_000; // 2026-10-26 14:03 UTC

fn s<const N: usize>(t: &str) -> String<N> {
    let mut out = String::new();
    out.push_str(t).unwrap();
    out
}

fn base(fix: bool) -> UiState {
    let mut st = UiState {
        now_ms: NOW,
        utc_ms: Some(UTC),
        gps: if fix {
            Gps {
                fix: Fix::Fix3D,
                sats: 11,
                ..Gps::default()
            }
        } else {
            Gps {
                fix: Fix::None,
                sats: 2,
                ..Gps::default()
            }
        },
        power: Power::Battery { pct: 76 },
        sd: SdLog::Recording,
        hub_uptime_s: (NOW / 1000) as u32,
        ..UiState::default()
    };
    for (kind, fps, rid) in [
        (NodeKind::Wifi24, 412, 1830),
        (NodeKind::Wifi5, 96, 0),
        (NodeKind::Ble, 233, 2410),
    ] {
        st.nodes
            .push(NodeStatus {
                kind,
                alive: true,
                frames_per_s: fps,
                rid_found: rid,
                drops: if kind == NodeKind::Wifi24 { 3 } else { 0 },
                uptime_s: (NOW / 1000) as u32 - 4,
            })
            .unwrap();
    }
    st
}

#[allow(clippy::too_many_arguments)]
fn drone(
    key: u32,
    id: &str,
    dist: f32,
    brg: f32,
    height: f32,
    age_s: u64,
    radios: &[Radio],
    rssi: i8,
) -> DroneView {
    let mut r = Radios::default();
    let mut per = [None; 6];
    for x in radios {
        r.insert(*x);
        per[x.index()] = Some(rssi - x.index() as i8 * 3);
    }
    DroneView {
        key,
        id: s(id),
        id_kind: IdKind::Serial,
        ua: UaKind::Multirotor,
        stale: age_s >= 60,
        first_seen_ms: NOW - 240_000 + u64::from(key) * 30_000,
        last_seen_ms: NOW - age_s * 1000,
        drone: Some(Polar {
            distance_m: dist,
            bearing_deg: brg,
        }),
        operator: None,
        drone_to_operator_m: None,
        height_m: Some(height),
        height_ref: HeightRef::Takeoff,
        alt_m: Some(height + 18.0),
        above_you_m: Some(height + 2.0),
        speed_mps: Some(6.5),
        vspeed_mps: Some(0.5),
        track_deg: Some(brg + 120.0),
        radios: r,
        rssi: per,
        ..DroneView::default()
    }
}

fn one_drone(fix: bool) -> UiState {
    let mut st = base(fix);
    let mut d = drone(
        7,
        "1581F5FKD229400D",
        320.0,
        47.0,
        85.0,
        0,
        &[Radio::WifiBeacon, Radio::BleLongRange],
        -58,
    );
    d.operator = Some(Polar {
        distance_m: 140.0,
        bearing_deg: 300.0,
    });
    d.drone_to_operator_m = Some(390.0);
    d.self_id = s("Roof survey - Acme");
    d.operator_id = s("USA-RP-8F2K1Q");
    if !fix {
        d.drone = None;
        d.operator = None;
        d.above_you_m = None;
    }
    st.drones.push(d).unwrap();
    st.total_drones = 1;
    st
}

fn five_drones(fix: bool) -> UiState {
    let mut st = base(fix);
    use Radio::*;
    let list = [
        drone(
            1,
            "1581F5FKD229400D",
            320.0,
            47.0,
            85.0,
            0,
            &[WifiBeacon, BleLongRange],
            -58,
        ),
        drone(
            2,
            "1668B2A07C330091",
            1420.0,
            200.0,
            118.0,
            1,
            &[WifiNan, Wifi5],
            -77,
        ),
        drone(3, "SKY04X118", 75.0, 310.0, 32.0, 3, &[Ble], -49),
        drone(
            4,
            "1581F6QAD2310077",
            4800.0,
            110.0,
            120.0,
            4,
            &[BleLongRange],
            -88,
        ),
        drone(
            5,
            "FIN87astrdge12k8",
            860.0,
            15.0,
            60.0,
            95,
            &[WifiBeacon],
            -81,
        ),
    ];
    for mut d in list {
        if d.key == 1 {
            d.operator = Some(Polar {
                distance_m: 140.0,
                bearing_deg: 300.0,
            });
        }
        if !fix {
            d.drone = None;
            d.operator = None;
        }
        st.drones.push(d).unwrap();
    }
    // A MAC-only track: BLE heard, no Basic ID yet, no position.
    let mut m = DroneView {
        key: 6,
        id: s("c0:11:22:33:44:55"),
        id_kind: IdKind::Mac,
        first_seen_ms: NOW - 2_000,
        last_seen_ms: NOW - 1_000,
        ..DroneView::default()
    };
    m.radios.insert(Radio::Ble);
    m.rssi[Radio::Ble.index()] = Some(-91);
    st.drones.push(m).unwrap();
    st.total_drones = 6;
    st
}

fn press(ui: &mut Ui, st: &UiState, b: &[Button]) {
    for b in b {
        ui.button(*b, st);
    }
}

// ---------------------------------------------------------------------------
// Snapshots
// ---------------------------------------------------------------------------

#[test]
fn radar_empty() {
    let mut st = base(true);
    st.power = Power::Usb { battery_pct: None };
    st.sd = SdLog::Off;
    shot("radar_0_drones", &Ui::default(), &st);
}

#[test]
fn radar_one() {
    shot("radar_1_drone", &Ui::default(), &one_drone(true));
}

#[test]
fn radar_five() {
    shot("radar_5_drones", &Ui::default(), &five_drones(true));
}

#[test]
fn radar_five_day() {
    let ui = Ui::new(Settings {
        theme: ThemeMode::Fixed(Theme::FieldDay),
        ..Settings::default()
    });
    shot("radar_5_drones_day", &ui, &five_drones(true));
}

#[test]
fn radar_picked() {
    let st = five_drones(true);
    let mut ui = Ui::default();
    press(&mut ui, &st, &[Button::Select, Button::Down, Button::Down]);
    shot("radar_pick", &ui, &st);
}

#[test]
fn list_five() {
    let st = five_drones(true);
    let mut ui = Ui::default();
    press(&mut ui, &st, &[Button::Down]);
    ui.tick(NOW + 5_000); // let the page-name toast expire
    shot("list_5_drones", &ui, &st);
    press(&mut ui, &st, &[Button::Select, Button::Down]);
    shot("list_pick", &ui, &st);
}

/// WP-09: a flood of forged drones claiming to be at the hub. The real
/// drones keep their numbered rows at the top; the fakes are listed after
/// them, dim, numbered "?", hollow on the radar; the bar says ID FLOOD.
#[test]
fn list_flood() {
    let mut st = five_drones(true);
    for k in 0..6u32 {
        let mut f = drone(100 + k, "FAKE00000000", 0.0 + k as f32, 10.0 * k as f32, 60.0, 0, &[Radio::WifiBeacon], -40);
        f.unconfirmed = true;
        f.first_seen_ms = NOW - 1_000;
        st.drones.push(f).unwrap();
    }
    st.total_drones = 64;
    st.total_unconfirmed = 58;
    st.threats = Threats { active: 1, jammed: false, muted: 0, flood_per_s: 230, ..Threats::default() };
    let order = st.order();
    assert!(order.iter().take(5).all(|&i| !st.drones[usize::from(i)].unconfirmed));
    shot("radar_flood", &Ui::default(), &st);
    let mut ui = Ui::default();
    press(&mut ui, &st, &[Button::Down]);
    ui.tick(NOW + 5_000);
    shot("list_flood", &ui, &st);
}

#[test]
fn no_fix() {
    let mut st = five_drones(false);
    st.nodes[1].alive = false;
    shot("no_fix", &Ui::default(), &st);
    let mut ui = Ui::default();
    press(&mut ui, &st, &[Button::Down]);
    ui.tick(NOW + 5_000);
    shot("list_no_fix", &ui, &st);
}

#[test]
fn alert() {
    let st = five_drones(true);
    let mut ui = Ui::default();
    ui.new_drone(3, NOW);
    shot("alert", &ui, &st);
    ui.new_drone(2, NOW + 500);
    shot("alert_more", &ui, &st);
}

#[test]
fn detail() {
    let st = one_drone(true);
    let mut ui = Ui::default();
    press(&mut ui, &st, &[Button::Select, Button::Select]);
    assert_eq!(ui.mode(), Mode::Detail);
    shot("detail", &ui, &st);

    let st = one_drone(false);
    let mut ui = Ui::default();
    press(&mut ui, &st, &[Button::Select, Button::Select]);
    shot("detail_no_fix", &ui, &st);
}

/// WP-10: a forger repeats this drone's ID 1.9 km away. The shown position
/// stays the established one; the list puts "!" before the ID, the radar
/// shows the other claim as a hollow "?" diamond joined to it, and the
/// detail page says DISPUTED with where the other claim is. A quiet drone's
/// detail says "broadcast, not verified" (see `detail`).
#[test]
fn disputed() {
    let mut st = one_drone(true);
    let d = &mut st.drones[0];
    d.disputed = true;
    d.other_claim = Some(Polar { distance_m: 1_900.0, bearing_deg: 80.0 });
    d.claims_apart_m = Some(1_640.0);
    let mut ui = Ui::default();
    press(&mut ui, &st, &[Button::Select]);
    shot("radar_disputed", &ui, &st);
    press(&mut ui, &st, &[Button::Select]);
    assert_eq!(ui.mode(), Mode::Detail);
    shot("detail_disputed", &ui, &st);
    let mut ui = Ui::default();
    press(&mut ui, &st, &[Button::Down]);
    ui.tick(NOW + 5_000);
    shot("list_disputed", &ui, &st);
}

#[test]
fn health() {
    let mut st = five_drones(true);
    st.nodes[1].alive = false;
    st.power = Power::Usb {
        battery_pct: Some(100),
    };
    let mut ui = Ui::default();
    press(&mut ui, &st, &[Button::Down, Button::Down, Button::Down]);
    ui.tick(NOW + 5_000);
    shot("health", &ui, &st);
}

/// A hub with a coverage coordinator: each node's assigned role under it.
#[test]
fn health_assigned() {
    let mut st = five_drones(true);
    st.nodes[2].kind = NodeKind::DroneId;
    st.assigned.extend([
        Some(Assigned {
            role: "Wi-Fi 2.4",
            plan: 0x5a3c,
            applied: true,
        }),
        None,
        Some(Assigned {
            role: "DroneID",
            plan: 0x0b17,
            applied: false,
        }),
    ]);
    let mut ui = Ui::default();
    press(&mut ui, &st, &[Button::Down, Button::Down, Button::Down]);
    ui.tick(NOW + 5_000);
    shot("health_assigned", &ui, &st);
}

/// A hub with an ADS-B receiver: the ADSBee as a fourth node, and the
/// AIRCRAFT line (one low, nearest a medevac helicopter).
fn with_aircraft() -> UiState {
    let mut st = five_drones(true);
    st.nodes
        .push(NodeStatus {
            kind: NodeKind::Adsb,
            alive: true,
            frames_per_s: 118,
            rid_found: 0,
            drops: 0,
            uptime_s: 3_600,
        })
        .unwrap();
    st.aircraft = Some(AircraftSummary {
        total: 14,
        active: 12,
        low_nearby: 1,
        nearest: Some(NearestAircraft {
            name: s("N911PH"),
            distance_m: 1_240.0,
            alt_ft: Some(1_200),
        }),
    });
    st
}

#[test]
fn health_aircraft() {
    let st = with_aircraft();
    let mut ui = Ui::default();
    press(&mut ui, &st, &[Button::Down, Button::Down, Button::Down]);
    ui.tick(NOW + 5_000);
    shot("health_aircraft", &ui, &st);
}

/// Protocol 4: a DroneID node and a gateway, a DJI drone heard on DroneID
/// only (model, no Remote ID), an O4 presence track, RF emitters and Wi-Fi
/// drones on the SIGNALS page.
fn with_signals() -> UiState {
    let mut st = base(true);
    st.nodes.truncate(2);
    for kind in [NodeKind::DroneId, NodeKind::Gateway] {
        st.nodes
            .push(NodeStatus {
                kind,
                alive: true,
                frames_per_s: 0,
                rid_found: 0,
                drops: 0,
                uptime_s: 600,
            })
            .unwrap();
    }
    let mut d = drone(
        1,
        "TEST0SYNTH000001",
        640.0,
        80.0,
        95.0,
        0,
        &[Radio::DroneId],
        -71,
    );
    d.model = s("Mavic 3");
    d.operator = Some(Polar {
        distance_m: 20.0,
        bearing_deg: 10.0,
    });
    st.drones.push(d).unwrap();
    let mut o4 = DroneView {
        key: 2,
        id: s("O4 01020304 5756.5"),
        id_kind: IdKind::Presence,
        first_seen_ms: NOW - 9_000,
        last_seen_ms: NOW - 1_000,
        ..DroneView::default()
    };
    o4.radios.insert(Radio::DroneId);
    o4.rssi[Radio::DroneId.index()] = Some(-84);
    st.drones.push(o4).unwrap();
    st.total_drones = 2;
    for (key, kind, mhz, snr, duty, line) in [
        (1, EmitterKind::AnalogVideo, 5806.0, 24, 100, 15_734),
        (2, EmitterKind::Hdzero, 5732.0, 18, 100, 0),
        (3, EmitterKind::LteStyle, 5770.0, 11, 40, 0),
        (4, EmitterKind::Energy, 2437.0, 6, 12, 0),
    ] {
        st.emitters
            .push(EmitterView {
                key,
                kind,
                freq_mhz: mhz,
                snr_db: snr,
                duty_pct: duty,
                line_hz: line,
                stale: key == 4,
                last_seen_ms: NOW - u64::from(key) * 2_000,
            })
            .unwrap();
    }
    st.total_emitters = 6;
    for (key, maker, ssid, ch, rssi) in [
        (1, "DJI Tello", "TELLO-5A1B2C", 6, -52),
        (2, "Parrot", "Bebop2_123456", 149, -70),
        (3, "Toy drone", "WIFI-UFO-1a2b3c-and-a-long-tail", 1, -83),
    ] {
        st.wifi_drones
            .push(WifiDroneView {
                key,
                maker: s(maker),
                ssid: s(ssid),
                channel: ch,
                rssi,
                stale: false,
                last_seen_ms: NOW - u64::from(key) * 1_000,
            })
            .unwrap();
    }
    st.total_wifi_drones = 3;
    for (key, sys, band, sure, rssi, detail, stale) in [
        (1, "ELRS", "915", ControlSure::Confirmed, -64, "1500 1500 988 1500", false),
        (2, "SiK", "915", ControlSure::Confirmed, -88, "64k", false),
        (3, "ELRS?", "2.4", ControlSure::Energy, -71, "24ch 1000k 4.0ms", true),
    ]
    .into_iter()
    .take(MAX_CONTROL_ROWS)
    {
        st.control
            .push(ControlView {
                key,
                system: s(sys),
                band,
                sure,
                rssi,
                detail: s(detail),
                stale,
                last_seen_ms: NOW - u64::from(key) * 1_500,
            })
            .unwrap();
    }
    st.total_control = 4;
    st
}

#[test]
fn signals() {
    let st = with_signals();
    let mut ui = Ui::default();
    press(&mut ui, &st, &[Button::Down, Button::Down]);
    assert_eq!(ui.page(), Page::Signals);
    ui.tick(NOW + 5_000);
    shot("signals", &ui, &st);
    shot("signals_empty", &ui, &base(true));
    let mut ui = Ui::default();
    press(&mut ui, &st, &[Button::Down]);
    ui.tick(NOW + 5_000);
    shot("list_droneid", &ui, &st);
    press(&mut ui, &st, &[Button::Select, Button::Select]);
    shot("detail_droneid", &ui, &st);
    let mut ui = Ui::default();
    press(&mut ui, &st, &[Button::Down, Button::Down, Button::Down]);
    ui.tick(NOW + 5_000);
    shot("health_roles", &ui, &st);
}

/// The FLAGS page (PLAN.md Phase 12): only in the page cycle when the hub
/// fills `flags`.
#[test]
fn flags() {
    let mut st = base(true);
    let mut f = FlagsView {
        pack: s("pack 2026-11-15"),
        total: 4,
        ..Default::default()
    };
    for (who, level, rule, text, secs) in [
        ("RSTEST-SD-0001", 3, "AREA", "Natl security UAS", 12),
        ("RSTEST-SD-0001", 2, "HEIGHT", "853ft AGL", 41),
        ("1581F5FJD2", 1, "VLOS", "640m from pilot", 9),
        ("N12345", 2, "LOW", "520ft<1000 91.119", 33),
    ] {
        let _ = f.rows.push(FlagRow {
            who: s(who),
            level,
            rule,
            text: s(text),
            secs,
        });
    }
    st.flags = Some(f);
    let mut ui = Ui::default();
    press(&mut ui, &st, &[Button::Down, Button::Down]);
    assert_eq!(ui.page(), Page::Flags);
    ui.tick(NOW + 5_000);
    shot("flags", &ui, &st);
    // Without the engine the page is skipped.
    let st = base(true);
    let mut ui = Ui::default();
    press(&mut ui, &st, &[Button::Down, Button::Down]);
    assert_eq!(ui.page(), Page::Signals);
}

#[test]
fn settings() {
    let st = base(true);
    let mut ui = Ui::default();
    press(&mut ui, &st, &[Button::Up, Button::Select, Button::Down]);
    shot("settings", &ui, &st);
}

#[test]
fn settings_actions() {
    let mut st = base(true);
    st.threats = Threats { active: 2, jammed: false, muted: 1, flood_per_s: 0, mutable: 2, snoozable: 0 };
    let mut ui = Ui::default();
    // Settings page, edit, up to the last row (Nodes at hub), up to Alarms.
    press(&mut ui, &st, &[Button::Up, Button::Select, Button::Up, Button::Up]);
    shot("settings_actions", &ui, &st);
    // Only a jammer sounds: the row snoozes it, it never mutes it.
    st.threats = Threats { active: 1, jammed: true, muted: 2, flood_per_s: 0, mutable: 0, snoozable: 1 };
    shot("settings_snooze", &ui, &st);
}

#[test]
fn threat_warnings() {
    // GPS faked and a node jammed, with no drones heard: the list must not
    // read as all clear.
    let mut st = base(true);
    st.gps.integrity = GpsIntegrity::Spoofing;
    st.threats = Threats { active: 2, jammed: true, muted: 0, flood_per_s: 0, ..Threats::default() };
    let mut ui = Ui::default();
    press(&mut ui, &st, &[Button::Down]);
    shot("threat_warnings", &ui, &st);
}

#[test]
fn health_conflict() {
    // A drone 240 m from a helicopter that is 30 m above it: the status bar
    // shows AIRCRAFT (ahead of jamming) and the health page says which.
    let mut st = with_aircraft();
    st.threats = Threats { active: 0, jammed: true, muted: 0, flood_per_s: 0, ..Threats::default() };
    let mut name = heapless::String::new();
    name.push_str("N911PH").unwrap();
    st.conflict = Some(ConflictView { level: 3, drone: 1, aircraft: name, range_m: 240.0, dz_m: Some(30.0), count: 1 });
    let mut ui = Ui::default();
    press(&mut ui, &st, &[Button::Down, Button::Down, Button::Down]);
    ui.tick(NOW + 5_000);
    shot("health_conflict", &ui, &st);
}

#[test]
fn health_area_gnss() {
    // GPS jamming in the area: 4 of 6 aircraft lost GPS and the hub's own
    // receiver agrees. AREA JAM in the status bar, a line on the health page.
    let mut st = with_aircraft();
    st.area_gnss = Some(AreaGnssView { jamming: true, degraded: 4, counted: 6, own_agrees: true, ..Default::default() });
    let mut ui = Ui::default();
    press(&mut ui, &st, &[Button::Down, Button::Down, Button::Down]);
    ui.tick(NOW + 5_000);
    shot("health_area_gnss", &ui, &st);
    // Spoofing: two drones moved together; AREA FAKE in the status bar.
    st.area_gnss = Some(AreaGnssView { spoofing: true, drones_moved: 2, ..Default::default() });
    shot("health_area_spoof", &ui, &st);
    // With a near aircraft too there is no room for the line on this
    // four-node hub: the NEAR line stays (nothing drawn off the panel).
    let mut name = heapless::String::new();
    name.push_str("N911PH").unwrap();
    st.conflict = Some(ConflictView { level: 1, drone: 1, aircraft: name, range_m: 2400.0, dz_m: Some(300.0), count: 0 });
    shot("health_area_conflict", &ui, &st);
}

#[test]
fn page_toast() {
    let st = five_drones(true);
    let mut ui = Ui::default();
    press(&mut ui, &st, &[Button::Down]);
    shot("page_toast", &ui, &st);
}

#[test]
fn status_warnings() {
    // Everything that can go wrong in the status bar at once.
    let mut st = one_drone(true);
    st.gps = Gps {
        fix: Fix::Fix2D,
        sats: 4,
        ..Gps::default()
    };
    st.power = Power::Battery { pct: 9 };
    st.sd = SdLog::Error;
    st.utc_ms = None;
    for n in st.nodes.iter_mut() {
        n.alive = false;
    }
    shot("status_warnings", &Ui::default(), &st);
}

/// The P4 hub's caveats strip: a test-key bench build on an open link,
/// with evidence not reaching the card while the CSV log records.
#[test]
fn caveats_strip() {
    let mut st = five_drones(true);
    st.caveats = Caveats {
        key: EvidenceKey::Test,
        bench: true,
        link_open: true,
        evidence: Some(EvidenceStop { why: EvidenceStopWhy::FileError, waiting: 1234, dropped: 56 }),
        ..Caveats::default()
    };
    st.sd = SdLog::Recording;
    shot("caveats_all", &Ui::default(), &st);
    let mut ui = Ui::default();
    press(&mut ui, &st, &[Button::Down, Button::Down, Button::Down]);
    ui.tick(NOW + 5_000);
    shot("caveats_health", &ui, &st);
    // Only the key class: the usual development hub.
    let mut st = five_drones(true);
    st.caveats.key = EvidenceKey::Development;
    shot("caveats_dev_key", &Ui::default(), &st);
    // The longest numbers still stay on the panel (checked by `shot`).
    let mut st = base(true);
    st.caveats.evidence = Some(EvidenceStop { why: EvidenceStopWhy::CardError, waiting: u32::MAX, dropped: u32::MAX });
    st.caveats.key = EvidenceKey::Development;
    st.caveats.bench = true;
    st.caveats.link_open = true;
    shot("caveats_long", &Ui::default(), &st);
    // Alarms silenced and collection switches on (WP-11b): a jammer
    // snoozed, two nuisance alarms muted, the venue's guest network muted
    // for good, control IDs and content read. The health page says what
    // the silence hides.
    let mut st = five_drones(true);
    st.threats = Threats { active: 1, jammed: true, muted: 3, flood_per_s: 0, mutable: 1, snoozable: 0 };
    st.caveats.silenced = Silenced { muted: 2, mute_left_s: 7_140, snoozed: 1, snooze_left_s: 245, site: 1 };
    st.caveats.switches = Switches { control_ids: true, control_content: true, audio_clips: false };
    shot("caveats_alarms", &Ui::default(), &st);
    let mut ui = Ui::default();
    press(&mut ui, &st, &[Button::Down, Button::Down, Button::Down]);
    ui.tick(NOW + 5_000);
    shot("caveats_alarms_health", &ui, &st);
}

// ---------------------------------------------------------------------------
// Banded rendering (the hub has no PSRAM for a full framebuffer)
// ---------------------------------------------------------------------------

/// Drawing band by band gives exactly the pixels of a full-frame render, for
/// every page and overlay, at the hub's band height and at awkward ones (a
/// band height that does not divide 240, and a single row).
#[test]
fn banded_render_matches_full_frame() {
    let mut cases: Vec<(&str, Ui, UiState)> = Vec::new();
    cases.push(("radar_empty", Ui::default(), base(true)));
    cases.push(("radar_five", Ui::default(), five_drones(true)));
    cases.push(("no_fix", Ui::default(), five_drones(false)));
    let st = five_drones(true);
    let mut ui = Ui::default();
    press(&mut ui, &st, &[Button::Down]);
    cases.push(("list", ui, st));
    let st = five_drones(true);
    let mut ui = Ui::default();
    press(&mut ui, &st, &[Button::Select, Button::Select]);
    cases.push(("detail", ui, st));
    let st = five_drones(true);
    let mut ui = Ui::default();
    press(&mut ui, &st, &[Button::Down, Button::Down]);
    cases.push(("health", ui, st));
    let st = with_aircraft();
    let mut ui = Ui::default();
    press(&mut ui, &st, &[Button::Down, Button::Down]);
    cases.push(("health_aircraft", ui, st));
    let st = one_drone(true);
    let mut ui = Ui::default();
    ui.new_drone(st.drones[0].key, NOW);
    cases.push(("alert", ui, st));
    // The new status-bar states.
    let mut st = five_drones(true);
    st.gps.fix = Fix::Manual;
    st.sd = SdLog::NoCard;
    cases.push(("manual_nocard", Ui::default(), st));
    let mut st = base(false);
    st.gps.fix = Fix::Absent;
    cases.push(("absent", Ui::default(), st));
    let mut st = five_drones(true);
    st.caveats = Caveats {
        key: EvidenceKey::Test,
        bench: true,
        link_open: true,
        evidence: Some(EvidenceStop { why: EvidenceStopWhy::NoCard, waiting: 7, dropped: 0 }),
        silenced: Silenced { muted: 1, mute_left_s: 3_600, snoozed: 0, snooze_left_s: 0, site: 2 },
        switches: Switches { control_ids: true, ..Switches::default() },
    };
    cases.push(("caveats", Ui::default(), st));

    for (name, ui, st) in &cases {
        let mut full = Fb::new();
        render(ui, st, &mut full).unwrap();
        for rows in [BAND_ROWS, 7, 1] {
            let mut buf = vec![Rgb565::new(1, 2, 3); WIDTH as usize * rows];
            let mut band = Band::new(&mut buf);
            let mut out = Fb::new();
            let mut flushed_rows = 0;
            render_banded(ui, st, &mut band, |top, n, px| {
                assert_eq!(top, flushed_rows, "{name}: bands out of order");
                assert_eq!(px.len(), (n * WIDTH) as usize);
                out.px[(top * WIDTH) as usize..((top + n) * WIDTH) as usize].copy_from_slice(px);
                flushed_rows += n;
                Ok::<(), ()>(())
            })
            .unwrap();
            assert_eq!(flushed_rows, HEIGHT, "{name}: rows {rows}");
            assert!(
                out.px == full.px,
                "{name}: banded ({rows} rows) differs from full frame"
            );
        }
    }
}

/// The status bar's GPS-absent, manual-position and no-card states.
#[test]
fn status_absent_manual_nocard() {
    let mut st = base(false);
    st.gps.fix = Fix::Absent;
    st.sd = SdLog::NoCard;
    st.power = Power::Usb { battery_pct: None };
    shot("status_gps_absent", &Ui::default(), &st);
    let mut st = one_drone(true);
    st.gps.fix = Fix::Manual;
    st.sd = SdLog::NoCard;
    shot("status_manual_pos", &Ui::default(), &st);
}

// ---------------------------------------------------------------------------
// The P4's native 800x480 layout (feature `wide`), in every theme
// ---------------------------------------------------------------------------

mod wide_shots {
    use super::*;
    use screen::wide::{self, Canvas};

    const WW: usize = wide::W as usize;
    const WH: usize = wide::H as usize;

    /// The same screen in theme `t` (Auto keeps its rows; the one of its
    /// two themes showing now is set to `t`).
    fn in_theme(ui: &Ui, s: &UiState, t: Theme) -> Ui {
        let mut ui = ui.clone();
        let mut st = *ui.settings();
        if st.theme == ThemeMode::Auto {
            // Whichever of Auto's two themes is showing now becomes `t`.
            match ui.auto_status(s) {
                AutoStatus::Sun { day: true, .. } => st.day_theme = t,
                _ => st.night_theme = t,
            }
        } else {
            st.theme = ThemeMode::Fixed(t);
        }
        ui.set_settings(st);
        ui
    }

    fn dir(t: Theme) -> &'static str {
        match t {
            Theme::GlassCockpit => "glass-cockpit",
            Theme::FieldDay => "field-day",
            Theme::RedNight => "red-night",
        }
    }

    /// Full frame in one canvas; returns pixels and off-panel count.
    fn full(ui: &Ui, s: &UiState, lit: Option<Button>) -> (Vec<u16>, u32) {
        let mut px = vec![0u16; WW * WH];
        let mut c = Canvas::new(&mut px, 0);
        wide::render(ui, s, lit, &mut c);
        let off = c.off_panel;
        (px, off)
    }

    fn save(path: &std::path::Path, px: &[u16]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut rgb = Vec::with_capacity(px.len() * 3);
        for &v in px {
            let (r, g, b) = (v >> 11, (v >> 5) & 63, v & 31);
            rgb.push(((r << 3) | (r >> 2)) as u8);
            rgb.push(((g << 2) | (g >> 4)) as u8);
            rgb.push(((b << 3) | (b >> 2)) as u8);
        }
        let file = std::fs::File::create(path).unwrap();
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), WW as u32, WH as u32);
        enc.set_compression(png::Compression::Best);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&rgb).unwrap();
    }

    /// Write `snapshots/wide/<theme>/<name>.png` for all three themes.
    fn shot(name: &str, ui: &Ui, s: &UiState, lit: Option<Button>) {
        for t in Theme::ALL {
            let ui = in_theme(ui, s, t);
            let (px, off) = full(&ui, s, lit);
            assert_eq!(off, 0, "{name} ({t:?}): drawn off the panel");
            let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("snapshots/wide").join(dir(t)).join(format!("{name}.png"));
            save(&p, &px);
        }
    }

    fn after(ui: &mut Ui, st: &UiState, b: &[Button], settle: bool) {
        press(ui, st, b);
        if settle {
            ui.tick(NOW + 5_000);
        }
    }

    /// Every case, also used by the banding test.
    fn cases() -> Vec<(&'static str, Ui, UiState, Option<Button>)> {
        let mut v: Vec<(&'static str, Ui, UiState, Option<Button>)> = Vec::new();
        let mut empty = base(true);
        empty.power = Power::Usb { battery_pct: None };
        empty.sd = SdLog::Off;
        v.push(("radar_0_drones", Ui::default(), empty, None));
        v.push(("radar_1_drone", Ui::default(), one_drone(true), None));
        v.push(("radar_5_drones", Ui::default(), five_drones(true), None));
        // A key being pressed (lit) on the radar.
        v.push(("radar_key_pressed", Ui::default(), five_drones(true), Some(Button::Select)));
        let st = five_drones(true);
        let mut ui = Ui::default();
        after(&mut ui, &st, &[Button::Select, Button::Down, Button::Down], false);
        v.push(("radar_pick", ui, st, None));
        let mut st = one_drone(true);
        st.drones[0].disputed = true;
        st.drones[0].other_claim = Some(Polar { distance_m: 1_900.0, bearing_deg: 80.0 });
        st.drones[0].claims_apart_m = Some(1_640.0);
        let mut ui = Ui::default();
        after(&mut ui, &st, &[Button::Select], false);
        v.push(("radar_disputed", ui.clone(), st.clone(), None));
        after(&mut ui, &st, &[Button::Select], false);
        v.push(("detail_disputed", ui, st, None));

        let st = five_drones(true);
        let mut ui = Ui::default();
        after(&mut ui, &st, &[Button::Down], true);
        v.push(("list_5_drones", ui.clone(), st.clone(), None));
        after(&mut ui, &st, &[Button::Select, Button::Down], false);
        v.push(("list_pick", ui, st, None));

        // WP-09: a flood of forged IDs.
        let mut st = five_drones(true);
        for k in 0..6u32 {
            let mut f = drone(100 + k, "FAKE00000000", k as f32, 10.0 * k as f32, 60.0, 0, &[Radio::WifiBeacon], -40);
            f.unconfirmed = true;
            f.first_seen_ms = NOW - 1_000;
            st.drones.push(f).unwrap();
        }
        st.total_drones = 64;
        st.total_unconfirmed = 58;
        st.threats = Threats { active: 1, flood_per_s: 230, ..Threats::default() };
        v.push(("radar_flood", Ui::default(), st.clone(), None));
        let mut ui = Ui::default();
        after(&mut ui, &st, &[Button::Down], true);
        v.push(("list_flood", ui, st, None));

        let mut st = five_drones(false);
        st.nodes[1].alive = false;
        v.push(("no_fix", Ui::default(), st.clone(), None));
        let mut ui = Ui::default();
        after(&mut ui, &st, &[Button::Down], true);
        v.push(("list_no_fix", ui, st, None));

        let st = five_drones(true);
        let mut ui = Ui::default();
        ui.new_drone(3, NOW);
        v.push(("alert", ui.clone(), st.clone(), None));
        ui.new_drone(2, NOW + 500);
        v.push(("alert_more", ui, st, None));
        let st = five_drones(false);
        let mut ui = Ui::default();
        ui.new_drone(1, NOW);
        v.push(("alert_no_fix", ui, st, None));

        let st = one_drone(true);
        let mut ui = Ui::default();
        after(&mut ui, &st, &[Button::Select, Button::Select], false);
        v.push(("detail", ui, st, None));
        let st = one_drone(false);
        let mut ui = Ui::default();
        after(&mut ui, &st, &[Button::Select, Button::Select], false);
        v.push(("detail_no_fix", ui, st, None));
        let mut st = five_drones(true);
        let mut ui = Ui::default();
        after(&mut ui, &st, &[Button::Select, Button::Down, Button::Down, Button::Down, Button::Down, Button::Select], false);
        st.drones[4].stale = true;
        v.push(("detail_stale", ui, st, None));

        let st = with_signals();
        let mut ui = Ui::default();
        after(&mut ui, &st, &[Button::Down, Button::Down], true);
        v.push(("signals", ui.clone(), st.clone(), None));
        v.push(("signals_empty", ui, base(true), None));
        let mut ui = Ui::default();
        after(&mut ui, &st, &[Button::Down], true);
        v.push(("list_droneid", ui.clone(), st.clone(), None));
        after(&mut ui, &st, &[Button::Select, Button::Select], false);
        v.push(("detail_droneid", ui, st.clone(), None));
        let mut ui = Ui::default();
        after(&mut ui, &st, &[Button::Down, Button::Down, Button::Down], true);
        v.push(("health_roles", ui, st, None));

        let mut st = base(true);
        let mut fl = FlagsView { pack: s("pack 2026-11-15"), total: 4, ..Default::default() };
        for (who, level, rule, text, secs) in [
            ("RSTEST-SD-0001", 3, "AREA", "Natl security UAS", 12),
            ("RSTEST-SD-0001", 2, "HEIGHT", "853ft AGL", 41),
            ("1581F5FJD2", 1, "VLOS", "640m from pilot", 9),
            ("N12345", 2, "LOW", "520ft<1000 91.119", 33),
        ] {
            let _ = fl.rows.push(FlagRow { who: s(who), level, rule, text: s(text), secs });
        }
        st.flags = Some(fl);
        let mut ui = Ui::default();
        after(&mut ui, &st, &[Button::Down, Button::Down], true);
        v.push(("flags", ui, st, None));

        let mut st = five_drones(true);
        st.nodes[1].alive = false;
        st.power = Power::Usb { battery_pct: Some(100) };
        let mut ui = Ui::default();
        after(&mut ui, &st, &[Button::Down, Button::Down, Button::Down], true);
        v.push(("health", ui.clone(), st, None));
        let mut st = five_drones(true);
        st.nodes[2].kind = NodeKind::DroneId;
        st.assigned.extend([
            Some(Assigned { role: "Wi-Fi 2.4", plan: 0x5a3c, applied: true }),
            None,
            Some(Assigned { role: "DroneID", plan: 0x0b17, applied: false }),
        ]);
        v.push(("health_assigned", ui.clone(), st, None));
        v.push(("health_aircraft", ui.clone(), with_aircraft(), None));
        let mut st = with_aircraft();
        st.threats = Threats { jammed: true, ..Threats::default() };
        st.conflict = Some(ConflictView { level: 3, drone: 1, aircraft: s("N911PH"), range_m: 240.0, dz_m: Some(30.0), count: 1 });
        v.push(("health_conflict", ui.clone(), st, None));
        let mut st = with_aircraft();
        st.area_gnss = Some(AreaGnssView { jamming: true, degraded: 4, counted: 6, own_agrees: true, ..Default::default() });
        v.push(("health_area_gnss", ui.clone(), st.clone(), None));
        st.area_gnss = Some(AreaGnssView { spoofing: true, drones_moved: 2, ..Default::default() });
        v.push(("health_area_spoof", ui.clone(), st.clone(), None));
        st.conflict = Some(ConflictView { level: 1, drone: 1, aircraft: s("N911PH"), range_m: 2400.0, dz_m: Some(300.0), count: 0 });
        st.threats = Threats { active: 2, muted: 1, snoozable: 1, ..Threats::default() };
        st.caveats.silenced = Silenced { snoozed: 1, snooze_left_s: 245, ..Silenced::default() };
        // Five nodes: the busiest health page.
        st.nodes.push(NodeStatus { kind: NodeKind::Lr, alive: true, frames_per_s: 3, rid_found: 0, drops: 0, uptime_s: 900 }).unwrap();
        v.push(("health_busiest", ui, st, None));

        let st = base(true);
        let mut ui = Ui::default();
        after(&mut ui, &st, &[Button::Up], true);
        v.push(("settings_browse", ui.clone(), st.clone(), None));
        after(&mut ui, &st, &[Button::Select, Button::Down], false);
        v.push(("settings_edit", ui.clone(), st.clone(), None));
        let mut auto = Ui::new(Settings { theme: ThemeMode::Auto, ..Settings::default() });
        after(&mut auto, &st, &[Button::Up, Button::Select, Button::Down, Button::Down], false);
        let mut day = st.clone();
        day.position = Some((39.95, -75.16));
        v.push(("settings_auto", auto.clone(), day, None));
        let mut waiting = st.clone();
        waiting.utc_ms = None;
        v.push(("settings_auto_waiting_gps", auto.clone(), waiting, None));
        let mut st2 = base(true);
        st2.threats = Threats { active: 2, muted: 1, mutable: 2, ..Threats::default() };
        let mut ui = Ui::default();
        after(&mut ui, &st2, &[Button::Up, Button::Select, Button::Up, Button::Up], false);
        v.push(("settings_alarms", ui.clone(), st2.clone(), None));
        st2.threats = Threats { active: 1, jammed: true, muted: 2, snoozable: 1, ..Threats::default() };
        v.push(("settings_snooze", ui.clone(), st2.clone(), None));
        after(&mut ui, &st2, &[Button::Up, Button::Select], false);
        let mut pend = st2.clone();
        pend.gps.home_pending = true;
        v.push(("settings_nodes_done", ui, pend, None));

        let mut st = base(true);
        st.gps.integrity = GpsIntegrity::Spoofing;
        st.threats = Threats { active: 2, jammed: true, ..Threats::default() };
        let mut ui = Ui::default();
        after(&mut ui, &st, &[Button::Down], false);
        v.push(("threat_warnings", ui, st, None));

        let st = five_drones(true);
        let mut ui = Ui::default();
        after(&mut ui, &st, &[Button::Down], false);
        v.push(("page_toast", ui, st, None));

        let mut st = one_drone(true);
        st.gps = Gps { fix: Fix::Fix2D, sats: 4, ..Gps::default() };
        st.power = Power::Battery { pct: 9 };
        st.sd = SdLog::Error;
        st.utc_ms = None;
        for n in st.nodes.iter_mut() {
            n.alive = false;
        }
        v.push(("status_warnings", Ui::default(), st, None));
        let mut st = base(false);
        st.gps.fix = Fix::Absent;
        st.sd = SdLog::NoCard;
        st.power = Power::Usb { battery_pct: None };
        v.push(("status_gps_absent", Ui::default(), st, None));
        let mut st = one_drone(true);
        st.gps.fix = Fix::Manual;
        st.sd = SdLog::NoCard;
        v.push(("status_manual_pos", Ui::default(), st, None));

        let mut st = five_drones(true);
        st.caveats = Caveats {
            key: EvidenceKey::Test,
            bench: true,
            link_open: true,
            evidence: Some(EvidenceStop { why: EvidenceStopWhy::FileError, waiting: 1234, dropped: 56 }),
            silenced: Silenced { muted: 2, mute_left_s: 7_140, snoozed: 1, snooze_left_s: 245, site: 1 },
            switches: Switches { control_ids: true, control_content: true, audio_clips: false },
        };
        st.threats = Threats { active: 1, jammed: true, muted: 3, mutable: 1, ..Threats::default() };
        v.push(("caveats_all", Ui::default(), st.clone(), None));
        let mut ui = Ui::default();
        after(&mut ui, &st, &[Button::Down], true);
        v.push(("caveats_list", ui.clone(), st.clone(), None));
        after(&mut ui, &st, &[Button::Down, Button::Down], true);
        v.push(("caveats_health", ui, st.clone(), None));
        let mut ui = Ui::default();
        after(&mut ui, &st, &[Button::Up, Button::Select], false);
        ui.set_settings(Settings { theme: ThemeMode::Auto, ..Settings::default() });
        v.push(("caveats_settings_auto", ui, st, None));
        v
    }

    #[test]
    fn wide_snapshots() {
        for (name, ui, st, lit) in cases() {
            shot(name, &ui, &st, lit);
        }
    }

    /// Band by band gives exactly the full frame, at the P4's band height
    /// and at awkward ones.
    #[test]
    fn wide_banded_matches_full_frame() {
        for (name, ui, st, lit) in cases() {
            let (whole, _) = full(&ui, &st, lit);
            for rows in [wide::BAND_ROWS, 7, 1] {
                let mut band = vec![0x1234u16; WW * rows];
                let mut out = vec![0u16; WW * WH];
                let mut next = 0;
                wide::render_banded(&ui, &st, lit, &mut band, |top, px| {
                    assert_eq!(top, next, "{name}: bands out of order");
                    out[top as usize * WW..top as usize * WW + px.len()].copy_from_slice(px);
                    next = top + (px.len() / WW) as i32;
                    Ok::<(), ()>(())
                })
                .unwrap();
                assert_eq!(next, wide::H);
                if out != whole {
                    let i = out.iter().zip(&whole).position(|(a, b)| a != b).unwrap();
                    panic!("{name}: {rows}-row bands differ from the full frame at x {} y {}", i % WW, i / WW);
                }
            }
        }
    }

    /// The keys say what the state machine does, and a touch lands on the
    /// key under it (and nowhere in the page).
    #[test]
    fn keys_label_what_they_do_and_touches_hit_them() {
        use wide::{key_at, key_labels};
        let st = five_drones(true);
        let mut ui = Ui::default();
        let k = key_labels(&ui, &st);
        assert_eq!((k[0].text.as_str(), k[1].text.as_str(), k[2].text.as_str()), ("SETTINGS", "Choose drone", "DRONE LIST"));
        press(&mut ui, &st, &[Button::Select]);
        let k = key_labels(&ui, &st);
        assert_eq!((k[1].text.as_str(), k[1].hold), ("Open", Some("hold: back")));
        press(&mut ui, &st, &[Button::Select]);
        assert_eq!(ui.mode(), Mode::Detail);
        let k = key_labels(&ui, &st);
        assert_eq!((k[0].text.as_str(), k[1].text.as_str()), ("Prev drone", "Hold: back"));
        // The banner: Select opens, the others only dismiss.
        ui.new_drone(3, NOW);
        let k = key_labels(&ui, &st);
        assert_eq!((k[0].text.as_str(), k[1].text.as_str(), k[2].text.as_str()), ("Dismiss", "Show details", "Dismiss"));
        // Settings while editing: the row's action, and "hold: done".
        let mut ui = Ui::default();
        press(&mut ui, &st, &[Button::Up, Button::Select]);
        let k = key_labels(&ui, &st);
        assert_eq!((k[1].text.as_str(), k[1].hold), ("Change", Some("hold: done")));
        press(&mut ui, &st, &[Button::Up]);
        assert_eq!(key_labels(&ui, &st)[1].text.as_str(), "Place all here");
        // Empty radar: Select does nothing, and the key says so.
        let k = key_labels(&Ui::default(), &base(true));
        assert!(!k[1].active);

        assert_eq!(key_at(10, 470), Some(Button::Up));
        assert_eq!(key_at(400, 440), Some(Button::Select));
        assert_eq!(key_at(799, 479), Some(Button::Down));
        assert_eq!(key_at(266, 430), Some(Button::Up));
        assert_eq!(key_at(267, 430), Some(Button::Select));
        assert_eq!(key_at(400, 300), None, "the page is not a key");
        assert_eq!(key_at(400, wide::TOUCH_TOP - 1), None);
        assert_eq!(key_at(400, 480), None);
    }
}
