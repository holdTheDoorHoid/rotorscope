//! Reassembling MAVLink frames from radio packets, and the [`Watch`] that
//! ties streams to the [`Analyser`].
//!
//! A telemetry radio (SiK) is a serial pipe: a MAVLink frame may start in
//! one radio packet and end in the next. Each stream keeps the unfinished
//! tail of the last packet and joins it to the next one. Packets lost on
//! the air simply leave a frame that fails its checksum and is skipped.

use crate::analyse::{Analyser, Finding, Link};
use crate::frame::{Frame, MAX_FRAME_LEN, Parse, parse_at};

/// Bytes a stream holds: one frame waiting to complete plus a full packet.
pub const STREAM_BUF: usize = MAX_FRAME_LEN + 256;
/// Streams followed at once.
pub const MAX_STREAMS: usize = 4;

/// Which byte stream a packet belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StreamKey {
    /// The radio system (any number the caller uses, e.g.
    /// `ctrlwatch::System as u8`).
    pub system: u8,
    /// The link's identifier when the caller may use one (SiK NET ID, mLRS
    /// sync word); `None` when identifiers are off.
    pub id: Option<u32>,
    /// Direction, when the radio tells (0 unknown, 1 up, 2 down): mLRS
    /// carries the two directions in different frame types.
    pub dir: u8,
}

impl StreamKey {
    /// The analyser's view of the link: frames from both directions of one
    /// link are one link.
    pub fn link(&self) -> Link {
        let id = self.id.map_or(0xFFFF_FFFF, |v| v.wrapping_mul(0x9E37_79B9));
        Link {
            key: id ^ ((self.system as u32) << 24),
            distinct: self.id.is_some(),
        }
    }
}

/// One reassembly buffer.
#[derive(Debug, Clone)]
pub struct Stream {
    key: StreamKey,
    last_ms: u64,
    len: usize,
    buf: [u8; STREAM_BUF],
}

impl Stream {
    pub const fn new(key: StreamKey) -> Self {
        Self {
            key,
            last_ms: 0,
            len: 0,
            buf: [0; STREAM_BUF],
        }
    }

    pub fn key(&self) -> StreamKey {
        self.key
    }

    /// Append `bytes` and hand every complete frame to `on`.
    pub fn push(&mut self, bytes: &[u8], on: &mut dyn FnMut(&Frame<'_>)) {
        let mut bytes = bytes;
        while !bytes.is_empty() {
            let room = STREAM_BUF - self.len;
            if room == 0 {
                // Stuck on a long false start: drop the oldest half.
                let keep = STREAM_BUF / 2;
                self.buf.copy_within(self.len - keep..self.len, 0);
                self.len = keep;
                continue;
            }
            let n = room.min(bytes.len());
            self.buf[self.len..self.len + n].copy_from_slice(&bytes[..n]);
            self.len += n;
            bytes = &bytes[n..];
            self.scan(on);
        }
    }

    fn scan(&mut self, on: &mut dyn FnMut(&Frame<'_>)) {
        let mut i = 0;
        while i < self.len {
            match parse_at(&self.buf[i..self.len]) {
                Parse::Frame(f) => {
                    on(&f);
                    i += f.len();
                }
                Parse::Incomplete => break,
                Parse::Bad => i += 1,
            }
        }
        self.buf.copy_within(i..self.len, 0);
        self.len -= i;
    }
}

/// Streams plus the analyser: the whole detector for one receiver.
#[derive(Debug, Clone)]
pub struct Watch {
    streams: heapless::Vec<Stream, MAX_STREAMS>,
    analyser: Analyser,
}

impl Default for Watch {
    fn default() -> Self {
        Self::new()
    }
}

impl Watch {
    pub const fn new() -> Self {
        Self {
            streams: heapless::Vec::new(),
            analyser: Analyser::new(),
        }
    }

    /// Feed one packet's MAVLink-carrying bytes (SiK serial data, an mLRS
    /// payload, a UDP datagram). `out` gets each finding due for a report
    /// and the frame that caused it.
    pub fn feed(
        &mut self,
        key: StreamKey,
        bytes: &[u8],
        now_ms: u64,
        out: &mut dyn FnMut(&Finding, &Frame<'_>),
    ) {
        self.feed_on(key.link(), key, bytes, now_ms, out);
    }

    /// [`Watch::feed`] with the analyser's [`Link`] given separately from
    /// the reassembly stream: the stream follows one sender's byte flow
    /// (bytes from two senders must never be joined), while the link may
    /// gather several flows. Over Wi-Fi (`wifiwatch::mavwifi`) each
    /// direction of each UDP/TCP flow is a stream, and the network (BSSID)
    /// is the link, so an injector on the same network shares the
    /// vehicle's link.
    pub fn feed_on(
        &mut self,
        link: Link,
        key: StreamKey,
        bytes: &[u8],
        now_ms: u64,
        out: &mut dyn FnMut(&Finding, &Frame<'_>),
    ) {
        let i = match self.streams.iter().position(|s| s.key == key) {
            Some(i) => i,
            None if !self.streams.is_full() => {
                let _ = self.streams.push(Stream::new(key));
                self.streams.len() - 1
            }
            None => {
                let i = (0..self.streams.len())
                    .min_by_key(|&i| self.streams[i].last_ms)
                    .unwrap_or(0);
                self.streams[i] = Stream::new(key);
                i
            }
        };
        let (streams, analyser) = (&mut self.streams, &mut self.analyser);
        let s = &mut streams[i];
        s.last_ms = now_ms;
        s.push(bytes, &mut |f| analyser.frame(link, f, now_ms, out));
    }

    /// Analyse frames that need no reassembly (each `bytes` holds whole
    /// frames, e.g. one UDP datagram).
    pub fn feed_frames(
        &mut self,
        link: Link,
        bytes: &[u8],
        now_ms: u64,
        out: &mut dyn FnMut(&Finding, &Frame<'_>),
    ) {
        for f in crate::frame::frames(bytes) {
            self.analyser.frame(link, &f, now_ms, out);
        }
    }
}
