//! GPS on the P4 hub: a u-blox MAX-M10S or a Quectel LC29H(AA) on the UART3-IN header, PPS
//! on a GPIO interrupt. **Untested with a real module**: built, host-tested
//! (`crates/gnss`) and loop-back tested on the chip only (`--features
//! gpsloop` / `gpsloop-m10`, see `gpsloop.rs` and BRINGUP.md).
//!
//! Wiring (PARITY.md, BRINGUP.md "GPS and ADS-B pins"):
//! - UART3-IN header (XH2.54): GPS TX -> GPIO28 (hub RX), hub TX GPIO27 ->
//!   GPS RX, GND. Driven by the UART1 peripheral. **That header's power pin
//!   is 5 V**: power a 3.3 V-only GPS from the UART1 or I2C header's 3V3.
//!   (Until 2026-11-17 the GPS had the UART1 header and the nodes UART3-IN.)
//! - PPS -> GPIO29 (wireless-module socket pin, free with no module fitted).
//!
//! The bytes go through `crates/gnss`; this module is the superloop driver:
//!
//! 1. **Hunt.** Listen at 115200 (LC29H default; a configured M10), then
//!    9600 (M10 as shipped), then 38400. Valid frames within
//!    [`HUNT_MS`] mean present; nothing at any rate means absent, and the
//!    hunt keeps cycling (a module plugged in later is found).
//! 2. **Detect.** Send `gnss::module::probe_bytes` (UBX MON-VER poll and
//!    `$PQTMVERNO`); the first answer names the module. No answer after
//!    three tries while NMEA flows: a generic NMEA receiver, used as is.
//! 3. **Configure.** `Module::sequence`: one step at a time, each confirmed
//!    (1.2 s, three tries). M10: UBX NAV-PVT, NMEA off, PPS, then 115200.
//!    LC29H: GGA/RMC/GSA/GSV on, GLL/VTG off, PPS with 3D fix only. RAM
//!    (and the M10's battery-backed RAM) only; never the module's flash.
//! 4. **Run.** Fixes and UTC are published; [`SILENT_MS`] without a valid
//!    frame starts over at 1.
//!
//! UTC is anchored to the PPS edge when one arrived within the last second
//! (`utc_src` `gps_pps`), else to the message's arrival (`gps`), exactly as
//! the S3 hub (`firmware/hub/src/gnss.rs`).
//!
//! **Integrity** (docs/GNSS-INTEGRITY.md): every parser event also goes to
//! `gnss::Monitor`, which watches for jamming and spoofing of this receiver,
//! and checks GPS time against the hub's own clock (`gnss::timeguard`, PPS
//! edge included) and against the build date and the remembered time.
//! [`Gps::integrity`] is its verdict. What is published is decided by
//! `gnss::trust::Publisher` (WP-12):
//! - Only an epoch the monitor **trusts** (no spoofing check fired for ten
//!   epochs in a row, no spoofing state) moves [`Gps::fix`] and [`Gps::time`].
//! - Otherwise the last trusted ones are held: [`Fix::held`] set (while
//!   spoofing is suspected, the home position if there is one), [`Gps::time`]
//!   at the last trusted anchor so UTC runs on from the hub's own clock
//!   ([`Gps::time_held`]; the evidence log says `free_running` after 2.5 s).
//! - With **nothing trusted** this boot, nothing is published: `fix` and
//!   `time` are `None` ([`Gps::untrusted_pos`], [`Gps::untrusted_time`] say
//!   why, for the screen, the status line and the evidence context). Every
//!   consumer already copes with no GPS.
//! - The receiver status (`status`: fix type, satellites) still shows what
//!   the receiver says.
//!
//! Home and the latest trusted time are remembered on the card
//! (`ROTORSCP/GPSMEM.TXT`, written through the SD logger: [`Gps::memory_text`]).
//! The integrity steps of the configuration are optional: a module that
//! refuses them is still used, just with fewer checks.

use core::cell::{Cell, RefCell};
use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

use critical_section::Mutex;
use esp_hal::gpio::{Event as GpioEvent, Flex, Pull};
use esp_hal::time::Instant;
use esp_hal::{handler, ram};
use gnss::{ConfigOptions, Event, FixType, Lc29hOptions, Module, Parser, Source, Step, Steps};
use tracker::json::{GpsState, GpsStatus};

use crate::uartring::gps_rx;

/// Rates to try, in order.
pub const BAUDS: [u32; 3] = [115_200, 9_600, 38_400];
/// Listen this long at one rate for a valid frame.
const HUNT_MS: u64 = 2_500;
/// Wait this long for an answer to the probe, and try this often.
const PROBE_WAIT_MS: u64 = 1_200;
const PROBE_TRIES: u8 = 3;
const ACK_TIMEOUT_MS: u64 = 1_200;
const ACK_TRIES: u8 = 3;
/// A present receiver with no valid frame for this long: start over.
const SILENT_MS: u64 = 5_000;

// ------------------------------------------------------------------ PPS --

static PPS_PIN: Mutex<RefCell<Option<Flex<'static>>>> = Mutex::new(RefCell::new(None));
/// Last PPS rising edge, microseconds since boot.
static PPS_LAST_US: Mutex<Cell<Option<u64>>> = Mutex::new(Cell::new(None));
pub static PPS_COUNT: AtomicU32 = AtomicU32::new(0);

#[handler]
#[ram]
fn on_gpio() {
    let t = Instant::now().duration_since_epoch().as_micros();
    critical_section::with(|cs| {
        if let Some(pin) = PPS_PIN.borrow_ref_mut(cs).as_mut()
            && pin.is_interrupt_set()
        {
            pin.clear_interrupt();
            PPS_LAST_US.borrow(cs).set(Some(t));
            PPS_COUNT.fetch_add(1, Relaxed);
        }
    });
}

/// Start timestamping rising edges on `pin` (pulled down: quiet with nothing
/// connected). Installs the GPIO interrupt handler; nothing else on this hub
/// uses GPIO interrupts.
pub fn pps_init(io: &mut esp_hal::gpio::Io<'_>, mut pin: Flex<'static>) {
    io.set_interrupt_handler(on_gpio);
    pin.apply_input_config(&esp_hal::gpio::InputConfig::default().with_pull(Pull::Down));
    pin.set_input_enable(true);
    critical_section::with(|cs| {
        pin.listen(GpioEvent::RisingEdge);
        PPS_PIN.borrow_ref_mut(cs).replace(pin);
    });
}

pub fn pps_last_us() -> Option<u64> {
    critical_section::with(|cs| PPS_LAST_US.borrow(cs).get())
}

/// Loop-back test only: drive the PPS pad from inside the chip (the input
/// stays enabled, so the edge interrupt sees it).
#[cfg(feature = "gpsloop")]
pub fn pps_drive(high: bool) {
    critical_section::with(|cs| {
        if let Some(pin) = PPS_PIN.borrow_ref_mut(cs).as_mut() {
            pin.set_level(high.into());
            pin.set_output_enable(true);
        }
    });
}

// ----------------------------------------------------------------- state --

/// One usable GPS solution.
#[derive(Clone, Copy)]
pub struct Fix {
    pub lat: f64,
    pub lon: f64,
    /// Height above the WGS-84 ellipsoid (Remote ID altitudes are geodetic).
    pub alt_hae_m: Option<f64>,
    pub at_ms: u64,
    /// Horizontal accuracy estimate (UBX only) and HDOP (NMEA only), for
    /// the evidence log's fix quality.
    pub hacc_m: Option<f32>,
    pub hdop: Option<f32>,
    /// Not from the receiver's latest epoch: held while the integrity monitor
    /// suspects spoofing (the learned home, or the last trusted fix).
    pub held: bool,
}

/// UTC anchored to hub time.
#[derive(Clone, Copy)]
pub struct TimeAnchor {
    pub utc_ms: u64,
    pub at_ms: u64,
    pub pps: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Hunt,
    Detect,
    Configure,
    Run,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cfg {
    None,
    Running,
    Ok,
    Nak,
    NoAck,
}

impl Cfg {
    pub fn name(self) -> &'static str {
        match self {
            Cfg::None => "none",
            Cfg::Running => "running",
            Cfg::Ok => "ok",
            Cfg::Nak => "nak",
            Cfg::NoAck => "no_ack",
        }
    }
}

pub struct Gps {
    parser: Parser,
    pub phase: Phase,
    pub baud_idx: usize,
    phase_since: u64,
    /// Valid-frame counter at the last tick, and when the phase began.
    frames_at: u32,
    phase_frames: u32,
    last_frame_ms: u64,
    probes: u8,
    pub module: Option<Module>,
    /// Version text from MON-VER or PQTMVERNO.
    pub version: heapless::String<30>,
    steps: Steps,
    pending: Option<Step>,
    sent_ms: u64,
    tries: u8,
    pub cfg: Cfg,
    pub cfg_failed_step: &'static str,
    pub status: GpsStatus,
    pub fix: Option<Fix>,
    pub time: Option<TimeAnchor>,
    pub tx_short: u32,
    /// Jamming and spoofing watch on this receiver (about 500 bytes).
    integrity: gnss::Monitor,
    /// The last trusted fix and time anchor, and what to publish.
    publisher: gnss::trust::Publisher<Fix, TimeAnchor>,
    /// UTC is running on the hub clock because GPS time is not trusted.
    pub time_held: bool,
    /// Spoofing suspected (or GPS time before the floor) and no trusted
    /// position / time to hold: none is published.
    pub untrusted_pos: bool,
    pub untrusted_time: bool,
    /// Optional configuration steps the module refused.
    pub cfg_skipped: u8,
}

impl Gps {
    pub fn new() -> Self {
        Self {
            parser: Parser::new(),
            phase: Phase::Hunt,
            baud_idx: 0,
            phase_since: 0,
            frames_at: 0,
            phase_frames: 0,
            last_frame_ms: 0,
            probes: 0,
            module: None,
            version: heapless::String::new(),
            steps: Steps::None,
            pending: None,
            sent_ms: 0,
            tries: 0,
            cfg: Cfg::None,
            cfg_failed_step: "",
            status: GpsStatus { state: GpsState::Absent, sats: None },
            fix: None,
            time: None,
            tx_short: 0,
            integrity: {
                let mut m = gnss::Monitor::new();
                m.set_time_floor(build_floor_ms());
                m
            },
            publisher: gnss::trust::Publisher::new(),
            time_held: false,
            untrusted_pos: false,
            untrusted_time: false,
            cfg_skipped: 0,
        }
    }

    /// The integrity verdict: `state` (ok / suspect jamming / jamming /
    /// suspect spoofing / spoofing), the check behind it, its figures, and
    /// `write_reason` for a plain-English line.
    pub fn integrity(&self) -> &gnss::IntegrityStatus {
        self.integrity.status()
    }

    /// The learned home `(lat, lon, height above ellipsoid)` the position
    /// checks compare against.
    #[allow(dead_code)] // for the screen / web integration
    pub fn integrity_home(&self) -> Option<(f64, f64, Option<f64>)> {
        self.integrity.home()
    }

    /// The hub was moved on purpose: forget the learned home and learn the
    /// new place (about 15 minutes of steady fixes).
    pub fn integrity_forget_home(&mut self) {
        self.integrity.forget_home();
    }

    /// Where home came from, and whether a new place waits for the operator.
    pub fn home_origin(&self) -> Option<gnss::HomeOrigin> {
        self.integrity.home_origin()
    }

    pub fn home_pending(&self) -> Option<(f64, f64, Option<f64>)> {
        self.integrity.home_pending()
    }

    /// The operator confirms the waiting new home; returns it.
    pub fn confirm_home(&mut self) -> Option<(f64, f64, Option<f64>)> {
        self.integrity.confirm_home()
    }

    /// Home from the card (remembered) or the site file (placed).
    pub fn set_home(&mut self, lat: f64, lon: f64, alt: Option<f64>, origin: gnss::HomeOrigin) -> bool {
        self.integrity.set_home(lat, lon, alt, origin)
    }

    /// Home learned / new place waiting, since the last call.
    pub fn take_home_event(&mut self) -> Option<gnss::HomeEvent> {
        self.integrity.take_home_event()
    }

    /// The latest time the hub trusted, from the card.
    pub fn set_remembered_time(&mut self, utc_ms: Option<u64>) {
        self.integrity.set_remembered_time(utc_ms.map(|u| u as i64));
    }

    pub fn remembered_time(&self) -> Option<u64> {
        self.integrity.time_guard().remembered().map(|u| u.max(0) as u64)
    }

    /// The operator accepts GPS time as it now is (console `T`): the hub's
    /// clock references and the remembered time are dropped.
    pub fn accept_time(&mut self) {
        self.integrity.accept_time();
    }

    /// UTC now from trusted GPS (not held, not free-running): what may be
    /// remembered on the card.
    pub fn trusted_utc_now(&self, now_ms: u64) -> Option<u64> {
        if self.time_held {
            return None;
        }
        self.time.filter(|a| now_ms.saturating_sub(a.at_ms) < 2_500).map(|a| a.utc_ms + now_ms.saturating_sub(a.at_ms))
    }

    /// The memory file's text (`ROTORSCP/GPSMEM.TXT`): home, if any, and the
    /// time to remember (`utc_ms`, or the remembered one kept).
    pub fn memory_text(&self, utc_ms: Option<u64>) -> heapless::String<128> {
        use core::fmt::Write;
        let mut t = heapless::String::new();
        let _ = writeln!(t, "# rotorscope hub GPS memory (WP-12): written by the hub");
        if let Some((lat, lon, alt)) = self.integrity.home()
            && self.integrity.home_origin() != Some(gnss::HomeOrigin::Placed)
        {
            let _ = match alt {
                Some(a) => writeln!(t, "home {lat:.7} {lon:.7} {a:.1}"),
                None => writeln!(t, "home {lat:.7} {lon:.7}"),
            };
        }
        if let Some(u) = utc_ms.or(self.remembered_time()) {
            let _ = writeln!(t, "time {u}");
        }
        t
    }
    pub fn baud(&self) -> u32 {
        BAUDS[self.baud_idx]
    }

    pub fn stats(&self) -> gnss::Stats {
        self.parser.stats()
    }

    pub fn sky(&self) -> &gnss::SkyView {
        self.parser.sky()
    }

    /// UTC for hub time `now_ms`.
    pub fn utc_ms(&self, now_ms: u64) -> Option<u64> {
        self.time.map(|a| a.utc_ms.saturating_add(now_ms.saturating_sub(a.at_ms)))
    }

    /// The fix if younger than `max_age_ms`, or held (see the module doc:
    /// a static hub's held position stays valid).
    pub fn fresh_fix(&self, now_ms: u64, max_age_ms: u64) -> Option<Fix> {
        self.fix.filter(|f| f.held || now_ms.saturating_sub(f.at_ms) <= max_age_ms)
    }

    fn send(&mut self, bytes: &[u8]) {
        let n = gps_rx::write(bytes);
        if n < bytes.len() {
            self.tx_short += 1;
        }
    }

    fn enter(&mut self, phase: Phase, now: u64) {
        self.phase = phase;
        self.phase_since = now;
        self.phase_frames = self.parser.stats().frames_ok;
    }

    /// One step: take received bytes, parse, and move the state machine.
    pub fn tick(&mut self, now: u64) {
        let mut buf = [0u8; 256];
        loop {
            let n = gps_rx::take(&mut buf);
            if n == 0 {
                break;
            }
            // `feed` borrows the parser; collect events first (a 256-byte
            // chunk holds at most a few).
            let mut evs: heapless::Vec<Event, 16> = heapless::Vec::new();
            for ev in self.parser.feed(&buf[..n]) {
                if evs.push(ev).is_err() {
                    break;
                }
            }
            for ev in evs {
                self.on_event(&ev, now);
            }
        }
        let frames = self.parser.stats().frames_ok;
        if frames != self.frames_at {
            self.frames_at = frames;
            self.last_frame_ms = now;
        }
        match self.phase {
            Phase::Hunt => {
                if self.parser.stats().frames_ok != self.phase_frames {
                    crate::println!("hub-p4: gps frames at {} baud: probing which module", self.baud());
                    if self.status.state == GpsState::Absent {
                        self.status = GpsStatus { state: GpsState::Searching, sats: None };
                    }
                    self.probes = 0;
                    self.enter(Phase::Detect, now);
                    self.probe(now);
                } else if now.saturating_sub(self.phase_since) > HUNT_MS {
                    self.status = GpsStatus { state: GpsState::Absent, sats: None };
                    self.baud_idx = (self.baud_idx + 1) % BAUDS.len();
                    gps_rx::set_baud(self.baud());
                    self.parser.reset();
                    self.enter(Phase::Hunt, now);
                }
            }
            Phase::Detect => {
                if now.saturating_sub(self.sent_ms) > PROBE_WAIT_MS {
                    if self.probes < PROBE_TRIES {
                        self.probe(now);
                    } else {
                        crate::println!("hub-p4: gps answered no probe; using it as a plain NMEA receiver");
                        self.start_config(Module::Nmea, now);
                    }
                }
            }
            Phase::Configure => self.pump(now),
            Phase::Run => {}
        }
        if self.phase != Phase::Hunt && now.saturating_sub(self.last_frame_ms) > SILENT_MS {
            crate::println!("hub-p4: gps silent for {} ms: hunting again", SILENT_MS);
            self.status = GpsStatus { state: GpsState::Absent, sats: None };
            self.cfg = Cfg::None;
            self.module = None;
            self.pending = None;
            self.parser.reset();
            self.enter(Phase::Hunt, now);
        }
    }

    fn probe(&mut self, now: u64) {
        let mut b = [0u8; 40];
        let n = gnss::module::probe_bytes(&mut b);
        self.send(&b[..n]);
        self.probes += 1;
        self.sent_ms = now;
    }

    fn start_config(&mut self, m: Module, now: u64) {
        self.module = Some(m);
        let ubx = ConfigOptions {
            layers: gnss::config::LAYER_RAM | gnss::config::LAYER_BBR,
            baud: Some(115_200),
            pps_len_us: 100_000,
        };
        self.steps = m.sequence(ubx, Lc29hOptions::default());
        self.pending = None;
        self.cfg = Cfg::Running;
        self.enter(Phase::Configure, now);
        crate::println!("hub-p4: gps module {} ({}) at {} baud: configuring", m.name(), self.version, self.baud());
        self.pump(now);
    }

    fn pump(&mut self, now: u64) {
        if let Some(step) = self.pending {
            if now.saturating_sub(self.sent_ms) < ACK_TIMEOUT_MS {
                return;
            }
            if self.tries >= ACK_TRIES {
                if step.optional() {
                    self.skip(step.name(), "no answer", now);
                } else {
                    self.fail(Cfg::NoAck, step.name());
                }
                return;
            }
            self.tries += 1;
            self.sent_ms = now;
            self.send(step.bytes());
            return;
        }
        match self.steps.next() {
            None => {
                self.cfg = if self.module == Some(Module::Nmea) { Cfg::None } else { Cfg::Ok };
                self.enter(Phase::Run, now);
                crate::println!("hub-p4: gps configured ({}); running", self.module.map_or("?", |m| m.name()));
            }
            Some(step) => {
                self.send(step.bytes());
                if let Some(b) = step.baud_after() {
                    // u-blox baud step: the ACK may be lost in the change;
                    // the next valid frame at the new rate confirms it.
                    // Let the 20-odd bytes leave the FIFO first (~2 ms at 115200,
                    // ~20 ms at 9600).
                    let t = Instant::now();
                    while t.elapsed().as_millis() < 30 {}
                    if let Some(i) = BAUDS.iter().position(|x| *x == b) {
                        self.baud_idx = i;
                    }
                    gps_rx::set_baud(b);
                    self.parser.reset();
                    self.last_frame_ms = now;
                    self.sent_ms = now;
                } else {
                    self.pending = Some(step);
                    self.sent_ms = now;
                    self.tries = 1;
                }
            }
        }
    }

    /// An optional step (integrity messages) was refused: carry on without.
    fn skip(&mut self, step: &'static str, why: &str, now: u64) {
        crate::println!("hub-p4: gps optional config step {step}: {why}; continuing without it (fewer integrity checks)");
        self.cfg_skipped += 1;
        self.pending = None;
        self.pump(now);
    }

    fn fail(&mut self, why: Cfg, step: &'static str) {
        self.cfg = why;
        self.cfg_failed_step = step;
        self.pending = None;
        self.steps = Steps::None;
        self.phase = Phase::Run;
        crate::println!("hub-p4: gps config step {step}: {}; running with what the module sends", why.name());
    }

    fn on_event(&mut self, ev: &Event, now: u64) {
        if matches!(ev, Event::Fix(_)) {
            // The PPS edge of this epoch, for the time check.
            self.integrity.set_pps(pps_last_us());
        }
        if self.phase == Phase::Detect
            && let Some(m) = Module::from_event(ev)
        {
            self.version.clear();
            match ev {
                Event::MonVer(v) => {
                    let _ = self.version.push_str(v.sw_str().get(..30).unwrap_or(v.sw_str()));
                }
                Event::Quectel(gnss::Reply::Version(v)) => {
                    let _ = self.version.push_str(v.version.as_str());
                }
                _ => {}
            }
            self.start_config(m, now);
            return;
        }
        if let Some(step) = self.pending {
            match step.confirmed_by(ev) {
                Some(true) => {
                    self.pending = None;
                    self.pump(now);
                }
                Some(false) if step.optional() => self.skip(step.name(), "refused", now),
                Some(false) => self.fail(Cfg::Nak, step.name()),
                None => {}
            }
        }
        if let Some(c) = self.integrity.on_event(ev, self.parser.sats(), now) {
            let mut r: heapless::String<200> = heapless::String::new();
            let _ = c.status.write_reason(&mut r);
            crate::println!("hub-p4: gps integrity {} -> {}: {}", c.from.name(), c.status.state.name(), r);
        }
        if let Event::Fix(f) = ev {
            self.on_fix(f, now);
        }
    }

    fn on_fix(&mut self, f: &gnss::Fix, now: u64) {
        let has_pos = f.fix_ok && f.position_valid;
        let state = match f.fix {
            FixType::ThreeD | FixType::GnssDeadReckoning if has_pos => GpsState::Fix3d,
            FixType::TwoD if has_pos => GpsState::Fix2d,
            _ => GpsState::Searching,
        };
        self.status = GpsStatus { state, sats: Some(f.sats_used) };
        // The monitor has already seen this epoch (on_event runs it first).
        let integrity = *self.integrity.status();
        let live_fix = if f.is_usable()
            && let Some((lat, lon)) = f.position()
        {
            let alt_hae_m = if f.fix.has_altitude() { f.alt_ellipsoid_m.map(f64::from) } else { None };
            Some(Fix { lat, lon, alt_hae_m, at_ms: now, hacc_m: f.h_acc_m, hdop: f.hdop, held: false })
        } else {
            None
        };
        // UTC only when the receiver vouches for it: NMEA RMC status A, or
        // UBX with the time of day fully resolved.
        let live_time = f.utc.filter(|_| f.source != Source::UbxNavPvt || f.utc_fully_resolved).map(|utc| {
            let utc_ms = utc.unix_ms().max(0) as u64;
            let now_us = Instant::now().duration_since_epoch().as_micros();
            match pps_last_us() {
                // The epoch is the top of a second (1 Hz), and the last PPS edge
                // marked it if that edge is under a second old.
                Some(edge) if now_us.saturating_sub(edge) < 1_000_000 => {
                    TimeAnchor { utc_ms: (utc_ms + 500) / 1000 * 1000, at_ms: edge / 1000, pps: true }
                }
                _ => TimeAnchor { utc_ms, at_ms: now, pps: false },
            }
        });
        // Trusted time found before the build date or the remembered time:
        // it is not held either.
        if integrity.metrics.time_floor {
            self.publisher.forget_time();
        }
        let out = self.publisher.epoch(integrity.trusted, live_fix, live_time);
        let hold = integrity.hold();
        use gnss::trust::Pub;
        self.untrusted_pos = false;
        match out.fix {
            Pub::Live(x) => self.fix = Some(Fix { held: false, ..x }),
            Pub::Keep => {}
            // Spoofing suspected: home (a static hub's best estimate), else
            // the last trusted fix. Otherwise (a check fired once, or trust
            // not yet established): the last trusted fix.
            Pub::Held(b) => self.fix = Some(self.held_fix(hold, Some(b))),
            Pub::Withhold if hold && self.integrity.home().is_some() => self.fix = Some(self.held_fix(true, None)),
            Pub::Withhold => {
                self.fix = None;
                self.untrusted_pos = hold;
            }
        }
        self.untrusted_time = false;
        match out.time {
            Pub::Live(a) => {
                self.time = Some(a);
                self.time_held = false;
            }
            Pub::Keep => {}
            Pub::Held(a) => {
                self.time = Some(a);
                self.time_held = true;
            }
            Pub::Withhold => {
                self.time = None;
                self.time_held = false;
                self.untrusted_time = hold || integrity.metrics.time_floor;
            }
        }
    }

    /// The fix to publish while GPS is not believed: home while spoofing is
    /// suspected (if there is one), else the last trusted fix `b`; marked
    /// held, keeping `b`'s time so the evidence log's fix age grows.
    fn held_fix(&self, hold: bool, b: Option<Fix>) -> Fix {
        match (hold.then(|| self.integrity.home()).flatten(), b) {
            (Some((lat, lon, alt)), b) => Fix {
                lat,
                lon,
                alt_hae_m: alt.or(b.and_then(|b| b.alt_hae_m)),
                at_ms: b.map_or(0, |b| b.at_ms),
                hacc_m: b.and_then(|b| b.hacc_m),
                hdop: b.and_then(|b| b.hdop),
                held: true,
            },
            (None, Some(b)) => Fix { held: true, ..b },
            // Not reached: callers pass a fix or hold with a home.
            (None, None) => Fix { lat: 0.0, lon: 0.0, alt_hae_m: None, at_ms: 0, hacc_m: None, hdop: None, held: true },
        }
    }
}

/// This firmware's build date (build.rs), Unix ms: GPS time before it is
/// impossible.
pub fn build_floor_ms() -> i64 {
    let s: i64 = env!("ROTORSCOPE_BUILD_UNIX_S").parse().unwrap_or(0);
    s.saturating_mul(1000)
}

/// What `ROTORSCP/GPSMEM.TXT` says (written by [`Gps::memory_text`]):
/// `home LAT LON [HEIGHT]` and `time UNIX_MS` lines; anything else ignored.
#[derive(Clone, Copy, Default)]
pub struct Memory {
    pub home: Option<(f64, f64, Option<f64>)>,
    pub time_ms: Option<u64>,
}

impl Memory {
    pub fn parse(b: &[u8]) -> Memory {
        let mut m = Memory::default();
        let text = core::str::from_utf8(b).unwrap_or("");
        for line in text.lines() {
            let mut it = line.split_whitespace();
            match it.next() {
                Some("home") => {
                    let lat = it.next().and_then(|v| v.parse::<f64>().ok());
                    let lon = it.next().and_then(|v| v.parse::<f64>().ok());
                    let alt = it.next().and_then(|v| v.parse::<f64>().ok());
                    if let (Some(lat), Some(lon)) = (lat, lon)
                        && (-90.0..=90.0).contains(&lat)
                        && (-180.0..=180.0).contains(&lon)
                    {
                        m.home = Some((lat, lon, alt));
                    }
                }
                Some("time") => m.time_ms = it.next().and_then(|v| v.parse().ok()),
                _ => {}
            }
        }
        m
    }
}
