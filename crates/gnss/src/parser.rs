//! The streaming, transport-agnostic parser.

use crate::fix::Fix;
use crate::nmea;
use crate::quectel::{self, Reply};
use crate::sky::SkyView;
use crate::ubx::{self, MonVer, ValGet};

/// Largest UBX payload the parser will accept. Larger "frames" are treated as garbage so a
/// false `B5 62` in noise cannot swallow real data. NAV-PVT is 92; a MON-VER with many
/// extension strings is under 300.
pub const MAX_UBX_PAYLOAD: usize = 392;
/// Longest NMEA sentence including `$`, checksum and CR LF (NMEA 0183 limit).
pub const MAX_NMEA_LEN: usize = 82;

const BUF: usize = MAX_UBX_PAYLOAD + 8;

/// Something the parser decoded.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Event {
    /// A navigation solution, from NAV-PVT or merged NMEA GGA + RMC.
    Fix(Fix),
    /// UBX-ACK-ACK for the message with this class and id.
    Ack {
        /// Class of the acknowledged message (CFG-VALSET is 0x06).
        class: u8,
        /// ID of the acknowledged message (CFG-VALSET is 0x8A).
        id: u8,
    },
    /// UBX-ACK-NAK: the receiver rejected the message.
    Nak {
        /// Class of the rejected message.
        class: u8,
        /// ID of the rejected message.
        id: u8,
    },
    /// Reply to a MON-VER poll.
    MonVer(MonVer),
    /// Reply to a CFG-VALGET poll.
    ValGet(ValGet),
    /// A Quectel `$PAIR001` acknowledgement or `$PQTM...` reply (LC29H).
    Quectel(Reply),
}

/// Counters for diagnostics (wrapping).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Frames/sentences that passed their checksum.
    pub frames_ok: u32,
    /// UBX or NMEA frames whose checksum failed (and were discarded).
    pub checksum_errors: u32,
    /// Bytes thrown away while hunting for the next frame start.
    pub bytes_dropped: u32,
}

enum Step {
    Need,
    /// Discard this many bytes (>= 1): garbage, or a bad frame.
    Drop(usize, bool),
    Ubx(usize),
    Line(usize),
}

/// Streaming UBX + NMEA parser with a fixed internal buffer (about 450 bytes) and no allocation.
///
/// Feed it whatever the transport gives you, in any chunking. UBX and NMEA frames may be
/// interleaved with garbage; a bad checksum or a truncated frame costs only that frame, and
/// the parser re-scans the bytes after a failed header, so a real frame hidden behind a
/// false start is still found.
pub struct Parser {
    buf: [u8; BUF],
    len: usize,
    nmea: nmea::Pending,
    stats: Stats,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    /// A fresh parser.
    pub const fn new() -> Self {
        Parser { buf: [0; BUF], len: 0, nmea: nmea::Pending::new(), stats: Stats {
            frames_ok: 0,
            checksum_errors: 0,
            bytes_dropped: 0,
        } }
    }

    /// Diagnostic counters.
    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// Satellites per constellation and band (from NMEA GSV/GSA; empty on a UBX-only link).
    pub fn sky(&self) -> &SkyView {
        &self.nmea.sky
    }

    /// Forget any partial frame (call after reconfiguring the link, e.g. a baud change).
    pub fn reset(&mut self) {
        self.len = 0;
        self.nmea = nmea::Pending::new();
    }

    /// Feed bytes; iterate the result to get events. The iterator consumes `data` lazily, so
    /// **run it to the end** (a plain `for` loop does); bytes after an early `break` are
    /// discarded.
    #[must_use = "events are produced only while the iterator is driven"]
    pub fn feed<'p, 'd>(&'p mut self, data: &'d [u8]) -> Events<'p, 'd> {
        Events { parser: self, data }
    }

    /// Convenience: feed one byte. If one byte yields several events (a recovered frame hidden
    /// inside a false one) only the first is returned; prefer [`Parser::feed`].
    pub fn push(&mut self, byte: u8) -> Option<Event> {
        self.feed(core::slice::from_ref(&byte)).next()
    }

    fn append(&mut self, b: u8) {
        // The longest frame we ever wait for fits BUF, so this can't overflow.
        self.buf[self.len] = b;
        self.len += 1;
    }

    fn analyze(&self) -> Step {
        let b = &self.buf[..self.len];
        let Some(&first) = b.first() else { return Step::Need };
        match first {
            ubx::SYNC1 => {
                if b.len() >= 2 && b[1] != ubx::SYNC2 {
                    return Step::Drop(1, false);
                }
                if b.len() < 6 {
                    return Step::Need;
                }
                let plen = u16::from_le_bytes([b[4], b[5]]) as usize;
                if plen > MAX_UBX_PAYLOAD {
                    return Step::Drop(1, false);
                }
                let total = plen + 8;
                if b.len() < total {
                    return Step::Need;
                }
                let (a, c) = ubx::fletcher(&b[2..6 + plen]);
                if a == b[6 + plen] && c == b[7 + plen] {
                    Step::Ubx(total)
                } else {
                    Step::Drop(1, true)
                }
            }
            b'$' => {
                for (i, &c) in b.iter().enumerate().skip(1) {
                    match c {
                        b'\n' => {
                            return if nmea::verify(&b[..=i]).is_some() {
                                Step::Line(i + 1)
                            } else {
                                Step::Drop(1, true)
                            };
                        }
                        b'\r' => {}
                        0x20..=0x7e if c != b'$' => {}
                        _ => return Step::Drop(1, false),
                    }
                }
                if b.len() >= MAX_NMEA_LEN { Step::Drop(1, false) } else { Step::Need }
            }
            _ => Step::Drop(1, false),
        }
    }

    /// Remove `n` bytes from the front, then also skip ahead to the next possible frame start.
    fn drop_front(&mut self, n: usize) -> usize {
        let b = &self.buf[..self.len];
        let next = b[n.min(b.len())..]
            .iter()
            .position(|&c| c == ubx::SYNC1 || c == b'$')
            .map_or(b.len(), |p| p + n);
        self.consume(next);
        next
    }

    fn consume(&mut self, n: usize) {
        self.buf.copy_within(n..self.len, 0);
        self.len -= n;
    }

    /// Decode everything already buffered until more bytes are needed or an event is ready.
    fn poll(&mut self) -> Option<Event> {
        loop {
            match self.analyze() {
                Step::Need => return None,
                Step::Drop(n, bad_sum) => {
                    if bad_sum {
                        self.stats.checksum_errors = self.stats.checksum_errors.wrapping_add(1);
                    }
                    let d = self.drop_front(n);
                    self.stats.bytes_dropped = self.stats.bytes_dropped.wrapping_add(d as u32);
                }
                Step::Ubx(total) => {
                    let ev = decode_ubx(self.buf[2], self.buf[3], &self.buf[6..total - 2]);
                    self.consume(total);
                    if let Some(ev) = ev {
                        self.stats.frames_ok = self.stats.frames_ok.wrapping_add(1);
                        return Some(ev);
                    }
                }
                Step::Line(total) => {
                    let ev = nmea::verify(&self.buf[..total]).and_then(|body| {
                        if quectel::is_quectel(body) {
                            quectel::decode(body).map(Event::Quectel)
                        } else {
                            self.nmea.accept(body).map(Event::Fix)
                        }
                    });
                    self.consume(total);
                    self.stats.frames_ok = self.stats.frames_ok.wrapping_add(1);
                    if let Some(ev) = ev {
                        return Some(ev);
                    }
                }
            }
        }
    }
}

fn decode_ubx(class: u8, id: u8, p: &[u8]) -> Option<Event> {
    use ubx::*;
    match (class, id) {
        (CLS_NAV, ID_NAV_PVT) => nav_pvt_to_fix(p).map(Event::Fix),
        (CLS_ACK, ID_ACK_ACK) if p.len() >= 2 => Some(Event::Ack { class: p[0], id: p[1] }),
        (CLS_ACK, ID_ACK_NAK) if p.len() >= 2 => Some(Event::Nak { class: p[0], id: p[1] }),
        (CLS_MON, ID_MON_VER) => decode_mon_ver(p).map(Event::MonVer),
        (CLS_CFG, ID_CFG_VALGET) => decode_val_get(p).map(Event::ValGet),
        _ => None,
    }
}

/// Iterator returned by [`Parser::feed`].
pub struct Events<'p, 'd> {
    parser: &'p mut Parser,
    data: &'d [u8],
}

impl Iterator for Events<'_, '_> {
    type Item = Event;

    fn next(&mut self) -> Option<Event> {
        loop {
            if let Some(ev) = self.parser.poll() {
                return Some(ev);
            }
            let (&b, rest) = self.data.split_first()?;
            self.data = rest;
            self.parser.append(b);
        }
    }
}
