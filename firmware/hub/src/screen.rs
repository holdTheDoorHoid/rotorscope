//! The on-device screen (PLAN.md O2, O6): `crates/screen` on an SPI panel
//! through `mipidsi` (ST7789 by default, ILI9341 with `--features ili9341`).
//!
//! Two halves, on two executors:
//! - [`feeder`] (thread-mode executor, core 0, where the tracker lives)
//!   builds a `screen::UiState` from the tracker, GPS, nodes, SD and battery
//!   whenever something changed (at most [`FEED_MIN_GAP`] apart) and at least
//!   once a second, into [`UI_STATE`]. It yields between tracks.
//! - [`display`] (the S3's second core; see `periph.rs`) owns the panel,
//!   the buttons and the `screen::Ui` state machine. Every 10 ms it samples
//!   the buttons; it redraws when the feeder published a new state, at 1 Hz,
//!   and at once on a button press or a new-drone alert.
//!
//! **No framebuffer.** 320x240 RGB565 is 150 KB and the S3 has no PSRAM, so
//! the frame is drawn in 10 bands of 320x24 (15 KB) with `screen::band`,
//! each sent to the panel as soon as it is drawn. The panel only ever gets
//! finished pixels, so nothing flickers. The cost is CPU (every band re-runs
//! the layout), which is why this runs on the second core: the hub's network
//! and USB work on core 0 never wait for it. Times are in the `hub` line
//! (`screen.render_us` / `flush_us`).
//!
//! **No panel attached** cannot be detected (SPI is write-only here), so the
//! task simply runs; the bytes go nowhere.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};

use alloc::boxed::Box;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Ticker, Timer, with_timeout};
use embedded_graphics::pixelcolor::Rgb565;
use embedded_hal_bus::spi::RefCellDevice;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Input, Output};
use mipidsi::interface::SpiInterface;
use mipidsi::options::{ColorInversion, Orientation, Rotation};
use screen::{Band, ButtonDecoder, Fix, NodeKind, Ui, UiState, from_tracker::view, render_banded};
use tracker::{TrackId, TrackState};
use tracker::json::GpsState;

use crate::bus::{Bus, Speed, SpiBus};
use crate::hub::{Hub, MAX_TRACKS, OwnSource, now_ms};
use crate::{gnss, panel, sd};

/// The view-model shared between the feeder (core 0) and the display (core
/// 1). An async mutex: the display holds it for a whole frame so all bands
/// show the same data, and the feeder simply waits its turn (it never blocks
/// anything else on core 0 while waiting). `None` only before `main` fills it.
/// Boxed: 7 KB on the heap rather than in `.data` (see `periph::start_core1`
/// on why static RAM is the scarce kind here).
pub static UI_STATE: Mutex<CriticalSectionRawMutex, Option<Box<UiState>>> = Mutex::new(None);
/// Bumped each time the feeder publishes.
static UI_VERSION: AtomicU32 = AtomicU32::new(0);
/// Something the screen shows changed (a frame was ingested, GPS state...).
static DIRTY: Signal<CriticalSectionRawMutex, ()> = Signal::new();
/// New-drone alerts, by track key, from the hub to the screen's `Ui`.
static NEW_DRONES: Channel<CriticalSectionRawMutex, u32, 8> = Channel::new();

/// Detections arrive several times a second per drone; rebuilding the view
/// more often than this buys nothing on a 1 Hz-ish display.
const FEED_MIN_GAP: Duration = Duration::from_millis(250);
/// Redraw at least this often (ages and the clock tick), and at most this
/// often for feeder updates alone.
const REDRAW_MAX_MS: u64 = 1_000;
const REDRAW_MIN_MS: u64 = 200;

// Measurements for the `hub` line.
pub static FRAMES: AtomicU32 = AtomicU32::new(0);
pub static RENDER_US: AtomicU32 = AtomicU32::new(0);
pub static RENDER_MAX_US: AtomicU32 = AtomicU32::new(0);
pub static FLUSH_US: AtomicU32 = AtomicU32::new(0);
pub static FEED_US: AtomicU32 = AtomicU32::new(0);
pub static BUTTONS: AtomicU32 = AtomicU32::new(0);
/// The SD task has had its first look at the card (see `display`).
pub static SD_PROBED: AtomicBool = AtomicBool::new(false);

/// Something on screen may have changed; the feeder rebuilds soon.
pub fn mark_dirty() {
    DIRTY.signal(());
}

/// The tracker announced a new drone (once per drone).
pub fn new_drone(key: u32) {
    // A full queue means the screen is far behind; the banner says "+N more"
    // anyway, so dropping an alert is harmless.
    let _ = NEW_DRONES.try_send(key);
    mark_dirty();
}

/// Put the (large) initial view-model in place. Called once from `main`,
/// early, while its stack has room for the temporary.
pub fn init_state() {
    if let Ok(mut g) = UI_STATE.try_lock() {
        *g = Some(Box::new(UiState::default()));
    }
}

// ------------------------------------------------------------- core 0 --

/// What the status bar says about position: GPS fix kind, a typed position,
/// no fix, or no receiver.
fn gps_view(hub: &Hub, now: u64) -> screen::Gps {
    let g = gnss::status();
    let fix = match hub.own_pos(now).map(|p| p.source) {
        Some(OwnSource::Gps) => match g.state {
            GpsState::Fix2d => Fix::Fix2D,
            _ => Fix::Fix3D,
        },
        Some(OwnSource::Manual) => Fix::Manual,
        None if g.state == GpsState::Absent => Fix::Absent,
        None => Fix::None,
    };
    screen::Gps { fix, sats: g.sats.unwrap_or(0) }
}

/// Fill `st` from the hub. Does what `screen::from_tracker::fill_from_tracker`
/// does, but a few tracks at a time with a yield in between: range and
/// bearing are f64 Vincenty solves, soft-float on this core (~0.4 ms each),
/// and a 64-track table needs ~150 of them per rebuild. In one piece that
/// would stall USB, HTTP and CoT for tens of milliseconds.
async fn build(hub: &Hub, now: u64, st: &mut UiState) {
    st.gps = gps_view(hub, now);
    let ctx = hub.context(now);
    st.now_ms = ctx.now_ms;
    st.utc_ms = ctx.utc_ms;
    if ctx.own.is_none() && st.gps.has_position() {
        st.gps.fix = Fix::None;
    }

    // Sort keys as fill_from_tracker: active before stale, nearest, newest.
    let ids: heapless::Vec<TrackId, MAX_TRACKS> = hub.tracker.borrow().tracks().iter().map(|t| t.id).collect();
    st.total_drones = ids.len() as u16;
    let mut keyed: heapless::Vec<((bool, u64, u64), TrackId), MAX_TRACKS> = heapless::Vec::new();
    for (i, id) in ids.iter().enumerate() {
        let key = {
            let tracker = hub.tracker.borrow();
            let Some(t) = tracker.get(*id) else { continue };
            let dist = ctx.geometry(t).drone.map_or(u64::MAX, |g| g.distance_m.max(0.0) as u64);
            (ctx.state(t) == TrackState::Stale, dist, u64::MAX - t.last_seen_ms)
        };
        let _ = keyed.push((key, *id));
        if i % 8 == 7 {
            embassy_futures::yield_now().await;
        }
    }
    keyed.sort_unstable_by_key(|(k, _)| *k);
    st.drones.clear();
    for (_, id) in keyed.iter().take(screen::MAX_DRONES) {
        let v = {
            let tracker = hub.tracker.borrow();
            let Some(t) = tracker.get(*id) else { continue };
            view(t, &ctx)
        };
        let _ = st.drones.push(v);
        embassy_futures::yield_now().await;
    }

    st.nodes.clear();
    for (i, n) in hub.nodes.borrow().iter().enumerate() {
        let Some(role) = n.role else { continue };
        let kind = match role {
            link::NodeRole::Wifi24 => NodeKind::Wifi24,
            link::NodeRole::Wifi5 => NodeKind::Wifi5,
            link::NodeRole::Ble => NodeKind::Ble,
            link::NodeRole::Fpv => NodeKind::Fpv,
        };
        let _ = st.nodes.push(screen::NodeStatus {
            kind,
            alive: n.alive(now),
            frames_per_s: n.seen_per_s.unwrap_or(0).min(u32::from(u16::MAX)) as u16,
            rid_found: n.rid_total(),
            drops: hub.link_drops(i),
            uptime_s: n.uptime_s(),
        });
    }
    st.power = panel::power();
    st.sd = sd::ui_state();
    st.hub_uptime_s = (now / 1000) as u32;
}

/// Builds the view-model on change (rate-limited) and at least once a second.
#[embassy_executor::task]
pub async fn feeder(hub: &'static Hub) {
    let mut last = Instant::now();
    loop {
        let _ = with_timeout(Duration::from_secs(1), DIRTY.wait()).await;
        Timer::at(last + FEED_MIN_GAP).await;
        {
            let mut g = UI_STATE.lock().await;
            let t0 = Instant::now();
            if let Some(st) = g.as_mut() {
                build(hub, now_ms(), st).await;
            }
            // Wall time including yields to other tasks, so an upper bound
            // on the CPU it took.
            FEED_US.store(t0.elapsed().as_micros() as u32, Relaxed);
        }
        UI_VERSION.fetch_add(1, Relaxed);
        last = Instant::now();
    }
}

// ------------------------------------------------------------- core 1 --

#[cfg(not(feature = "ili9341"))]
type Panel = mipidsi::models::ST7789;
#[cfg(feature = "ili9341")]
type Panel = mipidsi::models::ILI9341Rgb565;

pub type PanelSpi = RefCellDevice<'static, SpiBus, Output<'static>, Delay>;
type Di = SpiInterface<'static, PanelSpi, Output<'static>>;
type Display = mipidsi::Display<Di, Panel, mipidsi::NoResetPin>;

/// The panel's native framebuffer is 240x320 portrait; rotating 90 degrees
/// gives the 320x240 landscape `crates/screen` draws. Most ST7789 modules
/// need colour inversion; ILI9341 modules do not. PLACEHOLDERS until the
/// panel is chosen: check orientation and colours on the real one.
fn init_panel(di: Di) -> Option<Display> {
    let b = mipidsi::Builder::new(Panel {}, di)
        .display_size(240, 320)
        .orientation(Orientation::new().rotate(Rotation::Deg90));
    #[cfg(not(feature = "ili9341"))]
    let b = b.invert_colors(ColorInversion::Inverted);
    #[cfg(feature = "ili9341")]
    let b = b.invert_colors(ColorInversion::Normal);
    b.init(&mut Delay::new()).ok()
}

/// Backlight PWM duty for brightness 1..=5. Perceived brightness is roughly
/// logarithmic, so the steps grow.
fn backlight_pct(level: u8) -> u8 {
    match level {
        0 | 1 => 8,
        2 => 20,
        3 => 40,
        4 => 70,
        _ => 100,
    }
}

pub struct DisplayParts {
    pub bus: &'static Bus,
    pub cs: Output<'static>,
    pub dc: Output<'static>,
    pub backlight: crate::pwm::Pwm,
    /// Up, Down, Select; active low with pull-ups.
    pub buttons: [Input<'static>; 3],
    /// `screen::BAND_ROWS` rows of the panel width.
    pub band: &'static mut [Rgb565],
    /// Batches pixel bytes for SPI (mipidsi's interface buffer).
    pub spi_buf: &'static mut [u8],
}

#[embassy_executor::task]
pub async fn display(p: DisplayParts) {
    let DisplayParts { bus, cs, dc, mut backlight, buttons, band, spi_buf } = p;

    // Let the SD card be put into SPI mode before any panel traffic: a card
    // still in SD mode reads MOSI as its command line. One look is enough.
    let t0 = Instant::now();
    while !SD_PROBED.load(Relaxed) && t0.elapsed() < Duration::from_secs(2) {
        Timer::after_millis(20).await;
    }

    let Ok(dev) = RefCellDevice::new(&bus.spi, cs, Delay::new());
    bus.set(Speed::Panel);
    let mut panel = init_panel(SpiInterface::new(dev, dc, spi_buf));
    if panel.is_none() {
        crate::usb::msg("warn", "panel init failed; screen off");
    }

    let mut band = Band::new(band);
    let mut ui = Ui::default();
    let mut keys = ButtonDecoder::new();
    let mut seen_version = u32::MAX;
    let mut last_draw = 0u64;
    let mut force = true;
    let mut lit = false;
    let mut brightness = 0u8;
    let mut ticker = Ticker::every(Duration::from_millis(10));

    loop {
        ticker.next().await;
        let now = now_ms();

        // Buttons: sampled every 10 ms; ButtonDecoder debounces and turns
        // levels into presses (short, long = Back, auto-repeat).
        let [up, down, select] = &buttons;
        if let Some(b) = keys.poll(up.is_low(), down.is_low(), select.is_low(), now) {
            BUTTONS.fetch_add(1, Relaxed);
            let g = UI_STATE.lock().await;
            if let Some(st) = g.as_ref() {
                ui.button(b, st);
            }
            force = true;
        }
        while let Ok(key) = NEW_DRONES.try_receive() {
            ui.new_drone(key, now);
            force = true;
        }
        if ui.take_buzz().is_some() {
            panel::buzz();
        }
        let before = (ui.alert().is_some(), ui.page_toast(), ui.mode());
        ui.tick(now);
        if before != (ui.alert().is_some(), ui.page_toast(), ui.mode()) {
            force = true; // a banner or toast just went away
        }

        let version = UI_VERSION.load(Relaxed);
        let since = now.saturating_sub(last_draw);
        let due = force || since >= REDRAW_MAX_MS || (version != seen_version && since >= REDRAW_MIN_MS);
        if !due {
            continue;
        }
        let Some(display) = panel.as_mut() else { continue };
        force = false;
        last_draw = now;
        seen_version = version;

        let g = UI_STATE.lock().await;
        let Some(st) = g.as_ref() else { continue };
        bus.set(Speed::Panel);
        let start = Instant::now();
        let mut flush_us = 0u64;
        let _ = render_banded(&ui, st, &mut band, |top, rows, px| {
            let t = Instant::now();
            let r = display.set_pixels(0, top as u16, screen::WIDTH as u16 - 1, (top + rows - 1) as u16, px.iter().copied());
            flush_us += t.elapsed().as_micros();
            r
        });
        drop(g);
        let total = start.elapsed().as_micros();
        let render = total.saturating_sub(flush_us) as u32;
        RENDER_US.store(render, Relaxed);
        RENDER_MAX_US.fetch_max(render, Relaxed);
        FLUSH_US.store(flush_us as u32, Relaxed);
        FRAMES.fetch_add(1, Relaxed);

        // Backlight on only once there is a picture (no garbage at boot), and
        // follow the brightness setting.
        let want = ui.settings().brightness;
        if !lit || want != brightness {
            backlight.set_pct(backlight_pct(want));
            brightness = want;
            lit = true;
        }
    }
}

/// For the `hub` line.
#[derive(serde::Serialize)]
pub struct ScreenLine {
    frames: u32,
    /// Last frame: drawing all bands, and sending them over SPI.
    render_us: u32,
    render_max_us: u32,
    flush_us: u32,
    /// Building the view-model on core 0.
    feed_us: u32,
    buttons: u32,
    /// Untouched bytes of the second core's stack (S3).
    core1_stack_free: Option<usize>,
}

impl ScreenLine {
    pub fn now() -> Self {
        Self {
            frames: FRAMES.load(Relaxed),
            render_us: RENDER_US.load(Relaxed),
            render_max_us: RENDER_MAX_US.swap(0, Relaxed),
            flush_us: FLUSH_US.load(Relaxed),
            feed_us: FEED_US.load(Relaxed),
            buttons: BUTTONS.load(Relaxed),
            core1_stack_free: crate::periph::core1_stack_free(),
        }
    }
}
