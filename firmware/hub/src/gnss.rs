//! GPS: a u-blox MAX-M10S on I2C (DDC, address 0x42), or on a UART with the
//! `gps-uart` feature (PLAN.md P1, P3, P4).
//!
//! The bytes go through `crates/gnss` (`Parser`, `configure_sequence`); this
//! module only moves them and publishes the results:
//! - [`latest_fix`]: the last *usable* fix (`gnss::Fix::is_usable`), height
//!   above the WGS-84 ellipsoid (NAV-PVT `height`, not `hMSL`: Remote ID
//!   altitudes are geodetic). `Hub::own_pos` prefers it over the manual
//!   position while it is younger than the tracker's 10 s fix timeout.
//! - [`status`]: absent / searching / 2d / 3d plus satellites, for
//!   `/api/state`'s `gps`, the screen and the USB `hub` line.
//! - [`utc_ms`]: UTC, anchored to the PPS edge when there is one.
//!
//! **Absent.** On I2C the receiver either acknowledges its address or not.
//! A NACK (or any bus error) means "absent"; the task then sleeps
//! [`ABSENT_RECHECK`] and tries again, so a GPS plugged in later is found and
//! nothing ever waits on a missing one (every transfer is async with a
//! timeout). On a UART, silence for [`UART_SILENT`] means absent.
//!
//! **Configuration.** When the receiver first answers, and again after
//! [`SILENT_RECONFIGURE`] without a single valid frame, the task sends
//! `configure_sequence()` one frame at a time and waits for each ACK
//! (1.2 s, three tries). On I2C the baud step is left out
//! (`ConfigOptions { baud: None }`): it only changes UART1's speed.
//!
//! **PPS.** [`pps_task`] timestamps the receiver's TIMEPULSE rising edge on
//! the high-priority executor. The edge marks the top of the UTC second that
//! the next NAV-PVT names, so `utc_ms` is anchored to it (sub-millisecond)
//! rather than to the message's arrival (late by tens to hundreds of ms).
//! The last edge is reported in the `hub` line. Distributing PPS to the nodes
//! to align their clocks (P3) is a later task: today only the hub uses it.

use core::cell::Cell;
use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

use critical_section::Mutex;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_hal::gpio::{AnyPin, Input, InputConfig, Pull};
use gnss::{ConfigFrame, ConfigOptions, ConfigSequence, Event, FixType, Parser, Source};
use tracker::json::{GpsState, GpsStatus};

use crate::hub::now_ms;
use crate::usb;

/// How often the receiver is polled. The MAX-M10S sends NAV-PVT once a
/// second; 10 Hz keeps the delay to the hub low without busying the bus
/// (the gnss README asks for 20 Hz at most).
const POLL: Duration = Duration::from_millis(100);
/// After a NACK, try again this often. Short enough that a GPS plugged in
/// while running is found quickly; long enough to cost nothing.
pub const ABSENT_RECHECK: Duration = Duration::from_secs(3);
/// A present receiver that sends nothing valid for this long is configured
/// again (it may have been power-cycled with no backup supply).
const SILENT_RECONFIGURE: u64 = 10_000;
/// The receiver answers a CFG-VALSET within 1 s.
const ACK_TIMEOUT: u64 = 1_200;
const ACK_TRIES: u8 = 3;
/// UART only: no bytes for this long means no receiver.
#[cfg(feature = "gps-uart")]
const UART_SILENT: u64 = 3_000;

/// One GPS solution.
#[derive(Clone, Copy)]
pub struct Fix {
    pub lat: f64,
    pub lon: f64,
    /// WGS-84 ellipsoid height, metres; `None` for a 2D fix.
    pub alt_hae_m: Option<f64>,
    /// Hub time of the fix.
    pub at_ms: u64,
}

/// UTC anchored to hub time: `utc_ms` at hub time `at_ms`.
#[derive(Clone, Copy)]
pub struct TimeAnchor {
    pub utc_ms: u64,
    pub at_ms: u64,
    /// Anchored to a PPS edge (sub-ms) rather than to message arrival.
    pub pps: bool,
}

static FIX: Mutex<Cell<Option<Fix>>> = Mutex::new(Cell::new(None));
static TIME: Mutex<Cell<Option<TimeAnchor>>> = Mutex::new(Cell::new(None));
static STATUS: Mutex<Cell<GpsStatus>> = Mutex::new(Cell::new(GpsStatus { state: GpsState::Absent, sats: None }));
/// Last PPS rising edge, hub microseconds. (No 64-bit atomics on Xtensa.)
static PPS_LAST_US: Mutex<Cell<Option<u64>>> = Mutex::new(Cell::new(None));
pub static PPS_COUNT: AtomicU32 = AtomicU32::new(0);
/// Valid frames and checksum errors from the parser, for the `hub` line.
pub static FRAMES_OK: AtomicU32 = AtomicU32::new(0);
pub static CSUM_ERR: AtomicU32 = AtomicU32::new(0);
/// Why the receiver counts as absent: 0 = it answered, 1 = NACK (nothing at
/// 0x42), 2 = another bus error or a timeout.
static ABSENT_WHY: AtomicU32 = AtomicU32::new(0);
/// Configuration progress, for the `hub` line ([`config_state`]).
static CFG: AtomicU32 = AtomicU32::new(CfgState::None as u32);

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
enum CfgState {
    None = 0,
    Running = 1,
    Done = 2,
    Nak = 3,
    NoAck = 4,
}

/// The latest usable fix: this board's own receiver, or the upstream hub's
/// (web head, `Frame::HubFix`), whichever is newer.
pub fn latest_fix() -> Option<Fix> {
    let own = critical_section::with(|cs| FIX.borrow(cs).get());
    let up = upstream().and_then(|u| u.fix);
    match (own, up) {
        (Some(a), Some(b)) => Some(if b.at_ms > a.at_ms { b } else { a }),
        (a, b) => a.or(b),
    }
}

/// The UTC anchor in use: this board's receiver first (its PPS edge is
/// measured here), else the upstream hub's.
fn time_anchor() -> Option<TimeAnchor> {
    critical_section::with(|cs| TIME.borrow(cs).get()).or_else(|| upstream().and_then(|u| u.time))
}

/// UTC for hub time `now_ms`, if GPS time has ever been seen. The crystal
/// drifts ~20 ppm, so an anchor older than ~10 minutes is ~10 ms off; the
/// task refreshes it every second while the receiver has time.
pub fn utc_ms(now_ms: u64) -> Option<u64> {
    time_anchor().map(|a| a.utc_ms.saturating_add(now_ms.saturating_sub(a.at_ms)))
}

/// The current UTC anchor came from a PPS edge (here or on the upstream hub).
pub fn utc_from_pps() -> bool {
    time_anchor().is_some_and(|a| a.pps)
}

/// UTC comes from the upstream hub, not this board's own receiver.
pub fn utc_from_upstream() -> bool {
    critical_section::with(|cs| TIME.borrow(cs).get()).is_none() && time_anchor().is_some()
}

/// Receiver state for `/api/state` `gps`: this board's receiver, or, while
/// it is absent, the upstream hub's (when heard recently).
pub fn status() -> GpsStatus {
    let own = critical_section::with(|cs| STATUS.borrow(cs).get());
    if own.state != GpsState::Absent {
        return own;
    }
    upstream().filter(|u| now_ms().saturating_sub(u.at_ms) <= UPSTREAM_ALIVE_MS).map_or(own, |u| u.status)
}

// ------------------------------------------------------------ upstream --

/// What the upstream hub (a P4 feeding this board as its web head) last
/// said about its GPS (`link::Frame::HubFix`, once a second).
#[derive(Clone, Copy)]
pub struct Upstream {
    pub status: GpsStatus,
    pub fix: Option<Fix>,
    pub time: Option<TimeAnchor>,
    /// Hub time the frame arrived.
    pub at_ms: u64,
}

/// The upstream hub's GPS state counts for this long after its last frame.
/// Its fix and UTC age on their own rules (the tracker's fix timeout; UTC
/// keeps running from its last anchor, as for this board's own receiver).
const UPSTREAM_ALIVE_MS: u64 = 3_000;

static UPSTREAM: Mutex<Cell<Option<Upstream>>> = Mutex::new(Cell::new(None));
pub static UPSTREAM_FRAMES: AtomicU32 = AtomicU32::new(0);

pub fn upstream() -> Option<Upstream> {
    critical_section::with(|cs| UPSTREAM.borrow(cs).get())
}

/// A `HubFix` frame arrived (at hub time `now`).
pub fn set_upstream(h: &link::HubFix, now: u64) {
    use link::GnssState;
    let state = match h.state {
        GnssState::Absent => GpsState::Absent,
        GnssState::Searching => GpsState::Searching,
        GnssState::Fix2d => GpsState::Fix2d,
        GnssState::Fix3d => GpsState::Fix3d,
    };
    let prev = upstream();
    let fix = match h.position {
        Some(p) => Some(Fix {
            lat: f64::from(p.lat_e7) / 1e7,
            lon: f64::from(p.lon_e7) / 1e7,
            alt_hae_m: p.alt_hae_mm.map(|mm| f64::from(mm) / 1000.0),
            at_ms: now.saturating_sub(u64::from(p.age_ms)),
        }),
        // Keep the last one: it ages out by its own timestamp.
        None => prev.and_then(|u| u.fix),
    };
    let time = match h.utc_ms {
        Some(utc_ms) => Some(TimeAnchor { utc_ms, at_ms: now, pps: h.utc_pps }),
        None => prev.and_then(|u| u.time),
    };
    let u = Upstream { status: GpsStatus { state, sats: h.sats }, fix, time, at_ms: now };
    critical_section::with(|cs| UPSTREAM.borrow(cs).set(Some(u)));
    UPSTREAM_FRAMES.fetch_add(1, Relaxed);
}

fn publish_status(s: GpsStatus) {
    critical_section::with(|cs| STATUS.borrow(cs).set(s));
}

/// The last PPS edge: (hub microseconds, edges seen since boot).
pub fn pps_last_us() -> Option<u64> {
    critical_section::with(|cs| PPS_LAST_US.borrow(cs).get())
}

/// `"none"`, `"running"`, `"ok"`, `"nak"` or `"no_ack"`.
pub fn config_state() -> &'static str {
    match CFG.load(Relaxed) {
        1 => "running",
        2 => "ok",
        3 => "nak",
        4 => "no_ack",
        _ => "none",
    }
}

fn set_cfg(s: CfgState) {
    CFG.store(s as u32, Relaxed);
}

// --------------------------------------------------------------- links --

#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "gps-uart", allow(dead_code, reason = "a UART cannot NACK"))]
pub enum LinkError {
    /// Nobody acknowledged the address (I2C NACK).
    Absent,
    /// Anything else: arbitration lost, a stuck bus, a timeout.
    Bus,
}

/// A byte pipe to the receiver.
#[allow(async_fn_in_trait, reason = "used only inside this crate, with concrete types")]
pub trait GpsLink {
    /// The boot configuration for this transport.
    const OPTIONS: ConfigOptions;
    /// Read what is waiting (`Ok(0)` if nothing).
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, LinkError>;
    async fn write(&mut self, bytes: &[u8]) -> Result<(), LinkError>;
    /// UART only: follow the receiver to a new baud rate.
    async fn set_baud(&mut self, _baud: u32) {}
    /// Called when the link has been quiet for a while: a UART tries the
    /// next likely baud rate. Returns true if it changed something.
    fn hunt(&mut self) -> bool {
        false
    }
}

/// I2C (DDC). See `crates/gnss` README "I2C (DDC)".
#[cfg(not(feature = "gps-uart"))]
pub struct I2cLink(pub esp_hal::i2c::master::I2c<'static, esp_hal::Async>);

#[cfg(not(feature = "gps-uart"))]
impl I2cLink {
    /// Every transfer has two timeouts: esp-hal's own (set in `config`) and
    /// this one, so a wedged bus can never hold the task.
    async fn io<F: core::future::Future<Output = Result<(), esp_hal::i2c::master::Error>>>(
        f: F,
    ) -> Result<(), LinkError> {
        use esp_hal::i2c::master::Error;
        match with_timeout(Duration::from_millis(200), f).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(Error::AcknowledgeCheckFailed(_))) => Err(LinkError::Absent),
            Ok(Err(_)) | Err(_) => Err(LinkError::Bus),
        }
    }

    /// 100 kHz: NAV-PVT is 100 bytes a second, so speed buys nothing, and
    /// 100 kHz still works if a breakout lacks pull-ups (esp-hal enables the
    /// weak internal ones on the pins).
    pub fn config() -> esp_hal::i2c::master::Config {
        use esp_hal::i2c::master::{Config, SoftwareTimeout};
        Config::default()
            .with_frequency(esp_hal::time::Rate::from_khz(100))
            .with_software_timeout(SoftwareTimeout::Transaction(esp_hal::time::Duration::from_millis(100)))
    }
}

#[cfg(not(feature = "gps-uart"))]
impl GpsLink for I2cLink {
    const OPTIONS: ConfigOptions = ConfigOptions { baud: None, ..I2C_DEFAULTS };

    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, LinkError> {
        let mut avail = [0u8; 2];
        Self::io(self.0.write_read_async(gnss::ddc::ADDRESS, &[gnss::ddc::REG_AVAIL_HI], &mut avail)).await?;
        let n = gnss::ddc::bytes_available(avail).min(buf.len());
        if n == 0 {
            return Ok(0);
        }
        // Exactly the advertised count, so no 0xFF fill is ever read.
        Self::io(self.0.write_read_async(gnss::ddc::ADDRESS, &[gnss::ddc::REG_STREAM], &mut buf[..n])).await?;
        Ok(n)
    }

    async fn write(&mut self, bytes: &[u8]) -> Result<(), LinkError> {
        Self::io(self.0.write_async(gnss::ddc::ADDRESS, bytes)).await
    }
}

/// `ConfigOptions::default()` is not `const`; these are its values.
const I2C_DEFAULTS: ConfigOptions = ConfigOptions {
    layers: gnss::config::LAYER_RAM | gnss::config::LAYER_BBR,
    baud: Some(115_200),
    pps_len_us: 100_000,
};

/// UART (`gps-uart`). The module ships at 9600 baud NMEA; once configured it
/// runs at 115200 (kept in battery-backed RAM), so the hub cycles through the
/// likely rates until valid frames arrive.
#[cfg(feature = "gps-uart")]
pub struct UartLink {
    pub uart: esp_hal::uart::Uart<'static, esp_hal::Async>,
    pub baud_idx: usize,
}

#[cfg(feature = "gps-uart")]
const BAUDS: [u32; 3] = [115_200, 9_600, 38_400];

#[cfg(feature = "gps-uart")]
impl UartLink {
    pub fn config(baud: u32) -> esp_hal::uart::Config {
        esp_hal::uart::Config::default().with_baudrate(baud)
    }
}

#[cfg(feature = "gps-uart")]
impl GpsLink for UartLink {
    const OPTIONS: ConfigOptions = I2C_DEFAULTS;

    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, LinkError> {
        match with_timeout(Duration::from_millis(20), self.uart.read_async(buf)).await {
            Ok(Ok(n)) => Ok(n),
            Ok(Err(_)) => Err(LinkError::Bus),
            Err(_) => Ok(0),
        }
    }

    async fn write(&mut self, bytes: &[u8]) -> Result<(), LinkError> {
        let mut rest = bytes;
        while !rest.is_empty() {
            match self.uart.write_async(rest).await {
                Ok(n) => rest = &rest[n..],
                Err(_) => return Err(LinkError::Bus),
            }
        }
        Ok(())
    }

    async fn set_baud(&mut self, baud: u32) {
        let _ = self.uart.flush_async().await;
        let _ = self.uart.apply_config(&Self::config(baud));
        if let Some(i) = BAUDS.iter().position(|b| *b == baud) {
            self.baud_idx = i;
        }
    }

    fn hunt(&mut self) -> bool {
        self.baud_idx = (self.baud_idx + 1) % BAUDS.len();
        let _ = self.uart.apply_config(&Self::config(BAUDS[self.baud_idx]));
        true
    }
}

// ------------------------------------------------------- configuration --

/// Sends `configure_sequence()` one frame at a time, each confirmed by ACK.
struct Configurer {
    seq: Option<ConfigSequence>,
    pending: Option<ConfigFrame>,
    sent_ms: u64,
    tries: u8,
}

impl Configurer {
    const fn new() -> Self {
        Self { seq: None, pending: None, sent_ms: 0, tries: 0 }
    }

    fn start<L: GpsLink>(&mut self) {
        self.seq = Some(ConfigSequence::with_options(L::OPTIONS));
        self.pending = None;
        set_cfg(CfgState::Running);
    }

    fn running(&self) -> bool {
        self.seq.is_some()
    }

    fn abort(&mut self, why: CfgState, step: &str) {
        self.seq = None;
        self.pending = None;
        set_cfg(why);
        let mut m: heapless::String<64> = heapless::String::new();
        let _ = core::fmt::Write::write_fmt(
            &mut m,
            format_args!("gps config step {step}: {}", if why == CfgState::Nak { "NAK" } else { "no ACK" }),
        );
        usb::msg("warn", &m);
    }

    /// An event from the parser: is it the answer we wait for?
    fn on_event(&mut self, ev: &Event) {
        let Some(step) = &self.pending else { return };
        match step.confirmed_by(ev) {
            Some(true) => self.pending = None,
            Some(false) => {
                let name = step.name;
                self.abort(CfgState::Nak, name);
            }
            None => {}
        }
    }

    /// Send the next step, or resend one that went unanswered.
    async fn pump<L: GpsLink>(&mut self, link: &mut L, parser: &mut Parser, now: u64) {
        let Some(seq) = self.seq.as_mut() else { return };
        if let Some(step) = &self.pending {
            if now.saturating_sub(self.sent_ms) < ACK_TIMEOUT {
                return;
            }
            if self.tries >= ACK_TRIES {
                let name = step.name;
                self.abort(CfgState::NoAck, name);
                return;
            }
            let bytes = *step;
            self.tries += 1;
            self.sent_ms = now;
            let _ = link.write(bytes.bytes()).await;
            return;
        }
        match seq.next() {
            None => {
                self.seq = None;
                set_cfg(CfgState::Done);
                usb::msg("gps", "configured: UBX NAV-PVT 1 Hz, NMEA off, portable, PPS on");
            }
            // UART only (the I2C sequence has no baud step): the ACK may be
            // lost in the speed change, so the next valid frame confirms it.
            Some(step) if step.baud_after.is_some() => {
                let _ = link.write(step.bytes()).await;
                link.set_baud(step.baud_after.unwrap_or(115_200)).await;
                parser.reset();
            }
            Some(step) => {
                let _ = link.write(step.bytes()).await;
                self.pending = Some(step);
                self.sent_ms = now;
                self.tries = 1;
            }
        }
    }
}

// ---------------------------------------------------------------- task --

fn on_fix(f: &gnss::Fix, now: u64) {
    let has_pos = f.fix_ok && f.position_valid;
    let state = match f.fix {
        FixType::ThreeD | FixType::GnssDeadReckoning if has_pos => GpsState::Fix3d,
        FixType::TwoD if has_pos => GpsState::Fix2d,
        _ => GpsState::Searching,
    };
    publish_status(GpsStatus { state, sats: Some(f.sats_used) });

    if f.is_usable()
        && let Some((lat, lon)) = f.position()
    {
        let alt_hae_m = if f.fix.has_altitude() { f.alt_ellipsoid_m.map(f64::from) } else { None };
        critical_section::with(|cs| FIX.borrow(cs).set(Some(Fix { lat, lon, alt_hae_m, at_ms: now })));
    }

    // UTC only when the receiver vouches for it: NMEA RMC status A, or UBX
    // with the time of day fully resolved.
    let Some(utc) = f.utc else { return };
    if f.source == Source::UbxNavPvt && !f.utc_fully_resolved {
        return;
    }
    let utc_ms = utc.unix_ms().max(0) as u64;
    let now_us = Instant::now().as_micros();
    let anchor = match pps_last_us() {
        // The navigation epoch is the top of a second (1 Hz rate), and the
        // last PPS edge marked it, if that edge is under a second old.
        Some(edge) if now_us.saturating_sub(edge) < 1_000_000 => {
            TimeAnchor { utc_ms: (utc_ms + 500) / 1000 * 1000, at_ms: edge / 1000, pps: true }
        }
        _ => TimeAnchor { utc_ms, at_ms: now, pps: false },
    };
    critical_section::with(|cs| TIME.borrow(cs).set(Some(anchor)));
}

/// The GPS task (thread-mode executor). Never blocks: every transfer is async
/// with a timeout, and an absent receiver costs one NACKed address every few
/// seconds.
pub async fn run<L: GpsLink>(mut link: L) {
    let mut parser = Parser::new();
    let mut cfg = Configurer::new();
    let mut buf = [0u8; 256];
    let mut present = false;
    // Hub time of the last valid frame (any message), and of the last byte.
    let mut last_frame = 0u64;
    #[cfg(feature = "gps-uart")]
    let mut last_byte = now_ms();
    let mut last_config_start = 0u64;

    loop {
        Timer::after(POLL).await;
        let now = now_ms();
        let n = match link.read(&mut buf).await {
            Ok(n) => {
                ABSENT_WHY.store(0, Relaxed);
                n
            }
            Err(e) => {
                ABSENT_WHY.store(if matches!(e, LinkError::Absent) { 1 } else { 2 }, Relaxed);
                if present {
                    present = false;
                    usb::msg("gps", "receiver stopped answering: absent");
                }
                publish_status(GpsStatus { state: GpsState::Absent, sats: None });
                set_cfg(CfgState::None);
                cfg = Configurer::new();
                Timer::after(ABSENT_RECHECK).await;
                continue;
            }
        };

        #[cfg(feature = "gps-uart")]
        {
            if n > 0 {
                last_byte = now;
            } else if now.saturating_sub(last_byte) > UART_SILENT {
                if present {
                    present = false;
                    usb::msg("gps", "no bytes on the GPS UART: absent");
                }
                publish_status(GpsStatus { state: GpsState::Absent, sats: None });
                // Maybe it is talking at another speed.
                link.hunt();
                last_byte = now;
                continue;
            }
        }

        if !present && (cfg!(not(feature = "gps-uart")) || n > 0) {
            present = true;
            last_frame = now;
            parser.reset();
            publish_status(GpsStatus { state: GpsState::Searching, sats: None });
            usb::msg("gps", "receiver found; configuring");
            cfg.start::<L>();
            last_config_start = now;
        }

        for ev in parser.feed(&buf[..n]) {
            last_frame = now;
            cfg.on_event(&ev);
            if let Event::Fix(f) = ev {
                on_fix(&f, now);
            }
        }
        let st = parser.stats();
        FRAMES_OK.store(st.frames_ok, Relaxed);
        CSUM_ERR.store(st.checksum_errors, Relaxed);

        // Silent but present: power-cycled without backup, or (UART) at
        // another speed. Configure again, at most every 30 s.
        if present
            && !cfg.running()
            && now.saturating_sub(last_frame) > SILENT_RECONFIGURE
            && now.saturating_sub(last_config_start) > 30_000
        {
            publish_status(GpsStatus { state: GpsState::Searching, sats: None });
            if link.hunt() {
                parser.reset();
            }
            cfg.start::<L>();
            last_config_start = now;
        }
        if present {
            cfg.pump(&mut link, &mut parser, now).await;
        }
    }
}

#[cfg(not(feature = "gps-uart"))]
#[embassy_executor::task]
pub async fn i2c_task(link: I2cLink) {
    run(link).await
}

#[cfg(feature = "gps-uart")]
#[embassy_executor::task]
pub async fn uart_task(link: UartLink) {
    run(link).await
}

/// PPS edge timestamps. Runs on the high-priority interrupt executor (with
/// the node UARTs) so a busy thread-mode executor cannot delay the stamp.
/// The input is pulled down: with no GPS fitted it stays low and quiet.
///
/// The driver is built here because esp-hal async drivers are not `Send`
/// (see `links::reader`).
#[embassy_executor::task]
pub async fn pps_task(pin: AnyPin<'static>) {
    let mut input = Input::new(pin, InputConfig::default().with_pull(Pull::Down));
    loop {
        input.wait_for_rising_edge().await;
        let t = Instant::now().as_micros();
        critical_section::with(|cs| PPS_LAST_US.borrow(cs).set(Some(t)));
        PPS_COUNT.fetch_add(1, Relaxed);
    }
}

/// For the `hub` line.
#[derive(serde::Serialize)]
pub struct GpsLine {
    state: GpsState,
    sats: Option<u8>,
    cfg: &'static str,
    /// Why it is absent: `"nack"` (nothing at 0x42), `"bus"` (bus error or
    /// timeout), or null when it answers.
    why: Option<&'static str>,
    frames: u32,
    csum_err: u32,
    /// PPS edges since boot, and the age of the last one.
    pps_n: u32,
    pps_age_ms: Option<u64>,
    fix_age_ms: Option<u64>,
    /// The upstream hub's GPS (web head): `HubFix` frames received and the
    /// age of the last one; null if none ever arrived.
    up_frames: u32,
    up_age_ms: Option<u64>,
}

impl GpsLine {
    pub fn now(now: u64) -> Self {
        let s = status();
        let now_us = Instant::now().as_micros();
        Self {
            state: s.state,
            sats: s.sats,
            cfg: config_state(),
            why: match ABSENT_WHY.load(Relaxed) {
                1 => Some("nack"),
                2 => Some("bus"),
                _ => None,
            },
            frames: FRAMES_OK.load(Relaxed),
            csum_err: CSUM_ERR.load(Relaxed),
            pps_n: PPS_COUNT.load(Relaxed),
            pps_age_ms: pps_last_us().map(|t| now_us.saturating_sub(t) / 1000),
            fix_age_ms: latest_fix().map(|f| now.saturating_sub(f.at_ms)),
            up_frames: UPSTREAM_FRAMES.load(Relaxed),
            up_age_ms: upstream().map(|u| now.saturating_sub(u.at_ms)),
        }
    }
}
