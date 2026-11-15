//! SD-card log on the P4 hub: the S3 hub's CSV log (`firmware/hub/src/sd.rs`,
//! `tracker::csv`), driven from the main loop in small steps.
//!
//! **Where.** `ROTORSCP\LOGnnnnn.CSV` on the card's first FAT16/FAT32
//! partition: one file per boot, numbered one past the highest already
//! there, `tracker::csv::HEADER` first. The P4 has no GPS, so no time-based
//! names. Only that folder and our own files are ever created or written;
//! nothing is deleted and the card is never formatted. exFAT (most cards over
//! 32 GB) is not supported by `embedded-sdmmc`: such a card is reported and
//! left alone.
//!
//! **What.** Once a second, one row per *active* drone, open FPV link, RF
//! emitter and Wi-Fi drone (`csv::row`, `link_row`, `rf_row`,
//! `wifi_drone_row`); the P4 tracker keeps no ADS-B aircraft. Rows wait in a
//! RAM buffer ([`PENDING`] bytes; when it is full new rows are dropped and
//! counted).
//!
//! **Never block the loop for long.** [`Logger::tick`] does at most one step
//! per call: one probe, one ACMD41, the mount, one block-aligned chunk of at
//! most 512 bytes, or one flush. While the card is still programming the
//! last block (DAT0 low) it does nothing at all. The file is flushed (its
//! directory entry updated) every [`FLUSH_EVERY_MS`], so a power cut loses
//! at most that much.
//!
//! **No card.** Every [`PROBE_EVERY_MS`] a probe (wake-up clocks, CMD0, CMD8,
//! CMD55) looks for a card: about 1 ms at 400 kHz. Any card error while
//! recording drops back to probing; a card put back gets a new file.
//!
//! **Proof of writing.** While the file is small ([`READBACK_MAX`]) each
//! flush reads the whole file back and counts its lines; at mount the
//! previous boot's file is counted too. Both are printed.

use core::fmt::Write as _;
use core::ops::ControlFlow;

use embedded_sdmmc::{Mode, RawDirectory, RawFile, RawVolume, TimeSource, Timestamp, VolumeIdx, VolumeManager};
use esp_hal::gpio::Flex;
use esp_hal::time::Instant;
use esp_println::println;
use tracker::TrackState;
use tracker::csv::{self, MAX_ROW};

use crate::app::HubTracker;
use crate::sdbb::{BbPins, Card, SdDev};

const DIR: &str = "ROTORSCP";
const PENDING: usize = 8192;
const ROW_EVERY_MS: u64 = 1_000;
const FLUSH_EVERY_MS: u64 = 5_000;
const PROBE_EVERY_MS: u64 = 3_000;
/// A card that answered but could not be used: try again this much later.
const ERROR_RETRY_MS: u64 = 15_000;
/// ACMD41 spacing and limit (spec: ready within 1 s).
const POLL_EVERY_MS: u64 = 10;
const POLL_FOR_MS: u64 = 1_500;
/// With nothing to write, check the card is still there this often (CMD13).
const PRESENCE_EVERY_MS: u64 = 3_000;
/// Read the file back after a flush while it is at most this long.
const READBACK_MAX: u32 = 16 * 1024;
/// Count the previous boot's file at mount if it is at most this long.
const PREVIOUS_MAX: u32 = 32 * 1024;
const STATS_EVERY_MS: u64 = 5_000;

type Vm = VolumeManager<SdDev, NoClock, 2, 2, 1>;
type FsError = embedded_sdmmc::Error<sdnative::Error>;

/// No clock on the P4 yet: FAT timestamps say 2026-01-01 00:00.
pub struct NoClock;

impl TimeSource for NoClock {
    fn get_timestamp(&self) -> Timestamp {
        Timestamp { year_since_1970: 56, zero_indexed_month: 0, zero_indexed_day: 0, hours: 0, minutes: 0, seconds: 0 }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    /// Probe again at this time.
    NoCard(u64),
    /// ACMD41 in progress since, next poll at.
    Waking(u64, u64),
    Recording,
    /// A card answered but could not be used; retry at this time.
    Error(u64),
}

/// Handles kept open while recording (all dropped together with the volume
/// manager when the card goes away).
#[allow(dead_code)]
struct Open {
    vol: RawVolume,
    root: RawDirectory,
    dir: RawDirectory,
    file: RawFile,
}

pub struct Logger {
    card: Option<Card>,
    vm: Option<Vm>,
    open: Option<Open>,
    state: State,
    name: heapless::String<12>,
    pending: heapless::Vec<u8, PENDING>,
    /// Our idea of the file length (we only append).
    pos: u32,
    flushed_at: u64,
    dirty: bool,
    last_rows: u64,
    last_io: u64,
    last_stats: u64,
    /// Lines in the file: header plus rows written to it.
    lines: u32,
    pub rows: u32,
    pub dropped: u32,
    pub errors: u32,
    pub flushes: u32,
    busy_skips: u32,
    max_tick_us: u64,
    max_tick_what: &'static str,
}

impl Logger {
    pub fn new(clk: Flex<'static>, cmd: Flex<'static>, dat: Flex<'static>) -> Self {
        let pins = BbPins::new(clk, cmd, dat);
        let (slow, fast) = pins.spins();
        let (c, d) = pins.idle_levels();
        println!("hub-p4: sd 1-bit on CLK 43 CMD 44 D0 39: idle cmd/d0 high = {c}/{d}, half-period spins slow {slow} fast {fast}");
        Self {
            card: Some(sdnative::SdNative::new(pins)),
            vm: None,
            open: None,
            state: State::NoCard(0),
            name: heapless::String::new(),
            pending: heapless::Vec::new(),
            pos: 0,
            flushed_at: 0,
            dirty: false,
            last_rows: 0,
            last_io: 0,
            last_stats: 0,
            lines: 0,
            rows: 0,
            dropped: 0,
            errors: 0,
            flushes: 0,
            busy_skips: 0,
            max_tick_us: 0,
            max_tick_what: "",
        }
    }

    /// One step. Call every main-loop pass.
    pub fn tick(&mut self, tracker: &HubTracker, now: u64) {
        let t0 = Instant::now();
        let what = self.step(tracker, now);
        let us = t0.elapsed().as_micros();
        if us > self.max_tick_us {
            self.max_tick_us = us;
            self.max_tick_what = what;
        }
        if now.saturating_sub(self.last_stats) >= STATS_EVERY_MS {
            self.last_stats = now;
            self.print_stats();
        }
    }

    fn print_stats(&mut self) {
        let state = match self.state {
            State::NoCard(_) => "no_card",
            State::Waking(..) => "waking",
            State::Recording => "recording",
            State::Error(_) => "error",
        };
        let io = self.io_stats();
        println!(
            "hub-p4: sd {state} file={}/{} rows={} dropped={} pending={} bytes={} flushes={} errors={} | blocks r={} w={} io_err={} busy_skips={} max_io={}us max_tick={}us ({})",
            DIR, self.name, self.rows, self.dropped, self.pending.len(), self.pos, self.flushes, self.errors,
            io.blocks_read, io.blocks_written, io.errors, self.busy_skips, io.max_io_us, self.max_tick_us, self.max_tick_what
        );
        self.max_tick_us = 0;
        if let Some(vm) = &self.vm {
            vm.device(|d| d.stats.borrow_mut().max_io_us = 0);
        }
    }

    fn io_stats(&self) -> crate::sdbb::IoStats {
        match &self.vm {
            Some(vm) => vm.device(|d| *d.stats.borrow()),
            None => Default::default(),
        }
    }

    fn step(&mut self, tracker: &HubTracker, now: u64) -> &'static str {
        match self.state {
            State::NoCard(at) | State::Error(at) => {
                if now < at {
                    return "idle";
                }
                let Some(card) = self.card.as_mut() else { return "idle" };
                let r = card.begin().and_then(|_| card.poll_ready());
                match r {
                    Ok(true) => self.identify(now),
                    Ok(false) => self.state = State::Waking(now, now + POLL_EVERY_MS),
                    Err(sdnative::Error::NoResponse(_)) => {
                        if !matches!(self.state, State::NoCard(_)) || at == 0 {
                            println!("hub-p4: sd no card");
                        }
                        self.state = State::NoCard(now + PROBE_EVERY_MS);
                    }
                    Err(e) => self.fail("probe", &e, now),
                }
                "probe"
            }
            State::Waking(since, next) => {
                if now < next {
                    return "idle";
                }
                let Some(card) = self.card.as_mut() else { return "idle" };
                match card.poll_ready() {
                    Ok(true) => {
                        self.identify(now);
                        "identify+mount"
                    }
                    Ok(false) if now.saturating_sub(since) < POLL_FOR_MS => {
                        self.state = State::Waking(since, now + POLL_EVERY_MS);
                        "acmd41"
                    }
                    Ok(false) => {
                        self.fail("init", &sdnative::Error::NotReady, now);
                        "acmd41"
                    }
                    Err(e) => {
                        self.fail("init", &e, now);
                        "acmd41"
                    }
                }
            }
            State::Recording => self.record(tracker, now),
        }
    }

    fn fail(&mut self, what: &str, e: &dyn core::fmt::Debug, now: u64) {
        self.errors += 1;
        println!("hub-p4: sd {what} failed: {e:?}");
        self.close_all();
        self.state = State::Error(now + ERROR_RETRY_MS);
    }

    /// Drop every handle and get the card driver back.
    fn close_all(&mut self) {
        self.open = None;
        if let Some(vm) = self.vm.take() {
            let (dev, _) = vm.free();
            self.card = Some(dev.card.into_inner());
        }
        if let Some(c) = self.card.as_mut() {
            c.reset();
        }
        self.pending.clear();
    }

    fn identify(&mut self, now: u64) {
        let Some(card) = self.card.as_mut() else { return };
        let info = match card.finish() {
            Ok(i) => i,
            Err(e) => return self.fail("identify", &e, now),
        };
        let mut rev: heapless::String<8> = heapless::String::new();
        let _ = write!(rev, "{}.{}", info.revision >> 4, info.revision & 15);
        println!(
            "hub-p4: sd card {} {} MiB ({} blocks), CSD v{}, manufacturer 0x{:02x} oem {:?} product {:?} rev {} made {}-{:02}",
            if info.high_capacity { "SDHC/SDXC" } else if info.v2 { "SDSC v2" } else { "SDSC v1" },
            info.mib(), info.blocks, info.csd_version + 1, info.manufacturer, ascii(&info.oem), ascii(&info.product), rev,
            info.year, info.month
        );
        // Speed check: time one read of block 0 (the MBR or boot sector).
        let mut card = self.card.take().unwrap();
        let mut b = [[0u8; sdnative::BLOCK]];
        let t = Instant::now();
        let r = card.read(0, &mut b);
        let us = t.elapsed().as_micros().max(1);
        println!(
            "hub-p4: sd block 0 read {:?} in {us} us (~{} kHz clock, ~{} kB/s); partition 1 type 0x{:02x}, signature {:02x}{:02x}",
            r.map(|_| ()), 4180 * 1000 / us, 512 * 1000 / us, b[0][446 + 4], b[0][510], b[0][511]
        );
        self.vm = Some(VolumeManager::new_with_limits(SdDev::new(card), NoClock, 1));
        match self.mount() {
            Ok(()) => {
                self.state = State::Recording;
                self.flushed_at = now;
                self.last_io = now;
                println!("hub-p4: sd recording to {}/{} ({} bytes already)", DIR, self.name, self.pos);
            }
            Err(e) => {
                if let embedded_sdmmc::Error::FormatError(_) = e {
                    println!(
                        "hub-p4: sd card is not FAT16/FAT32 (partition type 0x{:02x}; 0x07 = exFAT/NTFS). Left untouched; no logging.",
                        b[0][446 + 4]
                    );
                }
                self.fail("mount", &e, now);
            }
        }
    }

    fn mount(&mut self) -> Result<(), FsError> {
        let vm = self.vm.as_ref().unwrap();
        let vol = vm.open_raw_volume(VolumeIdx(0))?;
        let root = vm.open_root_dir(vol)?;
        let dir = match vm.open_dir(root, DIR) {
            Ok(d) => d,
            Err(embedded_sdmmc::Error::NotFound) => {
                println!("hub-p4: sd creating folder {DIR}");
                vm.make_dir_in_dir(root, DIR)?;
                vm.open_dir(root, DIR)?
            }
            Err(e) => return Err(e),
        };
        let mut max = 0u32;
        let mut count = 0u32;
        vm.iterate_dir(dir, |e| {
            let base = e.name.base_name();
            if e.name.extension() == b"CSV" && base.len() == 8 && base.starts_with(b"LOG") {
                let n = base[3..].iter().try_fold(0u32, |acc, &d| d.is_ascii_digit().then(|| acc * 10 + u32::from(d - b'0')));
                if let Some(n) = n {
                    max = max.max(n);
                    count += 1;
                }
            }
            ControlFlow::Continue(())
        })?;
        if max > 0 {
            let mut prev: heapless::String<12> = heapless::String::new();
            let _ = write!(prev, "LOG{max:05}.CSV");
            let f = vm.open_file_in_dir(dir, prev.as_str(), Mode::ReadOnly)?;
            let len = vm.file_length(f)?;
            if len <= PREVIOUS_MAX {
                let (lines, bytes) = count_lines(vm, f)?;
                println!("hub-p4: sd previous file {DIR}/{prev}: {len} bytes, read back {bytes} bytes, {lines} lines");
            } else {
                println!("hub-p4: sd previous file {DIR}/{prev}: {len} bytes (too long to count here)");
            }
            vm.close_file(f)?;
        }
        println!("hub-p4: sd {count} log files in {DIR}");
        self.name.clear();
        let _ = write!(self.name, "LOG{:05}.CSV", (max + 1) % 100_000);
        let file = vm.open_file_in_dir(dir, self.name.as_str(), Mode::ReadWriteCreateOrAppend)?;
        self.pos = vm.file_length(file)?;
        self.lines = 0;
        if self.pos == 0 {
            vm.write(file, csv::HEADER.as_bytes())?;
            self.pos = csv::HEADER.len() as u32;
            self.lines = 1;
        }
        vm.flush_file(file)?;
        self.open = Some(Open { vol, root, dir, file });
        self.pending.clear();
        self.dirty = false;
        Ok(())
    }

    fn record(&mut self, tracker: &HubTracker, now: u64) -> &'static str {
        if now.saturating_sub(self.last_rows) >= ROW_EVERY_MS {
            self.last_rows = now;
            self.queue_rows(tracker, now);
        }
        let (Some(vm), Some(open)) = (self.vm.as_ref(), self.open.as_ref()) else { return "idle" };
        let file = open.file;
        if vm.device(|d| d.card.borrow_mut().is_busy()) {
            self.busy_skips += 1;
            return "busy";
        }
        // One block-aligned chunk.
        let room = 512 - (self.pos % 512) as usize;
        let flush_due = now.saturating_sub(self.flushed_at) >= FLUSH_EVERY_MS;
        let mut what = "idle";
        let r = (|| -> Result<(), FsError> {
            if self.pending.len() >= room || (flush_due && !self.pending.is_empty()) {
                let n = room.min(self.pending.len());
                vm.write(file, &self.pending[..n])?;
                self.lines += self.pending[..n].iter().filter(|&&b| b == b'\n').count() as u32;
                self.pending.copy_within(n.., 0);
                self.pending.truncate(self.pending.len() - n);
                self.pos += n as u32;
                self.dirty = true;
                self.last_io = now;
                what = "write";
            } else if flush_due {
                self.flushed_at = now;
                if self.dirty {
                    vm.flush_file(file)?;
                    self.dirty = false;
                    self.flushes += 1;
                    self.last_io = now;
                    what = "flush";
                    if self.pos <= READBACK_MAX {
                        let (lines, bytes) = count_lines(vm, file)?;
                        vm.file_seek_from_end(file, 0)?;
                        let ok = lines == self.lines && bytes == self.pos;
                        println!(
                            "hub-p4: sd readback {DIR}/{}: {bytes} bytes {lines} lines, expected {} bytes {} lines: {}",
                            self.name, self.pos, self.lines, if ok { "OK" } else { "MISMATCH" }
                        );
                        what = "flush+readback";
                    }
                }
            } else if now.saturating_sub(self.last_io) >= PRESENCE_EVERY_MS {
                self.last_io = now;
                vm.device(|d| d.card.borrow_mut().status()).map_err(embedded_sdmmc::Error::DeviceError)?;
                what = "status";
            }
            Ok(())
        })();
        if let Err(e) = r {
            let rows_lost = self.pending.len();
            self.fail("write", &e, now);
            println!("hub-p4: sd lost the card while recording ({rows_lost} bytes unwritten); probing again");
            self.state = State::NoCard(now + PROBE_EVERY_MS);
        }
        what
    }

    fn queue_rows(&mut self, tracker: &HubTracker, now: u64) {
        let ctx = tracker.context(now, None);
        let mut row = [0u8; MAX_ROW];
        for t in tracker.tracks().iter().filter(|t| ctx.state(t) == TrackState::Active) {
            let r = csv::row(t, &ctx, &mut row);
            self.push_row(&row, r);
        }
        for l in tracker.open_links().iter().filter(|l| ctx.link_state(l) == TrackState::Active) {
            let r = csv::link_row(l, &ctx, &mut row);
            self.push_row(&row, r);
        }
        for e in tracker.emitters().iter().filter(|e| ctx.emitter_state(e) == TrackState::Active) {
            let r = csv::rf_row(e, &ctx, &mut row);
            self.push_row(&row, r);
        }
        for w in tracker.wifi_drones().iter().filter(|w| ctx.wifi_drone_state(w) == TrackState::Active) {
            let r = csv::wifi_drone_row(w, &ctx, &mut row);
            self.push_row(&row, r);
        }
    }

    fn push_row<E>(&mut self, row: &[u8; MAX_ROW], r: Result<usize, E>) {
        let Ok(n) = r else { return };
        if self.pending.extend_from_slice(&row[..n]).is_ok() {
            self.rows += 1;
        } else {
            self.dropped += 1;
        }
    }
}

/// Read `file` from the start and count newlines: (lines, bytes).
fn count_lines(vm: &Vm, file: RawFile) -> Result<(u32, u32), FsError> {
    vm.file_seek_from_start(file, 0)?;
    let mut buf = [0u8; 512];
    let (mut lines, mut bytes) = (0u32, 0u32);
    loop {
        let n = vm.read(file, &mut buf)?;
        if n == 0 {
            break;
        }
        bytes += n as u32;
        lines += buf[..n].iter().filter(|&&b| b == b'\n').count() as u32;
        if vm.file_eof(file)? {
            break;
        }
    }
    Ok((lines, bytes))
}

fn ascii(b: &[u8]) -> &str {
    core::str::from_utf8(b).unwrap_or("?").trim_end_matches('\0')
}
