//! The hub proper on the P4: tracker fed by the gateway link, the shared
//! screen UI on the panel, touch keys.

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::RgbColor;
use esp_hal::time::Instant;
use screen::{Band, NodeKind, NodeStatus, Ui, UiState};
use tracker::Tracker;

use crate::bb::BbI2c;
use crate::links::GatewayLink;
use crate::rgb::Panel;
use crate::touch::Gt911;
use crate::{display, links};

pub const MAX_TRACKS: usize = 64;
pub type HubTracker = Tracker<MAX_TRACKS, 0>;
/// A relayed node not heard for this long shows as down.
const NODE_ALIVE_MS: u64 = 5_000;
/// Redraw at most this often (and at once after a key).
const FRAME_MS: u64 = 250;
/// Holding SELECT this long is Back.
const HOLD_MS: u64 = 700;
/// No touch report for this long while SELECT is held counts as released.
const RELEASE_GAP_MS: u64 = 250;
const BAND_PX: usize = screen::WIDTH as usize * screen::BAND_ROWS;

pub fn now_ms() -> u64 {
    Instant::now().duration_since_epoch().as_millis()
}

pub struct App {
    pub tracker: &'static mut HubTracker,
    ui: Ui,
    state: UiState,
    touch: Gt911,
    band: &'static mut [Rgb565; BAND_PX],
    last_frame: u64,
    lit: Option<(screen::Button, u64)>,
    /// SELECT is down since this time; true once it has fired Back.
    sel_down: Option<(u64, bool)>,
    last_touch: u64,
    pub frames_drawn: u32,
    pub max_band_us: u64,
}

impl App {
    pub fn new(tracker: &'static mut HubTracker, band: &'static mut [Rgb565; BAND_PX]) -> Self {
        Self {
            tracker,
            ui: Ui::new(Default::default()),
            state: UiState::default(),
            touch: Gt911::default(),
            band,
            last_frame: 0,
            lit: None,
            sel_down: None,
            last_touch: 0,
            frames_drawn: 0,
            max_band_us: 0,
        }
    }

    /// Hand queued link frames to the tracker.
    pub fn ingest(&mut self, gw: &mut GatewayLink<'_>, now: u64) {
        while let Some((node, f)) = gw.out.pop_front() {
            match f {
                link::Frame::Detection(det) => {
                    self.tracker.ingest(node, &det, now);
                }
                link::Frame::OpenLink(s) => {
                    self.tracker.ingest_open_link(node, &s, now);
                }
                _ => {}
            }
        }
        while let Some(id) = self.tracker.take_new_drone(now) {
            self.ui.new_drone(id.0, now);
        }
    }

    /// Poll touch; a press on a key is a button event.
    ///
    /// UP and DOWN act at once. SELECT waits: released before [`HOLD_MS`] it
    /// is Select; still held at [`HOLD_MS`] it is Back (once), like the long
    /// press on the S3 hub's button.
    pub fn touch(&mut self, i2c: &mut BbI2c<'_>, now: u64) {
        if let Some(t) = self.touch.poll(i2c) {
            self.last_touch = now;
            if t.pressed {
                esp_println::println!("hub-p4: touch x={} y={}", t.x, t.y);
                match display::key_at(t.x, t.y) {
                    Some(screen::Button::Select) => {
                        self.sel_down = Some((now, false));
                        self.light(screen::Button::Select, now);
                    }
                    Some(b) => self.press(b, now),
                    None => {}
                }
            }
        }
        let Some((start, fired)) = self.sel_down else { return };
        // Released: the chip says so, or (if that report was missed) it has
        // sent nothing for a while; it reports continuously while touched.
        let released = !self.touch.is_down() || now.saturating_sub(self.last_touch) > RELEASE_GAP_MS;
        if released {
            self.sel_down = None;
            if !fired {
                self.press(screen::Button::Select, now);
            }
        } else if !fired && now.saturating_sub(start) >= HOLD_MS {
            self.sel_down = Some((start, true));
            self.press(screen::Button::Back, now);
        }
    }

    fn press(&mut self, b: screen::Button, now: u64) {
        self.ui.button(b, &self.state);
        self.light(b, now);
    }

    fn light(&mut self, b: screen::Button, now: u64) {
        // Back lights the SELECT key: it is the key being held.
        let key = if b == screen::Button::Back { screen::Button::Select } else { b };
        self.lit = Some((key, now));
        self.last_frame = 0; // redraw now
    }

    /// Redraw if due. Calls `between` after each band so the caller can
    /// service the link (a whole frame takes longer than the UART FIFO lasts).
    pub fn draw(&mut self, panel: &mut Panel, gw: &mut GatewayLink<'_>, now: u64, mut between: impl FnMut(&mut GatewayLink<'_>)) {
        self.ui.tick(now);
        if now.saturating_sub(self.last_frame) < FRAME_MS {
            return;
        }
        self.last_frame = now;
        self.fill_state(gw, now);
        if self.lit.is_some_and(|(_, t)| now.saturating_sub(t) > 300) {
            self.lit = None;
        }
        let mut band = Band::new(&mut self.band[..]);
        let mut top = 0;
        while top < screen::HEIGHT {
            let t0 = Instant::now();
            band.set_top(top);
            let _ = screen::render(&self.ui, &self.state, &mut band);
            display::blit_band(panel.fb_mut(), &band);
            self.max_band_us = self.max_band_us.max(t0.elapsed().as_micros());
            between(gw);
            top += screen::BAND_ROWS as i32;
        }
        display::draw_keys(panel.fb_mut(), self.lit.map(|(b, _)| b));
        panel.flush();
        self.frames_drawn += 1;
    }

    fn fill_state(&mut self, gw: &GatewayLink<'_>, now: u64) {
        let ctx = self.tracker.context(now, None);
        screen::from_tracker::fill_from_tracker(self.tracker, &ctx, &mut self.state);
        self.state.hub_uptime_s = (now / 1000) as u32;
        self.state.nodes.clear();
        self.state.assigned.clear();
        for n in gw.nodes.iter() {
            let kind = match n.role {
                Some(link::NodeRole::Wifi5) => NodeKind::Wifi5,
                Some(link::NodeRole::Ble) => NodeKind::Ble,
                Some(link::NodeRole::Fpv) => NodeKind::Fpv,
                Some(link::NodeRole::DroneId) => NodeKind::DroneId,
                _ => NodeKind::Wifi24,
            };
            let _ = self.state.assigned.push(n.assigned.map(|(role, plan)| screen::Assigned {
                role: crate::downlink::label(role),
                plan,
                applied: n.applied,
            }));
            let _ = self.state.nodes.push(NodeStatus {
                kind,
                alive: now.saturating_sub(n.last_ms) < NODE_ALIVE_MS,
                frames_per_s: 0,
                rid_found: n.rid_found,
                drops: n.dropped,
                uptime_s: n.uptime_s,
            });
        }
        let _ = (links::BAUD, Rgb565::BLACK);
    }
}
