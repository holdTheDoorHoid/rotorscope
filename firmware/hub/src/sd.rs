//! SD-card log (PLAN.md O3): CSV per `tracker::csv`, one file per boot.
//!
//! Two halves:
//! - [`log_tick`] runs in the 1 Hz tick (core 0, where the tracker lives) and
//!   formats `csv::row` for every *active* drone (and `csv::link_row` for
//!   every active open FPV link) into the [`ROWS`] pipe. It
//!   never touches the card. When the pipe is full it waits at most
//!   [`ROW_PATIENCE`] per row and then drops the row (counted), so a slow or
//!   stuck card can never hold up the tick.
//! - [`task`] runs with the screen on the S3's second core, sharing its SPI
//!   bus (`bus.rs`, separate chip select). It owns the card: finds it, opens
//!   the file, drains the pipe into it and flushes every [`FLUSH_EVERY_MS`].
//!
//! **No card.** Every [`PROBE_EVERY`] the task wakes the card slot with
//! CMD0 at 400 kHz and looks for the idle reply in the next 16 bytes. No
//! reply means "no card" and costs about a millisecond. Only a card that
//! answers is handed to `embedded-sdmmc` (whose own init can take seconds to
//! give up on an empty slot).
//!
//! **Hot removal.** Any card error closes everything and drops back to
//! probing; with nothing to write the task checks the card is still there
//! every few seconds. Rows already flushed are safe; at most the last
//! [`FLUSH_EVERY_MS`] are lost. A card put back gets the same file name and
//! is appended to, so a boot is still one file.
//!
//! **File name.** FAT short names (8.3) only. With GPS time: `MMDDhhmm.CSV`
//! (UTC, when the file was first opened). Without: `LOGnnnnn.CSV`, one more
//! than the highest already on the card.

use core::cell::RefCell;
use core::fmt::Write as _;
use core::ops::ControlFlow;
use core::sync::atomic::{AtomicU32, AtomicU8, Ordering::Relaxed};

use critical_section::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::pipe::Pipe;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use embedded_hal::spi::{Operation, SpiDevice};
use embedded_hal_bus::spi::RefCellDevice;
use embedded_sdmmc::sdcard::spi::AcquireOpts;
use embedded_sdmmc::{
    Mode, RawDirectory, RawFile, SdCard, TimeSource, Timestamp, VolumeIdx, VolumeManager,
};
use esp_hal::delay::Delay;
use esp_hal::gpio::Output;
use tracker::csv::{self, MAX_ROW};
use tracker::openlinks::MAX_LINK_TRACKS;
use tracker::{Context, LinkTrackId, TrackId, TrackState};

use crate::bus::{Bus, Speed, SpiBus};
use crate::hub::{Hub, MAX_TRACKS, now_ms};
use crate::{clock, gnss, usb};

/// Rows waiting for the card. 4 KB is ~10 rows: enough for a normal second;
/// the tick waits briefly for room when there are more drones than that.
static ROWS: Pipe<CriticalSectionRawMutex, 4096> = Pipe::new();
const ROW_PATIENCE: Duration = Duration::from_millis(30);
const PROBE_EVERY: Duration = Duration::from_secs(3);
/// After a card that answers but cannot be used (not FAT, write-protected),
/// wait longer before trying again: the problem will not fix itself.
const ERROR_RETRY: Duration = Duration::from_secs(15);
const FLUSH_EVERY_MS: u64 = 5_000;
/// With nothing to write, check the card is still in the slot this often.
const PRESENCE_EVERY_MS: u64 = 3_000;

pub static ROWS_LOGGED: AtomicU32 = AtomicU32::new(0);
pub static ROWS_DROPPED: AtomicU32 = AtomicU32::new(0);
pub static BYTES_WRITTEN: AtomicU32 = AtomicU32::new(0);
pub static CARD_ERRORS: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum State {
    /// Not looked yet.
    Starting = 0,
    NoCard = 1,
    Recording = 2,
    /// A card answered but could not be used, or a write failed.
    Error = 3,
}

static STATE: AtomicU8 = AtomicU8::new(State::Starting as u8);
/// The file this boot logs to, once chosen (kept across card swaps).
static FILE_NAME: Mutex<RefCell<heapless::String<12>>> = Mutex::new(RefCell::new(heapless::String::new()));

fn state() -> State {
    match STATE.load(Relaxed) {
        1 => State::NoCard,
        2 => State::Recording,
        3 => State::Error,
        _ => State::Starting,
    }
}

fn set_state(s: State) {
    let old = STATE.swap(s as u8, Relaxed);
    if old != s as u8 {
        crate::screen::mark_dirty();
        match s {
            State::NoCard => usb::msg("sd", "no card"),
            State::Recording => usb::msg("sd", "recording"),
            _ => {}
        }
    }
}

/// True while rows are being written to a card (`"rec"` in `/api/state`).
pub fn recording() -> bool {
    state() == State::Recording
}

/// For the screen.
pub fn ui_state() -> screen::SdLog {
    match state() {
        State::Starting => screen::SdLog::Off,
        State::NoCard => screen::SdLog::NoCard,
        State::Recording => screen::SdLog::Recording,
        State::Error => screen::SdLog::Error,
    }
}

/// For the `hub` line.
#[derive(serde::Serialize)]
pub struct SdLine {
    /// `"starting"`, `"no_card"`, `"recording"` or `"error"`.
    state: &'static str,
    file: heapless::String<12>,
    rows: u32,
    dropped: u32,
    bytes: u32,
    errors: u32,
}

impl SdLine {
    pub fn now() -> Self {
        Self {
            state: match state() {
                State::Starting => "starting",
                State::NoCard => "no_card",
                State::Recording => "recording",
                State::Error => "error",
            },
            file: critical_section::with(|cs| FILE_NAME.borrow_ref(cs).clone()),
            rows: ROWS_LOGGED.load(Relaxed),
            dropped: ROWS_DROPPED.load(Relaxed),
            bytes: BYTES_WRITTEN.load(Relaxed),
            errors: CARD_ERRORS.load(Relaxed),
        }
    }
}

// ------------------------------------------------------------- core 0 --

/// Queue one CSV row per active drone and per active open link. Called by
/// the 1 Hz tick.
pub async fn log_tick(hub: &Hub, ctx: &Context) {
    if !recording() {
        return; // nothing to write to: skip the formatting too
    }
    let ids: heapless::Vec<TrackId, MAX_TRACKS> = {
        let t = hub.tracker.borrow();
        t.tracks().iter().filter(|t| ctx.state(t) == TrackState::Active).map(|t| t.id).collect()
    };
    let mut row = [0u8; MAX_ROW];
    for id in ids {
        let n = {
            let tracker = hub.tracker.borrow();
            let Some(t) = tracker.get(id) else { continue };
            csv::row(t, ctx, &mut row)
        };
        if let Ok(n) = n {
            if push(&row[..n]).await {
                ROWS_LOGGED.fetch_add(1, Relaxed);
            } else {
                ROWS_DROPPED.fetch_add(1, Relaxed);
            }
        }
        // A row takes ~1-2 ms to format (soft-float); let others in.
        embassy_futures::yield_now().await;
    }
    let links: heapless::Vec<LinkTrackId, MAX_LINK_TRACKS> = {
        let t = hub.tracker.borrow();
        t.open_links().iter().filter(|l| ctx.link_state(l) == TrackState::Active).map(|l| l.id).collect()
    };
    for id in links {
        let n = {
            let tracker = hub.tracker.borrow();
            let Some(l) = tracker.get_open_link(id) else { continue };
            csv::link_row(l, ctx, &mut row)
        };
        if let Ok(n) = n {
            if push(&row[..n]).await {
                ROWS_LOGGED.fetch_add(1, Relaxed);
            } else {
                ROWS_DROPPED.fetch_add(1, Relaxed);
            }
        }
        embassy_futures::yield_now().await;
    }
}

/// Put a whole row in the pipe or none of it. The tick is the only writer,
/// so once there is room the row cannot be split by another one.
async fn push(row: &[u8]) -> bool {
    let give_up = Instant::now() + ROW_PATIENCE;
    while ROWS.free_capacity() < row.len() {
        if Instant::now() >= give_up {
            return false;
        }
        Timer::after_millis(2).await;
    }
    let mut rest = row;
    while !rest.is_empty() {
        match ROWS.try_write(rest) {
            Ok(n) => rest = &rest[n..],
            Err(_) => return false,
        }
    }
    true
}

// ------------------------------------------------------------- core 1 --

type Dev = RefCellDevice<'static, SpiBus, Output<'static>, Delay>;
type Card = SdCard<Dev, Delay>;
type Vm = VolumeManager<Card, GpsClock, 1, 1, 1>;
type SdError = embedded_sdmmc::Error<embedded_sdmmc::SdCardError>;

/// FAT timestamps from GPS (or demo) UTC; 2026-01-01 when there is none.
pub struct GpsClock;

impl TimeSource for GpsClock {
    fn get_timestamp(&self) -> Timestamp {
        let c = clock::utc_ms(now_ms()).map_or(
            clock::Civil { year: 2026, month: 1, day: 1, hour: 0, minute: 0, second: 0 },
            clock::civil,
        );
        Timestamp {
            year_since_1970: (c.year - 1970).clamp(0, 255) as u8,
            zero_indexed_month: c.month - 1,
            zero_indexed_day: c.day - 1,
            hours: c.hour,
            minutes: c.minute,
            seconds: c.second,
        }
    }
}

/// Is anything answering in the card slot? CMD0 (GO_IDLE_STATE) after the
/// 74+ wake-up clocks; a card replies 0x01 within 8 bytes. With an empty
/// slot MISO stays high (0xFF) or floats, and we stop after 16 bytes rather
/// than embedded-sdmmc's tens of thousands. Must run at [`Speed::SdInit`].
fn probe(card: &Card, bus: &Bus) -> bool {
    // Wake-up clocks with chip select HIGH: straight on the bus.
    if let Ok(mut spi) = bus.spi.try_borrow_mut() {
        let _ = embedded_hal::spi::SpiBus::write(&mut *spi, &[0xFF; 10]);
    } else {
        return false;
    }
    // CMD0, argument 0, CRC 0x95 (CRC is checked for CMD0 in SPI mode).
    const CMD0: [u8; 6] = [0x40, 0, 0, 0, 0, 0x95];
    let mut reply = [0xFFu8; 16];
    let ok = card.spi(|dev: &mut Dev| {
        dev.transaction(&mut [Operation::Write(&CMD0), Operation::TransferInPlace(&mut reply)]).is_ok()
    });
    ok && reply.iter().any(|&b| b == 0x01)
}

/// Pick this boot's file name (once) and remember it.
fn choose_name(vm: &Vm, dir: RawDirectory) -> heapless::String<12> {
    let existing = critical_section::with(|cs| FILE_NAME.borrow_ref(cs).clone());
    if !existing.is_empty() {
        return existing;
    }
    let mut name: heapless::String<12> = heapless::String::new();
    // Real GPS time only: the demo build clock is not worth naming files by.
    if let Some(utc) = gnss::utc_ms(now_ms()) {
        let c = clock::civil(utc);
        let _ = write!(name, "{:02}{:02}{:02}{:02}.CSV", c.month, c.day, c.hour, c.minute);
        if vm.find_directory_entry(dir, name.as_str()).is_err() {
            critical_section::with(|cs| *FILE_NAME.borrow_ref_mut(cs) = name.clone());
            return name;
        }
        name.clear(); // taken (two boots in one minute): fall through
    }
    let mut max = 0u32;
    let _ = vm.iterate_dir(dir, |e| {
        let base = e.name.base_name();
        if e.name.extension() == b"CSV" && base.len() == 8 && base.starts_with(b"LOG") {
            let n = base[3..].iter().try_fold(0u32, |acc, &d| d.is_ascii_digit().then(|| acc * 10 + u32::from(d - b'0')));
            if let Some(n) = n {
                max = max.max(n);
            }
        }
        ControlFlow::Continue(())
    });
    let _ = write!(name, "LOG{:05}.CSV", (max + 1) % 100_000);
    critical_section::with(|cs| *FILE_NAME.borrow_ref_mut(cs) = name.clone());
    name
}

/// Open volume 0, the root directory and this boot's file; write the header
/// if the file is new.
fn open(vm: &Vm) -> Result<RawFile, SdError> {
    let vol = vm.open_raw_volume(VolumeIdx(0))?;
    let dir = vm.open_root_dir(vol)?;
    let name = choose_name(vm, dir);
    let file = vm.open_file_in_dir(dir, name.as_str(), Mode::ReadWriteCreateOrAppend)?;
    if vm.file_length(file)? == 0 {
        vm.write(file, csv::HEADER.as_bytes())?;
    }
    vm.flush_file(file)?;
    Ok(file)
}

fn report(what: &str, e: &SdError) {
    CARD_ERRORS.fetch_add(1, Relaxed);
    let mut m: heapless::String<96> = heapless::String::new();
    let _ = write!(m, "sd {what}: {e:?}");
    usb::msg("warn", &m);
}

/// Drain the pipe into the open file until a card error.
async fn record(vm: &Vm, file: RawFile, bus: &Bus) -> SdError {
    let mut chunk = [0u8; 512];
    let mut last_flush = now_ms();
    let mut last_io = now_ms();
    let mut dirty = false;
    loop {
        let n = with_timeout(Duration::from_secs(1), ROWS.read(&mut chunk)).await.unwrap_or(0);
        let now = now_ms();
        bus.set(Speed::Sd);
        if n > 0 {
            if let Err(e) = vm.write(file, &chunk[..n]) {
                return e;
            }
            BYTES_WRITTEN.fetch_add(n as u32, Relaxed);
            dirty = true;
            last_io = now;
        }
        if dirty && now.saturating_sub(last_flush) >= FLUSH_EVERY_MS {
            if let Err(e) = vm.flush_file(file) {
                return e;
            }
            dirty = false;
            last_flush = now;
        } else if !dirty && now.saturating_sub(last_io) >= PRESENCE_EVERY_MS {
            // Nothing to write: make sure the card is still there (reads
            // its CSD), so "rec" goes false soon after it is pulled out.
            if let Err(e) = vm.device(|d| d.num_bytes()) {
                return embedded_sdmmc::Error::DeviceError(e);
            }
            last_io = now;
        }
    }
}

#[embassy_executor::task]
pub async fn task(bus: &'static Bus, cs: Output<'static>) {
    let Ok(dev) = RefCellDevice::new(&bus.spi, cs, Delay::new());
    // Few acquire retries: the probe has already seen the card answer.
    let opts = AcquireOpts { use_crc: true, acquire_retries: 5 };
    let mut card = Some(SdCard::new_with_options(dev, Delay::new(), opts));
    loop {
        let Some(c) = card.take() else { return };
        bus.set(Speed::SdInit);
        let present = probe(&c, bus);
        crate::screen::SD_PROBED.store(true, Relaxed);
        if !present {
            card = Some(c);
            set_state(State::NoCard);
            Timer::after(PROBE_EVERY).await;
            continue;
        }
        // A card answered: full init (still at 400 kHz), then fast.
        c.mark_card_uninit();
        if let Err(e) = c.num_bytes() {
            report("init", &embedded_sdmmc::Error::DeviceError(e));
            card = Some(c);
            set_state(State::Error);
            Timer::after(ERROR_RETRY).await;
            continue;
        }
        bus.set(Speed::Sd);
        let vm: Vm = VolumeManager::new_with_limits(c, GpsClock, 1);
        let retry = match open(&vm) {
            Ok(file) => {
                set_state(State::Recording);
                let e = record(&vm, file, bus).await;
                report("write", &e);
                // Pulled out, most likely: back to probing promptly.
                set_state(State::NoCard);
                PROBE_EVERY
            }
            Err(e) => {
                report("open", &e);
                set_state(State::Error);
                ERROR_RETRY
            }
        };
        // Throw away every handle with the volume manager; the card driver
        // comes back out and is re-initialised on the next attempt.
        let (c, _) = vm.free();
        // Rows queued for a card that is gone would land in the wrong place
        // in the next file: drop them.
        let mut sink = [0u8; 256];
        while ROWS.try_read(&mut sink).is_ok() {}
        card = Some(c);
        Timer::after(retry).await;
    }
}
