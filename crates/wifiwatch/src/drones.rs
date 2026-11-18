//! Wi-Fi drone hijacking: someone kicking a toy drone's pilot off the
//! drone's own Wi-Fi and taking the controls, or copying the drone's network
//! to catch the pilot's phone (docs/DRONE-ATTACKS.md, "Wi-Fi drone
//! hijacking").
//!
//! Many cheap drones (Parrot AR.Drone and Bebop, DJI Tello and Spark, the
//! "WIFI-UFO" toys) run an access point that the pilot's phone joins. The
//! published hijacks (SkyJack, DroneJack, Drone-Hacking-Tool, DroneSploit,
//! Hack-a-drone, the Drone Wars contest) all do the same thing: find the
//! drone's network, deauthenticate the phone (aireplay-ng `-0`), join the
//! network themselves and send flight commands. A copy of the network
//! (Wifiphisher-style evil twin) catches the phone instead.
//!
//! Two halves, like the rest of the crate:
//!
//! - [`DroneLinks`] follows each drone network a node has recognised (at most
//!   [`NETS`]): who its controller is, deauthentications aimed at it, and
//!   stations joining it. It reads only frame headers (plus a reason or
//!   status code) and data-frame addresses, so a node can feed it from the
//!   receive callback next to [`crate::floods::Floods`]. With no drone known
//!   it returns on the first compare. It reports
//!   [`ThreatKind::DroneDeauth`] and [`ThreatKind::DroneTakeover`] from
//!   [`DroneLinks::poll`].
//! - [`Clones`] lives in [`crate::beacons::Beacons`] and looks at beacons and
//!   probe responses: [`ThreatKind::DroneNetClone`]. It also tells the
//!   caller which beacons are drones' ([`Heard`]), which is how
//!   [`DroneLinks`] learns them ([`DroneLinks::learn`]).
//!
//! # What a node can see
//!
//! The promiscuous filter passes every management frame (authentication,
//! (re)association requests **and responses**, deauth / disassoc) and, on
//! node-wifi (`chip::OPEN_LINKS`), data frames. Data frames are what make a
//! controller visible after it joined: a phone flying a drone sends commands
//! tens of times a second and receives video. Without them the node only
//! learns a controller from the moment it joins (or from a deauth aimed at
//! it), and "two controllers at once" can only be noticed while both join.
//! Control frames are off, and encrypted payloads are never read: only
//! addresses.
//!
//! # Numbers (beyond `link::ThreatKind`'s doc)
//!
//! Every kind: `detail >> 24` = the maker (`wifidrone::MAKERS` index),
//! `ssid` = the drone's network name.
//!
//! - `DroneDeauth`: `detail & 0xFFFF` = most common reason code; bit 16
//!   ([`crate::floods::DETAIL_BROADCAST`]) some went to broadcast; bit 17
//!   ([`crate::floods::DETAIL_PROTECTED`]) all protected; bit 18
//!   ([`KILL_DISASSOC`]) some were disassociations; bit 19
//!   ([`KILL_CONTROLLER`]) the drone's known controller was hit. `source` =
//!   claimed transmitter (latest), `target` = drone BSSID, `count` = fresh
//!   frames since the last report.
//! - `DroneTakeover`: `detail` bits [`TAKE_AUTH`] .. [`TAKE_TWO_CONTROLLERS`]
//!   say what was seen of the newcomer and why it is suspicious. `source` =
//!   the newcomer, `target` = drone BSSID, `count` = its frames since the
//!   last report, `window_ms` = since its first frame.
//! - `DroneNetClone`: `detail` bits [`CLONE_OPEN`] .. [`CLONE_OTHER_CHANNEL`].
//!   `source` = the copy, `target` = the original (all zero if the node never
//!   heard it), `count` = frames from the copy.

use crate::mac::{self, ASSOC_REQ, ASSOC_RESP, AUTH, BssInfo, DEAUTH, DISASSOC, REASSOC_REQ, REASSOC_RESP, Security};
use crate::util::{BROADCAST, Dups, Ms, NONE, RateLimit, elapsed, hash, ms};
use crate::{ThreatKind, ThreatSighting, floods, sighting};

/// Drone networks followed at once. A node rarely has more than one or two
/// Wi-Fi drones in range; the stalest one not being reported makes room.
pub const NETS: usize = 3;

/// Deauth / disassoc frames aimed at one drone network within
/// [`KILL_WINDOW_MS`] that make a [`ThreatKind::DroneDeauth`], counting a
/// retransmission (Retry bit and the transmitter's last sequence number,
/// [`Dups`]) once. A toy drone has one controller: a phone leaving sends one
/// deauth or disassoc (two at most, one of each), the drone kicking an idle
/// phone one (its retransmissions are not counted). aireplay-ng `-0 1`
/// alone sends 128. A Retry-bit frame with a new sequence number is a new
/// frame to the phone, and counts.
pub const KILL_FRAMES: usize = 3;
pub const KILL_WINDOW_MS: Ms = 5000;
/// The attack is over when no counted frame came for this long.
pub const KILL_QUIET_MS: Ms = 5000;
/// A drone that started less than this long ago (its beacon timestamp)
/// legitimately sends reason 6 / 7 deauths ("frame from a station that is not
/// joined") to the phone that kept talking to it across the reboot. Those
/// are not counted.
pub const BOOT_GRACE_MS: Ms = 60_000;
/// A beacon timestamp that puts the drone's start this much later than
/// before means it restarted: its controller has to join again, so the
/// controller and any newcomer are forgotten.
pub const REBOOT_JUMP_MS: Ms = 2000;

/// A station joining within this long after a [`ThreatKind::DroneDeauth`]
/// on the drone (and not its controller) is a takeover.
pub const KICK_RECENT_MS: Ms = 30_000;
/// ...or within this long after the controller's own single deauth /
/// disassoc (a one-frame kick races the pilot's phone, which rejoins in a
/// second or two).
pub const LEAVE_RACE_MS: Ms = 5000;
/// A controller heard (data, association) within this long is still flying
/// the drone: a second station joining then is a takeover.
pub const CTRL_ACTIVE_MS: Ms = 10_000;
/// Frames from the newcomer before it is reported (authentication then
/// association, or a couple of data frames): one stray frame is not enough.
pub const TAKEOVER_FRAMES: u16 = 2;
/// A newcomer not heard for this long is forgotten (the takeover ended).
pub const TAKEOVER_QUIET_MS: Ms = 10_000;
/// A drone network not heard (beacons) for this long is forgotten, unless
/// something is being reported on it.
pub const FORGET_MS: Ms = 120_000;
/// Shortest gap between two reports of one kind for one drone.
pub const REPORT_GAP_MS: Ms = 1000;

/// `DroneDeauth` detail: some frames were disassociations.
pub const KILL_DISASSOC: u32 = 1 << 18;
/// `DroneDeauth` detail: a frame hit the drone's known controller.
pub const KILL_CONTROLLER: u32 = 1 << 19;

/// `DroneTakeover` detail: the newcomer sent an authentication request.
pub const TAKE_AUTH: u32 = 1 << 0;
/// ...an association or reassociation request.
pub const TAKE_ASSOC: u32 = 1 << 1;
/// ...and the drone accepted it (association response, status 0).
pub const TAKE_ACCEPTED: u32 = 1 << 2;
/// ...and is exchanging data with the drone (it is flying it).
pub const TAKE_DATA: u32 = 1 << 3;
/// It came within [`KICK_RECENT_MS`] of a `DroneDeauth` on this drone.
pub const TAKE_AFTER_KICK: u32 = 1 << 4;
/// It came within [`LEAVE_RACE_MS`] of the controller's single deauth.
pub const TAKE_AFTER_LEAVE: u32 = 1 << 5;
/// The previous controller was still active (two controllers at once).
pub const TAKE_TWO_CONTROLLERS: u32 = 1 << 6;

/// `DroneNetClone` detail: the copy is an open network.
pub const CLONE_OPEN: u32 = 1 << 0;
/// The copy's security differs from the original's.
pub const CLONE_SECURITY: u32 = 1 << 1;
/// The copy's address is not in the maker's registered blocks.
pub const CLONE_WRONG_MAKER: u32 = 1 << 2;
/// The copy uses the original's own address, on another channel.
pub const CLONE_SAME_ADDRESS: u32 = 1 << 3;
/// The copy says it is on another channel than the original.
pub const CLONE_OTHER_CHANNEL: u32 = 1 << 4;
/// Bit offset of the maker index in every drone kind's `detail`.
pub const MAKER_SHIFT: u32 = 24;

/// Frames from the original before its name is "established".
pub const CLONE_ESTABLISHED: u16 = 5;
/// Frames from a second address before it is reported.
pub const CLONE_FRAMES: u16 = 2;
/// Frames from an address the maker does not own before it is reported
/// without an original heard (the drone may be out of the node's range).
pub const CLONE_WRONG_MAKER_FRAMES: u16 = 3;
/// The original must have been heard this recently; a drone not heard for
/// longer may have moved or restarted, and the next address with its name
/// takes over as the original.
pub const CLONE_RECENT_MS: Ms = 30_000;
/// Same address on two channels: the original must have been heard on its
/// channel again within this long after the other channel appeared. A
/// drone that changes channel stops on the old one.
pub const CLONE_INTERLEAVE_MS: Ms = 10_000;
/// Network names compared between addresses must end in at least this many
/// hex digits: a per-unit suffix. Two toys of one model sold with one
/// fixed name ("WiFi-720P") would otherwise look like copies of each other.
pub const UNIT_SUFFIX: usize = 4;
/// Drone names followed by [`Clones`].
pub const CLONE_NAMES: usize = 4;

/// A beacon or probe response from a drone's own network (by
/// `wifidrone::classify`), for [`DroneLinks::learn`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Heard<'a> {
    pub bssid: [u8; 6],
    /// `wifidrone::MAKERS` index.
    pub maker: u8,
    pub ssid: &'a [u8],
    /// Channel it was heard on.
    pub channel: u8,
    /// The beacon's timestamp (the drone's radio uptime), µs; 0 if unknown.
    pub tsf_us: u64,
}

const fn group(a: &[u8; 6]) -> bool {
    a[0] & 0x01 != 0
}

#[derive(Clone, Copy, Debug)]
struct Net {
    bssid: [u8; 6],
    ssid: [u8; 32],
    ssid_len: u8,
    maker: u8,
    channel: u8,
    /// Bit set: [`F_BOOTED`] etc.
    flags: u8,
    /// Last beacon (node ms).
    seen: Ms,
    /// The drone's start, node ms (from its beacon timestamp).
    boot: Ms,

    /// The station flying it, `NONE` if not known.
    ctrl: [u8; 6],
    ctrl_last: Ms,
    left_at: Ms,

    /// Times of the last [`KILL_FRAMES`] counted frames; `ring_head` is the
    /// next slot to write (the oldest once full).
    ring: [Ms; KILL_FRAMES],
    ring_head: u8,
    ring_len: u8,
    /// Last counted frame.
    k_last: Ms,
    kicked_at: Ms,
    k_frames: u16,
    k_reason: u16,
    k_votes: u16,
    k_prot: u16,
    k_first: Ms,
    k_reported: Ms,
    /// `detail` bits seen in this report's frames (broadcast, disassoc,
    /// controller).
    k_bits: u32,
    k_src: [u8; 6],
    k_rssi: i8,
    k_ch: u8,
    /// Last sequence number of the drone and of one station.
    dups: Dups<2>,

    /// The newcomer.
    cand: [u8; 6],
    cand_n: u16,
    cand_reported_n: u16,
    cand_bits: u8,
    cand_rssi: i8,
    cand_ch: u8,
    cand_first: Ms,
    cand_last: Ms,
    t_reported: Ms,
}

/// The beacon timestamp gave the drone's start.
const F_BOOTED: u8 = 1 << 0;
/// The controller sent (or was sent) a deauth / disassoc: it may be leaving.
const F_LEFT: u8 = 1 << 1;
/// A `DroneDeauth` is on (enough frames in the window, not yet quiet).
const F_ATTACK: u8 = 1 << 2;
/// There was a `DroneDeauth` at `kicked_at`.
const F_KICKED: u8 = 1 << 3;
/// The current `DroneDeauth` has been reported at least once.
const F_K_REPORTED: u8 = 1 << 4;
/// The newcomer has enough frames: a `DroneTakeover` is on.
const F_TAKEN: u8 = 1 << 5;

impl Net {
    fn new(bssid: [u8; 6], now: Ms) -> Self {
        Self {
            bssid,
            ssid: [0; 32],
            ssid_len: 0,
            maker: 0,
            channel: 0,
            flags: 0,
            seen: now,
            boot: now,
            ctrl: NONE,
            ctrl_last: now,
            left_at: now,
            ring: [0; KILL_FRAMES],
            ring_head: 0,
            ring_len: 0,
            k_last: now,
            kicked_at: now,
            k_frames: 0,
            k_reason: 0,
            k_votes: 0,
            k_prot: 0,
            k_first: now,
            k_reported: now,
            k_bits: 0,
            k_src: NONE,
            k_rssi: i8::MIN,
            k_ch: 0,
            dups: Dups::new(),
            cand: NONE,
            cand_n: 0,
            cand_reported_n: 0,
            cand_bits: 0,
            cand_rssi: i8::MIN,
            cand_ch: 0,
            cand_first: now,
            cand_last: now,
            t_reported: now,
        }
    }

    fn has(&self, f: u8) -> bool {
        self.flags & f != 0
    }

    fn set(&mut self, f: u8, on: bool) {
        if on {
            self.flags |= f;
        } else {
            self.flags &= !f;
        }
    }

    fn busy(&self) -> bool {
        self.has(F_ATTACK) || self.has(F_TAKEN)
    }

    fn ssid(&self) -> &[u8] {
        &self.ssid[..usize::from(self.ssid_len)]
    }

    fn maker_bits(&self) -> u32 {
        u32::from(self.maker) << MAKER_SHIFT
    }

    /// Forget who is attached (the drone restarted).
    fn reset_link(&mut self, now: Ms) {
        self.flags &= F_BOOTED;
        self.ctrl = NONE;
        self.ring_len = 0;
        self.clear_kills(now);
        self.cand = NONE;
        self.cand_n = 0;
        self.cand_reported_n = 0;
    }

    fn clear_kills(&mut self, now: Ms) {
        self.k_frames = 0;
        self.k_votes = 0;
        self.k_prot = 0;
        self.k_bits = 0;
        self.k_rssi = i8::MIN;
        self.k_first = now;
    }

    fn kill(&mut self, h: &mac::Header, reason: u16, rssi: i8, channel: u8, now: Ms) {
        // The station side of the frame (the AP side is the drone).
        let sta = if h.addr2 == self.bssid { h.addr1 } else { h.addr2 };
        let hits_ctrl = self.ctrl != NONE && (sta == self.ctrl || sta == BROADCAST);
        if self.ctrl == NONE && sta != BROADCAST && !group(&sta) && sta != NONE {
            // The station a deauth is about was joined: the controller.
            self.ctrl = sta;
            self.ctrl_last = now;
        }
        if sta == self.ctrl || hits_ctrl {
            self.set(F_LEFT, true);
            self.left_at = now;
        }
        // A retransmission is discarded by the receiver; a Retry-bit frame
        // with a new number is not, and counts.
        if self.dups.duplicate(&h.addr2, h.seq, h.retry) {
            return;
        }
        // A drone that just restarted tells the phone it does not know it.
        if h.addr2 == self.bssid
            && matches!(reason, 6 | 7)
            && self.has(F_BOOTED)
            && elapsed(now, self.boot) < BOOT_GRACE_MS
        {
            return;
        }
        if self.k_frames == 0 {
            self.k_first = now;
        }
        self.k_frames = self.k_frames.saturating_add(1);
        if h.addr1 == BROADCAST {
            self.k_bits |= floods::DETAIL_BROADCAST;
        }
        if h.subtype == DISASSOC {
            self.k_bits |= KILL_DISASSOC;
        }
        if hits_ctrl || sta == self.ctrl {
            self.k_bits |= KILL_CONTROLLER;
        }
        if h.protected {
            self.k_prot = self.k_prot.saturating_add(1);
        }
        if self.k_votes == 0 {
            self.k_reason = reason;
            self.k_votes = 1;
        } else if self.k_reason == reason {
            self.k_votes = self.k_votes.saturating_add(1);
        } else {
            self.k_votes -= 1;
        }
        self.k_src = h.addr2;
        self.k_rssi = self.k_rssi.max(rssi);
        self.k_ch = channel;
        self.k_last = now;
        let i = usize::from(self.ring_head);
        self.ring[i] = now;
        let next = (i + 1) % KILL_FRAMES;
        self.ring_head = next as u8;
        self.ring_len = (self.ring_len + 1).min(KILL_FRAMES as u8);
        // The oldest of the last KILL_FRAMES (this one included).
        if usize::from(self.ring_len) == KILL_FRAMES && elapsed(now, self.ring[next]) <= KILL_WINDOW_MS {
            self.set(F_ATTACK, true);
            self.set(F_KICKED, true);
            self.kicked_at = now;
        }
    }

    /// A frame showing `sta` joining or attached to the drone.
    fn station(&mut self, sta: [u8; 6], bit: u32, rssi: i8, channel: u8, now: Ms) {
        if sta == self.bssid || sta == NONE || group(&sta) {
            return;
        }
        if sta == self.ctrl {
            self.ctrl_last = now;
            self.set(F_LEFT, false);
            return;
        }
        if self.ctrl == NONE {
            self.ctrl = sta;
            self.ctrl_last = now;
            return;
        }
        let kicked = self.has(F_KICKED) && elapsed(now, self.kicked_at) <= KICK_RECENT_MS;
        let left = self.has(F_LEFT) && elapsed(now, self.left_at) <= LEAVE_RACE_MS;
        let active = !self.has(F_LEFT) && elapsed(now, self.ctrl_last) <= CTRL_ACTIVE_MS;
        if !(kicked || left || active) && sta != self.cand {
            // The controller has gone quietly (or is long gone): a handover,
            // or the pilot back with a new address.
            self.ctrl = sta;
            self.ctrl_last = now;
            self.set(F_LEFT, false);
            return;
        }
        if sta != self.cand {
            self.cand = sta;
            self.cand_n = 0;
            self.cand_reported_n = 0;
            self.cand_bits = 0;
            self.cand_rssi = i8::MIN;
            self.cand_first = now;
            self.set(F_TAKEN, false);
        }
        let mut bits = bit;
        if kicked {
            bits |= TAKE_AFTER_KICK;
        }
        if left && !kicked {
            bits |= TAKE_AFTER_LEAVE;
        }
        if active {
            bits |= TAKE_TWO_CONTROLLERS;
        }
        self.cand_bits |= bits as u8;
        self.cand_n = self.cand_n.saturating_add(1);
        self.cand_last = now;
        self.cand_rssi = self.cand_rssi.max(rssi);
        self.cand_ch = channel;
        if self.cand_n >= TAKEOVER_FRAMES {
            self.set(F_TAKEN, true);
        }
    }
}

/// Running totals since boot, for a node's stats line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    /// Drone networks learned (new entries).
    pub learned: u32,
    /// Sightings reported.
    pub threats: u32,
}

/// Deauth and takeover watch over the drone networks the node knows. See
/// the module docs.
pub struct DroneLinks {
    nets: heapless::Vec<Net, NETS>,
    pub totals: Totals,
}

impl Default for DroneLinks {
    fn default() -> Self {
        Self::new()
    }
}

impl DroneLinks {
    pub const fn new() -> Self {
        Self { nets: heapless::Vec::new(), totals: Totals { learned: 0, threats: 0 } }
    }

    /// Drone networks being followed (a node can skip [`Self::data`] and
    /// [`Self::frame`] altogether while this is 0).
    pub fn len(&self) -> usize {
        self.nets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nets.is_empty()
    }

    fn find(&mut self, bssid: &[u8; 6]) -> Option<&mut Net> {
        self.nets.iter_mut().find(|n| n.bssid == *bssid)
    }

    /// A drone's beacon or probe response (from [`Clones`], via
    /// `Beacons::frame`). Call for every one; cheap.
    pub fn learn(&mut self, d: &Heard<'_>, now_us: u64) {
        let now = ms(now_us);
        let i = match self.nets.iter().position(|n| n.bssid == d.bssid) {
            Some(i) => i,
            None => {
                if self.nets.is_full() {
                    // The stalest one not mid-report; all busy: keep them.
                    let Some(v) = (0..self.nets.len())
                        .filter(|&i| !self.nets[i].busy())
                        .max_by_key(|&i| elapsed(now, self.nets[i].seen))
                    else {
                        return;
                    };
                    self.nets.swap_remove(v);
                }
                self.totals.learned = self.totals.learned.wrapping_add(1);
                // Cannot fail: room was made above.
                let _ = self.nets.push(Net::new(d.bssid, now));
                self.nets.len() - 1
            }
        };
        let n = &mut self.nets[i];
        let len = d.ssid.len().min(32);
        n.ssid[..len].copy_from_slice(&d.ssid[..len]);
        n.ssid_len = len as u8;
        n.maker = d.maker;
        n.channel = d.channel;
        n.seen = now;
        if d.tsf_us > 0 {
            let boot = now.wrapping_sub((d.tsf_us / 1000) as Ms);
            let later = boot.wrapping_sub(n.boot) as i32;
            if n.has(F_BOOTED) && later > REBOOT_JUMP_MS as i32 {
                n.reset_link(now);
            }
            n.boot = boot;
            n.set(F_BOOTED, true);
        }
    }

    /// One management frame (MPDU, FCS or not). Only the
    /// [`mac::drone_link`] subtypes are looked at; anything else returns at
    /// once. Fit for a receive callback.
    pub fn frame(&mut self, mpdu: &[u8], rssi: i8, channel: u8, now_us: u64) {
        if self.nets.is_empty() {
            return;
        }
        let Some(&fc0) = mpdu.first() else { return };
        if fc0 & 0x0F != 0 || !mac::drone_link(fc0 >> 4) {
            return;
        }
        let Some(h) = mac::header(mpdu) else { return };
        let now = ms(now_us);
        let body = |at: usize| match (h.protected, mpdu.get(h.body_at + at..h.body_at + at + 2)) {
            (false, Some(b)) => Some(u16::from_le_bytes([b[0], b[1]])),
            _ => None,
        };
        match h.subtype {
            DEAUTH | DISASSOC => {
                let Some(n) =
                    self.nets.iter_mut().find(|n| n.bssid == h.addr3 || n.bssid == h.addr1 || n.bssid == h.addr2)
                else {
                    return;
                };
                n.kill(&h, body(0).unwrap_or(0), rssi, channel, now);
            }
            AUTH => {
                // Requests only: station -> access point.
                if h.addr1 == h.addr3
                    && h.addr2 != h.addr3
                    && let Some(n) = self.find(&h.addr1)
                {
                    n.station(h.addr2, TAKE_AUTH, rssi, channel, now);
                }
            }
            ASSOC_REQ | REASSOC_REQ => {
                if let Some(n) = self.find(&h.addr1) {
                    n.station(h.addr2, TAKE_ASSOC, rssi, channel, now);
                }
            }
            ASSOC_RESP | REASSOC_RESP => {
                // Capability (2), status (2): 0 = accepted.
                if body(2) == Some(0)
                    && let Some(n) = self.find(&h.addr2)
                {
                    n.station(h.addr1, TAKE_ACCEPTED, rssi, channel, now);
                }
            }
            _ => {}
        }
    }

    /// One data frame (MPDU, FCS or not): which station is talking to which
    /// drone. Anything that is not a unicast station <-> access point data
    /// frame of a known drone returns at once.
    pub fn data(&mut self, mpdu: &[u8], rssi: i8, channel: u8, now_us: u64) {
        if self.nets.is_empty() {
            return;
        }
        let Some((bssid, sta, _)) = mac::data_link(mpdu) else { return };
        if let Some(n) = self.find(&bssid) {
            n.station(sta, TAKE_DATA, rssi, channel, ms(now_us));
        }
    }

    /// Report what is due and forget what is stale. Call a few times a
    /// second. At most two sightings per drone network.
    pub fn poll(&mut self, now_us: u64, emit: &mut impl FnMut(ThreatSighting)) {
        let now = ms(now_us);
        let mut sent = 0u32;
        for n in self.nets.iter_mut() {
            // Deauth.
            let since_kill = elapsed(now, n.k_last);
            if n.has(F_ATTACK) {
                let due = !n.has(F_K_REPORTED) || elapsed(now, n.k_reported) >= REPORT_GAP_MS;
                if n.k_frames > 0 && due {
                    let mut detail = u32::from(n.k_reason) | n.k_bits;
                    if n.k_prot == n.k_frames {
                        detail = floods::DETAIL_PROTECTED | (n.k_bits & !0xFFFF);
                    }
                    let window = elapsed(now, n.k_first);
                    emit(sighting(
                        ThreatKind::DroneDeauth,
                        n.k_ch,
                        n.k_rssi,
                        n.k_frames.into(),
                        window,
                        n.k_src,
                        n.bssid,
                        detail | n.maker_bits(),
                        n.ssid(),
                        now_us,
                    ));
                    sent += 1;
                    n.k_reported = now;
                    n.set(F_K_REPORTED, true);
                    n.clear_kills(now);
                } else if since_kill > KILL_QUIET_MS {
                    n.set(F_ATTACK, false);
                    n.set(F_K_REPORTED, false);
                    n.clear_kills(now);
                }
            } else if n.k_frames > 0 && since_kill > KILL_WINDOW_MS {
                n.clear_kills(now);
            }

            // Takeover.
            if n.cand != NONE && elapsed(now, n.cand_last) > TAKEOVER_QUIET_MS {
                n.cand = NONE;
                n.cand_n = 0;
                n.cand_reported_n = 0;
                n.set(F_TAKEN, false);
            }
            let fresh = n.cand_n.wrapping_sub(n.cand_reported_n);
            let due = n.cand_reported_n == 0 || elapsed(now, n.t_reported) >= REPORT_GAP_MS;
            if n.has(F_TAKEN) && fresh > 0 && due {
                emit(sighting(
                    ThreatKind::DroneTakeover,
                    n.cand_ch,
                    n.cand_rssi,
                    fresh.into(),
                    elapsed(now, n.cand_first),
                    n.cand,
                    n.bssid,
                    u32::from(n.cand_bits) | n.maker_bits(),
                    n.ssid(),
                    now_us,
                ));
                sent += 1;
                n.t_reported = now;
                n.cand_reported_n = n.cand_n;
            }
        }
        self.nets.retain(|n| n.busy() || elapsed(now, n.seen) <= FORGET_MS);
        self.totals.threats = self.totals.threats.wrapping_add(sent);
    }
}

// ------------------------------------------------------------- clones --

#[derive(Clone, Copy, Debug)]
struct Name {
    /// Hash of the network name; 0 = free slot.
    name: u32,
    maker: u8,
    orig_ch: u8,
    orig_sec: u8,
    copy_ch: u8,
    copy_sec: u8,
    wrong: bool,
    orig_frames: u16,
    copy_frames: u16,
    orig: [u8; 6],
    copy: [u8; 6],
    orig_last: Ms,
    copy_first: Ms,
    copy_last: Ms,
}

impl Name {
    const FREE: Self = Self {
        name: 0,
        maker: 0,
        orig_ch: 0,
        orig_sec: 0,
        copy_ch: 0,
        copy_sec: 0,
        wrong: false,
        orig_frames: 0,
        copy_frames: 0,
        orig: NONE,
        copy: NONE,
        orig_last: 0,
        copy_first: 0,
        copy_last: 0,
    };

    fn set_orig(&mut self, bssid: [u8; 6], ch: u8, sec: u8, now: Ms) {
        self.orig = bssid;
        self.orig_ch = ch;
        self.orig_sec = sec;
        self.orig_frames = 0;
        self.orig_last = now;
        if self.copy == bssid {
            self.copy = NONE;
            self.copy_frames = 0;
        }
    }

    fn copy_frame(&mut self, bssid: [u8; 6], ch: u8, sec: u8, wrong: bool, now: Ms) {
        if self.copy != bssid || self.copy_ch != ch || elapsed(now, self.copy_last) > CLONE_RECENT_MS {
            self.copy = bssid;
            self.copy_ch = ch;
            self.copy_frames = 0;
            self.copy_first = now;
        }
        self.copy_sec = sec;
        self.wrong = wrong;
        self.copy_frames = self.copy_frames.saturating_add(1);
        self.copy_last = now;
    }
}

/// A name ending in at least [`UNIT_SUFFIX`] hex digits (a per-unit name).
fn unit_name(ssid: &[u8]) -> bool {
    ssid.iter().rev().take_while(|b| b.is_ascii_hexdigit()).count() >= UNIT_SUFFIX
}

/// A maker's name pattern specific enough to accuse an address of not
/// being the maker's: a fixed number of hex or decimal digits after it.
fn strong(p: &wifidrone::Pattern) -> bool {
    matches!(p.tail, wifidrone::Tail::Hex(_) | wifidrone::Tail::Digits(_))
}

/// One drone's two radios (2.4 and 5 GHz), or its own second network, get
/// neighbouring addresses: the same apart from the locally administered bit
/// of the first byte and a small step in the last.
fn sibling(a: &[u8; 6], b: &[u8; 6]) -> bool {
    (a[0] | 0x02) == (b[0] | 0x02) && a[1..5] == b[1..5] && a[5].abs_diff(b[5]) <= 8
}

/// Copies of drone networks, from beacons and probe responses. Lives in
/// [`crate::beacons::Beacons`]. See the module docs.
pub struct Clones {
    names: [Name; CLONE_NAMES],
}

impl Default for Clones {
    fn default() -> Self {
        Self::new()
    }
}

impl Clones {
    pub const fn new() -> Self {
        Self { names: [Name::FREE; CLONE_NAMES] }
    }

    /// One beacon or probe response. Returns the drone it is from when
    /// `wifidrone` says it is one (with an address that fits the maker), for
    /// [`DroneLinks::learn`].
    #[allow(clippy::too_many_arguments, reason = "internal; all from one frame")]
    pub(crate) fn bss<'a>(
        &mut self,
        bssid: [u8; 6],
        info: &BssInfo<'a>,
        rssi: i8,
        channel: u8,
        now: Ms,
        now_us: u64,
        limit: &mut RateLimit<16>,
        emit: &mut impl FnMut(ThreatSighting),
    ) -> Option<Heard<'a>> {
        let ssid = info.ssid;
        let genuine = wifidrone::classify(&bssid, ssid);
        let (maker, wrong) = match genuine {
            Some(m) => (m, false),
            None => match wifidrone::name_maker(ssid) {
                Some((m, p)) if strong(p) && wifidrone::has_prefixes(m) && !wifidrone::has_prefix(m, &bssid) => {
                    (m, true)
                }
                _ => return None,
            },
        };
        let heard = genuine.map(|maker| Heard { bssid, maker, ssid, channel, tsf_us: info.tsf });
        if !wrong && !unit_name(ssid) {
            return heard;
        }
        let h = hash(ssid).max(1);
        let i = match self.names.iter().position(|n| n.name == h) {
            Some(i) => i,
            None => {
                // A free slot, else the one heard least recently.
                let last = |n: &Name| if n.orig_last.wrapping_sub(n.copy_last) as i32 > 0 { n.orig_last } else { n.copy_last };
                let v = self
                    .names
                    .iter()
                    .position(|n| n.name == 0)
                    .or_else(|| (0..CLONE_NAMES).max_by_key(|&i| elapsed(now, last(&self.names[i]))))
                    .unwrap_or(0);
                self.names[v] = Name { name: h, maker, ..Name::FREE };
                v
            }
        };
        let n = &mut self.names[i];
        n.maker = maker;
        let ch = info.ds_channel.unwrap_or(0);
        let sec = info.security as u8;
        let orig_recent = n.orig != NONE && elapsed(now, n.orig_last) <= CLONE_RECENT_MS;

        if !wrong && n.orig != bssid && !orig_recent {
            // No original (or it fell silent): this one is it now.
            n.set_orig(bssid, ch, sec, now);
        }
        if n.orig == bssid {
            if ch != 0 && n.orig_ch != 0 && ch != n.orig_ch {
                // Same address, another channel: a copy using the drone's
                // own address, or the drone moved (then the old channel
                // falls silent).
                if elapsed(now, n.orig_last) > CLONE_INTERLEAVE_MS {
                    n.orig_ch = ch;
                    n.orig_last = now;
                    return heard;
                }
                n.copy_frame(bssid, ch, sec, false, now);
                let interleaved = n.orig_last.wrapping_sub(n.copy_first) as i32 > 0;
                if n.copy_frames >= CLONE_FRAMES && interleaved {
                    let detail = CLONE_SAME_ADDRESS | CLONE_OTHER_CHANNEL | security_bits(n.orig_sec, sec);
                    report(n, detail, rssi, channel, ssid, now, now_us, limit, emit);
                }
                return heard;
            }
            n.orig_frames = n.orig_frames.saturating_add(1);
            n.orig_last = now;
            n.orig_sec = sec;
            if ch != 0 {
                n.orig_ch = ch;
            }
            return heard;
        }
        // A second address with the name.
        if !wrong && n.orig != NONE && sibling(&n.orig, &bssid) {
            return heard;
        }
        n.copy_frame(bssid, ch, sec, wrong, now);
        let established = orig_recent && n.orig_frames >= CLONE_ESTABLISHED;
        let due = (established && n.copy_frames >= CLONE_FRAMES) || (wrong && n.copy_frames >= CLONE_WRONG_MAKER_FRAMES);
        if due {
            let mut detail = if established { security_bits(n.orig_sec, sec) } else { open_bit(sec) };
            if wrong {
                detail |= CLONE_WRONG_MAKER;
            }
            if established && ch != 0 && n.orig_ch != 0 && ch != n.orig_ch {
                detail |= CLONE_OTHER_CHANNEL;
            }
            report(n, detail, rssi, channel, ssid, now, now_us, limit, emit);
        }
        heard
    }
}

fn open_bit(sec: u8) -> u32 {
    if sec == Security::Open as u8 { CLONE_OPEN } else { 0 }
}

fn security_bits(orig: u8, copy: u8) -> u32 {
    open_bit(copy) | if orig != copy { CLONE_SECURITY } else { 0 }
}

#[allow(clippy::too_many_arguments, reason = "internal; all from one frame")]
fn report(
    n: &Name,
    detail: u32,
    rssi: i8,
    channel: u8,
    ssid: &[u8],
    now: Ms,
    now_us: u64,
    limit: &mut RateLimit<16>,
    emit: &mut impl FnMut(ThreatSighting),
) {
    if !limit.allow(ThreatKind::DroneNetClone as u8, n.copy, now) {
        return;
    }
    let target = if n.orig != NONE && elapsed(now, n.orig_last) <= CLONE_RECENT_MS { n.orig } else { NONE };
    emit(sighting(
        ThreatKind::DroneNetClone,
        channel,
        rssi,
        n.copy_frames.into(),
        elapsed(now, n.copy_first),
        n.copy,
        target,
        detail | (u32::from(n.maker) << MAKER_SHIFT),
        ssid,
        now_us,
    ));
}

#[cfg(test)]
mod tests;
