//! What radio nodes tell the hub.
//!
//! Each frame is a [`Frame`] serialized with postcard and COBS-delimited (a
//! zero byte ends every frame), so a reader that joins mid-stream resyncs at
//! the next zero. Nodes send *raw* Remote ID messages; the hub decodes them
//! with the `rid` crate.
#![no_std]

use heapless::Vec;
use rid::{MAX_PACK, RawMessage, Transport};
use serde::{Deserialize, Serialize};

/// Bump when a change would make an old hub misread a new node.
pub const PROTOCOL_VERSION: u8 = 1;

/// Largest encoded frame, COBS overhead included. Size buffers with this.
pub const MAX_FRAME: usize = 320;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeRole {
    Wifi24,
    Wifi5,
    Ble,
    Fpv,
}

/// One Remote ID reception: everything one frame or advertisement carried.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Detection {
    pub transport: Transport,
    /// Wi-Fi channel number, or BLE advertising channel (37-39) when known, else 0.
    pub channel: u8,
    pub rssi: i8,
    /// Wi-Fi transmitter address or BLE advertiser address.
    pub mac: [u8; 6],
    /// F3411 message counter from the frame.
    pub counter: u8,
    /// Node uptime when the frame arrived.
    pub uptime_us: u64,
    pub messages: Vec<RawMessage, MAX_PACK>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Frame {
    /// Sent at boot and every few seconds, so a hub that restarts learns who is attached.
    Hello { role: NodeRole, protocol: u8, firmware: heapless::String<16> },
    Detection(Detection),
    /// Once a second: frames seen vs. Remote ID found, for the health screen.
    Stats { uptime_us: u64, frames_seen: u32, rid_found: u32, dropped: u32 },
}
