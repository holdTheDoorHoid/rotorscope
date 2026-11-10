//! Streaming a capture over USB serial as text, and getting it back.
//!
//! USB-Serial-JTAG is the only link a bare C5 has, it carries the node's
//! other output too, and esp-println drops output when the host stops
//! reading. So a dump is a sequence of self-checking text lines that any
//! log of the port can be mined for afterwards, in any order, with
//! everything else interleaved:
//!
//! ```text
//! @H {"id":3,"kind":"nib8","rate":40000000,"bytes":40960,...,"lines":854,"crc":"9a0c51e2"}
//! @D 3 0 1c2b3a49 <base64 of bytes 0..48>
//! @D 3 1 ...
//! @E 3 854 40960 9a0c51e2
//! ```
//!
//! - every `@D` line carries its dump id, sequence number and the CRC-32 of
//!   its own bytes, so a corrupt or missing line is pinpointed, and
//!   `resend <first> [last]` on the console sends just those again;
//! - `@E` repeats the total length and the CRC-32 of the whole payload;
//! - the payload is raw: `nib8` (Path B bytes), `w32` (Path A words, little
//!   endian) or `c16` (what the decoder was given: little-endian `i16` I, Q).
//!
//! `tools/rawcap-pull` uses [`Collector`] to rebuild the payload and
//! [`to_ci16`] to write a file `tools/droneid-file --format ci16` reads.

use core::fmt;

use crate::crc32::crc32;

/// Payload bytes per `@D` line: 64 base64 characters or 96 hex digits.
pub const CHUNK: usize = 48;

/// Longest line [`data_line`] writes.
pub const LINE_MAX: usize = 3 + 11 + 11 + 9 + 2 * CHUNK + 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Enc {
    B64,
    Hex,
}

impl Enc {
    pub const fn name(self) -> &'static str {
        match self {
            Enc::B64 => "b64",
            Enc::Hex => "hex",
        }
    }
    pub fn parse(s: &str) -> Option<Enc> {
        match s {
            "b64" | "base64" => Some(Enc::B64),
            "hex" => Some(Enc::Hex),
            _ => None,
        }
    }
}

/// What the payload holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Path B: one PARL_IO byte per sample, I nibble high, Q nibble low.
    Nib8,
    /// Path A: dump-engine words, little-endian u32.
    W32,
    /// Decoder input: little-endian i16 I, i16 Q.
    C16,
}

impl Kind {
    pub const fn name(self) -> &'static str {
        match self {
            Kind::Nib8 => "nib8",
            Kind::W32 => "w32",
            Kind::C16 => "c16",
        }
    }
    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "nib8" => Kind::Nib8,
            "w32" => Kind::W32,
            "c16" => Kind::C16,
            _ => return None,
        })
    }
    /// Bytes per complex sample.
    pub const fn sample_bytes(self) -> usize {
        match self {
            Kind::Nib8 => 1,
            Kind::W32 | Kind::C16 => 4,
        }
    }
}

/// The header of a dump. `Display` writes the JSON object of the `@H` line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Meta {
    pub id: u32,
    pub kind: Kind,
    /// Sample rate of the payload, S/s.
    pub rate: u32,
    pub bytes: u32,
    /// LO frequency, kHz.
    pub freq_khz: u32,
    /// Forced gain index.
    pub gain: u8,
    /// Path B window (`iq::Window::lsb`): 6 clip, 5 fold5, 4 fold.
    pub lsb: u8,
    /// Path A: I taken from the low field.
    pub swap_iq: bool,
    /// Analog low-pass capacitor code written (BBTOP regs 6/7), or -1.
    pub lpf: i8,
    pub enc: Enc,
    /// Microseconds since boot when the capture was frozen.
    pub t_us: u64,
    /// Stream position of the first byte (Path B), else 0.
    pub pos: u64,
    pub crc: u32,
}

impl Meta {
    pub const fn lines(&self) -> u32 {
        (self.bytes as usize).div_ceil(CHUNK) as u32
    }
}

impl fmt::Display for Meta {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{{\"id\":{},\"kind\":\"{}\",\"rate\":{},\"bytes\":{},\"freq_khz\":{},\"gain\":{},\"lsb\":{},\
             \"swap_iq\":{},\"lpf\":{},\"enc\":\"{}\",\"t_us\":{},\"pos\":{},\"lines\":{},\"crc\":\"{:08x}\"}}",
            self.id,
            self.kind.name(),
            self.rate,
            self.bytes,
            self.freq_khz,
            self.gain,
            self.lsb,
            self.swap_iq,
            self.lpf,
            self.enc.name(),
            self.t_us,
            self.pos,
            self.lines(),
            self.crc
        )
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const HEX: &[u8; 16] = b"0123456789abcdef";

struct Cursor<'a> {
    buf: &'a mut [u8],
    n: usize,
}

impl Cursor<'_> {
    fn push(&mut self, b: u8) {
        self.buf[self.n] = b;
        self.n += 1;
    }
    fn bytes(&mut self, s: &[u8]) {
        for &b in s {
            self.push(b);
        }
    }
    fn dec(&mut self, mut v: u32) {
        let mut d = [0u8; 10];
        let mut k = 0;
        loop {
            d[k] = b'0' + (v % 10) as u8;
            k += 1;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        while k > 0 {
            k -= 1;
            self.push(d[k]);
        }
    }
    fn hex32(&mut self, v: u32) {
        for s in (0..8).rev() {
            self.push(HEX[((v >> (4 * s)) & 15) as usize]);
        }
    }
}

/// Encode `data` as base64 (standard alphabet, `=` padding) into `out`.
/// Returns the length written (`4 * ceil(len / 3)`).
pub fn b64_encode(data: &[u8], out: &mut [u8]) -> usize {
    let mut n = 0;
    for c in data.chunks(3) {
        let v = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out[n] = B64[(v >> 18) as usize & 63];
        out[n + 1] = B64[(v >> 12) as usize & 63];
        out[n + 2] = if c.len() > 1 { B64[(v >> 6) as usize & 63] } else { b'=' };
        out[n + 3] = if c.len() > 2 { B64[v as usize & 63] } else { b'=' };
        n += 4;
    }
    n
}

/// Format `@D <id> <seq> <crc> <payload>` for `chunk` (at most [`CHUNK`]
/// bytes) into `buf`; returns the line (no newline).
pub fn data_line<'a>(buf: &'a mut [u8; LINE_MAX], id: u32, seq: u32, chunk: &[u8], enc: Enc) -> &'a str {
    assert!(chunk.len() <= CHUNK);
    let mut c = Cursor { buf: &mut buf[..], n: 0 };
    c.bytes(b"@D ");
    c.dec(id);
    c.push(b' ');
    c.dec(seq);
    c.push(b' ');
    c.hex32(crc32(chunk));
    c.push(b' ');
    match enc {
        Enc::B64 => {
            let n = c.n;
            let w = b64_encode(chunk, &mut c.buf[n..]);
            c.n += w;
        }
        Enc::Hex => {
            for &b in chunk {
                c.push(HEX[(b >> 4) as usize]);
                c.push(HEX[(b & 15) as usize]);
            }
        }
    }
    let n = c.n;
    // Only ASCII was written.
    core::str::from_utf8(&buf[..n]).unwrap_or("")
}

/// The chunk of `data` that line `seq` carries.
pub fn chunk(data: &[u8], seq: u32) -> &[u8] {
    let a = (seq as usize * CHUNK).min(data.len());
    &data[a..(a + CHUNK).min(data.len())]
}

/// `@E <id> <lines> <bytes> <crc>` (no newline).
pub fn end_line<'a>(buf: &'a mut [u8; LINE_MAX], m: &Meta) -> &'a str {
    let mut c = Cursor { buf: &mut buf[..], n: 0 };
    c.bytes(b"@E ");
    c.dec(m.id);
    c.push(b' ');
    c.dec(m.lines());
    c.push(b' ');
    c.dec(m.bytes);
    c.push(b' ');
    c.hex32(m.crc);
    let n = c.n;
    core::str::from_utf8(&buf[..n]).unwrap_or("")
}

#[cfg(any(test, feature = "std"))]
pub use host::*;

#[cfg(any(test, feature = "std"))]
mod host {
    extern crate std;
    use std::collections::BTreeMap;
    use std::string::{String, ToString};
    use std::vec::Vec;

    use super::{CHUNK, Enc, Kind, Meta};
    use crate::crc32::crc32;

    pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
        let val = |c: u8| -> Option<u32> {
            Some(match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a' + 26,
                b'0'..=b'9' => c - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                _ => return None,
            } as u32)
        };
        let s = s.as_bytes();
        if !s.len().is_multiple_of(4) {
            return None;
        }
        let mut out = Vec::with_capacity(s.len() / 4 * 3);
        for q in s.chunks(4) {
            let pad = q.iter().rev().take_while(|&&c| c == b'=').count();
            if pad > 2 {
                return None;
            }
            let mut v = 0u32;
            for &c in &q[..4 - pad] {
                v = v << 6 | val(c)?;
            }
            v <<= 6 * pad as u32;
            out.push((v >> 16) as u8);
            if pad < 2 {
                out.push((v >> 8) as u8);
            }
            if pad < 1 {
                out.push(v as u8);
            }
        }
        Some(out)
    }

    pub fn hex_decode(s: &str) -> Option<Vec<u8>> {
        if !s.len().is_multiple_of(2) {
            return None;
        }
        (0..s.len()).step_by(2).map(|k| u8::from_str_radix(s.get(k..k + 2)?, 16).ok()).collect()
    }

    /// Values of a flat JSON object as written by `Meta`'s `Display`
    /// (strings, numbers, booleans; no nesting, no escapes).
    fn flat_json(s: &str) -> Option<BTreeMap<String, String>> {
        let s = s.trim().strip_prefix('{')?.strip_suffix('}')?;
        let mut m = BTreeMap::new();
        for kv in s.split(',') {
            let (k, v) = kv.split_once(':')?;
            m.insert(k.trim().trim_matches('"').to_string(), v.trim().trim_matches('"').to_string());
        }
        Some(m)
    }

    /// Parse the JSON after `@H `.
    pub fn parse_meta(json: &str) -> Option<Meta> {
        let m = flat_json(json)?;
        let n = |k: &str| m.get(k)?.parse::<u64>().ok();
        Some(Meta {
            id: n("id")? as u32,
            kind: Kind::parse(m.get("kind")?)?,
            rate: n("rate")? as u32,
            bytes: n("bytes")? as u32,
            freq_khz: n("freq_khz")? as u32,
            gain: n("gain")? as u8,
            lsb: n("lsb")? as u8,
            swap_iq: m.get("swap_iq")? == "true",
            lpf: m.get("lpf")?.parse().ok()?,
            enc: Enc::parse(m.get("enc")?)?,
            t_us: n("t_us")?,
            pos: n("pos")?,
            crc: u32::from_str_radix(m.get("crc")?, 16).ok()?,
        })
    }

    /// One dump being reassembled.
    #[derive(Clone, Debug)]
    pub struct Partial {
        pub meta: Meta,
        pub chunks: BTreeMap<u32, Vec<u8>>,
        /// Lines whose own CRC failed (kept out of `chunks`).
        pub bad: Vec<u32>,
        pub ended: bool,
    }

    impl Partial {
        pub fn missing(&self) -> Vec<u32> {
            (0..self.meta.lines()).filter(|s| !self.chunks.contains_key(s)).collect()
        }

        /// The payload, if every line arrived and the whole CRC matches.
        pub fn payload(&self) -> Result<Vec<u8>, String> {
            let missing = self.missing();
            if !missing.is_empty() {
                return Err(format_missing(self.meta.id, &missing));
            }
            let mut v = Vec::with_capacity(self.meta.bytes as usize);
            for c in self.chunks.values() {
                v.extend_from_slice(c);
            }
            if v.len() != self.meta.bytes as usize {
                return Err(std::format!("length {} != {}", v.len(), self.meta.bytes));
            }
            let c = crc32(&v);
            if c != self.meta.crc {
                return Err(std::format!("payload CRC {c:08x} != {:08x}", self.meta.crc));
            }
            Ok(v)
        }
    }

    /// The console command that asks for the missing lines again.
    pub fn format_missing(id: u32, missing: &[u32]) -> String {
        // Collapse into ranges.
        let mut out = std::format!("dump {id}: {} line(s) missing; type on the console:", missing.len());
        let mut k = 0;
        while k < missing.len() {
            let a = missing[k];
            let mut b = a;
            while k + 1 < missing.len() && missing[k + 1] == b + 1 {
                k += 1;
                b = missing[k];
            }
            out += &std::format!("  resend {a} {b}");
            k += 1;
        }
        out
    }

    /// Mines a serial log for dumps. Feed it every line; unrelated lines are
    /// ignored.
    #[derive(Default, Debug)]
    pub struct Collector {
        pub dumps: BTreeMap<u32, Partial>,
        /// Data lines for an id whose header was never seen.
        pub orphans: u32,
    }

    impl Collector {
        pub fn feed(&mut self, line: &str) {
            let line = line.trim_end_matches(['\r', '\n']);
            // Tolerate junk before the marker (a partial line from a reset).
            let Some(at) = line.find('@') else { return };
            let line = &line[at..];
            if let Some(json) = line.strip_prefix("@H ") {
                if let Some(meta) = parse_meta(json) {
                    // A re-sent header for the same capture keeps what arrived.
                    let e = self.dumps.entry(meta.id).or_insert_with(|| Partial {
                        meta,
                        chunks: BTreeMap::new(),
                        bad: Vec::new(),
                        ended: false,
                    });
                    if e.meta != meta {
                        *e = Partial { meta, chunks: BTreeMap::new(), bad: Vec::new(), ended: false };
                    }
                }
            } else if let Some(rest) = line.strip_prefix("@D ") {
                let mut f = rest.split(' ');
                let (Some(id), Some(seq), Some(crc), Some(data), None) = (f.next(), f.next(), f.next(), f.next(), f.next())
                else {
                    return;
                };
                let (Ok(id), Ok(seq), Ok(crc)) = (id.parse::<u32>(), seq.parse::<u32>(), u32::from_str_radix(crc, 16))
                else {
                    return;
                };
                let Some(p) = self.dumps.get_mut(&id) else {
                    self.orphans += 1;
                    return;
                };
                let bytes = match p.meta.enc {
                    Enc::B64 => b64_decode(data),
                    Enc::Hex => hex_decode(data),
                };
                let want_len = (p.meta.bytes as usize).saturating_sub(seq as usize * CHUNK).min(CHUNK);
                match bytes {
                    Some(b) if crc32(&b) == crc && b.len() == want_len && seq < p.meta.lines() => {
                        p.chunks.insert(seq, b);
                        p.bad.retain(|&s| s != seq);
                    }
                    _ => {
                        if !p.chunks.contains_key(&seq) && !p.bad.contains(&seq) {
                            p.bad.push(seq);
                        }
                    }
                }
            } else if let Some(rest) = line.strip_prefix("@E ") {
                let id = rest.split(' ').next().and_then(|s| s.parse::<u32>().ok());
                if let Some(p) = id.and_then(|id| self.dumps.get_mut(&id)) {
                    p.ended = true;
                }
            }
        }
    }

    /// The payload as interleaved little-endian `i16` I, Q at the payload's
    /// own rate (what `droneid-file --format ci16` reads). Cell-centre
    /// levels, no DC removal (the decoder removes DC per burst).
    pub fn to_ci16(meta: &Meta, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(payload.len() / meta.kind.sample_bytes() * 4);
        let mut put = |c: droneid::math::C16| {
            out.extend_from_slice(&c.re.to_le_bytes());
            out.extend_from_slice(&c.im.to_le_bytes());
        };
        match meta.kind {
            Kind::Nib8 => payload.iter().for_each(|&b| put(crate::iq::to_c16(b))),
            Kind::W32 => payload
                .as_chunks::<4>()
                .0
                .iter()
                .for_each(|w| put(crate::word::to_c16(u32::from_le_bytes(*w), meta.swap_iq))),
            Kind::C16 => out.extend_from_slice(payload),
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(bytes: u32, crc: u32, enc: Enc) -> Meta {
        Meta {
            id: 7,
            kind: Kind::Nib8,
            rate: 40_000_000,
            bytes,
            freq_khz: 5_756_000,
            gain: 60,
            lsb: 6,
            swap_iq: false,
            lpf: 12,
            enc,
            t_us: 123_456_789_012,
            pos: 987_654,
            crc,
        }
    }

    /// The firmware side: every line of a dump, as the console prints them.
    fn emit(data: &[u8], enc: Enc) -> Vec<String> {
        let m = meta(data.len() as u32, crate::crc32::crc32(data), enc);
        let mut v = vec![format!("@H {m}")];
        let mut buf = [0u8; LINE_MAX];
        for s in 0..m.lines() {
            v.push(data_line(&mut buf, m.id, s, chunk(data, s), enc).to_string());
        }
        v.push(end_line(&mut buf, &m).to_string());
        v
    }

    fn sample(n: usize) -> Vec<u8> {
        (0..n).map(|k| (k * 7 + k / 13) as u8).collect()
    }

    #[test]
    fn base64_matches_reference_vectors() {
        // RFC 4648 section 10.
        for (i, o) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foobar", "Zm9vYmFy")] {
            let mut out = [0u8; 16];
            let n = b64_encode(i.as_bytes(), &mut out);
            assert_eq!(&out[..n], o.as_bytes());
            assert_eq!(b64_decode(o).unwrap(), i.as_bytes());
        }
        assert!(b64_decode("Zm9").is_none());
        assert!(b64_decode("Z===").is_none());
    }

    #[test]
    fn header_round_trips() {
        let m = meta(40_000, 0xdead_beef, Enc::Hex);
        let s = format!("{m}");
        assert_eq!(parse_meta(&s), Some(m));
        assert!(s.contains("\"lines\":834"));
        let m = Meta { kind: Kind::W32, swap_iq: true, lpf: -1, ..meta(65_520, 1, Enc::B64) };
        assert_eq!(parse_meta(&format!("{m}")), Some(m));
    }

    #[test]
    fn whole_dump_round_trips_both_encodings() {
        for enc in [Enc::B64, Enc::Hex] {
            for n in [1, 47, 48, 49, 40_000] {
                let data = sample(n);
                let mut c = Collector::default();
                for l in emit(&data, enc) {
                    assert!(l.starts_with("@H") || l.len() <= LINE_MAX);
                    c.feed(&l);
                }
                let p = &c.dumps[&7];
                assert!(p.ended);
                assert_eq!(p.payload().unwrap(), data, "{enc:?} {n}");
            }
        }
    }

    #[test]
    fn noise_loss_and_resend() {
        let data = sample(5000);
        let lines = emit(&data, Enc::B64);
        let mut c = Collector::default();
        c.feed("I (32) boot: ESP-IDF v5.5 2nd stage bootloader");
        for (k, l) in lines.iter().enumerate() {
            match k {
                // Lost entirely.
                5 | 6 | 7 | 50 => {}
                // Corrupted in transit.
                20 => {
                    let mut b = l.clone().into_bytes();
                    let last = b.len() - 1;
                    b[last] = if b[last] == b'A' { b'B' } else { b'A' };
                    c.feed(core::str::from_utf8(&b).unwrap());
                }
                // Truncated by a reset, then junk glued in front.
                30 => c.feed(&l[..l.len() / 2]),
                40 => c.feed(&format!("garbage{l}\r")),
                _ => c.feed(l),
            }
            if k == 60 {
                c.feed(r#"{"t":"stats","x":1}"#);
            }
        }
        let p = &c.dumps[&7];
        // Lines are offset by one (the header is lines[0]).
        assert_eq!(p.missing(), vec![4, 5, 6, 19, 29, 49]);
        let err = p.payload().unwrap_err();
        assert!(err.contains("resend 4 6") && err.contains("resend 19 19") && err.contains("resend 49 49"), "{err}");
        // The console re-sends: only those lines, same id.
        for s in [4, 5, 6, 19, 29, 49] {
            c.feed(&lines[s as usize + 1]);
        }
        assert_eq!(c.dumps[&7].payload().unwrap(), data);
        assert_eq!(c.orphans, 0);
    }

    #[test]
    fn ci16_conversions() {
        let m = meta(2, 0, Enc::B64);
        let out = to_ci16(&m, &[crate::iq::pack(1, -2), crate::iq::pack(-8, 7)]);
        let v: Vec<i16> = out.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect();
        assert_eq!(v, vec![3 << 10, -3 << 10, -15 << 10, 15 << 10]);
        let w: u32 = (60 << 20) | ((5u32 & 0x3ff) << 10) | ((-7i32 as u32) & 0x3ff);
        let m = Meta { kind: Kind::W32, ..meta(4, 0, Enc::B64) };
        let out = to_ci16(&m, &w.to_le_bytes());
        let v: Vec<i16> = out.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect();
        assert_eq!(v, vec![crate::word::level(5), crate::word::level(-7)]);
    }
}
