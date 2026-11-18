//! The evidence log on the P4 (PLAN.md Phase 13, `crates/evidence`,
//! docs/EVIDENCE.md): every data frame the hub ingests and every event that
//! could matter later, hash-chained, with a signed checkpoint every minute.
//!
//! **Records wait in PSRAM.** [`Evidence`] builds each record at once (so
//! its time and position are those of the moment it arrived) into a queue
//! of [`QUEUE`] bytes in PSRAM (about 9 minutes of one detection a second).
//! The SD logger (`sd.rs`) appends the queue to `ROTORSCP/EVIDENCE/EVnnnnnn.EVL`
//! and only drops records from the queue once the file has been flushed
//! (its directory entry updated) with them inside. A card pulled or a power
//! cut therefore never loses a record that left the queue: whatever was not
//! flushed is written again, whole, to the next file.
//!
//! **Nothing is ever dropped after it is chained.** Room is checked before a
//! record is built; a frame that finds the queue full is counted, and a
//! `not_logged` event with the count goes in as soon as there is room.
//!
//! **Console stream.** Each record is also printed on the USB console as
//! `@EV1 <base64>` (a laptop uploader collects them; see
//! tools/evidence-server). A line that does not fit the console buffer is
//! dropped whole and counted; the card copy is complete regardless, and the
//! console command `r` replays the current card file.
//!
//! **Key.** A development key built into the firmware image
//! (`ROTORSCOPE_DEV_KEY`, 64 hex digits, at build time), or the published
//! test key when unset. A set but malformed value stops the build
//! (build.rs). Every checkpoint says which, the hello line says which, and
//! the screen keeps saying so ("TEST KEY: log not evidence" / "DEV KEY: not
//! evidence-grade"). Not evidence-grade: see docs/EVIDENCE.md for the
//! hardware key design. Nothing here touches eFuses. Key bytes are never
//! printed or logged; only the public key and its fingerprint are.
//!
//! **Bench builds.** The boot event's text lists the bench features
//! compiled in (`bench: none` for a normal build), so a log made by a build
//! that can fake data says so in its first record.

use core::fmt::Write as _;

use esp_hal::time::Instant;
use evidence::format::{self, CheckpointReason, EventCode, MAX_RECORD};
use evidence::sign::{KeyClass, SoftSigner};
use evidence::writer::{Chain, Context, Detection};
use evidence::{PosSource, Signer, TimeSource};

/// Queue bytes (in PSRAM).
pub const QUEUE: usize = 256 * 1024;
/// Queue location inside PSRAM: past the framebuffer (768 KB), below the
/// airspace pack (8 MB, `faa::PACK_OFFSET`).
pub const QUEUE_OFFSET: usize = 4 << 20;
/// Fallback queue in internal RAM when PSRAM failed.
const RAM_QUEUE: usize = 8 * 1024;
static mut RAM_Q: [u8; RAM_QUEUE] = [0; RAM_QUEUE];

/// A signed checkpoint this often (uptime).
pub const CHECKPOINT_EVERY_MS: u64 = 60_000;
/// UTC older than this since the last GPS time counts as free-running.
const FRESH_TIME_MS: u64 = 2_500;
/// A node silent this long has left.
const NODE_GONE_MS: u64 = 30_000;
/// Faster than this between two fixes is a jump (possible spoofing).
const JUMP_MPS: f64 = 150.0;
/// A clock correction smaller than this is not logged.
const CLOCK_STEP_MS: i64 = 500;

/// The dev key, if the build has one (`ROTORSCOPE_DEV_KEY`; build.rs has
/// checked it is 64 hex digits and not zero).
const DEV_KEY_HEX: Option<&str> = option_env!("ROTORSCOPE_DEV_KEY");

/// The key class this build means to sign with, for the hello line (before
/// the signer exists). The signer's own class is what the screen shows.
pub fn built_key_class() -> KeyClass {
    if DEV_KEY_HEX.is_some() { KeyClass::DevelopmentSoftware } else { KeyClass::PublicTest }
}

/// Bench features compiled in (build.rs), comma separated, empty for a
/// normal build.
pub const BENCH: &str = env!("ROTORSCOPE_BENCH_LIST");

fn dev_key() -> Option<[u8; 32]> {
    let h = DEV_KEY_HEX?.as_bytes();
    if h.len() != 64 {
        return None;
    }
    let mut k = [0u8; 32];
    for (i, c) in h.chunks(2).enumerate() {
        let d = |x: u8| (x as char).to_digit(16);
        k[i] = (d(c[0])? * 16 + d(c[1])?) as u8;
    }
    Some(k)
}

/// SHA-256 of the firmware ELF file, as espflash wrote it into the app
/// descriptor when flashing. Zero if flashed some other way.
pub fn firmware_hash() -> [u8; 32] {
    // SAFETY: the descriptor is a static in flash; read through a volatile
    // pointer because espflash rewrites it after linking (the compiled-in
    // value is all zero and could otherwise be folded).
    let d = unsafe { core::ptr::read_volatile(&raw const crate::ESP_APP_DESC) };
    *d.app_elf_sha256()
}

#[derive(Clone, Copy, Default)]
struct NodeSeen {
    mac: [u8; 6],
    alive: bool,
}

pub struct Evidence {
    chain: Chain,
    signer: SoftSigner,
    q: &'static mut [u8],
    len: usize,
    /// Bytes at the front of the queue written to the current card file
    /// since its last flush.
    written: usize,
    pub ctx: Context,
    not_logged: u32,
    pub not_logged_total: u32,
    last_checkpoint: u64,
    /// Console stream on (command `u`).
    pub stream: bool,
    pub stream_drops: u32,
    nodes: heapless::Vec<NodeSeen, 16>,
    gps_present: bool,
    fix: bool,
    last_fix: Option<(f64, f64, u64)>,
    anchor: Option<crate::gps::TimeAnchor>,
    pack_logged: bool,
    /// Last GPS integrity state logged (see `watch_integrity`).
    integrity: gnss::IntegrityState,
    // Figures for the 5 s console line and the docs.
    pub records: u32,
    pub bytes: u32,
    pub checkpoints: u32,
    pub max_build_us: u64,
    pub max_sign_us: u64,
    pub last_sign_us: u64,
    pub drained: u32,
    /// Records in `q` (see `queued_records`).
    queued_records: u32,
}

impl Evidence {
    /// `psram`: the PSRAM base, if PSRAM works.
    pub fn new(psram: Option<(*mut u32, usize)>, boot_id: [u8; 8]) -> Self {
        let q: &'static mut [u8] = match psram {
            Some((base, size)) if size >= QUEUE_OFFSET + QUEUE => {
                // SAFETY: [QUEUE_OFFSET, +QUEUE) is inside PSRAM and used by
                // nothing else (framebuffer below 768 KB, pack from 8 MB).
                unsafe { core::slice::from_raw_parts_mut(base.cast::<u8>().add(QUEUE_OFFSET), QUEUE) }
            }
            // SAFETY: taken once, here.
            _ => unsafe { &mut *(&raw mut RAM_Q) },
        };
        let signer = match dev_key().and_then(|k| SoftSigner::new(&k, KeyClass::DevelopmentSoftware)) {
            Some(s) => s,
            None => {
                if DEV_KEY_HEX.is_some() {
                    // Well-formed digits (build.rs) but not a usable P-256
                    // scalar: say so loudly; the screen shows TEST KEY.
                    println!("hub-p4: evidence dev key REFUSED (not a valid P-256 private key): signing with the PUBLISHED TEST KEY");
                }
                SoftSigner::public_test()
            }
        };
        let pk = signer.public_key();
        let fp = evidence::fingerprint(&pk);
        println!(
            "hub-p4: evidence key {} fingerprint {} ({}); queue {} KB in {}",
            signer.key_class().name(),
            core::str::from_utf8(&fp).unwrap_or("?"),
            signer.key_class().note(),
            q.len() / 1024,
            if q.len() == QUEUE { "PSRAM" } else { "RAM" }
        );
        let mut hexpk: heapless::String<130> = heapless::String::new();
        for b in pk {
            let _ = write!(hexpk, "{b:02x}");
        }
        println!("hub-p4: evidence public key {hexpk}");
        let fw = firmware_hash();
        Self {
            chain: Chain::new(boot_id, fw),
            signer,
            q,
            len: 0,
            written: 0,
            ctx: Context::default(),
            not_logged: 0,
            not_logged_total: 0,
            last_checkpoint: 0,
            stream: true,
            stream_drops: 0,
            nodes: heapless::Vec::new(),
            gps_present: false,
            fix: false,
            last_fix: None,
            anchor: None,
            pack_logged: false,
            integrity: gnss::IntegrityState::Ok,
            records: 0,
            bytes: 0,
            checkpoints: 0,
            max_build_us: 0,
            max_sign_us: 0,
            last_sign_us: 0,
            drained: 0,
            queued_records: 0,
        }
    }

    /// The class of the key actually signing (for the screen).
    pub fn key_class(&self) -> KeyClass {
        self.signer.key_class()
    }

    /// Records in the queue not yet flushed to a card file.
    pub fn queued_records(&self) -> u32 {
        self.queued_records
    }

    pub fn fw_hash(&self) -> [u8; 32] {
        firmware_hash()
    }

    pub fn seq(&self) -> u64 {
        self.chain.seq()
    }

    pub fn queued(&self) -> usize {
        self.len
    }

    /// Store a built record (`n` bytes of `buf`), chain it, stream it.
    fn keep(&mut self, buf: &[u8], n: usize) {
        self.q[self.len..self.len + n].copy_from_slice(&buf[..n]);
        self.len += n;
        self.queued_records += 1;
        self.chain.commit();
        self.records += 1;
        self.bytes += n as u32;
        if self.stream && !stream_record(&buf[..n]) {
            self.stream_drops += 1;
        }
    }

    fn room(&self) -> usize {
        self.q.len() - self.len
    }

    /// The first record: who signs, which firmware.
    /// `reset` is why the chip last reset (`fault::reset_reason`).
    pub fn boot(&mut self, chip: (u8, u8), reset: &str) {
        let mut data = [0u8; 1 + format::PUBKEY_LEN + 2];
        data[0] = self.signer.key_class() as u8;
        data[1..66].copy_from_slice(&self.signer.public_key());
        data[66] = chip.0;
        data[67] = chip.1;
        // Version (~6), git (~40), reset reason (~10) and the bench list
        // (every hub-p4 bench feature at once is ~110): 256 holds them all.
        let mut text: heapless::String<256> = heapless::String::new();
        let _ = write!(
            text,
            "hub-p4 {} {} last reset: {reset}; bench: {}",
            env!("CARGO_PKG_VERSION"),
            option_env!("ROTORSCOPE_GIT").unwrap_or(""),
            if BENCH.is_empty() { "none" } else { BENCH }
        );
        self.event(EventCode::Boot, text.as_bytes(), &data);
    }

    /// An event record. Events are small; they still need room.
    pub fn event(&mut self, code: EventCode, text: &[u8], data: &[u8]) {
        if self.room() < MAX_RECORD {
            self.not_logged += 1;
            self.not_logged_total += 1;
            return;
        }
        let mut buf = [0u8; MAX_RECORD];
        if let Ok(n) = self.chain.event(&self.ctx, code, text, data, &mut buf) {
            self.keep(&buf, n);
        }
    }

    /// A data frame from node `index` (0 = wired), relayed from `node`.
    pub fn frame(&mut self, index: u8, node: Option<([u8; 6], i8)>, f: &link::Frame) {
        let Some(info) = evidence::summary::info(f) else { return };
        if self.room() < MAX_RECORD {
            self.not_logged += 1;
            self.not_logged_total += 1;
            return;
        }
        let t0 = Instant::now();
        let mut raw = [0u8; link::MAX_FRAME];
        let Some(raw) = evidence::summary::raw_bytes(f, &mut raw) else { return };
        let mut s = evidence::summary::Summary::new();
        let _ = evidence::summary::write_summary(f, &mut s);
        let d = Detection {
            node: node.map_or([0; 6], |n| n.0),
            link_rssi: node.map(|n| n.1),
            node_index: index,
            band: info.band,
            frame_kind: info.frame_kind,
            channel: info.channel,
            freq_100khz: info.freq_100khz,
            rssi: info.rssi,
            raw_format: format::RAW_LINK_FRAME_V6,
            raw,
            summary: s.as_bytes(),
        };
        let mut buf = [0u8; MAX_RECORD];
        if let Ok(n) = self.chain.detection(&self.ctx, &d, &mut buf) {
            self.keep(&buf, n);
        }
        self.max_build_us = self.max_build_us.max(t0.elapsed().as_micros());
    }

    pub fn checkpoint(&mut self, reason: CheckpointReason, now: u64) {
        self.last_checkpoint = now;
        if self.room() < MAX_RECORD {
            self.not_logged += 1;
            self.not_logged_total += 1;
            return;
        }
        let t0 = Instant::now();
        let mut buf = [0u8; MAX_RECORD];
        match self.chain.checkpoint(&self.ctx, reason, &mut self.signer, &mut buf) {
            Ok(n) => {
                let us = t0.elapsed().as_micros();
                self.last_sign_us = us;
                self.max_sign_us = self.max_sign_us.max(us);
                self.checkpoints += 1;
                self.keep(&buf, n);
            }
            Err(e) => println!("hub-p4: evidence checkpoint failed: {e:?}"),
        }
    }

    /// Every main-loop pass: the minute checkpoint, the not-logged count.
    pub fn tick(&mut self, now: u64) {
        if self.not_logged > 0 && self.room() >= 2 * MAX_RECORD {
            let n = self.not_logged;
            self.not_logged = 0;
            let mut t: heapless::String<64> = heapless::String::new();
            let _ = write!(t, "{n} frames not logged (evidence queue full)");
            self.event(EventCode::NotLogged, t.as_bytes(), &n.to_le_bytes());
        }
        if now.saturating_sub(self.last_checkpoint) >= CHECKPOINT_EVERY_MS {
            self.checkpoint(CheckpointReason::Periodic, now);
        }
    }

    /// Once a second: GPS, clock, nodes and pack changes become events.
    pub fn watch(&mut self, gps: &crate::gps::Gps, nodes: &[crate::links::NodeStats], pack: Option<&airspace::Pack<'_>>, now: u64) {
        use tracker::json::GpsState;
        self.watch_integrity(gps);
        let present = gps.status.state != GpsState::Absent;
        if present != self.gps_present {
            self.gps_present = present;
            let t: &[u8] = if present { b"GPS receiver answering" } else { b"GPS receiver silent" };
            self.event(EventCode::GpsReceiver, t, &[present as u8]);
        }
        let fix = gps.fresh_fix(now, 5_000);
        if fix.is_some() != self.fix {
            self.fix = fix.is_some();
            match fix {
                Some(f) => {
                    let mut t: heapless::String<64> = heapless::String::new();
                    let _ = write!(t, "fix {:.6},{:.6} sats {:?}", f.lat, f.lon, gps.status.sats);
                    self.event(EventCode::GpsFix, t.as_bytes(), &[]);
                }
                None => self.event(EventCode::GpsLost, b"no fresh GPS fix", &[]),
            }
        }
        if let Some(f) = fix {
            if let Some((lat, lon, at)) = self.last_fix
                && f.at_ms > at
            {
                let d = metres(lat, lon, f.lat, f.lon);
                let dt = (f.at_ms - at) as f64 / 1000.0;
                if d > 50.0 && d / dt.max(1.0) > JUMP_MPS {
                    let mut t: heapless::String<80> = heapless::String::new();
                    let _ = write!(t, "position jumped {d:.0} m in {dt:.1} s (possible spoofing or a bad fix)");
                    let mut data = [0u8; 8];
                    data[..4].copy_from_slice(&(d as u32).to_le_bytes());
                    data[4..].copy_from_slice(&(dt as u32).to_le_bytes());
                    self.event(EventCode::GpsJump, t.as_bytes(), &data);
                }
            }
            self.last_fix = Some((f.lat, f.lon, f.at_ms));
        }
        if let Some(a) = gps.time {
            let changed = self.anchor.is_none_or(|o| o.at_ms != a.at_ms || o.pps != a.pps);
            if changed {
                let old = self.anchor.map(|o| o.utc_ms.saturating_add(a.at_ms.saturating_sub(o.at_ms)));
                let step = old.map_or(0, |o| a.utc_ms as i64 - o as i64);
                let pps_change = self.anchor.is_some_and(|o| o.pps != a.pps);
                if old.is_none() || step.abs() >= CLOCK_STEP_MS || pps_change {
                    let mut t: heapless::String<96> = heapless::String::new();
                    let _ = match old {
                        None => write!(t, "UTC set from GPS{}", if a.pps { " (PPS-locked)" } else { "" }),
                        Some(_) => write!(t, "UTC corrected by {step} ms; PPS {}", if a.pps { "locked" } else { "not locked" }),
                    };
                    let mut data = [0u8; 25];
                    data[..8].copy_from_slice(&old.unwrap_or(0).to_le_bytes());
                    data[8..16].copy_from_slice(&a.utc_ms.to_le_bytes());
                    data[16..24].copy_from_slice(&step.to_le_bytes());
                    data[24] = a.pps as u8;
                    self.event(EventCode::Clock, t.as_bytes(), &data);
                }
                self.anchor = Some(a);
            }
        }
        for n in nodes {
            let alive = now.saturating_sub(n.last_ms) < NODE_GONE_MS;
            let i = match self.nodes.iter().position(|s| s.mac == n.mac) {
                Some(i) => i,
                None => {
                    // Full: a node already logged as gone that the link's
                    // table has since forgotten (`links::EVICT_AFTER_MS`)
                    // makes room.
                    if self.nodes.is_full()
                        && let Some(j) = self.nodes.iter().position(|s| !s.alive && !nodes.iter().any(|m| m.mac == s.mac))
                    {
                        self.nodes.swap_remove(j);
                    }
                    if self.nodes.push(NodeSeen { mac: n.mac, alive: false }).is_err() {
                        continue;
                    }
                    self.nodes.len() - 1
                }
            };
            if self.nodes[i].alive != alive {
                self.nodes[i].alive = alive;
                let m = n.mac;
                let mut t: heapless::String<64> = heapless::String::new();
                let _ = write!(
                    t,
                    "node {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} {} (role {:?})",
                    m[0], m[1], m[2], m[3], m[4], m[5],
                    if alive { "joined" } else { "silent 30 s" },
                    n.role
                );
                self.event(if alive { EventCode::NodeJoin } else { EventCode::NodeLeave }, t.as_bytes(), &m);
            }
        }
        if !self.pack_logged
            && let Some(p) = pack
        {
            self.pack_logged = true;
            let mut t: heapless::String<64> = heapless::String::new();
            let _ = write!(t, "airspace pack v{} built {} ({} bytes)", p.version, p.created, p.len());
            let mut data = [0u8; 10];
            data[..4].copy_from_slice(&p.created.to_le_bytes());
            data[4..8].copy_from_slice(&(p.len() as u32).to_le_bytes());
            data[8..].copy_from_slice(&p.version.to_le_bytes());
            self.event(EventCode::Pack, t.as_bytes(), &data);
        }
    }

    // ---------------------------------------------- GPS integrity hook --

    /// Log a change of the GPS integrity verdict (docs/GNSS-INTEGRITY.md):
    /// entering or changing a jamming state is `GnssJamming` (data: kind u8,
    /// level i16 LE), a spoofing state `GnssSpoofing` (data: kind u8, level
    /// i32 LE); kind and level are `gnss::integrity::Cause::kind` and the
    /// finding's value in `Cause::unit`. Back to normal is the event of the
    /// category left, with kind 0 and level 0. Text: the plain-English
    /// reason. The monitor's hysteresis keeps this to state changes, never
    /// once a second.
    fn watch_integrity(&mut self, gps: &crate::gps::Gps) {
        let st = *gps.integrity();
        if st.state == self.integrity {
            return;
        }
        let was = self.integrity;
        self.integrity = st.state;
        let mut t: heapless::String<200> = heapless::String::new();
        let _ = st.write_reason(&mut t);
        if st.state == gnss::IntegrityState::Ok {
            let _ = write!(t, " (was {})", was.text());
        }
        let (kind, level) = st.cause.map_or((0, 0), |f| (f.cause.kind(), f.value));
        let spoofing = st.state.is_spoofing() || (st.state == gnss::IntegrityState::Ok && was.is_spoofing());
        if spoofing {
            let mut d = [0u8; 5];
            d[0] = kind;
            d[1..].copy_from_slice(&level.to_le_bytes());
            self.event(EventCode::GnssSpoofing, t.as_bytes(), &d);
        } else {
            let mut d = [0u8; 3];
            d[0] = kind;
            d[1..].copy_from_slice(&(level.clamp(i16::MIN.into(), i16::MAX.into()) as i16).to_le_bytes());
            self.event(EventCode::GnssJamming, t.as_bytes(), &d);
        }
    }

    // ------------------------------------------------ card side (sd.rs) --

    /// Queued bytes not yet written to the current file.
    pub fn unwritten(&self) -> &[u8] {
        &self.q[self.written..self.len]
    }

    pub fn wrote(&mut self, n: usize) {
        self.written += n;
    }

    /// The file was flushed with everything written so far inside: those
    /// records leave the queue.
    pub fn flushed(&mut self) {
        let w = self.written;
        // Whole records only are flushed (sd.rs flushes at a record boundary).
        let mut at = 0;
        while at < w {
            match format::peek_len(&self.q[at..w]) {
                Ok(l) if l > 0 => {
                    at += l;
                    self.queued_records = self.queued_records.saturating_sub(1);
                }
                _ => break,
            }
        }
        self.q.copy_within(w..self.len, 0);
        self.len -= w;
        self.written = 0;
        self.drained += w as u32;
    }

    /// The card is gone: whatever was not flushed goes to the next file.
    pub fn card_lost(&mut self) {
        let unflushed = self.len as u32;
        self.written = 0;
        let mut t: heapless::String<64> = heapless::String::new();
        let _ = write!(t, "card stopped answering; {unflushed} queued bytes go to the next file");
        self.event(EventCode::CardLost, t.as_bytes(), &unflushed.to_le_bytes());
    }

    /// A new evidence file `file_no` was opened.
    pub fn card_mounted(&mut self, name: &str, file_no: u32, prev: Option<(u32, u32, [u8; 32], u8)>) {
        self.written = 0;
        let (pn, plen, ph, ps) = prev.unwrap_or((0, 0, [0; 32], 0));
        let mut data = [0u8; 45];
        data[..4].copy_from_slice(&file_no.to_le_bytes());
        data[4..8].copy_from_slice(&pn.to_le_bytes());
        data[8..12].copy_from_slice(&plen.to_le_bytes());
        data[12..44].copy_from_slice(&ph);
        data[44] = ps;
        self.event(EventCode::CardMounted, name.as_bytes(), &data);
    }

    /// Where the hub is and what time it is, for every record.
    pub fn set_context(&mut self, app: &crate::app::App, gps: &crate::gps::Gps, now: u64) {
        use tracker::json::GpsState;
        let mut c = Context { uptime_ms: now, ..Context::default() };
        if let Some(a) = gps.time {
            let age = now.saturating_sub(a.at_ms);
            c.utc_ms = gps.utc_ms(now);
            c.time_age_ms = Some(age.min(u64::from(u32::MAX - 1)) as u32);
            c.time_source = if age > FRESH_TIME_MS {
                TimeSource::FreeRunning
            } else if a.pps {
                TimeSource::GpsPps
            } else {
                TimeSource::Gps
            };
        } else if gps.untrusted_time {
            // GPS gives a time the hub withholds (WP-12): say so, UTC 0.
            c.time_source = TimeSource::Untrusted;
        }
        c.gps_state = match gps.status.state {
            GpsState::Absent => 0,
            GpsState::Searching => 1,
            GpsState::Fix2d => 2,
            GpsState::Fix3d => 3,
        };
        c.sats = gps.status.sats;
        if let Some(p) = app.own_pos(gps, now) {
            c.pos_source = match p.source {
                crate::app::OwnSource::Gps => PosSource::Gps,
                crate::app::OwnSource::Manual => PosSource::Manual,
            };
            c.lat_e7 = (p.lat * 1e7) as i32;
            c.lon_e7 = (p.lon * 1e7) as i32;
            c.alt_hae_mm = p.alt.map(|a| (a * 1000.0) as i32);
            if p.source == crate::app::OwnSource::Gps {
                c.fix_age_ms = Some(now.saturating_sub(p.at_ms).min(u64::from(u32::MAX - 1)) as u32);
                if let Some(f) = gps.fix {
                    c.hacc_dm = f.hacc_m.map(|m| (m * 10.0).min(65_534.0) as u16);
                    c.hdop_x100 = f.hdop.map(|h| (h * 100.0).min(65_534.0) as u16);
                }
            }
        }
        self.ctx = c;
    }
}

/// Print one record as `@EV1 <base64>`; false if the console had no room.
pub fn stream_record(rec: &[u8]) -> bool {
    let mut line = [0u8; 6 + evidence::b64::encoded_len(MAX_RECORD) + 1];
    line[..5].copy_from_slice(b"@EV1 ");
    let Some(n) = evidence::b64::encode(rec, &mut line[5..]) else { return false };
    line[5 + n] = b'\n';
    crate::console::json(&line[..6 + n])
}

fn metres(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let dlat = (lat2 - lat1) * 111_195.0;
    let dlon = (lon2 - lon1) * 111_195.0 * libm_cos(lat1.to_radians());
    libm_sqrt(dlat * dlat + dlon * dlon)
}

fn libm_cos(x: f64) -> f64 {
    // Enough for distances on the order of a km (small-angle Taylor is
    // not enough at 40 degrees, so a few more terms).
    let x2 = x * x;
    1.0 - x2 / 2.0 + x2 * x2 / 24.0 - x2 * x2 * x2 / 720.0 + x2 * x2 * x2 * x2 / 40_320.0
}

fn libm_sqrt(v: f64) -> f64 {
    if v <= 0.0 {
        return 0.0;
    }
    let mut x = v.max(1.0);
    for _ in 0..30 {
        x = 0.5 * (x + v / x);
    }
    x
}
