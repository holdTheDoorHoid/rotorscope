//! Recognising open-source digital FPV and telemetry links on the air.
//!
//! wfb-ng (and OpenIPC, which uses it), OpenHD, RubyFPV and DroneBridge for
//! Linux all put a Wi-Fi card in monitor mode and *inject* ordinary-looking
//! 802.11 frames with a fixed, home-made header. Their payloads are encrypted
//! or simply not our business; their headers are distinctive enough to say
//! "a wfb-ng video downlink is on the air here" from one frame.
//!
//! [`classify`] takes one raw frame as an ESP32 promiscuous callback delivers
//! it (FCS slot included) and returns an [`OpenLink`], or `None`. It only
//! reads the 802.11 header and, for two systems, a few cleartext bytes after
//! it; it never decodes video or MAVLink. [`Sightings`] is the small table a
//! node keeps to count packets per link and rate-limit its reports.
//!
//! `no_std`, no allocator, no dependencies (serde behind a feature, for the
//! node-to-hub wire format).
//!
//! # Signatures
//!
//! Every byte rule below comes from the project's own source; the reference
//! is next to each rule in [`classify_mpdu`]. All offsets are into the MPDU
//! (frame control first, FCS removed).
//!
//! | System | Frame | Addresses | Payload |
//! |---|---|---|---|
//! | wfb-ng / OpenIPC | `08 01` (data, ToDS) or `b4 01` (RTS) | A1 broadcast; A2 = A3 = `57:42:LL:LL:LL:PP` | byte 0 = 1 (data) or 2 (session) |
//! | OpenHD | `08 01` | A1 broadcast; A2 = `[1\|2][nonce lo][port]`, A3 = `[same][nonce hi][same]` | encrypted |
//! | RubyFPV | `08 01` | A1 = `[port<<4 \| 0x0F] ff ff ff ff ff`; A2 = A3 = `13:12:34:56:78:90` | Ruby header; vehicle IDs in clear unless the encryption flag is set |
//! | DroneBridge | `08 00` or `b4 00`, duration 0 | none: a 10-byte header replaces them | direction, comm ID, port, length |
//!
//! # False positives
//!
//! The rules are built so that ordinary Wi-Fi cannot match, not merely so
//! that it rarely does:
//!
//! - **A ToDS frame with a broadcast receiver address does not occur in normal
//!   traffic.** A station sends ToDS frames to its access point, so A1 is the
//!   BSSID. Broadcast ARP and DHCP from a station still go *to the AP* (A1 =
//!   BSSID, A3 = ff:ff:..); when the AP repeats them it sets FromDS, not ToDS.
//!   Mesh and WDS frames set both bits and are rejected. wfb-ng, OpenHD and
//!   RubyFPV all need this shape, and it is checked first.
//! - **`57:42` cannot be a real station's address.** 0x57 is easy to misread
//!   as a unicast, locally administered first byte, but it is `0101_0111`:
//!   bit 0 (the I/G bit) is set, so `57:42:..` is a *group* address, as
//!   wfb-ng's own header comment says. A transmitter
//!   address (A2) is always an individual address, and a phone's randomised
//!   MAC sets bit 1 and clears bit 0, so it can never begin 0x57. On top of
//!   that wfb-ng writes the same six bytes into A3, and the first payload
//!   byte must be a packet type wfb-ng sends on the air (1 or 2). RubyFPV's
//!   `13:12:..` is a group address too, and must appear twice.
//! - **OpenHD's 0x02 is a legal first byte** (locally administered unicast,
//!   about 1 in 64 randomised MACs). Besides the frame shape above, OpenHD
//!   writes the same first and last byte into A2 and A3, and A3's middle is
//!   the high half of a counter that starts below 2^32 (`m_nonce =
//!   randombytes_random()` then `++`), written little-endian. So A3 bytes
//!   2..=4 are zero until 2^40 packets have been sent. A random station would
//!   have to pass the shape test and then match 1 + 1 + 3 more bytes.
//! - **DroneBridge has no MAC header at all.** Its 10-byte header puts the
//!   direction byte (1 or 3) where A1 would start, which makes "A1" a group
//!   address, and needs FromDS = ToDS = 0 (only ad-hoc / NAN / OCB data does
//!   that), a zero duration, a port of 1-7 in A1's third byte (which rules out
//!   every common multicast prefix: `01:00:5e`, `01:80:c2`, `01:00:0c`,
//!   `01:1b:19`, `33:33`, `03:00:00`), and a little-endian payload length that
//!   matches the frame's length to the byte (or to the byte plus 10, in
//!   DroneBridge's "compatibility mode"). A random frame has to match all of
//!   that, the length to 1 in 65,536.
//!
//! What does still match is another program injecting frames that copy one of
//! these headers. That is a true positive in the sense that matters: someone
//! is running a raw-injection link on that channel.
#![cfg_attr(not(test), no_std)]

mod sightings;

pub use sightings::{Sighting, Sightings};

/// Bytes the ESP32 promiscuous callback delivers past the end of the MPDU
/// (the FCS slot; see `firmware/node-wifi/src/chip.rs`).
pub const FCS_LEN: usize = 4;

/// Which link software sent a frame.
///
/// Serialized with the variant names (`"WfbNg"`, ...), the same convention as
/// the `rid` crate's enums; [`System::label`] is the name for people.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum System {
    /// wfb-ng, and OpenIPC (which uses wfb-ng unchanged). The two are
    /// identical on the air.
    WfbNg,
    /// OpenHD's own wifibroadcast fork.
    OpenHd,
    RubyFpv,
    /// DroneBridge for Linux (raw protocol v2). Not the ESP32 DroneBridge,
    /// which uses ordinary Wi-Fi or ESP-NOW.
    DroneBridge,
}

impl System {
    /// Every system, in declaration order.
    pub const ALL: [System; 4] = [
        System::WfbNg,
        System::OpenHd,
        System::RubyFpv,
        System::DroneBridge,
    ];

    /// The name to show people.
    pub const fn label(self) -> &'static str {
        match self {
            System::WfbNg => "wfb-ng/OpenIPC",
            System::OpenHd => "OpenHD",
            System::RubyFpv => "RubyFPV",
            System::DroneBridge => "DroneBridge",
        }
    }

    /// What [`OpenLink::id`] means for this system, or `None` when the system
    /// sends no identifier in the clear.
    pub const fn id_name(self) -> Option<&'static str> {
        match self {
            System::WfbNg => Some("link_id"),
            System::OpenHd => None,
            System::RubyFpv => Some("vehicle_id"),
            System::DroneBridge => Some("comm_id"),
        }
    }
}

/// Which way a frame was going, when the system says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Direction {
    /// From the aircraft to the ground station (video, telemetry down).
    AirToGround,
    /// From the ground station to the aircraft (RC, MAVLink up).
    GroundToAir,
    Unknown,
}

/// One frame recognised as an open FPV / telemetry link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct OpenLink {
    pub system: System,
    /// The identifier the frame carries in the clear, if any:
    /// - wfb-ng: the 24-bit link ID (first three bytes of SHA-1 of the
    ///   `link_domain`; the default "default" gives `0x7505d6`);
    /// - RubyFPV: the vehicle ID (the source on a downlink, the destination
    ///   on an uplink), `None` if the packet's encryption flag is set or the
    ///   ID is the broadcast ID;
    /// - DroneBridge: the 8-bit comm ID that pairs a drone with its ground station;
    /// - OpenHD: always `None` (nothing stable is sent in the clear).
    ///
    /// These are identifiers of a person's equipment. The hub does not show
    /// them unless its operator opts in (see `tracker::Config::show_link_ids`).
    pub id: Option<u32>,
    /// The stream within the link: wfb-ng's radio port (0-127 down, 128-255
    /// up), OpenHD's radio port byte (bit 0 = encrypted, bits 1-7 = stream
    /// index), RubyFPV's decoded port, DroneBridge's port (1-7).
    pub port: u8,
    pub direction: Direction,
    /// 802.11 address 2, the transmitter, as received (display order). For
    /// wfb-ng it embeds the link ID; for OpenHD it changes with every packet;
    /// DroneBridge frames have no address, so it is all zero.
    pub tx: [u8; 6],
}

impl OpenLink {
    /// wfb-ng's stream class from its radio port (spec section 4.1), or
    /// `None` for other systems and for reserved ranges.
    pub const fn wfb_stream(&self) -> Option<WfbStream> {
        if !matches!(self.system, System::WfbNg) {
            return None;
        }
        match (self.port & 0x7F) >> 4 {
            0 => Some(WfbStream::Video),
            1 => Some(WfbStream::Mavlink),
            2 => Some(WfbStream::Tunnel),
            _ => None,
        }
    }
}

/// wfb-ng's allocation of radio ports within each direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WfbStream {
    /// Ports 0-15 (128-143 up).
    Video,
    /// Ports 16-31 (144-159 up).
    Mavlink,
    /// Ports 32-47 (160-175 up), the IP tunnel.
    Tunnel,
}

const BROADCAST: [u8; 6] = [0xFF; 6];
/// wfb-ng's address prefix: ASCII "WB".
pub const WFB_PREFIX: [u8; 2] = [0x57, 0x42];
/// wfb-ng's link ID for the default `link_domain` "default": the first three
/// bytes of SHA-1("default"). OpenIPC ships with it too, so most links on the
/// air carry this ID and it says little about whose they are.
pub const WFB_DEFAULT_LINK_ID: u32 = 0x75_05_D6;
/// RubyFPV's fixed A2 and A3.
pub const RUBY_MAC: [u8; 6] = [0x13, 0x12, 0x34, 0x56, 0x78, 0x90];
/// RubyFPV's broadcast vehicle ID (`BROADCAST_VEHICLE_ID`).
pub const RUBY_BROADCAST_ID: u32 = 1_111_111_111;

/// Frame control byte 0 of a plain data frame (version 0, type 2, subtype 0).
const FC_DATA: u8 = 0x08;
/// Frame control byte 0 of an RTS (type 1, subtype 11). wfb-ng and
/// DroneBridge can send their header in one instead of a data frame.
const FC_RTS: u8 = 0xB4;

/// Cheapest possible first test, for a receive callback that sees every
/// data frame: can this frame-control byte belong to any known system?
/// (`classify` makes the same test first; this is for callers that want to
/// skip even the call.)
pub const fn maybe(fc0: u8) -> bool {
    fc0 == FC_DATA || fc0 == FC_RTS
}

/// Classify a frame as the ESP32 promiscuous callback delivers it: from the
/// frame control field to the end, *including* the 4-byte FCS slot.
pub fn classify(frame: &[u8]) -> Option<OpenLink> {
    classify_mpdu(frame.get(..frame.len().checked_sub(FCS_LEN)?)?)
}

/// Classify an MPDU (frame control to the end of the body, no FCS). The
/// whole frame is needed: DroneBridge is matched on its exact length.
pub fn classify_mpdu(f: &[u8]) -> Option<OpenLink> {
    let (&fc0, rest) = f.split_first()?;
    let &fc1 = rest.first()?;
    if !maybe(fc0) {
        return None;
    }
    // Flags byte: ToDS 0x01, FromDS 0x02, MoreFrag 0x04, Retry 0x08,
    // PwrMgt 0x10, MoreData 0x20, Protected 0x40, Order 0x80. Retry and the
    // power bits are left alone (a driver may touch them); the DS bits,
    // Protected and Order decide the header layout and must match exactly.
    match fc1 & 0xC3 {
        0x01 => injected_24(fc0, f),
        0x00 => dronebridge(f),
        _ => None,
    }
}

/// The three systems that send a full 24-byte header with ToDS set and a
/// broadcast (or nearly broadcast) receiver address.
fn injected_24(fc0: u8, f: &[u8]) -> Option<OpenLink> {
    let a1: [u8; 6] = f.get(4..10)?.try_into().ok()?;
    let a2: [u8; 6] = f.get(10..16)?.try_into().ok()?;
    let a3: [u8; 6] = f.get(16..22)?.try_into().ok()?;
    // Bytes 22-23 are the sequence control; the body starts at 24 (no QoS
    // field: subtype 0, and no HT Control: Order is clear).
    let body = f.get(24..)?;
    // All three send to broadcast; RubyFPV overwrites A1's first byte with
    // its port (its downlink port 0x0F gives 0xFF, plain broadcast again).
    if a1[1..] != BROADCAST[1..] {
        return None;
    }
    if a1 == BROADCAST
        && let Some(l) = wfb_ng(a2, a3, body).or_else(|| openhd(fc0, a2, a3, body))
    {
        return Some(l);
    }
    rubyfpv(fc0, a1, a2, a3, body)
}

/// wfb-ng: `wifibroadcast.hpp` `ieee80211_header` (`08 01 00 00`, A1
/// broadcast, A2 and A3 both `57:42` followed by the big-endian
/// `channel_id = (link_id << 8) + radio_port`), `tx.cpp` (byte 0 is `FRAME_TYPE_DATA` 0x08 or
/// `FRAME_TYPE_RTS` 0xb4), and the packet types `WFB_PACKET_DATA` = 1 and
/// `WFB_PACKET_SESSION` = 2 (3, `DEDUP_STATS`, only travels between wfb-ng
/// processes, never on the air).
fn wfb_ng(a2: [u8; 6], a3: [u8; 6], body: &[u8]) -> Option<OpenLink> {
    if a2[..2] != WFB_PREFIX || a3 != a2 {
        return None;
    }
    // Smallest real packet: a data packet's 9-byte block header plus the
    // 16-byte ChaCha20-Poly1305 tag. A session packet is larger still.
    const MIN_BODY: usize = 1 + 8 + 16;
    if body.len() < MIN_BODY || !matches!(body[0], 1 | 2) {
        return None;
    }
    let port = a2[5];
    Some(OpenLink {
        system: System::WfbNg,
        id: Some(u32::from_be_bytes([0, a2[2], a2[3], a2[4]])),
        port,
        // Spec 4.1: streams 0-127 are vehicle to ground, 128-255 ground to vehicle.
        direction: if port < 128 {
            Direction::AirToGround
        } else {
            Direction::GroundToAir
        },
        tx: a2,
    })
}

/// OpenHD: `wifibroadcast/src/Ieee80211Header.hpp` (`Ieee80211HeaderOpenHD`)
/// and `WBTxRx.cpp`. Data frames only (`is_data_frame()` requires `08 01`).
/// A2 = `[unique id][nonce bytes 0-3][radio port]`, A3 = `[unique id][nonce
/// bytes 4-7][radio port]`; unique id 0x01 = air, 0x02 = ground; the nonce is
/// a little-endian u64 counter that starts at a random u32.
fn openhd(fc0: u8, a2: [u8; 6], a3: [u8; 6], body: &[u8]) -> Option<OpenLink> {
    if fc0 != FC_DATA || a2[0] != a3[0] || a2[5] != a3[5] || a3[2..5] != [0, 0, 0] {
        return None;
    }
    let direction = match a2[0] {
        0x01 => Direction::AirToGround,
        0x02 => Direction::GroundToAir,
        _ => return None,
    };
    // Every OpenHD packet carries at least its 16-byte authentication tag.
    if body.len() < 16 {
        return None;
    }
    Some(OpenLink {
        system: System::OpenHd,
        id: None,
        port: a2[5],
        direction,
        tx: a2,
    })
}

/// RubyFPV: `code/radio/radiolink.c` (`s_uIEEEHeaderData`, and
/// `_radio_encode_port`: A1's first byte is `(port << 4) | 0x0F`) and
/// `radiopackets2.h` (`t_packet_header`, `RADIO_PORT_ROUTER_UPLINK` 0x0E,
/// `RADIO_PORT_ROUTER_DOWNLINK` 0x0F, `PACKET_FLAGS_BIT_HAS_ENCRYPTION`).
fn rubyfpv(fc0: u8, a1: [u8; 6], a2: [u8; 6], a3: [u8; 6], body: &[u8]) -> Option<OpenLink> {
    if fc0 != FC_DATA || a1[0] & 0x0F != 0x0F || a2 != RUBY_MAC || a3 != RUBY_MAC {
        return None;
    }
    let port = a1[0] >> 4;
    let direction = match port {
        0x0F => Direction::AirToGround,
        0x0E => Direction::GroundToAir,
        _ => Direction::Unknown,
    };
    Some(OpenLink {
        system: System::RubyFpv,
        id: ruby_vehicle_id(body, direction),
        port,
        direction,
        tx: a2,
    })
}

/// The vehicle ID from the first Ruby packet header in `body`, if it is in
/// the clear.
///
/// `t_packet_header` (packed, little-endian): `u32 uCRC` (0), `u8
/// packet_flags` (4), `u8 packet_type` (5), `u32 stream_packet_idx` (6), `u16
/// packet_flags_extended` (10), `u16 total_length` (12), `u16
/// radio_link_packet_index` (14), `u32 vehicle_id_src` (16), `u32
/// vehicle_id_dest` (20): 24 bytes.
///
/// Current RubyFPV encrypts only what follows this header, but older versions
/// encrypted "everything after packet_flags", so with the encryption flag set
/// the IDs are not trusted. `total_length` must also be plausible (a header's
/// worth, and no more than the frame holds), so a frame from an unknown
/// version does not yield a made-up ID.
fn ruby_vehicle_id(body: &[u8], direction: Direction) -> Option<u32> {
    const HEADER: usize = 24;
    const FLAG_ENCRYPTED: u8 = 1 << 6;
    let h = body.get(..HEADER)?;
    if h[4] & FLAG_ENCRYPTED != 0 {
        return None;
    }
    let total = usize::from(u16::from_le_bytes([h[12], h[13]]));
    if total < HEADER || total > body.len() {
        return None;
    }
    let le = |at: usize| u32::from_le_bytes([h[at], h[at + 1], h[at + 2], h[at + 3]]);
    let id = match direction {
        Direction::AirToGround => le(16),
        Direction::GroundToAir => le(20),
        Direction::Unknown => return None,
    };
    (id != 0 && id != RUBY_BROADCAST_ID).then_some(id)
}

/// DroneBridge for Linux, raw protocol v2 (dronebridge.gitbook.io, "Raw
/// protocol"): bytes 0-3 frame control and duration (`08 00 00 00` or `b4 00
/// 00 00`), 4 direction (0x01 to the drone, 0x03 to the ground station), 5
/// comm ID, 6 port (1 control, 2 telemetry, 3 video, 4 communication, 5
/// status, 6 proxy, 7 RC), 7-8 payload length (little-endian), 9 sequence
/// number, then the payload. In "compatibility mode" 10 bytes of padding
/// follow the header.
fn dronebridge(f: &[u8]) -> Option<OpenLink> {
    const HEADER: usize = 10;
    const COMPAT_PAD: usize = 10;
    let h = f.get(..HEADER)?;
    if h[2] != 0 || h[3] != 0 {
        return None;
    }
    let direction = match h[4] {
        0x01 => Direction::GroundToAir,
        0x03 => Direction::AirToGround,
        _ => return None,
    };
    if !(1..=7).contains(&h[6]) {
        return None;
    }
    let payload = usize::from(u16::from_le_bytes([h[7], h[8]]));
    if f.len() != HEADER + payload && f.len() != HEADER + COMPAT_PAD + payload {
        return None;
    }
    Some(OpenLink {
        system: System::DroneBridge,
        id: Some(u32::from(h[5])),
        port: h[6],
        direction,
        tx: [0; 6],
    })
}

#[cfg(test)]
mod tests;
