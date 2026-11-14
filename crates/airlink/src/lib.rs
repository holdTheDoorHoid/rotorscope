//! ESP-NOW between radio nodes and a gateway (protocol "RS" v1).
//!
//! The wired link is a byte stream of COBS-delimited `link` frames. Over the
//! air we keep exactly those bytes and only add a 5-byte header: a node packs
//! as many whole frames as fit into one ESP-NOW packet, and the gateway feeds
//! the payload straight into a `link::Decoder`. One packet per batch rather
//! than per frame is the main efficiency lever: every ESP-NOW packet costs a
//! preamble, a MAC header, an ACK and a contention window regardless of size,
//! and a Wi-Fi node is deaf to drones while it transmits.
//!
//! Two packet kinds:
//! - [`Kind::Beacon`], gateway to broadcast, about twice a second on the
//!   gateway channel. Nodes learn the gateway's address from it, then send
//!   unicast, which gets a MAC-layer ACK and hardware retries (broadcast gets
//!   neither).
//! - [`Kind::Frames`], node to gateway: `seq` counts packets, so the gateway
//!   can count what was lost after the radio's own retries gave up.
//!
//! Nothing here is secret or authenticated: the same detections a node hears
//! anyone nearby can hear. A forged packet could inject a fake track; see
//! docs/ESPNOW.md for why that is accepted for now.
#![cfg_attr(not(test), no_std)]

/// First two bytes of every packet.
pub const MAGIC: [u8; 2] = *b"RS";
/// Bump on any incompatible change.
pub const VERSION: u8 = 1;
/// Header: magic (2), version (1), kind (1), sequence (2, little-endian).
pub const HEADER: usize = 6;
/// Largest packet we send. ESP-NOW v2 allows 1470 bytes; both ends here run
/// the same Espressif blobs, which support v2. Past ~1 KB the airtime at the
/// 1 Mbit/s default rate (~8 ms) starts to cost the sender real listening time.
pub const MAX_PACKET: usize = 1024;
/// Largest batch payload.
pub const MAX_PAYLOAD: usize = MAX_PACKET - HEADER;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Beacon = 1,
    Frames = 2,
}

/// A parsed packet. `payload` is the bytes after the header.
#[derive(Debug, PartialEq, Eq)]
pub struct Packet<'a> {
    pub kind: Kind,
    pub seq: u16,
    pub payload: &'a [u8],
}

/// Parse a received ESP-NOW payload. `None` for anything that is not ours
/// (other ESP-NOW users share the air) or is from another protocol version.
pub fn parse(data: &[u8]) -> Option<Packet<'_>> {
    if data.len() < HEADER || data[..2] != MAGIC || data[2] != VERSION {
        return None;
    }
    let kind = match data[3] {
        1 => Kind::Beacon,
        2 => Kind::Frames,
        _ => return None,
    };
    let seq = u16::from_le_bytes([data[4], data[5]]);
    Some(Packet { kind, seq, payload: &data[HEADER..] })
}

fn header(buf: &mut [u8], kind: Kind, seq: u16) {
    buf[..2].copy_from_slice(&MAGIC);
    buf[2] = VERSION;
    buf[3] = kind as u8;
    buf[4..6].copy_from_slice(&seq.to_le_bytes());
}

/// Gateway beacon: header plus the gateway's channel, so a node that heard it
/// on a neighbouring channel (2.4 GHz channels overlap) knows where to send.
pub fn beacon(buf: &mut [u8; HEADER + 1], seq: u16, channel: u8) -> &[u8] {
    header(buf, Kind::Beacon, seq);
    buf[HEADER] = channel;
    &buf[..]
}

/// The channel a beacon announces.
pub fn beacon_channel(p: &Packet<'_>) -> Option<u8> {
    (p.kind == Kind::Beacon).then(|| p.payload.first().copied()).flatten()
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
        self.len + frame.len() <= MAX_PACKET
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
        let mut buf = [0u8; HEADER + 1];
        let pkt = beacon(&mut buf, 3, 6).to_vec();
        let p = parse(&pkt).unwrap();
        assert_eq!(p.kind, Kind::Beacon);
        assert_eq!(beacon_channel(&p), Some(6));
    }

    #[test]
    fn rejects_foreign_packets() {
        assert!(parse(b"").is_none());
        assert!(parse(b"XX\x01\x02\x00\x00").is_none());
        assert!(parse(b"RS\x02\x02\x00\x00").is_none(), "other version");
        assert!(parse(b"RS\x01\x09\x00\x00").is_none(), "unknown kind");
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
