//! Aircraft from aero (LR2021) nodes (docs/AERO-NODE.md "Hub side").
//!
//! An aero node sends each aircraft radio frame raw (`link::Frame::Air`).
//! Here the hub decodes it with the same decoders the laptop tools use and
//! hands the aircraft to the tracker and the web head exactly as ADSBee
//! aircraft are handed over:
//!
//! - **1090 ES** (`Es1090`, 14-byte DF17): into a `crates/modes` table. With
//!   the `modes` feature that is the ADSBee's own table (`adsbee::Raw`), so
//!   both receivers build one picture; without it, a table of the aero
//!   nodes' own, reported once a second like the ADSBee's. Either way the
//!   table is [`Store::table`] in PSRAM.
//! - **UAT 978** (`Uat978`): `crates/uat` (Reed-Solomon correction first).
//! - **OGN** (`Ogn`): `crates/flarm` module `ogn`.
//! - **FLARM** (`Flarm`): `crates/flarm` module `legacy`, only with the key
//!   file (`ROTORSCP/FLARMKEY.TXT`, `sd.rs`), GPS-derived UTC (a wrong time
//!   gives a wrong key and FLARM v7 has no check of its own) and an own
//!   position (GPS or manual: FLARM sends only the low digits of its
//!   position). Missing any of them, the frame is counted (`no_keys`,
//!   `no_time`, `no_pos`) and not decoded. The raw frame is in the
//!   evidence log either way.
//!
//! Every decoded aircraft is treated the same, including FLARM "no-track"
//! and "stealth" ones: logged, shown and relayed (owner decision
//! 2026-11-17: "everything should be logged"). The radio's packet RSSI is
//! the record's `signal_dbm`.
//!
//! The FLARM key words are never printed, logged or sent anywhere; only
//! the state ("none" / "loaded" / "bad" / "error") is reported.

use link::{AeroMode, AeroStatus, AeroTime, AirFrame, AirProto};

use crate::app::{HubTracker, MAX_AIRCRAFT};
use crate::webhead::Feed;

/// The 1090 ES table aero frames go into.
pub type EsTable = modes::Table<MAX_AIRCRAFT>;

/// Aircraft frames waiting for the main loop (`links.rs` fills it while the
/// screen is drawn; a node may send up to 200 a second). In PSRAM.
pub type AirQueue = heapless::Deque<(u8, AirFrame), 64>;

/// Aero nodes whose latest `AeroStatus` is kept.
pub const MAX_AERO_NODES: usize = 4;

/// Console summary this often (when anything was counted).
const PRINT_MS: u64 = 60_000;
/// A node's own UTC stamp on a frame is used for the FLARM key only when it
/// agrees with the hub's GPS time this closely (else the hub's time is).
const NODE_UTC_TOLERANCE_MS: u64 = 2_000;

/// Where [`Store`] lives in PSRAM: past the evidence queue (4 MB + 256 KB),
/// below the airspace pack (8 MB, `faa::PACK_OFFSET`).
pub const STORE_OFFSET: usize = 6 << 20;

/// The aero state too big for internal RAM.
pub struct Store {
    pub queue: AirQueue,
    /// The 1090 table: with `modes` the ADSBee's (`adsbee::Raw::table`),
    /// which aero nodes' 1090 frames join; without, theirs alone.
    pub table: EsTable,
    /// Protocol 12: the same nodes' drone control links and control-band
    /// energy (`tracker::ctrllinks`).
    pub control: tracker::ctrllinks::ControlTable,
}

const _: () = assert!(STORE_OFFSET + core::mem::size_of::<Store>() <= crate::faa::PACK_OFFSET);
const _: () = assert!(crate::evlog::QUEUE_OFFSET + crate::evlog::QUEUE <= STORE_OFFSET);

/// Set up [`Store`] in PSRAM, field by field (never a whole `Store` on the
/// stack). `None` without PSRAM: aero frames then share the link's small
/// queue and, without `modes`, 1090 frames are counted but not decoded.
#[inline(never)]
pub fn store(psram: Option<(*mut u32, usize)>) -> Option<&'static mut Store> {
    let (base, size) = psram?;
    if size < STORE_OFFSET + core::mem::size_of::<Store>() {
        return None;
    }
    // SAFETY: [STORE_OFFSET, + size_of::<Store>()) is inside PSRAM (checked
    // above) and used by nothing else (see the asserts); 6 MB is aligned
    // for any type; every field is written before the reference is made.
    unsafe {
        let p = base.cast::<u8>().add(STORE_OFFSET).cast::<Store>();
        (&raw mut (*p).queue).write(heapless::Deque::new());
        EsTable::init_in_place(&raw mut (*p).table, modes::Config::default());
        (&raw mut (*p).control).write(tracker::ctrllinks::ControlTable::new());
        Some(&mut *p)
    }
}

/// One protocol's counters. `rejected`: the decoder refused the frame (bad
/// parity / CRC / Reed-Solomon, a packet type that carries no position, or
/// for 1090 no table to put it in).
#[derive(Clone, Copy, Default)]
pub struct Counts {
    pub frames: u32,
    pub decoded: u32,
    pub rejected: u32,
}

impl Counts {
    fn any(&self) -> bool {
        self.frames != 0
    }
}

/// FLARM's extra reasons not to decode.
#[derive(Clone, Copy, Default)]
pub struct FlarmSkips {
    /// No key file loaded.
    pub no_keys: u32,
    /// No GPS-derived UTC on the hub.
    pub no_time: u32,
    /// No own position (GPS or manual).
    pub no_pos: u32,
}

/// One aero node's latest report.
pub struct NodeAero {
    pub mac: [u8; 6],
    pub status: AeroStatus,
    pub at_ms: u64,
}

/// What decoding needs from the rest of the hub at this moment.
pub struct Ctx<'a> {
    /// UTC from the GPS (held through a spoofing alarm, never typed in).
    pub gps_utc_ms: Option<u64>,
    /// The 1090 table: the ADSBee's (`modes`) or [`Store::table`].
    pub es: Option<&'a mut EsTable>,
}

#[derive(Default)]
pub struct Aero {
    pub es: Counts,
    pub uat: Counts,
    pub ogn: Counts,
    pub flarm: Counts,
    pub flarm_skips: FlarmSkips,
    keys: Option<flarm::Keys>,
    pub nodes: heapless::Vec<NodeAero, MAX_AERO_NODES>,
    last_print_ms: u64,
    #[cfg(not(feature = "modes"))]
    last_report_ms: u64,
}

impl Aero {
    /// The FLARM key words from the card. Kept here only; never printed.
    pub fn set_keys(&mut self, k: flarm::Keys) {
        self.keys = Some(k);
    }

    pub fn has_keys(&self) -> bool {
        self.keys.is_some()
    }

    /// One `Frame::Air`: decode, and hand any aircraft on. Out of line:
    /// its buffers stay off the main loop's frame.
    #[inline(never)]
    pub fn on_air(&mut self, f: &AirFrame, ctx: &mut Ctx<'_>, tracker: &mut HubTracker, feed: Option<&mut Feed>, now: u64) {
        let rssi = i16::from(f.rssi);
        match f.proto {
            AirProto::Es1090 => {
                self.es.frames += 1;
                let ok = ctx.es.as_deref_mut().zip(modes::Frame::new(&f.raw)).is_some_and(|(t, frame)| {
                    let rx = modes::Received {
                        frame,
                        signal_dbm: Some(rssi),
                        ticks_12mhz: None,
                    };
                    t.ingest(&rx, now).is_ok()
                });
                // The table reports its aircraft once a second (`report`,
                // or the ADSBee's `report_raw` with `modes`).
                if ok {
                    self.es.decoded += 1;
                } else {
                    self.es.rejected += 1;
                }
            }
            AirProto::Uat978 => {
                self.uat.frames += 1;
                let mut buf = [0u8; link::MAX_AIR_RAW];
                let n = f.raw.len();
                buf[..n].copy_from_slice(&f.raw);
                match uat::decode(&mut buf[..n]) {
                    Ok(d) => {
                        self.uat.decoded += 1;
                        deliver(d.aircraft(), rssi, tracker, feed, now);
                    }
                    Err(_) => self.uat.rejected += 1,
                }
            }
            AirProto::Ogn => {
                self.ogn.frames += 1;
                match flarm::ogn::decode(&f.raw) {
                    Ok(d) => {
                        self.ogn.decoded += 1;
                        deliver(d.aircraft, rssi, tracker, feed, now);
                    }
                    Err(_) => self.ogn.rejected += 1,
                }
            }
            AirProto::Flarm => {
                self.flarm.frames += 1;
                let Some(keys) = self.keys.as_ref() else {
                    self.flarm_skips.no_keys += 1;
                    return;
                };
                let Some(hub_utc) = ctx.gps_utc_ms else {
                    self.flarm_skips.no_time += 1;
                    return;
                };
                let Some(own) = tracker.own_position(now) else {
                    self.flarm_skips.no_pos += 1;
                    return;
                };
                // The node's reception stamp is the better time (the frame
                // may have waited in queues), when it agrees with ours.
                let utc = f.utc_ms.filter(|t| t.abs_diff(hub_utc) <= NODE_UTC_TOLERANCE_MS).unwrap_or(hub_utc);
                let rx = flarm::legacy::Receiver {
                    unix_time: (utc / 1000) as u32,
                    lat_deg: own.lat,
                    lon_deg: own.lon,
                };
                match flarm::legacy::decode(&f.raw, keys, &rx) {
                    Ok(d) => {
                        self.flarm.decoded += 1;
                        deliver(d.aircraft, rssi, tracker, feed, now);
                    }
                    Err(_) => self.flarm.rejected += 1,
                }
            }
        }
    }

    /// Keep a node's latest `AeroStatus`.
    #[inline(never)]
    pub fn on_status(&mut self, mac: [u8; 6], st: &AeroStatus, now: u64) {
        if let Some(n) = self.nodes.iter_mut().find(|n| n.mac == mac) {
            n.status = st.clone();
            n.at_ms = now;
            return;
        }
        if self.nodes.is_full() {
            // The one heard from longest ago makes room.
            if let Some(i) = (0..self.nodes.len()).min_by_key(|&i| self.nodes[i].at_ms) {
                self.nodes.swap_remove(i);
            }
        }
        let _ = self.nodes.push(NodeAero {
            mac,
            status: st.clone(),
            at_ms: now,
        });
    }

    pub fn status_of(&self, mac: &[u8; 6]) -> Option<&AeroStatus> {
        self.nodes.iter().find(|n| &n.mac == mac).map(|n| &n.status)
    }

    /// Once a second, without `modes`: the aero nodes' own 1090 table to
    /// the tracker and the web head (with `modes`, `adsbee::report_raw`
    /// reports the shared table).
    #[cfg(not(feature = "modes"))]
    #[inline(never)]
    pub fn report(&mut self, table: Option<&mut EsTable>, tracker: &mut HubTracker, mut feed: Option<&mut Feed>, now: u64) {
        let Some(table) = table else { return };
        if now.saturating_sub(self.last_report_ms) < 1000 || table.stats.frames == 0 {
            return;
        }
        self.last_report_ms = now;
        table.set_reference(tracker.own_position(now).map(|f| (f.lat, f.lon)));
        table.report(now, |a| {
            tracker.ingest_aircraft(&a, now);
            if let Some(f) = feed.as_deref_mut() {
                f.adsb(&adsb::Report::Aircraft(a));
            }
        });
    }

    /// Every [`PRINT_MS`], one console line when anything was counted.
    #[inline(never)]
    pub fn print(&mut self, key_state: &str, syncs: u32, now: u64) {
        if now.saturating_sub(self.last_print_ms) < PRINT_MS {
            return;
        }
        self.last_print_ms = now;
        if !(self.es.any() || self.uat.any() || self.ogn.any() || self.flarm.any() || !self.nodes.is_empty()) {
            return;
        }
        let (e, u, o, f, s) = (&self.es, &self.uat, &self.ogn, &self.flarm, &self.flarm_skips);
        crate::println!(
            "hub-p4: aero es1090 {}/{}/{} uat {}/{}/{} ogn {}/{}/{} flarm {}/{}/{} (frames/decoded/rejected) flarm no_keys={} no_time={} no_pos={} keys={} nodes={} time_syncs={}",
            e.frames,
            e.decoded,
            e.rejected,
            u.frames,
            u.decoded,
            u.rejected,
            o.frames,
            o.decoded,
            o.rejected,
            f.frames,
            f.decoded,
            f.rejected,
            s.no_keys,
            s.no_time,
            s.no_pos,
            key_state,
            self.nodes.len(),
            syncs
        );
    }

    /// The `"aero"` object of the hub line (no leading comma). Arrays are
    /// `[frames, decoded, rejected]`; FLARM's adds `no_keys, no_time,
    /// no_pos`. Short on purpose: the hub line's buffer is nearly full.
    /// `lost`: aircraft frames dropped because their queue was full.
    #[inline(never)]
    pub fn hub_json(&self, w: &mut impl core::fmt::Write, key_state: &str, syncs: u32, lost: u32) {
        let (e, u, o, f, s) = (&self.es, &self.uat, &self.ogn, &self.flarm, &self.flarm_skips);
        let _ = write!(
            w,
            "\"aero\":{{\"keys\":\"{key_state}\",\"syncs\":{syncs},\"es\":[{},{},{}],\"uat\":[{},{},{}],\"ogn\":[{},{},{}],\"flarm\":[{},{},{},{},{},{}],\"lost\":{lost},\"nodes\":{}}}",
            e.frames,
            e.decoded,
            e.rejected,
            u.frames,
            u.decoded,
            u.rejected,
            o.frames,
            o.decoded,
            o.rejected,
            f.frames,
            f.decoded,
            f.rejected,
            s.no_keys,
            s.no_time,
            s.no_pos,
            self.nodes.len()
        );
    }

    /// One `{"t":"aero"}` line with every aero node's latest status (none
    /// when no aero node has reported).
    #[inline(never)]
    pub fn nodes_json(&self, w: &mut impl core::fmt::Write, now: u64) {
        if self.nodes.is_empty() {
            return;
        }
        let _ = write!(w, "{{\"t\":\"aero\",\"nodes\":[");
        for (i, n) in self.nodes.iter().enumerate() {
            let (m, st) = (n.mac, &n.status);
            let _ = write!(
                w,
                "{}{{\"mac\":\"{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}\",\"mode\":\"{}\",\"code\":{},\"freq_khz\":{},\"time\":\"{}\",\"parked\":{},\"radio_ok\":{},\"restarts\":{},\"rejected\":{},\"rate_dropped\":{},\"age_ms\":{}}}",
                if i == 0 { "" } else { "," },
                m[0],
                m[1],
                m[2],
                m[3],
                m[4],
                m[5],
                mode_name(st.mode),
                st.mode,
                st.freq_khz,
                time_name(st.time),
                st.parked,
                st.radio_ok,
                st.rx_restarts,
                st.rejected,
                st.rate_dropped,
                now.saturating_sub(n.at_ms)
            );
        }
        let _ = writeln!(w, "]}}");
    }
}

/// A decoded aircraft to the tracker and the web head, as ADSBee aircraft go.
fn deliver(mut a: adsb::Aircraft, rssi: i16, tracker: &mut HubTracker, feed: Option<&mut Feed>, now: u64) {
    a.signal_dbm = rssi;
    tracker.ingest_aircraft(&a, now);
    if let Some(f) = feed {
        f.adsb(&adsb::Report::Aircraft(a));
    }
}

/// `AeroStatus::mode` for JSON.
pub fn mode_name(code: u32) -> &'static str {
    match AeroMode::from_code(code) {
        _ if code == 0 => "none",
        Some(AeroMode::Es1090) => "es1090",
        Some(AeroMode::Uat978) => "uat978",
        Some(AeroMode::FlarmHop { ogn: false, .. }) => "flarm",
        Some(AeroMode::FlarmHop { ogn: true, .. }) => "ogn",
        Some(AeroMode::Control { .. }) => "control",
        // Settings, never a running mode.
        Some(AeroMode::ControlFlags { .. }) | None => "unknown",
    }
}

fn time_name(t: AeroTime) -> &'static str {
    match t {
        AeroTime::Unknown => "none",
        AeroTime::Hub => "hub",
        AeroTime::HubPps => "hub_pps",
    }
}

/// The health screen's line under an aero node: what its radio is doing.
pub fn screen_label(st: &AeroStatus) -> &'static str {
    if !st.radio_ok {
        return "LR no radio";
    }
    match (AeroMode::from_code(st.mode), st.parked) {
        _ if st.mode == 0 => "LR idle",
        (Some(AeroMode::Es1090), _) => "LR 1090 ES",
        (Some(AeroMode::Uat978), _) => "LR UAT 978",
        (Some(AeroMode::FlarmHop { ogn: false, .. }), false) => "LR FLARM hop",
        (Some(AeroMode::FlarmHop { ogn: false, .. }), true) => "LR FLARM parked",
        (Some(AeroMode::FlarmHop { ogn: true, .. }), false) => "LR OGN hop",
        (Some(AeroMode::FlarmHop { ogn: true, .. }), true) => "LR OGN parked",
        (Some(AeroMode::Control { .. }), _) => "LR control",
        (Some(AeroMode::ControlFlags { .. }) | None, _) => "LR other",
    }
}

/// The PSRAM store's parts for their users (`links.rs` gets the queue; the
/// 1090 table goes to the ADSBee with `modes`, else stays with the main
/// loop for aero frames; the control-link table goes to the app). All
/// `None` without PSRAM.
pub type Parts = (
    Option<&'static mut AirQueue>,
    Option<&'static mut EsTable>,
    Option<&'static mut tracker::ctrllinks::ControlTable>,
);

#[inline(never)]
pub fn split(st: Option<&'static mut Store>) -> Parts {
    match st {
        Some(Store { queue, table, control }) => (Some(queue), Some(table), Some(control)),
        None => (None, None, None),
    }
}
