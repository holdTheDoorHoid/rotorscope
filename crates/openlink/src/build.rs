//! Fake frames for the bench: one per system, shaped exactly as
//! [`classify`](crate::classify) expects, so the test beacon
//! (`firmware/rid-beacon`) and the unit tests send the same bytes.
//!
//! Behind the `build` feature; the receivers never need it.
//!
//! # Conduct
//!
//! These frames copy a system's *header shape*, never a real link. Every
//! identifier is a deliberately odd value starting `7E57` ("TEST"):
//!
//! | System | Field | Value | Why it cannot be a real link |
//! |---|---|---|---|
//! | wfb-ng | link ID | [`FAKE_WFB_LINK_ID`] `0x7E57ED` | not "default"'s `0x7505D6`; chosen by hand, not a SHA-1 of any domain we know |
//! | OpenHD | nonce high bytes | [`FAKE_OPENHD_NONCE_HI`] `0x7E` | a real nonce starts below 2^32 and would need 2^38 packets to set this byte |
//! | RubyFPV | vehicle ID | [`FAKE_RUBY_VEHICLE_ID`] `0x7E57_0001` | real IDs are random; this one spells TEST |
//! | DroneBridge | comm ID | [`FAKE_DRONEBRIDGE_COMM_ID`] `0x7E` | not DroneBridge's default |
//!
//! The payload is the ASCII text [`FAKE_PAYLOAD`] repeated, so a capture
//! shows at a glance that it is a bench test. Nothing is encrypted and
//! nothing would decode as video or MAVLink.
//!
//! Every builder writes the MPDU from frame control to the end of the body,
//! *without* an FCS (the radio appends it), and returns its length. The
//! sequence-control field is written by the builder (`seq`), so the caller
//! must tell the driver not to overwrite it: DroneBridge has no MAC header,
//! and bytes 22-23 of its frame are payload.

use crate::{RUBY_MAC, System, WFB_PREFIX};

/// wfb-ng link ID on the fake frames. NOT the default `0x7505D6`.
pub const FAKE_WFB_LINK_ID: u32 = 0x7E_57_ED;
/// wfb-ng radio port: 0, the first video downlink stream.
pub const FAKE_WFB_PORT: u8 = 0;
/// OpenHD nonce byte 4 (A3 byte 1). Real OpenHD nonces start below 2^32, so
/// this byte stays zero in practice; a non-zero value marks the frame fake
/// while still satisfying the classifier (which only checks bytes 5-7).
pub const FAKE_OPENHD_NONCE_HI: u8 = 0x7E;
/// OpenHD radio port byte: stream index 1, unencrypted (bit 0 clear).
pub const FAKE_OPENHD_PORT: u8 = 0x02;
/// RubyFPV vehicle ID (sent as the source on a downlink).
pub const FAKE_RUBY_VEHICLE_ID: u32 = 0x7E57_0001;
/// RubyFPV downlink radio port (`RADIO_PORT_ROUTER_DOWNLINK`).
pub const FAKE_RUBY_PORT: u8 = 0x0F;
/// DroneBridge comm ID.
pub const FAKE_DRONEBRIDGE_COMM_ID: u8 = 0x7E;
/// DroneBridge port 2: telemetry.
pub const FAKE_DRONEBRIDGE_PORT: u8 = 2;
/// Filler text for every body.
pub const FAKE_PAYLOAD: &[u8] = b"ROTORSCOPE BENCH TEST - NOT A REAL LINK ";

/// Body length after each system's own header. Short: the point is the
/// header, and short frames keep air time down.
const FILL: usize = 48;
/// The largest frame any builder writes.
pub const MAX_LEN: usize = 24 + 24 + FILL;

/// The fake frame for `system`. `seq` is a running counter (it moves the
/// sequence number, wfb-ng's block index and OpenHD's nonce, as real links
/// do). Returns the MPDU length, or `None` if `out` is shorter than
/// [`MAX_LEN`].
pub fn fake_frame(system: System, seq: u32, out: &mut [u8]) -> Option<usize> {
    let out = out.get_mut(..MAX_LEN)?;
    out.fill(0);
    let n = match system {
        System::WfbNg => wfb_ng(seq, out),
        System::OpenHd => openhd(seq, out),
        System::RubyFpv => rubyfpv(seq, out),
        System::DroneBridge => dronebridge(seq, out),
    };
    Some(n)
}

/// The cleartext identifier [`classify`](crate::classify) reports for the
/// fake frame of `system` (what a bench test checks for).
pub const fn fake_id(system: System) -> Option<u32> {
    match system {
        System::WfbNg => Some(FAKE_WFB_LINK_ID),
        System::OpenHd => None,
        System::RubyFpv => Some(FAKE_RUBY_VEHICLE_ID),
        System::DroneBridge => Some(FAKE_DRONEBRIDGE_COMM_ID as u32),
    }
}

fn fill(out: &mut [u8]) {
    for (b, &t) in out.iter_mut().zip(FAKE_PAYLOAD.iter().cycle()) {
        *b = t;
    }
}

/// `fc` `00 00` A1 A2 A3 seq-ctrl: the 24-byte header the three ToDS systems use.
fn header24(out: &mut [u8], fc: [u8; 2], a1: [u8; 6], a2: [u8; 6], a3: [u8; 6], seq: u32) {
    out[0..2].copy_from_slice(&fc);
    out[2..4].copy_from_slice(&[0, 0]);
    out[4..10].copy_from_slice(&a1);
    out[10..16].copy_from_slice(&a2);
    out[16..22].copy_from_slice(&a3);
    // Sequence number in bits 4-15, fragment 0.
    out[22..24].copy_from_slice(&(((seq & 0x0FFF) << 4) as u16).to_le_bytes());
}

/// wfb-ng data packet: `08 01`, A2 = A3 = `57:42` + big-endian
/// `(link_id << 8) | port`; body = packet type 1 (data), the 8-byte block
/// header, then filler where the ciphertext and tag would be.
fn wfb_ng(seq: u32, out: &mut [u8]) -> usize {
    let ch = ((FAKE_WFB_LINK_ID << 8) | u32::from(FAKE_WFB_PORT)).to_be_bytes();
    let mac = [WFB_PREFIX[0], WFB_PREFIX[1], ch[0], ch[1], ch[2], ch[3]];
    header24(out, [0x08, 0x01], [0xFF; 6], mac, mac, seq);
    out[24] = 1; // WFB_PACKET_DATA
    // data_nonce: block index << 8 | fragment index, big-endian.
    out[25..33].copy_from_slice(&(u64::from(seq) << 8).to_be_bytes());
    fill(&mut out[33..33 + FILL]);
    33 + FILL
}

/// OpenHD: A2 = `[01][nonce 0-3][port]`, A3 = `[01][nonce 4-7][port]`, air
/// side, little-endian nonce.
fn openhd(seq: u32, out: &mut [u8]) -> usize {
    let nonce = (u64::from(FAKE_OPENHD_NONCE_HI) << 32) | u64::from(seq);
    let n = nonce.to_le_bytes();
    let a2 = [0x01, n[0], n[1], n[2], n[3], FAKE_OPENHD_PORT];
    let a3 = [0x01, n[4], n[5], n[6], n[7], FAKE_OPENHD_PORT];
    header24(out, [0x08, 0x01], [0xFF; 6], a2, a3, seq);
    fill(&mut out[24..24 + FILL]);
    24 + FILL
}

/// RubyFPV downlink: A1 = `[port << 4 | 0x0F] ff..` (= broadcast for port
/// 0x0F), A2 = A3 = `13:12:34:56:78:90`, then one unencrypted
/// `t_packet_header` naming the fake vehicle as the source.
fn rubyfpv(seq: u32, out: &mut [u8]) -> usize {
    let a1 = [(FAKE_RUBY_PORT << 4) | 0x0F, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];
    header24(out, [0x08, 0x01], a1, RUBY_MAC, RUBY_MAC, seq);
    let h = &mut out[24..48];
    // uCRC (0..4) left zero: nothing checks it, and a zero CRC is one more
    // sign this is not a real Ruby packet.
    h[4] = 0; // packet_flags: no encryption
    h[5] = 1; // packet_type
    h[6..10].copy_from_slice(&seq.to_le_bytes()); // stream_packet_idx
    // packet_flags_extended (10..12) zero.
    h[12..14].copy_from_slice(&((24 + FILL) as u16).to_le_bytes()); // total_length
    h[14..16].copy_from_slice(&(seq as u16).to_le_bytes()); // radio_link_packet_index
    h[16..20].copy_from_slice(&FAKE_RUBY_VEHICLE_ID.to_le_bytes()); // vehicle_id_src
    h[20..24].copy_from_slice(&0u32.to_le_bytes()); // vehicle_id_dest
    fill(&mut out[48..48 + FILL]);
    48 + FILL
}

/// DroneBridge raw v2: `08 00 00 00`, direction 3 (to the ground station),
/// comm ID, port, little-endian payload length, sequence, payload. No MAC
/// header: the frame is exactly 10 + payload bytes.
fn dronebridge(seq: u32, out: &mut [u8]) -> usize {
    let len = (FILL as u16).to_le_bytes();
    out[..10].copy_from_slice(&[
        0x08,
        0x00,
        0x00,
        0x00,
        0x03,
        FAKE_DRONEBRIDGE_COMM_ID,
        FAKE_DRONEBRIDGE_PORT,
        len[0],
        len[1],
        seq as u8,
    ]);
    fill(&mut out[10..10 + FILL]);
    10 + FILL
}
