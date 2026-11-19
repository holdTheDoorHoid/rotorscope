//! Reading a peer's lines without letting it grow memory (the uploader's
//! serial console). Same approach as `tools/rid-view`'s bounded reader: an
//! over-long line is read through to its newline and thrown away. Unlike a
//! plain `read_until`, a partial line survives a read error such as a
//! serial port's timeout, so a line split by a pause is not lost.

use std::io::{self, BufRead};

/// The longest console line kept. An `@EV1` line is a record (at most
/// [`evidence::format::MAX_RECORD`] bytes) in base64 plus a short prefix.
pub const MAX_LINE: usize = 16 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum Line {
    /// A whole line, with its newline if it had one (the last line of an
    /// input may not).
    Text(Vec<u8>),
    /// A line longer than the cap: dropped.
    TooLong,
}

#[derive(Debug)]
pub struct LineReader {
    buf: Vec<u8>,
    max: usize,
    too_long: bool,
}

impl LineReader {
    pub fn new(max: usize) -> Self {
        Self { buf: Vec::new(), max, too_long: false }
    }

    /// The next line; `Ok(None)` at the end of the input. On an error the
    /// part of a line read so far is kept for the next call.
    pub fn read<R: BufRead>(&mut self, r: &mut R) -> io::Result<Option<Line>> {
        loop {
            let chunk = match r.fill_buf() {
                Ok(c) => c,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            if chunk.is_empty() {
                // End of input: whatever is held is the last line.
                return Ok(self.finish(false));
            }
            let (take, done) = match chunk.iter().position(|&b| b == b'\n') {
                Some(i) => (i + 1, true),
                None => (chunk.len(), false),
            };
            if !self.too_long {
                if self.buf.len() + take > self.max {
                    self.too_long = true;
                    self.buf = Vec::new();
                } else {
                    self.buf.extend_from_slice(&chunk[..take]);
                }
            }
            r.consume(take);
            if done {
                return Ok(self.finish(true));
            }
        }
    }

    fn finish(&mut self, newline: bool) -> Option<Line> {
        let too_long = std::mem::take(&mut self.too_long);
        let buf = std::mem::take(&mut self.buf);
        match (too_long, buf.is_empty()) {
            (true, _) => Some(Line::TooLong),
            (false, true) if !newline => None,
            (false, _) => Some(Line::Text(buf)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Read};

    #[test]
    fn a_line_without_a_newline_cannot_grow_memory() {
        let mut data = vec![b'x'; 1 << 20];
        data.extend_from_slice(b"\nnext\n");
        let mut r = BufReader::with_capacity(4096, &data[..]);
        let mut l = LineReader::new(MAX_LINE);
        assert_eq!(l.read(&mut r).unwrap(), Some(Line::TooLong));
        assert!(l.buf.capacity() <= MAX_LINE + 4096, "{}", l.buf.capacity());
        assert_eq!(l.read(&mut r).unwrap(), Some(Line::Text(b"next\n".to_vec())));
        assert_eq!(l.read(&mut r).unwrap(), None);
    }

    #[test]
    fn the_cap_is_per_line_and_inclusive() {
        let mut data = vec![b'a'; MAX_LINE - 1];
        data.push(b'\n');
        data.extend_from_slice(&vec![b'b'; MAX_LINE]);
        data.push(b'\n');
        data.extend_from_slice(b"tail");
        let mut r = BufReader::with_capacity(512, &data[..]);
        let mut l = LineReader::new(MAX_LINE);
        assert!(matches!(l.read(&mut r).unwrap(), Some(Line::Text(t)) if t.len() == MAX_LINE));
        assert_eq!(l.read(&mut r).unwrap(), Some(Line::TooLong));
        assert_eq!(l.read(&mut r).unwrap(), Some(Line::Text(b"tail".to_vec())));
        assert_eq!(l.read(&mut r).unwrap(), None);
    }

    /// A serial port times out between bytes; the line read so far is kept.
    #[test]
    fn a_timeout_mid_line_loses_nothing() {
        struct Pausing {
            parts: Vec<&'static [u8]>,
        }
        impl Read for Pausing {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                match self.parts.first().copied() {
                    None => Ok(0),
                    Some(b"") => {
                        self.parts.remove(0);
                        Err(io::Error::new(io::ErrorKind::TimedOut, "pause"))
                    }
                    Some(p) => {
                        out[..p.len()].copy_from_slice(p);
                        self.parts.remove(0);
                        Ok(p.len())
                    }
                }
            }
        }
        let mut r = BufReader::new(Pausing { parts: vec![b"@EV1 AA", b"", b"AA\n"] });
        let mut l = LineReader::new(MAX_LINE);
        assert_eq!(l.read(&mut r).unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert_eq!(l.read(&mut r).unwrap(), Some(Line::Text(b"@EV1 AAAA\n".to_vec())));
    }
}
