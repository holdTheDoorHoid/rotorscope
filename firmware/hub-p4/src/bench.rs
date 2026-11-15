//! Bench only (`--features benchlog`): print what the tracker holds, so an
//! end-to-end test can prove a detection reached the hub's tables.
//!
//! * At once (checked every 200 ms): one `bench new-...` line for every drone
//!   slot (transport and band) heard for the first time, every new open-link
//!   stream, RF emitter and Wi-Fi drone, with the hub time.
//! * Every 5 s: a summary of every table and of each relayed node's role.
//!
//! Never prints an identifier that is not one of our own test values: a drone
//! ID is printed only if it is the test beacon's (`TESTDRONE...`) or the
//! `fakedrone` feature's (`RSTEST-...`), an open link's ID only as
//! `fake` / `none` / `other`, Wi-Fi drones without address or network name.

use core::fmt::Write;

use esp_println::println;
use heapless::{String, Vec};

use crate::app::HubTracker;
use crate::links::GatewayLink;

const CHECK_MS: u64 = 200;
const SUMMARY_MS: u64 = 5_000;
/// The rid-beacon's fake open-link IDs (crates/openlink/src/build.rs).
const FAKE_LINK_IDS: [u32; 3] = [0x7e57ed, 0x7e57_0001, 0x7e];

pub fn our_id(id: &[u8]) -> bool {
    id.starts_with(b"TESTDRONE") || id.starts_with(b"RSTEST-")
}

fn shown_id(t: &tracker::Track) -> &str {
    match t.primary_id() {
        Some(u) if our_id(u.id.as_bytes()) => u.id.as_str().unwrap_or("?"),
        Some(_) => "(not a test ID)",
        None => "(none)",
    }
}

fn link_id(id: Option<u32>) -> &'static str {
    match id {
        None => "none",
        Some(v) if FAKE_LINK_IDS.contains(&v) => "fake",
        Some(_) => "other",
    }
}

#[derive(Default)]
pub struct Bench {
    last_check: u64,
    last_summary: u64,
    /// (track id << 3 | slot index) already announced.
    slots: Vec<u32, 256>,
    /// (link track id, channel, port, direction) already announced.
    streams: Vec<(u32, u8, u8, u8), 128>,
    emitters: Vec<u32, 64>,
    wdrones: Vec<u32, 64>,
}

impl Bench {
    pub fn tick(&mut self, t: &HubTracker, gw: &GatewayLink<'_>, now: u64) {
        if now.saturating_sub(self.last_check) >= CHECK_MS {
            self.last_check = now;
            self.new_things(t, now);
        }
        if now.saturating_sub(self.last_summary) >= SUMMARY_MS {
            self.last_summary = now;
            summary(t, gw, now);
        }
    }

    fn new_things(&mut self, t: &HubTracker, now: u64) {
        for tr in t.tracks() {
            for (i, s) in tracker::SLOTS.iter().enumerate() {
                let l = tr.slot(*s);
                if !l.heard() {
                    continue;
                }
                let key = (tr.id.0 << 3) | i as u32;
                if self.slots.contains(&key) {
                    continue;
                }
                let _ = self.slots.push(key);
                println!(
                    "hub-p4: bench new-slot ms={now} track={} id={} slot={:?}/{:?} ch={} rssi={} node={} first_ms={} count={}",
                    tr.id.0, shown_id(tr), s.transport, s.band, l.channel, l.rssi, l.node, tr.first_seen_ms, l.count
                );
            }
        }
        for lt in t.open_links() {
            for s in &lt.streams {
                let key = (lt.id.0, s.channel, s.port, s.direction as u8);
                if self.streams.contains(&key) {
                    continue;
                }
                let _ = self.streams.push(key);
                println!(
                    "hub-p4: bench new-link ms={now} link=L{} sys={:?} id={} ch={} port={} dir={:?} rssi={} node={} pkts={}",
                    lt.id.0, lt.system, link_id(lt.link_id), s.channel, s.port, s.direction, s.rssi, s.node, s.packets
                );
            }
        }
        for e in t.emitters() {
            if self.emitters.contains(&e.id.0) {
                continue;
            }
            let _ = self.emitters.push(e.id.0);
            println!(
                "hub-p4: bench new-rf ms={now} rf=R{} kind={:?} mhz={:.1} width_khz={} snr_db={} duty={} line_hz={} node={}",
                e.id.0, e.kind, e.freq_mhz(), e.width_khz, e.snr_db, e.duty_pct, e.line_hz, e.node
            );
        }
        for w in t.wifi_drones() {
            if self.wdrones.contains(&w.id.0) {
                continue;
            }
            let _ = self.wdrones.push(w.id.0);
            println!(
                "hub-p4: bench new-wifidrone ms={now} w=W{} maker={} ch={} rssi={} node={}",
                w.id.0, w.maker_name(), w.channel, w.rssi, w.node
            );
        }
    }
}

fn summary(t: &HubTracker, gw: &GatewayLink<'_>, now: u64) {
    for tr in t.tracks() {
        let mut s: String<200> = String::new();
        for (slot, l) in tr.slots() {
            let _ = write!(s, " {:?}/{:?}:{}@ch{}", slot.transport, slot.band, l.count, l.channel);
        }
        println!(
            "hub-p4: bench track ms={now} track={} id={} first_ms={} last_ms={} loc={} decode_err={}{}",
            tr.id.0, shown_id(tr), tr.first_seen_ms, tr.last_seen_ms, tr.location.is_some(), tr.decode_errors, s
        );
    }
    for lt in t.open_links() {
        let mut s: String<160> = String::new();
        for st in &lt.streams {
            let _ = write!(s, " ch{}:{}", st.channel, st.packets);
        }
        println!(
            "hub-p4: bench link ms={now} link=L{} sys={:?} id={} sightings={} first_ms={} last_ms={} streams{}",
            lt.id.0, lt.system, link_id(lt.link_id), lt.sightings, lt.first_seen_ms, lt.last_seen_ms, s
        );
    }
    for e in t.emitters() {
        println!(
            "hub-p4: bench rf ms={now} rf=R{} kind={:?} mhz={:.1} sightings={} peak_snr_db={} last_ms={}",
            e.id.0, e.kind, e.freq_mhz(), e.sightings, e.peak_snr_db, e.last_seen_ms
        );
    }
    for w in t.wifi_drones() {
        println!("hub-p4: bench wifidrone ms={now} w=W{} maker={} ch={} sightings={}", w.id.0, w.maker_name(), w.channel, w.sightings);
    }
    for n in gw.nodes.iter() {
        let m = n.mac;
        println!(
            "hub-p4: bench node ms={now} {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} role={:?} can={:#04x} plan={:04x} assigned={:?} applied={} frames={} detections={} last_ms={}",
            m[0], m[1], m[2], m[3], m[4], m[5], n.role, n.can, n.plan, n.assigned, n.applied, n.frames, n.detections, n.last_ms
        );
    }
    println!(
        "hub-p4: bench tables ms={now} drones={} links={} rf={} wifidrones={}",
        t.tracks().len(), t.open_links().len(), t.emitters().len(), t.wifi_drones().len()
    );
}

/// Counts of one CSV log file's rows by kind, for the SD read-back
/// ([`crate::sd`] calls [`CsvCount::line`] for every line).
#[derive(Default)]
pub struct CsvCount {
    pub header: u32,
    pub ours: u32,
    pub other_drones: u32,
    pub links: u32,
    pub rf: u32,
    pub wdrones: u32,
    pub aircraft: u32,
    pub bad: u32,
    /// Distinct `transports` values on our drone and link rows, and RF kinds.
    pub kinds: Vec<String<48>, 12>,
}

impl CsvCount {
    pub fn line(&mut self, line: &[u8]) {
        let mut fields: Vec<&[u8], 24> = Vec::new();
        let (mut start, mut quoted) = (0, false);
        for (i, &b) in line.iter().enumerate() {
            match b {
                b'"' => quoted = !quoted,
                b',' if !quoted => {
                    let _ = fields.push(&line[start..i]);
                    start = i + 1;
                }
                _ => {}
            }
        }
        let _ = fields.push(&line[start..]);
        if fields.len() < 10 {
            self.bad += 1;
            return;
        }
        let (track, uas, ua_type, tr) = (fields[2], fields[6], fields[8], fields[9]);
        let kind = match track.first() {
            _ if track == b"track" => {
                self.header += 1;
                return;
            }
            Some(b'L') => {
                self.links += 1;
                tr
            }
            Some(b'R') => {
                self.rf += 1;
                ua_type
            }
            Some(b'W') => {
                self.wdrones += 1;
                return;
            }
            Some(b'I') => {
                self.aircraft += 1;
                return;
            }
            Some(b'0'..=b'9') if our_id(uas) => {
                self.ours += 1;
                tr
            }
            Some(b'0'..=b'9') => {
                self.other_drones += 1;
                return;
            }
            _ => {
                self.bad += 1;
                return;
            }
        };
        let Ok(k) = core::str::from_utf8(kind) else { return };
        if !self.kinds.iter().any(|x| x.as_str() == k) {
            if let Ok(s) = String::try_from(k) {
                let _ = self.kinds.push(s);
            }
        }
    }

    pub fn print(&self, name: &str, lines: u32) {
        let mut s: String<600> = String::new();
        for k in &self.kinds {
            let _ = write!(s, " {k}");
        }
        println!(
            "hub-p4: bench sd {name}: {lines} lines: header={} test_drone={} other_drones={} links={} rf={} wifidrones={} aircraft={} unparsed={} kinds:{}",
            self.header, self.ours, self.other_drones, self.links, self.rf, self.wdrones, self.aircraft, self.bad, s
        );
    }
}
