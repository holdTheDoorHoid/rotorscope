//! Raw Mode S frames from a receiver: the **Beast** binary stream and the
//! ADSBee's **RAW** text lines.
//!
//! The ADSBee 1090 (firmware 0.9.0) offers both with `AT+PROTOCOL_OUT`
//! (`BEAST`, `BEAST_NO_UAT`, `BEAST_NO_UAT_UPLINK`, `RAW`). Both carry only
//! frames the ADSBee already accepted: its own decoder repairs single-bit
//! errors and drops frames it cannot validate before reporting (see
//! docs/ADSBEE.md, "Raw frames"). Beast is preferred on a UART: about 23
//! bytes per long frame against about 70 for RAW, a 12 MHz timestamp and a
//! signal level.
//!
//! **Beast** (as the ADSBee writes it, `comms/beast/beast_utils.cpp`):
//! `0x1A`, a type byte (`0x31` Mode A/C, `0x32` 56-bit Mode S, `0x33`
//! 112-bit Mode S), a 6-byte big-endian 12 MHz counter, a signal byte
//! (`255 * sqrt(power)`, 0 = none) and the frame. Every `0x1A` inside is
//! sent twice. `0xE3`/`0xE4` (readsb feed and ingest ids) are skipped.
//!
//! **RAW**: `#MDS*<hex>;(<source>,<dBm>,<SNR dB>,<48 MHz counter hex>)\r\n`.

use crate::frame::Frame;

/// A frame as received.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Received {
    pub frame: Frame,
    /// Signal strength, dBm (as the ADSBee measures it).
    pub signal_dbm: Option<i16>,
    /// Receiver clock at reception, 12 MHz ticks (Beast) or 48 MHz ticks
    /// divided by 4 (RAW), so both read the same.
    pub ticks_12mhz: Option<u64>,
}

/// The ADSBee's Beast signal byte back to dBm. Its encoder maps dBm
/// (-100..-40) linearly onto dBFS (-45..0) and writes
/// `255 * 10^(dBFS / 20)`; this is that mapping inverted.
pub fn beast_signal_dbm(level: u8) -> Option<i16> {
    if level == 0 {
        return None;
    }
    let dbfs = 20.0 * libm::log10(f64::from(level) / 255.0);
    let dbm = (dbfs + 45.0) / 0.75 - 100.0;
    Some(libm::round(dbm) as i16)
}

const ESC: u8 = 0x1A;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Waiting for `0x1A`.
    Hunt,
    /// Saw `0x1A`; the next byte is the type.
    Type,
    /// Inside a frame body, `want` unescaped bytes long.
    Body { kind: u8, want: u8 },
    /// Inside a frame body, after an `0x1A`: the next byte must be another
    /// `0x1A`, or a frame was cut short and a new one starts.
    BodyEsc { kind: u8, want: u8 },
    /// Skipping an id record of `left` bytes (types E3, E4).
    Skip { left: u8 },
}

/// Counters for a Beast stream.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BeastStats {
    /// Mode S frames delivered.
    pub frames: u32,
    /// Mode A/C frames (no address; the ADSBee never sends these).
    pub mode_ac: u32,
    /// Frames cut short by a new frame start, or with a length that does
    /// not match their downlink format.
    pub broken: u32,
    /// Bytes thrown away while hunting for a frame start (log text on the
    /// console, line noise on a UART).
    pub skipped: u32,
}

/// Turns a Beast byte stream into frames. Feed it whatever arrives, in any
/// chunks; it resynchronises on the next frame start after an error, so a
/// lost byte costs at most two frames.
#[derive(Debug, Clone)]
pub struct BeastReader {
    state: State,
    buf: [u8; 21],
    len: u8,
    pub stats: BeastStats,
}

impl Default for BeastReader {
    fn default() -> Self {
        Self { state: State::Hunt, buf: [0; 21], len: 0, stats: BeastStats::default() }
    }
}

impl BeastReader {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed bytes; `out` is called for each Mode S frame.
    pub fn feed(&mut self, bytes: &[u8], mut out: impl FnMut(Received)) {
        for &b in bytes {
            self.byte(b, &mut out);
        }
    }

    fn start(&mut self, kind: u8) {
        self.len = 0;
        self.state = match kind {
            0x31 => State::Body { kind, want: 6 + 1 + 2 },
            0x32 => State::Body { kind, want: 6 + 1 + 7 },
            0x33 => State::Body { kind, want: 6 + 1 + 14 },
            0xE4 => State::Skip { left: 36 },
            // E3 is an 8-byte id followed by a whole frame: skip the id,
            // then the frame is found by the hunt.
            0xE3 => State::Skip { left: 8 },
            _ => {
                self.stats.skipped += 2;
                State::Hunt
            }
        };
    }

    fn byte(&mut self, b: u8, out: &mut impl FnMut(Received)) {
        match self.state {
            State::Hunt => {
                if b == ESC {
                    self.state = State::Type;
                } else {
                    self.stats.skipped += 1;
                }
            }
            State::Type => {
                if b == ESC {
                    // An escaped data byte: we joined mid-frame.
                    self.stats.skipped += 2;
                    self.state = State::Hunt;
                } else {
                    self.start(b);
                }
            }
            State::Skip { left } => {
                // Ids are ASCII or escaped binary; count raw bytes, which is
                // close enough: the hunt recovers either way.
                self.state = if left <= 1 { State::Hunt } else { State::Skip { left: left - 1 } };
            }
            State::Body { kind, want } => {
                if b == ESC {
                    self.state = State::BodyEsc { kind, want };
                } else {
                    self.push(b, kind, want, out);
                }
            }
            State::BodyEsc { kind, want } => {
                if b == ESC {
                    self.state = State::Body { kind, want };
                    self.push(ESC, kind, want, out);
                } else {
                    // A lone 0x1A: the frame was cut short and `b` is the
                    // type of a new one.
                    self.stats.broken += 1;
                    self.start(b);
                }
            }
        }
    }

    fn push(&mut self, b: u8, kind: u8, want: u8, out: &mut impl FnMut(Received)) {
        self.buf[usize::from(self.len)] = b;
        self.len += 1;
        if self.len < want {
            return;
        }
        self.state = State::Hunt;
        let ticks = self.buf[..6].iter().fold(0u64, |a, &x| a << 8 | u64::from(x));
        let level = self.buf[6];
        let data = &self.buf[7..usize::from(want)];
        if kind == 0x31 {
            self.stats.mode_ac += 1;
            return;
        }
        match Frame::new(data) {
            Some(frame) => {
                self.stats.frames += 1;
                out(Received {
                    frame,
                    signal_dbm: beast_signal_dbm(level),
                    ticks_12mhz: (ticks != 0).then_some(ticks),
                });
            }
            None => self.stats.broken += 1,
        }
    }
}

/// One ADSBee RAW Mode S line (`#MDS*...`), with or without the line end.
/// `None` for anything else (UAT lines, log text, a damaged line).
pub fn parse_raw_line(line: &[u8]) -> Option<Received> {
    let mut l = line;
    while let [rest @ .., b'\r' | b'\n'] = l {
        l = rest;
    }
    let text = core::str::from_utf8(l).ok()?;
    let rest = text.strip_prefix("#MDS*")?;
    let (hex, meta) = rest.split_once(';')?;
    let frame = Frame::from_hex(hex)?;
    let meta = meta.strip_prefix('(')?.strip_suffix(')')?;
    let mut it = meta.split(',');
    let _source: i16 = it.next()?.parse().ok()?;
    let dbm: i16 = it.next()?.parse().ok()?;
    let _snr: i16 = it.next()?.parse().ok()?;
    let ticks48 = u64::from_str_radix(it.next()?, 16).ok()?;
    if it.next().is_some() {
        return None;
    }
    Some(Received { frame, signal_dbm: Some(dbm), ticks_12mhz: Some(ticks48 / 4) })
}

/// Build a Beast frame for `frame` (tests, and the hub's on-chip loop-back
/// test). Returns the length written into `out` (at most 2 + 2*21 bytes).
pub fn beast_encode(frame: &Frame, ticks_12mhz: u64, level: u8, out: &mut [u8; 44]) -> usize {
    let mut n = 0;
    let mut put = |b: u8, esc: bool, n: &mut usize| {
        out[*n] = b;
        *n += 1;
        if esc && b == ESC {
            out[*n] = ESC;
            *n += 1;
        }
    };
    put(ESC, false, &mut n);
    put(if frame.is_long() { 0x33 } else { 0x32 }, false, &mut n);
    for i in (0..6).rev() {
        put((ticks_12mhz >> (8 * i)) as u8, true, &mut n);
    }
    put(level, true, &mut n);
    for &b in frame.as_bytes() {
        put(b, true, &mut n);
    }
    n
}
