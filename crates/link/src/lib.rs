//! What radio nodes tell the hub.
//!
//! Each frame is a [`Frame`] serialized with postcard and COBS-delimited (a
//! zero byte ends every frame), so a reader that joins mid-stream resyncs at
//! the next zero. Nodes send *raw* Remote ID messages; the hub decodes them
//! with the `rid` crate.
#![cfg_attr(not(test), no_std)]

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
    /// Wi-Fi transmitter address or BLE advertiser address, in display order
    /// (most significant byte first, as printed `aa:bb:..`).
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
    Hello {
        role: NodeRole,
        protocol: u8,
        firmware: heapless::String<16>,
    },
    Detection(Detection),
    /// Once a second: frames seen vs. Remote ID found, for the health screen.
    /// Every count is a running total since boot; the hub takes differences.
    Stats {
        uptime_us: u64,
        frames_seen: u32,
        rid_found: u32,
        dropped: u32,
        /// Frames seen per transport, for nodes that can tell them apart (the
        /// BLE node splits legacy / extended / Long Range). Empty otherwise.
        by_transport: Vec<(Transport, u32), 5>,
    },
}

/// Serialize `frame` with postcard and COBS-encode it into `buf`, including
/// the trailing zero delimiter. Returns the used part of `buf`; send it as is.
pub fn encode<'a>(frame: &Frame, buf: &'a mut [u8]) -> Result<&'a mut [u8], postcard::Error> {
    postcard::to_slice_cobs(frame, buf)
}

/// Why a chunk of the byte stream did not become a [`Frame`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkError {
    /// More than [`MAX_FRAME`] bytes arrived without a delimiter; they were dropped.
    Overflow,
    /// A delimited chunk was not a valid COBS + postcard [`Frame`] (corruption,
    /// a reader that joined mid-frame, or a peer on another protocol version).
    Corrupt,
}

/// Streaming decoder for the byte stream from one UART: feed it whatever
/// arrived, it hands back each complete frame. Fixed `MAX_FRAME` buffer, no
/// allocation. Zero bytes between frames (resync padding) are ignored.
pub struct Decoder {
    acc: postcard::accumulator::CobsAccumulator<MAX_FRAME>,
    /// True while the accumulator holds part of a frame.
    pending: bool,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder {
    pub const fn new() -> Self {
        Self {
            acc: postcard::accumulator::CobsAccumulator::new(),
            pending: false,
        }
    }

    /// Consume `input`, calling `on_frame` once per complete frame or error,
    /// in stream order. Partial frames are kept for the next call.
    pub fn feed(&mut self, mut input: &[u8], mut on_frame: impl FnMut(Result<Frame, LinkError>)) {
        use postcard::accumulator::FeedResult;
        loop {
            if !self.pending {
                let start = input.iter().position(|&b| b != 0).unwrap_or(input.len());
                input = &input[start..];
            }
            if input.is_empty() {
                return;
            }
            let (result, rest) = match self.acc.feed::<Frame>(input) {
                FeedResult::Consumed => {
                    self.pending = true;
                    return;
                }
                FeedResult::OverFull(rest) => (Err(LinkError::Overflow), rest),
                FeedResult::DeserError(rest) => (Err(LinkError::Corrupt), rest),
                FeedResult::Success { data, remaining } => (Ok(data), remaining),
            };
            // An overflow without a delimiter leaves us mid-frame.
            self.pending = matches!(result, Err(LinkError::Overflow)) && !rest.is_empty();
            on_frame(result);
            input = rest;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detection(fill: u8, n: usize) -> Detection {
        let mut messages = Vec::new();
        for _ in 0..n {
            messages.push([fill; rid::MESSAGE_SIZE]).unwrap();
        }
        Detection {
            transport: Transport::BleLongRange,
            channel: 255,
            rssi: i8::MIN,
            mac: [fill; 6],
            counter: 255,
            uptime_us: u64::MAX,
            messages,
        }
    }

    fn samples() -> [Frame; 5] {
        [
            Frame::Hello {
                role: NodeRole::Wifi5,
                protocol: PROTOCOL_VERSION,
                firmware: heapless::String::try_from("0.1.0-abcdef1234").unwrap(),
            },
            Frame::Detection(detection(0xFF, MAX_PACK)),
            Frame::Detection(detection(0x00, MAX_PACK)),
            Frame::Detection(detection(0x5A, 1)),
            Frame::Stats {
                uptime_us: u64::MAX,
                frames_seen: u32::MAX,
                rid_found: 7,
                dropped: 0,
                by_transport: [
                    (Transport::BleLegacy, u32::MAX),
                    (Transport::BleExtended, u32::MAX),
                    (Transport::BleLongRange, u32::MAX),
                    (Transport::WifiBeacon, u32::MAX),
                    (Transport::WifiNan, u32::MAX),
                ]
                .into_iter()
                .collect(),
            },
        ]
    }

    #[test]
    fn round_trip() {
        let mut dec = Decoder::new();
        for f in samples() {
            let mut buf = [0u8; MAX_FRAME];
            let wire = encode(&f, &mut buf).unwrap();
            assert_eq!(wire.last(), Some(&0));
            assert!(!wire[..wire.len() - 1].contains(&0));
            let mut got = None;
            dec.feed(wire, |r| got = Some(r));
            assert_eq!(got, Some(Ok(f)));
        }
    }

    /// The worst case: a full pack of non-zero bytes and every integer at its
    /// widest varint. All-0xFF content maximises both COBS and varint size.
    #[test]
    fn largest_detection_fits() {
        let mut big = [0u8; 1024];
        let mut worst = 0;
        for f in samples() {
            worst = worst.max(encode(&f, &mut big).unwrap().len());
        }
        std::println!("largest encoded frame: {worst} bytes");
        assert!(worst <= MAX_FRAME, "{worst} > {MAX_FRAME}");
        let mut exact = [0u8; MAX_FRAME];
        assert!(encode(&samples()[1], &mut exact).is_ok());
    }

    /// Frames split at every possible point, run together, and mixed with
    /// junk still come out in order.
    #[test]
    fn streaming_split_and_resync() {
        let mut stream = std::vec::Vec::new();
        stream.extend_from_slice(&[0x13, 0x37, 0x42]); // tail of a frame we joined mid-way
        stream.push(0);
        stream.extend_from_slice(&[0, 0]); // idle padding
        for f in samples() {
            let mut buf = [0u8; MAX_FRAME];
            stream.extend_from_slice(encode(&f, &mut buf).unwrap());
        }
        for split in 0..stream.len() {
            let mut dec = Decoder::new();
            let mut out = std::vec::Vec::new();
            dec.feed(&stream[..split], |r| out.push(r));
            dec.feed(&stream[split..], |r| out.push(r));
            assert_eq!(out[0], Err(LinkError::Corrupt), "split {split}");
            let frames: std::vec::Vec<_> = out[1..].to_vec();
            assert_eq!(frames, samples().map(Ok).to_vec(), "split {split}");
        }
        // Byte at a time.
        let mut dec = Decoder::new();
        let mut n = 0;
        for b in &stream {
            dec.feed(core::slice::from_ref(b), |r| n += r.is_ok() as usize);
        }
        assert_eq!(n, samples().len());
    }

    #[test]
    fn overflow_then_recovery() {
        let mut dec = Decoder::new();
        let mut out = std::vec::Vec::new();
        dec.feed(&[0x55; MAX_FRAME + 40], |r| out.push(r));
        dec.feed(&[0x55, 0x00], |r| out.push(r));
        let mut buf = [0u8; MAX_FRAME];
        let f = samples()[3].clone();
        dec.feed(encode(&f, &mut buf).unwrap(), |r| out.push(r));
        assert_eq!(out.first(), Some(&Err(LinkError::Overflow)));
        assert_eq!(out.last(), Some(&Ok(f)));
    }

    #[test]
    fn random_garbage_never_panics() {
        let mut dec = Decoder::new();
        let mut x: u64 = 0x1234_5678_9ABC_DEF0;
        let mut chunk = [0u8; 97];
        for _ in 0..20_000 {
            for b in chunk.iter_mut() {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                *b = if x.is_multiple_of(11) { 0 } else { x as u8 };
            }
            dec.feed(&chunk[..(x % 98) as usize], |_| {});
        }
    }
}
