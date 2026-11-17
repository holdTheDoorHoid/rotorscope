//! Floods of header-only management frames: deauthentication (12),
//! disassociation (10), authentication (11) and (re)association requests
//! (0, 2) aimed at one access point, and probe requests (4) from many
//! sources.
//!
//! [`Floods::frame`] reads frame control, the three addresses and (for
//! deauth / disassoc) the two-byte reason code, and bumps counters: no
//! allocation, a scan of a 16-entry table at worst. It is meant to run in
//! the radio's receive callback. [`Floods::poll`] closes one-second windows
//! and reports.
//!
//! # Thresholds (per BSSID, per one-second window)
//!
//! Only frames without the retry bit count towards the thresholds: a real
//! access point that kicks a station which has already left sends one
//! deauth and then up to ~7 retries, which would otherwise look like a
//! burst. Injected floods are sent fresh (broadcast frames are never
//! retried at all).
//!
//! A hopping node hears a channel only part of the time (75% on its home
//! channel, ~12% on a 50 ms visit), so these are rates *as heard*: a flood
//! at 100 frames/s on a visited channel still reads well above them.

use crate::mac::{self, ASSOC_REQ, AUTH, DEAUTH, DISASSOC, PROBE_REQ, REASSOC_REQ};
use crate::util::{BROADCAST, Bits, Ms, NONE, elapsed, hash, ms};
use crate::{ThreatKind, ThreatSighting, sighting};

/// Length of one counting window.
pub const WINDOW_MS: Ms = 1000;

/// Deauth / disassoc: fresh frames in one window at one BSSID that are an
/// attack outright. Ordinary networks send a handful when a station leaves,
/// roams or is steered; aireplay-ng sends 64-frame bursts, mdk4 hundreds a
/// second.
pub const KILL_BURST: u16 = 30;
/// ...or this many in each of [`KILL_SUSTAIN`] windows in a row (a steady
/// flood paced to stay under the burst level).
pub const KILL_RATE: u16 = 10;
pub const KILL_SUSTAIN: u8 = 2;
/// Broadcast-destination deauths / disassocs disconnect every client at
/// once and a real access point sends at most a few (when it shuts a
/// network down), so a lower bar applies to them.
pub const KILL_BROADCAST_BURST: u16 = 10;

/// Authentication / association requests to one access point: this many
/// fresh requests from at least [`AUTH_SOURCES`] addresses, in each of two
/// windows in a row (one window can be every client rejoining after the
/// access point rebooted)...
pub const AUTH_FLOOD: u16 = 30;
pub const AUTH_SOURCES: u32 = 8;
/// ...or this many in one window from anyone (mdk4's authentication flood
/// sends hundreds a second).
pub const AUTH_FLOOD_ONE: u16 = 100;

/// Probe requests on the channel: this many in one window from at least
/// [`PROBE_SOURCES`] distinct addresses (lower bound from a 512-bit map).
/// Phones randomise their address per scan, so a crowd gives a few new
/// addresses a second; flood tools give hundreds.
pub const PROBE_FLOOD: u16 = 100;
pub const PROBE_SOURCES: u32 = 36;
/// ...or this many from anyone (one address hammering).
pub const PROBE_FLOOD_ONE: u16 = 300;

/// `detail` bit: some deauth / disassoc frames went to the broadcast address.
pub const DETAIL_BROADCAST: u32 = 1 << 16;
/// `detail` bit: every frame was protected (802.11w), reason unreadable.
pub const DETAIL_PROTECTED: u32 = 1 << 17;

/// BSSIDs followed at once.
pub const TABLE: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    Deauth,
    Disassoc,
    Auth,
}

#[derive(Clone, Copy, Debug)]
struct Counts {
    /// Every frame, retries included.
    frames: u16,
    /// Without the retry bit.
    fresh: u16,
    bcast: u16,
    protected: u16,
    /// Boyer-Moore majority vote over reason codes.
    reason: u16,
    votes: u16,
    sources: Bits<2>,
    src: [u8; 6],
    dst: [u8; 6],
    many_dst: bool,
    rssi: i8,
    channel: u8,
}

impl Counts {
    const EMPTY: Self = Self {
        frames: 0,
        fresh: 0,
        bcast: 0,
        protected: 0,
        reason: 0,
        votes: 0,
        sources: Bits::new(),
        src: NONE,
        dst: NONE,
        many_dst: false,
        rssi: i8::MIN,
        channel: 0,
    };

    fn add(&mut self, h: &mac::Header, rssi: i8, channel: u8) {
        if self.frames > 0 && self.dst != h.addr1 {
            self.many_dst = true;
        }
        self.frames = self.frames.saturating_add(1);
        if !h.retry {
            self.fresh = self.fresh.saturating_add(1);
        }
        if h.addr1 == BROADCAST {
            self.bcast = self.bcast.saturating_add(1);
        }
        if h.protected {
            self.protected = self.protected.saturating_add(1);
        }
        self.sources.insert(hash(&h.addr2));
        self.src = h.addr2;
        self.dst = h.addr1;
        self.rssi = self.rssi.max(rssi);
        self.channel = channel;
    }

    fn vote(&mut self, reason: u16) {
        if self.votes == 0 {
            self.reason = reason;
            self.votes = 1;
        } else if self.reason == reason {
            self.votes = self.votes.saturating_add(1);
        } else {
            self.votes -= 1;
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Entry {
    class: Class,
    bssid: [u8; 6],
    c: Counts,
    /// Windows in a row at or above the rate threshold.
    hot: u8,
    alarmed: bool,
    /// Windows in a row with nothing.
    idle: u8,
}

/// Running totals since boot, for a node's stats line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub deauth: u32,
    pub disassoc: u32,
    pub auth: u32,
    pub probe: u32,
    /// Sightings reported.
    pub threats: u32,
}

/// Header-only flood counters. See the module docs.
pub struct Floods {
    entries: heapless::Vec<Entry, TABLE>,
    probe: Counts,
    /// Distinct probe sources: a bigger map than per-BSSID entries get.
    probe_sources: Bits<8>,
    probe_hot: bool,
    start: Ms,
    started: bool,
    pub totals: Totals,
}

impl Default for Floods {
    fn default() -> Self {
        Self::new()
    }
}

impl Floods {
    pub const fn new() -> Self {
        Self {
            entries: heapless::Vec::new(),
            probe: Counts::EMPTY,
            probe_sources: Bits::new(),
            probe_hot: false,
            start: 0,
            started: false,
            totals: Totals { deauth: 0, disassoc: 0, auth: 0, probe: 0, threats: 0 },
        }
    }

    /// Count one frame (MPDU; a trailing FCS does no harm). Returns true if
    /// the frame is one of the header-only subtypes
    /// ([`mac::header_only`]), counted or too short to count: the caller
    /// need not queue it anywhere else.
    pub fn frame(&mut self, mpdu: &[u8], rssi: i8, channel: u8, now_us: u64) -> bool {
        let Some(&fc0) = mpdu.first() else { return false };
        if fc0 & 0x0F != 0 || !mac::header_only(fc0 >> 4) {
            return false;
        }
        let Some(h) = mac::header(mpdu) else { return true };
        if !self.started {
            self.started = true;
            self.start = ms(now_us);
        }
        match h.subtype {
            PROBE_REQ => {
                self.totals.probe = self.totals.probe.wrapping_add(1);
                self.probe.add(&h, rssi, channel);
                self.probe_sources.insert(hash(&h.addr2));
            }
            DEAUTH | DISASSOC => {
                let class = if h.subtype == DEAUTH {
                    self.totals.deauth = self.totals.deauth.wrapping_add(1);
                    Class::Deauth
                } else {
                    self.totals.disassoc = self.totals.disassoc.wrapping_add(1);
                    Class::Disassoc
                };
                let reason = match (h.protected, mpdu.get(h.body_at..h.body_at + 2)) {
                    (false, Some(r)) => u16::from_le_bytes([r[0], r[1]]),
                    _ => 0,
                };
                let e = self.entry(class, h.addr3);
                e.c.add(&h, rssi, channel);
                e.c.vote(reason);
            }
            AUTH => {
                // Both directions are subtype 11; requests go to the AP
                // (receiver = BSSID, transmitter = the station).
                if h.addr1 == h.addr3 && h.addr2 != h.addr3 {
                    self.totals.auth = self.totals.auth.wrapping_add(1);
                    self.entry(Class::Auth, h.addr3).c.add(&h, rssi, channel);
                }
            }
            ASSOC_REQ | REASSOC_REQ => {
                self.totals.auth = self.totals.auth.wrapping_add(1);
                self.entry(Class::Auth, h.addr1).c.add(&h, rssi, channel);
            }
            _ => {}
        }
        true
    }

    fn entry(&mut self, class: Class, bssid: [u8; 6]) -> &mut Entry {
        let i = match self.entries.iter().position(|e| e.class == class && e.bssid == bssid) {
            Some(i) => i,
            None => {
                if self.entries.is_full() {
                    // Make room: the quietest entry that is not mid-attack.
                    let victim = (0..self.entries.len())
                        .min_by_key(|&i| (self.entries[i].alarmed, self.entries[i].c.frames))
                        .unwrap_or(0);
                    self.entries.swap_remove(victim);
                }
                let e = Entry { class, bssid, c: Counts::EMPTY, hot: 0, alarmed: false, idle: 0 };
                // Cannot fail: room was made above.
                let _ = self.entries.push(e);
                self.entries.len() - 1
            }
        };
        &mut self.entries[i]
    }

    /// Close the window if it is due and report every flood in it. Call a
    /// few times a second.
    pub fn poll(&mut self, now_us: u64, emit: &mut impl FnMut(ThreatSighting)) {
        let now = ms(now_us);
        if !self.started {
            self.started = true;
            self.start = now;
            return;
        }
        let win = elapsed(now, self.start);
        if win < WINDOW_MS {
            return;
        }
        self.start = now;
        let mut sent = 0u32;
        for e in self.entries.iter_mut() {
            if let Some(s) = judge(e, win, now_us) {
                emit(s);
                sent += 1;
            }
            e.idle = if e.c.frames == 0 { e.idle.saturating_add(1) } else { 0 };
            e.c = Counts::EMPTY;
        }
        self.entries.retain(|e| e.idle < 3 || e.alarmed);
        if let Some(s) = self.judge_probes(win, now_us) {
            emit(s);
            sent += 1;
        }
        self.probe = Counts::EMPTY;
        self.probe_sources.clear();
        self.totals.threats = self.totals.threats.wrapping_add(sent);
    }

    fn judge_probes(&mut self, win: Ms, now_us: u64) -> Option<ThreatSighting> {
        let c = &self.probe;
        let n = self.probe_sources.count();
        let alarm = (c.fresh >= PROBE_FLOOD && n >= PROBE_SOURCES)
            || c.fresh >= PROBE_FLOOD_ONE
            || (self.probe_hot && c.fresh >= PROBE_FLOOD / 2 && n >= PROBE_SOURCES / 2);
        self.probe_hot = alarm;
        if !alarm {
            return None;
        }
        let target = if c.bcast * 2 >= c.frames { BROADCAST } else { c.dst };
        let source = if n <= 1 { c.src } else { NONE };
        Some(sighting(ThreatKind::ProbeFlood, c.channel, c.rssi, c.frames.into(), win, source, target, n, &[], now_us))
    }
}

/// Decide one BSSID's window; updates its streak state.
fn judge(e: &mut Entry, win: Ms, now_us: u64) -> Option<ThreatSighting> {
    let c = &e.c;
    let f = c.fresh;
    let alarm = match e.class {
        Class::Deauth | Class::Disassoc => {
            let alarm = f >= KILL_BURST
                || c.bcast >= KILL_BROADCAST_BURST
                || (f >= KILL_RATE && (e.hot + 1 >= KILL_SUSTAIN || e.alarmed));
            e.hot = if f >= KILL_RATE { e.hot.saturating_add(1) } else { 0 };
            alarm
        }
        Class::Auth => {
            let many = f >= AUTH_FLOOD && c.sources.count() >= AUTH_SOURCES;
            let alarm = f >= AUTH_FLOOD_ONE || (many && (e.hot >= 1 || e.alarmed));
            e.hot = if many { e.hot.saturating_add(1) } else { 0 };
            alarm
        }
    };
    e.alarmed = alarm;
    if !alarm {
        return None;
    }
    let (kind, source, target, detail) = match e.class {
        Class::Deauth | Class::Disassoc => {
            let target = if c.bcast * 2 >= c.frames {
                BROADCAST
            } else if !c.many_dst {
                c.dst
            } else {
                e.bssid
            };
            let mut detail = u32::from(c.reason);
            if c.bcast > 0 {
                detail |= DETAIL_BROADCAST;
            }
            if c.protected == c.frames {
                detail = DETAIL_PROTECTED | (detail & DETAIL_BROADCAST);
            }
            let kind = if e.class == Class::Deauth { ThreatKind::Deauth } else { ThreatKind::Disassoc };
            (kind, c.src, target, detail)
        }
        Class::Auth => {
            let n = c.sources.count();
            (ThreatKind::AuthFlood, if n <= 1 { c.src } else { NONE }, e.bssid, n)
        }
    };
    Some(sighting(kind, c.channel, c.rssi, c.frames.into(), win, source, target, detail, &[], now_us))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mac::build::*;
    use std::vec::Vec;

    const AP: [u8; 6] = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55];
    const AP2: [u8; 6] = [0x00, 0x11, 0x22, 0x33, 0x44, 0x66];
    const STA: [u8; 6] = [0x00, 0xAA, 0xBB, 0xCC, 0xDD, 0x01];

    /// Feed `n` frames spread over one second starting at `t0` ms, then
    /// poll at the end of the window.
    fn second(fl: &mut Floods, t0: u64, n: usize, mut f: impl FnMut(usize) -> Vec<u8>) -> Vec<ThreatSighting> {
        let mut out = Vec::new();
        for i in 0..n {
            let t = t0 * 1000 + (i as u64 * 900_000) / n.max(1) as u64;
            assert!(fl.frame(&f(i), -40 - (i % 20) as i8, 6, t));
            fl.poll(t, &mut |s| out.push(s));
        }
        fl.poll((t0 + 1000) * 1000, &mut |s| out.push(s));
        out
    }

    fn started() -> Floods {
        let mut fl = Floods::new();
        fl.poll(0, &mut |_| panic!());
        fl
    }

    fn sta(i: usize) -> [u8; 6] {
        [0x02, 0, 0, 0, (i >> 8) as u8, i as u8]
    }

    #[test]
    fn other_subtypes_pass_through() {
        let mut fl = Floods::new();
        assert!(!fl.frame(&wpa2(AP, b"x"), -50, 6, 0));
        assert!(!fl.frame(&mgmt(13, AP, AP, AP, &[4, 4]), -50, 6, 0));
        assert!(!fl.frame(&[0x08, 0, 0], -50, 6, 0), "data frame");
        assert!(!fl.frame(&[], -50, 6, 0));
        // A truncated deauth is still "handled" (nothing else wants it).
        assert!(fl.frame(&[0xC0, 0, 0, 0], -50, 6, 0));
    }

    #[test]
    fn deauth_burst_reported_with_reason() {
        let mut fl = started();
        let out = second(&mut fl, 0, 40, |i| kill(DEAUTH, STA, AP, AP, if i % 4 == 0 { 3 } else { 7 }));
        assert_eq!(out.len(), 1, "{out:?}");
        let s = &out[0];
        assert_eq!(s.kind, ThreatKind::Deauth);
        assert_eq!(s.count, 40);
        assert_eq!(s.detail, 7);
        assert_eq!((s.source, s.target), (AP, STA));
        assert_eq!(s.level, -40);
        assert_eq!(s.window_ms, 1000);
        assert_eq!((s.channel, s.freq, s.width_khz), (6, 24370, 20_000));
        assert_eq!(fl.totals.deauth, 40);
        assert_eq!(fl.totals.threats, 1);
    }

    #[test]
    fn a_few_deauths_are_normal() {
        let mut fl = started();
        for s in 0..10 {
            // One station leaving per second, its deauth retried 7 times.
            let out = second(&mut fl, s * 1000, 8, |i| {
                let mut f = kill(DEAUTH, sta(s as usize), AP, AP, 8);
                if i > 0 {
                    f[1] |= 0x08;
                }
                f
            });
            assert!(out.is_empty(), "second {s}: {out:?}");
        }
    }

    #[test]
    fn retries_do_not_count() {
        let mut fl = started();
        let out = second(&mut fl, 0, 60, |_| {
            let mut f = kill(DEAUTH, STA, AP, AP, 7);
            f[1] |= 0x08;
            f
        });
        assert!(out.is_empty());
    }

    #[test]
    fn sustained_rate_needs_two_windows_then_continues() {
        let mut fl = started();
        let f = |_| kill(DISASSOC, STA, AP, AP, 1);
        assert!(second(&mut fl, 0, 12, f).is_empty());
        let out = second(&mut fl, 1000, 12, f);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, ThreatKind::Disassoc);
        // Keeps reporting once a second while it lasts.
        assert_eq!(second(&mut fl, 2000, 11, f).len(), 1);
        assert_eq!(second(&mut fl, 3000, 10, f).len(), 1);
        // Then stops.
        assert!(second(&mut fl, 4000, 2, f).is_empty());
        assert!(second(&mut fl, 5000, 12, f).is_empty(), "streak restarts");
    }

    #[test]
    fn broadcast_deauths_have_a_lower_bar() {
        let mut fl = started();
        let out = second(&mut fl, 0, 10, |_| kill(DEAUTH, BROADCAST, AP, AP, 7));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].target, BROADCAST);
        assert_eq!(out[0].detail, 7 | DETAIL_BROADCAST);
        // Nine is an access point shutting its networks down.
        let mut fl = started();
        assert!(second(&mut fl, 0, 9, |_| kill(DEAUTH, BROADCAST, AP, AP, 3)).is_empty());
    }

    #[test]
    fn many_stations_target_is_the_bssid() {
        let mut fl = started();
        let out = second(&mut fl, 0, 50, |i| kill(DEAUTH, sta(i), AP, AP, 7));
        assert_eq!(out[0].target, AP);
    }

    #[test]
    fn protected_deauths_are_flagged() {
        let mut fl = started();
        let out = second(&mut fl, 0, 35, |_| {
            let mut f = kill(DEAUTH, STA, AP, AP, 0);
            f[1] |= 0x40;
            f
        });
        assert_eq!(out[0].detail, DETAIL_PROTECTED);
    }

    #[test]
    fn floods_at_two_networks_are_separate() {
        let mut fl = started();
        let out = second(&mut fl, 0, 80, |i| kill(DEAUTH, BROADCAST, if i % 2 == 0 { AP } else { AP2 }, if i % 2 == 0 { AP } else { AP2 }, 7));
        assert_eq!(out.len(), 2);
        assert_ne!(out[0].source, out[1].source);
    }

    #[test]
    fn table_full_keeps_the_attack() {
        let mut fl = started();
        let mut out = Vec::new();
        // An attack on AP, with one-off deauths at 40 other BSSIDs mixed in.
        for i in 0..80usize {
            let t = i as u64 * 10_000;
            fl.frame(&kill(DEAUTH, BROADCAST, AP, AP, 7), -50, 6, t);
            if i % 2 == 0 {
                let b = [0x00, 0x22, 0, 0, 0, i as u8];
                fl.frame(&kill(DEAUTH, STA, b, b, 8), -70, 6, t);
            }
        }
        fl.poll(1_000_000, &mut |s| out.push(s));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].source, AP);
        assert!(fl.entries.len() <= TABLE);
    }

    #[test]
    fn auth_flood_from_random_sources() {
        let mut fl = started();
        let auth = |i: usize| mgmt(AUTH, AP, sta(i), AP, &[0, 0, 1, 0, 0, 0]);
        // One window like this could be clients rejoining after a reboot...
        assert!(second(&mut fl, 0, 40, auth).is_empty());
        // ...two in a row is a flood.
        let out = second(&mut fl, 1000, 40, |i| auth(i + 100));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, ThreatKind::AuthFlood);
        assert_eq!(out[0].target, AP);
        assert_eq!(out[0].source, NONE);
        assert!(out[0].detail >= 30, "{}", out[0].detail);
        // Association requests count too.
        let mut fl = started();
        assert_eq!(second(&mut fl, 0, 120, |i| mgmt(ASSOC_REQ, AP, sta(i), AP, &[0; 4])).len(), 1);
    }

    #[test]
    fn auth_responses_are_not_requests() {
        let mut fl = started();
        // AP -> station (addr1 = station): not counted.
        assert!(second(&mut fl, 0, 150, |i| mgmt(AUTH, sta(i), AP, AP, &[0, 0, 2, 0, 0, 0])).is_empty());
        assert_eq!(fl.totals.auth, 0);
    }

    #[test]
    fn single_source_auth_hammering() {
        let mut fl = started();
        let out = second(&mut fl, 0, 120, |_| mgmt(AUTH, AP, STA, AP, &[0, 0, 1, 0, 0, 0]));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].source, STA);
    }

    #[test]
    fn probe_flood_vs_a_crowd() {
        let probe = |src: [u8; 6]| mgmt(PROBE_REQ, BROADCAST, src, BROADCAST, &[0, 0, 1, 1, 0x82]);
        // A crowd: 10 phones, each a 4-probe scan burst from a new random address.
        let mut fl = started();
        for s in 0..20 {
            assert!(second(&mut fl, s * 1000, 40, |i| probe(sta(s as usize * 10 + i / 4))).is_empty());
        }
        // A flood: 200 a second, each from a new address.
        let out = second(&mut fl, 20_000, 200, |i| probe(sta(1000 + i)));
        assert_eq!(out.len(), 1);
        let s = &out[0];
        assert_eq!(s.kind, ThreatKind::ProbeFlood);
        assert_eq!((s.count, s.target, s.source), (200, BROADCAST, NONE));
        assert!(s.detail >= 150, "{}", s.detail);
        // One address hammering.
        let out = second(&mut fl, 21_000, 400, |_| probe(STA));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].source, STA);
    }

    #[test]
    fn idle_entries_are_dropped() {
        let mut fl = started();
        second(&mut fl, 0, 3, |_| kill(DEAUTH, STA, AP, AP, 7));
        assert_eq!(fl.entries.len(), 1);
        for s in 1..4 {
            fl.poll(s * 1_000_000 + 1_000_000, &mut |_| {});
        }
        assert!(fl.entries.is_empty());
    }

    #[test]
    fn one_report_per_window() {
        let mut fl = started();
        let mut out = Vec::new();
        for i in 0..1000u64 {
            fl.frame(&kill(DEAUTH, BROADCAST, AP, AP, 7), -50, 1, i * 3000);
            fl.poll(i * 3000, &mut |s| out.push(s));
        }
        // ~3 s of flood (last frame at 2.997 s): windows close at ~1 s and
        // ~2 s; the third is still open.
        assert_eq!(out.len(), 2, "{}", out.len());
        assert!(out.iter().all(|s| s.channel == 1));
    }
}
