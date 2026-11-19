//! Possible FAA rule violations on the P4 (PLAN.md Phase 12).
//!
//! - **Pack.** `ROTORSCP/AIRSPACE.PAK` on the SD card (written by
//!   `tools/airspace-pack`) is copied into PSRAM at [`PACK_OFFSET`] by the SD
//!   logger (`sd.rs`), a block at a time, and checked (CRC) once. A missing
//!   or damaged pack turns off the checks that need it (airspace, facility
//!   map, terrain height, line of sight, 91.119); the night check and the
//!   Remote ID checks still run, and a height reported "above ground" by the
//!   drone is still checked. Once loaded, the pack stays in PSRAM even if
//!   the card is pulled.
//! - **Engine.** `faarules::Engine` runs once a second over the tracker.
//!   It has one entry for every drone and aircraft the tracker can hold
//!   ([`ENTRIES`], WP-09: with fewer, a flood of forged tracks used to
//!   restart real drones' timers), ~84 KB, so it lives in PSRAM at
//!   [`ENGINE_OFFSET`].
//!   Its update time and the slowest one are printed every 5 s
//!   (`hub-p4: faa ...`), and `b` on the console times single lookups.
//! - **Outputs.** New or raised flags (Watch and up) are logged as full
//!   sentences; the screen's FLAGS page, the SD CSV columns `flag_level` and
//!   `flags`, and `{"t":"flags"}` JSON lines come from [`Faa`].
//!
//! Every message says "possible violation", never "illegal".

use core::fmt::Write as _;

use airspace::{AreaRef, Pack, PackError};
use esp_hal::time::Instant;
use faarules::{Engine, Entry, Key};

use crate::app::HubTracker;

/// Pack location inside PSRAM (the framebuffer takes the first 768 KB).
pub const PACK_OFFSET: usize = 8 << 20;
/// Largest pack accepted.
pub const PACK_MAX: usize = 8 << 20;
/// Tracks evaluated at once (drones with a fresh position, flagged
/// aircraft): every drone and aircraft the tracker can hold, so no entry is
/// ever dropped for another (WP-09).
pub const ENTRIES: usize = crate::app::MAX_TRACKS + crate::app::MAX_AIRCRAFT;
/// Where the engine lives in PSRAM: past the evidence queue (4 MB + 256 KB),
/// below the aero store (6 MB).
pub const ENGINE_OFFSET: usize = 5 << 20;
const _: () = assert!(crate::evlog::QUEUE_OFFSET + crate::evlog::QUEUE <= ENGINE_OFFSET);
const _: () = assert!(
    ENGINE_OFFSET + core::mem::size_of::<Engine<ENTRIES>>() <= crate::aero::STORE_OFFSET,
    "FAA engine overlaps the aero store"
);
/// Areas the in-RAM bounding-box index holds (a 100 km Philadelphia pack
/// has 236). A pack with more still works, by scanning (slower).
const INDEX_MAX: usize = 384;

/// The pack's area index, in internal RAM: a lookup then reads PSRAM only
/// for the few areas whose box holds the point (PSRAM reads are slow).
static mut INDEX: [AreaRef; INDEX_MAX] = [AreaRef {
    off: 0,
    lat_min: 0,
    lat_max: 0,
    lon_min: 0,
    lon_max: 0,
    kind: 0,
}; INDEX_MAX];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackState {
    /// PSRAM failed: no room for a pack.
    NoPsram,
    /// No card (yet).
    NoCard,
    /// Card mounted, no `AIRSPACE.PAK`.
    #[cfg_attr(feature = "embedpack", allow(dead_code))]
    Missing,
    /// `embedpack` builds: writing the built-in pack to the card.
    #[cfg_attr(not(feature = "embedpack"), allow(dead_code))]
    Writing {
        done: u32,
        total: u32,
    },
    Loading {
        got: u32,
        total: u32,
    },
    Loaded,
    TooBig(u32),
    Bad(PackError),
}

pub struct Faa {
    /// In PSRAM at [`ENGINE_OFFSET`].
    pub engine: &'static mut Engine<ENTRIES>,
    base: *mut u8,
    pub state: PackState,
    len: usize,
    pack: Option<Pack<'static>>,
    load_started_ms: u64,
    pub load_ms: u64,
    pub crc_ms: u64,
    /// SHA-256 of the pack as it is copied in, and once loaded (for the
    /// evidence log's `pack` event: which airspace data the hub used).
    sha: evidence::Sha256Stream,
    sha256: Option<[u8; 32]>,
    pub update_us_last: u64,
    pub update_us_max: u64,
    evals_prev: u32,
    /// Highest level already logged per flag, so each flag is printed once
    /// per level it reaches.
    printed: heapless::Vec<(Key, faarules::Rule, Option<airspace::AreaKind>, u8), 64>,
    last_print: u64,
}

impl Faa {
    pub fn new(psram: Option<(*mut u32, usize)>) -> Self {
        let Some((pbase, psize)) = psram else {
            // The hub cannot run without PSRAM (the tracker lives there too).
            panic!("hub-p4: no PSRAM: the FAA engine lives there; cannot start");
        };
        assert!(psize >= ENGINE_OFFSET + core::mem::size_of::<Engine<ENTRIES>>(), "PSRAM too small for the FAA engine");
        // SAFETY: [ENGINE_OFFSET, + size_of) is inside PSRAM (checked
        // above) and used by nothing else (the asserts at ENGINE_OFFSET:
        // after the evidence queue, before the aero store); 5 MB is aligned
        // for any type. Called once, from main.
        let engine = unsafe {
            let p = pbase.cast::<u8>().add(ENGINE_OFFSET).cast::<Engine<ENTRIES>>();
            p.write(Engine::new(faarules::Settings::default()));
            &mut *p
        };
        let (base, state) = match psram {
            // SAFETY: PACK_OFFSET + PACK_MAX is inside the 32 MB PSRAM; only
            // this module uses that range.
            Some((b, size)) if size >= PACK_OFFSET + PACK_MAX => (
                unsafe { b.cast::<u8>().add(PACK_OFFSET) },
                PackState::NoCard,
            ),
            _ => (core::ptr::null_mut(), PackState::NoPsram),
        };
        Self {
            engine,
            base,
            state,
            len: 0,
            pack: None,
            load_started_ms: 0,
            load_ms: 0,
            crc_ms: 0,
            sha: evidence::Sha256Stream::new(),
            sha256: None,
            update_us_last: 0,
            update_us_max: 0,
            evals_prev: 0,
            printed: heapless::Vec::new(),
            last_print: 0,
        }
    }

    pub fn pack(&self) -> Option<&Pack<'static>> {
        self.pack.as_ref()
    }

    /// SHA-256 of the loaded pack's bytes.
    pub fn pack_sha256(&self) -> Option<[u8; 32]> {
        self.sha256.filter(|_| self.pack.is_some())
    }

    /// The SD logger found a pack of `total` bytes: start copying it.
    /// False if it cannot be taken (no PSRAM, too big, already loaded).
    pub fn begin(&mut self, total: u32, now: u64) -> bool {
        if self.pack.is_some() || self.base.is_null() {
            return false;
        }
        if total as usize > PACK_MAX {
            self.state = PackState::TooBig(total);
            println!(
                "hub-p4: faa pack is {total} bytes, more than the {PACK_MAX} reserved; not loaded"
            );
            return false;
        }
        self.state = PackState::Loading { got: 0, total };
        self.sha = evidence::Sha256Stream::new();
        self.sha256 = None;
        self.load_started_ms = now;
        println!("hub-p4: faa loading airspace pack ({total} bytes) from the SD card");
        true
    }

    /// Where the next `n` bytes of the pack go.
    pub fn chunk(&mut self, n: usize) -> Option<&mut [u8]> {
        let PackState::Loading { got, total } = self.state else {
            return None;
        };
        let n = n.min((total - got) as usize);
        // SAFETY: inside the reserved PSRAM range (total <= PACK_MAX); no
        // `Pack` borrows it while loading.
        Some(unsafe { core::slice::from_raw_parts_mut(self.base.add(got as usize), n) })
    }

    /// `n` more bytes arrived. True when the whole pack is in.
    pub fn advance(&mut self, n: usize, now: u64) -> bool {
        let PackState::Loading { got, total } = self.state else {
            return false;
        };
        // SAFETY: the `n` bytes just copied in at `got` (inside the
        // reserved range, see `chunk`); nothing writes them meanwhile.
        self.sha.update(unsafe { core::slice::from_raw_parts(self.base.add(got as usize), n) });
        let got = got + n as u32;
        self.state = PackState::Loading { got, total };
        if got < total {
            return false;
        }
        self.finish(total as usize, now);
        true
    }

    fn finish(&mut self, len: usize, now: u64) {
        self.load_ms = now.saturating_sub(self.load_started_ms);
        self.sha256 = Some(core::mem::take(&mut self.sha).finish());
        // SAFETY: the bytes just written; from here on only read.
        let data: &'static [u8] = unsafe { core::slice::from_raw_parts(self.base, len) };
        let t = Instant::now();
        let r = Pack::parse(data);
        self.crc_ms = t.elapsed().as_millis();
        match r {
            Ok(p) => {
                // SAFETY: written only here, once per loaded pack, while no
                // `Pack` holds the old index (self.pack is None).
                let idx = unsafe { &mut *(&raw mut INDEX) };
                let p = match p.build_index(idx) {
                    Some(n) => {
                        println!("hub-p4: faa area index: {n} areas in RAM");
                        // SAFETY: not written again while this pack lives.
                        p.with_index(unsafe {
                            core::slice::from_raw_parts((&raw const INDEX).cast::<AreaRef>(), n)
                        })
                    }
                    None => {
                        println!(
                            "hub-p4: faa more than {INDEX_MAX} areas: lookups scan the pack (slower)"
                        );
                        p
                    }
                };
                self.len = len;
                self.state = PackState::Loaded;
                let (y, m, d) = faarules::text::civil_date(p.created);
                println!(
                    "hub-p4: faa airspace pack loaded: {len} bytes in {} ms (check {} ms), built {y:04}-{m:02}-{d:02}, centre {:.3},{:.3} radius {} km, {} areas",
                    self.load_ms,
                    self.crc_ms,
                    p.center_lat,
                    p.center_lon,
                    p.radius_m / 1000,
                    p.areas().count()
                );
                for s in p.sources() {
                    let (y, m, d) = faarules::text::civil_date(s.date);
                    println!(
                        "hub-p4: faa   source {:>2} {} {y:04}-{m:02}-{d:02}",
                        s.id, s.name
                    );
                }
                self.pack = Some(p);
            }
            Err(e) => {
                println!("hub-p4: faa airspace pack refused: {e:?}; airspace/terrain checks off");
                self.state = PackState::Bad(e);
            }
        }
    }

    /// The card has no pack.
    #[cfg_attr(feature = "embedpack", allow(dead_code))]
    pub fn missing(&mut self) {
        if self.pack.is_none()
            && !matches!(
                self.state,
                PackState::NoPsram | PackState::Bad(_) | PackState::TooBig(_)
            )
        {
            if self.state != PackState::Missing {
                println!(
                    "hub-p4: faa no ROTORSCP/AIRSPACE.PAK on the card: airspace/terrain checks off (night and Remote ID checks still on)"
                );
            }
            self.state = PackState::Missing;
        }
    }

    /// The card went away mid-load (a loaded pack is kept).
    pub fn card_gone(&mut self) {
        if matches!(
            self.state,
            PackState::Loading { .. } | PackState::Writing { .. }
        ) {
            self.state = PackState::NoCard;
        }
    }

    #[cfg_attr(not(feature = "embedpack"), allow(dead_code))]
    pub fn set_writing(&mut self, done: u32, total: u32) {
        if self.pack.is_none() {
            self.state = PackState::Writing { done, total };
        }
    }

    /// Run the rules once (call at 1 Hz).
    pub fn tick(&mut self, tracker: &HubTracker, now: u64, utc: Option<u64>) {
        let t = Instant::now();
        self.engine.update(tracker, self.pack.as_ref(), now, utc);
        let us = t.elapsed().as_micros();
        self.update_us_last = us;
        self.update_us_max = self.update_us_max.max(us);
        self.log_new(tracker);
        // A flagged flight ended (the tracker dropped its track): one
        // summary line, as a log line and a {"t":"flight"} JSON line.
        while let Some((key, sm)) = self.engine.take_finished() {
            let mut j: heapless::String<512> = heapless::String::new();
            let _ = match key {
                Key::Drone(id) => write!(j, "{{\"t\":\"flight\",\"track\":{},", id.0),
                Key::Aircraft(icao) => write!(j, "{{\"t\":\"flight\",\"icao\":\"{icao:06x}\",", ),
            };
            let _ = faarules::text::write_summary_json(&mut j, &sm);
            let _ = j.push_str("}\n");
            println!("hub-p4: faa flight summary {}", j.as_str().trim_end());
            crate::console::json(j.as_bytes());
        }
        if now.saturating_sub(self.last_print) >= 5_000 {
            self.last_print = now;
            let flagged = self
                .engine
                .entries()
                .iter()
                .filter(|e| !e.flags.is_empty())
                .count();
            let worst = self.engine.entries().iter().filter_map(Entry::level).max();
            let evals = self.engine.evals;
            println!(
                "hub-p4: faa pack={} entries={} flagged={} worst={:?} evals={} (+{}) update last={}us max={}us",
                self.pack_text().as_str(),
                self.engine.entries().len(),
                flagged,
                worst,
                evals,
                evals - self.evals_prev,
                us,
                self.update_us_max
            );
            self.evals_prev = evals;
            self.update_us_max = 0;
        }
    }

    /// Log each flag of level 1+ when it first reaches a level.
    fn log_new(&mut self, tracker: &HubTracker) {
        let engine = &self.engine;
        self.printed.retain(|(k, r, a, _)| {
            engine
                .get(*k)
                .is_some_and(|e| e.flags.iter().any(|f| f.rule == *r && f.area == *a))
        });
        for e in self.engine.entries() {
            for f in e.flags.iter().filter(|f| f.level >= 1) {
                let i = match self
                    .printed
                    .iter()
                    .position(|(k, r, a, _)| *k == e.key && *r == f.rule && *a == f.area)
                {
                    Some(i) => i,
                    None => {
                        if self.printed.push((e.key, f.rule, f.area, 0)).is_err() {
                            continue;
                        }
                        self.printed.len() - 1
                    }
                };
                if f.level <= self.printed[i].3 {
                    continue;
                }
                self.printed[i].3 = f.level;
                let who = who(tracker, e.key);
                let mut s: heapless::String<400> = heapless::String::new();
                let _ = faarules::text::write_flag(&mut s, f, self.pack.as_ref());
                println!("hub-p4: faa {}: {}", who.as_str(), s.as_str());
            }
        }
    }

    /// Once a second: one `Frame::Flags` per flagged track down the web-head
    /// wire (WEBHEAD.md), worst items first.
    pub fn forward(&self, tracker: &HubTracker, feed: &mut crate::webhead::Feed) {
        let pack_date = self.pack.map_or(0, |p| p.created);
        for e in self.engine.entries().iter().filter(|e| !e.flags.is_empty()) {
            let mut who: heapless::String<24> = heapless::String::new();
            match e.key {
                Key::Aircraft(icao) => {
                    let _ = write!(who, "ICAO-{icao:06x}");
                }
                Key::Drone(_) => who = self::who(tracker, e.key),
            }
            let mut flags: heapless::Vec<&faarules::Flag, 8> = e.flags.iter().collect();
            flags.sort_unstable_by(|a, b| b.level.cmp(&a.level));
            let items = flags
                .iter()
                .take(link::MAX_FLAG_ITEMS)
                .map(|f| link::FlagItem {
                    rule: f.rule as u8,
                    area: f.area.map_or(0, |a| a as u8),
                    level: f.level,
                    value: faarules::text::item_value(f),
                    secs: f.secs.min(65_535.0) as u16,
                })
                .collect();
            let report = link::FlagReport { who, level: e.level().unwrap_or(0), items, pack_date };
            feed.fwd.send(&link::Frame::Flags(report));
        }
    }

    /// "pack 2026-11-15", "loading 40%", "no airspace pack", ...
    pub fn pack_text(&self) -> heapless::String<32> {
        let mut s = heapless::String::new();
        let _ = match self.state {
            PackState::Loaded => {
                let (y, m, d) = faarules::text::civil_date(self.pack.map_or(0, |p| p.created));
                write!(s, "pack {y:04}-{m:02}-{d:02}")
            }
            PackState::Loading { got, total } => write!(
                s,
                "pack loading {}%",
                u64::from(got) * 100 / u64::from(total.max(1))
            ),
            PackState::Writing { done, total } => write!(
                s,
                "pack writing {}%",
                u64::from(done) * 100 / u64::from(total.max(1))
            ),
            PackState::Missing => write!(s, "no airspace pack"),
            PackState::NoCard => write!(s, "pack: no card"),
            PackState::NoPsram => write!(s, "pack: no PSRAM"),
            PackState::TooBig(_) => write!(s, "pack too big"),
            PackState::Bad(_) => write!(s, "pack damaged"),
        };
        s
    }

    /// The screen's FLAGS page.
    pub fn view(&self, tracker: &HubTracker) -> screen::FlagsView {
        let mut v = screen::FlagsView {
            pack: self.pack_text(),
            ..Default::default()
        };
        let mut all: heapless::Vec<(u8, Key, &faarules::Flag), 64> = heapless::Vec::new();
        for e in self.engine.entries() {
            for f in &e.flags {
                let _ = all.push((f.level, e.key, f));
            }
        }
        v.total = all.len() as u16;
        all.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(b.2.secs.total_cmp(&a.2.secs)));
        for (level, key, f) in all.iter().take(screen::MAX_FLAG_ROWS) {
            let w = who(tracker, *key);
            let mut who14: heapless::String<14> = heapless::String::new();
            for c in w.chars() {
                if who14.push(c).is_err() {
                    break;
                }
            }
            let _ = v.rows.push(screen::FlagRow {
                who: who14,
                level: *level,
                rule: f.rule.code(),
                text: faarules::text::screen_text(f),
                secs: f.secs.min(65_535.0) as u16,
            });
        }
        v
    }

    /// Console `b`: time the lookups one evaluation makes, at three spots.
    pub fn bench(&self) {
        let Some(p) = self.pack.as_ref() else {
            println!("hub-p4: faa bench: no pack loaded");
            return;
        };
        let spots = [
            (39.872, -75.241, "PHL"),
            (40.082, -75.011, "PNE"),
            (39.9503, -75.1496, "Independence Mall"),
        ];
        for (lat, lon, name) in spots {
            let n = 50;
            let t = Instant::now();
            let mut areas = 0;
            for _ in 0..n {
                areas = p.areas_at(lat, lon).count();
            }
            let a_us = t.elapsed().as_micros() / n;
            let t = Instant::now();
            let mut g = None;
            for _ in 0..n {
                g = p.ground_hae_m(lat, lon);
            }
            let g_us = t.elapsed().as_micros() / n;
            let t = Instant::now();
            let mut c = None;
            for _ in 0..n {
                c = p.uasfm_ceiling_ft(lat, lon);
            }
            let c_us = t.elapsed().as_micros() / n;
            let t = Instant::now();
            let los = p.line_of_sight((lat, lon, 20.0), (lat + 0.01, lon, 120.0));
            let l_us = t.elapsed().as_micros();
            let t = Instant::now();
            let el = airspace::sun::elevation_deg(1_794_700_000_000, lat, lon);
            let s_us = t.elapsed().as_micros();
            println!(
                "hub-p4: faa bench {name}: areas_at {a_us} us ({areas} areas), ground {g_us} us ({g:?} m HAE), facility map {c_us} us ({c:?} ft), 1.1 km line of sight {l_us} us ({:?} m), sun {s_us} us ({el:.1} deg)",
                los.map(|l| l.worst_m)
            );
        }
    }
}

/// A short name for a flagged track: its UAS ID, else the transmitter MAC;
/// an aircraft's callsign, else its ICAO address.
pub fn who(tracker: &HubTracker, key: Key) -> heapless::String<24> {
    let mut s = heapless::String::new();
    match key {
        Key::Drone(id) => match tracker.get(id) {
            Some(t) => match t.primary_id().and_then(|u| u.id.as_str()) {
                Some(txt) => {
                    for c in txt.trim_end_matches('\0').chars() {
                        if s.push(c).is_err() {
                            break;
                        }
                    }
                }
                None => {
                    let mac = t.latest_slot().map(|(_, l)| l.mac).unwrap_or_default();
                    let _ = write!(
                        s,
                        "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
                        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
                    );
                }
            },
            None => {
                let _ = write!(s, "track {}", id.0);
            }
        },
        Key::Aircraft(icao) => match tracker.aircraft().iter().find(|a| a.key.icao == icao) {
            Some(a) if !a.callsign.is_empty() => {
                let _ = write!(s, "{}", a.callsign.as_str());
            }
            _ => {
                let _ = write!(s, "{icao:06x}");
            }
        },
    }
    s
}
