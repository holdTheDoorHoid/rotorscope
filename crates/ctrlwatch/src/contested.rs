//! A control link heard from **more transmitters than it can have**
//! (docs/DRONE-ATTACKS.md, "Second transmitter on a control link").
//!
//! A drone's control or telemetry link has a fixed cast: one ExpressLRS
//! transmitter per UID, one mLRS transmitter and one receiver per bind
//! phrase, exactly two SiK radios per NET ID. Hijacking adds a member: a
//! second ELRS transmitter with the same binding phrase, a SiK radio that
//! brute-forced the NET ID and injects (SiKW00F, SiKening), a rogue SiK pair
//! sitting in the middle. [`Watch`] looks for that extra member in what the
//! node measures anyway, and prefers **contradictions in protocol state** to
//! signal strength: signal strength moves whenever a drone flies.
//!
//! # Rules, per system
//!
//! **ExpressLRS ([`RULE_PHASE`]).** A SYNC packet carries the transmitter's
//! packet counter (`nonce`, +1 every packet interval, wraps at 256). From
//! one transmitter, `t - nonce * interval` is the same modulo
//! `256 * interval` for every sync (to within timestamp jitter and clock
//! drift): its *phase*. Two transmitters on one UID have independent
//! phases (a 2-5 % chance of landing within tolerance of each other). Syncs
//! of one link are grouped by phase; the link is contested when two groups
//! of at least [`MIN_EACH`] syncs each **alternate** in time (A, B, A, B:
//! at least [`MIN_SWITCHES`] changes): one transmitter that restarts gives
//! A, A, A, B, B, B, never back to A.
//!
//! **mLRS ([`RULE_PHASE`]).** Every frame starts on the transmitter's frame
//! tick, so on the one channel the node listens to, uplink frames from one
//! transmitter arrive at the same time modulo the frame interval (20, 32 or
//! 53 ms); downlink frames from the receiver likewise. Same grouping and
//! alternation rule, per direction.
//!
//! **SiK ([`RULE_CLOCKS`], [`RULE_SEQUENCE`]).** SiK is time-division: in
//! each window exactly one radio transmits, and every packet's trailer says
//! how many 16 µs ticks are left in the sender's window. So every packet
//! claims a window **end time** (`t + window * 16 µs`). Packets inside one
//! window that claim ends more than [`SIK_END_TOL_US`] apart come from two
//! TDM clocks: a third radio that is not (or not yet) in step with the pair
//! (SiKening while it syncs, a crude injector, a second pair on the same NET
//! ID, a rogue pair with its own clock). Contested after [`SIK_MIN_EVENTS`]
//! such windows, and only on a link whose trailers otherwise agree (the
//! trailer sits at a different offset on AES-capable builds; the layout is
//! learned per link from which reading gives agreeing ends).
//! With content decoding on, MAVLink sequence numbers inside a window add a
//! second check: one component's frames count up by one; two interleaved
//! counts for the same system and component (A 100, 101, B 7, A 102) mean
//! two senders. That is what a SiKW00F injector in step with the pair
//! looks like, since its timing is borrowed from the pair.
//!
//! **Forced re-sync ([`RULE_HANDOVER`], ExpressLRS and mLRS).** A link
//! that falls silent and comes back within [`HANDOVER_GAP_US`] with a
//! phase that is not the old transmitter's, *and* a different signature
//! (ExpressLRS switch mode / telemetry ratio / link mode, or signal
//! strength [`HANDOVER_RSSI_DB`] away): what jamming followed by a
//! takeover looks like. A pilot power-cycling the radio also gives a new
//! phase, so the signature change is required; reported as its own rule
//! because it is the weakest one.
//!
//! # History first, and few new alarms (WP-11a)
//!
//! These protocols are unauthenticated: a sync packet or a SiK trailer is
//! whatever its sender wrote. Until WP-11a about six ExpressLRS syncs from
//! **one** radio, alternating between two nonce phases on a made-up UID,
//! raised "link hijack" with no drone anywhere. Two rules now stand between
//! a forger and the alarm:
//!
//! - **History.** A link's contradictions only count once the link has
//!   been heard as **one** consistent transmitter for [`HISTORY_US`] before
//!   a second one appeared ([`Link`] is then *seasoned*, for good): for the
//!   phase systems, an established group spanning [`HISTORY_US`] with no
//!   other established group of its class starting within that time; for
//!   SiK, window ends agreeing for [`HISTORY_US`]. A real hijack happens to
//!   a link that was flying before; a link that was contested from its first
//!   packet is counted ([`Watch::unseasoned`]) but not reported. The
//!   handover rule needs the same history of the old transmitter.
//! - **New alarms are budgeted.** A link's first report costs a token
//!   ([`NEW_FINDING_BURST`], then one per [`NEW_FINDING_REFILL_US`], for the
//!   whole node); a finding waiting for one is counted
//!   ([`Watch::throttled`]) and goes out when a token comes, unless it has
//!   lapsed ([`HOLD_US`]) by then. Reports of a link already reported are
//!   not limited beyond [`REPORT_GAP_US`].
//!
//! A forger who first plays a made-up link alone for [`HISTORY_US`] and
//! then alternates still gets the alarm; it costs seconds instead of six
//! packets, and at most a handful of such alarms a minute.
//!
//! # What cannot be separated
//!
//! - An injector that copies the victim's timing exactly (follows the ELRS
//!   nonce, rides the SiK TDM clock) and sends nothing contradictory is the
//!   same as the real transmitter in timing. Only content (MAVLink sequence,
//!   with content decoding on) or signal strength could tell, and signal
//!   strength alone is not used.
//! - Two people flying with the same ExpressLRS binding phrase (Model Match
//!   off) or the same SiK NET ID (the factory default 25) really are two
//!   transmitters on one link; this reports them. Mute rules exist for that.
//! - The LR2021 does not report a per-packet frequency error, so the
//!   transmitter's crystal offset (a good fingerprint) is not available.
//! - OpenLRSng carries no counter the node can read without the bind data,
//!   and its packet interval depends on it: not covered.
//!
//! # Privacy
//!
//! Packets are grouped per link by its identifier (UID bytes, NET ID, sync
//! word), which the node computes anyway to check CRCs. It stays in node
//! memory: [`Report::id`] is only filled in when [`Config::read_ids`] is
//! set at report time. The MAVLink sequence check only runs with
//! [`Config::decode_content`]; nothing it reads leaves the node.
//!
//! Fixed memory: [`Watch`] holds `L` links of [`RING`] observations each.

use crate::crc::{pn9_xor, x25};
use crate::rx::{Modulation, Probe, RxConfig, RxMeta};
use crate::sik::si443x_parse;
use crate::{Config, LinkId, Region, System};

// ------------------------------------------------------------ thresholds --

/// Timestamp tolerance, µs: how far one transmitter's phase may wander
/// between two packets close in time. The node stamps a packet when it sees
/// the radio's interrupt (async wake-up, SPI reads), a few hundred µs late
/// at worst; 2 ms leaves margin for a busy moment.
pub const TIME_TOL_US: u32 = 2_000;
/// Clock drift budget, parts per million, added to the tolerance per µs of
/// gap: ESP32 crystal (±10 ppm) plus the transmitter's crystal or TCXO
/// (±2.5 to ±20 ppm), with margin.
pub const DRIFT_PPM: u32 = 50;
/// Observations kept per link.
pub const RING: usize = 12;
/// Each transmitter must be seen this many times, consistently with
/// itself, before it counts: one odd packet (a timestamp stretched by a
/// busy moment) can never make a second transmitter.
pub const MIN_EACH: usize = 3;
/// Changes between transmitters, in time order, before a link is
/// contested: A B A B is three. Two would also accept a single packet
/// stamped late in the middle of a run; three needs both to come back.
pub const MIN_SWITCHES: usize = 3;
/// Forced re-sync: the new transmitter starts at most this long after the
/// old one's last packet (jam, then take over).
pub const HANDOVER_GAP_US: u64 = 30_000_000;
/// Forced re-sync: mean signal strength of the two this far apart, dB
/// (when the protocol signature is the same). A pilot holding the same
/// radio does not move 15 dB in half a minute; a different radio elsewhere
/// usually does.
pub const HANDOVER_RSSI_DB: i16 = 15;
/// Observations older than this are dropped.
pub const OBS_MAX_AGE_US: u64 = 600_000_000;
/// A link not heard for this long is forgotten.
pub const LINK_MAX_AGE_US: u64 = 600_000_000;
/// SiK: 16 µs TDM ticks.
pub const SIK_TICK_US: u64 = 16;
/// SiK: two packets in one window may claim ends this far apart and still
/// be one clock (timestamp jitter plus the sender's flight-time estimate,
/// which is rounded to whole bytes).
pub const SIK_END_TOL_US: u64 = 3_000;
/// SiK: packets further apart than this are never compared (longer than
/// the largest TDM window, 8191 ticks = 131 ms).
pub const SIK_PAIR_US: u64 = 150_000;
/// SiK: contradicting windows before a link is contested.
pub const SIK_MIN_EVENTS: usize = 3;
/// SiK: contradictions this close are one window.
pub const SIK_EVENT_SEP_US: u64 = 150_000;
/// SiK: the trailer reading must have given at least this many agreeing
/// pairs, and agreeing pairs times [`SIK_AGREE_RATIO`] must be at least the
/// contradicting ones (agreement in a quarter of all compared pairs),
/// before its contradictions count. A trailer read at the wrong offset is
/// noise and agrees with itself about 5 % of the time; an injector that
/// slips one packet into a window of five still leaves half the pairs
/// agreeing.
pub const SIK_MIN_AGREE: u16 = 8;
pub const SIK_AGREE_RATIO: u16 = 3;
/// SiK / MAVLink: one sender's sequence moves on by at most this much
/// inside one window (frames lost in between).
pub const MAV_MAX_STEP: u8 = 16;
/// A finding is re-reported this often while it stands...
pub const REFRESH_US: u64 = 5_000_000;
/// ...for this long after its last new evidence.
pub const HOLD_US: u64 = 60_000_000;
/// New evidence goes out at most this often.
pub const REPORT_GAP_US: u64 = 1_000_000;
/// A link must have been heard as one transmitter this long before a
/// second one counts (see "History first").
pub const HISTORY_US: u64 = 5_000_000;
/// New contested links reported in a burst...
pub const NEW_FINDING_BURST: u32 = 2;
/// ...then one every this long.
pub const NEW_FINDING_REFILL_US: u64 = 10_000_000;

/// Rule bits in [`Report::rules`] (and in the threat's `detail`).
pub const RULE_PHASE: u8 = 1;
pub const RULE_CLOCKS: u8 = 2;
pub const RULE_SEQUENCE: u8 = 4;
pub const RULE_HANDOVER: u8 = 8;

// --------------------------------------------------------------- report --

/// One contested link, for the hub.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    pub system: System,
    pub region: Region,
    /// Frequency the node heard it on.
    pub freq_hz: u32,
    /// Transmitters the evidence needs, at least (SiK: the pair plus the
    /// intruder).
    pub transmitters: u8,
    /// Which rules fired ([`RULE_PHASE`] ...).
    pub rules: u8,
    /// Pieces of evidence (switches between transmitters, contradicting
    /// windows, interleaved sequences).
    pub events: u32,
    pub first_us: u64,
    pub last_us: u64,
    /// Strongest packet on the link, dBm.
    pub peak_rssi_dbm: i16,
    /// The node's own handle for the link (1-255): tells two contested
    /// links apart without naming them. Not an identifier of the link.
    pub handle: u8,
    /// The link's identifier: `None` unless [`Config::read_ids`].
    pub id: Option<LinkId>,
}

impl Report {
    /// The threat's `detail`: bits 0-7 the [`System`] code (its
    /// discriminant), 8-11 transmitters (at most 15), 12-15 the rule bits,
    /// 16-23 the [`Region`] code, 24-31 zero.
    pub fn detail(&self) -> u32 {
        encode_detail(self.system, self.transmitters, self.rules, self.region)
    }
}

pub fn encode_detail(system: System, transmitters: u8, rules: u8, region: Region) -> u32 {
    system as u32
        | (u32::from(transmitters.min(15)) << 8)
        | (u32::from(rules & 0xF) << 12)
        | ((region as u32) << 16)
}

/// The parts of a `detail` (see [`Report::detail`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Detail {
    pub system: Option<System>,
    pub transmitters: u8,
    pub rules: u8,
    pub region: Option<Region>,
}

pub fn decode_detail(d: u32) -> Detail {
    const SYSTEMS: [System; 10] = [
        System::ExpressLrs,
        System::Mlrs,
        System::Sik,
        System::OpenLrsNg,
        System::Crossfire,
        System::Tracer,
        System::Ghost,
        System::FrskyR9,
        System::FrskyX,
        System::FlyskyAfhds2a,
    ];
    Detail {
        system: SYSTEMS.get((d & 0xFF) as usize).copied(),
        transmitters: ((d >> 8) & 0xF) as u8,
        rules: ((d >> 12) & 0xF) as u8,
        region: Region::ALL.get(((d >> 16) & 0xFF) as usize).copied(),
    }
}

/// Short words for the rule bits, for logs and the hub's text.
pub fn rule_name(rules: u8) -> &'static str {
    if rules & RULE_PHASE != 0 {
        "interleaved timing"
    } else if rules & RULE_CLOCKS != 0 {
        "two clocks in one window"
    } else if rules & RULE_SEQUENCE != 0 {
        "two sequence counts"
    } else if rules & RULE_HANDOVER != 0 {
        "new transmitter after silence"
    } else {
        "-"
    }
}

// ----------------------------------------------------------------- state --

/// One timing observation (ExpressLRS sync, mLRS frame).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Obs {
    t_us: u64,
    /// Phase within `cycle_us`.
    phase_us: u32,
    cycle_us: u32,
    /// Direction class: 0 uplink, 1 downlink.
    class: u8,
    /// Protocol settings that one transmitter keeps (ELRS switch mode,
    /// telemetry ratio, link mode); 0 when none.
    sig: u16,
    rssi: i16,
}

/// The previous SiK packet, as each trailer layout reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SikPrev {
    t_us: u64,
    /// Claimed window end per layout (2-byte trailer, 4-byte AES trailer);
    /// `None` for statistics packets (window 0) or too short.
    end: [Option<u64>; 2],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MavHead {
    sysid: u8,
    compid: u8,
    seq: u8,
    t_us: u64,
    /// Order of the last update (higher = later).
    stamp: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Sik {
    prev: Option<SikPrev>,
    agree: [u16; 2],
    contra: [u16; 2],
    /// Recent contradicting windows per layout (ring), and the next slot.
    events: [[Option<u64>; SIK_MIN_EVENTS]; 2],
    next_event: [u8; 2],
    last_event: [Option<u64>; 2],
    mav: [Option<MavHead>; 4],
    mav_stamp: u32,
    mav_last_event_us: u64,
    /// First agreeing pair of window ends (either layout).
    agree_since: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Finding {
    rules: u8,
    transmitters: u8,
    events: u32,
    first_us: u64,
    last_ev_us: u64,
    reported_us: Option<u64>,
    reported_events: u32,
    /// Has had its token (first report allowed).
    admitted: bool,
    /// Counted in [`Watch::throttled`] already.
    held: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Link {
    system: System,
    region: Region,
    id: LinkId,
    handle: u8,
    freq_hz: u32,
    last_us: u64,
    peak_rssi: i16,
    ring: heapless::Vec<Obs, RING>,
    /// Start of the newest transmitter already reported as a handover.
    handover_done: Option<u64>,
    sik: Sik,
    finding: Option<Finding>,
    /// Heard as one transmitter for [`HISTORY_US`] (see "History first").
    seasoned: bool,
    /// Contradictions seen before the link was seasoned (not reported).
    unseasoned: u32,
}

impl Link {
    fn evidence(&mut self, rule: u8, transmitters: u8, t_us: u64) {
        if !self.seasoned {
            self.unseasoned = self.unseasoned.saturating_add(1);
            return;
        }
        let f = self.finding.get_or_insert(Finding {
            rules: 0,
            transmitters: 0,
            events: 0,
            first_us: t_us,
            last_ev_us: t_us,
            reported_us: None,
            reported_events: 0,
            admitted: false,
            held: false,
        });
        f.rules |= rule;
        f.transmitters = f.transmitters.max(transmitters);
        f.events = f.events.saturating_add(1);
        f.last_ev_us = f.last_ev_us.max(t_us);
    }
}

/// The contested-link check for up to `L` links (see the module docs).
#[derive(Debug, Clone)]
pub struct Watch<const L: usize> {
    links: heapless::Vec<Link, L>,
    next_handle: u8,
    /// Links dropped to make room.
    pub evicted: u32,
    /// Contradictions on links without history (not reported).
    pub unseasoned: u32,
    /// New findings that had to wait for a token.
    pub throttled: u32,
    tokens: u32,
    token_at_us: u64,
}

impl<const L: usize> Default for Watch<L> {
    fn default() -> Self {
        Self::new()
    }
}

/// What one packet says, per system.
enum Seen<'a> {
    Phase(Obs),
    Sik {
        end: [Option<u64>; 2],
        serial: [&'a [u8]; 2],
    },
}

impl<const L: usize> Watch<L> {
    pub const fn new() -> Self {
        Self {
            links: heapless::Vec::new(),
            next_handle: 1,
            evicted: 0,
            unseasoned: 0,
            throttled: 0,
            tokens: NEW_FINDING_BURST,
            token_at_us: 0,
        }
    }

    fn take_token(&mut self, now_us: u64) -> bool {
        if self.tokens >= NEW_FINDING_BURST {
            self.token_at_us = now_us;
        } else {
            let add = now_us.saturating_sub(self.token_at_us) / NEW_FINDING_REFILL_US;
            if add > 0 {
                self.tokens = (u64::from(self.tokens) + add).min(u64::from(NEW_FINDING_BURST)) as u32;
                self.token_at_us += add * NEW_FINDING_REFILL_US;
            }
        }
        if self.tokens == 0 {
            return false;
        }
        self.tokens -= 1;
        true
    }

    /// One packet `crate::recognise` accepted on `rx` (pass every one; the
    /// ones this check cannot use are ignored).
    pub fn observe(&mut self, raw: &[u8], rx: &RxConfig, meta: RxMeta, cfg: &Config) {
        let mut buf = [0u8; 258];
        let Some((system, id, seen)) = parse(raw, rx, meta, &mut buf) else {
            return;
        };
        let t = meta.t_us;
        let i = match self
            .links
            .iter()
            .position(|l| l.system == system && l.region == rx.region && l.id == id)
        {
            Some(i) => i,
            None => {
                if self.links.is_full() {
                    // Drop the least recently heard link without a finding,
                    // else the least recently heard.
                    let victim = (0..self.links.len())
                        .filter(|&k| self.links[k].finding.is_none())
                        .min_by_key(|&k| self.links[k].last_us)
                        .or_else(|| (0..self.links.len()).min_by_key(|&k| self.links[k].last_us));
                    if let Some(k) = victim {
                        self.links.swap_remove(k);
                        self.evicted = self.evicted.saturating_add(1);
                    }
                }
                let handle = self.next_handle;
                self.next_handle = self.next_handle.wrapping_add(1).max(1);
                let _ = self.links.push(Link {
                    system,
                    region: rx.region,
                    id,
                    handle,
                    freq_hz: rx.freq_hz,
                    last_us: t,
                    peak_rssi: meta.rssi_dbm,
                    ring: heapless::Vec::new(),
                    handover_done: None,
                    sik: Sik::default(),
                    finding: None,
                    seasoned: false,
                    unseasoned: 0,
                });
                self.links.len() - 1
            }
        };
        let link = &mut self.links[i];
        link.last_us = link.last_us.max(t);
        link.freq_hz = rx.freq_hz;
        link.peak_rssi = link.peak_rssi.max(meta.rssi_dbm);
        let before = link.unseasoned;
        match seen {
            Seen::Phase(o) => phase_observe(link, o),
            Seen::Sik { end, serial } => sik_observe(link, t, end, serial, cfg.decode_content),
        }
        let more = link.unseasoned - before;
        self.unseasoned = self.unseasoned.saturating_add(more);
    }

    /// Hand every link with a finding due a report to `out`: new evidence
    /// at most every [`REPORT_GAP_US`], and a standing finding every
    /// [`REFRESH_US`] for [`HOLD_US`] after its last evidence.
    pub fn take_due(&mut self, now_us: u64, cfg: &Config, mut out: impl FnMut(&Report)) {
        for k in 0..self.links.len() {
            let Some(f) = self.links[k].finding else {
                continue;
            };
            let since = |t: Option<u64>| t.map_or(u64::MAX, |t| now_us.saturating_sub(t));
            let fresh = f.events != f.reported_events && since(f.reported_us) >= REPORT_GAP_US;
            let live = now_us.saturating_sub(f.last_ev_us) < HOLD_US;
            let standing = live && since(f.reported_us) >= REFRESH_US;
            if !(fresh || standing) {
                continue;
            }
            if !f.admitted {
                // A new contested link: it needs a token, and a finding
                // that lapsed while waiting is not sent late.
                if !live || !self.take_token(now_us) {
                    if !f.held {
                        self.throttled = self.throttled.saturating_add(1);
                    }
                    if let Some(f) = self.links[k].finding.as_mut() {
                        f.held = true;
                    }
                    continue;
                }
            }
            let l = &mut self.links[k];
            let Some(f) = l.finding.as_mut() else {
                continue;
            };
            f.admitted = true;
            f.reported_us = Some(now_us);
            f.reported_events = f.events;
            out(&Report {
                system: l.system,
                region: l.region,
                freq_hz: l.freq_hz,
                transmitters: f.transmitters,
                rules: f.rules,
                events: f.events,
                first_us: f.first_us,
                last_us: f.last_ev_us,
                peak_rssi_dbm: l.peak_rssi,
                handle: l.handle,
                id: cfg.read_ids.then_some(l.id),
            });
        }
    }

    /// Forget links not heard for [`LINK_MAX_AGE_US`] whose finding (if
    /// any) has lapsed.
    pub fn expire(&mut self, now_us: u64) {
        self.links.retain(|l| {
            let heard = now_us.saturating_sub(l.last_us) < LINK_MAX_AGE_US;
            let holding = l.finding.is_some_and(|f| now_us.saturating_sub(f.last_ev_us) < HOLD_US);
            heard || holding
        });
    }

    /// Links held.
    pub fn len(&self) -> usize {
        self.links.len()
    }

    pub fn is_empty(&self) -> bool {
        self.links.is_empty()
    }
}

// ---------------------------------------------------------------- parsing --

fn parse<'b>(raw: &[u8], rx: &RxConfig, meta: RxMeta, buf: &'b mut [u8; 258]) -> Option<(System, LinkId, Seen<'b>)> {
    match rx.probe {
        Probe::Elrs { rate, version } => {
            let info = rate.info();
            if raw.len() != info.len as usize {
                return None;
            }
            let Modulation::LoRa(l) = rx.modulation else {
                return None;
            };
            let iq = (!info.sub_ghz).then_some(l.invert_iq);
            let dom = crate::elrs::domain(rx.region, version);
            let s = crate::elrs::parse_sync(raw, version, &dom, iq, info.sub_ghz)?;
            // A 3.x OTA4 sync that only checks with Model Match is Likely,
            // not Confirmed (see elrs::recognise): not used.
            if s.model_match && version == crate::elrs::OtaVersion::V3 && raw.len() == crate::elrs::OTA4_LEN {
                return None;
            }
            let interval = info.interval_us;
            let cycle = interval.checked_mul(256)?;
            let id = LinkId::ElrsUid {
                uid3: s.uid3,
                uid4: s.uid4,
                uid5: s.uid5_sent,
                uid5_real: s.uid5,
            };
            let back = (u64::from(s.nonce) * u64::from(interval)) % u64::from(cycle);
            let phase = ((meta.t_us % u64::from(cycle)) + u64::from(cycle) - back) % u64::from(cycle);
            let sig = u16::from(s.switch_mode) | (u16::from(s.tlm_ratio) << 1) | (u16::from(s.ota_protocol) << 4);
            Some((
                System::ExpressLrs,
                id,
                Seen::Phase(Obs {
                    t_us: meta.t_us,
                    phase_us: phase as u32,
                    cycle_us: cycle,
                    class: 0,
                    sig,
                    rssi: meta.rssi_dbm,
                }),
            ))
        }
        Probe::Mlrs(mode) => {
            let f = raw;
            if f.len() != crate::mlrs::FRAME_LEN || x25(&f[..89]) != u16::from_le_bytes([f[89], f[90]]) {
                return None;
            }
            let st = crate::mlrs::status(&f[2..7]);
            let class = match st.frame_type {
                0 | 3 => 0,
                1 => 1,
                _ => return None,
            };
            let cycle = mode.interval_us();
            Some((
                System::Mlrs,
                LinkId::MlrsSyncWord(u16::from_le_bytes([f[0], f[1]])),
                Seen::Phase(Obs {
                    t_us: meta.t_us,
                    phase_us: (meta.t_us % u64::from(cycle)) as u32,
                    cycle_us: cycle,
                    class,
                    sig: 0,
                    rssi: meta.rssi_dbm,
                }),
            ))
        }
        Probe::Sik { .. } => {
            let n = raw.len().min(buf.len());
            buf[..n].copy_from_slice(&raw[..n]);
            let b: &'b mut [u8] = &mut buf[..n];
            pn9_xor(b);
            let b: &'b [u8] = b;
            let (hdr, data) = si443x_parse(b, 2, 0x8005)?;
            if data.len() < 2 {
                return None;
            }
            let netid = u16::from_be_bytes([hdr[0], hdr[1]]);
            // Layout 0: the 2-byte trailer at the end. Layout 1: AES-capable
            // builds append the payload CRC after it (4-byte trailer).
            let end_of = |at: usize| {
                let w = u16::from_le_bytes([data[at], data[at + 1]]) & 0x1FFF;
                (w != 0).then(|| meta.t_us + u64::from(w) * SIK_TICK_US)
            };
            let end2 = end_of(data.len() - 2);
            let end4 = (data.len() >= 4).then(|| end_of(data.len() - 4)).flatten();
            let ser2 = &data[..data.len() - 2];
            let ser4 = if data.len() >= 4 { &data[..data.len() - 4] } else { &data[..0] };
            Some((
                System::Sik,
                LinkId::SikNetId(netid),
                Seen::Sik {
                    end: [end2, end4],
                    serial: [ser2, ser4],
                },
            ))
        }
        Probe::OpenLrs { .. } | Probe::Energy => None,
    }
}

// ---------------------------------------------------------- phase systems --

fn circ_dist(a: u32, b: u32, c: u32) -> u32 {
    let d = a.abs_diff(b) % c;
    d.min(c - d)
}

/// Could `a` and `b` be the same transmitter? `None`: too far apart in
/// time to tell (drift could explain anything).
fn consistent(a: &Obs, b: &Obs) -> Option<bool> {
    if a.class != b.class || a.cycle_us != b.cycle_us {
        return Some(false);
    }
    let gap = a.t_us.abs_diff(b.t_us);
    let tol = u64::from(TIME_TOL_US) + gap * u64::from(DRIFT_PPM) / 1_000_000;
    if tol * 4 >= u64::from(a.cycle_us) {
        return None;
    }
    Some(u64::from(circ_dist(a.phase_us, b.phase_us, a.cycle_us)) <= tol)
}

/// Group the ring into transmitters: label per observation.
fn cluster(ring: &[Obs]) -> [u8; RING] {
    let mut lab = [0u8; RING];
    for (i, l) in lab.iter_mut().enumerate().take(ring.len()) {
        *l = i as u8;
    }
    // Union by relabelling (n <= 12).
    for i in 0..ring.len() {
        for j in i + 1..ring.len() {
            if consistent(&ring[i], &ring[j]) == Some(true) && lab[i] != lab[j] {
                let (from, to) = (lab[j].max(lab[i]), lab[j].min(lab[i]));
                for l in lab.iter_mut().take(ring.len()) {
                    if *l == from {
                        *l = to;
                    }
                }
            }
        }
    }
    lab
}

#[derive(Debug, Clone, Copy, Default)]
struct Group {
    n: usize,
    first: u64,
    last: u64,
    rssi_sum: i32,
    sig: u16,
    class: u8,
    cycle: u32,
}

fn phase_observe(link: &mut Link, o: Obs) {
    link.ring.retain(|x| o.t_us.saturating_sub(x.t_us) < OBS_MAX_AGE_US);
    if link.ring.is_full() {
        link.ring.remove(0);
    }
    let _ = link.ring.push(o);
    let ring = &link.ring;
    let lab = cluster(ring);
    let mut groups = [Group::default(); RING];
    for (k, x) in ring.iter().enumerate() {
        let g = &mut groups[lab[k] as usize];
        if g.n == 0 {
            g.first = x.t_us;
            g.class = x.class;
            g.cycle = x.cycle_us;
        }
        g.n += 1;
        g.last = x.t_us;
        g.rssi_sum += i32::from(x.rssi);
        g.sig = x.sig;
    }
    let est = |g: &Group| g.n >= MIN_EACH;
    let me = lab[ring.len() - 1];
    let class = o.class;

    // History: one established transmitter heard for HISTORY_US with no
    // other established one of its class starting within that time.
    if !link.seasoned {
        link.seasoned = (0..RING).any(|g| {
            let a = &groups[g];
            est(a)
                && a.last - a.first >= HISTORY_US
                && !(0..RING).any(|h| {
                    let b = &groups[h];
                    h != g && est(b) && b.class == a.class && b.first < a.first + HISTORY_US
                })
        });
    }

    // Interleaving: the established groups of this class, in time order.
    // A change of group counts only between two packets close enough in
    // time to tell apart: one transmitter heard on scan visits minutes
    // apart falls into several groups, but those never contradict.
    let mut switches = 0usize;
    let mut prev: Option<usize> = None;
    let mut me_switch = false;
    for (k, x) in ring.iter().enumerate() {
        if x.class != class || !est(&groups[lab[k] as usize]) {
            continue;
        }
        if let Some(p) = prev
            && lab[p] != lab[k]
            && consistent(&ring[p], x) == Some(false)
        {
            switches += 1;
            me_switch = k == ring.len() - 1;
        }
        prev = Some(k);
    }
    if est(&groups[me as usize]) && switches >= MIN_SWITCHES {
        // New evidence only when this packet is itself a switch.
        if me_switch {
            let overlapping = (0..RING)
                .filter(|&g| {
                    let a = &groups[g];
                    est(a)
                        && a.class == class
                        && (0..RING).any(|h| {
                            let b = &groups[h];
                            h != g && est(b) && b.class == class && a.first < b.last && b.first < a.last
                        })
                })
                .count();
            // Other directions' established transmitters (mLRS: the
            // receiver) count towards the link's total.
            let others = (0..RING)
                .filter(|&g| est(&groups[g]) && groups[g].class != class)
                .map(|g| groups[g].class)
                .fold(0u8, |m, c| m | (1 << c))
                .count_ones() as u8;
            link.evidence(RULE_PHASE, (overlapping as u8).max(2) + others, o.t_us);
        }
        return;
    }

    // Handover: the newest transmitter just became established, the one
    // before it (same class and cycle) ended shortly before it began, and
    // the signature differs.
    let g = groups[me as usize];
    if g.n != MIN_EACH || link.handover_done == Some(g.first) {
        return;
    }
    let before = (0..RING)
        .filter(|&h| h != me as usize && est(&groups[h]))
        .map(|h| groups[h])
        .filter(|b| b.class == g.class && b.cycle == g.cycle && b.last < g.first)
        .max_by_key(|b| b.last);
    let Some(b) = before else {
        return;
    };
    // Something from the old one after the new one started would have been
    // interleaving, handled above.
    if g.first - b.last > HANDOVER_GAP_US {
        return;
    }
    let mean = |x: &Group| (x.rssi_sum / x.n as i32) as i16;
    let differs = b.sig != g.sig || mean(&b).abs_diff(mean(&g)) >= HANDOVER_RSSI_DB as u16;
    if differs {
        link.handover_done = Some(g.first);
        link.evidence(RULE_HANDOVER, 2, o.t_us);
    }
}

// --------------------------------------------------------------------- SiK --

fn sik_observe(link: &mut Link, t: u64, end: [Option<u64>; 2], serial: [&[u8]; 2], content: bool) {
    let s = &mut link.sik;
    let mut new_event = false;
    if let Some(p) = s.prev
        && t.saturating_sub(p.t_us) <= SIK_PAIR_US
    {
        for (k, (&pa, &pb)) in p.end.iter().zip(end.iter()).enumerate() {
            let (Some(a), Some(b)) = (pa, pb) else {
                continue;
            };
            // Only a packet inside the window the previous one claimed.
            if t > a + SIK_END_TOL_US {
                continue;
            }
            if a.abs_diff(b) <= SIK_END_TOL_US {
                s.agree[k] = s.agree[k].saturating_add(1);
                s.agree_since.get_or_insert(t);
            } else {
                s.contra[k] = s.contra[k].saturating_add(1);
                if s.last_event[k].is_none_or(|l| t.saturating_sub(l) > SIK_EVENT_SEP_US) {
                    let i = s.next_event[k] as usize % SIK_MIN_EVENTS;
                    s.events[k][i] = Some(t);
                    s.next_event[k] = ((i + 1) % SIK_MIN_EVENTS) as u8;
                    s.last_event[k] = Some(t);
                    if k == sik_layout(s) {
                        new_event = true;
                    }
                }
            }
        }
    }
    s.prev = Some(SikPrev { t_us: t, end });
    if !link.seasoned {
        let s = &link.sik;
        link.seasoned = s.agree_since.is_some_and(|a| t.saturating_sub(a) >= HISTORY_US)
            && s.agree[sik_layout(s)] >= SIK_MIN_AGREE;
    }
    let s = &mut link.sik;
    let k = sik_layout(s);
    let trusted = s.agree[k] >= SIK_MIN_AGREE && s.agree[k].saturating_mul(SIK_AGREE_RATIO) >= s.contra[k];
    let recent = s.events[k]
        .iter()
        .flatten()
        .filter(|&&e| t.saturating_sub(e) < OBS_MAX_AGE_US)
        .count();
    if new_event && trusted && recent >= SIK_MIN_EVENTS {
        link.evidence(RULE_CLOCKS, 3, t);
    }
    let layout = sik_layout(&link.sik);
    if content && sik_mavlink(&mut link.sik, t, serial[layout]) {
        link.evidence(RULE_SEQUENCE, 3, t);
    }
}

/// The trailer layout whose window ends agree more often.
fn sik_layout(s: &Sik) -> usize {
    if s.agree[1] > s.agree[0] { 1 } else { 0 }
}

/// MAVLink sequence numbers in one window: true when a frame continues a
/// count that another count for the same component interrupted.
fn sik_mavlink(s: &mut Sik, t: u64, serial: &[u8]) -> bool {
    let mut hit = false;
    for f in crate::mavlink::frames(serial) {
        s.mav_stamp = s.mav_stamp.wrapping_add(1);
        let stamp = s.mav_stamp;
        // Heads of this component still inside the window.
        let live = |h: &Option<MavHead>| {
            h.is_some_and(|h| h.sysid == f.sysid && h.compid == f.compid && t.saturating_sub(h.t_us) <= SIK_PAIR_US)
        };
        let newest = s.mav.iter().filter(|h| live(h)).filter_map(|h| h.map(|h| h.stamp)).max();
        let cont = s.mav.iter().position(|h| {
            live(h) && h.is_some_and(|h| f.seq.wrapping_sub(h.seq) <= MAV_MAX_STEP)
        });
        match cont {
            Some(i) => {
                let h = s.mav[i].as_mut().unwrap();
                // Back to a count after another one moved: two senders.
                if newest.is_some_and(|n| n != h.stamp) && t.saturating_sub(s.mav_last_event_us) > SIK_EVENT_SEP_US {
                    hit = true;
                    s.mav_last_event_us = t;
                }
                h.seq = f.seq;
                h.t_us = t;
                h.stamp = stamp;
            }
            None => {
                let head = Some(MavHead {
                    sysid: f.sysid,
                    compid: f.compid,
                    seq: f.seq,
                    t_us: t,
                    stamp,
                });
                let slot = s
                    .mav
                    .iter()
                    .position(|h| h.is_none_or(|h| t.saturating_sub(h.t_us) > SIK_PAIR_US))
                    .or_else(|| (0..s.mav.len()).min_by_key(|&i| s.mav[i].map_or(0, |h| h.stamp)));
                if let Some(i) = slot {
                    s.mav[i] = head;
                }
            }
        }
    }
    hit
}

#[cfg(test)]
mod tests;
