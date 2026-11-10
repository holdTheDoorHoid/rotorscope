//! Raw serial bytes to CSBee lines.

use crate::{Error, MAX_LINE, Report, parse};

/// Splits a byte stream into lines and parses each one.
///
/// A line ends at `\n` (the ADSBee sends `\r\n`). A `#` always starts a new
/// line, even without a line end before it: `#` never appears inside a
/// CSBee line, so when a UART drops the `\n` (or any byte) only the damaged
/// line is lost, not the next one as well. Text with no `#` at all (console
/// log messages on the USB port) comes out as [`Error::NotCsbee`]; empty
/// lines are skipped silently.
#[derive(Debug, Clone)]
pub struct LineSplitter {
    buf: [u8; MAX_LINE],
    len: usize,
    /// The current line outgrew `buf`; drop bytes until its end.
    overflow: bool,
}

impl Default for LineSplitter {
    fn default() -> Self {
        Self::new()
    }
}

impl LineSplitter {
    pub const fn new() -> Self {
        Self {
            buf: [0; MAX_LINE],
            len: 0,
            overflow: false,
        }
    }

    /// Feed bytes; `out` is called once per finished line.
    pub fn feed(&mut self, bytes: &[u8], mut out: impl FnMut(Result<Report, Error>)) {
        for &b in bytes {
            match b {
                b'\n' => self.finish(&mut out),
                b'#' => {
                    self.finish(&mut out);
                    self.push(b);
                }
                _ => self.push(b),
            }
        }
    }

    /// Drop a partial line (e.g. after a UART error).
    pub fn reset(&mut self) {
        self.len = 0;
        self.overflow = false;
    }

    fn push(&mut self, b: u8) {
        if self.len < self.buf.len() {
            self.buf[self.len] = b;
            self.len += 1;
        } else {
            self.overflow = true;
        }
    }

    fn finish(&mut self, out: &mut impl FnMut(Result<Report, Error>)) {
        if self.overflow {
            out(Err(Error::TooLong));
        } else {
            let line = &self.buf[..self.len];
            if line.iter().any(|b| !b.is_ascii_whitespace()) {
                out(parse(line));
            }
        }
        self.reset();
    }
}
