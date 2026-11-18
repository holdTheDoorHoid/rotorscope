//! SD-card log on the P4 hub: the S3 hub's CSV log (`firmware/hub/src/sd.rs`,
//! `tracker::csv`), driven from the main loop in small steps.
//!
//! **Where.** `ROTORSCP\` on the card's first FAT16/FAT32 partition, one
//! file per boot, `tracker::csv::HEADER` first. With GPS time at mount:
//! `MMDDhhmm.CSV` (UTC, as the S3 hub); without: `LOGnnnnn.CSV`, numbered
//! one past the highest already there. While a GPS is answering but has no
//! time yet, the mount waits up to [`WAIT_FOR_TIME_MS`] after boot (rows
//! are kept in RAM meanwhile), so a normal start gets a dated name. FAT
//! timestamps and the CSV `utc` column come from GPS time too. Only that folder and our own files are ever created or written;
//! nothing is deleted and the card is never formatted. exFAT (most cards over
//! 32 GB) is not supported by `embedded-sdmmc`: such a card is reported and
//! left alone.
//!
//! **What.** Once a second, one row per *active* drone, open FPV link, RF
//! emitter, Wi-Fi drone and ADS-B aircraft (`csv::row`, `link_row`,
//! `rf_row`, `wifi_drone_row`, `aircraft_row`). Rows wait in a
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
//! **Evidence log.** `ROTORSCP/EVIDENCE/EVnnnnnn.EVL`, one new file per
//! mount, numbered one past the highest there (never rewritten: a new file
//! never takes a name that exists; the number of a deleted file, the newest
//! one's included, can come back, docs/EVIDENCE.md). The queue lives in `evlog.rs`; here it is appended in
//! block-aligned chunks ahead of the CSV, and flushed only at a record
//! boundary, after which the flushed records leave the queue. At mount the
//! previous evidence file's last record is read and named in the new file's
//! first event, so the files link up. Console `r` replays the current file.
//! The folder is not trusted (`evidence::files`): only real files with an
//! in-range number count, a name taken by a folder is skipped, and a taken
//! top number (`EV999999.EVL`) makes the next file the lowest free number
//! instead of failing every mount. If the
//! evidence folder or file cannot be opened while the CSV log records, the
//! mount is retried every [`EV_RETRY_MS`], and the screen shows evidence
//! as not written (`Logger::evidence_stop`) whenever it is not.
//!
//! **Proof of writing.** While the file is small ([`READBACK_MAX`]) each
//! flush reads the whole file back and counts its lines; at mount the
//! previous boot's file is counted too. Both are printed.

use core::fmt::Write as _;
use core::ops::ControlFlow;

use embedded_sdmmc::{Mode, RawDirectory, RawFile, RawVolume, TimeSource, Timestamp, VolumeIdx, VolumeManager};
use esp_hal::gpio::Flex;
use esp_hal::time::Instant;
use tracker::TrackState;
use tracker::csv::{self, MAX_ROW};

use crate::app::HubTracker;
use crate::evlog::Evidence;
use crate::faa::Faa;
use crate::sdbb::{BbPins, Card, SdDev};

/// The airspace pack (PLAN.md Phase 12), in the same folder.
const PACK_NAME: &str = "AIRSPACE.PAK";
/// Pack bytes read per main-loop step (~2 blocks, ~12 ms bit-banged).
const PACK_READ: usize = 1024;
/// `embedpack` builds: the pack built into the firmware, written to the card
/// when the card has none or a different one (`AIRSPACE_PAK` at build time).
#[cfg(feature = "embedpack")]
static EMBEDDED: &[u8] = include_bytes!(env!("AIRSPACE_PAK"));

const DIR: &str = "ROTORSCP";
/// Evidence files, inside `DIR`.
const EV_DIR: &str = "EVIDENCE";
/// Bytes of the previous evidence file read at mount to find its last record.
const EV_TAIL: usize = 2048;
const PENDING: usize = 8192;
const ROW_EVERY_MS: u64 = 1_000;
const FLUSH_EVERY_MS: u64 = 5_000;
const PROBE_EVERY_MS: u64 = 3_000;
/// A card that answered but could not be used: try again this much later.
const ERROR_RETRY_MS: u64 = 15_000;
/// The card records but the evidence folder or file could not be opened:
/// try again this often (the screen says evidence is not written meanwhile).
const EV_RETRY_MS: u64 = 15_000;
/// ACMD41 spacing and limit (spec: ready within 1 s).
const POLL_EVERY_MS: u64 = 10;
const POLL_FOR_MS: u64 = 1_500;
/// With nothing to write, check the card is still there this often (CMD13).
const PRESENCE_EVERY_MS: u64 = 3_000;
/// Read the file back after a flush while it is at most this long. Each
/// block costs ~6 ms bit-banged, so keep this small: it is a proof, not a
/// feature.
const READBACK_MAX: u32 = 2 * 1024;
/// Count the previous boot's file at mount if it is at most this long.
const PREVIOUS_MAX: u32 = 16 * 1024;
const STATS_EVERY_MS: u64 = 5_000;

/// Hold the mount this long after boot for GPS time (see the module docs).
const WAIT_FOR_TIME_MS: u64 = 90_000;
/// ... and always this long after boot, for the GPS to be found.
const GPS_FIND_MS: u64 = 8_000;

/// Directories: root, ROTORSCP, EVIDENCE. Files: CSV, pack, evidence (and
/// the previous evidence file briefly at mount).
type Vm = VolumeManager<SdDev, GpsClock, 4, 4, 1>;
type FsError = embedded_sdmmc::Error<sdnative::Error>;

/// UTC (Unix ms) at hub time (ms), set from the GPS by [`set_clock`].
static CLOCK: critical_section::Mutex<core::cell::Cell<Option<(u64, u64)>>> =
    critical_section::Mutex::new(core::cell::Cell::new(None));

/// Tell the FAT layer what time it is (`utc_ms` at hub time `now_ms`).
pub fn set_clock(utc_ms: Option<u64>, now_ms: u64) {
    critical_section::with(|cs| CLOCK.borrow(cs).set(utc_ms.map(|u| (u, now_ms))));
}

fn clock_utc() -> Option<u64> {
    let (u, at) = critical_section::with(|cs| CLOCK.borrow(cs).get())?;
    Some(u + crate::app::now_ms().saturating_sub(at))
}

/// FAT timestamps from GPS time; 2026-01-01 00:00 without it.
pub struct GpsClock;

impl TimeSource for GpsClock {
    fn get_timestamp(&self) -> Timestamp {
        match clock_utc().map(crate::civil::civil) {
            Some(c) => Timestamp {
                year_since_1970: (c.year - 1970).clamp(0, 255) as u8,
                zero_indexed_month: c.month - 1,
                zero_indexed_day: c.day - 1,
                hours: c.hour,
                minutes: c.minute,
                seconds: c.second,
            },
            None => Timestamp { year_since_1970: 56, zero_indexed_month: 0, zero_indexed_day: 0, hours: 0, minutes: 0, seconds: 0 },
        }
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

/// The open evidence file.
struct EvFile {
    #[allow(dead_code)]
    dir: RawDirectory,
    file: RawFile,
    name: heapless::String<12>,
    /// Bytes in the file (we only append).
    pos: u32,
    dirty: bool,
    flushed_at: u64,
}

/// Console `r`: the current evidence file, re-streamed from the start.
struct Replay {
    pos: u32,
    buf: heapless::Vec<u8, { 2 * evidence::format::MAX_RECORD }>,
    sent: u32,
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
    utc_ms: Option<u64>,
    holding: bool,
    max_tick_us: u64,
    max_tick_what: &'static str,
    /// The pack being copied into PSRAM (or, `embedpack`, written).
    pack_file: Option<RawFile>,
    #[cfg(feature = "embedpack")]
    pack_writing: Option<u32>,
    shown_flag_row: bool,
    ev_file: Option<EvFile>,
    /// While recording without an evidence file: when to try again.
    ev_retry_at: u64,
    replay: Option<Replay>,
    pub ev_flushes: u32,
    pub ev_errors: u32,
    max_ev_us: u64,
    max_ev_what: &'static str,
    /// The owner's O4 key, read from the card at mount, until the main loop
    /// takes it ([`Logger::take_o4_key`]).
    o4_key: Option<djicrypt::PrivateKey>,
    /// What the key file check found, for the status line.
    pub o4_key_state: &'static str,
    /// The FLARM legacy key words (`ROTORSCP/FLARMKEY.TXT`), read at mount,
    /// until the main loop takes them ([`Logger::take_flarm_keys`]).
    flarm_keys: Option<flarm::Keys>,
    /// none / loaded / bad / error, for the status line.
    pub flarm_key_state: &'static str,
    /// `ROTORSCP/SITE.TXT` as read at mount, until the main loop takes it.
    site: Option<tracker::site::SiteState>,
}

/// Node positions and muted alarms (`tracker::site`). The hub only ever
/// appends to it, so hand edits are never lost.
const SITE_NAME: &str = "SITE.TXT";


/// The owner's O4 DroneID private key (64 hex digits) in the log folder.
/// Never in the repository; see docs/DRONEID-O4.md "Decryption".
const O4_KEY_NAME: &str = "O4KEY.TXT";

/// The FLARM legacy key constants (`flarm::Keys` format, docs/AERO-NODE.md
/// "Hub side") in the log folder. Never in the repository, never printed.
const FLARM_KEY_NAME: &str = "FLARMKEY.TXT";

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
            utc_ms: None,
            holding: false,
            max_tick_us: 0,
            max_tick_what: "",
            pack_file: None,
            #[cfg(feature = "embedpack")]
            pack_writing: None,
            shown_flag_row: false,
            ev_file: None,
            ev_retry_at: 0,
            replay: None,
            ev_flushes: 0,
            ev_errors: 0,
            max_ev_us: 0,
            max_ev_what: "",
            o4_key: None,
            o4_key_state: "none",
            flarm_keys: None,
            flarm_key_state: "none",
            site: None,
        }
    }

    /// The O4 key read from the card, once (the decryptor keeps it).
    pub fn take_o4_key(&mut self) -> Option<djicrypt::PrivateKey> {
        self.o4_key.take()
    }

    /// The FLARM keys read from the card, once (`aero.rs` keeps them).
    #[inline(never)]
    pub fn take_flarm_keys(&mut self) -> Option<flarm::Keys> {
        self.flarm_keys.take()
    }

    /// The site file read at mount, once.
    pub fn take_site(&mut self) -> Option<tracker::site::SiteState> {
        self.site.take()
    }

    /// Read `ROTORSCP/SITE.TXT` if present, a piece at a time (it only
    /// grows, so it can be any length; later lines win).
    fn load_site(&mut self) {
        use tracker::site::{Lines, SiteState, parse_line, read_into};
        let (Some(vm), Some(dir)) = (self.vm.as_ref(), self.open.as_ref().map(|o| o.dir)) else { return };
        let f = match vm.open_file_in_dir(dir, SITE_NAME, Mode::ReadOnly) {
            Ok(f) => f,
            Err(embedded_sdmmc::Error::NotFound) => {
                println!("hub-p4: no {DIR}/{SITE_NAME}: no node positions or muted alarms yet");
                return;
            }
            Err(e) => {
                println!("hub-p4: {DIR}/{SITE_NAME} open failed: {e:?}");
                return;
            }
        };
        let (mut st, mut lines) = (SiteState::default(), Lines::default());
        let refused = |n: u32, e: &str| println!("hub-p4: {SITE_NAME} line {n} ignored: {e}");
        let mut buf = [0u8; 256];
        let mut total = 0usize;
        loop {
            match vm.read(f, &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(k) => {
                    total += k;
                    read_into(&mut st, &mut lines, &buf[..k], refused);
                }
            }
        }
        let _ = vm.close_file(f);
        lines.finish(|n, r| match r.and_then(parse_line) {
            Ok(l) => st.apply(l),
            Err(e) => {
                st.ignored += 1;
                refused(n, e);
            }
        });
        println!("hub-p4: {DIR}/{SITE_NAME} read ({total} bytes, {} lines)", lines.number);
        self.site = Some(st);
    }

    /// Append `text` (whole lines) to `ROTORSCP/SITE.TXT`, creating it.
    /// Returns false when there is no card or the write failed.
    pub fn append_site(&mut self, text: &str) -> bool {
        let (Some(vm), Some(dir)) = (self.vm.as_ref(), self.open.as_ref().map(|o| o.dir)) else { return false };
        let Ok(f) = vm.open_file_in_dir(dir, SITE_NAME, Mode::ReadWriteCreateOrAppend) else { return false };
        let ok = vm.write(f, text.as_bytes()).is_ok();
        vm.close_file(f).is_ok() && ok
    }

    /// Read `ROTORSCP/O4KEY.TXT` if present. Only a fingerprint of the
    /// matching public key is printed; the file buffer is wiped.
    fn load_o4_key(&mut self) {
        use zeroize::Zeroize;
        let (Some(vm), Some(dir)) = (self.vm.as_ref(), self.open.as_ref().map(|o| o.dir)) else { return };
        let f = match vm.open_file_in_dir(dir, O4_KEY_NAME, Mode::ReadOnly) {
            Ok(f) => f,
            Err(embedded_sdmmc::Error::NotFound) => {
                self.o4_key_state = "none";
                println!("hub-p4: no {DIR}/{O4_KEY_NAME}: O4 DroneID stays detect-only");
                return;
            }
            Err(e) => {
                self.o4_key_state = "error";
                println!("hub-p4: {DIR}/{O4_KEY_NAME} open failed: {e:?}");
                return;
            }
        };
        let mut buf = [0u8; 512];
        let n = vm.read(f, &mut buf).unwrap_or(0);
        let _ = vm.close_file(f);
        match djicrypt::PrivateKey::from_hex_text(&buf[..n]) {
            Ok(k) => {
                let fp = k.fingerprint();
                println!(
                    "hub-p4: O4 DroneID key loaded from {DIR}/{O4_KEY_NAME} (public fingerprint {:02x}{:02x}-{:02x}{:02x})",
                    fp[0], fp[1], fp[2], fp[3]
                );
                self.o4_key = Some(k);
                self.o4_key_state = "loaded";
            }
            Err(e) => {
                self.o4_key_state = "bad";
                println!("hub-p4: {DIR}/{O4_KEY_NAME} refused ({e:?}): O4 stays detect-only");
            }
        }
        buf.zeroize();
    }

    /// Read `ROTORSCP/FLARMKEY.TXT` if present. Nothing of its contents is
    /// ever printed (a refusal names a line number only); the file buffer
    /// is wiped, and the parsed words wipe themselves when dropped.
    #[inline(always)]
    fn load_flarm_keys(&mut self) {
        use zeroize::Zeroize;
        let (Some(vm), Some(dir)) = (self.vm.as_ref(), self.open.as_ref().map(|o| o.dir)) else { return };
        let f = match vm.open_file_in_dir(dir, FLARM_KEY_NAME, Mode::ReadOnly) {
            Ok(f) => f,
            Err(embedded_sdmmc::Error::NotFound) => {
                self.flarm_key_state = "none";
                println!("hub-p4: no {DIR}/{FLARM_KEY_NAME}: FLARM aircraft are counted, not decoded");
                return;
            }
            Err(e) => {
                self.flarm_key_state = "error";
                println!("hub-p4: {DIR}/{FLARM_KEY_NAME} open failed: {e:?}");
                return;
            }
        };
        // A key file is ~400 bytes; room for generous comments.
        let mut buf = [0u8; 1024];
        let mut n = 0;
        while n < buf.len() {
            match vm.read(f, &mut buf[n..]) {
                Ok(0) | Err(_) => break,
                Ok(k) => n += k,
            }
        }
        let too_long = n == buf.len();
        let _ = vm.close_file(f);
        let parsed = match core::str::from_utf8(&buf[..n]) {
            _ if too_long => Err("longer than 1024 bytes"),
            Ok(text) => flarm::Keys::parse(text).map_err(|e| match e {
                flarm::KeyError::Syntax(_) => "a line without '='",
                flarm::KeyError::UnknownName(_) => "a name other than key1..key5",
                flarm::KeyError::Duplicate(_) => "a name given twice",
                flarm::KeyError::BadWord(_) => "a value that is not a 32-bit hex word",
                flarm::KeyError::WordCount(_) => "a wrong number of words",
                flarm::KeyError::Missing(_) => "a key missing",
            }),
            Err(_) => Err("not text"),
        };
        buf.zeroize();
        match parsed {
            Ok(k) => {
                self.flarm_keys = Some(k);
                self.flarm_key_state = "loaded";
                println!("hub-p4: FLARM keys loaded from {DIR}/{FLARM_KEY_NAME} (contents never shown)");
            }
            Err(why) => {
                self.flarm_key_state = "bad";
                println!("hub-p4: {DIR}/{FLARM_KEY_NAME} refused ({why}): FLARM aircraft are counted, not decoded");
            }
        }
    }

    /// Console `r`: re-stream the current evidence file from its start.
    pub fn start_replay(&mut self) -> bool {
        if self.ev_file.is_none() {
            return false;
        }
        self.replay = Some(Replay { pos: 0, buf: heapless::Vec::new(), sent: 0 });
        true
    }

    /// Console `e`: flush the evidence file as soon as the queue is written.
    pub fn flush_now(&mut self) {
        if let Some(f) = self.ev_file.as_mut() {
            f.flushed_at = 0;
        }
    }

    pub fn ev_name(&self) -> &str {
        self.ev_file.as_ref().map_or("-", |f| f.name.as_str())
    }

    /// One step. Call every main-loop pass. `utc_ms`: GPS time now, if
    /// known; `gps_present`: a receiver is answering (worth waiting for its
    /// time before naming the file).
    pub fn tick(&mut self, tracker: &HubTracker, faa: &mut Faa, ev: &mut Evidence, now: u64, utc_ms: Option<u64>, gps_present: bool) {
        let t0 = Instant::now();
        self.utc_ms = utc_ms;
        // The first few seconds too: the GPS hunt needs ~1-3 s to find a
        // receiver at all.
        let hold = self.state != State::Recording
            && utc_ms.is_none()
            && (gps_present || now < GPS_FIND_MS)
            && now < WAIT_FOR_TIME_MS;
        if hold != self.holding {
            self.holding = hold;
            if hold && gps_present {
                println!("hub-p4: sd waiting up to {} s after boot for GPS time before opening the log", WAIT_FOR_TIME_MS / 1000);
            }
        }
        let what = if hold {
            if now.saturating_sub(self.last_rows) >= ROW_EVERY_MS {
                self.last_rows = now;
                self.queue_rows(tracker, faa, now);
            }
            "wait_time"
        } else {
            self.step(tracker, faa, ev, now)
        };
        if self.state != State::Recording {
            faa.card_gone();
        }
        let us = t0.elapsed().as_micros();
        if us > self.max_tick_us {
            self.max_tick_us = us;
            self.max_tick_what = what;
        }
        if now.saturating_sub(self.last_stats) >= STATS_EVERY_MS {
            self.last_stats = now;
            self.print_stats(ev);
        }
    }

    /// `no_card`, `waking`, `wait_time`, `recording` or `error`.
    pub fn state_name(&self) -> &'static str {
        match self.state {
            _ if self.holding => "wait_time",
            State::NoCard(_) => "no_card",
            State::Waking(..) => "waking",
            State::Recording => "recording",
            State::Error(_) => "error",
        }
    }

    /// For the screen.
    pub fn ui_state(&self) -> screen::SdLog {
        match self.state {
            State::Recording => screen::SdLog::Recording,
            State::Error(_) => screen::SdLog::Error,
            _ if self.holding => screen::SdLog::Recording,
            _ => screen::SdLog::NoCard,
        }
    }

    pub fn file_name(&self) -> &str {
        self.name.as_str()
    }

    /// For the screen: why evidence is not reaching the card, and how much
    /// waits; `None` while it is written (or briefly held at boot waiting
    /// for GPS time, when nothing is written yet by design).
    pub fn evidence_stop(&self, ev: &Evidence) -> Option<screen::EvidenceStop> {
        use screen::EvidenceStopWhy as W;
        let why = match self.state {
            State::Recording if self.ev_file.is_some() => return None,
            _ if self.holding => return None,
            State::Recording => W::FileError,
            State::Error(_) => W::CardError,
            State::NoCard(_) | State::Waking(..) => W::NoCard,
        };
        Some(screen::EvidenceStop { why, waiting: ev.queued_records(), dropped: ev.not_logged_total })
    }

    fn print_stats(&mut self, ev: &Evidence) {
        let state = self.state_name();
        let io = self.io_stats();
        println!(
            "hub-p4: sd {state} file={}/{} rows={} dropped={} pending={} bytes={} flushes={} errors={} | blocks r={} w={} io_err={} busy_skips={} max_io={}us max_tick={}us ({})",
            DIR, self.name, self.rows, self.dropped, self.pending.len(), self.pos, self.flushes, self.errors,
            io.blocks_read, io.blocks_written, io.errors, self.busy_skips, io.max_io_us, self.max_tick_us, self.max_tick_what
        );
        println!(
            "hub-p4: evidence file={}/{}/{} bytes={} seq={} queued={} records={} record_bytes={} drained={} flushes={} errors={} checkpoints={} sign={}us max_sign={}us max_build={}us max_sd_step={}us ({}) stream={} stream_drops={} not_logged={}",
            DIR, EV_DIR, self.ev_name(), self.ev_file.as_ref().map_or(0, |f| f.pos), ev.seq(), ev.queued(), ev.records, ev.bytes,
            ev.drained, self.ev_flushes, self.ev_errors, ev.checkpoints, ev.last_sign_us, ev.max_sign_us, ev.max_build_us,
            self.max_ev_us, self.max_ev_what, ev.stream, ev.stream_drops, ev.not_logged_total
        );
        self.max_tick_us = 0;
        self.max_ev_us = 0;
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

    fn step(&mut self, tracker: &HubTracker, faa: &mut Faa, ev: &mut Evidence, now: u64) -> &'static str {
        match self.state {
            State::NoCard(at) | State::Error(at) => {
                if now < at {
                    return "idle";
                }
                let Some(card) = self.card.as_mut() else { return "idle" };
                let r = card.begin().and_then(|_| card.poll_ready());
                match r {
                    Ok(true) => self.identify(faa, ev, now),
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
                        self.identify(faa, ev, now);
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
            State::Recording => self.record(tracker, faa, ev, now),
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
        self.ev_file = None;
        self.replay = None;
        self.pack_file = None;
        #[cfg(feature = "embedpack")]
        {
            self.pack_writing = None;
        }
        if let Some(vm) = self.vm.take() {
            let (dev, _) = vm.free();
            self.card = Some(dev.card.into_inner());
        }
        if let Some(c) = self.card.as_mut() {
            c.reset();
        }
        self.pending.clear();
    }

    fn identify(&mut self, faa: &mut Faa, ev: &mut Evidence, now: u64) {
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
        self.vm = Some(VolumeManager::new_with_limits(SdDev::new(card), GpsClock, 1));
        match self.mount() {
            Ok(()) => {
                self.state = State::Recording;
                self.flushed_at = now;
                self.last_io = now;
                println!("hub-p4: sd recording to {}/{} ({} bytes already)", DIR, self.name, self.pos);
                let t = Instant::now();
                match self.mount_evidence(ev, now) {
                    Ok(()) => println!(
                        "hub-p4: evidence recording to {DIR}/{EV_DIR}/{} (mount {} us)",
                        self.ev_name(),
                        t.elapsed().as_micros()
                    ),
                    Err(e) => {
                        self.ev_errors += 1;
                        self.ev_file = None;
                        self.ev_retry_at = now + EV_RETRY_MS;
                        println!(
                            "hub-p4: evidence folder/file failed: {e:?}; records stay queued, NOT written (screen says so); retry in {} s",
                            EV_RETRY_MS / 1000
                        );
                    }
                }
                if let Err(e) = self.open_pack(faa, now) {
                    println!("hub-p4: sd airspace pack: {e:?}");
                    self.pack_file = None;
                }
                self.load_o4_key();
                self.load_flarm_keys();
                self.load_site();
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
            #[cfg(feature = "benchlog")]
            {
                let (c, lines) = bench_count(vm, f)?;
                c.print(prev.as_str(), lines);
            }
            vm.close_file(f)?;
        }
        println!("hub-p4: sd {count} numbered log files in {DIR}");
        self.name.clear();
        // GPS time: MMDDhhmm.CSV, unless that name is taken (two boots in
        // one minute): then the numbered name.
        if let Some(c) = self.utc_ms.map(crate::civil::civil) {
            let _ = write!(self.name, "{:02}{:02}{:02}{:02}.CSV", c.month, c.day, c.hour, c.minute);
            if vm.find_directory_entry(dir, self.name.as_str()).is_ok() {
                self.name.clear();
            }
        }
        if self.name.is_empty() {
            let _ = write!(self.name, "LOG{:05}.CSV", (max + 1) % 100_000);
        }
        let file = vm.open_file_in_dir(dir, self.name.as_str(), Mode::ReadWriteCreateOrAppend)?;
        self.pos = vm.file_length(file)?;
        self.lines = 0;
        if self.pos == 0 {
            // The tracker's header plus this hub's flag columns.
            let head = csv::HEADER.trim_end_matches('\n');
            let suffix = faarules::text::CSV_HEADER_SUFFIX;
            vm.write(file, head.as_bytes())?;
            vm.write(file, suffix.as_bytes())?;
            vm.write(file, b"\n")?;
            self.pos = (head.len() + suffix.len() + 1) as u32;
            self.lines = 1;
        }
        vm.flush_file(file)?;
        self.open = Some(Open { vol, root, dir, file });
        // Rows kept while waiting for GPS time stay queued.
        self.dirty = false;
        Ok(())
    }

    /// Open `ROTORSCP/EVIDENCE` (creating it if missing), read the end of
    /// the highest-numbered file, create the next one. Never opens an
    /// existing evidence file for writing.
    ///
    /// The card is not trusted (`evidence::files`): only regular files
    /// named `EVnnnnnn.EVL` with a number in range count, a name taken by a
    /// folder is never chosen, and when the top number is taken the lowest
    /// free one is used instead of failing every mount. A previous file that cannot be read is noted, not fatal. On
    /// an error the folder handle is closed again, so the retry (every
    /// [`EV_RETRY_MS`] while the card records) cannot run out of handles.
    fn mount_evidence(&mut self, ev: &mut Evidence, now: u64) -> Result<(), FsError> {
        let vm = self.vm.as_ref().unwrap();
        let Some(parent) = self.open.as_ref().map(|o| o.dir) else { return Ok(()) };
        let dir = match vm.open_dir(parent, EV_DIR) {
            Ok(d) => d,
            Err(embedded_sdmmc::Error::NotFound) => {
                println!("hub-p4: sd creating folder {DIR}/{EV_DIR}");
                vm.make_dir_in_dir(parent, EV_DIR)?;
                vm.open_dir(parent, EV_DIR)?
            }
            Err(e) => return Err(e),
        };
        match Self::new_evidence_file(vm, dir) {
            Ok((file, name, no, prev)) => {
                ev.card_mounted(name.as_str(), no, prev);
                self.ev_file = Some(EvFile { dir, file, name, pos: 0, dirty: false, flushed_at: now });
                Ok(())
            }
            Err(e) => {
                let _ = vm.close_dir(dir);
                Err(e)
            }
        }
    }

    /// Choose and create the next evidence file in `dir`; the previous
    /// file's link (number, length, last record hash, status) if it has one.
    #[allow(clippy::type_complexity)]
    fn new_evidence_file(vm: &Vm, dir: RawDirectory) -> Result<(RawFile, heapless::String<12>, u32, Option<(u32, u32, [u8; 32], u8)>), FsError> {
        use evidence::files::{self, Next};
        let next = files::choose_next(|f: &mut dyn FnMut(u32, bool)| {
            vm.iterate_dir(dir, |e| {
                if let Some(n) = files::number(e.name.base_name(), e.name.extension()) {
                    f(n, !e.attributes.is_directory() && !e.attributes.is_volume());
                }
                ControlFlow::Continue(())
            })
        })?;
        let Some(next) = next else {
            println!("hub-p4: evidence: no free file number in {DIR}/{EV_DIR}");
            return Err(embedded_sdmmc::Error::FormatError("no free evidence file number"));
        };
        let mut prev = None;
        match next {
            Next::After { prev: 0, .. } => {}
            Next::After { prev: p, .. } => {
                let pname = files::name(p).unwrap_or(*b"EV000000.EVL");
                let pname = core::str::from_utf8(&pname).unwrap_or("?");
                match Self::previous_tail(vm, dir, pname) {
                    Ok((len, hash, status)) => {
                        println!(
                            "hub-p4: evidence previous file {DIR}/{EV_DIR}/{pname}: {len} bytes, last record {:02x}{:02x}{:02x}{:02x}.. ({})",
                            hash[0], hash[1], hash[2], hash[3],
                            match status { 1 => "ends on a whole record", 2 => "ends in an incomplete record", _ => "no whole record in its last 2 KB" }
                        );
                        prev = Some((p, len, hash, status));
                    }
                    Err(e) => {
                        // Linked as "unreadable" (status 3) rather than
                        // stopping the evidence log over it.
                        println!("hub-p4: evidence previous file {DIR}/{EV_DIR}/{pname} unreadable: {e:?}");
                        prev = Some((p, 0, [0; 32], 3));
                    }
                }
            }
            Next::Lowest { no } => println!(
                "hub-p4: evidence: no free number above the highest file in {DIR}/{EV_DIR} (EV999999.EVL taken); using the lowest free number {no}, with no previous file to link"
            ),
        }
        let no = next.no();
        let raw = files::name(no).unwrap_or(*b"EV000000.EVL");
        let mut name: heapless::String<12> = heapless::String::new();
        let _ = name.push_str(core::str::from_utf8(&raw).unwrap_or("?"));
        // ReadWriteCreate refuses a name that exists: never rewrites a file.
        let file = vm.open_file_in_dir(dir, name.as_str(), Mode::ReadWriteCreate)?;
        Ok((file, name, no, prev))
    }

    /// Length, hash of the last whole record, and status (1 whole, 2 ends
    /// in an incomplete record, 3 no whole record) of the evidence file
    /// `name`. Closes it again whatever happens.
    fn previous_tail(vm: &Vm, dir: RawDirectory, name: &str) -> Result<(u32, [u8; 32], u8), FsError> {
        let f = vm.open_file_in_dir(dir, name, Mode::ReadOnly)?;
        let r = (|| {
            let len = vm.file_length(f)?;
            let n = (len as usize).min(EV_TAIL);
            let mut tail = [0u8; EV_TAIL];
            vm.file_seek_from_start(f, len - n as u32)?;
            let mut got = 0;
            while got < n {
                let r = vm.read(f, &mut tail[got..n])?;
                if r == 0 {
                    break;
                }
                got += r;
            }
            let (hash, status) = match evidence::format::last_complete(&tail[..got]) {
                Some((at, partial)) => {
                    let l = evidence::format::peek_len(&tail[at..]).unwrap_or(0);
                    (evidence::sha256(&tail[at..at + l]), if partial { 2 } else { 1 })
                }
                None => ([0; 32], 3),
            };
            Ok((len, hash, status))
        })();
        let _ = vm.close_file(f);
        r
    }

    /// One evidence step, if one is due: a chunk, a flush, or (console `r`)
    /// a replay read. Evidence goes before the CSV.
    fn ev_step(&mut self, ev: &mut Evidence, now: u64) -> Result<Option<&'static str>, FsError> {
        let (Some(vm), Some(f)) = (self.vm.as_ref(), self.ev_file.as_mut()) else { return Ok(None) };
        let un = ev.unwritten();
        let room = 512 - (f.pos % 512) as usize;
        let flush_due = now.saturating_sub(f.flushed_at) >= FLUSH_EVERY_MS;
        if un.len() >= room || (flush_due && !un.is_empty()) {
            let n = room.min(un.len());
            vm.write(f.file, &un[..n])?;
            ev.wrote(n);
            f.pos += n as u32;
            f.dirty = true;
            self.last_io = now;
            return Ok(Some("ev_write"));
        }
        if flush_due {
            f.flushed_at = now;
            if f.dirty {
                // Everything queued is written, so the file ends on a record.
                vm.flush_file(f.file)?;
                f.dirty = false;
                ev.flushed();
                self.ev_flushes += 1;
                self.last_io = now;
                return Ok(Some("ev_flush"));
            }
        }
        let Some(rp) = self.replay.as_mut() else { return Ok(None) };
        // One record line needs ~1.4 KB of console room.
        if crate::console::room() < 4096 {
            return Ok(None);
        }
        // Send whole records already read.
        while let Ok(len) = evidence::format::peek_len(&rp.buf) {
            if rp.buf.len() < len {
                break;
            }
            if !crate::evlog::stream_record(&rp.buf[..len]) {
                return Ok(Some("ev_replay"));
            }
            rp.sent += 1;
            rp.buf.copy_within(len.., 0);
            rp.buf.truncate(rp.buf.len() - len);
        }
        if rp.pos >= f.pos || (rp.buf.len() >= 8 && evidence::format::peek_len(&rp.buf).is_err()) {
            println!("hub-p4: evidence replay of {} done: {} records, {} bytes", f.name, rp.sent, rp.pos);
            self.replay = None;
            return Ok(Some("ev_replay"));
        }
        let mut b = [0u8; 512];
        let want = (512 - (rp.pos % 512) as usize).min((f.pos - rp.pos) as usize).min(rp.buf.capacity() - rp.buf.len());
        vm.file_seek_from_start(f.file, rp.pos)?;
        let n = vm.read(f.file, &mut b[..want])?;
        vm.file_seek_from_end(f.file, 0)?;
        let _ = rp.buf.extend_from_slice(&b[..n]);
        rp.pos += n as u32;
        Ok(Some("ev_replay"))
    }

    /// After mount: find the airspace pack and start copying it (or, in an
    /// `embedpack` build, start writing the built-in one first).
    fn open_pack(&mut self, faa: &mut Faa, now: u64) -> Result<(), FsError> {
        if faa.pack().is_some() {
            return Ok(());
        }
        let vm = self.vm.as_ref().unwrap();
        let Some(dir) = self.open.as_ref().map(|o| o.dir) else {
            return Ok(());
        };
        let found = match vm.open_file_in_dir(dir, PACK_NAME, Mode::ReadOnly) {
            Ok(f) => Some(f),
            Err(embedded_sdmmc::Error::NotFound) => None,
            Err(e) => return Err(e),
        };
        #[cfg(feature = "embedpack")]
        {
            let same = match found {
                Some(f) => {
                    let mut head = [0u8; 64];
                    let n = vm.read(f, &mut head)?;
                    let len = vm.file_length(f)?;
                    vm.close_file(f)?;
                    n == 64 && head[..] == EMBEDDED[..64] && len as usize == EMBEDDED.len()
                }
                None => false,
            };
            if !same {
                println!(
                    "hub-p4: sd writing the built-in airspace pack to {DIR}/{PACK_NAME} ({} bytes)",
                    EMBEDDED.len()
                );
                let f = vm.open_file_in_dir(dir, PACK_NAME, Mode::ReadWriteCreateOrTruncate)?;
                self.pack_file = Some(f);
                self.pack_writing = Some(0);
                faa.set_writing(0, EMBEDDED.len() as u32);
                return Ok(());
            }
            println!("hub-p4: sd {DIR}/{PACK_NAME} already matches the built-in pack");
            let f = vm.open_file_in_dir(dir, PACK_NAME, Mode::ReadOnly)?;
            return self.start_load(f, faa, now);
        }
        #[cfg(not(feature = "embedpack"))]
        match found {
            Some(f) => self.start_load(f, faa, now),
            None => {
                faa.missing();
                Ok(())
            }
        }
    }

    fn start_load(&mut self, f: RawFile, faa: &mut Faa, now: u64) -> Result<(), FsError> {
        let vm = self.vm.as_ref().unwrap();
        let len = vm.file_length(f)?;
        if faa.begin(len, now) {
            self.pack_file = Some(f);
        } else {
            vm.close_file(f)?;
        }
        Ok(())
    }

    /// One step of the pack copy (or write). Returns what it did.
    fn pack_step(&mut self, faa: &mut Faa, now: u64) -> Result<&'static str, FsError> {
        let (Some(vm), Some(f)) = (self.vm.as_ref(), self.pack_file) else {
            return Ok("idle");
        };
        #[cfg(feature = "embedpack")]
        if let Some(done) = self.pack_writing {
            let total = EMBEDDED.len();
            let n = 512.min(total - done as usize);
            vm.write(f, &EMBEDDED[done as usize..done as usize + n])?;
            let done = done + n as u32;
            faa.set_writing(done, total as u32);
            if done as usize >= total {
                vm.close_file(f)?;
                self.pack_writing = None;
                self.pack_file = None;
                println!(
                    "hub-p4: sd wrote {DIR}/{PACK_NAME} ({total} bytes); reading it back into PSRAM"
                );
                let dir = self.open.as_ref().map(|o| o.dir).unwrap();
                let f = vm.open_file_in_dir(dir, PACK_NAME, Mode::ReadOnly)?;
                self.start_load(f, faa, now)?;
            } else {
                self.pack_writing = Some(done);
            }
            return Ok("pack_write");
        }
        let Some(buf) = faa.chunk(PACK_READ) else {
            vm.close_file(f)?;
            self.pack_file = None;
            return Ok("idle");
        };
        let mut got = 0;
        while got < buf.len() {
            let n = vm.read(f, &mut buf[got..])?;
            if n == 0 {
                break;
            }
            got += n;
        }
        if got == 0 {
            // Shorter than its length said: let the CRC check refuse it.
            println!("hub-p4: sd airspace pack ended early");
            vm.close_file(f)?;
            self.pack_file = None;
            faa.card_gone();
            return Ok("pack_read");
        }
        if faa.advance(got, now) {
            vm.close_file(f)?;
            self.pack_file = None;
        }
        Ok("pack_read")
    }

    fn record(&mut self, tracker: &HubTracker, faa: &mut Faa, ev: &mut Evidence, now: u64) -> &'static str {
        if now.saturating_sub(self.last_rows) >= ROW_EVERY_MS {
            self.last_rows = now;
            self.queue_rows(tracker, faa, now);
        }
        if self.vm.as_ref().is_some_and(|vm| vm.device(|d| d.card.borrow_mut().is_busy())) {
            self.busy_skips += 1;
            return "busy";
        }
        // Recording without an evidence file (its folder or file failed at
        // mount): try again now and then rather than leave it stopped.
        if self.ev_file.is_none() && now >= self.ev_retry_at {
            self.ev_retry_at = now + EV_RETRY_MS;
            return match self.mount_evidence(ev, now) {
                Ok(()) => {
                    println!("hub-p4: evidence recording to {DIR}/{EV_DIR}/{} (retry)", self.ev_name());
                    "ev_mount"
                }
                Err(e) => {
                    self.ev_errors += 1;
                    println!("hub-p4: evidence folder/file failed again: {e:?}; {} records waiting; retry in {} s", ev.queued_records(), EV_RETRY_MS / 1000);
                    "ev_mount_err"
                }
            };
        }
        // Evidence first: its queue only shrinks once flushed.
        let t = Instant::now();
        let r = self.ev_step(ev, now);
        let us = t.elapsed().as_micros();
        match r {
            Ok(Some(w)) => {
                if us > self.max_ev_us {
                    self.max_ev_us = us;
                    self.max_ev_what = w;
                }
                return w;
            }
            Ok(None) => {}
            Err(e) => {
                self.ev_errors += 1;
                return self.lost(ev, &e, now);
            }
        }
        if self.pack_file.is_some() && self.pending.len() < PENDING / 2 {
            if self
                .vm
                .as_ref()
                .is_some_and(|vm| vm.device(|d| d.card.borrow_mut().is_busy()))
            {
                self.busy_skips += 1;
                return "busy";
            }
            match self.pack_step(faa, now) {
                Ok(w) => return w,
                Err(e) => {
                    println!("hub-p4: sd airspace pack read failed: {e:?}");
                    self.pack_file = None;
                    faa.card_gone();
                    return "pack_err";
                }
            }
        }

        let (Some(vm), Some(open)) = (self.vm.as_ref(), self.open.as_ref()) else { return "idle" };
        let file = open.file;
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
            return self.lost(ev, &e, now);
        }
        what
    }

    /// A write failed while recording: the card is taken to be gone.
    fn lost(&mut self, ev: &mut Evidence, e: &FsError, now: u64) -> &'static str {
        let rows_lost = self.pending.len();
        if self.ev_file.is_some() {
            ev.card_lost();
        }
        self.fail("write", e, now);
        println!("hub-p4: sd lost the card while recording ({rows_lost} CSV bytes unwritten; evidence stays queued); probing again");
        self.state = State::NoCard(now + PROBE_EVERY_MS);
        "write_err"
    }

    fn queue_rows(&mut self, tracker: &HubTracker, faa: &Faa, now: u64) {
        let ctx = tracker.context(now, self.utc_ms);
        let mut row = [0u8; ROW_BUF];
        let pack = faa.pack();
        for t in tracker.tracks().iter().filter(|t| ctx.state(t) == TrackState::Active) {
            let entry = faa.engine.drone(t.id);
            let r = csv::row(t, &ctx, &mut row).map(|n| with_flags(&mut row, n, entry, pack));
            // Show the first row with flags once, as proof of the columns.
            if let Ok(n) = r
                && entry.is_some_and(|e| e.level() >= Some(1))
                && !self.shown_flag_row
            {
                self.shown_flag_row = true;
                println!("hub-p4: sd first flagged row: {}", core::str::from_utf8(&row[..n]).unwrap_or("?").trim_end());
            }
            self.push_row(&row, r);
        }
        for l in tracker.open_links().iter().filter(|l| ctx.link_state(l) == TrackState::Active) {
            let r = csv::link_row(l, &ctx, &mut row).map(|n| with_flags(&mut row, n, None, pack));
            self.push_row(&row, r);
        }
        for e in tracker.emitters().iter().filter(|e| ctx.emitter_state(e) == TrackState::Active) {
            let r = csv::rf_row(e, &ctx, &mut row).map(|n| with_flags(&mut row, n, None, pack));
            self.push_row(&row, r);
        }
        for w in tracker.wifi_drones().iter().filter(|w| ctx.wifi_drone_state(w) == TrackState::Active) {
            let r = csv::wifi_drone_row(w, &ctx, &mut row).map(|n| with_flags(&mut row, n, None, pack));
            self.push_row(&row, r);
        }
        for a in tracker.aircraft().iter().filter(|a| ctx.aircraft_state(a) == TrackState::Active) {
            let r = csv::aircraft_row(a, &ctx, &mut row).map(|n| with_flags(&mut row, n, faa.engine.aircraft(a.key.icao), pack));
            self.push_row(&row, r);
        }
    }

    fn push_row<E>(&mut self, row: &[u8; ROW_BUF], r: Result<usize, E>) {
        let Ok(n) = r else { return };
        if self.pending.extend_from_slice(&row[..n]).is_ok() {
            self.rows += 1;
        } else {
            self.dropped += 1;
        }
    }
}

/// A row plus the flag columns: up to [`MAX_ROW`] of tracker columns and
/// the rest for `flag_level,flags`.
const ROW_BUF: usize = MAX_ROW + 768;

/// Replace the row's newline with the two flag columns (empty for rows with
/// no flags), cut to fit, and end the row again. Returns the new length.
fn with_flags(
    row: &mut [u8; ROW_BUF],
    n: usize,
    entry: Option<&faarules::Entry>,
    pack: Option<&airspace::Pack<'_>>,
) -> usize {
    struct Cut<'a> {
        buf: &'a mut [u8],
        len: usize,
    }
    impl core::fmt::Write for Cut<'_> {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            // Keep one byte for the newline; cut on a char boundary.
            let room = self.buf.len() - 1 - self.len;
            let mut take = s.len().min(room);
            while !s.is_char_boundary(take) {
                take -= 1;
            }
            self.buf[self.len..self.len + take].copy_from_slice(&s.as_bytes()[..take]);
            self.len += take;
            Ok(())
        }
    }
    let start = if n > 0 && row[n - 1] == b'\n' {
        n - 1
    } else {
        n
    };
    let mut w = Cut {
        buf: &mut row[..],
        len: start,
    };
    let _ = faarules::text::write_csv_columns(&mut w, entry, pack);
    let end = w.len;
    row[end] = b'\n';
    end + 1
}

/// Bench only: read the whole file and count its rows by kind (blocks the
/// loop for the length of the file, once, at mount).
#[cfg(feature = "benchlog")]
fn bench_count(vm: &Vm, file: RawFile) -> Result<(crate::bench::CsvCount, u32), FsError> {
    vm.file_seek_from_start(file, 0)?;
    let mut c = crate::bench::CsvCount::default();
    let mut buf = [0u8; 512];
    let mut line: heapless::Vec<u8, 600> = heapless::Vec::new();
    let mut lines = 0u32;
    loop {
        // A long file takes many seconds bit-banged: keep the watchdog fed.
        crate::fault::feed();
        let n = vm.read(file, &mut buf)?;
        if n == 0 {
            break;
        }
        for &b in &buf[..n] {
            if b == b'\n' {
                lines += 1;
                c.line(&line);
                line.clear();
            } else if line.push(b).is_err() {
                c.bad += 1;
                line.clear();
            }
        }
        if vm.file_eof(file)? {
            break;
        }
    }
    Ok((c, lines))
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
