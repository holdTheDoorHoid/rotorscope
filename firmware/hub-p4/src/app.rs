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
    /// Key class, bench build, open link, evidence not written: kept on
    /// screen while they hold.
    pub caveats: screen::Caveats,
}

/// Over-the-air text (a drone ID, a callsign, an SSID, a decrypted serial)
/// inside a hub-built line, copied with every character outside printable
/// ASCII shown as `?` (`link::printable`), before it goes to the console or
/// into evidence text. Nodes clean what they send; the hub does not rely on
/// it.
fn clean<const N: usize>(text: &str) -> heapless::String<N> {
    use core::fmt::Write;
    let mut out = heapless::String::new();
    let _ = write!(out, "{}", link::printable::Printable(text));
    out
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
    /// Set over the console (`p`, bench builds with `testpoint` only), kept
    /// until cleared (`c`). Not kept across reboots.
    pub manual: Option<OwnPos>,
    /// New drones since the last report, for the `{"t":"new"}` lines.
    pub new_drones: heapless::Deque<u32, 8>,
    /// O4 DroneID decryption; detect-only until the SD card supplies the
    /// owner's key (`sd::Logger::take_o4_key`).
    pub o4: djicrypt::Decryptor<8>,
    /// Aircraft from aero (LR2021) nodes (`aero.rs`).
    pub aero: crate::aero::Aero,
    /// The control-link opt-ins in force (read IDs, decode content), for
    /// the `{"t":"control_stats"}` line.
    pub control_flags: (bool, bool),
    /// Drone control links and control-band energy from LR2021 nodes
    /// (protocol 12), in PSRAM (`aero::Store`); `None` without PSRAM.
    pub control: Option<&'static mut tracker::ctrllinks::ControlTable>,
    /// GPS jamming / spoofing in the area seen through aircraft and drones
    /// (`tracker::area_gnss`), in PSRAM (`aero::Store`); `None` without.
    pub area: Option<&'static mut tracker::area_gnss::AreaWatch>,
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
            aero: crate::aero::Aero::default(),
            control_flags: (false, false),
            control: None,
            area: None,
        }
    }

    /// Hand queued link frames to the evidence log and the tracker; aero
    /// nodes' aircraft frames (their own queue) to `aero.rs` too.
    pub fn ingest(&mut self, gw: &mut GatewayLink<'_>, ev: &mut crate::evlog::Evidence, actx: &mut crate::aero::Ctx<'_>, now: u64) {
        // One loop, so `take` is inlined once (its buffers once on the stack).
        loop {
            let (node, f) = match gw.out.pop_front() {
                Some(x) => x,
                None => match gw.air.as_deref_mut().and_then(|q| q.pop_front()) {
                    Some((node, a)) => (node, link::Frame::Air(a)),
                    None => break,
                },
            };
            self.take(gw, ev, actx, node, f, now);
        }
        self.announce_new(now);
        self.announce_threats(gw, ev, now);
        self.announce_spoof(ev, now);
        self.announce_disputes(ev, now);
        self.announce_conflicts(ev, now);
    }

    /// One frame: the evidence log first (raw bytes, always), then whoever
    /// decodes it. Inlined into the main loop as the loop body was before
    /// (a frame of its own here would add ~3 KB to every path below it,
    /// O4 decryption included; the stack is tight).
    #[inline(never)]
    fn take(
        &mut self,
        gw: &mut GatewayLink<'_>,
        ev: &mut crate::evlog::Evidence,
        actx: &mut crate::aero::Ctx<'_>,
        node: u8,
        f: link::Frame,
        now: u64,
    ) {
        let relay = node.checked_sub(1).and_then(|i| gw.nodes.get(i as usize)).map(|n| (n.mac, n.rssi));
        ev.frame(node, relay, &f);
        self.tracker.ingest_frame(node, &f, now);
        match &f {
            link::Frame::DroneId(link::DroneIdSighting { record: link::DroneIdRecord::O4 { key_id, object, .. }, .. })
                if self.o4.has_key_or_sessions() =>
            {
                self.decrypt_o4(*key_id, object, ev, now);
            }
            link::Frame::Air(a) => self.aero.on_air(a, actx, self.tracker, gw.feed.as_mut(), now),
            link::Frame::AeroStatus(st) => self.aero.on_status(relay.map_or([0; 6], |r| r.0), st, now),
            link::Frame::Control(_) | link::Frame::ControlEnergy(_) => {
                if let Some(ct) = self.control.as_deref_mut() {
                    ct.ingest_frame(self.tracker, node, &f, now);
                }
            }
            _ => {}
        }
    }

    /// Drones close to crewed aircraft: one evidence event and one console
    /// line per pair and level (again after `conflict::REALERT_MS`).
    fn announce_conflicts(&mut self, ev: &mut crate::evlog::Evidence, now: u64) {
        while let Some(c) = self.tracker.take_new_conflict(now) {
            let Some(drone) = self.tracker.get(c.drone) else { continue };
            // Drone ID and callsign arrived over the air.
            let text: heapless::String<160> = clean(&tracker::conflict::describe(&c, drone, self.tracker.get_aircraft(c.aircraft)));
            let mut data = [0u8; 10];
            data[0] = c.level as u8;
            data[1..5].copy_from_slice(&c.drone.0.to_le_bytes());
            data[5..9].copy_from_slice(&c.aircraft.icao.to_le_bytes());
            data[9] = c.aircraft.source as u8;
            ev.event(evidence::format::EventCode::AircraftConflict, text.as_bytes(), &data);
            crate::println!("hub-p4: AIRCRAFT {}", text);
        }
    }

    /// Drone tracks whose broadcasts look forged or replayed: one evidence
    /// event and one console line each time a track's suspicion rises.
    fn announce_spoof(&mut self, ev: &mut crate::evlog::Evidence, now: u64) {
        while let Some(e) = self.tracker.take_spoof_flag(now) {
            // The track's ID arrived over the air.
            let text: heapless::String<192> = clean(&tracker::spoof::event_text(&e, self.tracker.get(e.track)));
            ev.event(evidence::format::EventCode::SpoofSuspect, text.as_bytes(), &e.data());
            crate::println!("hub-p4: SPOOF? {}", text);
        }
    }

    /// Drone IDs claimed by two transmitters (tracker `claims`): one
    /// evidence event and one console line per dispute, and again when it
    /// grows. Worded as a dispute over the ID, never as the drone spoofing.
    fn announce_disputes(&mut self, ev: &mut crate::evlog::Evidence, now: u64) {
        while let Some(d) = self.tracker.take_dispute(now) {
            // The track's ID arrived over the air.
            let text: heapless::String<192> = clean(&tracker::claims::event_text(&d, self.tracker.get(d.track)));
            ev.event(evidence::format::EventCode::IdDisputed, text.as_bytes(), &d.data());
            crate::println!("hub-p4: DISPUTED {}", text);
        }
    }

    /// New attacks and jamming (protocol 9, and the hub's own deaf-node
    /// check): one evidence event and one console line each, once.
    fn announce_threats(&mut self, gw: &GatewayLink<'_>, ev: &mut crate::evlog::Evidence, now: u64) {
        use core::fmt::Write;
        while let Some(id) = self.tracker.take_new_threat(now) {
            let Some(t) = self.tracker.get_threat(id) else { continue };
            let mac = t.node.checked_sub(1).and_then(|i| gw.nodes.get(i as usize)).map(|n| n.mac).unwrap_or([0; 6]);
            if t.kind == link::ThreatKind::AreaGnss {
                // Raised by the hub from aircraft and drones: its own text.
                let text = tracker::area_gnss::describe(t);
                let mut data = [0u8; 7];
                data[0] = t.kind as u8;
                ev.event(evidence::format::EventCode::Threat, text.as_bytes(), &data);
                crate::println!("hub-p4: THREAT {}", text);
                continue;
            }
            if t.kind == link::ThreatKind::IdFlood {
                // Raised by the tracker itself (WP-09, `tracker::probation`):
                // a flood of forged drone IDs, aircraft addresses or
                // control links, with the rate seen.
                let text = tracker::probation::describe(t);
                let mut data = [0u8; 7];
                data[0] = t.kind as u8;
                ev.event(evidence::format::EventCode::Threat, text.as_bytes(), &data);
                crate::println!("hub-p4: THREAT {}", text);
                continue;
            }
            let mut text: heapless::String<192> = heapless::String::new();
            let _ = write!(text, "{} (node {}", t.label(), t.node);
            if t.channel != 0 {
                let _ = write!(text, ", ch {}", t.channel);
            }
            if t.freq != 0 {
                let _ = write!(text, ", {:.1} MHz", t.freq_mhz());
            }
            let _ = if let Some(d) = t.contested() {
                // A control link with more transmitters than it can have.
                write!(
                    text,
                    ", {}, {} transmitters, {}, {} events, {} dBm)",
                    d.system.map_or("?", |s| s.label()),
                    d.transmitters,
                    ctrlwatch::contested::rule_name(d.rules),
                    t.count,
                    t.level
                )
            } else if t.kind.is_jamming() {
                write!(text, ", +{} dB, detail {})", t.level, t.detail)
            } else {
                write!(text, ", {} frames, {} dBm, detail {})", t.count, t.level, t.detail)
            };
            if !t.ssid.is_empty() {
                // From a link frame: never printed or logged as received.
                let _ = write!(text, " \"{}\"", link::printable::Printable(&t.ssid));
            }
            if let Some(maker) = tracker::threats::drone_maker(t) {
                let _ = write!(text, " ({maker})");
            }
            let mut data = [0u8; 7];
            data[0] = t.kind as u8;
            data[1..].copy_from_slice(&mac);
            ev.event(evidence::format::EventCode::Threat, text.as_bytes(), &data);
            crate::println!("hub-p4: THREAT {}", text);
        }
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
            // Counted in the decryptor's stats; not worth a console line
            // each (a replay or a flood of forged CRYPs would fill it).
            Err(
                djicrypt::Error::NoSession
                | djicrypt::Error::NoKey
                | djicrypt::Error::Replay
                | djicrypt::Error::TooFarAhead
                | djicrypt::Error::RateLimited,
            ) => return,
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
        // The serial numbers come out of a radio payload: shown cleaned.
        use link::printable::Printable as P;
        let _ = match &plain {
            Plain::Cryp(a) => write!(
                text,
                "CRYP sn={} pos={:.6},{:.6} valid={}",
                P(a.serial_str().unwrap_or("?")),
                deg(a.lat_raw),
                deg(a.lon_raw),
                a.position_valid()
            ),
            Plain::Infp10(t) => write!(
                text,
                "INFP-10 sn={} pos={:.6},{:.6} alt={}m h={:.1}m pilot={:.6},{:.6} type={} state={:04x}",
                P(t.serial_str().unwrap_or("?")),
                t.lat(),
                t.lon(),
                t.altitude_m,
                t.height_m(),
                deg(t.app_lat_raw),
                deg(t.app_lon_raw),
                t.product_type,
                t.state
            ),
            Plain::Infp11(p) => write!(text, "INFP-11 sn={} purpose", P(p.serial_str().unwrap_or("?"))),
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
        // The spoofing checks compare broadcast timestamps with GPS time.
        match gps.utc_ms(now) {
            Some(utc) => self.tracker.set_utc(utc, now),
            None => self.tracker.clear_utc(),
        }
        self.tracker.expire(now);
        if let Some(ct) = self.control.as_deref_mut() {
            ct.expire(now, self.tracker.config());
        }
        // GPS interference in the area: the hub's own GPS verdict as
        // corroboration (only while a receiver is there), then the check;
        // a raised threat is announced and logged with the others.
        if let Some(area) = self.area.as_deref_mut() {
            use gnss::IntegrityState as I;
            use tracker::area_gnss::OwnGnss as O;
            if gps.status.state != tracker::json::GpsState::Absent {
                let own = match gps.integrity().state {
                    I::Ok => O::Ok,
                    I::SuspectJamming => O::SuspectJamming,
                    I::Jamming => O::Jamming,
                    I::SuspectSpoofing => O::SuspectSpoofing,
                    I::Spoofing => O::Spoofing,
                };
                area.set_own_gnss(own, now);
            }
            area.tick(self.tracker, now);
        }
        self.announce_new(now);
    }

    /// The action the operator chose on the settings page (once).
    pub fn take_request(&mut self) -> Option<screen::Request> {
        self.ui.take_request()
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
        if let Some(ct) = self.control.as_deref() {
            screen::from_tracker::fill_control(ct, &ctx, &mut self.state);
        }
        self.state.sd = ext.sd;
        self.state.caveats = ext.caveats;
        self.state.flags = ext.flags.clone();
        self.state.threats = screen::Threats {
            active: self.tracker.active_threats(now).count().min(255) as u8,
            jammed: self.tracker.partly_blind(now),
            // WP-09: a flood of forged drone IDs, shown as FLOOD in the bar.
            flood_per_s: self.tracker.flood_status(now).map_or(0, |f| f.new_per_s.min(u32::from(u16::MAX)) as u16),
            muted: self.tracker.threats().iter().filter(|t| t.muted && t.active(now)).count().min(255) as u8,
        };
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
                Some(link::NodeRole::Aero) => NodeKind::Lr,
                _ => NodeKind::Wifi24,
            };
            // An aero node's line says what its radio is doing right now.
            let aero = self.aero.status_of(&n.mac).map(crate::aero::screen_label);
            let _ = self.state.assigned.push(n.assigned.map(|(role, plan)| screen::Assigned {
                role: aero.unwrap_or(crate::downlink::label(role)),
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
            self.state.conflict = screen::from_tracker::conflict_view(&self.tracker.conflicts(now), self.tracker.aircraft());
        } else {
            self.state.aircraft = None;
            self.state.conflict = None;
        }
        self.state.area_gnss = self.area.as_deref().and_then(|a| screen::from_tracker::area_gnss_view(a.status()));
        let _ = (links::BAUD, Rgb565::BLACK);
    }
}
