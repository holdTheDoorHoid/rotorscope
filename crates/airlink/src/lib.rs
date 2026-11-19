//! ESP-NOW between radio nodes and a gateway (protocol "RS").
//!
//! The wired link is a byte stream of COBS-delimited `link` frames. Over the
//! air we keep exactly those bytes and only add a 6-byte header: a node packs
//! as many whole frames as fit into one ESP-NOW packet, the gateway relays
//! the packet to the hub as it is ([`relay`]), and the hub splits the payload
//! back into frames and keeps those a node may originate
//! (`link::origin::node_batch`). One packet per batch rather
//! than per frame is the main efficiency lever: every ESP-NOW packet costs a
//! preamble, a MAC header, an ACK and a contention window regardless of size,
//! and a Wi-Fi node is deaf to drones while it transmits.
//!
//! Three packet kinds:
//! - [`Kind::Beacon`], broadcast by the gateway, about twice a second on the
//!   gateway channel. Nodes learn the gateway's address from it, then send
//!   unicast, which gets a MAC-layer ACK and hardware retries (broadcast gets
//!   neither).
//! - [`Kind::Frames`], node to hub through the gateway: `seq` counts
//!   packets, so the receiver can count what was lost after the radio's own
//!   retries gave up.
//! - [`Kind::Command`], hub to one node through the gateway (unicast,
//!   MAC-acked): hub commands (`link::Command`, COBS frames exactly as the
//!   hub wrote them). `seq` counts per node, so a node can drop a retry it
//!   already has.
//!
//! Nothing here is secret: the same detections a node hears anyone nearby can
//! hear. What this module builds is **version 1**, the unauthenticated packet
//! an open kit uses. A sealed kit uses [`v3`] (docs/LINK-KEYS-DESIGN.md):
//! per-node keys from `kitkeys`, kept in each board's provisioning partition
//! and never in a firmware image, a tick clock, a multi-tag beacon, and a
//! gateway that holds no key ([`relay`]).
//!
//! Version 2 (one kit key compiled into every image) is retired. No build
//! carries a key any more, and nothing here can make or check a version 2
//! tag. A version 2 packet is still recognised by its header alone and
//! refused as `old_firmware` ([`v3::inspect`]): that is how a board still on
//! the old firmware shows up.
#![cfg_attr(not(test), no_std)]

pub mod relay;
pub mod v3;

/// First two bytes of every packet.
pub const MAGIC: [u8; 2] = *b"RS";
/// Version 1: the unauthenticated (open-kit) packet this module builds.
pub const VERSION: u8 = 1;
/// Version 2: the retired kit-key packet (a header like version 1's, the
/// payload, a 16-byte stamp and a 12-byte tag). Recognised by its header and
/// refused as old firmware; never made or checked.
pub const VERSION_KIT_KEY: u8 = 2;
/// Header: magic (2), version (1), kind (1), sequence (2, little-endian).
pub const HEADER: usize = 6;
/// Largest packet we send. ESP-NOW v2 allows 1470 bytes; both ends here run
/// the same Espressif blobs, which support v2. Past ~1 KB the airtime at the
/// 1 Mbit/s default rate (~8 ms) starts to cost the sender real listening time.
pub const MAX_PACKET: usize = 1024;
/// Largest batch payload. Room for the version 3 trailer ([`v3::TRAILER`]) is
/// always kept, so a batch can be sealed or not without repacking.
pub const MAX_PAYLOAD: usize = MAX_PACKET - HEADER - v3::TRAILER;
/// A version 1 beacon: header plus the channel.
pub const BEACON_LEN: usize = HEADER + 1;
/// The 2.4 GHz channel the gateway listens and beacons on. Nodes relayed
/// through it can only send while tuned here, so every channel plan for
/// such a node visits it (`coverage::plans`). One constant for the gateway,
/// the nodes and the coordinator, so they cannot drift apart.
pub const GATEWAY_CHANNEL: u8 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Beacon = 1,
    Frames = 2,
    Command = 3,
}

/// A parsed version 1 packet. `payload` is the bytes after the header.
#[derive(Debug, PartialEq, Eq)]
pub struct Packet<'a> {
    pub kind: Kind,
    pub seq: u16,
    pub payload: &'a [u8],
}

/// Parse a received version 1 (open-kit) packet. `None` for anything else:
/// not ours (other ESP-NOW users share the air), version 3 (read it with
/// [`v3::inspect`]), or the retired version 2 ([`v3::inspect`] names it
/// `old_firmware`).
pub fn parse(data: &[u8]) -> Option<Packet<'_>> {
    if data.len() < HEADER || data[..2] != MAGIC || data[2] != VERSION {
        return None;
    }
    let end = data.len();
    let kind = match data[3] {
        1 => Kind::Beacon,
        2 => Kind::Frames,
        3 => Kind::Command,
        _ => return None,
    };
    let seq = u16::from_le_bytes([data[4], data[5]]);
    Some(Packet { kind, seq, payload: &data[HEADER..end] })
}

fn header(buf: &mut [u8], kind: Kind, seq: u16) {
    buf[..2].copy_from_slice(&MAGIC);
    buf[2] = VERSION;
    buf[3] = kind as u8;
    buf[4..6].copy_from_slice(&seq.to_le_bytes());
}

/// Gateway beacon: header plus the gateway's channel, so a node that heard it
/// on a neighbouring channel (2.4 GHz channels overlap) knows where to send.
/// `buf` needs [`BEACON_LEN`] bytes.
pub fn beacon(buf: &mut [u8], seq: u16, channel: u8) -> &[u8] {
    header(buf, Kind::Beacon, seq);
    buf[HEADER] = channel;
    &buf[..BEACON_LEN]
}

/// The channel a beacon announces.
pub fn beacon_channel(p: &Packet<'_>) -> Option<u8> {
    (p.kind == Kind::Beacon).then(|| p.payload.first().copied()).flatten()
}

/// A command packet for one node: header plus `frames`, one or more whole
/// COBS command frames. `None` if `frames` is not whole frames or does not
/// fit in `buf` or in [`MAX_PAYLOAD`].
pub fn command<'a>(buf: &'a mut [u8], seq: u16, frames: &[u8]) -> Option<&'a [u8]> {
    let whole = frames.last() == Some(&0);
    let len = HEADER + frames.len();
    if !whole || len > buf.len() || len > HEADER + MAX_PAYLOAD {
        return None;
    }
    header(buf, Kind::Command, seq);
    buf[HEADER..len].copy_from_slice(frames);
    Some(&buf[..len])
}

/// The whole COBS frames in a command (or batch) packet's payload; a
/// truncated tail is skipped.
pub fn frames<'a>(p: &Packet<'a>) -> impl Iterator<Item = &'a [u8]> {
    p.payload.split_inclusive(|&b| b == 0).filter(|f| f.last() == Some(&0))
}

/// Packs whole COBS frames into one packet.
pub struct Batch {
    buf: [u8; MAX_PACKET],
    len: usize,
    frames: u16,
}

impl Default for Batch {
    fn default() -> Self {
        Self::new()
    }
}

impl Batch {
    pub const fn new() -> Self {
        Self { buf: [0; MAX_PACKET], len: HEADER, frames: 0 }
    }

    pub fn is_empty(&self) -> bool {
        self.frames == 0
    }

    pub fn frames(&self) -> u16 {
        self.frames
    }

    /// Payload bytes so far.
    pub fn payload_len(&self) -> usize {
        self.len - HEADER
    }

    /// Whether `frame` (one COBS frame with its trailing zero) still fits.
    pub fn fits(&self, frame: &[u8]) -> bool {
        self.len + frame.len() <= HEADER + MAX_PAYLOAD
    }

    /// Append one whole frame. Returns false, unchanged, if it does not fit
    /// (send the batch first) or is not a single delimited frame.
    pub fn push(&mut self, frame: &[u8]) -> bool {
        let one_frame = frame.last() == Some(&0) && !frame[..frame.len() - 1].contains(&0);
        if !one_frame || !self.fits(frame) {
            return false;
        }
        self.buf[self.len..self.len + frame.len()].copy_from_slice(frame);
        self.len += frame.len();
        self.frames += 1;
        true
    }

    /// The packet to send, stamped with `seq`. Call [`Batch::clear`] once it
    /// was delivered (keep it to retry if it was not).
    pub fn packet(&mut self, seq: u16) -> &[u8] {
        header(&mut self.buf, Kind::Frames, seq);
        &self.buf[..self.len]
    }

    /// The batch as a version 3 packet sealed by `node` at node uptime
    /// `now_ms` ([`v3::Node::seal_frames`]). Call again for every retry: each
    /// attempt gets a fresh stamp. `None` while the node may not send yet.
    pub fn sealed_v3(&mut self, seq: u16, node: &mut v3::Node, now_ms: u64) -> Option<&[u8]> {
        let n = node.seal_frames(&mut self.buf, self.len, seq, now_ms)?;
        Some(&self.buf[..n])
    }

    pub fn clear(&mut self) {
        self.len = HEADER;
        self.frames = 0;
    }
}

/// Gateway-side loss counting for one node, from packet sequence numbers.
#[derive(Debug, Default, Clone, Copy)]
pub struct SeqTracker {
    last: Option<u16>,
    pub received: u32,
    /// Packets skipped over (sent by the node but never received).
    pub lost: u32,
    /// Same sequence number again: an ESP-NOW retry whose ACK was lost.
    pub duplicates: u32,
}

impl SeqTracker {
    /// Record `seq`. Returns false for a duplicate, which the caller drops.
    pub fn on_packet(&mut self, seq: u16) -> bool {
        if let Some(last) = self.last {
            let gap = seq.wrapping_sub(last);
            if gap == 0 {
                self.duplicates += 1;
                return false;
            }
            // A big jump backwards is a node reboot: start over, count nothing.
            if gap < 0x8000 {
                self.lost += u32::from(gap - 1);
            }
        }
        self.last = Some(seq);
        self.received += 1;
        true
    }
}

#[cfg(test)]
mod fuzz_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_round_trip() {
        let mut b = Batch::new();
        assert!(b.push(&[1, 2, 3, 0]));
        assert!(b.push(&[9, 0]));
        assert!(!b.push(&[5, 0, 6, 0]), "two frames in one push");
        assert!(!b.push(&[5, 6]), "no delimiter");
        assert_eq!(b.frames(), 2);
        let pkt = b.packet(0x1234).to_vec();
        let p = parse(&pkt).unwrap();
        assert_eq!(p, Packet { kind: Kind::Frames, seq: 0x1234, payload: &[1, 2, 3, 0, 9, 0] });
        b.clear();
        assert!(b.is_empty());
    }

    #[test]
    fn batch_stops_at_max() {
        let mut b = Batch::new();
        let mut frame = [7u8; 300];
        frame[299] = 0;
        let mut n = 0;
        while b.push(&frame) {
            n += 1;
        }
        assert_eq!(n, MAX_PAYLOAD / 300);
        assert!(b.packet(0).len() <= MAX_PACKET);
    }

    #[test]
    fn beacon_round_trip() {
        let mut buf = [0u8; BEACON_LEN];
        let pkt = beacon(&mut buf, 3, 6).to_vec();
        let p = parse(&pkt).unwrap();
        assert_eq!(p.kind, Kind::Beacon);
        assert_eq!(beacon_channel(&p), Some(6));
    }

    #[test]
    fn rejects_foreign_packets() {
        assert!(parse(b"").is_none());
        assert!(parse(b"XX\x01\x02\x00\x00").is_none());
        assert!(parse(b"RS\x03\x02\x00\x00").is_none(), "other version");
        assert!(parse(b"RS\x02\x02\x00\x00").is_none(), "version 2 (retired)");
        assert!(parse(b"RS\x01\x09\x00\x00").is_none(), "unknown kind");
    }

    #[test]
    fn command_round_trip() {
        let mut buf = [0u8; 64];
        let pkt = command(&mut buf, 0xBEEF, &[4, 1, 2, 3, 0, 2, 9, 0]).unwrap().to_vec();
        let p = parse(&pkt).unwrap();
        assert_eq!((p.kind, p.seq), (Kind::Command, 0xBEEF));
        let f: std::vec::Vec<&[u8]> = frames(&p).collect();
        assert_eq!(f, [&[4, 1, 2, 3, 0][..], &[2, 9, 0][..]]);
        assert!(beacon_channel(&p).is_none(), "a node must not take it for a beacon");
        assert!(command(&mut buf, 0, &[1, 2]).is_none(), "not a whole frame");
        assert!(command(&mut buf, 0, &[]).is_none());
        assert!(command(&mut [0u8; 8], 0, &[1, 2, 3, 0]).is_none(), "too big for buf");
        // A truncated tail is not handed out.
        let mut t = pkt.clone();
        t.push(7);
        let p = parse(&t).unwrap();
        assert_eq!(frames(&p).count(), 2);
    }

    #[test]
    fn command_carries_a_real_assignment() {
        let mut plan = heapless::Vec::new();
        plan.extend([(149u32, 1000u16), (6, 40)]);
        let cmd = link::Command::Assign(link::Assignment { role: link::NodeRole::Wifi5, plan, id: 0x1234 });
        let mut enc = [0u8; link::MAX_FRAME];
        let wire = link::encode_command(&cmd, &mut enc).unwrap();
        let mut buf = [0u8; MAX_PACKET];
        let pkt = command(&mut buf, 1, wire).unwrap().to_vec();
        let p = parse(&pkt).unwrap();
        let mut dec = link::CommandDecoder::new();
        let mut got = std::vec::Vec::new();
        for f in frames(&p) {
            dec.feed(f, |r| got.push(r));
        }
        assert_eq!(got, [Ok(cmd)]);
    }

    #[test]
    fn seq_counts_loss_dups_and_reboots() {
        let mut t = SeqTracker::default();
        assert!(t.on_packet(10));
        assert!(t.on_packet(11));
        assert!(!t.on_packet(11));
        assert!(t.on_packet(14));
        assert_eq!((t.received, t.lost, t.duplicates), (3, 2, 1));
        assert!(t.on_packet(0), "reboot");
        assert_eq!(t.lost, 2);
        assert!(t.on_packet(u16::MAX));
        assert!(t.on_packet(1), "wraps");
        assert_eq!(t.lost, 3);
    }
}
