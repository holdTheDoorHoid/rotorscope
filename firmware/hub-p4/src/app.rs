//! The hub proper on the P4: tracker fed by the gateway link (drones) and
//! the ADSBee (aircraft), own position from the GPS or a manual position,
//! the shared screen UI on the panel, touch keys.

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
/// ADS-B aircraft (the tracker's default capacity, as the S3 hub with `adsbee`).
pub const MAX_AIRCRAFT: usize = tracker::aircraft::MAX_AIRCRAFT;
pub type HubTracker = Tracker<MAX_TRACKS, MAX_AIRCRAFT>;

/// Where the own position in use came from.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OwnSource {
    Gps,
    Manual,
}

/// The unit's position in use (GPS fix first, else a manual position).
#[derive(Clone, Copy)]
pub struct OwnPos {
    pub lat: f64,
    pub lon: f64,
    pub alt: Option<f64>,
    pub source: OwnSource,
    pub at_ms: u64,
}

/// What the screen needs from outside the tracker and the gateway link.
pub struct Ext {
    pub gps: screen::Gps,
    pub utc_ms: Option<u64>,
    pub sd: screen::SdLog,
    /// The ADSBee's health and uptime.
    pub adsb: Option<(tracker::json::AdsbStatus, u32)>,
    /// The FLAGS page (Phase 12).
    pub flags: Option<screen::FlagsView>,
}
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
    /// Set over the console (`p`), kept until cleared (`c`). Not kept across
    /// reboots.
    pub manual: Option<OwnPos>,
    /// New drones since the last report, for the `{"t":"new"}` lines.
    pub new_drones: heapless::Deque<u32, 8>,
    /// O4 DroneID decryption; detect-only until the SD card supplies the
    /// owner's key (`sd::Logger::take_o4_key`).
    pub o4: djicrypt::Decryptor<8>,
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
            manual: None,
            new_drones: heapless::Deque::new(),
            o4: djicrypt::Decryptor::default(),
        }
    }

    /// Hand queued link frames to the evidence log and the tracker.
    pub fn ingest(&mut self, gw: &mut GatewayLink<'_>, ev: &mut crate::evlog::Evidence, now: u64) {
        while let Some((node, f)) = gw.out.pop_front() {
            let relay = node.checked_sub(1).and_then(|i| gw.nodes.get(i as usize)).map(|n| (n.mac, n.rssi));
            ev.frame(node, relay, &f);
            self.tracker.ingest_frame(node, &f, now);
            if let link::Frame::DroneId(link::DroneIdSighting {
                record: link::DroneIdRecord::O4 { key_id, object, .. },
                ..
            }) = &f
                && self.o4.has_key_or_sessions()
            {
                self.decrypt_o4(*key_id, object, ev, now);
            }
        }
        self.announce_new(now);
    }

    /// Decrypt one O4 record with the owner's key and fold it into its
    /// track; log a derived evidence record (the raw frame, ciphertext
    /// included, was logged just before). The key is never logged.
    fn decrypt_o4(&mut self, key_id: [u8; 4], object: &[u8], ev: &mut crate::evlog::Evidence, now: u64) {
        use core::fmt::Write;
        use djicrypt::Plain;
        use tracker::O4Decrypted;
        let plain = match self.o4.process(object, now) {
            Ok(p) => p,
            Err(djicrypt::Error::NoSession | djicrypt::Error::NoKey) => return,
            Err(e) => {
                crate::println!("hub-p4: O4 {:02x}{:02x}{:02x}{:02x} not decrypted: {e:?}", key_id[0], key_id[1], key_id[2], key_id[3]);
                return;
            }
        };
        let (rec, sel) = match &plain {
            Plain::Cryp(a) => (O4Decrypted::Cryp(a), 0x13u8),
            Plain::Infp10(t) => (O4Decrypted::Infp10(t), 0x10),
            Plain::Infp11(p) => (O4Decrypted::Infp11(p), 0x11),
        };
        self.tracker.ingest_o4_decrypted(key_id, rec, now);
        let mut text: heapless::String<160> = heapless::String::new();
        let deg = droneid::payload::rad_e7_to_deg;
        let _ = match &plain {
            Plain::Cryp(a) => write!(
                text,
                "CRYP sn={} pos={:.6},{:.6} valid={}",
                a.serial_str().unwrap_or("?"),
                deg(a.lat_raw),
                deg(a.lon_raw),
                a.position_valid()
            ),
            Plain::Infp10(t) => write!(
                text,
                "INFP-10 sn={} pos={:.6},{:.6} alt={}m h={:.1}m pilot={:.6},{:.6} type={} state={:04x}",
                t.serial_str().unwrap_or("?"),
                t.lat(),
                t.lon(),
                t.altitude_m,
                t.height_m(),
                deg(t.app_lat_raw),
                deg(t.app_lon_raw),
                t.product_type,
                t.state
            ),
            Plain::Infp11(p) => write!(text, "INFP-11 sn={} purpose", p.serial_str().unwrap_or("?")),
        };
        let mut data = [0u8; 5];
        data[..4].copy_from_slice(&key_id);
        data[4] = sel;
        ev.event(evidence::format::EventCode::O4Decrypted, text.as_bytes(), &data);
        self.announce_new(now);
    }

    /// New drones to the screen alert and the console, once per drone. Also
    /// called at 1 Hz: a MAC-only track becomes due only after its grace.
    pub fn announce_new(&mut self, now: u64) {
        while let Some(id) = self.tracker.take_new_drone(now) {
            self.ui.new_drone(id.0, now);
            if self.new_drones.is_full() {
                self.new_drones.pop_front();
            }
            let _ = self.new_drones.push_back(id.0);
        }
    }

    /// The position in use: a fresh GPS fix wins; the manual position is
    /// the fallback (S3 hub policy, `firmware/hub/src/hub.rs`).
    pub fn own_pos(&self, gps: &crate::gps::Gps, now: u64) -> Option<OwnPos> {
        let max_age = self.tracker.config().own_fix_max_age_ms;
        gps.fresh_fix(now, max_age)
            .map(|f| OwnPos { lat: f.lat, lon: f.lon, alt: f.alt_hae_m, source: OwnSource::Gps, at_ms: f.at_ms })
            .or(self.manual)
    }

    /// Once a second: own position into the tracker (a manual one
    /// re-stamped so the tracker's fix timeout never drops it), expire old
    /// tracks, announce new drones.
    pub fn housekeeping(&mut self, gps: &crate::gps::Gps, now: u64) {
        match self.own_pos(gps, now) {
            Some(p) => {
                let stamp = if p.source == OwnSource::Manual { now } else { p.at_ms };
                self.tracker.set_own_position(p.lat, p.lon, p.alt, stamp);
            }
            None => self.tracker.clear_own_position(),
        }
        self.tracker.expire(now);
        self.announce_new(now);
    }

    /// Redraw at once (e.g. the own position changed).
    pub fn redraw_now(&mut self) {
        self.last_frame = 0;
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
                crate::println!("hub-p4: touch x={} y={}", t.x, t.y);
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
    pub fn draw(
        &mut self,
        panel: &mut Panel,
        gw: &mut GatewayLink<'_>,
        ext: &Ext,
        now: u64,
        mut between: impl FnMut(&mut GatewayLink<'_>),
    ) {
        self.ui.tick(now);
        if now.saturating_sub(self.last_frame) < FRAME_MS {
            return;
        }
        self.last_frame = now;
        self.fill_state(gw, ext, now);
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

    fn fill_state(&mut self, gw: &GatewayLink<'_>, ext: &Ext, now: u64) {
        let ctx = self.tracker.context(now, ext.utc_ms);
        // Before fill_from_tracker: it clears a fix the tracker has no
        // position for.
        self.state.gps = ext.gps;
        screen::from_tracker::fill_from_tracker(self.tracker, &ctx, &mut self.state);
        self.state.sd = ext.sd;
        self.state.flags = ext.flags.clone();
        // No battery sense on this board (the BAT connector's voltage is not
        // wired to a GPIO): report USB power, as a bench S3 does.
        self.state.power = screen::Power::Usb { battery_pct: None };
        self.state.hub_uptime_s = (now / 1000) as u32;
        self.state.nodes.clear();
        self.state.assigned.clear();
        for n in gw.nodes.iter() {
            let kind = match n.role {
                Some(link::NodeRole::Wifi5) => NodeKind::Wifi5,
                Some(link::NodeRole::Ble) => NodeKind::Ble,
                Some(link::NodeRole::Fpv) => NodeKind::Fpv,
                Some(link::NodeRole::DroneId) => NodeKind::DroneId,
                Some(link::NodeRole::Gateway) => NodeKind::Gateway,
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
        // The ADSBee as a node, and the aircraft line.
        if let Some((a, up)) = ext.adsb {
            let _ = self.state.nodes.push(NodeStatus {
                kind: NodeKind::Adsb,
                alive: a.alive,
                frames_per_s: a.frames_per_s.unwrap_or(0).min(u32::from(u16::MAX)) as u16,
                rid_found: 0,
                drops: a.errors,
                uptime_s: up,
            });
            let _ = self.state.assigned.push(None);
            self.state.aircraft = Some(screen::from_tracker::aircraft_summary(self.tracker.aircraft(), &ctx));
        } else {
            self.state.aircraft = None;
        }
        let _ = (links::BAUD, Rgb565::BLACK);
    }
}
