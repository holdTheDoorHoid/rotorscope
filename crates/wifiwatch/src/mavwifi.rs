//! Hostile MAVLink over open Wi-Fi (docs/DRONE-ATTACKS.md, "Hostile MAVLink
//! telemetry", "Over Wi-Fi"): the `mavwatch` scan the LR2021 node runs on
//! SiK / mLRS, here on MAVLink carried in unprotected 802.11 data frames
//! (ESP8266 / ESP32 telemetry bridges, Wi-Fi hobby drones, anything sending
//! MAVLink over UDP, or TCP to port 5760).
//!
//! Three steps, split so a node can run the first two in its receive
//! callback and the third in a task:
//!
//! 1. [`parse`]: 802.11 data frame -> LLC/SNAP -> IPv4 -> UDP or TCP
//!    payload, from headers alone. Handles QoS and non-QoS data, all four
//!    ToDS/FromDS address layouts and the HT Control field. Skips protected
//!    frames (WEP/WPA: unreadable), null frames, 802.11 fragments, A-MSDUs
//!    (telemetry bridges do not aggregate; left out to keep the callback
//!    simple), non-IPv4, fragmented IPv4 and anything not UDP or TCP.
//! 2. [`plausible`]: is this worth queuing? To or from a usual MAVLink
//!    port ([`MAVLINK_PORTS`]): yes (a chunk of a frame split by a bridge
//!    or by TCP may hold no start byte). Other UDP ports: only when the
//!    payload starts with a plausible MAVLink header (0xFD with no flag
//!    but "signed" in its incompat byte and a message id below 65536, or
//!    0xFE) whose frame fits in the datagram. Other TCP ports: no (5760 is
//!    the TCP case in practice).
//!    Ports are a hint, not the test: whatever is queued is only analysed
//!    as far as it holds CRC-checked frames of known messages
//!    (`mavwatch::frame`). Then [`Bucket`] caps the rate.
//! 3. [`MavWifi::feed`]: reassembly and analysis (`mavwatch::Watch`), and
//!    the `Frame::Threat` + `Frame::MavlinkEvidence` pair node-lr sends.
//!
//! **Link and streams.** Each direction of each UDP / TCP flow (network,
//! addresses, ports) is its own reassembly stream, so bytes from two
//! senders are never joined. The *link* the analyser judges (the scope of
//! the link-integrity kinds 16-18) is the **network**: its BSSID (the
//! transmitter address for a four-address frame). An injector on the same
//! open network as a drone is then on the drone's link, as it would be on
//! a SiK link, and two drones on two networks stay apart. The BSSID is not
//! a control-link identifier in the sense of `control ids` (it is in every
//! beacon, and the node's other Wi-Fi detectors already key on it), so the
//! link is always [`mavwatch::Link::distinct`] here and `control ids` is
//! not needed. It stays in the node: a report carries the MAVLink sender
//! and target (system/component), as from node-lr, not addresses.
//!
//! **TCP.** Segments are scanned one by one through the same per-flow
//! stream, without TCP reassembly: a lost segment costs the frames it cut
//! (they fail their checksum and are skipped), a retransmitted one repeats
//! frames that the analyser counts once (`mavwatch::analyse::RESEND_MS`),
//! and reordering is rare on one hop. Telemetry over TCP is a minority
//! (SITL, some bridges' optional TCP server); a full reassembler would cost
//! a window of buffer per connection for little.

use link::{MavlinkEvidence, ThreatKind, ThreatSighting};
use mavwatch::frame::{INCOMPAT_SIGNED, SIGNATURE_LEN, STX_V1, STX_V2};

use crate::util;

/// Usual MAVLink ports: 14550 (ground station), 14555 and 14540 (PX4
/// onboard / offboard), 14580 (PX4 SITL), 5760 (TCP, ArduPilot SITL and
/// bridges' TCP servers).
pub const MAVLINK_PORTS: [u16; 5] = [14550, 14555, 14540, 14580, 5760];

/// Most candidate datagrams queued per second, on average, and the burst
/// allowed above it ([`Bucket`]). A busy telemetry bridge sends a few dozen
/// datagrams a second; 100/s keeps up with one or two of them while the
/// task's work stays around 2 % of a core, and the burst of 10 absorbs a
/// parameter download.
pub const PER_S: u32 = 100;
pub const BURST: u32 = 10;

/// LLC/SNAP header of an IPv4 packet.
const SNAP_IPV4: [u8; 8] = [0xAA, 0xAA, 0x03, 0x00, 0x00, 0x00, 0x08, 0x00];
const IP_UDP: u8 = 17;
const IP_TCP: u8 = 6;
/// `StreamKey::system` values for Wi-Fi flows (not `ctrlwatch` systems).
const SYSTEM_UDP: u8 = 0xF1;
const SYSTEM_TCP: u8 = 0xF2;

/// One direction of one UDP or TCP flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flow {
    /// The network: BSSID, or the transmitter for a four-address frame.
    pub bssid: [u8; 6],
    /// Source address (802.11 SA).
    pub src_mac: [u8; 6],
    pub src_ip: [u8; 4],
    pub dst_ip: [u8; 4],
    pub sport: u16,
    pub dport: u16,
    pub tcp: bool,
}

impl Flow {
    /// The reassembly stream for this direction of this flow.
    pub fn stream(&self) -> mavwatch::StreamKey {
        let mut b = [0u8; 6 + 6 + 4 + 4 + 2 + 2];
        b[..6].copy_from_slice(&self.bssid);
        b[6..12].copy_from_slice(&self.src_mac);
        b[12..16].copy_from_slice(&self.src_ip);
        b[16..20].copy_from_slice(&self.dst_ip);
        b[20..22].copy_from_slice(&self.sport.to_le_bytes());
        b[22..24].copy_from_slice(&self.dport.to_le_bytes());
        mavwatch::StreamKey {
            system: if self.tcp { SYSTEM_TCP } else { SYSTEM_UDP },
            id: Some(util::hash(&b)),
            dir: 0,
        }
    }

    /// The analyser's link: the network (see the module doc).
    pub fn link(&self) -> mavwatch::Link {
        mavwatch::Link { key: util::hash(&self.bssid), distinct: true }
    }

    fn port(&self, ports: &[u16]) -> bool {
        ports.contains(&self.sport) || ports.contains(&self.dport)
    }
}

/// A UDP datagram's or TCP segment's payload, found in a data frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Datagram<'a> {
    pub flow: Flow,
    pub payload: &'a [u8],
    /// The IP or UDP length says there was more than the frame holds.
    pub truncated: bool,
}

/// Why a frame was passed over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    /// Not a data frame (or not version 0).
    NotData,
    /// Null / CF data: no body.
    NoBody,
    /// Protected (encrypted): nothing readable.
    Protected,
    /// An 802.11 fragment.
    Fragment,
    /// An A-MSDU (not unpacked).
    Amsdu,
    /// Cut short before the transport header.
    Short,
    /// Not LLC/SNAP IPv4.
    NotIpv4,
    /// A fragment of an IPv4 packet.
    IpFragment,
    /// Neither UDP nor TCP.
    Transport,
    /// No MAVLink in sight ([`plausible`]).
    NotMavlink,
}

fn u16_be(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?]))
}

fn mac(b: &[u8], i: usize) -> Option<[u8; 6]> {
    b.get(i..i + 6)?.try_into().ok()
}

/// Read the UDP / TCP payload of an unprotected 802.11 data frame. `mpdu`
/// starts at frame control; a trailing FCS is harmless (lengths come from
/// the IP header) but should be cut off by the caller where known.
pub fn parse(mpdu: &[u8]) -> Result<Datagram<'_>, Skip> {
    let f = mpdu;
    let (&fc0, &fc1) = (f.first().ok_or(Skip::Short)?, f.get(1).ok_or(Skip::Short)?);
    if fc0 & 0x0F != 0x08 {
        return Err(Skip::NotData);
    }
    let subtype = fc0 >> 4;
    if subtype & 0x4 != 0 {
        return Err(Skip::NoBody);
    }
    if fc1 & 0x40 != 0 {
        return Err(Skip::Protected);
    }
    let (to_ds, from_ds) = (fc1 & 0x01 != 0, fc1 & 0x02 != 0);
    let more_frag = fc1 & 0x04 != 0;
    let frag_no = f.get(22).ok_or(Skip::Short)? & 0x0F;
    if more_frag || frag_no != 0 {
        return Err(Skip::Fragment);
    }
    let mut hdr = 24;
    if to_ds && from_ds {
        hdr += 6;
    }
    if subtype & 0x8 != 0 {
        // QoS Control; bit 7 of its first byte: A-MSDU present.
        if f.get(hdr).ok_or(Skip::Short)? & 0x80 != 0 {
            return Err(Skip::Amsdu);
        }
        hdr += 2;
        // Order bit on a QoS data frame: a 4-byte HT Control field.
        if fc1 & 0x80 != 0 {
            hdr += 4;
        }
    }
    let (a1, a2, a3) = (mac(f, 4).ok_or(Skip::Short)?, mac(f, 10).ok_or(Skip::Short)?, mac(f, 16).ok_or(Skip::Short)?);
    let (bssid, src_mac) = match (to_ds, from_ds) {
        (false, false) => (a3, a2),
        (true, false) => (a1, a2),
        (false, true) => (a2, a3),
        // Four addresses (WDS / mesh): no BSSID; the transmitter stands in.
        (true, true) => (a2, mac(f, 24).ok_or(Skip::Short)?),
    };
    let body = f.get(hdr..).ok_or(Skip::Short)?;
    if body.len() < SNAP_IPV4.len() || body[..SNAP_IPV4.len()] != SNAP_IPV4 {
        return Err(if body.len() < SNAP_IPV4.len() { Skip::Short } else { Skip::NotIpv4 });
    }
    let ip = &body[SNAP_IPV4.len()..];
    if ip.len() < 20 {
        return Err(Skip::Short);
    }
    if ip[0] >> 4 != 4 {
        return Err(Skip::NotIpv4);
    }
    let ihl = usize::from(ip[0] & 0x0F) * 4;
    let total = usize::from(u16_be(ip, 2).ok_or(Skip::Short)?);
    // More-fragments flag or a fragment offset.
    if u16_be(ip, 6).ok_or(Skip::Short)? & 0x3FFF != 0 {
        return Err(Skip::IpFragment);
    }
    if ihl < 20 || total < ihl {
        return Err(Skip::NotIpv4);
    }
    let proto = ip[9];
    let src_ip: [u8; 4] = ip[12..16].try_into().map_err(|_| Skip::Short)?;
    let dst_ip: [u8; 4] = ip[16..20].try_into().map_err(|_| Skip::Short)?;
    let mut truncated = total > ip.len();
    let l4 = ip.get(ihl..total.min(ip.len())).ok_or(Skip::Short)?;
    let (sport, dport, payload, tcp) = match proto {
        IP_UDP => {
            if l4.len() < 8 {
                return Err(Skip::Short);
            }
            let ulen = usize::from(u16_be(l4, 4).ok_or(Skip::Short)?);
            if ulen < 8 {
                return Err(Skip::Short);
            }
            truncated |= ulen > l4.len();
            (u16_be(l4, 0).ok_or(Skip::Short)?, u16_be(l4, 2).ok_or(Skip::Short)?, &l4[8..ulen.min(l4.len())], false)
        }
        IP_TCP => {
            if l4.len() < 20 {
                return Err(Skip::Short);
            }
            let off = usize::from(l4[12] >> 4) * 4;
            if off < 20 {
                return Err(Skip::Short);
            }
            let payload = l4.get(off..).ok_or(Skip::Short)?;
            (u16_be(l4, 0).ok_or(Skip::Short)?, u16_be(l4, 2).ok_or(Skip::Short)?, payload, true)
        }
        _ => return Err(Skip::Transport),
    };
    Ok(Datagram { flow: Flow { bssid, src_mac, src_ip, dst_ip, sport, dport, tcp }, payload, truncated })
}

/// Cheap "might carry MAVLink?" test on a parsed datagram (see the module
/// doc). False for an empty payload.
pub fn plausible(d: &Datagram<'_>) -> bool {
    let p = d.payload;
    if p.is_empty() {
        return false;
    }
    // On a MAVLink port every payload counts: a bridge forwarding raw
    // serial chunks, or a TCP stream, cuts frames anywhere, and the middle
    // of a long frame holds no start byte.
    d.flow.port(&MAVLINK_PORTS) || (!d.flow.tcp && starts_with_header(p))
}

/// A plausible MAVLink header at the start of `p`, whose frame fits in `p`.
fn starts_with_header(p: &[u8]) -> bool {
    match p {
        [STX_V2, len, incompat, _, _, _, _, _, _, msg_hi, ..] => {
            let sig = if incompat & INCOMPAT_SIGNED != 0 { SIGNATURE_LEN } else { 0 };
            // MAVLink message ids in use stay below 65536.
            incompat & !INCOMPAT_SIGNED == 0 && *msg_hi == 0 && 12 + usize::from(*len) + sig <= p.len()
        }
        [STX_V1, len, ..] => 8 + usize::from(*len) <= p.len(),
        _ => false,
    }
}

/// [`parse`] then [`plausible`]: the receive callback's whole test.
pub fn candidate(mpdu: &[u8]) -> Result<Datagram<'_>, Skip> {
    let d = parse(mpdu)?;
    if plausible(&d) { Ok(d) } else { Err(Skip::NotMavlink) }
}

/// Token bucket: on average [`PER_S`] a second, bursts of [`BURST`].
#[derive(Debug, Clone, Copy)]
pub struct Bucket {
    /// Millionths of a token.
    micro: u64,
    last_us: u64,
}

impl Default for Bucket {
    fn default() -> Self {
        Self::new()
    }
}

impl Bucket {
    const ONE: u64 = 1_000_000;

    pub const fn new() -> Self {
        Self { micro: BURST as u64 * Self::ONE, last_us: 0 }
    }

    /// Take a token at node time `now_us` if there is one.
    pub fn allow(&mut self, now_us: u64) -> bool {
        let dt = now_us.saturating_sub(self.last_us);
        self.last_us = self.last_us.max(now_us);
        let cap = u64::from(BURST) * Self::ONE;
        self.micro = self.micro.saturating_add(dt.saturating_mul(u64::from(PER_S))).min(cap);
        if self.micro >= Self::ONE {
            self.micro -= Self::ONE;
            true
        } else {
            false
        }
    }
}

/// Where and when a queued datagram was heard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meta {
    pub flow: Flow,
    pub channel: u8,
    pub rssi: i8,
    pub up_us: u64,
    /// Only a prefix of the payload was kept: analysed without reassembly.
    pub truncated: bool,
}

/// Totals for a node's stats.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    pub datagrams: u32,
    pub findings: u32,
}

/// The task-side detector: `mavwatch::Watch` (about 8 KB) plus totals.
#[derive(Debug, Clone)]
pub struct MavWifi {
    watch: mavwatch::Watch,
    pub totals: Totals,
}

impl Default for MavWifi {
    fn default() -> Self {
        Self::new()
    }
}

impl MavWifi {
    pub const fn new() -> Self {
        Self { watch: mavwatch::Watch::new(), totals: Totals { datagrams: 0, findings: 0 } }
    }

    /// Analyse one queued payload. `out` gets each finding as the threat
    /// and the evidence frame node-lr sends for it, in that order.
    pub fn feed(&mut self, m: &Meta, payload: &[u8], out: &mut dyn FnMut(&ThreatSighting, &MavlinkEvidence)) {
        self.totals.datagrams = self.totals.datagrams.wrapping_add(1);
        let now_ms = m.up_us / 1000;
        let link = m.flow.link();
        let findings = &mut self.totals.findings;
        let mut on = |f: &mavwatch::Finding, fr: &mavwatch::Frame<'_>| {
            *findings = findings.wrapping_add(1);
            let (threat, ev) = report(m, f, fr, now_ms);
            out(&threat, &ev);
        };
        if m.truncated {
            // A cut payload must not leave half a frame in the stream.
            self.watch.feed_frames(link, payload, now_ms, &mut on);
        } else {
            self.watch.feed_on(link, m.flow.stream(), payload, now_ms, &mut on);
        }
    }
}

/// The threat and evidence frames for one finding, laid out exactly as
/// node-lr's (`source` / `target` = system and component in the last two
/// bytes), with the Wi-Fi channel and its centre frequency.
pub fn report(m: &Meta, f: &mavwatch::Finding, fr: &mavwatch::Frame<'_>, now_ms: u64) -> (ThreatSighting, MavlinkEvidence) {
    let addr = |(sys, comp): (u8, u8)| [0, 0, 0, 0, sys, comp];
    let threat = ThreatSighting {
        kind: ThreatKind::MavlinkAttack,
        channel: m.channel,
        freq: util::channel_mhz(m.channel) * 10,
        width_khz: 0,
        level: m.rssi,
        count: f.hits,
        window_ms: now_ms.saturating_sub(f.first_ms).min(u64::from(u32::MAX)) as u32,
        source: addr((f.sysid, f.compid)),
        target: f.target.map_or([0; 6], addr),
        // Kind code, plus mavwatch::analyse::DETAIL_SUSPECT for one
        // uncorroborated frame.
        detail: f.detail(),
        ssid: f.excerpt.clone(),
        uptime_us: m.up_us,
    };
    let n = fr.bytes.len().min(link::MAX_MAVLINK_EVIDENCE);
    let ev = MavlinkEvidence {
        detail: f.kind.code(),
        sysid: f.sysid,
        compid: f.compid,
        msgid: f.msgid,
        truncated: fr.bytes.len() > n,
        uptime_us: m.up_us,
        frame: heapless::Vec::from_slice(&fr.bytes[..n]).unwrap_or_default(),
    };
    (threat, ev)
}

#[cfg(test)]
mod tests;
