//! The ESP-NOW link logic every node image shares (docs/LINK-KEYS-DESIGN.md
//! sections 4.4, 6, 7.3 and 7.4; package 13g). Pure: the clock, the radio's
//! source address and the received bytes are passed in, so it is tested on
//! the host against a real `airlink::v3::Hub`. The radio itself (peers,
//! sends, channels) stays in each image's `uplink.rs`; the flash and TRNG
//! glue is `firmware/node-common` (`keystore`, `linkauth`).
//!
//! # What the node does, by provisioning state
//!
//! | Record in `rskey` | [`Mode`] | Over ESP-NOW |
//! |---|---|---|
//! | keyed, this board's MAC, TRNG up | [`Mode::Keyed`] | version 3: batches sealed with `K_up` ([`Link::packet`], fresh stamp per attempt); only Sync / Command / Beacon that [`airlink::v3::Node::accept`] takes are believed |
//! | open | [`Mode::Open`] | version 1, unauthenticated, as an open kit always was |
//! | missing, damaged, wrong board; or keyed without a TRNG | [`Mode::Blocked`] | **no reports**. Every [`ANNOUNCE_MS`]: a USB warning naming the reason and one unsealed `Unprovisioned` announcement ([`Link::announce`]) |
//!
//! A keyed node whose hardware random source is not running gets no session
//! (a session that repeats across boots would let a command recorded before
//! a reboot work after it), so it is blocked with reason `no_rng`, and its
//! announcements carry `<firmware> norng`: the only way it can say so beyond
//! USB, since without the keyed link no Hello reaches the hub.
//!
//! # Receive ([`Link::receive`])
//!
//! Keyed: [`airlink::v3::inspect`], then `Node::accept` with
//! `timesync = true` when the Command holds a `link::Command::TimeSync`
//! ([`has_timesync`]; the strictest rule then covers the whole packet).
//! Other nodes' batches (kind Frames, any version) and announcements are
//! not for us and are not counted. Everything else that is not taken is
//! counted by reason ([`Counts::rejects`]), version 1 as `unauthenticated`
//! and version 2 as `old_firmware`. A genuine packet refused only for its
//! epoch (`Stale`, `Unproven`) still shows the gateway is there
//! ([`airlink::v3::NodeVerdict::proves_gateway`]): the node keeps sending to
//! it, which earns the Sync that puts it right.
//!
//! Open: version 1 beacons and commands, as before; a version 2 command or
//! beacon is counted as `old_firmware`.
//!
//! [`Counts::heard`] counts the gateway packets that prove the gateway (taken,
//! repeated, or genuine but refused for their epoch). It is what each image's
//! deaf fall-back (`crate::deaf`) watches. The link itself relinks after 60 s
//! of silence (`airlink::v3::RELINK_MS`); the firmware never resets it.
//!
//! # Send
//!
//! [`Link::packet`] builds the packet for one attempt; [`Link::sent`] and
//! [`Link::given_up`] keep the delivery accounting `link::NodeHealth`
//! carries ([`Link::fill_health`]): attempts, radio-acknowledged batches
//! and their frames, batches given up.

use core::fmt::Write;

use airlink::v3::{self, Kind, Node, NodeKeys, NodeRejects, NodeVerdict, Reject, Verdict};
use kitkeys::Mac;
use kitkeys::record::{AuthSummary, State};
use link::{Command, CommandDecoder, NodeHealth};

/// Forget a gateway neither heard nor reached (radio ACK) for this long.
/// Generous: a node on channel 6 only 40 ms of a second (5 GHz plan) or
/// only in the Bluetooth scan's gaps catches few beacons.
pub const GATEWAY_TIMEOUT_MS: u64 = 30_000;
/// A blocked node warns on USB and announces itself this often.
pub const ANNOUNCE_MS: u64 = 10_000;

/// Image numbers in an `Unprovisioned` announcement (the node's own
/// numbering; 0 = unknown).
pub const IMAGE_WIFI: u8 = 1;
pub const IMAGE_BLE: u8 = 2;
pub const IMAGE_SDR: u8 = 3;
pub const IMAGE_LR: u8 = 4;

/// Why a node sends no reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// Both record copies erased (never provisioned), or no `rskey` partition.
    Missing,
    /// No copy with a good CRC.
    Damaged,
    /// A good record made for another board.
    WrongBoard,
    /// Keyed, but the hardware random source is not running.
    NoRng,
}

impl Why {
    pub const fn name(self) -> &'static str {
        match self {
            Why::Missing => "missing",
            Why::Damaged => "damaged",
            Why::WrongBoard => "wrong_board",
            Why::NoRng => "no_rng",
        }
    }

    const fn note(self) -> &'static str {
        match self {
            Why::Missing => "not provisioned: NO reports over ESP-NOW. Provision it: link-key node <board> (or link-key open <board> for an open kit)",
            Why::Damaged => "provisioning record damaged: NO reports over ESP-NOW. Provision it again: link-key node <board> --slot <n>",
            Why::WrongBoard => "provisioning record made for another board: NO reports over ESP-NOW. Provision this board: link-key node <board>",
            Why::NoRng => "hardware random source not running after the radio started: keyed link NOT started, NO reports over ESP-NOW",
        }
    }
}

/// The random source a keyed node's sessions come from, and its name for
/// the boot line (`"trng"`: the HAL's TRNG, checked running; `"radio"`: the
/// hardware RNG of a chip the HAL has no TRNG type for, read only after the
/// radio started, which is what makes it true random).
#[derive(Clone, Copy)]
pub struct RngSource {
    pub next: fn() -> u32,
    pub name: &'static str,
}

/// A cheap health check of a random source before a keyed link trusts it:
/// draws from a stuck or unclocked generator repeat. True when no two
/// neighbouring draws are equal and not all are zero (a working 32-bit
/// source fails this about once in 2^29 boots with 8 draws).
pub fn rng_sane(draws: &[u32]) -> bool {
    draws.len() >= 2 && draws.windows(2).all(|w| w[0] != w[1]) && draws.iter().any(|&d| d != 0)
}

/// How the link runs (module docs).
pub enum Mode {
    Keyed(Node),
    Open,
    Blocked(Why),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Gateway {
    mac: Mac,
    channel: u8,
    seen_ms: u64,
}

/// Running totals since boot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// Gateway packets that prove the gateway: taken, repeated, or genuine
    /// but refused for their epoch. The deaf fall-back watches this.
    pub heard: u32,
    pub beacons: u32,
    pub syncs: u32,
    /// Command packets taken.
    pub commands: u32,
    /// Hub-direction packets not taken, by `airlink::v3::Reject::index`,
    /// and repeats of the last one taken.
    pub rejects: NodeRejects,
    /// Radio sends (each retry is one).
    pub send_attempts: u32,
    /// Batches the radio acknowledged, and the frames in them.
    pub batches_sent: u32,
    pub frames_sent: u32,
    /// Batches dropped after every retry.
    pub batches_given_up: u32,
    /// Frames thrown away unsent because the node is blocked.
    pub not_sent: u32,
    pub announcements: u32,
}

impl Counts {
    /// Sends the radio did not acknowledge.
    pub const fn failed(&self) -> u32 {
        self.send_attempts.wrapping_sub(self.batches_sent)
    }
}

/// One node's link: mode, gateway, counters.
pub struct Link {
    mode: Mode,
    /// The board's eFuse station MAC (announcements).
    me: Mac,
    rng: Option<&'static str>,
    summary: Option<AuthSummary>,
    record_mac: Option<Mac>,
    gateway: Option<Gateway>,
    new_gateway: bool,
    /// Open mode: the last command packet taken (a radio retry repeats it).
    open_last: Option<(Mac, u16)>,
    next_announce_ms: u64,
    announce_seq: u16,
    pub counts: Counts,
}

impl Link {
    /// From what the board found in `rskey` (`kitkeys::record::read`, its
    /// MAC already checked against eFuse) and the hardware random source,
    /// `None` if it is not running. `me` is the eFuse station MAC.
    pub fn new(state: State, me: Mac, rng: Option<RngSource>) -> Self {
        let mut summary = None;
        let mut record_mac = None;
        let mode = match state {
            State::Keyed(rec) => {
                summary = Some(rec.auth_summary());
                match rng {
                    // The record's MAC is the eFuse MAC (checked by `read`):
                    // the address the radio sends from.
                    Some(r) => Mode::Keyed(Node::new(NodeKeys::new(rec.ident.slot, rec.ident.mac, &rec.key), r.next)),
                    None => Mode::Blocked(Why::NoRng),
                }
                // `rec` (and its key) is dropped and wiped here.
            }
            State::Open => Mode::Open,
            State::Missing => Mode::Blocked(Why::Missing),
            State::Damaged => Mode::Blocked(Why::Damaged),
            State::WrongBoard { record_mac: m } => {
                record_mac = Some(m);
                Mode::Blocked(Why::WrongBoard)
            }
        };
        Link {
            mode,
            me,
            rng: rng.map(|r| r.name),
            summary,
            record_mac,
            gateway: None,
            new_gateway: false,
            open_last: None,
            next_announce_ms: 0,
            announce_seq: 0,
            counts: Counts::default(),
        }
    }

    pub fn mode(&self) -> &Mode {
        &self.mode
    }

    /// Why the node sends nothing, if it is blocked.
    pub fn blocked(&self) -> Option<Why> {
        match self.mode {
            Mode::Blocked(w) => Some(w),
            _ => None,
        }
    }

    /// `"keyed"`, `"open"` or the blocking reason.
    pub fn state_name(&self) -> &'static str {
        match self.mode {
            Mode::Keyed(_) => "keyed",
            Mode::Open => "open",
            Mode::Blocked(w) => w.name(),
        }
    }

    /// The keyed node's link state (session, epoch), if keyed.
    pub fn node(&self) -> Option<&Node> {
        match &self.mode {
            Mode::Keyed(n) => Some(n),
            _ => None,
        }
    }

    /// The boot line (also the blocked node's repeated warning), one JSON
    /// object without a newline:
    ///
    /// - keyed: `{"t":"uplink","auth":"kit:<kit_id> slot:<n> gen:<g> check:<key_check>","rng":"trng","link":"v3"}`
///   (`"rng":"radio"` on a chip without a HAL TRNG type, [`RngSource`])
    /// - open: `{"t":"warn","what":"link_key","auth":"open",...}`
    /// - blocked: `{"t":"warn","what":"link_key","auth":"none","why":"<reason>",...}`
    ///
    /// `tools/flash-node.sh` calls a flash good only on the first (or the
    /// second with `--open`).
    pub fn boot_line(&self, out: &mut impl Write) -> core::fmt::Result {
        let rng = self.rng.unwrap_or("none");
        match (&self.mode, self.summary) {
            (Mode::Keyed(_), Some(s)) => write!(out, r#"{{"t":"uplink","auth":"{s}","rng":"{rng}","link":"v3"}}"#),
            (Mode::Open, _) => write!(
                out,
                r#"{{"t":"warn","what":"link_key","auth":"open","rng":"{rng}","link":"v1","note":"OPEN provisioning record: ESP-NOW reports and hub commands are NOT authenticated (docs/LINK-KEYS-DESIGN.md 4.4)"}}"#
            ),
            (Mode::Blocked(w), _) => {
                write!(out, r#"{{"t":"warn","what":"link_key","auth":"none","why":"{}","rng":"{rng}""#, w.name())?;
                if let Some(s) = self.summary {
                    write!(out, r#","record":"{s}""#)?;
                }
                if let Some(m) = self.record_mac {
                    write!(out, r#","record_mac":"{}""#, MacText(m))?;
                }
                write!(out, r#","board_mac":"{}","note":"{}"}}"#, MacText(self.me), w.note())
            }
            // Keyed always has a summary.
            (Mode::Keyed(_), None) => write!(out, r#"{{"t":"uplink","auth":"keyed","rng":"{rng}","link":"v3"}}"#),
        }
    }

    // ------------------------------------------------------------ gateway --

    fn learn(&mut self, mac: Mac, channel: Option<u8>, now_ms: u64) {
        let ch = channel.filter(|c| (1..=14).contains(c));
        match &mut self.gateway {
            Some(g) if g.mac == mac => {
                g.seen_ms = now_ms;
                if let Some(c) = ch {
                    g.channel = c;
                }
            }
            _ => {
                self.gateway = Some(Gateway { mac, channel: ch.unwrap_or(airlink::GATEWAY_CHANNEL), seen_ms: now_ms });
                self.new_gateway = true;
            }
        }
    }

    /// The gateway to send to and its channel; forgotten after
    /// [`GATEWAY_TIMEOUT_MS`] without hearing or reaching it.
    pub fn gateway(&mut self, now_ms: u64) -> Option<(Mac, u8)> {
        if self.gateway.is_some_and(|g| now_ms.saturating_sub(g.seen_ms) > GATEWAY_TIMEOUT_MS) {
            self.gateway = None;
        }
        self.gateway.map(|g| (g.mac, g.channel))
    }

    /// Whether a gateway is known (without forgetting a stale one).
    pub fn gateway_known(&self) -> bool {
        self.gateway.is_some()
    }

    /// A gateway learned (or changed) since the last call: add it as a
    /// peer and say so.
    pub fn take_new_gateway(&mut self) -> Option<(Mac, u8)> {
        if !core::mem::take(&mut self.new_gateway) {
            return None;
        }
        self.gateway.map(|g| (g.mac, g.channel))
    }

    // ------------------------------------------------------------ receive --

    /// One packet the radio received from `src`, at node uptime `now_ms`
    /// (the same monotonic clock as [`Link::packet`]).
    pub fn receive<'a>(&mut self, src: &Mac, data: &'a [u8], now_ms: u64) -> Rx<'a> {
        if is_other_nodes(data) {
            return Rx::Nothing;
        }
        match self.mode {
            Mode::Keyed(_) => self.receive_keyed(src, data, now_ms),
            Mode::Open => self.receive_open(src, data, now_ms),
            // Blocked: nothing from the hub is used.
            Mode::Blocked(_) => Rx::Nothing,
        }
    }

    fn receive_keyed<'a>(&mut self, src: &Mac, data: &'a [u8], now_ms: u64) -> Rx<'a> {
        let shape = match v3::inspect(data) {
            Ok(s) => s,
            Err(r) => {
                self.counts.rejects.take(Verdict::Reject(r));
                return Rx::Nothing;
            }
        };
        let timesync = shape.kind == Kind::Command && has_timesync(v3::payload(data, &shape));
        let Mode::Keyed(node) = &mut self.mode else { return Rx::Nothing };
        let v = node.accept(src, data, now_ms, timesync);
        self.counts.rejects.take(v.verdict());
        if !v.proves_gateway() {
            return Rx::Nothing;
        }
        self.counts.heard = self.counts.heard.wrapping_add(1);
        // Verified (tag checked) even when refused for its epoch.
        let channel = matches!(shape.kind, Kind::Beacon | Kind::Sync).then(|| data[airlink::HEADER]);
        self.learn(*src, channel, now_ms);
        match v {
            NodeVerdict::Accept(d) => match d.kind {
                Kind::Beacon => {
                    self.counts.beacons = self.counts.beacons.wrapping_add(1);
                    Rx::Gateway
                }
                Kind::Sync => {
                    self.counts.syncs = self.counts.syncs.wrapping_add(1);
                    Rx::Gateway
                }
                Kind::Command => {
                    self.counts.commands = self.counts.commands.wrapping_add(1);
                    Rx::Commands(d.payload)
                }
                Kind::Frames | Kind::Unprovisioned => Rx::Nothing,
            },
            _ => Rx::Gateway,
        }
    }

    fn receive_open<'a>(&mut self, src: &Mac, data: &'a [u8], now_ms: u64) -> Rx<'a> {
        let Some(pkt) = airlink::parse(data) else { return Rx::Nothing };
        if pkt.sealed {
            // Version 2: a board still on the kit-key firmware.
            self.counts.rejects.take(Verdict::Reject(Reject::OldFirmware));
            return Rx::Nothing;
        }
        match pkt.kind {
            airlink::Kind::Frames => Rx::Nothing,
            airlink::Kind::Beacon => {
                self.counts.heard = self.counts.heard.wrapping_add(1);
                self.counts.beacons = self.counts.beacons.wrapping_add(1);
                self.learn(*src, airlink::beacon_channel(&pkt), now_ms);
                Rx::Gateway
            }
            airlink::Kind::Command => {
                self.counts.heard = self.counts.heard.wrapping_add(1);
                self.learn(*src, None, now_ms);
                if self.open_last == Some((*src, pkt.seq)) {
                    self.counts.rejects.take(Verdict::Duplicate);
                    return Rx::Gateway;
                }
                self.open_last = Some((*src, pkt.seq));
                self.counts.commands = self.counts.commands.wrapping_add(1);
                Rx::Commands(pkt.payload)
            }
        }
    }

    // --------------------------------------------------------------- send --

    /// Whether this node may report over ESP-NOW at all.
    pub fn may_send(&self) -> bool {
        !matches!(self.mode, Mode::Blocked(_))
    }

    /// The packet for one send attempt of `batch` (every retry calls this
    /// again: a keyed node seals each attempt with a fresh stamp). `None`
    /// while a keyed node has no hub epoch yet (it has not heard a beacon),
    /// or when blocked.
    pub fn packet<'b>(&mut self, batch: &'b mut airlink::Batch, seq: u16, now_ms: u64) -> Option<&'b [u8]> {
        match &mut self.mode {
            Mode::Keyed(node) => batch.sealed_v3(seq, node, now_ms),
            Mode::Open => Some(batch.packet(seq)),
            Mode::Blocked(_) => None,
        }
    }

    /// One send attempt of a batch of `frames` frames finished: `acked`
    /// when the radio got the gateway's ACK (which also proves it is there).
    pub fn sent(&mut self, acked: bool, frames: u16, now_ms: u64) {
        let c = &mut self.counts;
        c.send_attempts = c.send_attempts.wrapping_add(1);
        if acked {
            c.batches_sent = c.batches_sent.wrapping_add(1);
            c.frames_sent = c.frames_sent.wrapping_add(u32::from(frames));
            if let Some(g) = &mut self.gateway {
                g.seen_ms = now_ms;
            }
        }
    }

    /// A batch dropped after every retry.
    pub fn given_up(&mut self) {
        self.counts.batches_given_up = self.counts.batches_given_up.wrapping_add(1);
    }

    /// Frames thrown away unsent (blocked node).
    pub fn not_sent(&mut self, frames: u32) {
        self.counts.not_sent = self.counts.not_sent.wrapping_add(frames);
    }

    // ------------------------------------------------------- announcement --

    /// A blocked node, every [`ANNOUNCE_MS`] (the first at once): the
    /// reason (print the warning, [`Link::boot_line`], blink) and the
    /// unsealed `Unprovisioned` announcement to broadcast on the gateway
    /// channel. `image` is one of the `IMAGE_*` numbers, `firmware` the
    /// image's name and version (replaced by `<name> norng` when the reason
    /// is a missing random source). `None` when not due or not blocked.
    pub fn announce<'b>(&mut self, now_ms: u64, image: u8, name: &str, version: &str, buf: &'b mut [u8; v3::UNPROVISIONED_LEN]) -> Option<(Why, &'b [u8])> {
        let Mode::Blocked(why) = self.mode else { return None };
        if now_ms < self.next_announce_ms {
            return None;
        }
        self.next_announce_ms = now_ms + ANNOUNCE_MS;
        let mut fw: heapless::String<{ v3::FIRMWARE_LEN }> = heapless::String::new();
        let tail = if why == Why::NoRng { "norng" } else { version };
        for part in [name, " ", tail] {
            for ch in part.chars() {
                if fw.push(ch).is_err() {
                    break;
                }
            }
        }
        let mut firmware = [0u8; v3::FIRMWARE_LEN];
        firmware[..fw.len()].copy_from_slice(fw.as_bytes());
        self.announce_seq = self.announce_seq.wrapping_add(1);
        let a = v3::Unprovisioned { mac: self.me, image, firmware };
        let p = v3::unprovisioned(buf, self.announce_seq, &a)?;
        self.counts.announcements = self.counts.announcements.wrapping_add(1);
        Some((why, p))
    }

    // ------------------------------------------------------------- health --

    /// The link's part of `link::NodeHealth`: delivery accounting, rejects
    /// by reason, repeats.
    pub fn fill_health(&self, h: &mut NodeHealth) {
        fill_health(&self.counts, h);
    }
}

/// [`Link::fill_health`] from published counts.
pub fn fill_health(c: &Counts, h: &mut NodeHealth) {
    h.frames_sent = c.frames_sent;
    h.batches_sent = c.batches_sent;
    h.batches_given_up = c.batches_given_up;
    h.send_attempts = c.send_attempts;
    h.rejects = node_rejects(&c.rejects);
    h.duplicates = c.rejects.dups;
}

/// The node-side reasons, in `link::NodeHealth::rejects` order.
pub fn node_rejects(r: &NodeRejects) -> [u32; link::NODE_REJECTS] {
    let mut out = [0u32; link::NODE_REJECTS];
    for reason in Reject::ALL {
        if let Some(i) = NodeHealth::reject_index(reason.code()) {
            out[i] = r.by_reason[reason.index()];
        }
    }
    out
}

/// The `link_auth` USB line: every node-side reason and the repeats,
/// `{"t":"link_auth","bad_tag":..,...,"dups":..}`.
pub fn link_auth_line(c: &Counts, out: &mut impl Write) -> core::fmt::Result {
    let r = node_rejects(&c.rejects);
    out.write_str(r#"{"t":"link_auth""#)?;
    for (i, &code) in link::NODE_REJECT_CODES.iter().enumerate() {
        let name = Reject::ALL.iter().find(|x| x.code() == code).map_or("?", |x| x.name());
        write!(out, r#","{name}":{}"#, r[i])?;
    }
    write!(out, r#","dups":{}}}"#, c.rejects.dups)
}

/// What a received packet means to the node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rx<'a> {
    /// Not for us, refused, or nothing to do (counted where it counts).
    Nothing,
    /// The gateway is there (a beacon, a Sync, a repeat, or a genuine packet
    /// refused only for its epoch): nothing else to do.
    Gateway,
    /// Hub commands to apply: whole COBS `link::Command` frames ([`commands`]).
    Commands(&'a [u8]),
}

/// Another node's batch (or announcement), any version: not for us.
/// (Kind 2 is Frames in every version; kind 5, Unprovisioned, exists only
/// in version 3.)
fn is_other_nodes(data: &[u8]) -> bool {
    data.len() >= airlink::HEADER
        && data[..2] == airlink::MAGIC
        && (data[3] == 2 || (data[2] == v3::VERSION_V3 && data[3] == Kind::Unprovisioned as u8))
}

/// Decode the commands in a command payload, in order.
pub fn commands(payload: &[u8], mut f: impl FnMut(Command)) {
    let mut dec = CommandDecoder::new();
    for frame in v3::frames(payload) {
        dec.feed(frame, |r| {
            if let Ok(c) = r {
                f(c);
            }
        });
    }
}

/// Whether a command payload holds a `TimeSync` (the node then asks the
/// link for the stricter `TimeSync` lateness rule).
pub fn has_timesync(payload: &[u8]) -> bool {
    let mut found = false;
    commands(payload, |c| found |= matches!(c, Command::TimeSync { .. }));
    found
}

/// A MAC as `aa:bb:cc:dd:ee:ff`.
pub struct MacText(pub Mac);

impl core::fmt::Display for MacText {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let m = self.0;
        write!(f, "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5])
    }
}

#[cfg(test)]
mod tests;
