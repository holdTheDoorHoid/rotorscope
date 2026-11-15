//! One received Mode S frame (56 or 112 bits), its downlink format, parity
//! check and error correction.

use crate::crc::{SYN_LONG, SYN_SHORT, syndrome};

/// A raw Mode S reply as received: 7 bytes (56 bits) or 14 bytes (112).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    bytes: [u8; 14],
    len: u8,
}

impl core::fmt::Debug for Frame {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for b in self.as_bytes() {
            write!(f, "{b:02X}")?;
        }
        Ok(())
    }
}

/// Length in bits a downlink format must have: DF0-15 are 56 bits, DF16
/// and up 112. (The first bit of the DF says it: 0 short, 1 long.)
pub const fn bits_for_df(df: u8) -> usize {
    if df < 16 { 56 } else { 112 }
}

impl Frame {
    /// From 7 or 14 bytes. `None` for any other length, or when the length
    /// does not match what the DF field says it should be.
    pub fn new(data: &[u8]) -> Option<Self> {
        if data.len() != 7 && data.len() != 14 {
            return None;
        }
        let mut bytes = [0u8; 14];
        bytes[..data.len()].copy_from_slice(data);
        let f = Self { bytes, len: data.len() as u8 };
        (bits_for_df(f.df()) == data.len() * 8).then_some(f)
    }

    /// From hex text (14 or 28 digits, either case).
    pub fn from_hex(text: &str) -> Option<Self> {
        let t = text.as_bytes();
        if t.len() != 14 && t.len() != 28 {
            return None;
        }
        let mut b = [0u8; 14];
        for (i, pair) in t.chunks(2).enumerate() {
            let hi = hex_digit(pair[0])?;
            let lo = hex_digit(pair[1])?;
            b[i] = hi << 4 | lo;
        }
        Self::new(&b[..t.len() / 2])
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }

    pub fn bits(&self) -> usize {
        usize::from(self.len) * 8
    }

    pub fn is_long(&self) -> bool {
        self.len == 14
    }

    /// Downlink format. DF24 and up share a two-bit prefix and are all
    /// reported as 24 (Comm-D extended length messages).
    pub fn df(&self) -> u8 {
        let d = self.bytes[0] >> 3;
        if d >= 24 { 24 } else { d }
    }

    /// Bits `start..start+n` (0 = first bit sent), MSB first, `n` <= 32.
    pub fn field(&self, start: usize, n: usize) -> u32 {
        get_bits(self.as_bytes(), start, n)
    }

    /// CRC syndrome: see [`crate::crc::syndrome`].
    pub fn syndrome(&self) -> u32 {
        syndrome(self.as_bytes())
    }

    /// Flip bit `bit` (0 = first bit sent).
    pub fn flip(&mut self, bit: usize) {
        if bit < self.bits() {
            self.bytes[bit / 8] ^= 0x80 >> (bit % 8);
        }
    }

    /// The 56 ME / MB / MV bits (bits 32..88) of a long frame.
    pub fn payload(&self) -> [u8; 7] {
        let mut m = [0u8; 7];
        if self.is_long() {
            m.copy_from_slice(&self.bytes[4..11]);
        }
        m
    }
}

fn hex_digit(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Bits `start..start+n` of `data`, MSB first; bits past the end read 0.
pub fn get_bits(data: &[u8], start: usize, n: usize) -> u32 {
    let mut v = 0u32;
    for i in start..start + n {
        let byte = data.get(i / 8).copied().unwrap_or(0);
        v = v << 1 | u32::from((byte >> (7 - i % 8)) & 1);
    }
    v
}

/// The bits of the DF field: never touched by correction, because a frame
/// whose DF is wrong is a different kind of frame and guessing it is how
/// decoders invent aircraft.
const DF_BITS: usize = 5;

/// What error correction did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fix {
    /// The frame was clean.
    Clean,
    /// One bit was flipped back (bit number).
    One(u8),
    /// Two bits were flipped back.
    Two(u8, u8),
}

impl Fix {
    pub fn bits(self) -> u8 {
        match self {
            Fix::Clean => 0,
            Fix::One(_) => 1,
            Fix::Two(..) => 2,
        }
    }
}

/// Repair a DF17/DF18 frame (parity/identity, syndrome zero when clean)
/// with up to `max_bits` (0-2) wrong bits.
///
/// Conservative by construction:
/// - only DF17 and DF18, whose parity is not overlaid with anything, so a
///   zero syndrome really means "no error" (address/parity formats have no
///   such check and are never corrected);
/// - never in the DF field (bits 0-4);
/// - only when exactly one pattern of at most `max_bits` flipped bits
///   explains the syndrome. CRC-24 on 112 bits has minimum distance 6, so
///   1- and 2-bit patterns never share a syndrome, but this is checked
///   rather than assumed.
///
/// Returns `None` (frame untouched) when the frame cannot be repaired
/// within those rules. A 2-bit repair can still be wrong when 3 or more
/// bits were hit; the [`crate::table`] only accepts one for an aircraft it
/// already knows from a clean frame.
pub fn correct(frame: &mut Frame, max_bits: u8) -> Option<Fix> {
    let df = frame.df();
    if !frame.is_long() || !(df == 17 || df == 18) {
        return None;
    }
    let s = frame.syndrome();
    if s == 0 {
        return Some(Fix::Clean);
    }
    if max_bits == 0 {
        return None;
    }
    let syn: &[u32] = &SYN_LONG;
    // One bit.
    let mut one = None;
    let mut count = 0;
    for (i, &v) in syn.iter().enumerate() {
        if v == s {
            one = Some(i);
            count += 1;
        }
    }
    if count == 1 {
        let i = one?;
        if i < DF_BITS {
            return None;
        }
        frame.flip(i);
        return Some(Fix::One(i as u8));
    }
    if max_bits < 2 {
        return None;
    }
    // Two bits: every pair (i < j) with syn[i] ^ syn[j] == s.
    let mut pair = None;
    let mut pairs = 0;
    for i in 0..syn.len() {
        let want = s ^ syn[i];
        for (j, &v) in syn.iter().enumerate().skip(i + 1) {
            if v == want {
                pair = Some((i, j));
                pairs += 1;
            }
        }
    }
    match (pairs, pair) {
        (1, Some((i, j))) if i >= DF_BITS => {
            frame.flip(i);
            frame.flip(j);
            Some(Fix::Two(i as u8, j as u8))
        }
        _ => None,
    }
}

/// Single-bit syndromes for this frame's length (for tests and tools).
pub fn bit_syndromes(long: bool) -> &'static [u32] {
    if long { &SYN_LONG } else { &SYN_SHORT }
}
