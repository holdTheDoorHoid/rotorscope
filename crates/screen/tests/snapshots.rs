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
            }
        } else {
            Gps {
                fix: Fix::None,
                sats: 2,
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
    let mut per = [None; 5];
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
        theme: Theme::Day,
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

#[test]
fn health() {
    let mut st = five_drones(true);
    st.nodes[1].alive = false;
    st.power = Power::Usb {
        battery_pct: Some(100),
    };
    let mut ui = Ui::default();
    press(&mut ui, &st, &[Button::Down, Button::Down]);
    ui.tick(NOW + 5_000);
    shot("health", &ui, &st);
}

#[test]
fn settings() {
    let st = base(true);
    let mut ui = Ui::default();
    press(&mut ui, &st, &[Button::Up, Button::Select, Button::Down]);
    shot("settings", &ui, &st);
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
    };
    st.power = Power::Battery { pct: 9 };
    st.sd = SdLog::Error;
    st.utc_ms = None;
    for n in st.nodes.iter_mut() {
        n.alive = false;
    }
    shot("status_warnings", &Ui::default(), &st);
}
