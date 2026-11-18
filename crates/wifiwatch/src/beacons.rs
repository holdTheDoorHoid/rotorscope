//! Detectors that need the frame body: beacons, probe responses and action
//! frames.
//!
//! - **Beacon flood**: many transmitters the node has not heard in the last
//!   10-20 s appearing within one second on a channel it has been listening
//!   to for a while ([`BEACON_FLOOD_NEW`]).
//! - **Karma / Pineapple**: one BSSID using many network names
//!   ([`KARMA_SSIDS`]). A BSS has exactly one SSID, so a real access point
//!   never does this; multi-network access points give each network its own
//!   BSSID.
//! - **Evil twin**: an *open* network with the name of a protected network
//!   the node has been hearing ([`TWIN_ESTABLISHED`]). Deliberately narrow:
//!   enterprise and mesh networks have many BSSIDs per name with the same
//!   security, and those are never flagged; neither are WPA2/WPA3
//!   transition pairs or the open half of an OWE transition pair.
//! - **Channel switch abuse**: CSA / extended CSA in beacons or action
//!   frames that contradict the same BSSID's own plain beacons on the same
//!   channel, name an impossible channel, go on far longer than a real
//!   switch, never count down, or arrive as a flood of action frames.
//! - **Attack tools**: [`crate::tools`].
//! - **Copies of drone networks** ([`crate::drones::Clones`]); and which
//!   beacons are drones' own, returned by [`Beacons::frame`] for
//!   [`crate::drones::DroneLinks::learn`].

use crate::mac::{self, ACTION, BEACON, BssInfo, PROBE_RESP, Security};
use crate::util::{BROADCAST, Bits, Ms, NONE, RateLimit, Recent, Warmth, elapsed, hash, ms};
use crate::drones::{Clones, Heard};
use crate::{ThreatKind, ThreatSighting, sighting, tools};

/// Beacon-flood and action-CSA counting window.
pub const WINDOW_MS: Ms = 1000;
/// A channel counts "new" transmitters only after it has been heard
/// continuously this long (see [`Warmth`]). Longer than a hop cycle, so a
/// visited channel's networks are all known by then.
pub const WARM_MS: Ms = 4000;
/// A channel not heard for this long is cold again. Plan channels are heard
/// every few hundred ms; scouting visits come tens of seconds apart and so
/// never warm up (a flood there is caught by its other signs).
pub const COLD_GAP_MS: Ms = 5000;
/// How long a transmitter stays "known" after it was last heard: one to two
/// of these. Long enough to bridge fades and hop gaps.
pub const KNOWN_GEN_MS: Ms = 10_000;

/// New transmitters within one window on warm channels that make a beacon
/// flood. A dense apartment block or office adds a weak network now and
/// then as it fades in and out (a few a second at worst); beacon spam tools
/// (Marauder, deauther boards, mdk4) send tens to hundreds of fake networks
/// a second, each from a fresh random address.
pub const BEACON_FLOOD_NEW: u16 = 20;
/// While a flood is on, keep reporting down to this level.
pub const BEACON_FLOOD_HOLD: u16 = 10;

/// Distinct non-empty names from one BSSID that make it a Karma /
/// Pineapple-style responder. A real BSS has one name (two if it is hidden
/// and answers probes with its real name, but the empty name is not
/// counted).
pub const KARMA_SSIDS: u32 = 5;
/// Names from one BSSID are counted over this long.
pub const KARMA_WINDOW_MS: Ms = 30_000;

/// Frames from a protected network before its name is "established" and an
/// open copy of it is an evil twin.
pub const TWIN_ESTABLISHED: u16 = 5;
/// Frames from the open copy before it is reported (one frame could be a
/// corrupted capability bit).
pub const TWIN_OPEN_FRAMES: u16 = 2;
/// The protected original must have been heard this recently: a network
/// that was reconfigured from protected to open stops being a twin.
pub const TWIN_RECENT_MS: Ms = 60_000;

/// Channel switch: CSA frames and plain beacons from one BSSID, on one
/// channel, inside [`CSA_WINDOW_MS`], that contradict each other. During a
/// real switch every beacon carries the announcement.
pub const CSA_CONFLICT: u16 = 2;
pub const CSA_WINDOW_MS: Ms = 3000;
/// A real switch announces for `count` beacon intervals, usually well under
/// two seconds. Still announcing after this long, with at least
/// [`CSA_LONG_FRAMES`] frames: abuse.
pub const CSA_LONG_MS: Ms = 10_000;
pub const CSA_LONG_FRAMES: u16 = 20;
/// The switch count must fall by one per beacon interval. Frames at least
/// [`CSA_STUCK_GAP_MS`] apart whose count did not fall, this many times:
/// a tool replaying one frame.
pub const CSA_STUCK: u16 = 10;
pub const CSA_STUCK_GAP_MS: Ms = 90;
/// CSA action frames (any sender) in one window. Access points send a few
/// when radar forces them off a DFS channel.
pub const CSA_ACTION_FLOOD: u16 = 10;
/// A CSA entry not updated for this long is forgotten.
const CSA_FORGET_MS: Ms = 30_000;

const SUSPECTS: usize = 8;
const NETS: usize = 64;
const CSA_TABLE: usize = 8;

#[derive(Clone, Copy, Debug)]
struct Suspect {
    bssid: [u8; 6],
    names: Bits<4>,
    frames: u32,
    first: Ms,
    last: Ms,
}

/// One network name and what has been heard using it.
#[derive(Clone, Copy, Debug)]
struct Net {
    used: bool,
    ssid: u32,
    prot_bssid: [u8; 6],
    prot_class: u8,
    prot_frames: u16,
    prot_last: Ms,
    open_bssid: [u8; 6],
    open_frames: u16,
    last: Ms,
}

#[derive(Clone, Copy, Debug)]
struct Csa {
    bssid: [u8; 6],
    channel: u8,
    new_ch: u8,
    count: u8,
    target: [u8; 6],
    frames: u16,
    first: Ms,
    last: Ms,
    /// CSA frames and plain beacons since `win`.
    win: Ms,
    csa_w: u16,
    plain_w: u16,
    stuck: u16,
    rssi: i8,
}

/// Running totals since boot, for a node's stats line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    /// Beacons / probe responses / action frames looked at.
    pub frames: u32,
    /// Frames carrying a channel switch.
    pub csa: u32,
    /// Sightings reported.
    pub threats: u32,
}

pub struct Beacons {
    warm: Warmth,
    known: Recent<16>,
    start: Ms,
    started: bool,
    new_tx: u16,
    new_names: Bits<4>,
    flood_rssi: i8,
    flood_ch: u8,
    flooding: bool,
    action_csa: u16,
    action_src: Bits<1>,
    action_last: ([u8; 6], [u8; 6], u8, u8, i8),
    /// Karma's first look: BSSIDs and (BSSID, name) pairs heard recently.
    /// A BSSID becomes a suspect when it is known but the pair is not.
    karma_bssids: Recent<16>,
    karma_pairs: Recent<16>,
    suspects: heapless::Vec<Suspect, SUSPECTS>,
    nets: [Net; NETS],
    csa: heapless::Vec<Csa, CSA_TABLE>,
    clones: Clones,
    limit: RateLimit<16>,
    pub totals: Totals,
}

impl Default for Beacons {
    fn default() -> Self {
        Self::new()
    }
}

/// Hidden networks send an empty name or one made of zero bytes.
fn hidden(ssid: &[u8]) -> bool {
    ssid.iter().all(|&b| b == 0)
}

impl Beacons {
    pub const fn new() -> Self {
        Self {
            warm: Warmth::new(COLD_GAP_MS, WARM_MS),
            known: Recent::new(KNOWN_GEN_MS),
            start: 0,
            started: false,
            new_tx: 0,
            new_names: Bits::new(),
            flood_rssi: i8::MIN,
            flood_ch: 0,
            flooding: false,
            action_csa: 0,
            action_src: Bits::new(),
            action_last: (NONE, NONE, 0, 0, i8::MIN),
            karma_bssids: Recent::new(KNOWN_GEN_MS),
            karma_pairs: Recent::new(KNOWN_GEN_MS),
            suspects: heapless::Vec::new(),
            nets: [Net {
                used: false,
                ssid: 0,
                prot_bssid: NONE,
                prot_class: 0,
                prot_frames: 0,
                prot_last: 0,
                open_bssid: NONE,
                open_frames: 0,
                last: 0,
            }; NETS],
            csa: heapless::Vec::new(),
            clones: Clones::new(),
            limit: RateLimit::new(1000),
            totals: Totals { frames: 0, csa: 0, threats: 0 },
        }
    }

    /// One management frame (MPDU without FCS). Other subtypes are ignored.
    /// Returns the drone network a beacon or probe response comes from, if
    /// it is one (`wifidrone`), for [`crate::drones::DroneLinks::learn`].
    pub fn frame<'a>(
        &mut self,
        mpdu: &'a [u8],
        rssi: i8,
        channel: u8,
        now_us: u64,
        emit: &mut impl FnMut(ThreatSighting),
    ) -> Option<Heard<'a>> {
        let mut heard = None;
        let now = ms(now_us);
        let mut sent = 0u32;
        let mut e = |s: ThreatSighting| {
            sent += 1;
            emit(s)
        };
        self.roll(now, now_us, &mut e);
        if let Some(h) = mac::header(mpdu) {
            match h.subtype {
                BEACON | PROBE_RESP => {
                    if let Some(info) = mac::bss_info(&h, mpdu) {
                        self.totals.frames = self.totals.frames.wrapping_add(1);
                        heard = self.bss(&h, &info, rssi, channel, now, now_us, &mut e);
                    }
                }
                ACTION => {
                    if let Some((new_ch, count)) = mac::action_csa(&h, mpdu) {
                        self.totals.frames = self.totals.frames.wrapping_add(1);
                        self.totals.csa = self.totals.csa.wrapping_add(1);
                        self.action_csa = self.action_csa.saturating_add(1);
                        self.action_src.insert(hash(&h.addr2));
                        self.action_last = (h.addr2, h.addr1, new_ch, channel, self.action_last.4.max(rssi));
                        self.csa_frame(h.addr3, h.addr1, new_ch, count, rssi, channel, now, now_us, &mut e);
                    }
                }
                _ => {}
            }
        }
        self.totals.threats = self.totals.threats.wrapping_add(sent);
        heard
    }

    /// Close the counting window if due. Called by `frame` too; call it
    /// from a timer as well so a flood that stops is not left hanging.
    pub fn poll(&mut self, now_us: u64, emit: &mut impl FnMut(ThreatSighting)) {
        let mut sent = 0u32;
        self.roll(ms(now_us), now_us, &mut |s| {
            sent += 1;
            emit(s)
        });
        self.totals.threats = self.totals.threats.wrapping_add(sent);
    }

    fn roll(&mut self, now: Ms, now_us: u64, emit: &mut impl FnMut(ThreatSighting)) {
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
        let n = self.new_tx;
        let flood = n >= BEACON_FLOOD_NEW || (self.flooding && n >= BEACON_FLOOD_HOLD);
        self.flooding = flood;
        if flood {
            emit(sighting(
                ThreatKind::BeaconFlood,
                self.flood_ch,
                self.flood_rssi,
                n.into(),
                win,
                NONE,
                NONE,
                self.new_names.count(),
                &[],
                now_us,
            ));
        }
        self.new_tx = 0;
        self.new_names.clear();
        self.flood_rssi = i8::MIN;

        if self.action_csa >= CSA_ACTION_FLOOD {
            let (src, dst, new_ch, ch, rssi) = self.action_last;
            let source = if self.action_src.count() <= 1 { src } else { NONE };
            emit(sighting(
                ThreatKind::ChannelSwitch,
                ch,
                rssi,
                self.action_csa.into(),
                win,
                source,
                dst,
                new_ch.into(),
                &[],
                now_us,
            ));
        }
        self.action_csa = 0;
        self.action_src.clear();
        self.action_last.4 = i8::MIN;
    }

    #[allow(clippy::too_many_arguments, reason = "internal; all from one frame")]
    fn bss<'a>(
        &mut self,
        h: &mac::Header,
        info: &BssInfo<'a>,
        rssi: i8,
        channel: u8,
        now: Ms,
        now_us: u64,
        emit: &mut impl FnMut(ThreatSighting),
    ) -> Option<Heard<'a>> {
        let bssid = h.addr3;
        // Known tools.
        if let Some(tool) = tools::match_tool(&h.addr2, info)
            && self.limit.allow(ThreatKind::AttackTool as u8, h.addr2, now)
        {
            emit(sighting(ThreatKind::AttackTool, channel, rssi, 1, 0, h.addr2, NONE, tool.into(), info.ssid, now_us));
        }

        // Beacon flood: new transmitters on warm channels (beacons only;
        // probe responses answer someone and come in bursts).
        if h.subtype == BEACON {
            let warm = self.warm.note(channel, now);
            let fresh = !self.known.seen_before(hash(&h.addr2), now);
            if warm && fresh {
                self.new_tx = self.new_tx.saturating_add(1);
                self.new_names.insert(hash(info.ssid));
                self.flood_rssi = self.flood_rssi.max(rssi);
                self.flood_ch = channel;
            }
        }

        if !hidden(info.ssid) {
            self.karma(bssid, info.ssid, rssi, channel, now, now_us, emit);
            self.twin(bssid, info, rssi, channel, now, now_us, emit);
        }
        let heard = self.clones.bss(bssid, info, rssi, channel, now, now_us, &mut self.limit, emit);

        match info.csa {
            Some((new_ch, count)) => {
                self.totals.csa = self.totals.csa.wrapping_add(1);
                self.csa_frame(bssid, BROADCAST, new_ch, count, rssi, channel, now, now_us, emit);
            }
            None if h.subtype == BEACON => self.plain_beacon(bssid, channel, now, now_us, emit),
            None => {}
        }
        heard
    }

    #[allow(clippy::too_many_arguments, reason = "internal; all from one frame")]
    fn karma(
        &mut self,
        bssid: [u8; 6],
        ssid: &[u8],
        rssi: i8,
        channel: u8,
        now: Ms,
        now_us: u64,
        emit: &mut impl FnMut(ThreatSighting),
    ) {
        let (hb, hs) = (hash(&bssid), hash(ssid));
        let mut pair = [0u8; 8];
        pair[..4].copy_from_slice(&hb.to_le_bytes());
        pair[4..].copy_from_slice(&hs.to_le_bytes());
        let hp = hash(&pair);
        // Both sets learn from every frame.
        let known_bssid = self.karma_bssids.seen_before(hb, now);
        let known_pair = self.karma_pairs.seen_before(hp, now);
        let i = match self.suspects.iter().position(|s| s.bssid == bssid) {
            Some(i) => i,
            // A BSSID heard recently, with a name it was not heard using:
            // follow it closely. (A Bloom collision can promote an innocent
            // new BSSID; it then shows one name and ages out.)
            None if known_bssid && !known_pair => {
                if self.suspects.is_full()
                    && let Some(v) = (0..self.suspects.len()).max_by_key(|&i| elapsed(now, self.suspects[i].last))
                {
                    self.suspects.swap_remove(v);
                }
                let _ = self.suspects.push(Suspect { bssid, names: Bits::new(), frames: 0, first: now, last: now });
                self.suspects.len() - 1
            }
            None => return,
        };
        let s = &mut self.suspects[i];
        if elapsed(now, s.last) > KARMA_WINDOW_MS {
            s.names.clear();
            s.frames = 0;
            s.first = now;
        }
        s.names.insert(hs);
        s.frames = s.frames.saturating_add(1);
        s.last = now;
        let n = s.names.count();
        if n >= KARMA_SSIDS && self.limit.allow(ThreatKind::Karma as u8, bssid, now) {
            let window = elapsed(now, s.first);
            emit(sighting(ThreatKind::Karma, channel, rssi, s.frames, window, bssid, NONE, n, ssid, now_us));
        }
    }

    #[allow(clippy::too_many_arguments, reason = "internal; all from one frame")]
    fn twin(
        &mut self,
        bssid: [u8; 6],
        info: &BssInfo<'_>,
        rssi: i8,
        channel: u8,
        now: Ms,
        now_us: u64,
        emit: &mut impl FnMut(ThreatSighting),
    ) {
        let hs = hash(info.ssid);
        let net = &mut self.nets[(hs as usize) % NETS];
        if !net.used || net.ssid != hs {
            // A different name holds the slot: take it over unless it is an
            // established network still on the air.
            let keep = net.used && net.prot_frames >= TWIN_ESTABLISHED && elapsed(now, net.last) < TWIN_RECENT_MS;
            if keep {
                return;
            }
            *net = Net {
                used: true,
                ssid: hs,
                prot_bssid: NONE,
                prot_class: 0,
                prot_frames: 0,
                prot_last: now,
                open_bssid: NONE,
                open_frames: 0,
                last: now,
            };
        }
        net.last = now;
        if info.security.protected() {
            if net.prot_frames == 0 {
                net.prot_bssid = bssid;
                net.prot_class = info.security as u8;
            }
            net.prot_frames = net.prot_frames.saturating_add(1);
            net.prot_last = now;
            return;
        }
        // Open.
        if net.open_bssid != bssid {
            net.open_bssid = bssid;
            net.open_frames = 0;
        }
        net.open_frames = net.open_frames.saturating_add(1);
        let established = net.prot_frames >= TWIN_ESTABLISHED && elapsed(now, net.prot_last) < TWIN_RECENT_MS;
        if established
            && net.open_frames >= TWIN_OPEN_FRAMES
            && self.limit.allow(ThreatKind::EvilTwin as u8, bssid, now)
        {
            let detail = (u32::from(net.prot_class) << 8) | Security::Open as u32;
            let (target, count) = (net.prot_bssid, u32::from(net.open_frames));
            emit(sighting(ThreatKind::EvilTwin, channel, rssi, count, 0, bssid, target, detail, info.ssid, now_us));
        }
    }

    fn csa_entry(&mut self, bssid: [u8; 6], channel: u8, now: Ms) -> &mut Csa {
        let fresh = Csa {
            bssid,
            channel,
            new_ch: 0,
            count: 0,
            target: BROADCAST,
            frames: 0,
            first: now,
            last: now,
            win: now,
            csa_w: 0,
            plain_w: 0,
            stuck: 0,
            rssi: i8::MIN,
        };
        let i = match self.csa.iter().position(|c| c.bssid == bssid) {
            Some(i) => {
                if elapsed(now, self.csa[i].last) > CSA_FORGET_MS {
                    self.csa[i] = fresh;
                }
                i
            }
            None => {
                if self.csa.is_full()
                    && let Some(v) = (0..self.csa.len()).max_by_key(|&i| elapsed(now, self.csa[i].last))
                {
                    self.csa.swap_remove(v);
                }
                let _ = self.csa.push(fresh);
                self.csa.len() - 1
            }
        };
        let c = &mut self.csa[i];
        if elapsed(now, c.win) > CSA_WINDOW_MS {
            c.win = now;
            c.csa_w = 0;
            c.plain_w = 0;
        }
        c
    }

    #[allow(clippy::too_many_arguments, reason = "internal; all from one frame")]
    fn csa_frame(
        &mut self,
        bssid: [u8; 6],
        target: [u8; 6],
        new_ch: u8,
        count: u8,
        rssi: i8,
        channel: u8,
        now: Ms,
        now_us: u64,
        emit: &mut impl FnMut(ThreatSighting),
    ) {
        let c = self.csa_entry(bssid, channel, now);
        if c.frames > 0 && count >= c.count && elapsed(now, c.last) >= CSA_STUCK_GAP_MS {
            c.stuck = c.stuck.saturating_add(1);
        }
        c.frames = c.frames.saturating_add(1);
        c.csa_w = c.csa_w.saturating_add(1);
        c.new_ch = new_ch;
        c.count = count;
        c.channel = channel;
        c.target = target;
        c.last = now;
        c.rssi = c.rssi.max(rssi);
        self.judge_csa(bssid, now, now_us, emit);
    }

    fn plain_beacon(&mut self, bssid: [u8; 6], channel: u8, now: Ms, now_us: u64, emit: &mut impl FnMut(ThreatSighting)) {
        // Only BSSIDs already announcing a switch are followed.
        let Some(c) = self.csa.iter().find(|c| c.bssid == bssid) else { return };
        if c.channel != channel || elapsed(now, c.last) > CSA_WINDOW_MS {
            return;
        }
        let c = self.csa_entry(bssid, channel, now);
        c.plain_w = c.plain_w.saturating_add(1);
        self.judge_csa(bssid, now, now_us, emit);
    }

    fn judge_csa(&mut self, bssid: [u8; 6], now: Ms, now_us: u64, emit: &mut impl FnMut(ThreatSighting)) {
        let Some(c) = self.csa.iter().find(|c| c.bssid == bssid) else { return };
        let invalid = !mac::valid_channel(c.new_ch);
        let conflict = c.csa_w >= CSA_CONFLICT && c.plain_w >= CSA_CONFLICT;
        let long = elapsed(now, c.first) >= CSA_LONG_MS && c.frames >= CSA_LONG_FRAMES;
        let stuck = c.stuck >= CSA_STUCK;
        if !(invalid || conflict || long || stuck) || !self.limit.allow(ThreatKind::ChannelSwitch as u8, bssid, now) {
            return;
        }
        emit(sighting(
            ThreatKind::ChannelSwitch,
            c.channel,
            c.rssi,
            c.frames.into(),
            elapsed(now, c.first),
            bssid,
            c.target,
            c.new_ch.into(),
            &[],
            now_us,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mac::build::*;
    use std::vec::Vec;

    const AP: [u8; 6] = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55];

    fn ap(i: usize) -> [u8; 6] {
        [0x00, 0x11, 0x22, 0x33, (i >> 8) as u8, i as u8]
    }

    fn rnd(i: usize) -> [u8; 6] {
        let h = hash(&(i as u32).to_le_bytes()).to_le_bytes();
        [0x02 | (h[0] & 0xFC), h[1], h[2], h[3], i as u8, (i >> 8) as u8]
    }

    struct Run {
        b: Beacons,
        out: Vec<ThreatSighting>,
    }

    impl Run {
        fn new() -> Self {
            Self { b: Beacons::new(), out: Vec::new() }
        }
        fn feed(&mut self, f: &[u8], ch: u8, t_ms: u64) {
            let out = &mut self.out;
            self.b.frame(f, -55, ch, t_ms * 1000, &mut |s| out.push(s));
        }
        fn poll(&mut self, t_ms: u64) {
            let out = &mut self.out;
            self.b.poll(t_ms * 1000, &mut |s| out.push(s));
        }
        fn kinds(&self) -> Vec<ThreatKind> {
            self.out.iter().map(|s| s.kind).collect()
        }
        /// A neighbourhood: `n` ordinary networks on channel 6 beaconing
        /// from `t0` to `t1` (one round every 100 ms).
        fn neighbours(&mut self, n: usize, t0: u64, t1: u64) {
            let mut t = t0;
            while t < t1 {
                for i in 0..n {
                    let name = std::format!("Home-{i}");
                    self.feed(&wpa2(ap(i), name.as_bytes()), 6, t);
                }
                t += 100;
            }
        }
    }

    #[test]
    fn busy_neighbourhood_is_quiet() {
        let mut r = Run::new();
        // 60 networks, two minutes, with some fading in and out.
        let mut t = 0;
        while t < 120_000 {
            for i in 0..60 {
                // Every fifth network is heard only in some 20 s stretches.
                if i % 5 == 0 && (t / 20_000) % 2 == 1 && i > 30 {
                    continue;
                }
                let name = std::format!("Home-{i}");
                r.feed(&wpa2(ap(i), name.as_bytes()), 6, t);
            }
            // A hidden network answering probes with its name.
            r.feed(&beacon(ap(500), b"", true, &rsn(2)), 6, t);
            r.feed(&beacon_from(PROBE_RESP, ap(500), b"Secret", true, &rsn(2)), 6, t + 5);
            // An enterprise network with several access points.
            for k in 0..4 {
                r.feed(&beacon(ap(600 + k), b"Corp", true, &rsn(1)), 6, t + 7);
            }
            // OWE transition pair.
            r.feed(&beacon(ap(700), b"Cafe", false, &ie(221, &[0x50, 0x6F, 0x9A, 0x1C, 0])), 6, t + 9);
            r.feed(&beacon(ap(701), b"Cafe", true, &rsn(18)), 6, t + 9);
            t += 100;
        }
        r.poll(t);
        assert!(r.out.is_empty(), "{:?}", r.kinds());
    }

    #[test]
    fn beacon_flood() {
        let mut r = Run::new();
        r.neighbours(20, 0, 6000);
        assert!(r.out.is_empty());
        // 50 fake networks a second from random addresses.
        for i in 0..150 {
            let name = std::format!("FREE WIFI {}", i % 40);
            r.feed(&open(rnd(i), name.as_bytes()), 6, 6000 + i as u64 * 20);
        }
        r.poll(9100);
        let floods: Vec<_> = r.out.iter().filter(|s| s.kind == ThreatKind::BeaconFlood).collect();
        assert!(floods.len() >= 2, "{:?}", r.kinds());
        assert!(floods[0].count >= 40, "{}", floods[0].count);
        assert!(floods[0].detail >= 30, "{}", floods[0].detail);
        assert_eq!(floods[0].channel, 6);
    }

    #[test]
    fn no_flood_while_warming_up() {
        let mut r = Run::new();
        // Boot into a place with 80 networks: all are new at first.
        r.neighbours(80, 0, 3000);
        r.poll(3000);
        assert!(r.out.is_empty(), "{:?}", r.kinds());
        // A scouting visit to a busy channel never heard before.
        for i in 0..40 {
            r.feed(&wpa2(ap(1000 + i), b"Elsewhere"), 13, 3500);
        }
        r.poll(5000);
        assert!(r.out.is_empty(), "{:?}", r.kinds());
    }

    #[test]
    fn karma_one_bssid_many_names() {
        let mut r = Run::new();
        let pine = [0x00, 0x99, 0x88, 0x77, 0x66, 0x55];
        for (k, name) in ["linksys", "attwifi", "Starbucks", "HomeNet", "xfinitywifi", "eduroam"].iter().enumerate() {
            r.feed(&beacon_from(PROBE_RESP, pine, name.as_bytes(), false, &[]), 6, 100 * k as u64);
        }
        let s: Vec<_> = r.out.iter().filter(|s| s.kind == ThreatKind::Karma).collect();
        assert_eq!(s.len(), 1, "{:?}", r.kinds());
        assert_eq!(s[0].source, pine);
        // The first name is not remembered (the BSSID became a suspect on
        // the second), so the sixth name is the fifth counted.
        assert_eq!(s[0].detail, 5);
        assert_eq!(s[0].ssid.as_str(), "eduroam");
        // Rate-limited, then again a second later.
        r.feed(&beacon_from(PROBE_RESP, pine, b"eduroam", false, &[]), 6, 700);
        assert_eq!(r.out.iter().filter(|s| s.kind == ThreatKind::Karma).count(), 1);
        r.feed(&beacon(pine, b"guest", false, &[]), 6, 1600);
        let last = r.out.last().unwrap();
        assert_eq!((last.kind, last.detail), (ThreatKind::Karma, 6));
    }

    #[test]
    fn karma_survives_a_busy_channel() {
        let mut r = Run::new();
        let pine = [0x00, 0x99, 0x88, 0x77, 0x66, 0x55];
        let mut t = 0;
        for round in 0..20 {
            for i in 0..100 {
                let name = std::format!("Home-{i}");
                r.feed(&wpa2(ap(i), name.as_bytes()), 6, t);
            }
            let name = std::format!("pool-{}", round % 8);
            r.feed(&open(pine, name.as_bytes()), 6, t + 50);
            t += 100;
        }
        assert!(r.out.iter().any(|s| s.kind == ThreatKind::Karma && s.source == pine), "{:?}", r.kinds());
        assert!(r.out.iter().all(|s| s.kind == ThreatKind::Karma));
    }

    #[test]
    fn evil_twin_open_copy() {
        let mut r = Run::new();
        let twin = [0x00, 0xDE, 0xAD, 0x00, 0x00, 0x01];
        for k in 0..10 {
            r.feed(&wpa2(AP, b"CoffeeShop"), 6, k * 100);
        }
        r.feed(&open(twin, b"CoffeeShop"), 6, 1000);
        assert!(r.out.is_empty(), "one frame is not enough");
        r.feed(&open(twin, b"CoffeeShop"), 6, 1100);
        assert_eq!(r.out.len(), 1);
        let s = &r.out[0];
        assert_eq!(s.kind, ThreatKind::EvilTwin);
        assert_eq!((s.source, s.target), (twin, AP));
        assert_eq!(s.detail, (Security::Psk as u32) << 8);
        assert_eq!(s.ssid.as_str(), "CoffeeShop");
    }

    #[test]
    fn evil_twin_seen_before_the_original() {
        let mut r = Run::new();
        let twin = [0x00, 0xDE, 0xAD, 0x00, 0x00, 0x01];
        r.feed(&open(twin, b"Corp"), 1, 0);
        for k in 0..6 {
            r.feed(&beacon(ap(k), b"Corp", true, &rsn(1)), 1, 10 + k as u64);
        }
        r.feed(&open(twin, b"Corp"), 1, 200);
        assert_eq!(r.kinds(), [ThreatKind::EvilTwin]);
        assert_eq!(r.out[0].detail, (Security::Enterprise as u32) << 8);
    }

    #[test]
    fn no_twin_for_same_security_or_reconfigured() {
        let mut r = Run::new();
        // Mesh: same name, same security, many BSSIDs.
        for k in 0..50 {
            r.feed(&wpa2(ap(k % 5), b"Mesh"), 6, k as u64 * 20);
        }
        // WPA3 transition: PSK and SAE BSSIDs with one name.
        for k in 0..20 {
            r.feed(&wpa2(ap(10), b"Home"), 6, k * 50);
            r.feed(&beacon(ap(11), b"Home", true, &rsn(8)), 6, k * 50);
        }
        assert!(r.out.is_empty());
        // Reconfigured from WPA2 to open, a minute later.
        for k in 0..10 {
            r.feed(&wpa2(ap(20), b"Shed"), 6, k * 100);
        }
        for k in 0..10 {
            r.feed(&open(ap(20), b"Shed"), 6, 70_000 + k * 100);
        }
        assert!(r.out.is_empty(), "{:?}", r.kinds());
    }

    #[test]
    fn csa_conflicting_with_real_beacons() {
        let mut r = Run::new();
        // The real AP beacons normally; spoofed beacons in between tell
        // clients to go to channel 13.
        for k in 0..6 {
            r.feed(&wpa2(AP, b"Net"), 6, k * 100);
            r.feed(&beacon(AP, b"Net", true, &ie(37, &[1, 13, 3])), 6, k * 100 + 50);
        }
        let s: Vec<_> = r.out.iter().filter(|s| s.kind == ThreatKind::ChannelSwitch).collect();
        assert_eq!(s.len(), 1, "{:?}", r.kinds());
        assert_eq!((s[0].source, s[0].detail, s[0].target), (AP, 13, BROADCAST));
    }

    #[test]
    fn real_channel_switch_is_quiet() {
        let mut r = Run::new();
        for k in 0..10 {
            r.feed(&wpa2(AP, b"Net"), 52, k * 100);
        }
        // Radar: count down from 10 over a second, then gone to 36.
        for k in 0..=10u8 {
            r.feed(&beacon(AP, b"Net", true, &ie(37, &[1, 36, 10 - k])), 52, 1000 + u64::from(k) * 100);
        }
        // Two CSA action frames to clients.
        r.feed(&mgmt(ACTION, BROADCAST, AP, AP, &[0, 4, 37, 3, 1, 36, 5]), 52, 1200);
        r.feed(&mgmt(ACTION, BROADCAST, AP, AP, &[0, 4, 37, 3, 1, 36, 4]), 52, 1300);
        for k in 0..20 {
            r.feed(&wpa2(AP, b"Net"), 36, 2200 + k * 100);
        }
        r.poll(5000);
        assert!(r.out.is_empty(), "{:?}", r.kinds());
    }

    #[test]
    fn csa_stuck_count_and_bad_channel() {
        let mut r = Run::new();
        for k in 0..12 {
            r.feed(&beacon(AP, b"Net", true, &ie(37, &[1, 1, 5])), 11, k * 100);
        }
        assert_eq!(r.kinds(), [ThreatKind::ChannelSwitch]);
        let mut r = Run::new();
        r.feed(&beacon(AP, b"Net", true, &ie(60, &[1, 81, 0, 1])), 6, 0);
        assert_eq!(r.kinds(), [ThreatKind::ChannelSwitch]);
        assert_eq!(r.out[0].detail, 0);
    }

    #[test]
    fn csa_action_flood() {
        let mut r = Run::new();
        r.poll(0);
        for k in 0..30 {
            let sta = [0x00, 0xAA, 0, 0, 0, k as u8];
            r.feed(&mgmt(ACTION, sta, AP, AP, &[4, 4, 1, 81, 1, 0]), 6, 10 + k * 30);
        }
        r.poll(1000);
        let s: Vec<_> = r.out.iter().filter(|s| s.kind == ThreatKind::ChannelSwitch && s.window_ms == 1000).collect();
        assert_eq!(s.len(), 1, "{:?}", r.out);
        assert_eq!(s[0].source, AP);
        assert_eq!(s[0].count, 30);
        assert_eq!(s[0].detail, 1);
    }

    #[test]
    fn tools_reported_once_a_second() {
        let mut r = Run::new();
        for k in 0..25 {
            r.feed(&mgmt(BEACON, BROADCAST, tools::PWNAGOTCHI_ADDR, AP, &[0; 12]), 1, k * 100);
        }
        assert_eq!(r.kinds(), [ThreatKind::AttackTool; 3]);
        assert_eq!(r.out[0].detail, u32::from(tools::PWNAGOTCHI));
        assert_eq!(r.out[0].source, tools::PWNAGOTCHI_ADDR);
        r.out.clear();
        r.feed(&open(AP, b"Pineapple_1F2E"), 6, 5000);
        assert_eq!(r.out[0].detail, u32::from(tools::PINEAPPLE));
        assert_eq!(r.out[0].ssid.as_str(), "Pineapple_1F2E");
    }

    #[test]
    fn totals() {
        let mut r = Run::new();
        r.feed(&wpa2(AP, b"x"), 6, 0);
        r.feed(&kill(12, AP, AP, AP, 1), 6, 0);
        r.feed(&beacon(AP, b"Net", true, &ie(60, &[1, 81, 0, 1])), 6, 0);
        assert_eq!(r.b.totals, Totals { frames: 2, csa: 1, threats: 1 });
    }
}
